use super::super::zsh_bridge::parse_capture_output;
use super::core::{single_quote, ZshDaemon};
use super::lifecycle::{
    cleanup_stale_compdumps, cleanup_stale_init_scripts, is_stale_compdump_name,
    parse_init_script_pid, process_is_alive,
};
use super::pty_io::{contains_line, create_daemon_pty, read_available, write_init_script};
use super::{DAEMON_INIT_SCRIPT, MAX_CONSECUTIVE_TIMEOUTS, READY_MARKER, SENTINEL_BYTE};
use nix::sys::termios;
use serial_test::serial;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ── ユニットテスト: センチネルフレーミング（zsh 不要） ──

#[test]
fn contains_line_finds_exact_match_among_multiple_lines() {
    let haystack = b"source /tmp/foo.zsh\r\nsome noise\r\njarvish_daemon_ok\r\n";
    assert!(contains_line(haystack, READY_MARKER));
}

#[test]
fn contains_line_does_not_match_substring_only() {
    let haystack = b"not jarvish_daemon_ok exactly\r\n";
    // 行全体が完全一致しない限り false（部分文字列一致では拾わない）。
    assert!(!contains_line(haystack, READY_MARKER));
}

#[test]
fn contains_line_absent_returns_false() {
    let haystack = b"still initializing...\r\n";
    assert!(!contains_line(haystack, READY_MARKER));
}

#[test]
fn contains_line_trims_trailing_carriage_return() {
    // PTY 由来の \r\n 行末で trim_end が \r も落とすことを確認する。
    let haystack = b"jarvish_daemon_ok\r\n";
    assert!(contains_line(haystack, READY_MARKER));
}

/// センチネル2個に挟まれたテキストを抽出するロジックを、
/// `request()` 本体から切り出さずに直接文字列操作で再現して検証する
/// （`request()` は PTY 越しの非同期読み取りを含むため、フレーミング
/// だけを純粋にテストする目的でロジックを模した最小実装を使う）。
fn extract_frame(buf: &[u8]) -> Option<String> {
    let mut toggles = 0u8;
    let mut frame_start = None;
    let mut frame_end = None;
    for (idx, &byte) in buf.iter().enumerate() {
        if byte == SENTINEL_BYTE {
            toggles += 1;
            if toggles == 1 {
                frame_start = Some(idx + 1);
            } else if toggles == 2 {
                frame_end = Some(idx);
                break;
            }
        }
    }
    match (frame_start, frame_end) {
        (Some(s), Some(e)) if s <= e => Some(String::from_utf8_lossy(&buf[s..e]).into_owned()),
        _ => None,
    }
}

#[test]
fn extract_frame_between_two_sentinels() {
    let buf = b"jarvishtestcmd \x00\r\nalpha\r\nbeta\r\ngamma\r\n\x00\r\n\x07";
    let frame = extract_frame(buf).expect("should find a frame");
    assert_eq!(frame, "\r\nalpha\r\nbeta\r\ngamma\r\n");
}

#[test]
fn extract_frame_missing_second_sentinel_returns_none() {
    let buf = b"jarvishtestcmd \x00\r\nalpha\r\nbeta\r\n";
    assert_eq!(extract_frame(buf), None);
}

#[test]
fn extract_frame_no_sentinel_at_all_returns_none() {
    let buf = b"alpha\r\nbeta\r\ngamma\r\n";
    assert_eq!(extract_frame(buf), None);
}

#[test]
fn extract_frame_empty_frame_between_adjacent_sentinels() {
    let buf = b"\x00\x00";
    let frame = extract_frame(buf).expect("adjacent sentinels should still frame (empty)");
    assert_eq!(frame, "");
}

#[test]
fn extracted_frame_feeds_into_parse_capture_output() {
    // フレーム抽出 → 既存の zsh_bridge パーサへ、という Task 2 の
    // 実配線を模した end-to-end 相当のユニットテスト（zsh 不要）。
    let buf = b"jarvishtestcmd \x00\r\nalpha\r\nbeta -- desc\r\n\x00\r\n\x07";
    let frame = extract_frame(buf).expect("should find a frame");
    let candidates = parse_capture_output(&frame);
    let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(values, vec!["alpha", "beta"]);
    assert_eq!(
        candidates
            .iter()
            .find(|c| c.value == "beta")
            .unwrap()
            .description,
        Some("desc".to_string())
    );
}

// ── zsh 実機統合テスト（zsh 不在なら runtime skip、#[serial]） ──

fn zsh_binary() -> Option<PathBuf> {
    which::which("zsh").ok()
}

/// pid が実際に ESRCH になる（プロセスが死んでいる）まで短時間・有界
/// 回数ポーリングする（テストフィクスチャ teardown 用共通
/// ヘルパー、`zsh_bridge.rs` / `shell/mod.rs` の同名パターンと同じ
/// 考え方）。
///
/// `mark_dead_and_kill`（バッファ超過・サーキットブレーカー等の内部
/// 経路）や `shutdown()`（非ブロッキング）は kill/reap をバックグラウンド
/// スレッドへ委譲するため、テスト関数がこれらを呼んだ直後に単に
/// スコープを抜けると、まだ子プロセスが生きたまま `cargo test` の
/// テストバイナリプロセスが終了しうる（単体実行で観測される孤児
/// `/bin/zsh -i` の根本原因）。テストは必ずこのヘルパーで実際の死亡を
/// 確認してから終わること。
fn wait_for_pid_death_for_test(pid: u32) -> bool {
    for _ in 0..80 {
        let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if ret == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// テスト用の隔離された ZDOTDIR + fpath ディレクトリ + カスタム補完
/// フィクスチャ（固定ワードリスト）+ 隔離 HOME を作る。
///
/// `zsh_bridge.rs` の E2E テストと同じ理由（`compinit -d
/// ~/.zcompdump_capture` は `$HOME` 基準の固定パスにキャッシュを
/// 読み書きするため、テストごとに `$HOME` も隔離しないと compdump が
/// 衝突する）で `HOME` も隔離する。
struct TestFixture {
    _tmpdir: tempfile::TempDir,
    zdotdir: PathBuf,
    home: PathBuf,
}

fn setup_fixture(completions: &[(&str, &str)]) -> TestFixture {
    let tmpdir = tempfile::tempdir().unwrap();
    let zdotdir = tmpdir.path().join("zdotdir");
    let fpath_dir = tmpdir.path().join("completions");
    let home = tmpdir.path().join("home");
    fs::create_dir_all(&zdotdir).unwrap();
    fs::create_dir_all(&fpath_dir).unwrap();
    fs::create_dir_all(&home).unwrap();

    for (name, body) in completions {
        fs::write(fpath_dir.join(name), body).unwrap();
    }
    fs::write(
        zdotdir.join(".zshrc"),
        format!("fpath=({} $fpath)\n", fpath_dir.display()),
    )
    .unwrap();

    TestFixture {
        _tmpdir: tmpdir,
        zdotdir,
        home,
    }
}

fn extra_envs_for(fixture: &TestFixture) -> Vec<(String, String)> {
    vec![(
        "HOME".to_string(),
        fixture.home.to_string_lossy().into_owned(),
    )]
}

#[test]
#[serial]
fn spawn_reaches_ready_marker() {
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let fixture = setup_fixture(&[(
        "_jarvishtestcmd",
        "#compdef jarvishtestcmd\ncompadd -- alpha beta gamma\n",
    )]);

    let daemon = ZshDaemon::spawn(
        &zsh,
        &fixture.zdotdir,
        &extra_envs_for(&fixture),
        Duration::from_secs(10),
    );
    let mut daemon = daemon.expect("daemon should spawn and reach ready marker");
    assert!(daemon.is_alive());
    // テストフィクスチャ teardown: `shutdown()`（非ブロッキング、
    // kill/reap をバックグラウンドスレッドへ委譲）はテストプロセスの
    // 終了と競合しうる ── 単体テスト実行（1テストのみ）だとテスト
    // 関数を抜けた直後にテストバイナリごと終了し、バックグラウンド
    // スレッドの `kill_tree` が実行される前にプロセスが道連れで消える
    // ことがある（実機計測で確認: `cargo test --lib` 1回の実行で
    // ppid=1 の孤児 `/bin/zsh -i` が複数残った根本原因）。テストでは
    // 常に有界同期版 `shutdown_blocking` を使い、関数が戻った時点で
    // 子プロセスの kill/reap が完了していることを保証する。
    daemon.shutdown_blocking(Duration::from_secs(2));
    assert!(!daemon.is_alive());
}

#[test]
#[serial]
fn two_sequential_requests_reuse_same_daemon_and_return_words() {
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let fixture = setup_fixture(&[(
        "_jarvishtestcmd",
        "#compdef jarvishtestcmd\ncompadd -- alpha beta gamma\n",
    )]);

    let mut daemon = ZshDaemon::spawn(
        &zsh,
        &fixture.zdotdir,
        &extra_envs_for(&fixture),
        Duration::from_secs(10),
    )
    .expect("daemon should spawn");

    let child_pid_before = daemon.child_pid_for_test();

    let first = daemon
        .request("jarvishtestcmd ", Duration::from_secs(15))
        .expect("first request should succeed");
    assert!(
        daemon.is_alive(),
        "daemon must still be alive after request 1"
    );

    let start = Instant::now();
    let second = daemon
        .request("jarvishtestcmd ", Duration::from_secs(15))
        .expect("second request should succeed");
    let elapsed = start.elapsed();

    assert!(
        daemon.is_alive(),
        "daemon must still be alive after request 2"
    );
    assert_eq!(
        daemon.child_pid_for_test(),
        child_pid_before,
        "the same child process must serve both requests (no respawn)"
    );

    let candidates1 = parse_capture_output(&first);
    let candidates2 = parse_capture_output(&second);
    let values1: Vec<&str> = candidates1.iter().map(|c| c.value.as_str()).collect();
    let values2: Vec<&str> = candidates2.iter().map(|c| c.value.as_str()).collect();
    assert!(values1.contains(&"alpha") && values1.contains(&"beta") && values1.contains(&"gamma"));
    assert!(values2.contains(&"alpha") && values2.contains(&"beta") && values2.contains(&"gamma"));

    eprintln!("warm second-request latency: {elapsed:?}");
    // ウォーム経路の主張は「デーモンを使い回すので compinit のコールド
    // コストを再度払わない」こと。コールド spawn は実測で秒オーダー
    // なので、2s を上限にしても回帰（使い回しが壊れて毎回 compinit）は
    // 検出できる。旧 500ms は負荷下での PTY ラウンドトリップでフレークした。
    assert!(
        elapsed < Duration::from_secs(2),
        "warm second request should not pay the cold compinit cost, took {elapsed:?}"
    );

    // テストフィクスチャ teardown: Drop に任せず明示的に
    // 有界同期 shutdown する（`spawn_reaches_ready_marker` のコメント
    // 参照）。
    daemon.shutdown_blocking(Duration::from_secs(2));
}

#[test]
#[serial]
fn no_state_bleed_between_different_requests() {
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let fixture = setup_fixture(&[
        (
            "_jarvishtestcmd",
            "#compdef jarvishtestcmd\ncompadd -- alphaone alphatwo\n",
        ),
        (
            "_jarvishtestcmd2",
            "#compdef jarvishtestcmd2\ncompadd -- betaone betatwo\n",
        ),
    ]);

    let mut daemon = ZshDaemon::spawn(
        &zsh,
        &fixture.zdotdir,
        &extra_envs_for(&fixture),
        Duration::from_secs(10),
    )
    .expect("daemon should spawn");

    // request A: long/different line.
    let out_a = daemon
        .request("jarvishtestcmd al", Duration::from_secs(15))
        .expect("request A should succeed");
    let candidates_a = parse_capture_output(&out_a);
    let values_a: Vec<&str> = candidates_a.iter().map(|c| c.value.as_str()).collect();
    assert!(values_a.contains(&"alphaone") && values_a.contains(&"alphatwo"));

    // request B: a DIFFERENT command entirely -- must reflect only B.
    let out_b = daemon
        .request("jarvishtestcmd2 ", Duration::from_secs(15))
        .expect("request B should succeed");
    let candidates_b = parse_capture_output(&out_b);
    let values_b: Vec<&str> = candidates_b.iter().map(|c| c.value.as_str()).collect();
    assert!(values_b.contains(&"betaone") && values_b.contains(&"betatwo"));
    assert!(
        !values_b.iter().any(|v| v.starts_with("alpha")),
        "request B must not bleed candidates from request A: {values_b:?}"
    );

    // テストフィクスチャ teardown（`spawn_reaches_ready_marker`
    // のコメント参照）。
    daemon.shutdown_blocking(Duration::from_secs(2));
}

#[test]
#[serial]
fn hung_completion_first_timeout_stays_alive_second_kills_descendants() {
    // サーキットブレーカー: `sleep 30` の完全ハング補完関数に
    // 対して、1回目のタイムアウトではまだデーモンを殺さず（グレース）、
    // 同じハング状態が続く2回目のリクエスト（= 1回目の残留フレームの
    // ドレインがタイムアウトし、それ自体が「2回連続」の2回目としてカウント
    // される）で初めてハングと判定してデーモンを kill することを検証する。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    // フィクスチャ: 30秒 sleep してから compadd するハング補完関数。
    let fixture = setup_fixture(&[(
        "_jarvishtesthang",
        "#compdef jarvishtesthang\nsleep 30\ncompadd -- neverseen\n",
    )]);

    let mut daemon = ZshDaemon::spawn(
        &zsh,
        &fixture.zdotdir,
        &extra_envs_for(&fixture),
        Duration::from_secs(10),
    )
    .expect("daemon should spawn");

    let child_pid = daemon.child_pid_for_test();

    let request_timeout = Duration::from_millis(500);

    // 1回目: タイムアウトするが、グレースによりまだ生きている。
    let start1 = Instant::now();
    let result1 = daemon.request("jarvishtesthang ", request_timeout);
    let elapsed1 = start1.elapsed();
    assert_eq!(result1, None, "hung completion should time out to None");
    // epsilon は 250ms -> 1s に緩和。回帰の実測値は「500ms タイムアウトに
    // 対して 2.86 秒」であり、1s の epsilon（= 上限 1.5s）でも十分に検出
    // できる一方、負荷の高いランナー上での PTY ラウンドトリップの揺れは
    // 吸収できる。
    assert!(
        elapsed1 < request_timeout + Duration::from_secs(1),
        "request() must return within timeout + small epsilon, took {elapsed1:?}"
    );
    assert!(
        daemon.is_alive(),
        "daemon must stay alive after a single (first) timeout (Fix D2 grace)"
    );

    // 2回目: 残留フレームのドレインがタイムアウトし、これが「2回連続」の
    // 2回目としてカウントされ、サーキットブレーカーが作動する。
    let start2 = Instant::now();
    let result2 = daemon.request("jarvishtesthang ", request_timeout);
    let elapsed2 = start2.elapsed();
    assert_eq!(result2, None);
    // kill_tree + reap は request() のタイムアウト/desync 経路から
    // バックグラウンドスレッドへ委譲されるようになったため、
    // request() 自体は「タイムアウト値 + 小さな epsilon」以内に戻る
    // はず（以前は kill_tree + 40x25ms 有界ポーリングがこの呼び出し元
    // スレッド上でインラインに走り、最大 ~1 秒余計にブロックしていた
    // — 実測 2.86 秒 vs 500ms タイムアウト。この下限を厳しくすること
    // 自体が「reap を呼び出し元スレッドから追い出せた」ことの直接証拠）。
    assert!(
        elapsed2 < request_timeout + Duration::from_secs(1),
        "request() must return within timeout + small epsilon (reap must not block \
             the caller thread), timeout={request_timeout:?}, took {elapsed2:?}"
    );
    assert!(
        !daemon.is_alive(),
        "daemon must be marked dead after 2 consecutive timeouts (circuit breaker)"
    );

    // 子プロセス（と、可能なら子孫）は request() が戻った後も
    // バックグラウンドスレッドによっていずれ確実に reap される
    // ことを、寛容な時間幅の ESRCH ポーリングで確認する
    // （external.rs のテストと同じ考え方 — ただし今回は呼び出し元
    // スレッドをブロックしないことが主張の核心なので、ポーリング自体は
    // request() が返った**後**に行う）。
    let mut alive = true;
    for _ in 0..80 {
        let ret = unsafe { libc::kill(child_pid as libc::pid_t, 0) };
        if ret == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            alive = false;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !alive,
        "child pid {child_pid} should eventually be dead after background reap"
    );
}

#[test]
#[serial]
fn grace_drain_recovers_slow_first_call_and_serves_second_request_correctly() {
    // 1回目の呼び出しだけ遅く（テスト用タイムアウトを
    // 超えて）、2回目以降は速く応答する補完関数フィクスチャで、
    // 1回目は None（グレースで daemon は生存継続）、2回目は 1回目の
    // 残留フレームをドレインしたうえで、正しい候補（2回目のリクエスト
    // に対応するもの）を返すことを検証する。
    //
    // フィクスチャ: マーカーファイルの有無で初回/以降を判別する
    // （プロセスをまたいで状態を持たせる必要があるため、zsh の変数では
    // なくファイルシステムを使う——1回目の呼び出しで sleep してから
    // マーカーを作り、2回目以降はマーカーがあるので即座に応答する）。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let tmpdir = tempfile::tempdir().unwrap();
    let marker = tmpdir.path().join("first-call-done");
    let fixture = setup_fixture(&[(
        "_jarvishtestslow",
        &format!(
            "#compdef jarvishtestslow\n\
                 if [[ ! -f {marker} ]]; then\n\
                 touch {marker}\n\
                 sleep 2\n\
                 fi\n\
                 compadd -- fastcandidate\n",
            marker = marker.display()
        ),
    )]);

    let mut daemon = ZshDaemon::spawn(
        &zsh,
        &fixture.zdotdir,
        &extra_envs_for(&fixture),
        Duration::from_secs(10),
    )
    .expect("daemon should spawn");
    let pid_before = daemon.child_pid_for_test();

    // テスト用の短いウォームタイムアウト（1回目の sleep 2 秒より短い）。
    let short_timeout = Duration::from_millis(400);

    let first = daemon.request("jarvishtestslow ", short_timeout);
    assert_eq!(
        first, None,
        "first (slow) call should yield None under the short test timeout"
    );
    assert!(
        daemon.is_alive(),
        "daemon must stay alive after the first timeout (grace)"
    );
    assert_eq!(
        daemon.child_pid_for_test(),
        pid_before,
        "grace must not respawn the daemon"
    );

    // 2回目: 残留フレームをドレインしたうえで、今回のリクエストの
    // 応答（fastcandidate）を正しく返す。ドレイン + 新リクエストの
    // 両方を賄うのに十分な予算を与える。
    let second = daemon.request("jarvishtestslow ", Duration::from_secs(5));
    let candidates = second.expect("second request should recover and return candidates");
    let values = parse_capture_output(&candidates);
    assert!(
        values.iter().any(|c| c.value == "fastcandidate"),
        "second request should see its own response, got {values:?}"
    );
    assert!(
        daemon.is_alive(),
        "daemon must still be alive after a successful drain + request"
    );
    assert_eq!(
        daemon.child_pid_for_test(),
        pid_before,
        "the same daemon process must still be serving requests (no respawn)"
    );

    // テストフィクスチャ teardown（`spawn_reaches_ready_marker`
    // のコメント参照）。
    daemon.shutdown_blocking(Duration::from_secs(2));
}

#[test]
#[serial]
fn success_between_timeouts_resets_consecutive_counter() {
    // timeout → success → timeout という並びでは、途中の success
    // がカウンタをリセットするため、2回目の timeout だけではサーキット
    // ブレーカーは作動せず、デーモンは生きたままである。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let fixture = setup_fixture(&[
        (
            "_jarvishtesthang",
            "#compdef jarvishtesthang\nsleep 30\ncompadd -- neverseen\n",
        ),
        (
            "_jarvishtestcmd",
            "#compdef jarvishtestcmd\ncompadd -- alpha beta gamma\n",
        ),
    ]);

    let mut daemon = ZshDaemon::spawn(
        &zsh,
        &fixture.zdotdir,
        &extra_envs_for(&fixture),
        Duration::from_secs(10),
    )
    .expect("daemon should spawn");
    let pid_before = daemon.child_pid_for_test();

    let request_timeout = Duration::from_millis(500);

    // 1回目: timeout (グレース、まだ生存)。
    let r1 = daemon.request("jarvishtesthang ", request_timeout);
    assert_eq!(r1, None);
    assert!(daemon.is_alive());

    // 2回目: 別の（ハングしない）コマンドへのリクエスト。まず1回目の
    // 残留フレームをドレインする必要があるが、`sleep 30` はまだ動作中
    // なのでこのドレイン自体もタイムアウトしうる——このテストの主張は
    // 「ドレインが失敗してもリクエスト自体は諦めて None を返すこと」
    // ではなく、**ドレインさえ間に合えば**カウンタがリセットされる
    // ことなので、ドレイン予算に十分な余裕を与える（`sleep 30` の残り
    // 時間を上回るテスト用タイムアウト）ことで確実にドレインを成功させる。
    let r2 = daemon.request("jarvishtestcmd ", Duration::from_secs(35));
    let candidates =
        r2.expect("second request should succeed once the stale hang frame is drained");
    let values = parse_capture_output(&candidates);
    assert!(values.iter().any(|c| c.value == "alpha"));
    assert!(
        daemon.is_alive(),
        "daemon must be alive after a successful request"
    );

    // 3回目: 再びハングするコマンド。timeout が起きるが、直前の success
    // がカウンタをリセットしているため、これは「連続1回目」に過ぎず、
    // まだ kill されない。
    let r3 = daemon.request("jarvishtesthang ", request_timeout);
    assert_eq!(r3, None);
    assert!(
        daemon.is_alive(),
        "daemon must stay alive: the counter was reset by the success in between"
    );
    assert_eq!(
        daemon.child_pid_for_test(),
        pid_before,
        "no respawn should have happened throughout"
    );

    // テストフィクスチャ teardown（`spawn_reaches_ready_marker`
    // のコメント参照）。デーモンはまだ生存中（pending_frame が残った
    // 状態）のため、明示的な有界同期 shutdown が必須。
    daemon.shutdown_blocking(Duration::from_secs(2));
}

#[test]
#[serial]
fn drop_kills_child_process() {
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let fixture = setup_fixture(&[(
        "_jarvishtestcmd",
        "#compdef jarvishtestcmd\ncompadd -- alpha\n",
    )]);

    let daemon = ZshDaemon::spawn(
        &zsh,
        &fixture.zdotdir,
        &extra_envs_for(&fixture),
        Duration::from_secs(10),
    )
    .expect("daemon should spawn");
    let child_pid = daemon.child_pid_for_test();

    // Drop 自体はバックグラウンド委譲でノンブロッキングになった
    // ため、`drop()` 呼び出し自体の所要時間ではなく、その後の
    // バックグラウンドスレッドがいずれ確実に reap することを証明する
    // （elapsed の主張は不要 — 「drop() が速く戻ること」は
    // hung_completion_times_out_and_kills_descendants で別途担保済み）。
    drop(daemon);

    let mut alive = true;
    for _ in 0..80 {
        let ret = unsafe { libc::kill(child_pid as libc::pid_t, 0) };
        if ret == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            alive = false;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive, "child pid {child_pid} should be dead after Drop");
}

#[test]
#[serial]
fn init_script_file_is_removed_on_drop() {
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let fixture = setup_fixture(&[(
        "_jarvishtestcmd",
        "#compdef jarvishtestcmd\ncompadd -- alpha\n",
    )]);

    let daemon = ZshDaemon::spawn(
        &zsh,
        &fixture.zdotdir,
        &extra_envs_for(&fixture),
        Duration::from_secs(10),
    )
    .expect("daemon should spawn");
    let script_path = daemon
        .init_script_path
        .clone()
        .expect("init_script_path should be Some while alive");
    assert!(script_path.exists());

    drop(daemon);

    // 一時ファイル削除もバックグラウンドスレッド側の reap_bundle が
    // 行うようになったため、`drop()` が戻った直後の同期確認
    // ではなく短時間ポーリングで確認する。
    let mut removed = false;
    for _ in 0..80 {
        if !script_path.exists() {
            removed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        removed,
        "init script temp file should eventually be removed after Drop"
    );
}

#[test]
#[serial]
fn request_on_dead_daemon_returns_none_without_hanging() {
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let fixture = setup_fixture(&[(
        "_jarvishtestcmd",
        "#compdef jarvishtestcmd\ncompadd -- alpha\n",
    )]);

    let mut daemon = ZshDaemon::spawn(
        &zsh,
        &fixture.zdotdir,
        &extra_envs_for(&fixture),
        Duration::from_secs(10),
    )
    .expect("daemon should spawn");

    // テストフィクスチャ teardown: このテストの主張（shutdown
    // 後の request() が即座に None を返すこと）自体は shutdown の
    // ブロッキング/非ブロッキングに依存しないため、有界同期版に置き換えて
    // 子プロセスの確実な reap を保証する（`spawn_reaches_ready_marker`
    // のコメント参照）。
    daemon.shutdown_blocking(Duration::from_secs(2));
    assert!(!daemon.is_alive());

    let start = Instant::now();
    let result = daemon.request("jarvishtestcmd ", Duration::from_secs(15));
    let elapsed = start.elapsed();

    assert_eq!(result, None);
    // full timeout (15s) を待たずに早期 return することの検証。絶対速度では
    // なく質的な差が主張なので、負荷に強い 1s を境界にする。
    assert!(
        elapsed < Duration::from_secs(1),
        "request on a dead daemon should return immediately (not wait the full timeout), \
             took {elapsed:?}"
    );
}

#[test]
#[serial]
fn spawn_with_invalid_zsh_binary_returns_err() {
    let fixture = setup_fixture(&[]);
    let result = ZshDaemon::spawn(
        Path::new("/no/such/zsh/binary/zzjarvish"),
        &fixture.zdotdir,
        &extra_envs_for(&fixture),
        Duration::from_secs(2),
    );
    assert!(result.is_err());
}

#[test]
#[serial]
fn request_after_external_sigkill_returns_none_fast_without_full_timeout() {
    // OOM killer や手動 `kill -9` のような外部要因でデーモン子プロセスが
    // 死んでいるケースを模す: テストから直接 SIGKILL を送ってから
    // request() を呼び、フルタイムアウトを待たず高速に None が返る
    // ことを確認する。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let fixture = setup_fixture(&[(
        "_jarvishtestcmd",
        "#compdef jarvishtestcmd\ncompadd -- alpha\n",
    )]);

    let mut daemon = ZshDaemon::spawn(
        &zsh,
        &fixture.zdotdir,
        &extra_envs_for(&fixture),
        Duration::from_secs(10),
    )
    .expect("daemon should spawn");

    let child_pid = daemon.child_pid_for_test();
    unsafe {
        libc::kill(child_pid as libc::pid_t, libc::SIGKILL);
    }
    // カーネルが実際に終了処理するまでの短い猶予（このポーリング自体は
    // テストのセットアップであり、request() の高速性の主張には数えない）。
    for _ in 0..40 {
        let ret = unsafe { libc::kill(child_pid as libc::pid_t, 0) };
        if ret == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    let start = Instant::now();
    let result = daemon.request("jarvishtestcmd ", Duration::from_secs(5));
    let elapsed = start.elapsed();

    assert_eq!(
        result, None,
        "request on an externally-killed daemon should yield None"
    );
    // 主張は「full timeout (5s) を待たずに早期 return する」という質的な
    // 差であって、絶対的な速度ではない。旧 150ms は負荷の高い CI ランナー
    // （2 コア）でスケジューラに 1 回プリエンプトされるだけで超過しうる
    // フレーク源だったため、timeout との差が十分に出る 1s まで緩める
    // （5s の 1/5 — try_wait() プローブが無ければ 5s 掛かるので検出力は保たれる）。
    assert!(
        elapsed < Duration::from_secs(1),
        "liveness probe should detect external kill without waiting the full 5s timeout, \
             took {elapsed:?} (a full-timeout wait would indicate the try_wait() probe is missing)"
    );
    assert!(
        !daemon.is_alive(),
        "daemon must be marked dead after detecting the external kill"
    );
}

#[test]
#[serial]
fn oversized_response_is_capped_and_marks_daemon_dead() {
    // フィクスチャ: MAX_RESPONSE_BYTES を超える出力を吐き続ける（センチネル
    // を出さない）補完関数。実運用のバグ/悪意ある補完関数を模す。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    // シェル内で大量出力させる: 十分に長い1行を大量回数 echo する
    // （compadd を経由せず、daemon_init.zsh の compadd オーバーライドが
    // 介在しない生の PTY 出力で MAX_RESPONSE_BYTES 超過を直接再現する）。
    let fixture = setup_fixture(&[(
        "_jarvishtestflood",
        "#compdef jarvishtestflood\n\
             for i in {1..200000}; do print -n 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'; done\n\
             compadd -- neverseen\n",
    )]);

    let mut daemon = ZshDaemon::spawn(
        &zsh,
        &fixture.zdotdir,
        &extra_envs_for(&fixture),
        Duration::from_secs(10),
    )
    .expect("daemon should spawn");
    let child_pid = daemon.child_pid_for_test();

    // 上限判定がタイムアウトより先に効くことを立証したいので、タイムアウト
    // 自体は「絶対に先に発火しない」水準まで広く取る。
    let request_timeout = Duration::from_secs(120);
    let result = daemon.request("jarvishtestflood ", request_timeout);

    assert_eq!(
        result, None,
        "oversized response must be treated as desync and yield None"
    );

    // 「バッファ上限が効いた（タイムアウトではない）」ことの証明には
    // **経過時間を使わない**。このテストは 200,000 行 × 32 バイト =
    // 6.4MB を zsh に生成させる重い処理で、CPU が飽和すると実測で
    // 14.9s → 28.6s と大きく揺れる。ストップウォッチによる区別は
    // 「上限で落ちたのか、タイムアウトで落ちたのか」を確率的にしか
    // 判定できず、実装が正しくても落ちるフレークになっていた。
    //
    // 代わりに因果関係で判定する: `BufferOverflow` は desync 相当として
    // **グレース対象外で即 kill** されるのに対し、クリーンなタイムアウトは
    // `MAX_CONSECUTIVE_TIMEOUTS`（= 2）に達するまでデーモンを生かしたまま
    // にする。したがって「1回のリクエスト後に死んでいる」こと自体が
    // 「タイムアウト経路ではなくバッファ上限経路を通った」ことの決定的な
    // 証拠であり、経過時間より厳密な検証になっている。
    assert!(
            !daemon.is_alive(),
            "daemon must be marked dead after ONE oversized response — a mere timeout \
             would have left it alive for the grace round (MAX_CONSECUTIVE_TIMEOUTS = {}), \
             so this also proves the buffer cap tripped rather than the {request_timeout:?} timeout",
            MAX_CONSECUTIVE_TIMEOUTS
        );

    // テストフィクスチャ teardown: バッファ上限超過は
    // `request()` 内部で `mark_dead_and_kill`（非ブロッキング、kill/reap
    // をバックグラウンドスレッドへ委譲）を経由するため、テスト関数を
    // 抜けた時点では reap が完了している保証がない（`spawn_reaches_
    // ready_marker` のコメント参照）。有界ポーリングで実際に子プロセスが
    // 死ぬまで待ってからテストを終える。
    assert!(
        wait_for_pid_death_for_test(child_pid),
        "child pid {child_pid} should eventually be reaped by the background thread"
    );
}

#[test]
#[serial]
fn echo_is_disabled_on_daemon_pty_slave() {
    // termios レベルで直接、ECHO が実際にオフになっていることを検証する
    // 。`daemon_init.zsh` の `zsh -i` は ZLE
    // （zsh のインタラクティブ行編集システム）を使っており、ZLE は
    // 端末の ECHO フラグとは独立に、入力バッファの再描画を自前で
    // 常に行う（実機検証済み: `tcsetattr` で ECHO を明示的に消しても
    // `zle complete-word` を経由した書き込みは ZLE 自身の再描画により
    // 引き続き画面に現れる — これは zsh の仕様であり termios では
    // 抑止できない）。そのため「応答ストリームに送信行が一切現れない」
    // ことをこのテストの主張にはできない（別テスト
    // `echo_off_reduces_duplicate_marker_occurrences` が、ECHO を切る
    // ことで**カーネル側の生エコーによる重複**が消えることを検証する）。
    // ここでは fix 自体が適用されていること — `disable_echo` が
    // slave fd の termios ECHO ビットを実際に落としていること — を
    // 直接 `tcgetattr` で確認する。
    use nix::sys::termios::LocalFlags;

    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let fixture = setup_fixture(&[(
        "_jarvishtestcmd",
        "#compdef jarvishtestcmd\ncompadd -- alpha\n",
    )]);

    let (master, slave) = create_daemon_pty().expect("daemon pty should be created");
    let attrs = termios::tcgetattr(slave.as_fd()).expect("tcgetattr should succeed");
    assert!(
        !attrs.local_flags.contains(LocalFlags::ECHO),
        "ECHO must be cleared on the daemon PTY slave immediately after create_daemon_pty()"
    );
    drop(master);
    drop(slave);

    // 実際に spawn() 経由で組み立てたデーモンでも同じ保証が効くことを
    // 一応 end-to-end で確認しておく（daemon が生きて壊れていないこと
    // 自体の回帰チェックも兼ねる）。
    let mut daemon = ZshDaemon::spawn(
        &zsh,
        &fixture.zdotdir,
        &extra_envs_for(&fixture),
        Duration::from_secs(10),
    )
    .expect("daemon should spawn");
    let result = daemon.request("jarvishtestcmd ", Duration::from_secs(15));
    assert!(result.is_some(), "daemon should still serve completions");
    // テストフィクスチャ teardown（`spawn_reaches_ready_marker`
    // のコメント参照）。
    daemon.shutdown_blocking(Duration::from_secs(2));
}

#[test]
#[serial]
fn echo_off_reduces_duplicate_marker_occurrences_vs_echo_on() {
    // 実測可能な保証: ECHO を切ると、送信ペイロードのカーネル側
    // 生エコー（tty line discipline による即時反響）は消える。ZLE 自身
    // の再描画は ECHO 設定に関わらず残る（上のテストのコメント参照）
    // ため、"0 回" を主張することはできないが、"ECHO オフ時の出現回数は
    // ECHO オン時より厳密に少ない" ことは実機で決定的に検証できる
    // （実機検証: ECHO オンで2回、オフで1回 — カーネル生エコー分だけ
    // 減る）。ここでは `create_daemon_pty`（ECHO オフ適用済み）と、
    // ECHO をあえて有効化し直した比較用 PTY の両方で同じマーカーを
    // 送り込み、出現回数を比較する。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let fixture = setup_fixture(&[(
        "_jarvishtestcmd",
        "#compdef jarvishtestcmd\ncompadd -- alpha\n",
    )]);

    let echo_off_count = capture_marker_occurrences(&zsh, &fixture, false);
    let echo_on_count = capture_marker_occurrences(&zsh, &fixture, true);

    eprintln!(
            "echo_off marker occurrences: {echo_off_count}, echo_on marker occurrences: {echo_on_count}"
        );
    assert!(
        echo_off_count < echo_on_count,
        "disabling ECHO must strictly reduce marker duplication in the raw PTY stream: \
             echo_off={echo_off_count}, echo_on={echo_on_count}"
    );
}

/// [`echo_off_reduces_duplicate_marker_occurrences_vs_echo_on`] 専用の
/// ヘルパー: `zsh -i` を直接 spawn し（`ZshDaemon::spawn` は常に ECHO を
/// オフにするため使えない）、`force_echo_on` の指示に応じて PTY slave
/// の ECHO を制御したうえでユニークマーカーを含む行を送り込み、応答
/// ストリーム中のマーカー出現回数を返す。
fn capture_marker_occurrences(zsh: &Path, fixture: &TestFixture, force_echo_on: bool) -> usize {
    use nix::sys::termios::{LocalFlags, SetArg};

    let (mut master, slave) = create_daemon_pty().expect("daemon pty should be created");
    if force_echo_on {
        let mut attrs = termios::tcgetattr(slave.as_fd()).expect("tcgetattr should succeed");
        attrs.local_flags.insert(LocalFlags::ECHO);
        termios::tcsetattr(slave.as_fd(), SetArg::TCSANOW, &attrs)
            .expect("tcsetattr should succeed");
    }

    let slave_raw_fd = slave.as_raw_fd();
    let stdin_fd = unsafe { libc::dup(slave_raw_fd) };
    let stdout_fd = unsafe { libc::dup(slave_raw_fd) };
    let stderr_fd = unsafe { libc::dup(slave_raw_fd) };

    let mut command = Command::new(zsh);
    command
        .arg("-i")
        .env("ZDOTDIR", &fixture.zdotdir)
        .env("TERM", "dumb")
        .envs(
            extra_envs_for(fixture)
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str())),
        )
        .stdin(unsafe { Stdio::from_raw_fd(stdin_fd) })
        .stdout(unsafe { Stdio::from_raw_fd(stdout_fd) })
        .stderr(unsafe { Stdio::from_raw_fd(stderr_fd) });
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("zsh -i should spawn");
    drop(slave);

    // シェルが起動しプロンプトを出すまで少し待つ（正確な待ち方をせず
    // 固定 sleep なのは、このヘルパーが比較専用の低リスクなテスト
    // ユーティリティであり、多少の余裕時間で十分なため）。
    std::thread::sleep(Duration::from_millis(800));
    let payload = b"\x15echo uniqechomarkerXYZ\t";
    master
        .write_all(payload)
        .expect("write to master should succeed");
    std::thread::sleep(Duration::from_millis(800));

    let mut raw = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match read_available(&mut master, Duration::from_millis(200)) {
            Some(chunk) if !chunk.is_empty() => raw.extend_from_slice(&chunk),
            _ => break,
        }
    }

    let _ = child.kill();
    let _ = child.wait();

    let raw_text = String::from_utf8_lossy(&raw);
    raw_text.matches("uniqechomarkerXYZ").count()
}

// ── shutdown_blocking: exit/exec 経路専用の有界同期 shutdown ──

#[test]
#[serial]
fn shutdown_blocking_reaps_within_deadline_and_kills_child() {
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let fixture = setup_fixture(&[(
        "_jarvishtestcmd",
        "#compdef jarvishtestcmd\ncompadd -- alpha\n",
    )]);

    let mut daemon = ZshDaemon::spawn(
        &zsh,
        &fixture.zdotdir,
        &extra_envs_for(&fixture),
        Duration::from_secs(10),
    )
    .expect("daemon should spawn");
    let child_pid = daemon.child_pid_for_test();

    daemon.shutdown_blocking(Duration::from_secs(2));

    assert!(!daemon.is_alive());
    // 有界同期版なので、戻ってきた時点で reap が完了している（または
    // deadline に達している）はず — ポーリングなしで即座に ESRCH を
    // 確認できることが非同期版との違いの直接証拠。
    let ret = unsafe { libc::kill(child_pid as libc::pid_t, 0) };
    let is_dead = ret == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
    assert!(
        is_dead,
        "child pid {child_pid} should already be reaped when shutdown_blocking returns"
    );
}

//
// 実 zsh を一切必要としない純粋なファイルシステムテスト。ランダムな
// ファイル名成分のおかげでテスト同士が衝突しないため #[serial] も不要。

#[test]
fn write_init_script_refuses_preexisting_symlink_at_target_pattern() {
    // 攻撃者が「pid + 総当たりのランダム値」のパターンへ事前にシンボ
    // リックリンクを仕込んでおいたケースを直接は再現できない
    // （ランダム値は予測不能なため）が、`create_new` が「対象パスに
    // 何であれ既に存在する（シンボリックリンクを含む）場合は常に拒否
    // する」という契約そのものを検証する: 実際に write_init_script が
    // 生成するのと同じ命名規則のパスへ事前にシンボリックリンクを
    // 置いておき、その特定のパスへの書き込みを試みても symlink を
    // たどらず失敗することを、write_init_script が内部で使う
    // OpenOptions::create_new のセマンティクスとして直接確認する。
    let tmpdir = tempfile::tempdir().unwrap();
    let bridge_dir = tmpdir.path().join("zsh-bridge");
    fs::create_dir_all(&bridge_dir).unwrap();

    // write_init_script と同じ命名パターンで、攻撃者が用意しうる
    // シンボリックリンクを再現する。
    let evil_target = tmpdir.path().join("evil-target.zsh");
    let victim_path = bridge_dir.join(format!(
        ".daemon_init.{}.{:016x}.zsh",
        std::process::id(),
        0u64
    ));
    std::os::unix::fs::symlink(&evil_target, &victim_path).unwrap();

    // create_new(true) は「対象パスに既に何か（シンボリックリンクを
    // 含む）が存在する」場合は常にエラーになる契約を直接検証する。
    let result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&victim_path);
    assert!(
        result.is_err(),
        "create_new must refuse to open a path that is already a symlink"
    );
    assert!(
        !evil_target.exists(),
        "the symlink target must never be written through"
    );

    // write_init_script 自体は毎回ランダムなファイル名を生成するため、
    // 事前に置かれたこのシンボリックリンクとは衝突せず正常に成功する
    // （= 予測できないファイル名であること自体が防御の一部）。
    let path = write_init_script(&bridge_dir).expect("write_init_script should succeed");
    assert_ne!(
        path, victim_path,
        "write_init_script must not reuse the attacker-planted path"
    );
    assert!(!fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_symlink());
    let _ = fs::remove_file(&path);
}

#[test]
fn write_init_script_refuses_preexisting_regular_file_then_succeeds_with_new_random_name() {
    let tmpdir = tempfile::tempdir().unwrap();
    let bridge_dir = tmpdir.path().join("zsh-bridge");
    fs::create_dir_all(&bridge_dir).unwrap();

    // 事前に通常ファイルを、write_init_script が今後生成しうる
    // ファイル名パターンへ置いておく。create_new(true) はシンボリック
    // リンクだけでなく既存の通常ファイルも同様に拒否する
    // （O_CREAT | O_EXCL のセマンティクス）ことを直接確認する。
    let preexisting = bridge_dir.join(format!(
        ".daemon_init.{}.{:016x}.zsh",
        std::process::id(),
        0u64
    ));
    fs::write(&preexisting, "pre-existing content").unwrap();

    let result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&preexisting);
    assert!(
        result.is_err(),
        "create_new must refuse an already-existing regular file"
    );
    assert_eq!(
        fs::read_to_string(&preexisting).unwrap(),
        "pre-existing content",
        "the pre-existing file's content must not be overwritten"
    );

    // write_init_script 自体は、ランダム成分のおかげで上記の
    // 事前配置ファイルとは別名になり正常に成功する。
    let path = write_init_script(&bridge_dir).expect("write_init_script should succeed");
    assert_ne!(path, preexisting);
    assert_eq!(fs::read_to_string(&path).unwrap(), DAEMON_INIT_SCRIPT);
    let _ = fs::remove_file(&path);
}

#[test]
fn write_init_script_creates_file_with_0600_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let tmpdir = tempfile::tempdir().unwrap();
    let bridge_dir = tmpdir.path().join("zsh-bridge");

    let path = write_init_script(&bridge_dir).expect("write_init_script should succeed");
    let mode = fs::metadata(&path).unwrap().permissions().mode();
    // 下位 9 ビット（パーミッションビットのみ）を比較する。
    assert_eq!(
        mode & 0o777,
        0o600,
        "init script must be owner-read/write only (0600), got {:o}",
        mode & 0o777
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn write_init_script_two_calls_yield_distinct_paths_and_both_contain_the_script() {
    // 同一 pid 内で複数回呼んでも（ランダム成分により）パスが衝突
    // しないことを確認する（デーモン再 spawn のたびに呼ばれるため
    // 重要な性質）。
    let tmpdir = tempfile::tempdir().unwrap();
    let bridge_dir = tmpdir.path().join("zsh-bridge");

    let path1 = write_init_script(&bridge_dir).expect("first call should succeed");
    let path2 = write_init_script(&bridge_dir).expect("second call should succeed");
    assert_ne!(
        path1, path2,
        "two calls must not collide on the same filename"
    );
    assert_eq!(fs::read_to_string(&path1).unwrap(), DAEMON_INIT_SCRIPT);
    assert_eq!(fs::read_to_string(&path2).unwrap(), DAEMON_INIT_SCRIPT);
    let _ = fs::remove_file(&path1);
    let _ = fs::remove_file(&path2);
}

// ── 残骸 init スクリプトの掃除テスト ──

#[test]
fn parse_init_script_pid_accepts_both_formats() {
    // 新形式（pid + ランダム）と旧形式（pid のみ）の両方から pid を拾う。
    assert_eq!(
        parse_init_script_pid(".daemon_init.1234.8d9e13a523321077.zsh"),
        Some(1234)
    );
    assert_eq!(parse_init_script_pid(".daemon_init.57763.zsh"), Some(57763));
}

#[test]
fn parse_init_script_pid_rejects_unrelated_names() {
    for name in [
        ".zshrc",
        "daemon_init.123.zsh",          // 先頭のドットが無い
        ".daemon_init.zsh",             // pid 部分が無い
        ".daemon_init..zsh",            // pid が空
        ".daemon_init.abc.zsh",         // pid が数値でない
        ".daemon_init.123.zsh.bak",     // 拡張子が違う
        ".daemon_init.-1.deadbeef.zsh", // 負値は pid ではない
    ] {
        assert_eq!(
            parse_init_script_pid(name),
            None,
            "{name} should not be treated as an init script"
        );
    }
}

#[test]
fn cleanup_removes_only_dead_pid_scripts() {
    let tmpdir = tempfile::tempdir().unwrap();
    let bridge_dir = tmpdir.path();

    // 1) 確実に存在しない pid の残骸（削除されるべき）。
    //    pid 1 は必ず生きているので使わない。十分大きな未使用 pid を
    //    実際に生存確認して選ぶ（環境依存で偶然使われている可能性を排除）。
    let dead_pid = (1..=50_000u32)
        .rev()
        .find(|&p| !process_is_alive(p))
        .expect("some pid in range must be free");
    let stale_new = bridge_dir.join(format!(".daemon_init.{dead_pid}.deadbeefdeadbeef.zsh"));
    let stale_old = bridge_dir.join(format!(".daemon_init.{dead_pid}.zsh"));
    fs::write(&stale_new, "x").unwrap();
    fs::write(&stale_old, "x").unwrap();

    // 2) 自分自身の pid（生きている → 残すべき）。
    let own = bridge_dir.join(format!(
        ".daemon_init.{}.cafecafecafecafe.zsh",
        std::process::id()
    ));
    fs::write(&own, "x").unwrap();

    // 3) 命名規則に合致しない無関係なファイル（触ってはいけない）。
    let zshrc = bridge_dir.join(".zshrc");
    let unrelated = bridge_dir.join("notes.txt");
    fs::write(&zshrc, "x").unwrap();
    fs::write(&unrelated, "x").unwrap();

    let removed = cleanup_stale_init_scripts(bridge_dir);

    assert_eq!(removed, 2, "both stale scripts should be removed");
    assert!(
        !stale_new.exists(),
        "stale new-format script should be gone"
    );
    assert!(
        !stale_old.exists(),
        "stale old-format script should be gone"
    );
    assert!(own.exists(), "must not delete a live process's script");
    assert!(zshrc.exists(), "must not touch the bridge .zshrc");
    assert!(unrelated.exists(), "must not touch unrelated files");
}

#[test]
fn is_stale_compdump_name_matches_only_rename_temp_files() {
    // `.zcompdump.<host>.<pid>` = compinit のリネーム途中の一時ファイル。
    assert!(is_stale_compdump_name(".zcompdump.MacBookPro.3262"));
    // ホスト名にドットを含むケース（実環境で観測した形）。
    assert!(is_stale_compdump_name(
        ".zcompdump.macnoMacBook-Pro.local.13066"
    ));

    // 完成品のキャッシュ本体は残す（消すと compinit がやり直しになる）。
    assert!(!is_stale_compdump_name(".zcompdump"));
    // 別プレフィックスのダンプは対象外。
    assert!(!is_stale_compdump_name(".zcompdump_capture"));
    assert!(!is_stale_compdump_name(".zcompdump_capture.Mac.123"));
    // pid 部分が数値でないものは対象外。
    assert!(!is_stale_compdump_name(".zcompdump.MacBookPro.abc"));
    // 無関係なファイル。
    assert!(!is_stale_compdump_name(".zshrc"));
    assert!(!is_stale_compdump_name("notes.txt"));
}

#[test]
fn cleanup_compdumps_removes_temps_but_keeps_the_real_cache() {
    let tmpdir = tempfile::tempdir().unwrap();
    let dir = tmpdir.path();

    let temp1 = dir.join(".zcompdump.MacBookPro.3262");
    let temp2 = dir.join(".zcompdump.macnoMacBook-Pro.local.13066");
    let real_cache = dir.join(".zcompdump");
    let capture = dir.join(".zcompdump_capture");
    let zshrc = dir.join(".zshrc");
    for p in [&temp1, &temp2, &real_cache, &capture, &zshrc] {
        fs::write(p, "x").unwrap();
    }

    let removed = cleanup_stale_compdumps(dir);

    assert_eq!(
        removed, 2,
        "only the two rename temp files should be removed"
    );
    assert!(!temp1.exists());
    assert!(!temp2.exists());
    assert!(
        real_cache.exists(),
        "the real .zcompdump cache must be kept — deleting it slows the next compinit"
    );
    assert!(capture.exists(), "must not touch .zcompdump_capture");
    assert!(zshrc.exists(), "must not touch the bridge .zshrc");
}

#[test]
fn cleanup_compdumps_on_missing_directory_is_noop() {
    let tmpdir = tempfile::tempdir().unwrap();
    let missing = tmpdir.path().join("does-not-exist");
    assert_eq!(cleanup_stale_compdumps(&missing), 0);
}

#[test]
fn cleanup_on_missing_directory_is_noop() {
    let tmpdir = tempfile::tempdir().unwrap();
    let missing = tmpdir.path().join("does-not-exist");
    assert_eq!(cleanup_stale_init_scripts(&missing), 0);
}

#[test]
fn process_is_alive_reports_self_as_alive() {
    assert!(process_is_alive(std::process::id()));
}

// ── cwd 同期（デーモンの作業ディレクトリ追従）テスト ──

#[test]
fn single_quote_wraps_plain_string() {
    assert_eq!(single_quote("/tmp/foo"), "'/tmp/foo'");
}

#[test]
fn single_quote_escapes_embedded_single_quotes() {
    // `'\''` パターン: クォートを閉じ、エスケープした ' を置き、再び開く。
    assert_eq!(single_quote("/tmp/it's"), r"'/tmp/it'\''s'");
}

#[test]
fn single_quote_leaves_expansion_chars_literal() {
    // シングルクォート内では展開が起きないため、これらは追加の
    // エスケープ無しでリテラルとして安全に渡る。
    for s in ["/tmp/$HOME", "/tmp/`id`", "/tmp/*", "/tmp/~x", "/tmp/a b"] {
        let quoted = single_quote(s);
        assert!(quoted.starts_with('\'') && quoted.ends_with('\''));
        assert_eq!(&quoted[1..quoted.len() - 1], s);
    }
}

/// デーモンが「起動後に jarvish が `cd` した先」のファイルを補完すること
/// を検証する回帰テスト（本バグの核心）。
///
/// 修正前は、デーモンが spawn 時の cwd に固まったままだったため、
/// `dir_a` の中身（`alpha_from_a`）が返ってしまっていた。
#[test]
#[serial]
fn completion_follows_jarvish_cwd_after_chdir() {
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    // `_files` を使う補完定義（相対パスを cwd 基準で解決する）。
    let fixture = setup_fixture(&[("_jarvishcwd", "#compdef jarvishcwd\n_files\n")]);

    let workdir = tempfile::tempdir().unwrap();
    let dir_a = workdir.path().join("dir_a");
    let dir_b = workdir.path().join("dir_b");
    fs::create_dir_all(&dir_a).unwrap();
    fs::create_dir_all(&dir_b).unwrap();
    fs::write(dir_a.join("alpha_from_a"), "").unwrap();
    fs::write(dir_b.join("bravo_from_b"), "").unwrap();

    let original = env::current_dir().unwrap();

    // dir_a でデーモンを spawn（この時点の cwd を継承する）。
    env::set_current_dir(&dir_a).unwrap();
    let spawned = ZshDaemon::spawn(
        &zsh,
        &fixture.zdotdir,
        &extra_envs_for(&fixture),
        Duration::from_secs(10),
    );

    // jarvish が dir_b へ移動（デーモンには自動では伝わらない）。
    let result = spawned.map(|mut daemon| {
        env::set_current_dir(&dir_b).unwrap();
        daemon.request("jarvishcwd ", Duration::from_secs(5))
    });

    // 他テストへ影響させないため cwd を必ず戻す。
    env::set_current_dir(&original).unwrap();

    let response = result
        .expect("daemon should spawn")
        .expect("request should return a frame");

    assert!(
        response.contains("bravo_from_b"),
        "completion should list files from the CURRENT directory (dir_b), got: {response:?}"
    );
    assert!(
        !response.contains("alpha_from_a"),
        "completion must NOT list files from the daemon's spawn directory (dir_a), \
             got: {response:?}"
    );
}

/// cwd が変わっていない場合は `cd` 行を送らない（無駄な往復をしない）
/// ことを、内部状態の追従で検証する。
#[test]
#[serial]
fn sync_cwd_is_noop_when_directory_is_unchanged() {
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let fixture = setup_fixture(&[("_jarvishcwd", "#compdef jarvishcwd\n_files\n")]);

    let workdir = tempfile::tempdir().unwrap();
    let original = env::current_dir().unwrap();
    env::set_current_dir(workdir.path()).unwrap();

    let spawned = ZshDaemon::spawn(
        &zsh,
        &fixture.zdotdir,
        &extra_envs_for(&fixture),
        Duration::from_secs(10),
    );

    let observed = spawned.map(|mut daemon| {
        // spawn 直後は継承した cwd が記録されている。
        let at_spawn = daemon.cwd.clone();
        // cwd を変えずに sync_cwd を呼んでも記録は変わらない。
        daemon.sync_cwd();
        let after_noop_sync = daemon.cwd.clone();
        (at_spawn, after_noop_sync, daemon.is_alive())
    });

    env::set_current_dir(&original).unwrap();

    let (at_spawn, after_noop_sync, alive) = observed.expect("daemon should spawn");
    assert!(at_spawn.is_some(), "spawn should record the inherited cwd");
    assert_eq!(
        at_spawn, after_noop_sync,
        "sync_cwd must not change the recorded cwd when the directory is unchanged"
    );
    assert!(alive, "a no-op sync must not kill the daemon");
}
