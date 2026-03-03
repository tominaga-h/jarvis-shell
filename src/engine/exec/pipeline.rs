//! パイプライン接続実行
//!
//! 複数コマンドをパイプで接続し、全ステージの stdout/stderr を tee でキャプチャする。

use std::process::{Command, Stdio};
use std::thread;

use tracing::debug;

use crate::engine::io::tee_to_terminal;
use crate::engine::parser::{Redirect, SimpleCommand};
use crate::engine::pty::create_capture_pair;
use crate::engine::redirect::{find_stdin_redirect, find_stdout_redirect};
use crate::engine::{CommandResult, LoopAction};

/// 複数コマンドをパイプで接続して実行する。
/// 全ステージの stdout/stderr を tee でキャプチャする。
pub(super) fn run_piped_commands(commands: &[SimpleCommand]) -> CommandResult {
    let n = commands.len();
    let mut children = Vec::new();
    let mut prev_stdout: Option<os_pipe::PipeReader> = None;

    // 中間ステージの stderr を共有パイプでキャプチャする。
    // Option でラップし、is_last ブロックで take() → drop して EOF を伝播させる。
    let (mid_stderr_reader, mid_stderr_writer) = match os_pipe::pipe() {
        Ok(pair) => pair,
        Err(e) => {
            let msg = format!("jarvish: pipe error: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };
    let mut mid_stderr_writer = Some(mid_stderr_writer);

    for (i, simple) in commands.iter().enumerate() {
        let is_last = i == n - 1;
        let cmd = &simple.cmd;
        let args: Vec<&str> = simple.args.iter().map(|s| s.as_str()).collect();

        debug!(
            command = %cmd,
            args = ?args,
            stage = i + 1,
            total = n,
            "Pipeline stage"
        );

        let stdin_cfg: Stdio = if let Some(prev) = prev_stdout.take() {
            prev.into()
        } else {
            match find_stdin_redirect(&simple.redirects) {
                Ok(Some(file)) => file.into(),
                Ok(None) => Stdio::inherit(),
                Err(e) => return e,
            }
        };

        if is_last {
            // 全中間ステージのクローン作成後、オリジナルを drop して EOF を伝播
            mid_stderr_writer.take();

            let (stdout_reader, stdout_writer) = match create_capture_pair() {
                Ok(pair) => pair,
                Err(e) => {
                    let msg = format!("jarvish: pipe error: {e}\n");
                    eprint!("{msg}");
                    return CommandResult::error(msg, 1);
                }
            };

            let (stderr_reader, stderr_writer) = match create_capture_pair() {
                Ok(pair) => pair,
                Err(e) => {
                    let msg = format!("jarvish: pipe error: {e}\n");
                    eprint!("{msg}");
                    return CommandResult::error(msg, 1);
                }
            };

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
                        let msg =
                            "jarvish: internal error: stdout redirect not found\n".to_string();
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
                    .stdin(stdin_cfg)
                    .stdout(final_stdout)
                    .stderr(stderr_writer);

                match command.spawn() {
                    Ok(child) => child,
                    Err(e) => {
                        for mut c in children {
                            super::kill_and_wait(&mut c);
                        }
                        return super::spawn_error(cmd, e);
                    }
                }
            };

            let stdout_handle = thread::spawn(move || tee_to_terminal(stdout_reader, false));
            let stderr_handle = thread::spawn(move || tee_to_terminal(stderr_reader, true));
            let mid_stderr_handle = thread::spawn(move || tee_to_terminal(mid_stderr_reader, true));

            let exit_code = match child.wait() {
                Ok(status) => status.code().unwrap_or(1),
                Err(e) => {
                    eprintln!("jarvish: wait error: {e}");
                    1
                }
            };

            for mut c in children {
                let _ = c.wait();
            }

            let stdout_bytes = stdout_handle.join().unwrap_or_default();
            let stderr_bytes = stderr_handle.join().unwrap_or_default();
            let mid_stderr_bytes = mid_stderr_handle.join().unwrap_or_default();

            // 中間ステージ + 最終ステージの stderr を結合
            let mut combined_stderr = mid_stderr_bytes;
            combined_stderr.extend_from_slice(&stderr_bytes);

            debug!(
                command = %cmd,
                exit_code = exit_code,
                stdout_size = stdout_bytes.len(),
                stderr_size = combined_stderr.len(),
                "Pipeline final stage completed"
            );

            return CommandResult {
                stdout: String::from_utf8_lossy(&stdout_bytes).to_string(),
                stderr: String::from_utf8_lossy(&combined_stderr).to_string(),
                exit_code,
                action: LoopAction::Continue,
                used_alt_screen: false,
            };
        }

        // 中間段
        let (pipe_read, pipe_write) = match os_pipe::pipe() {
            Ok(pair) => pair,
            Err(e) => {
                let msg = format!("jarvish: pipe error: {e}\n");
                eprint!("{msg}");
                return CommandResult::error(msg, 1);
            }
        };

        // 中間ステージの stderr を共有パイプに流してキャプチャする
        let mid_stderr: Stdio = mid_stderr_writer
            .as_ref()
            .and_then(|w| w.try_clone().ok())
            .map(|w| -> Stdio { w.into() })
            .unwrap_or_else(Stdio::inherit);

        let child = {
            let mut command = Command::new(cmd);
            command
                .args(&args)
                .stdin(stdin_cfg)
                .stdout(pipe_write)
                .stderr(mid_stderr);

            match command.spawn() {
                Ok(child) => child,
                Err(e) => {
                    for mut c in children {
                        super::kill_and_wait(&mut c);
                    }
                    return super::spawn_error(cmd, e);
                }
            }
        };
        children.push(child);
        prev_stdout = Some(pipe_read);
    }

    CommandResult::error("jarvish: internal error: empty pipeline".to_string(), 1)
}
