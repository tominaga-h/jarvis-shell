//! AI ツールのローカル実行
//!
//! AI が呼び出したツール（read_file, write_file, search_replace）をローカルで実行する。
//! execute_shell_command はここでは処理しない（呼び出し前にフィルタ済み）。

use tracing::{debug, info, warn};

use crate::cli::jarvis::{jarvis_read_file, jarvis_search_replace, jarvis_write_file};

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
        "search_replace" => execute_search_replace(arguments),
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
    if !result.starts_with("Error") {
        println!("  📖 Read: {path}");
    }
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
    if result.starts_with("Successfully") {
        println!("  📝 Wrote: {path}");
    }
    result
}

/// search_replace の内部ロジック（テスト用に分離）。
/// スピナーなしで純粋な置換処理のみを行う。
fn search_replace_inner(path: &str, old_string: &str, new_string: &str) -> String {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            warn!(path = %path, error = %e, "Failed to read file for search_replace");
            return format!("Error reading file '{path}': {e}");
        }
    };

    let match_count = content.matches(old_string).count();
    if match_count == 0 {
        warn!(path = %path, "old_string not found in file");
        return format!("Error: old_string not found in '{path}'");
    }
    if match_count > 1 {
        warn!(path = %path, match_count = match_count, "old_string matches multiple locations");
        return format!(
            "Error: old_string matches {match_count} locations in '{path}'. It must be unique."
        );
    }

    let new_content = content.replacen(old_string, new_string, 1);

    match std::fs::write(path, &new_content) {
        Ok(()) => {
            info!(path = %path, "search_replace applied successfully");
            format!("Successfully applied search_replace to '{path}'")
        }
        Err(e) => {
            warn!(path = %path, error = %e, "Failed to write file after search_replace");
            format!("Error writing file '{path}': {e}")
        }
    }
}

/// search_replace ツールのローカル実行
fn execute_search_replace(arguments: &str) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(e) => return format!("Error parsing arguments: {e}"),
    };

    let path = match parsed.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return "Error: 'path' parameter is required".to_string(),
    };
    let old_string = match parsed.get("old_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return "Error: 'old_string' parameter is required".to_string(),
    };
    let new_string = match parsed.get("new_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return "Error: 'new_string' parameter is required".to_string(),
    };

    let spinner = jarvis_search_replace(path);
    let result = search_replace_inner(path, old_string, new_string);
    spinner.finish_and_clear();
    if result.starts_with("Successfully") {
        println!("  🔧 Patched: {path}");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn search_replace_success() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        {
            let mut f = std::fs::File::create(&file_path).unwrap();
            write!(f, "hello world\nfoo bar\n").unwrap();
        }
        let path_str = file_path.to_str().unwrap();

        let result = search_replace_inner(path_str, "foo bar", "baz qux");
        assert!(result.contains("Successfully"));

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "hello world\nbaz qux\n");
    }

    #[test]
    fn search_replace_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "hello world\n").unwrap();
        let path_str = file_path.to_str().unwrap();

        let result = search_replace_inner(path_str, "nonexistent", "replacement");
        assert!(result.contains("not found"));
    }

    #[test]
    fn search_replace_multiple_matches() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "aaa\naaa\naaa\n").unwrap();
        let path_str = file_path.to_str().unwrap();

        let result = search_replace_inner(path_str, "aaa", "bbb");
        assert!(result.contains("matches 3 locations"));

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "aaa\naaa\naaa\n", "file should be unchanged");
    }

    #[test]
    fn search_replace_file_not_found() {
        let result = search_replace_inner("/tmp/nonexistent_file_12345.txt", "a", "b");
        assert!(result.contains("Error reading file"));
    }

    #[test]
    fn execute_tool_routes_search_replace() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("route_test.txt");
        std::fs::write(&file_path, "old content here\n").unwrap();
        let path_str = file_path.to_str().unwrap();

        let args = serde_json::json!({
            "path": path_str,
            "old_string": "old content",
            "new_string": "new content"
        })
        .to_string();

        let result = execute_tool("search_replace", &args);
        assert!(result.contains("Successfully"));

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "new content here\n");
    }
}
