//! Tool Call のストリーミング蓄積・変換ヘルパー
//!
//! ストリーミングで受信した Tool Call チャンクを蓄積し、
//! 完成した Tool Call を検査・変換するユーティリティ。

use async_openai::types::{
    ChatCompletionMessageToolCall, ChatCompletionMessageToolCallChunk, ChatCompletionToolType,
    FunctionCall,
};
use tracing::{debug, warn};

/// Tool Call のストリーミングチャンクを蓄積するための構造体
#[derive(Debug, Default, Clone)]
pub struct ToolCallAccumulator {
    pub id: String,
    pub function_name: String,
    pub arguments: String,
}

/// ストリーミングで受信した Tool Call チャンクを蓄積する
pub fn accumulate_tool_call(
    accumulators: &mut Vec<ToolCallAccumulator>,
    chunk: &ChatCompletionMessageToolCallChunk,
) {
    let idx = chunk.index as usize;

    // 必要に応じてアキュムレータを拡張
    while accumulators.len() <= idx {
        accumulators.push(ToolCallAccumulator::default());
    }

    let acc = &mut accumulators[idx];

    if let Some(ref id) = chunk.id {
        acc.id = id.clone();
    }
    if let Some(ref func) = chunk.function {
        if let Some(ref name) = func.name {
            acc.function_name = name.clone();
        }
        if let Some(ref args) = func.arguments {
            acc.arguments.push_str(args);
        }
    }
}

/// 蓄積した Tool Call から execute_shell_command のコマンド文字列を抽出する。
/// read_file / write_file はここでは抽出しない。
pub fn extract_shell_command(tool_calls: &[ToolCallAccumulator]) -> Option<String> {
    for tc in tool_calls {
        debug!(
            function_name = %tc.function_name,
            arguments = %tc.arguments,
            id = %tc.id,
            "Processing tool call"
        );
        if tc.function_name == "execute_shell_command" {
            // arguments は JSON 文字列: {"command": "ls -la"}
            match serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                Ok(parsed) => {
                    if let Some(cmd) = parsed.get("command").and_then(|v| v.as_str()) {
                        debug!(extracted_command = %cmd, "Successfully extracted command from tool call");
                        return Some(cmd.to_string());
                    }
                    warn!(parsed = %parsed, "Tool call JSON parsed but 'command' field not found");
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        raw_arguments = %tc.arguments,
                        "Failed to parse tool call arguments as JSON"
                    );
                }
            }
        }
    }
    None
}

/// ToolCallAccumulator から ChatCompletionMessageToolCall を構築する（会話履歴に追加用）
pub fn build_assistant_tool_calls(
    accumulators: &[ToolCallAccumulator],
) -> Vec<ChatCompletionMessageToolCall> {
    accumulators
        .iter()
        .map(|tc| ChatCompletionMessageToolCall {
            id: tc.id.clone(),
            r#type: ChatCompletionToolType::Function,
            function: FunctionCall {
                name: tc.function_name.clone(),
                arguments: tc.arguments.clone(),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_shell_command_from_tool_calls() {
        let tool_calls = vec![ToolCallAccumulator {
            id: "call_123".to_string(),
            function_name: "execute_shell_command".to_string(),
            arguments: r#"{"command": "ls -la"}"#.to_string(),
        }];

        let cmd = extract_shell_command(&tool_calls);
        assert_eq!(cmd, Some("ls -la".to_string()));
    }

    #[test]
    fn extract_shell_command_returns_none_for_empty() {
        let tool_calls: Vec<ToolCallAccumulator> = Vec::new();
        let cmd = extract_shell_command(&tool_calls);
        assert!(cmd.is_none());
    }

    #[test]
    fn extract_shell_command_handles_invalid_json() {
        let tool_calls = vec![ToolCallAccumulator {
            id: "call_456".to_string(),
            function_name: "execute_shell_command".to_string(),
            arguments: "invalid json".to_string(),
        }];

        let cmd = extract_shell_command(&tool_calls);
        assert!(cmd.is_none());
    }

    #[test]
    fn extract_shell_command_ignores_file_tools() {
        let tool_calls = vec![
            ToolCallAccumulator {
                id: "call_1".to_string(),
                function_name: "read_file".to_string(),
                arguments: r#"{"path": "test.txt"}"#.to_string(),
            },
            ToolCallAccumulator {
                id: "call_2".to_string(),
                function_name: "write_file".to_string(),
                arguments: r#"{"path": "test.txt", "content": "hello"}"#.to_string(),
            },
        ];

        let cmd = extract_shell_command(&tool_calls);
        assert!(cmd.is_none());
    }

    #[test]
    fn build_assistant_tool_calls_works() {
        let accumulators = vec![ToolCallAccumulator {
            id: "call_123".to_string(),
            function_name: "read_file".to_string(),
            arguments: r#"{"path": "test.txt"}"#.to_string(),
        }];

        let result = build_assistant_tool_calls(&accumulators);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "call_123");
        assert_eq!(result[0].function.name, "read_file");
        assert_eq!(result[0].function.arguments, r#"{"path": "test.txt"}"#);
    }
}
