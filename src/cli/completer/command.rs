//! コマンド名補完 — PATH 走査 + ビルトイン
//!
//! `$PATH` の走査はキャッシュレス（fish shell 式）で、Tab 押下ごとに
//! リアルタイムで走査する。`brew install` 等で新しいバイナリが追加された
//! 直後でも即座に補完候補に出現する。

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, RwLock};

use crate::engine::builtins::BUILTIN_COMMANDS;

use super::context::CompletionContext;
use super::provider::{Candidate, CompletionProvider};

/// コマンド名補完プロバイダ（先頭トークン）。
///
/// 先頭トークンがパスらしく見える場合（`looks_like_path`）は担当外とし、
/// `PathProvider` に処理を譲る（#321 の挙動を維持）。
///
/// `aliases` は `Shell` / `JarvishCompleter` と Arc を共有し、`alias`
/// ビルトインで定義したシェルエイリアス名を先頭トークン候補として提供する
/// （description にはエイリアス値を表示する）。
pub(super) struct CommandProvider {
    aliases: Arc<RwLock<HashMap<String, String>>>,
}

impl CommandProvider {
    pub(super) fn new(aliases: Arc<RwLock<HashMap<String, String>>>) -> Self {
        Self { aliases }
    }
}

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

        // シェルエイリアス名。同名の PATH コマンド/ビルトインが既にあっても
        // エイリアスの description（展開先の値）で上書きする — ユーザーが
        // 明示的に定義したエイリアスの意図を優先する。
        if let Ok(aliases) = self.aliases.read() {
            for (name, value) in aliases.iter() {
                if name.starts_with(partial) {
                    matches.insert(name.clone(), Some(value.clone()));
                }
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
    use super::super::context::extract_context;
    use super::*;

    #[test]
    fn provide_offers_alias_name_with_alias_value_as_description() {
        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), "git".to_string());
        let provider = CommandProvider::new(Arc::new(RwLock::new(aliases)));

        let ctx = extract_context("g", 1);
        let candidates = provider
            .provide(&ctx)
            .expect("first token should be handled");

        let g_candidate = candidates
            .iter()
            .find(|c| c.value == "g")
            .expect("alias 'g' should be offered as a candidate");
        assert_eq!(g_candidate.description.as_deref(), Some("git"));
        assert!(g_candidate.append_whitespace);
    }

    #[test]
    fn provide_no_aliases_matching_prefix_are_absent() {
        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), "git".to_string());
        let provider = CommandProvider::new(Arc::new(RwLock::new(aliases)));

        let ctx = extract_context("zzz_no_such_", "zzz_no_such_".len());
        let candidates = provider
            .provide(&ctx)
            .expect("first token should be handled");

        assert!(
            candidates.is_empty(),
            "no PATH/builtin/alias should match this prefix: {candidates:?}"
        );
    }

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
