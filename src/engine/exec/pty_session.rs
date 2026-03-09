//! PTY セッション方式によるコマンド実行
//!
//! 子プロセスをセッションリーダーとして起動し、PTY を制御端末として割り当てる。
//! stdin は PTY 経由で転送し、stdout は PTY 経由でキャプチャする。

use std::io::{self, IsTerminal, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;

use tracing::debug;

use crate::engine::io::{capture_pty_output, forward_stdin, tee_stderr};
use crate::engine::parser::SimpleCommand;
use crate::engine::pty::create_session_pty;
use crate::engine::terminal::TerminalStateGuard;
use crate::engine::{CommandResult, LoopAction};

/// フル PTY セッション方式で単一コマンドを実行する。
/// 子プロセスをセッションリーダーとして起動し、PTY を制御端末として割り当てる。
/// stdin は PTY 経由で転送し、stdout は PTY 経由でキャプチャする。
pub(super) fn run_single_command_pty_session(simple: &SimpleCommand) -> io::Result<CommandResult> {
    // テストビルドでは PTY セッションモードを使用しない。
    // PTY セッションは親ターミナルを raw mode（OPOST 無効）に変更するため、
    // 複数テストが並列実行されるとターミナル状態のレースコンディションが発生し、
    // 出力が斜めになる（\n → \r\n 変換が失われる）問題を引き起こす。
    if cfg!(test) || !io::stdout().is_terminal() {
        return Err(io::Error::other("PTY session not available"));
    }

    let cmd = &simple.cmd;
    let args: Vec<&str> = simple.args.iter().map(|s| s.as_str()).collect();

    debug!(command = %cmd, args = ?args, "Spawning external command (PTY session)");

    // 1. セッション PTY ペアを作成 (stdin + stdout 共用)
    let (master, slave) = create_session_pty()?;
    let master_raw_fd = master.as_raw_fd();

    // 2. stderr 用パイプを作成
    let (stderr_read, stderr_write) = os_pipe::pipe()?;

    // 3. ターミナル状態ガードを作成（RAII で確実に復元）
    let mut terminal_guard = TerminalStateGuard::new()?;

    // 4. PTY slave fd を複製して stdin / stdout に割り当てる
    let slave_raw_fd = slave.as_raw_fd();
    let stdin_fd = unsafe { libc::dup(slave_raw_fd) };
    let stdout_fd = unsafe { libc::dup(slave_raw_fd) };
    if stdin_fd < 0 || stdout_fd < 0 {
        return Err(io::Error::last_os_error());
    }

    // 5. 子プロセスを起動
    let mut child = {
        let mut command = Command::new(cmd);
        command
            .args(&args)
            .stdin(unsafe { Stdio::from_raw_fd(stdin_fd) })
            .stdout(unsafe { Stdio::from_raw_fd(stdout_fd) })
            .stderr(Stdio::from(stderr_write));

        // 新しいセッションを作成し、PTY を制御端末に設定
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                // fd 0 (stdin) は PTY slave → 制御端末として設定
                if libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }

        match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                return Err(e);
            }
        }
    };

    // 6. 親側の PTY slave fd を閉じる
    drop(slave);

    // 7. 親ターミナルを raw mode に設定（ガードが自動復元を保証）
    if let Err(e) = terminal_guard.activate_raw_mode() {
        debug!("Failed to set raw mode: {e}");
    }

    // 8. stdin 転送スレッドを起動 (停止パイプ付き)
    let (shutdown_read, shutdown_write) = os_pipe::pipe()?;
    let master_for_stdin = master.try_clone()?;
    let stdin_handle = thread::spawn(move || {
        forward_stdin(master_for_stdin, shutdown_read, master_raw_fd);
    });

    // 9. 出力キャプチャスレッドを起動 (Alternate Screen 検出付き)
    let output_handle = thread::spawn(move || capture_pty_output(master));

    // 10. stderr tee スレッドを起動
    let stderr_handle = thread::spawn(move || tee_stderr(stderr_read));

    // 11. 子プロセスの終了を待機
    let exit_code = match child.wait() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("jarvish: wait error: {e}");
            1
        }
    };

    // 12. stdin 転送スレッドを停止
    drop(shutdown_write);

    // 13. スレッドを join
    let _ = stdin_handle.join();
    let capture = output_handle.join().unwrap_or_default();
    let stderr_bytes = stderr_handle.join().unwrap_or_default();

    // 14. ターミナル状態を明示的に復元
    drop(terminal_guard);

    // 15. Alt screen プログラム (less, vim 等) 終了後、ターミナルに残る
    // エスケープシーケンスの処理完了を待ち、stdin の残留 DSR 応答を破棄する。
    // stdout.flush() で全シーケンスをターミナルに送出し、短い遅延で
    // ターミナルの処理・応答生成を待ってから tcflush する。
    if capture.used_alt_screen {
        let _ = io::stdout().flush();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ =
            nix::sys::termios::tcflush(io::stdin().as_fd(), nix::sys::termios::FlushArg::TCIFLUSH);
    }

    debug!(
        command = %cmd,
        exit_code = exit_code,
        stdout_size = capture.bytes.len(),
        stderr_size = stderr_bytes.len(),
        used_alt_screen = capture.used_alt_screen,
        "External command completed (PTY session)"
    );

    Ok(CommandResult {
        stdout: String::from_utf8_lossy(&capture.bytes).to_string(),
        stderr: String::from_utf8_lossy(&stderr_bytes).to_string(),
        exit_code,
        action: LoopAction::Continue,
        used_alt_screen: capture.used_alt_screen,
    })
}
