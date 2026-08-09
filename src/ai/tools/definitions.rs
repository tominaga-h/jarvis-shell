//! AI ツールの JSON スキーマ定義
//!
//! OpenAI Function Calling で使用するツールの定義を管理する。

use crate::ai::provider::types::ToolSpec;

/// execute_shell_command ツールの定義
pub fn shell_command_tool() -> ToolSpec {
    ToolSpec {
        name: "execute_shell_command".to_string(),
        description: "Execute a shell command. Use this when the user's input is a shell command."
            .to_string(),
        parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The full shell command to execute"
                    }
                },
                "required": ["command"]
        }),
    }
}

/// read_file ツールの定義
pub fn read_file_tool() -> ToolSpec {
    ToolSpec {
        name: "read_file".to_string(),
        description:
                "Read the contents of a file. Use this to inspect a file before editing it. The path is relative to the user's current working directory."
                    .to_string(),
        parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to read (relative to CWD)"
                    }
                },
                "required": ["path"]
        }),
    }
}

/// write_file ツールの定義
pub fn write_file_tool() -> ToolSpec {
    ToolSpec {
        name: "write_file".to_string(),
        description:
                "Write content to a file, creating it if it doesn't exist or overwriting if it does. Always read_file first before writing to preserve existing content. The path is relative to the user's current working directory."
                    .to_string(),
        parameters: serde_json::json!({
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
        }),
    }
}

/// search_replace ツールの定義
pub fn search_replace_tool() -> ToolSpec {
    ToolSpec {
        name: "search_replace".to_string(),
        description:
                "Make a targeted edit to a file by replacing an exact string match. \
                 Preferred over write_file for small, focused changes. \
                 The old_string must match exactly one location in the file (including whitespace/indentation). \
                 The path is relative to the user's current working directory."
                    .to_string(),
        parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to edit (relative to CWD)"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact string to find in the file (must be unique within the file)"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The replacement string"
                    }
                },
                "required": ["path", "old_string", "new_string"]
        }),
    }
}
