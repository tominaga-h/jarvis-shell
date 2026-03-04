//! レガシー方式（pipe + tee）によるコマンド実行
//!
//! リダイレクト対応、および PTY セッションのフォールバック先。
//! 旧来の PTY + tee キャプチャ方式で stdin は inherit する。

use std::process::{Command, Stdio};
use std::thread;

use tracing::debug;

use crate::engine::io::tee_to_terminal;
use crate::engine::parser::{Redirect, SimpleCommand};
use crate::engine::pty::create_capture_pair;
use crate::engine::redirect::{find_stdin_redirect, find_stdout_redirect};
use crate::engine::{CommandResult, LoopAction};

/// レガシー方式で単一コマンドを実行する（リダイレクト対応、PTY セッションのフォールバック）。
/// 旧来の PTY + tee キャプチャ方式。stdin は inherit。
pub(super) fn run_single_command_legacy(simple: &SimpleCommand) -> CommandResult {
    let cmd = &simple.cmd;
    let args: Vec<&str> = simple.args.iter().map(|s| s.as_str()).collect();

    debug!(command = %cmd, args = ?args, "Spawning external command (legacy mode)");

    // stdout キャプチャ: PTY (色出力保持) / pipe (フォールバック)
    let (stdout_reader, stdout_writer) = match create_capture_pair() {
        Ok(pair) => pair,
        Err(e) => {
            let msg = format!("jarvish: pipe error: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    // stderr キャプチャ: PTY (色出力保持) / pipe (フォールバック)
    let (stderr_reader, stderr_writer) = match create_capture_pair() {
        Ok(pair) => pair,
        Err(e) => {
            let msg = format!("jarvish: pipe error: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    // リダイレクト: stdin
    let final_stdin: Stdio = match find_stdin_redirect(&simple.redirects) {
        Ok(Some(file)) => file.into(),
        Ok(None) => Stdio::inherit(),
        Err(e) => return e,
    };

    // リダイレクト: stdout
    let has_stdout_redirect = simple
        .redirects
        .iter()
        .any(|r| matches!(r, Redirect::StdoutOverwrite(_) | Redirect::StdoutAppend(_)));

    let final_stdout: Stdio = if has_stdout_redirect {
        match find_stdout_redirect(&simple.redirects) {
            Some(file) => {
                drop(stdout_writer);
                file.into()
            }
            None => {
                let msg = "jarvish: internal error: stdout redirect not found\n".to_string();
                eprint!("{msg}");
                return CommandResult::error(msg, 1);
            }
        }
    } else {
        stdout_writer
    };

    let mut child = {
        let mut command = Command::new(cmd);
        command
            .args(&args)
            .stdin(final_stdin)
            .stdout(final_stdout)
            .stderr(stderr_writer);

        match command.spawn() {
            Ok(child) => child,
            Err(e) => return super::spawn_error(cmd, e),
        }
    };

    let stdout_handle = thread::spawn(move || tee_to_terminal(stdout_reader, false));
    let stderr_handle = thread::spawn(move || tee_to_terminal(stderr_reader, true));

    let exit_code = match child.wait() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("jarvish: wait error: {e}");
            1
        }
    };

    let stdout_bytes = stdout_handle.join().unwrap_or_default();
    let stderr_bytes = stderr_handle.join().unwrap_or_default();

    debug!(
        command = %cmd,
        exit_code = exit_code,
        stdout_size = stdout_bytes.len(),
        stderr_size = stderr_bytes.len(),
        "External command completed (legacy)"
    );

    CommandResult {
        stdout: String::from_utf8_lossy(&stdout_bytes).to_string(),
        stderr: String::from_utf8_lossy(&stderr_bytes).to_string(),
        exit_code,
        action: LoopAction::Continue,
        used_alt_screen: false,
    }
}
