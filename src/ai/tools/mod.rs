//! AI ツール関連モジュール
//!
//! ツールの定義、実行、Tool Call のストリーミング処理を管理する。

pub mod call;
pub mod definitions;
pub mod executor;

use async_openai::types::ChatCompletionTool;

/// すべてのツール定義を構築する
pub fn build_tools() -> Vec<ChatCompletionTool> {
    vec![
        definitions::shell_command_tool(),
        definitions::read_file_tool(),
        definitions::write_file_tool(),
    ]
}
