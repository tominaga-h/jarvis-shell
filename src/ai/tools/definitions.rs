//! AI ツールの JSON スキーマ定義
//!
//! OpenAI Function Calling で使用するツールの定義を管理する。

use async_openai::types::{ChatCompletionTool, ChatCompletionToolType, FunctionObject};

/// execute_shell_command ツールの定義
pub fn shell_command_tool() -> ChatCompletionTool {
    ChatCompletionTool {
        r#type: ChatCompletionToolType::Function,
        function: FunctionObject {
            name: "execute_shell_command".to_string(),
            description: Some(
                "Execute a shell command. Use this when the user's input is a shell command."
                    .to_string(),
            ),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The full shell command to execute"
                    }
                },
                "required": ["command"]
            })),
            strict: None,
        },
    }
}

/// read_file ツールの定義
pub fn read_file_tool() -> ChatCompletionTool {
    ChatCompletionTool {
        r#type: ChatCompletionToolType::Function,
        function: FunctionObject {
            name: "read_file".to_string(),
            description: Some(
                "Read the contents of a file. Use this to inspect a file before editing it. The path is relative to the user's current working directory."
                    .to_string(),
            ),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to read (relative to CWD)"
                    }
                },
                "required": ["path"]
            })),
            strict: None,
        },
    }
}

/// write_file ツールの定義
pub fn write_file_tool() -> ChatCompletionTool {
    ChatCompletionTool {
        r#type: ChatCompletionToolType::Function,
        function: FunctionObject {
            name: "write_file".to_string(),
            description: Some(
                "Write content to a file, creating it if it doesn't exist or overwriting if it does. Always read_file first before writing to preserve existing content. The path is relative to the user's current working directory."
                    .to_string(),
            ),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to write to (relative to CWD)"
                    },
                    "content": {
                        "type": "string",
                        "description": "The complete file content to write"
                    }
                },
                "required": ["path", "content"]
            })),
            strict: None,
        },
    }
}
