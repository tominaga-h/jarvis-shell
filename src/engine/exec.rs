//! パイプライン実行エンジン
//!
//! 単一コマンドやパイプラインの実行を管理する。
//! PTY セッション（vim/less 等の対話コマンド対応）とレガシーモード（tee キャプチャ）を
//! 使い分け、stdout/stderr をキャプチャしつつターミナルに表示する。

use std::io::{self, IsTerminal};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;

use tracing::debug;

use super::io::{capture_pty_output, forward_stdin, tee_stderr, tee_to_terminal};
use super::parser::{Pipeline, Redirect, SimpleCommand};
use super::pty::{create_capture_pair, create_session_pty};
use super::redirect::{find_stdin_redirect, find_stdout_redirect};
use super::terminal::TerminalStateGuard;
use super::CommandResult;
use crate::cli::jarvis::jarvis_talk;

// ── パイプライン実行 ──

/// パイプラインを実行する。
///
/// - 単一コマンド: フル PTY セッションで実行（vim/less/bat 等の対話コマンド対応）
/// - 複数コマンド: 前段の stdout を次段の stdin にパイプで接続し、
///   最終段の stdout/stderr のみ tee でキャプチャ
/// - リダイレクト: `>`, `>>`, `<` を処理
pub fn run_pipeline(pipeline: &Pipeline) -> CommandResult {
    let n = pipeline.commands.len();
    debug!(pipeline_length = n, "Running pipeline");

    if n == 1 {
        return run_single_command(&pipeline.commands[0]);
    }

    // 複数コマンドのパイプライン
    run_piped_commands(&pipeline.commands)
}

/// 単一コマンドの実行エントリポイント。
/// リダイレクトがある場合はレガシー（pipe + tee）方式にフォールバック。
/// リダイレクトがない場合はフル PTY セッションで実行する。
fn run_single_command(simple: &SimpleCommand) -> CommandResult {
    let has_redirect = !simple.redirects.is_empty();

    if has_redirect {
        return run_single_command_legacy(simple);
    }

    // フル PTY セッションを試行。ターミナル取得に失敗した場合はレガシーにフォールバック。
    match run_single_command_pty_session(simple) {
        Ok(result) => result,
        Err(e) => {
            debug!("PTY session failed ({e}), falling back to legacy mode");
            run_single_command_legacy(simple)
        }
    }
}

/// フル PTY セッション方式で単一コマンドを実行する。
/// 子プロセスをセッションリーダーとして起動し、PTY を制御端末として割り当てる。
/// stdin は PTY 経由で転送し、stdout は PTY 経由でキャプチャする。
fn run_single_command_pty_session(simple: &SimpleCommand) -> io::Result<CommandResult> {
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

    // 13. ターミナル状態は terminal_guard の Drop で自動復元される

    // 14. スレッドを join
    let _ = stdin_handle.join();
    let capture = output_handle.join().unwrap_or_default();
    let stderr_bytes = stderr_handle.join().unwrap_or_default();

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
        action: super::LoopAction::Continue,
        used_alt_screen: capture.used_alt_screen,
    })
}

/// レガシー方式で単一コマンドを実行する（リダイレクト対応、PTY セッションのフォールバック）。
/// 旧来の PTY + tee キャプチャ方式。stdin は inherit。
fn run_single_command_legacy(simple: &SimpleCommand) -> CommandResult {
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
        let file = find_stdout_redirect(&simple.redirects).expect("redirect checked above");
        drop(stdout_writer);
        file.into()
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
            Err(e) => return spawn_error(cmd, e),
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
        action: super::LoopAction::Continue,
        used_alt_screen: false,
    }
}

/// 複数コマンドをパイプで接続して実行する。
/// 全ステージの stdout/stderr を tee でキャプチャする。
fn run_piped_commands(commands: &[SimpleCommand]) -> CommandResult {
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
                let file = find_stdout_redirect(&simple.redirects).expect("redirect checked above");
                drop(stdout_writer);
                file.into()
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
                            kill_and_wait(&mut c);
                        }
                        return spawn_error(cmd, e);
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
                action: super::LoopAction::Continue,
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
                        kill_and_wait(&mut c);
                    }
                    return spawn_error(cmd, e);
                }
            }
        };
        children.push(child);
        prev_stdout = Some(pipe_read);
    }

    CommandResult::error("jarvish: internal error: empty pipeline".to_string(), 1)
}

// ── エラーヘルパー ──

/// プロセス起動エラーを CommandResult として返すヘルパー。
fn spawn_error(cmd: &str, e: io::Error) -> CommandResult {
    let reason = match e.kind() {
        io::ErrorKind::NotFound => "command not found".to_string(),
        io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        _ => format!("{e}"),
    };
    let msg = format!("{cmd}: {reason}. Something wrong, sir?");
    jarvis_talk(&msg);
    CommandResult::error(msg, 127)
}

/// 子プロセスを kill して wait するヘルパー。
fn kill_and_wait(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ヘルパー: 単一コマンドの SimpleCommand を生成する
    fn simple(cmd: &str, args: &[&str]) -> SimpleCommand {
        SimpleCommand {
            cmd: cmd.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            redirects: vec![],
        }
    }

    // ── run_single_command テスト ──

    #[test]
    fn echo_stdout_capture() {
        let result = run_single_command(&simple("echo", &["hello"]));
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "hello");
    }

    #[test]
    fn exit_code_success() {
        let result = run_single_command(&simple("true", &[]));
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn exit_code_failure() {
        let result = run_single_command(&simple("false", &[]));
        assert_eq!(result.exit_code, 1);
    }

    #[test]
    fn stderr_capture() {
        let result = run_single_command(&simple("sh", &["-c", "echo err >&2"]));
        assert_eq!(result.stderr.trim(), "err");
    }

    #[test]
    fn nonexistent_command_returns_error() {
        let result = run_single_command(&simple("__jarvish_nonexistent_command__", &[]));
        assert_ne!(result.exit_code, 0);
        assert!(!result.stderr.is_empty());
    }

    // ── run_pipeline テスト: パイプ ──

    #[test]
    fn pipeline_two_commands_piped() {
        let pipeline = Pipeline {
            commands: vec![
                SimpleCommand {
                    cmd: "echo".into(),
                    args: vec!["hello".into()],
                    redirects: vec![],
                },
                SimpleCommand {
                    cmd: "cat".into(),
                    args: vec![],
                    redirects: vec![],
                },
            ],
        };
        let result = run_pipeline(&pipeline);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "hello");
    }

    #[test]
    fn pipeline_three_commands_piped() {
        let pipeline = Pipeline {
            commands: vec![
                SimpleCommand {
                    cmd: "printf".into(),
                    args: vec!["aaa\\nbbb\\nccc\\n".into()],
                    redirects: vec![],
                },
                SimpleCommand {
                    cmd: "grep".into(),
                    args: vec!["bbb".into()],
                    redirects: vec![],
                },
                SimpleCommand {
                    cmd: "cat".into(),
                    args: vec![],
                    redirects: vec![],
                },
            ],
        };
        let result = run_pipeline(&pipeline);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "bbb");
    }

    #[test]
    fn pipeline_exit_code_from_last_command() {
        let pipeline = Pipeline {
            commands: vec![
                SimpleCommand {
                    cmd: "echo".into(),
                    args: vec!["hello".into()],
                    redirects: vec![],
                },
                SimpleCommand {
                    cmd: "false".into(),
                    args: vec![],
                    redirects: vec![],
                },
            ],
        };
        let result = run_pipeline(&pipeline);
        assert_eq!(result.exit_code, 1);
    }

    // ── run_pipeline テスト: リダイレクト ──

    #[test]
    fn redirect_stdout_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let path_str = path.to_str().unwrap().to_string();

        let pipeline = Pipeline {
            commands: vec![SimpleCommand {
                cmd: "echo".into(),
                args: vec!["redirected".into()],
                redirects: vec![Redirect::StdoutOverwrite(path_str)],
            }],
        };
        let result = run_pipeline(&pipeline);
        assert_eq!(result.exit_code, 0);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.trim(), "redirected");
    }

    #[test]
    fn redirect_stdout_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let path_str = path.to_str().unwrap().to_string();

        std::fs::write(&path, "first\n").unwrap();

        let pipeline = Pipeline {
            commands: vec![SimpleCommand {
                cmd: "echo".into(),
                args: vec!["second".into()],
                redirects: vec![Redirect::StdoutAppend(path_str)],
            }],
        };
        let result = run_pipeline(&pipeline);
        assert_eq!(result.exit_code, 0);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("first"));
        assert!(contents.contains("second"));
    }

    #[test]
    fn redirect_stdin_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.txt");
        let path_str = path.to_str().unwrap().to_string();

        std::fs::write(&path, "from_file\n").unwrap();

        let pipeline = Pipeline {
            commands: vec![SimpleCommand {
                cmd: "cat".into(),
                args: vec![],
                redirects: vec![Redirect::StdinFrom(path_str)],
            }],
        };
        let result = run_pipeline(&pipeline);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "from_file");
    }

    #[test]
    fn redirect_stdin_nonexistent_file_returns_error() {
        let pipeline = Pipeline {
            commands: vec![SimpleCommand {
                cmd: "cat".into(),
                args: vec![],
                redirects: vec![Redirect::StdinFrom(
                    "/tmp/__jarvish_nonexistent_input__".into(),
                )],
            }],
        };
        let result = run_pipeline(&pipeline);
        assert_ne!(result.exit_code, 0);
    }
}
