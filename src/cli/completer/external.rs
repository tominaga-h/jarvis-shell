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

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// `program` を `args` / `envs` 付きで起動し、`timeout` 以内に正常終了（exit
/// code 0）した場合のみ stdout を `String` として返す。
///
/// 以下のいずれの場合も `None`:
/// - `timeout` 以内にプロセスが終了しなかった（SIGKILL で強制終了する）
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

/// `pid` に `SIGKILL` を送る。プロセスが既に終了していた場合のエラーは
/// 無視する（reap 済みなら送信対象が既に存在しない = 正常なレース）。
#[cfg(unix)]
fn kill_pid(pid: u32) {
    let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    if ret != 0 {
        let err = io::Error::last_os_error();
        tracing::debug!("run_external_capped: kill({pid}) failed: {err}");
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
        let mut command = Command::new("/bin/sleep");
        command.arg("5").stdout(Stdio::null()).stderr(Stdio::null());
        let child = command.spawn().expect("failed to spawn /bin/sleep");
        let pid = child.id();

        std::thread::spawn(move || {
            let _ = child.wait_with_output();
        });

        kill_pid(pid);
        std::thread::sleep(Duration::from_millis(200));

        let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
        let err = io::Error::last_os_error();
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
