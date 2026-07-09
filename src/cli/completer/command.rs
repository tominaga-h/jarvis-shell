//! コマンド名補完 — PATH 走査 + ビルトイン
//!
//! `$PATH` の走査はキャッシュレス（fish shell 式）で、Tab 押下ごとに
//! リアルタイムで走査する。`brew install` 等で新しいバイナリが追加された
//! 直後でも即座に補完候補に出現する。

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use crate::engine::builtins::BUILTIN_COMMANDS;

use super::context::CompletionContext;
use super::provider::{Candidate, CompletionProvider};

/// コマンド名補完プロバイダ（先頭トークン）。
///
/// 先頭トークンがパスらしく見える場合（`looks_like_path`）は担当外とし、
/// `PathProvider` に処理を譲る（#321 の挙動を維持）。
pub(super) struct CommandProvider;

impl CompletionProvider for CommandProvider {
    fn provide(&self, ctx: &CompletionContext) -> Option<Vec<Candidate>> {
        if !ctx.is_first_token || looks_like_path(&ctx.partial) {
            return None;
        }

        let partial = ctx.partial.as_str();

        // 名前をキーにしたマップで統合する。同名が PATH 上の実行ファイルと
        // ビルトインの両方に存在する場合（例: macOS の `/usr/bin/cd`）、
        // ビルトインの説明文を優先する。
        let mut matches: BTreeMap<String, Option<String>> = scan_path_commands(partial)
            .into_iter()
            .map(|name| (name, None))
            .collect();

        for (cmd, description) in BUILTIN_COMMANDS {
            if cmd.starts_with(partial) {
                matches.insert(cmd.to_string(), Some((*description).to_string()));
            }
        }

        Some(
            matches
                .into_iter()
                .map(|(value, description)| Candidate {
                    value,
                    description,
                    append_whitespace: true,
                })
                .collect(),
        )
    }
}

/// 先頭トークンがパスらしいかを判定する。
///
/// `/` を含む (`./target/debug/`, `bin/foo`, `/usr/bin/ls`, `~/bin/x`)、
/// または `~` で始まる (`~` 単体もホーム基準) 場合にファイル補完へ回す。
pub(super) fn looks_like_path(token: &str) -> bool {
    token.contains('/') || token.starts_with('~')
}

/// `$PATH` 上の実行可能ファイルのうち、`prefix` に前方一致するものを収集する。
///
/// 実行権限チェック (`mode & 0o111 != 0`) を行い、
/// README 等の非実行ファイルを除外する。PATH コマンドに説明文は付けない。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_path_true_cases() {
        for token in [
            "./",
            "../",
            "./target/debug/",
            "/usr/bin/ls",
            "~/",
            "~",
            "sub/foo",
        ] {
            assert!(looks_like_path(token), "'{token}' should look like a path");
        }
    }

    #[test]
    fn looks_like_path_false_cases() {
        for token in ["ls", "cargo", "git", ""] {
            assert!(
                !looks_like_path(token),
                "'{token}' should not look like a path"
            );
        }
    }
}
