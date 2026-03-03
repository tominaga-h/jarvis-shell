//! AI パイプ用キャプチャ実行
//!
//! stdout をターミナルに表示せず、メモリ上にキャプチャして返す。
//! AI パイプ (`cmd | ai "..."`) の手前パイプライン実行に使用する。

use std::process::{Command, Stdio};

use tracing::debug;

use crate::engine::parser::{Pipeline, SimpleCommand};
use crate::engine::redirect::find_stdin_redirect;
use crate::engine::{CommandResult, LoopAction};

/// パイプラインを実行し、stdout をターミナルに表示せずキャプチャして返す。
///
/// UNIX パイプのセマンティクスに従い:
/// - stdout: `Stdio::piped()` でキャプチャ（ターミナルに表示しない）
/// - stderr: `Stdio::inherit()` でターミナルに直接表示
pub(super) fn run_pipeline_captured(pipeline: &Pipeline) -> CommandResult {
    let n = pipeline.commands.len();
    debug!(pipeline_length = n, "Running pipeline (captured mode)");

    if n == 0 {
        return CommandResult::success(String::new());
    }

    if n == 1 {
        return run_single_command_captured(&pipeline.commands[0]);
    }

    run_piped_commands_captured(&pipeline.commands)
}

/// 単一コマンドを stdout キャプチャモードで実行する。
fn run_single_command_captured(simple: &SimpleCommand) -> CommandResult {
    let cmd = &simple.cmd;
    let args: Vec<&str> = simple.args.iter().map(|s| s.as_str()).collect();

    debug!(command = %cmd, args = ?args, "Spawning external command (captured mode)");

    let stdin_cfg: Stdio = match find_stdin_redirect(&simple.redirects) {
        Ok(Some(file)) => file.into(),
        Ok(None) => Stdio::inherit(),
        Err(e) => return e,
    };

    let mut command = Command::new(cmd);
    command
        .args(&args)
        .stdin(stdin_cfg)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    match command.spawn() {
        Ok(child) => match child.wait_with_output() {
            Ok(output) => {
                let exit_code = output.status.code().unwrap_or(1);
                debug!(
                    command = %cmd,
                    exit_code = exit_code,
                    stdout_size = output.stdout.len(),
                    "External command completed (captured mode)"
                );
                CommandResult {
                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                    stderr: String::new(),
                    exit_code,
                    action: LoopAction::Continue,
                    used_alt_screen: false,
                }
            }
            Err(e) => {
                let msg = format!("jarvish: wait error: {e}\n");
                eprint!("{msg}");
                CommandResult::error(msg, 1)
            }
        },
        Err(e) => super::spawn_error(cmd, e),
    }
}

/// 複数コマンドのパイプラインを stdout キャプチャモードで実行する。
fn run_piped_commands_captured(commands: &[SimpleCommand]) -> CommandResult {
    let n = commands.len();
    let mut children: Vec<std::process::Child> = Vec::new();
    let mut prev_stdout: Option<std::process::ChildStdout> = None;

    for (i, simple) in commands.iter().enumerate() {
        let is_last = i == n - 1;
        let cmd = &simple.cmd;
        let args: Vec<&str> = simple.args.iter().map(|s| s.as_str()).collect();

        let stdin_cfg: Stdio = if let Some(prev) = prev_stdout.take() {
            prev.into()
        } else {
            match find_stdin_redirect(&simple.redirects) {
                Ok(Some(file)) => file.into(),
                Ok(None) => Stdio::inherit(),
                Err(e) => {
                    for mut c in children {
                        super::kill_and_wait(&mut c);
                    }
                    return e;
                }
            }
        };

        let mut command = Command::new(cmd);
        command
            .args(&args)
            .stdin(stdin_cfg)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        match command.spawn() {
            Ok(mut child) => {
                if is_last {
                    match child.wait_with_output() {
                        Ok(output) => {
                            for mut c in children {
                                let _ = c.wait();
                            }
                            let exit_code = output.status.code().unwrap_or(1);
                            debug!(
                                command = %cmd,
                                exit_code = exit_code,
                                stdout_size = output.stdout.len(),
                                "Pipeline final stage completed (captured mode)"
                            );
                            return CommandResult {
                                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                                stderr: String::new(),
                                exit_code,
                                action: LoopAction::Continue,
                                used_alt_screen: false,
                            };
                        }
                        Err(e) => {
                            for mut c in children {
                                super::kill_and_wait(&mut c);
                            }
                            let msg = format!("jarvish: wait error: {e}\n");
                            eprint!("{msg}");
                            return CommandResult::error(msg, 1);
                        }
                    }
                } else {
                    prev_stdout = child.stdout.take();
                    children.push(child);
                }
            }
            Err(e) => {
                for mut c in children {
                    super::kill_and_wait(&mut c);
                }
                return super::spawn_error(cmd, e);
            }
        }
    }

    CommandResult::error("jarvish: internal error: empty pipeline".to_string(), 1)
}
