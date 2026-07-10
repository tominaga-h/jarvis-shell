//! 外部コマンド実行ランナー — ハードタイムアウト + kill 付き
//!
//! carapace / zsh ブリッジ等、Phase 2a 以降の外部プロセス系プロバイダの
//! 共通基盤。completer は reedline の `Completer::complete` 内、つまり
//! UI スレッド同期実行のため、外部プロセスの実行時間はこのランナーの
//! タイムアウト値がそのままブロッキング予算になる（tokio 等の非同期化はしない。
//! `prompt/` の既存の同期実行慣行に合わせる）。
//!
//! # ゾンビプロセスを残さない保証
//! 子プロセスの `wait_with_output()` は専用スレッドで呼ぶため、メイン側が
//! `recv_timeout` でタイムアウトして `None` を返した後も、そのスレッドは
//! バックグラウンドで生き続け、いずれ子プロセスの終了を検知して reap する
//! （kill 後は SIGKILL により即座に終了するため実質即時）。メインスレッドが
//! 先に抜けても Rust の `Child` は Drop 時に kill しない（子プロセス自体は
//! 別スレッドが `wait_with_output()` で既に刈り取っている想定）。
//!
//! # プロセスグループ全体を kill する（孫プロセス対策）
//! carapace 等の外部補完プロバイダは、内部で `git` 等の実プロセスを
//! さらに spawn することがある（carapace が発行する孫プロセス）。直接の
//! 子プロセスの pid だけを kill しても、孫プロセスは carapace の pgid を
//! 共有したまま孤児化して生き残り、最悪ハングし続ける。これを防ぐため
//! `unix` では spawn 時に子プロセスを新しいプロセスグループのリーダーに
//! し（`command.process_group(0)`）、タイムアウト時は `kill(-pid, SIGKILL)`
//! （負の pid = プロセスグループ全体への送信）でグループごと確実に
//! 終了させる。
//!
//! ## それでも届かないケース（zsh ブリッジの zpty 子）と対処
//! グループ kill は「子プロセスが `fork` + `exec` だけで増やした孫」には
//! 有効だが、**孫プロセス自身が新しいセッション/プロセスグループを作る**
//! ケースには届かない。zsh ブリッジ（[`super::zsh_bridge`]）が使う
//! `zpty` はまさにこれで、`zpty` が起動する内側の zsh は PTY 経由の
//! 新しいプロセスグループのリーダーになり、外側 zsh の pgid には属さない。
//! そのため `kill(-outer_pgid, SIGKILL)` は内側の zsh には届かず、内側の
//! zsh が HUP/TERM を trap/ignore する補完関数を経由している場合、
//! 通常は PTY 破棄に伴う SIGHUP で死ぬところが、それすら効かず永久に
//! 孤児として残ってしまう可能性がある。これに対処するため、タイムアウト
//! 時は kill 前に `pid` の子孫プロセス全体（[`collect_descendants`]、
//! `ps -axo pid=,ppid=` を辿る再帰的な pgrep 相当の探索）を収集しておき、
//! 通常のグループ kill に加えて収集済みの各子孫についても
//! `kill(-descendant_pgid, SIGKILL)` と `kill(descendant_pid, SIGKILL)`
//! の両方を送る（`zpty` 内側 zsh のような別 pgid のプロセスも確実に殺す）。

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

/// `program` を `args` / `envs` 付きで起動し、`timeout` 以内に正常終了（exit
/// code 0）した場合のみ stdout を `String` として返す。
///
/// 以下のいずれの場合も `None`:
/// - `timeout` 以内にプロセスが終了しなかった（`unix` では子プロセスは
///   専用のプロセスグループのリーダーとして spawn されており、タイムアウト
///   時はそのプロセスグループ全体に SIGKILL を送る。直接の子プロセスだけ
///   でなく、子プロセスがさらに spawn した孫プロセス — carapace が実行する
///   git 等 — も道連れで終了する。加えて、kill 前に子孫プロセス全体を
///   収集しておき、`zpty` 内側の zsh のように独自の pgid を持つ子孫にも
///   個別に SIGKILL を送る（モジュール冒頭ドキュメントの「それでも届かない
///   ケース」参照）ため、孤児化して残り続けることはない）
/// - プロセスの spawn 自体に失敗した（バイナリが存在しない等）
/// - プロセスが非ゼロで終了した
///
/// stdout が非 UTF-8 の場合は `String::from_utf8_lossy` で置換文字混じりの
/// 文字列に変換する（`None` にはしない — carapace 等の出力は基本 UTF-8 だが、
/// 一部候補に不正なバイト列が混入しても他の候補まで丸ごと failure 扱いに
/// したくないため）。
pub(crate) fn run_external_capped(
    program: &Path,
    args: &[String],
    envs: &[(String, String)],
    timeout: Duration,
) -> Option<String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .envs(envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // 子プロセスを新しいプロセスグループのリーダーにする。これにより
    // タイムアウト時に `kill(-pid, SIGKILL)` で子プロセスが spawn した
    // 孫プロセス（carapace の場合は git 等）もまとめて確実に kill できる。
    #[cfg(unix)]
    command.process_group(0);

    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            tracing::debug!("run_external_capped: spawn failed for {program:?}: {err}");
            return None;
        }
    };
    let pid = child.id();

    let (tx, rx) = mpsc::channel();
    // 子プロセスの reap は必ずこのスレッドが担う。メインスレッドが
    // recv_timeout でタイムアウトして先に抜けても、このスレッドは
    // wait_with_output() が返るまで生き続けてゾンビ化を防ぐ。
    std::thread::spawn(move || {
        let output = child.wait_with_output();
        // 受信側が既にタイムアウトして drop している場合 send は失敗するが、
        // その時点で reap は完了しているので無視してよい。
        let _ = tx.send(output);
    });

    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) => {
            if !output.status.success() {
                tracing::debug!(
                    "run_external_capped: {program:?} exited with {:?}",
                    output.status.code()
                );
                return None;
            }
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(Err(err)) => {
            tracing::debug!("run_external_capped: wait failed for {program:?}: {err}");
            None
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            kill_tree(pid);
            None
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => None,
    }
}

/// `pid` のプロセスグループ、および `pid` の子孫プロセス全体（別 pgid の
/// ものを含む）を kill する。
///
/// 通常のグループ kill（`kill(-pid, SIGKILL)`）は `fork` だけで増えた
/// 孫プロセスには効くが、`zpty` のように孫プロセス自身が新しい
/// プロセスグループ/セッションを作るケースには届かない（モジュール冒頭
/// ドキュメント参照）。そのため **kill する前に** `pid` の子孫プロセス
/// 全体を [`collect_descendants`] で収集しておき、通常のグループ kill に
/// 加えて収集済みの各子孫についても `kill(-descendant_pgid, SIGKILL)` と
/// `kill(descendant_pid, SIGKILL)` の両方を送る。
///
/// 収集を kill より前に行うのは、kill 後に子孫プロセスツリーを辿ろうと
/// すると reap 済みで `ps` から消えてしまい後追いできなくなるため（先に
/// 収集 → kill の順序が必須）。
#[cfg(unix)]
fn kill_tree(pid: u32) {
    // 子孫は kill する前に収集する（kill 後は ps から消えて辿れなくなる）。
    let descendants = collect_descendants(pid);

    kill_pid(pid);

    for descendant in descendants {
        // 別 pgid を持つ子孫（`zpty` 内側の zsh 等）に届かせるため、
        // プロセスグループ全体への送信と pid 単体への送信の両方を試みる。
        // どちらも ESRCH（既に死んでいる）は正常なレースとして無視する。
        let _ = unsafe { libc::kill(-(descendant as libc::pid_t), libc::SIGKILL) };
        let _ = unsafe { libc::kill(descendant as libc::pid_t, libc::SIGKILL) };
    }
}

/// `pid` が属するプロセスグループ全体に `SIGKILL` を送る。
///
/// `command.process_group(0)` により `pid` は自身のプロセスグループの
/// リーダー（pgid == pid）になっているため、`kill(-pid, SIGKILL)`（負の
/// pid はプロセスグループ全体への送信を意味する）で直接の子プロセスだけ
/// でなく、そのプロセスが spawn した孫プロセス（carapace が発行する
/// git 等）もまとめて終了させる。プロセスが既に終了していた場合のエラーは
/// 無視する（reap 済みなら送信対象が既に存在しない = 正常なレース）。
#[cfg(unix)]
fn kill_pid(pid: u32) {
    let ret = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
    if ret != 0 {
        let err = io::Error::last_os_error();
        tracing::debug!("run_external_capped: kill(-{pid}) failed: {err}");
    }
}

/// `root` の子孫プロセス pid を全て収集する（`root` 自身は含まない）。
///
/// `ps -axo pid=,ppid=` の全プロセス一覧を1回取得し、`ppid -> pid` の
/// 隣接リストを組んでから `root` から幅優先探索で辿る（`pgrep -P` を
/// プロセス数分だけ繰り返し呼ぶより1回の `ps` 呼び出しで済み、かつ
/// 追加クレート不要）。macOS の `ps` は BSD 系オプション（`-axo`）を
/// サポートするためこの実装で動く。`ps` 自体の実行に失敗した場合は
/// 空リストを返す（グループ kill だけで縮退運転する = 既存の group-kill
/// のみだった時点から機能が退行することはない）。
#[cfg(unix)]
fn collect_descendants(root: u32) -> Vec<u32> {
    let output = match Command::new("ps").args(["-axo", "pid=,ppid="]).output() {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            tracing::debug!(
                "run_external_capped: ps exited with {:?} while collecting descendants of {root}",
                output.status.code()
            );
            return Vec::new();
        }
        Err(err) => {
            tracing::debug!(
                "run_external_capped: failed to run ps while collecting descendants of {root}: {err}"
            );
            return Vec::new();
        }
    };
    let text = String::from_utf8_lossy(&output.stdout);

    // ppid -> 直接の子 pid 一覧。
    let mut children_of: std::collections::HashMap<u32, Vec<u32>> =
        std::collections::HashMap::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid_str), Some(ppid_str)) = (fields.next(), fields.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid)) = (pid_str.parse::<u32>(), ppid_str.parse::<u32>()) else {
            continue;
        };
        children_of.entry(ppid).or_default().push(pid);
    }

    // root から幅優先探索。プロセスツリーに循環は存在しない前提だが、
    // 万一の異常データでの無限ループを避けるため訪問済み集合で防御する。
    let mut visited: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut queue: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
    queue.push_back(root);
    let mut descendants = Vec::new();
    while let Some(current) = queue.pop_front() {
        let Some(children) = children_of.get(&current) else {
            continue;
        };
        for &child in children {
            if visited.insert(child) {
                descendants.push(child);
                queue.push_back(child);
            }
        }
    }
    descendants
}

#[cfg(not(unix))]
fn kill_tree(_pid: u32) {
    // Windows 等では子プロセス kill の別実装が必要になるが、Phase 2a の
    // 外部補完プロバイダは carapace / zsh ブリッジいずれも unix 前提のため
    // 現状は未実装（no-op）。
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[cfg(unix)]
    #[test]
    fn success_returns_stdout() {
        let out = run_external_capped(
            Path::new("/bin/echo"),
            &["hello".to_string()],
            &[],
            Duration::from_secs(1),
        );
        let out = out.expect("echo should succeed");
        assert!(out.contains("hello"), "unexpected stdout: {out:?}");
    }

    #[cfg(unix)]
    #[test]
    fn timeout_returns_none_and_returns_quickly() {
        let start = Instant::now();
        let out = run_external_capped(
            Path::new("/bin/sleep"),
            &["5".to_string()],
            &[],
            Duration::from_millis(100),
        );
        let elapsed = start.elapsed();

        assert!(out.is_none(), "sleeping process should time out to None");
        assert!(
            elapsed < Duration::from_secs(1),
            "call should return well under 1s, took {elapsed:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn killed_on_timeout_process_is_really_dead() {
        // pid を取得できるよう、内部の spawn ロジックをここでも直接使う
        // （run_external_capped は pid を外部に返さない設計のため、テスト用に
        // 同じ spawn+kill の流れを再現して「本当に死んでいるか」を確認する）。
        // 本番と同じく process_group(0) を設定する（kill_pid は -pid で
        // プロセスグループ全体に送るため、揃えないと対象がずれる）。
        let mut command = Command::new("/bin/sleep");
        command
            .arg("5")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let child = command.spawn().expect("failed to spawn /bin/sleep");
        let pid = child.id();

        std::thread::spawn(move || {
            let _ = child.wait_with_output();
        });

        kill_pid(pid);

        // 固定 sleep + 単発 assert は CI 負荷下でリーピングが遅れるとフレーキー
        // になるため、timeout_kills_grandchild_process_spawned_by_child と
        // 同じポーリングパターンで ESRCH になるまで繰り返し確認する。
        let mut ret = -1;
        let mut err = io::Error::last_os_error();
        for _ in 0..20 {
            ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
            err = io::Error::last_os_error();
            if ret == -1 && err.raw_os_error() == Some(libc::ESRCH) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        assert_eq!(
            ret, -1,
            "kill(pid, 0) should fail once the process is reaped"
        );
        assert_eq!(
            err.raw_os_error(),
            Some(libc::ESRCH),
            "expected ESRCH (no such process) after kill+reap, got {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn kill_pid_on_already_reaped_process_does_not_panic() {
        // /usr/bin/true を spawn し、wait() で完全に reap してから kill_pid を
        // 呼ぶ。対象 pid は既に存在しないため kill(-pid, SIGKILL) は ESRCH
        // で失敗するはずだが、kill_pid はこれを無視してログ出力のみ行い、
        // パニックしないことを確認する（run_external_capped のタイムアウト
        // 経路とプロセスの自然終了が競合した場合の安全性）。
        let mut command = Command::new("/usr/bin/true");
        command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = command.spawn().expect("failed to spawn /usr/bin/true");
        let pid = child.id();
        let status = child.wait().expect("failed to wait for /usr/bin/true");
        assert!(status.success(), "/usr/bin/true should exit successfully");

        // 完全に reap 済みの pid に対して呼んでもパニックしないこと自体が
        // このテストの主張（アサーションは「ここまで到達した」ことで十分）。
        kill_pid(pid);
    }

    /// carapace が孫プロセス（git 等）を spawn するケースの再現テスト。
    ///
    /// `run_external_capped` 経由で `sh -c 'sleep 30 & echo $! > <tempfile>; wait'`
    /// を短いタイムアウトで実行する。`sh` は直接の子プロセス、`sleep 30` は
    /// その `sh` がバックグラウンドで spawn した孫プロセスに相当する。
    /// タイムアウト後、プロセスグループ全体への kill によって孫プロセスも
    /// 道連れで死んでいることを確認する（直接の子だけを kill する実装では
    /// 孫プロセスは孤児化して生き残ってしまう）。
    #[cfg(unix)]
    #[test]
    fn timeout_kills_grandchild_process_spawned_by_child() {
        let tempfile = std::env::temp_dir().join(format!(
            "jarvish-external-grandchild-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let tempfile_path = tempfile.to_str().unwrap().to_string();

        let out = run_external_capped(
            Path::new("/bin/sh"),
            &[
                "-c".to_string(),
                format!("sleep 30 & echo $! > {tempfile_path}; wait"),
            ],
            &[],
            Duration::from_millis(150),
        );
        assert!(out.is_none(), "the wrapping sh should time out to None");

        // グループ kill が孫プロセス (sleep 30) へ届き、reap されるまで
        // 少し待つ（グランドチャイルドの pid ファイル書き込み自体も
        // 非同期なので、多少のポーリング余地を持たせる）。
        let mut grandchild_pid: Option<i32> = None;
        for _ in 0..20 {
            if let Ok(contents) = std::fs::read_to_string(&tempfile) {
                if let Ok(pid) = contents.trim().parse::<i32>() {
                    grandchild_pid = Some(pid);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let gpid = grandchild_pid.expect("grandchild should have written its pid to tempfile");

        // グループ kill 後、孫プロセスが実際に死んでいるかポーリングで確認する。
        let mut alive = true;
        for _ in 0..20 {
            let ret = unsafe { libc::kill(gpid as libc::pid_t, 0) };
            if ret == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                alive = false;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let _ = std::fs::remove_file(&tempfile);

        assert!(
            !alive,
            "grandchild pid {gpid} should be dead after group-kill on timeout"
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_zero_exit_returns_none() {
        let out = run_external_capped(
            Path::new("/bin/sh"),
            &["-c".to_string(), "exit 3".to_string()],
            &[],
            Duration::from_secs(1),
        );
        assert!(out.is_none(), "non-zero exit should yield None");
    }

    #[cfg(unix)]
    #[test]
    fn missing_binary_returns_none_without_panic() {
        let out = run_external_capped(
            Path::new("/no/such/binary/zzjarvish"),
            &[],
            &[],
            Duration::from_secs(1),
        );
        assert!(out.is_none(), "missing binary should yield None");
    }

    #[cfg(unix)]
    #[test]
    fn env_vars_are_passed_to_child() {
        let out = run_external_capped(
            Path::new("/bin/sh"),
            &["-c".to_string(), "printf %s \"$FOO\"".to_string()],
            &[("FOO".to_string(), "bar".to_string())],
            Duration::from_secs(1),
        );
        assert_eq!(out, Some("bar".to_string()));
    }

    /// `collect_descendants` の素朴な2階層ツリーでの単体テスト。
    ///
    /// `sh -c 'sleep 30 & echo $! >> <tempfile>; sleep 30 & echo $! >>
    /// <tempfile>; wait'` を直接 spawn し（`process_group` は設定しない —
    /// 収集ロジック自体は pgid に依存せず ppid だけを辿ることの確認も兼ねる）、
    /// その pid を root として `collect_descendants` を呼ぶ。root の直接の
    /// 子である 2 本の `sleep 30` の pid が両方とも含まれることを確認する。
    #[cfg(unix)]
    #[test]
    fn collect_descendants_finds_two_level_tree() {
        let tempfile = std::env::temp_dir().join(format!(
            "jarvish-external-descendants-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let tempfile_path = tempfile.to_str().unwrap().to_string();

        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c".to_string(),
                format!(
                    "sleep 30 & echo $! >> {tempfile_path}; \
                     sleep 30 & echo $! >> {tempfile_path}; \
                     wait"
                ),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command.spawn().expect("failed to spawn /bin/sh");
        let root_pid = child.id();
        std::thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
        });

        // 子2本が実際に spawn されて pid ファイルへ書き込まれ、`ps` にも
        // 載るまでポーリングする。
        let mut expected_pids: Vec<u32> = Vec::new();
        for _ in 0..40 {
            if let Ok(contents) = std::fs::read_to_string(&tempfile) {
                let pids: Vec<u32> = contents
                    .lines()
                    .filter_map(|line| line.trim().parse::<u32>().ok())
                    .collect();
                if pids.len() == 2 {
                    expected_pids = pids;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_file(&tempfile);
        assert_eq!(
            expected_pids.len(),
            2,
            "expected two child pids from the sh script to appear in the tempfile"
        );

        let descendants = collect_descendants(root_pid);

        // 後始末: root プロセスグループ全体を kill しておく（テスト終了後に
        // sleep 30 が残らないようにする。process_group を設定していないため
        // ここでは kill_tree ではなく素朴に SIGKILL する）。
        unsafe {
            libc::kill(root_pid as libc::pid_t, libc::SIGKILL);
            for pid in &expected_pids {
                libc::kill(*pid as libc::pid_t, libc::SIGKILL);
            }
        }

        for pid in &expected_pids {
            assert!(
                descendants.contains(pid),
                "collect_descendants({root_pid}) = {descendants:?} should contain child pid {pid}"
            );
        }
    }

    /// zsh ブリッジの `zpty` 子が **別プロセスグループ** に属していても、
    /// タイムアウト時の kill が実際にそこまで届いて殺し切ることの証明テスト。
    ///
    /// 再現: `zsh -c 'zmodload zsh/zpty; zpty w zsh <script>; while zpty -r w
    /// line; do; done'`（`<script>` は HUP/TERM を trap して無視し 30秒
    /// sleep するだけの一時スクリプトファイル — インライン `-c '...'` は
    /// ネストした引用符で壊れやすいためファイル経由にしている）を
    /// `run_external_capped` 経由で短いタイムアウトで実行する。`zpty` が
    /// 起動する内側の zsh（HUP/TERM を trap で無視）は PTY 経由で外側 zsh
    /// とは異なる pgid を持つ。まず内側 zsh 自身に自分の pid/pgid を
    /// 一時ファイルへ書き出させて「outer の pgid とは実際に異なる」ことを
    /// テスト内で確認したうえで、タイムアウト後にその pid が実際に ESRCH
    /// になる（=死んでいる）ことをポーリングで確認する。outer 側だけを
    /// グループ kill する旧実装ではこの内側 zsh は生き残ってしまう
    /// （本 Fix の回帰検知テスト）。
    #[cfg(unix)]
    #[test]
    fn killed_on_timeout_reaches_zpty_child_in_different_process_group() {
        let Ok(zsh) = which::which("zsh") else {
            eprintln!("skipping: zsh not found on PATH");
            return;
        };

        let unique = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let tempfile =
            std::env::temp_dir().join(format!("jarvish-external-zpty-child-test-{unique}"));
        let tempfile_path = tempfile.to_str().unwrap().to_string();

        // 内側 zsh に実行させるスクリプトは一時ファイルに書いて `zsh
        // <scriptfile>` の形で呼ぶ（`zpty w zsh -c '<inline>'` に直接
        // 埋め込むと、スクリプト内の `'` が外側の `zpty -w`/シェル引用と
        // 衝突して壊れやすいため）。内側 zsh 自身の $$ (pid) と `ps -o
        // pgid=` で調べた自分の pgid をマーカーファイルへ即座に書き出して
        // から、HUP/TERM を trap して無視し、30秒 sleep する。
        let inner_script_path =
            std::env::temp_dir().join(format!("jarvish-external-zpty-child-inner-{unique}.zsh"));
        std::fs::write(
            &inner_script_path,
            format!(
                "print -r -- \"pid=$$ pgid=$(ps -o pgid= -p $$ | tr -d ' ')\" > {tempfile_path}\n\
                 trap '' HUP TERM\n\
                 sleep 30\n"
            ),
        )
        .expect("failed to write inner zpty script");
        let inner_script_path_str = inner_script_path.to_str().unwrap().to_string();

        // outer zsh は zpty 経由で `zsh <inner_script_path>` を起動する。
        let script = format!(
            "zmodload zsh/zpty; zpty w zsh {inner_script_path_str}; while zpty -r w line; do :; done"
        );

        // outer zsh (= run_external_capped が spawn するプロセス) 自身の
        // pgid は process_group(0) により pid と一致する。run_external_capped
        // は pid を外部に返さない設計のため、ここでは同じ spawn ロジックを
        // 直接再現して outer_pid（= outer_pgid）を取得する。
        let mut command = Command::new(&zsh);
        command
            .args(["-c".to_string(), script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        let child = command.spawn().expect("failed to spawn outer zsh");
        let outer_pid = child.id();

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let output = child.wait_with_output();
            let _ = tx.send(output);
        });

        let timed_out = matches!(
            rx.recv_timeout(Duration::from_millis(400)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        assert!(
            timed_out,
            "the wrapping zsh should still be running at 400ms (zpty setup takes time)"
        );

        // 内側 zsh が pid/pgid ファイルを書き終えるまで軽くポーリングする
        // （outer zsh の zpty 起動 + fork のタイミングは非同期）。
        let mut inner_pid_pgid: Option<(i32, i32)> = None;
        for _ in 0..40 {
            if let Ok(contents) = std::fs::read_to_string(&tempfile) {
                if let Some(parsed) = parse_pid_pgid_marker(&contents) {
                    inner_pid_pgid = Some(parsed);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_file(&tempfile);
        let _ = std::fs::remove_file(&inner_script_path);

        let Some((inner_pid, inner_pgid)) = inner_pid_pgid else {
            // 環境によっては zpty 自体が使えない（sandbox 制限等）ことが
            // あるため、マーカーファイルが一切書かれなかった場合はテスト
            // 環境の制約とみなして skip する（false negative を避けるため
            // ログには残し、outer 側は掃除しておく）。
            kill_tree(outer_pid);
            eprintln!(
                "skipping: inner zsh never wrote its pid/pgid marker (zpty unavailable in this env?)"
            );
            return;
        };

        // 前提の確認: 内側 zsh の pgid は outer zsh の pgid（= outer_pid、
        // process_group(0) により pid と一致）とは異なる別プロセスグループで
        // あること。これが同じなら旧実装の group-kill でも死ぬはずで、この
        // テストが証明したいシナリオになっていない。
        assert_ne!(
            inner_pgid, outer_pid as i32,
            "inner zpty child must be in a DIFFERENT process group than the outer zsh \
             for this test to prove anything; outer_pid(=outer_pgid)={outer_pid}, inner_pgid={inner_pgid}"
        );

        // ここで本番の kill_tree ロジックを、収集した outer_pid に対して
        // 直接呼び出す（run_external_capped 内部のタイムアウト分岐と同じ
        // コードパス）。
        kill_tree(outer_pid);

        // kill 後、内側 zsh (HUP/TERM を trap で無視) が実際に死んでいるかを
        // ポーリングで確認する。生きていれば kill(pid, 0) は 0 を返し続け、
        // 死んでいれば ESRCH で -1 を返す。
        let mut alive = true;
        for _ in 0..40 {
            let ret = unsafe { libc::kill(inner_pid as libc::pid_t, 0) };
            if ret == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                alive = false;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        assert!(
            !alive,
            "zpty inner zsh (pid {inner_pid}, pgid {inner_pgid}, trapping HUP/TERM, \
             different pgid than outer {outer_pid}) should be dead after kill_tree, \
             but kill(pid, 0) still succeeds"
        );
    }

    /// `pid=<pid> pgid=<pgid>` 形式のマーカー行から (pid, pgid) を取り出す。
    #[cfg(unix)]
    fn parse_pid_pgid_marker(contents: &str) -> Option<(i32, i32)> {
        let line = contents.lines().next()?;
        let mut fields = line.split_whitespace();
        let pid = fields.next()?.strip_prefix("pid=")?.parse::<i32>().ok()?;
        let pgid = fields.next()?.strip_prefix("pgid=")?.parse::<i32>().ok()?;
        Some((pid, pgid))
    }
}
