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
///   git 等 — も道連れで終了するため、孤児化して残り続けることはない）
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
            kill_pid(pid);
            None
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => None,
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

#[cfg(not(unix))]
fn kill_pid(_pid: u32) {
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
}
