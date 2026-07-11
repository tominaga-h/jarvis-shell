//! `complete` ビルトインで登録されたユーザー定義補完を提供するプロバイダ
//!
//! [`CompletionRegistry`] を `Arc<RwLock<_>>` で `Shell` と共有し、Tab 補完の
//! ホットパスで読み取る。プロバイダチェーン内の位置は `CommandProvider` の
//! 直後・`GitProvider` の直前（ユーザー登録がビルトインの git 補完や外部
//! 補完より優先される）。
//!
//! 対象コマンドの判定は `ctx.expanded_head`（alias 展開後）があればそれを、
//! なければ先頭トークンをそのまま使う（[`CompletionContext::head_command`]
//! に委譲）。
//!
//! - partial が `-` で始まる場合: 登録済み spec の `-s`/`-l` からフラグ候補を
//!   前方一致で列挙する（`-x` 形式・`--long` 形式の両方、description は
//!   spec の `-d`）。
//! - それ以外: 登録済み spec の `-a`（静的候補の生文字列）を空白/クォート
//!   区切りで展開し、前方一致するものを列挙する（description は同じ spec の
//!   `-d`）。
//! - 一致件数が 0 件なら `None` を返し、後続プロバイダ（外部補完・パス補完）
//!   にフォールスルーする — このフェーズには `-f`（ファイル補完併用）相当の
//!   機能はなく、ユーザーは `-a` に静的候補を明示登録する必要がある
//!   （ファイル名まで動的に欲しい場合は spec を登録しない選択肢を取る）。

use std::sync::{Arc, RwLock};

use crate::engine::expand::split_quoted;

use super::context::CompletionContext;
use super::provider::{Candidate, CompletionProvider};
use super::registry::{CompletionRegistry, CompletionSpec};

/// ユーザー定義補完（`complete` ビルトイン）プロバイダ。
pub(super) struct RegistryProvider {
    registry: Arc<RwLock<CompletionRegistry>>,
}

impl RegistryProvider {
    pub(super) fn new(registry: Arc<RwLock<CompletionRegistry>>) -> Self {
        Self { registry }
    }
}

impl CompletionProvider for RegistryProvider {
    fn provide(&self, ctx: &CompletionContext) -> Option<Vec<Candidate>> {
        if ctx.is_first_token {
            return None;
        }

        let head = ctx.head_command()?;

        let registry = self.registry.read().ok()?;
        let specs = registry.specs_for(head);
        if specs.is_empty() {
            return None;
        }

        let candidates = if ctx.partial.starts_with('-') {
            flag_candidates(specs, &ctx.partial)
        } else {
            static_candidates(specs, &ctx.partial)
        };

        if candidates.is_empty() {
            None
        } else {
            Some(candidates)
        }
    }
}

/// `-s`/`-l` からフラグ候補を組み立てる（`partial` に前方一致するもののみ）。
fn flag_candidates(specs: &[CompletionSpec], partial: &str) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for spec in specs {
        for s in &spec.short {
            let value = format!("-{s}");
            if value.starts_with(partial) {
                candidates.push(Candidate {
                    value,
                    description: spec.description.clone(),
                    append_whitespace: true,
                });
            }
        }
        for l in &spec.long {
            let value = format!("--{l}");
            if value.starts_with(partial) {
                candidates.push(Candidate {
                    value,
                    description: spec.description.clone(),
                    append_whitespace: true,
                });
            }
        }
    }
    candidates
}

/// `-a` の静的候補文字列を展開し、`partial` に前方一致するものを返す。
///
/// `split_quoted` でのパースを試み、エラー（未閉クォート等）が出た場合は
/// 空白区切りにフォールバックする（`-a` はユーザーが自由記述するため、
/// 不正な値でもパニックせず安全に処理する）。
fn static_candidates(specs: &[CompletionSpec], partial: &str) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for spec in specs {
        let Some(raw) = &spec.arguments else {
            continue;
        };
        for word in split_arguments(raw) {
            if word.starts_with(partial) {
                candidates.push(Candidate {
                    value: word,
                    description: spec.description.clone(),
                    append_whitespace: true,
                });
            }
        }
    }
    candidates
}

/// `-a` の生文字列を単語列に分割する。`split_quoted` を優先し、失敗したら
/// 空白区切りにフォールバックする。
fn split_arguments(raw: &str) -> Vec<String> {
    match split_quoted(raw) {
        Ok(tokens) => tokens.into_iter().map(|t| t.value).collect(),
        Err(_) => raw.split_whitespace().map(str::to_string).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::context::extract_context;
    use super::*;

    fn registry_with(cmd: &str, spec: CompletionSpec) -> Arc<RwLock<CompletionRegistry>> {
        let mut registry = CompletionRegistry::new();
        registry.register(cmd, spec);
        Arc::new(RwLock::new(registry))
    }

    // ── フラグ補完 ──

    #[test]
    fn flag_completion_filters_long_by_prefix() {
        let spec = CompletionSpec {
            long: vec!["verbose".to_string(), "version".to_string()],
            ..Default::default()
        };
        let provider = RegistryProvider::new(registry_with("mycmd", spec));

        let ctx = extract_context("mycmd --v", "mycmd --v".len());
        let candidates = provider.provide(&ctx).expect("should offer flag matches");

        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"--verbose"));
        assert!(values.contains(&"--version"));
    }

    #[test]
    fn flag_completion_bare_dash_offers_both_short_and_long() {
        let spec = CompletionSpec {
            short: vec!["v".to_string()],
            long: vec!["verbose".to_string()],
            ..Default::default()
        };
        let provider = RegistryProvider::new(registry_with("mycmd", spec));

        let ctx = extract_context("mycmd -", "mycmd -".len());
        let candidates = provider.provide(&ctx).expect("should offer flag matches");

        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"-v"));
        assert!(values.contains(&"--verbose"));
    }

    #[test]
    fn flag_completion_carries_description() {
        let spec = CompletionSpec {
            long: vec!["verbose".to_string()],
            description: Some("Verbose output".to_string()),
            ..Default::default()
        };
        let provider = RegistryProvider::new(registry_with("mycmd", spec));

        let ctx = extract_context("mycmd --verb", "mycmd --verb".len());
        let candidates = provider.provide(&ctx).expect("should offer flag matches");

        let verbose = candidates
            .iter()
            .find(|c| c.value == "--verbose")
            .expect("--verbose should be present");
        assert_eq!(verbose.description.as_deref(), Some("Verbose output"));
        assert!(verbose.append_whitespace);
    }

    // ── 静的引数補完 ──

    #[test]
    fn static_arguments_with_descriptions() {
        let spec = CompletionSpec {
            arguments: Some("build test release".to_string()),
            description: Some("subcommand".to_string()),
            ..Default::default()
        };
        let provider = RegistryProvider::new(registry_with("mycmd", spec));

        let ctx = extract_context("mycmd b", "mycmd b".len());
        let candidates = provider.provide(&ctx).expect("should offer static matches");

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].value, "build");
        assert_eq!(candidates[0].description.as_deref(), Some("subcommand"));
    }

    #[test]
    fn static_arguments_quoted_words_are_split_with_split_quoted() {
        let spec = CompletionSpec {
            arguments: Some("'hello world' foo".to_string()),
            ..Default::default()
        };
        let provider = RegistryProvider::new(registry_with("mycmd", spec));

        let ctx = extract_context("mycmd h", "mycmd h".len());
        let candidates = provider.provide(&ctx).expect("should offer static matches");

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].value, "hello world");
    }

    #[test]
    fn static_arguments_falls_back_to_whitespace_split_on_parse_error() {
        // 未閉シングルクォート: split_quoted はエラーになるので空白分割にフォールバック。
        let spec = CompletionSpec {
            arguments: Some("foo 'unterminated bar".to_string()),
            ..Default::default()
        };
        let provider = RegistryProvider::new(registry_with("mycmd", spec));

        let ctx = extract_context("mycmd f", "mycmd f".len());
        let candidates = provider.provide(&ctx).expect("should offer static matches");

        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"foo"));
    }

    // ── ゼロ一致 → None ──

    #[test]
    fn zero_matches_returns_none() {
        let spec = CompletionSpec {
            arguments: Some("build test".to_string()),
            ..Default::default()
        };
        let provider = RegistryProvider::new(registry_with("mycmd", spec));

        let ctx = extract_context("mycmd zzz_no_such_", "mycmd zzz_no_such_".len());
        assert!(provider.provide(&ctx).is_none());
    }

    #[test]
    fn zero_matching_flags_returns_none() {
        let spec = CompletionSpec {
            long: vec!["verbose".to_string()],
            ..Default::default()
        };
        let provider = RegistryProvider::new(registry_with("mycmd", spec));

        let ctx = extract_context("mycmd --zzz_no_such_", "mycmd --zzz_no_such_".len());
        assert!(provider.provide(&ctx).is_none());
    }

    // ── is_first_token → None ──

    #[test]
    fn is_first_token_returns_none() {
        let spec = CompletionSpec {
            arguments: Some("build".to_string()),
            ..Default::default()
        };
        let provider = RegistryProvider::new(registry_with("mycmd", spec));

        let ctx = extract_context("mycmd", "mycmd".len());
        assert!(ctx.is_first_token);
        assert!(provider.provide(&ctx).is_none());
    }

    // ── alias head 解決 ──

    #[test]
    fn alias_expanded_head_resolves_real_command_specs() {
        let spec = CompletionSpec {
            arguments: Some("checkout".to_string()),
            ..Default::default()
        };
        let provider = RegistryProvider::new(registry_with("git", spec));

        let mut ctx = extract_context("g c", "g c".len());
        ctx.expanded_head = Some(vec!["git".to_string(), "c".to_string()]);

        let candidates = provider
            .provide(&ctx)
            .expect("expanded_head should resolve to 'git' specs");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].value, "checkout");
    }

    // ── 未登録コマンド → None ──

    #[test]
    fn unknown_command_returns_none() {
        let spec = CompletionSpec {
            arguments: Some("build".to_string()),
            ..Default::default()
        };
        let provider = RegistryProvider::new(registry_with("mycmd", spec));

        let ctx = extract_context("othercmd b", "othercmd b".len());
        assert!(provider.provide(&ctx).is_none());
    }
}
