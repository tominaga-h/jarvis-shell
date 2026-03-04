//! パイプライン実行エンジン
//!
//! 単一コマンドやパイプラインの実行を管理する。
//! PTY セッション（vim/less 等の対話コマンド対応）とレガシーモード（tee キャプチャ）を
//! 使い分け、stdout/stderr をキャプチャしつつターミナルに表示する。

mod capture;
mod legacy;
mod pipeline;
mod pty_session;

use std::io;

use tracing::debug;

use super::parser::{Pipeline, SimpleCommand};
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
    pipeline::run_piped_commands(&pipeline.commands)
}

/// 単一コマンドの実行エントリポイント。
/// リダイレクトがある場合はレガシー（pipe + tee）方式にフォールバック。
/// リダイレクトがない場合はフル PTY セッションで実行する。
fn run_single_command(simple: &SimpleCommand) -> CommandResult {
    let has_redirect = !simple.redirects.is_empty();

    if has_redirect {
        return legacy::run_single_command_legacy(simple);
    }

    // フル PTY セッションを試行。ターミナル取得に失敗した場合はレガシーにフォールバック。
    match pty_session::run_single_command_pty_session(simple) {
        Ok(result) => result,
        Err(e) => {
            debug!("PTY session failed ({e}), falling back to legacy mode");
            legacy::run_single_command_legacy(simple)
        }
    }
}

/// パイプラインを実行し、stdout をターミナルに表示せずキャプチャして返す。
/// AI パイプ (`cmd | ai "..."`) の手前パイプライン実行に使用する。
///
/// UNIX パイプのセマンティクスに従い:
/// - stdout: `Stdio::piped()` でキャプチャ（ターミナルに表示しない）
/// - stderr: `Stdio::inherit()` でターミナルに直接表示
pub fn run_pipeline_captured(pipeline: &Pipeline) -> CommandResult {
    capture::run_pipeline_captured(pipeline)
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
    use crate::engine::parser::Redirect;

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
