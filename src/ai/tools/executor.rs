//! AI ツールのローカル実行
//!
//! AI が呼び出したツール（read_file, write_file）をローカルで実行する。
//! execute_shell_command はここでは処理しない（呼び出し前にフィルタ済み）。

use tracing::{debug, info, warn};

use crate::cli::jarvis::{jarvis_read_file, jarvis_write_file};

/// ツール名と引数に基づいてローカルでツールを実行する。
/// execute_shell_command はこの関数では処理しない（呼び出し前にフィルタ済み）。
pub fn execute_tool(function_name: &str, arguments: &str) -> String {
    debug!(
        function_name = %function_name,
        arguments = %arguments,
        "Executing tool locally"
    );

    match function_name {
        "read_file" => execute_read_file(arguments),
        "write_file" => execute_write_file(arguments),
        other => {
            warn!(tool = %other, "Unknown tool called");
            format!("Error: Unknown tool '{other}'")
        }
    }
}

/// read_file ツールのローカル実行
fn execute_read_file(arguments: &str) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(e) => return format!("Error parsing arguments: {e}"),
    };

    let path = match parsed.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return "Error: 'path' parameter is required".to_string(),
    };

    let spinner = jarvis_read_file(path);

    let result = match std::fs::read_to_string(path) {
        Ok(content) => {
            info!(path = %path, content_length = content.len(), "File read successfully");
            content
        }
        Err(e) => {
            warn!(path = %path, error = %e, "Failed to read file");
            format!("Error reading file '{path}': {e}")
        }
    };

    spinner.finish_and_clear();
    result
}

/// write_file ツールのローカル実行
fn execute_write_file(arguments: &str) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(e) => return format!("Error parsing arguments: {e}"),
    };

    let path = match parsed.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return "Error: 'path' parameter is required".to_string(),
    };

    let content = match parsed.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return "Error: 'content' parameter is required".to_string(),
    };

    let spinner = jarvis_write_file(path);

    // 親ディレクトリが存在しない場合は作成
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                warn!(path = %path, error = %e, "Failed to create parent directory");
                spinner.finish_and_clear();
                return format!("Error creating directory for '{path}': {e}");
            }
        }
    }

    let result = match std::fs::write(path, content) {
        Ok(()) => {
            info!(path = %path, content_length = content.len(), "File written successfully");
            format!("Successfully wrote {} bytes to '{path}'", content.len())
        }
        Err(e) => {
            warn!(path = %path, error = %e, "Failed to write file");
            format!("Error writing file '{path}': {e}")
        }
    };

    spinner.finish_and_clear();
    result
}
