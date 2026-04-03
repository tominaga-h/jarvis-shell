pub mod builtins;
pub mod classifier;
pub mod dispatch;
pub mod exec;
pub mod expand;
mod io;
pub mod parser;
mod pty;
mod redirect;
mod terminal;
pub mod typo;

pub use dispatch::{execute, try_builtin, try_execute_ai_pipe};

/// REPL ループの制御アクション
#[derive(Debug, Clone, PartialEq)]
pub enum LoopAction {
    /// ループを続行する
    Continue,
    /// ループを終了する（exit コマンド等）
    Exit,
    /// プロセスを再起動する（restart コマンド、SIGUSR1 受信時）
    Restart,
}

/// コマンド実行の結果を格納する構造体。
/// Phase 2 以降で stdout/stderr を Black Box に永続化する際に使用する。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CommandResult {
    /// キャプチャされた標準出力
    pub stdout: String,
    /// キャプチャされた標準エラー出力
    pub stderr: String,
    /// 終了コード (0 = 成功)
    pub exit_code: i32,
    /// REPL ループの制御アクション
    pub action: LoopAction,
    /// 子プロセスが Alternate Screen Buffer を使用したかどうか。
    /// true の場合、stdout は TUI の画面制御シーケンスであり、
    /// Black Box への保存をスキップすべきことを示す。
    pub used_alt_screen: bool,
}

impl CommandResult {
    /// 成功結果（Continue）を返すヘルパー
    pub fn success(stdout: String) -> Self {
        Self {
            stdout,
            stderr: String::new(),
            exit_code: 0,
            action: LoopAction::Continue,
            used_alt_screen: false,
        }
    }

    /// エラー結果（Continue）を返すヘルパー
    pub fn error(stderr: String, exit_code: i32) -> Self {
        Self {
            stdout: String::new(),
            stderr,
            exit_code,
            action: LoopAction::Continue,
            used_alt_screen: false,
        }
    }

    /// 指定した終了コードで Exit アクションを返すヘルパー
    pub fn exit_with(exit_code: i32) -> Self {
        Self {
            stdout: String::new(),
            stderr: String::new(),
            exit_code,
            action: LoopAction::Exit,
            used_alt_screen: false,
        }
    }

    /// Restart アクションを返すヘルパー
    pub fn restart() -> Self {
        Self {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            action: LoopAction::Restart,
            used_alt_screen: false,
        }
    }
}

#[cfg(test)]
mod loop_action_tests {
    use super::*;

    #[test]
    fn restart_is_distinct_from_continue_and_exit() {
        assert_ne!(LoopAction::Restart, LoopAction::Continue);
        assert_ne!(LoopAction::Restart, LoopAction::Exit);
        assert_ne!(LoopAction::Continue, LoopAction::Exit);
    }

    #[test]
    fn command_result_restart_fields() {
        let result = CommandResult::restart();
        assert_eq!(result.action, LoopAction::Restart);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
        assert!(!result.used_alt_screen);
    }

    #[test]
    fn command_result_success_is_continue() {
        let result = CommandResult::success("output".to_string());
        assert_eq!(result.action, LoopAction::Continue);
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn command_result_exit_with_is_exit() {
        let result = CommandResult::exit_with(42);
        assert_eq!(result.action, LoopAction::Exit);
        assert_eq!(result.exit_code, 42);
    }

    #[test]
    fn command_result_error_is_continue() {
        let result = CommandResult::error("err".to_string(), 1);
        assert_eq!(result.action, LoopAction::Continue);
        assert_eq!(result.exit_code, 1);
    }
}
