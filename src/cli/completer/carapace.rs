//! carapace 連携 — 外部補完プログラム carapace-bin をブリッジする Provider
//!
//! 起動時に `PATH` 上の `carapace` バイナリを 1 回だけ detect し（`which::which`）、
//! 以降の `provide()` はキャッシュ済みの `Option<PathBuf>` を参照する
//! （PATH 走査を Tab 押下ごとに繰り返さない）。
//!
//! carapace の起動は `carapace <cmd> export <spans...>` で、stdout に
//! JSON（1オブジェクト）を返す。フォーマットは carapace-bin 1.7.3 で実地検証済み:
//!
//! ```json
//! {"version":"v1.13.0","messages":[],"noprefix":"","nospace":"","usage":"","values":[
//!   {"value":"main","display":"main","description":"Merge branch 'develop' for release v1.13.3","style":"blue","tag":"local branches"}
//! ]}
//! ```
//!
//! **実機で確認した挙動（carapace-bin 1.7.3, macOS）**: carapace は最後の
//! span（partial）に対して既に前方一致フィルタを掛けた状態で `values` を
//! 返す（例: `carapace git export git checkout ma` は `main` `main-2` のみを
//! 返し、`feature-x` 等は含まれない）。そのためこの Provider 側では
//! `ctx.partial` によるフィルタを重ねて行わない（carapace の責務）。

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use super::context::CompletionContext;
use super::external::run_external_capped;
use super::provider::{Candidate, CompletionProvider};

// TODO(Phase2a Task 2a.3): [completion] timeout_ms 設定で上書き可能にする。
// 現状は 400ms 固定。
const CARAPACE_TIMEOUT: Duration = Duration::from_millis(400);

/// carapace の JSON 出力全体（`#[serde(default)]` で壊れたフィールドがあっても
/// パース自体は失敗させない — resilient parsing）。
#[derive(Debug, Default, Deserialize)]
struct CarapaceExport {
    #[serde(default)]
    nospace: String,
    #[serde(default)]
    values: Vec<CarapaceValue>,
}

/// carapace の `values[]` 内の 1 要素。
#[derive(Debug, Default, Deserialize)]
struct CarapaceValue {
    #[serde(default)]
    value: String,
    #[serde(default)]
    description: String,
}

/// carapace 連携補完プロバイダ。
///
/// 先頭トークン補完（コマンド名自体の補完）は [`super::command::CommandProvider`]
/// の担当のため、`ctx.is_first_token` の場合は必ず `None`（担当外）を返す。
pub(super) struct CarapaceProvider {
    /// 起動時に detect した carapace バイナリのパス。未検出なら `None`
    /// （以降ずっと `provide()` が `None` を返し続ける = 無害な no-op）。
    binary: Option<PathBuf>,
    timeout: Duration,
}

impl CarapaceProvider {
    pub(super) fn new() -> Self {
        Self {
            binary: which::which("carapace").ok(),
            timeout: CARAPACE_TIMEOUT,
        }
    }
}

impl CompletionProvider for CarapaceProvider {
    fn provide(&self, ctx: &CompletionContext) -> Option<Vec<Candidate>> {
        let binary = self.binary.as_ref()?;

        if ctx.is_first_token {
            // コマンド名自体の補完は CommandProvider の担当。
            return None;
        }

        let spans = ctx.spans();
        if spans.len() < 2 {
            // spans[0] (コマンド名) しかない = まだサブコマンド/引数の
            // 補完対象がない。
            return None;
        }

        let mut args = vec![spans[0].clone(), "export".to_string()];
        args.extend(spans.iter().cloned());

        let envs = [("CARAPACE_LENIENT".to_string(), "1".to_string())];
        let stdout = run_external_capped(binary, &args, &envs, self.timeout)?;

        let export: CarapaceExport = serde_json::from_str(&stdout).ok()?;
        if export.values.is_empty() {
            // エラー/空 → フォールスルー（plan.md の決定事項）。
            return None;
        }

        let mut candidates: Vec<Candidate> = export
            .values
            .into_iter()
            .filter(|v| !v.value.is_empty())
            .map(|v| {
                let append_whitespace = !should_suppress_whitespace(&v.value, &export.nospace);
                Candidate {
                    value: v.value,
                    description: if v.description.is_empty() {
                        None
                    } else {
                        Some(v.description)
                    },
                    append_whitespace,
                }
            })
            .collect();

        candidates.sort_by(|a, b| a.value.cmp(&b.value));
        candidates.dedup_by(|a, b| a.value == b.value);

        if candidates.is_empty() {
            return None;
        }

        Some(candidates)
    }
}

/// 確定後にスペースを追記しないべきかどうかを判定する。
///
/// - `nospace` が `"*"`（全候補で抑制、carapace の慣習的なワイルドカード値）
/// - `nospace` に `value` の最終文字が含まれる（carapace は `nospace` を
///   「この文字で終わる値の後ろにはスペースを入れない」という文字集合として
///   使う。実地検証: ディレクトリ補完では `nospace == "/"` かつ `value` が
///   `subdir/` のように既に `/` で終わる）
/// - `value` が `/` で終わる（ディレクトリ値。上記条件と重複しうるが、
///   `nospace` が空文字列のケースへの安全側フォールバックとして明示的に判定する）
fn should_suppress_whitespace(value: &str, nospace: &str) -> bool {
    if nospace == "*" {
        return true;
    }
    if value.ends_with('/') {
        return true;
    }
    if let Some(last) = value.chars().last() {
        if nospace.contains(last) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::completer::context::extract_context;
    use serial_test::serial;
    use std::env;
    use std::process::Command;

    // ── JSON 固定文字列パーステスト（実機キャプチャ, carapace-bin 1.7.3） ──

    const SAMPLE_SINGLE_VALUE: &str = r#"{"version":"v1.13.0","messages":[],"noprefix":"","nospace":"","usage":"","values":[{"value":"main","display":"main","description":"Merge branch 'develop' for release v1.13.3","style":"blue","tag":"local branches"}]}"#;

    // `carapace git export git chec` で実機キャプチャした複数値フィクスチャ。
    const SAMPLE_MULTI_VALUE: &str = r#"{"version":"v1.13.0","messages":[],"noprefix":"","nospace":"","usage":"","values":[{"value":"check-attr","display":"check-attr","description":"Display gitattributes information","style":"dim green","tag":"low-level helper commands"},{"value":"check-ignore","display":"check-ignore","description":"Debug gitignore / exclude files","style":"dim green","tag":"low-level helper commands"},{"value":"check-mailmap","display":"check-mailmap","description":"Show canonical names and email addresses of contacts","style":"dim green","tag":"low-level helper commands"},{"value":"check-ref-format","display":"check-ref-format","description":"Ensures that a reference name is well formed","style":"dim green","tag":"low-level helper commands"},{"value":"checkout","display":"checkout","description":"Switch branches or restore working tree files","style":"blue","tag":"main commands"},{"value":"checkout-index","display":"checkout-index","description":"Copy files from the index to the working tree","style":"dim yellow","tag":"low-level manipulator commands"}]}"#;

    // `carapace git export git log --one` で実機キャプチャ（nospace = "."）。
    const SAMPLE_FLAG_NOSPACE_DOT: &str = r#"{"version":"v1.13.0","messages":[],"noprefix":"","nospace":".","usage":"","values":[{"value":"--oneline","display":"--oneline","description":"This is a shorthand for \"--pretty=oneline --abbrev-commit\" used together","tag":"longhand flags"}]}"#;

    // `carapace ls export ls` で実機キャプチャ（ディレクトリ値は末尾 '/' 済み、nospace = "/"）。
    const SAMPLE_DIR_NOSPACE_SLASH: &str = r#"{"version":"v1.13.0","messages":[],"noprefix":"","nospace":"/","usage":"","values":[{"value":"afile.txt","display":"afile.txt","tag":"files"},{"value":"subdir/","display":"subdir/","style":"blue bold","tag":"files"}]}"#;

    #[test]
    fn parse_single_value_sample_maps_to_candidate() {
        let export: CarapaceExport = serde_json::from_str(SAMPLE_SINGLE_VALUE).unwrap();
        assert_eq!(export.values.len(), 1);
        assert_eq!(export.values[0].value, "main");
        assert_eq!(
            export.values[0].description,
            "Merge branch 'develop' for release v1.13.3"
        );
        assert_eq!(export.nospace, "");
    }

    #[test]
    fn parse_multi_value_sample_all_present() {
        let export: CarapaceExport = serde_json::from_str(SAMPLE_MULTI_VALUE).unwrap();
        assert_eq!(export.values.len(), 6);
        let values: Vec<&str> = export.values.iter().map(|v| v.value.as_str()).collect();
        assert!(values.contains(&"checkout"));
        assert!(values.contains(&"check-attr"));
    }

    #[test]
    fn empty_description_maps_to_none() {
        let json = r#"{"values":[{"value":"foo","description":""}]}"#;
        let export: CarapaceExport = serde_json::from_str(json).unwrap();
        assert_eq!(export.values[0].description, "");
        // Provider の変換ロジックと同じ判定（description.is_empty() -> None）。
        assert!(export.values[0].description.is_empty());
    }

    #[test]
    fn missing_optional_fields_default_via_serde_default() {
        // messages/usage/style/tag 等が欠けていてもパースが失敗しない。
        let json = r#"{"values":[{"value":"x"}]}"#;
        let export: CarapaceExport = serde_json::from_str(json).unwrap();
        assert_eq!(export.values.len(), 1);
        assert_eq!(export.values[0].value, "x");
        assert_eq!(export.values[0].description, "");
    }

    #[test]
    fn completely_empty_object_parses_to_empty_values() {
        let export: CarapaceExport = serde_json::from_str("{}").unwrap();
        assert!(export.values.is_empty());
        assert_eq!(export.nospace, "");
    }

    // ── nospace / append_whitespace マッピング ──

    #[test]
    fn nospace_wildcard_star_suppresses_all() {
        assert!(should_suppress_whitespace("anything", "*"));
    }

    #[test]
    fn nospace_containing_last_char_suppresses() {
        // value の最終文字 ('.') が nospace 文字集合に含まれる場合は抑制される。
        assert!(should_suppress_whitespace("example.", "."));
    }

    #[test]
    fn nospace_not_containing_last_char_does_not_suppress() {
        // 実地検証: SAMPLE_FLAG_NOSPACE_DOT の nospace は "." で value は "--oneline"
        // （末尾は 'e'）なので、このケースでは抑制されない。
        let export: CarapaceExport = serde_json::from_str(SAMPLE_FLAG_NOSPACE_DOT).unwrap();
        let v = &export.values[0];
        assert!(!should_suppress_whitespace(&v.value, &export.nospace));
    }

    #[test]
    fn nospace_empty_string_does_not_suppress_plain_value() {
        assert!(!should_suppress_whitespace("main", ""));
    }

    #[test]
    fn value_ending_in_slash_suppresses_even_with_empty_nospace() {
        assert!(should_suppress_whitespace("subdir/", ""));
    }

    #[test]
    fn dir_sample_slash_suffixed_value_suppresses_whitespace() {
        let export: CarapaceExport = serde_json::from_str(SAMPLE_DIR_NOSPACE_SLASH).unwrap();
        let file = export
            .values
            .iter()
            .find(|v| v.value == "afile.txt")
            .unwrap();
        let dir = export.values.iter().find(|v| v.value == "subdir/").unwrap();
        assert!(!should_suppress_whitespace(&file.value, &export.nospace));
        assert!(should_suppress_whitespace(&dir.value, &export.nospace));
    }

    // ── dedup ──

    #[test]
    fn dedup_removes_duplicate_values_across_tags() {
        let json = r#"{"values":[
            {"value":"foo","description":"from tag A"},
            {"value":"foo","description":"from tag B"},
            {"value":"bar","description":""}
        ]}"#;
        let export: CarapaceExport = serde_json::from_str(json).unwrap();
        let mut candidates: Vec<Candidate> = export
            .values
            .into_iter()
            .map(|v| Candidate {
                value: v.value,
                description: if v.description.is_empty() {
                    None
                } else {
                    Some(v.description)
                },
                append_whitespace: true,
            })
            .collect();
        candidates.sort_by(|a, b| a.value.cmp(&b.value));
        candidates.dedup_by(|a, b| a.value == b.value);

        assert_eq!(candidates.len(), 2, "duplicate 'foo' should be removed");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(values, vec!["bar", "foo"]);
    }

    // ── provider-contract テスト ──

    #[test]
    fn provide_returns_none_when_binary_absent() {
        let provider = CarapaceProvider {
            binary: None,
            timeout: CARAPACE_TIMEOUT,
        };
        let ctx = extract_context("git checkout ma", "git checkout ma".len());
        assert_eq!(provider.provide(&ctx), None);
    }

    #[test]
    fn provide_returns_none_for_first_token_even_with_binary_present() {
        // バイナリが存在する体で構築するが、実際の実行は起きない
        // (is_first_token で早期 return するはず)。存在しないダミーパスでも
        // 先頭トークン判定の方が先に効くことを確認する。
        let provider = CarapaceProvider {
            binary: Some(PathBuf::from("/no/such/carapace/binary")),
            timeout: CARAPACE_TIMEOUT,
        };
        let ctx = extract_context("gi", "gi".len());
        assert!(ctx.is_first_token);
        assert_eq!(provider.provide(&ctx), None);
    }

    #[test]
    fn provide_returns_none_when_spans_too_short() {
        // spans().len() < 2 は対象外（防御的ガード）。通常の extract_context
        // 経路では !is_first_token のとき spans は必ず 2 要素以上になるため
        // (command_words() が非空 + partial)、このガードへは通常到達しない。
        // ここでは境界を直接検証するため、CompletionContext を手組みして
        // spans() が 1 要素になる状況を人工的に作る。
        let provider = CarapaceProvider {
            binary: Some(PathBuf::from("/no/such/carapace/binary")),
            timeout: CARAPACE_TIMEOUT,
        };

        let mut ctx = extract_context("git", "git".len());
        assert!(ctx.is_first_token);
        // is_first_token を強制的に false にしても spans() は変わらず1要素のまま。
        ctx.is_first_token = false;
        assert_eq!(ctx.spans(), vec!["git"]);

        assert_eq!(provider.provide(&ctx), None);
    }

    // ── 統合テスト（実行時 skip: which carapace が失敗する環境では skip） ──

    fn create_test_git_repo() -> tempfile::TempDir {
        let tmpdir = tempfile::tempdir().unwrap();
        let dir = tmpdir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["branch", "test-feature"])
            .current_dir(dir)
            .output()
            .unwrap();

        tmpdir
    }

    #[test]
    #[serial]
    fn integration_git_checkout_branch_prefix_includes_branch() {
        let Ok(_) = which::which("carapace") else {
            eprintln!("skipping: carapace not installed");
            return;
        };

        let tmpdir = create_test_git_repo();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let provider = CarapaceProvider::new();
        let ctx = extract_context("git checkout test-", "git checkout test-".len());
        let result = provider.provide(&ctx);

        env::set_current_dir(&original_dir).unwrap();

        let candidates = result.expect("carapace should produce candidates for git checkout");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(
            values.contains(&"test-feature"),
            "carapace git checkout completion should include 'test-feature': {values:?}"
        );
    }

    #[test]
    #[serial]
    fn integration_git_log_oneline_flag_completion() {
        let Ok(_) = which::which("carapace") else {
            eprintln!("skipping: carapace not installed");
            return;
        };

        let provider = CarapaceProvider::new();
        let ctx = extract_context("git log --one", "git log --one".len());
        let result = provider.provide(&ctx);

        let candidates = result.expect("carapace should produce candidates for git log --one");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(
            values.iter().any(|v| v.starts_with("--one")),
            "carapace git log completion should offer a --one* flag: {values:?}"
        );
    }
}
