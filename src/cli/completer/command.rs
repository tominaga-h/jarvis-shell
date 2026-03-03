//! コマンド名補完 — PATH 走査 + ビルトイン

use std::fs;
use std::os::unix::fs::PermissionsExt;

use reedline::{Span, Suggestion};

/// ビルトインコマンド名（補完候補に常に含める）
const BUILTIN_COMMANDS: &[&str] = &["cd", "cwd", "exit", "export", "unset", "help", "history"];

impl super::JarvishCompleter {
    /// コマンド名補完（先頭トークン）
    ///
    /// `$PATH` をリアルタイム走査し、ビルトインコマンドと合わせて候補を生成する。
    pub(super) fn complete_command(&self, partial: &str, span: Span) -> Vec<Suggestion> {
        let mut matches = scan_path_commands(partial);

        for cmd in BUILTIN_COMMANDS {
            if cmd.starts_with(partial) {
                matches.push(cmd.to_string());
            }
        }

        matches.sort_unstable();
        matches.dedup();

        matches
            .into_iter()
            .map(|cmd| Suggestion {
                value: cmd,
                description: None,
                style: None,
                extra: None,
                span,
                append_whitespace: true,
                match_indices: None,
            })
            .collect()
    }
}

/// `$PATH` 上の実行可能ファイルのうち、`prefix` に前方一致するものを収集する。
///
/// 実行権限チェック (`mode & 0o111 != 0`) を行い、
/// README 等の非実行ファイルを除外する。
fn scan_path_commands(prefix: &str) -> Vec<String> {
    let path_var = match std::env::var("PATH") {
        Ok(p) => p,
        Err(_) => return vec![],
    };

    let mut commands = Vec::new();
    for dir in std::env::split_paths(&path_var) {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if !name.starts_with(prefix) {
                    continue;
                }
                if let Ok(metadata) = fs::metadata(entry.path()) {
                    if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                        commands.push(name.to_string());
                    }
                }
            }
        }
    }
    commands
}
