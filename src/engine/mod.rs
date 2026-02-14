pub mod builtins;
pub mod classifier;
mod dispatch;
pub mod exec;
pub mod expand;
mod io;
pub mod parser;
mod pty;
mod redirect;
mod terminal;

pub use dispatch::{execute, try_builtin};

/// REPL ループの制御アクション
#[derive(Debug, Clone, PartialEq)]
pub enum LoopAction {
    /// ループを続行する
    Continue,
    /// ループを終了する（exit コマンド等）
    Exit,
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
}
