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

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::Deserialize;
use tracing::warn;

use crate::config::CompletionConfig;

use super::context::CompletionContext;
use super::external::run_external_capped;
use super::provider::{Candidate, CompletionProvider};

/// `[completion] external` の使用方針。
///
/// [`ExternalCompletionSettings`] が [`super::JarvishCompleter::new`]（`pub`）の
/// 引数型に現れるため `pub` にしている（`private_interfaces` lint 対応）。
/// 実際の生成箇所は `Shell::new` / `reload_config` に限られ、外部クレートからの
/// 利用は想定していない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalMode {
    /// バイナリが検出できた場合のみ使用する（デフォルト）。
    Auto,
    /// 明示的に有効化。バイナリ未検出なら無効化して警告する。
    Carapace,
    /// 無効化。
    None,
}

impl ExternalMode {
    /// `config.toml` の `external` 値として書ける正規の文字列表現を返す。
    ///
    /// `source` ビルトインのサマリー表示（`src/shell/mod.rs` の
    /// `reload_config`）で、raw な設定文字列ではなく「実際に解決された
    /// モード」を表示するために使う（未知の値が `auto` へフォールバック
    /// した事実を隠さないための対応、#88 / #89）。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ExternalMode::Auto => "auto",
            ExternalMode::Carapace => "carapace",
            ExternalMode::None => "none",
        }
    }
}

impl fmt::Display for ExternalMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `[completion]` の外部補完（carapace）関連設定を解決した実行時状態。
///
/// `Shell::new` で構築し、`Arc<RwLock<_>>` として `editor::build_editor` 経由で
/// [`CarapaceProvider`] と共有する（`git_branch_commands` と同じ配管パターン）。
/// `Shell::reload_config`（`source` ビルトイン）が `which()` 再検出込みで
/// 更新するため、セッション中に carapace をインストールしてから `source` する
/// だけで再起動なしに有効化できる。
#[derive(Debug, Clone)]
pub struct ExternalCompletionSettings {
    pub(crate) mode: ExternalMode,
    pub(crate) timeout: Duration,
    /// 検出済みの carapace バイナリパス（`mode == None` または未検出なら `None`）。
    pub(crate) binary: Option<PathBuf>,
}

impl ExternalCompletionSettings {
    /// `[completion]` 設定から実行時状態を解決する。
    ///
    /// `external` の値に応じて `which::which("carapace")` を実行し、バイナリの
    /// 有無を確定する:
    /// - `"auto"`: 検出できれば使用、できなければ黙って無効（`binary = None`）
    /// - `"carapace"`: 検出できれば使用、できなければ警告を出して無効化
    /// - `"none"`: 検出自体を行わず無効
    /// - それ以外（未知の値）: `"auto"` として扱い警告を出す
    pub(crate) fn resolve(config: &CompletionConfig) -> Self {
        let mode = match config.external.as_str() {
            "auto" => ExternalMode::Auto,
            "carapace" => ExternalMode::Carapace,
            "none" => ExternalMode::None,
            other => {
                warn!(
                    value = %other,
                    "Unknown [completion] external value; falling back to \"auto\""
                );
                ExternalMode::Auto
            }
        };
        let timeout = Duration::from_millis(config.external_timeout_ms);

        let binary = match mode {
            ExternalMode::None => None,
            ExternalMode::Auto => which::which("carapace").ok(),
            ExternalMode::Carapace => match which::which("carapace") {
                Ok(path) => Some(path),
                Err(_) => {
                    warn!(
                        "[completion] external = \"carapace\" but the carapace binary was not \
                         found on PATH; external completion disabled"
                    );
                    None
                }
            },
        };

        Self {
            mode,
            timeout,
            binary,
        }
    }
}

/// `source` ビルトインのサマリーに載せる `external:` 行の右辺（binary 部分は
/// 含まない）を組み立てる純粋関数。
///
/// `raw`（`config.toml` の `[completion] external` の生文字列）と、それを
/// [`ExternalCompletionSettings::resolve`] が実際に解決した後の
/// `settings.mode` を突き合わせ、以下のいずれかを返す:
/// - `raw` が既知の値（`"auto"` / `"carapace"` / `"none"`）と一致する場合は
///   解決後モードをそのまま表示する（例: `"carapace"`）。
/// - `raw` が未知の値の場合は `resolve()` の暗黙フォールバック（`auto`）を
///   隠さず、その旨を明示するマーカー付きで表示する
///   （例: `auto (未対応の値 "bogus" のため auto を使用)`）。
///
/// `Shell` 全体を組み立てずにユニットテストできるよう、`&str` と
/// `ExternalCompletionSettings` のみを引数に取る形にしている（#88 / #89）。
///
/// [`ExternalCompletionSettings`] と同じ理由（`mod.rs` の `pub use` 経由で
/// `Shell::reload_config` から利用するため）で `pub` にしている。
pub fn format_external_summary(raw: &str, settings: &ExternalCompletionSettings) -> String {
    let resolved = settings.mode.as_str();
    let is_known_value = matches!(raw, "auto" | "carapace" | "none");
    if is_known_value {
        resolved.to_string()
    } else {
        format!("{resolved} (未対応の値 \"{raw}\" のため {resolved} を使用)")
    }
}

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
    /// `Shell` と共有する外部補完設定（`git_branch_commands` と同じ配管
    /// パターン）。`source` コマンドによる `reload_config` が `which()` の
    /// 再検出込みで更新するため、`provide()` 呼び出しごとに短命な read を行う。
    settings: Arc<RwLock<ExternalCompletionSettings>>,
}

impl CarapaceProvider {
    pub(super) fn new(settings: Arc<RwLock<ExternalCompletionSettings>>) -> Self {
        Self { settings }
    }
}

impl CompletionProvider for CarapaceProvider {
    fn provide(&self, ctx: &CompletionContext) -> Option<Vec<Candidate>> {
        // 短命な read ロック: バイナリパスと timeout を clone したら即座に drop する
        // （`mod.rs` の aliases スナップショットと同じ方針）。
        let (mode, binary, timeout) = {
            let settings = self.settings.read().ok()?;
            (settings.mode, settings.binary.clone(), settings.timeout)
        };
        if mode == ExternalMode::None {
            // 明示的な無効化。`resolve()` は mode == None のとき binary を
            // 常に None にするが、意図を読み取りやすくするためここでも
            // 明示的にガードする。
            return None;
        }
        let binary = binary?;

        if ctx.is_first_token {
            // コマンド名自体の補完は CommandProvider の担当。
            return None;
        }

        if ctx.head_command() == Some("cd") {
            // 防御的ガード（#88 / #89）: carapace-bin 1.7.3 は cd 用の spec を
            // 同梱しておらず、`values` は事実上 PathProvider の dirs_only
            // フィルタだけを頼りに空になる（=フォールスルー）。しかし将来の
            // carapace/bridge バージョンが cd 補完（ファイルを含みうる）を
            // 発行し始めた場合、ここで通してしまうと dirs-only 契約が
            // 静かに壊れる。cd は常に PathProvider（dirs_only 判定を持つ）
            // に担当させるため、carapace 側は最初から手を引く。
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
        let stdout = run_external_capped(&binary, &args, &envs, timeout)?;

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

    const CARAPACE_TIMEOUT: Duration = Duration::from_millis(400);

    fn settings_with_binary(binary: Option<PathBuf>) -> Arc<RwLock<ExternalCompletionSettings>> {
        Arc::new(RwLock::new(ExternalCompletionSettings {
            mode: ExternalMode::Auto,
            timeout: CARAPACE_TIMEOUT,
            binary,
        }))
    }

    fn settings_with_mode_and_binary(
        mode: ExternalMode,
        binary: Option<PathBuf>,
        timeout: Duration,
    ) -> Arc<RwLock<ExternalCompletionSettings>> {
        Arc::new(RwLock::new(ExternalCompletionSettings {
            mode,
            timeout,
            binary,
        }))
    }

    #[test]
    fn provide_returns_none_when_binary_absent() {
        let provider = CarapaceProvider::new(settings_with_binary(None));
        let ctx = extract_context("git checkout ma", "git checkout ma".len());
        assert_eq!(provider.provide(&ctx), None);
    }

    #[test]
    fn provide_mode_none_returns_none_without_spawning_even_with_binary_set() {
        // ExternalMode::None は「明示的な無効化」であり、たとえ settings.binary
        // に値が入っていても（通常 resolve() では None のとき binary は常に
        // None だが、ここでは防御ガード自体を検証するため意図的に矛盾した
        // 状態を手組みする）、provide() は mode チェックで即座に return する
        // べきで、バイナリを spawn してはならない。存在しないダミーパスを
        // 渡すことで、万一 spawn されれば大きな声で失敗する（Command::spawn
        // が Err を返し、run_external_capped 経由で None にはなるが、この
        // テストの主眼は「mode == None の時点で早期 return し、そもそも
        // run_external_capped にすら到達しない」ことの確認）。
        let provider = CarapaceProvider::new(settings_with_mode_and_binary(
            ExternalMode::None,
            Some(PathBuf::from("/no/such/carapace/binary/would-fail-loudly")),
            CARAPACE_TIMEOUT,
        ));
        let ctx = extract_context("git checkout ma", "git checkout ma".len());
        assert_eq!(provider.provide(&ctx), None);
    }

    #[test]
    fn provide_returns_none_on_timeout_and_returns_quickly() {
        // タイムアウト経路の統合的な検証: 実際に遅いスクリプトを spawn させ、
        // 短い timeout で provide() が None を返しつつ、timeout を大幅に
        // 超えず速やかに戻ることを確認する（run_external_capped 自体の
        // タイムアウト・kill ロジックは external.rs 側で検証済みのため、
        // ここでは CarapaceProvider::provide() がその結果を正しく素通し
        // することのみを見る）。
        let tmpdir = tempfile::tempdir().unwrap();
        let script_path = tmpdir.path().join("slow-fake-carapace.sh");
        std::fs::write(&script_path, "#!/bin/sh\nsleep 2\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }

        let short_timeout = Duration::from_millis(100);
        let provider = CarapaceProvider::new(settings_with_mode_and_binary(
            ExternalMode::Auto,
            Some(script_path),
            short_timeout,
        ));
        let ctx = extract_context("git checkout ma", "git checkout ma".len());

        let start = std::time::Instant::now();
        let result = provider.provide(&ctx);
        let elapsed = start.elapsed();

        assert_eq!(result, None, "slow external binary should time out to None");
        assert!(
            elapsed < Duration::from_secs(1),
            "provide() should return well under 1s on timeout, took {elapsed:?}"
        );
    }

    #[test]
    fn provide_returns_none_for_first_token_even_with_binary_present() {
        // バイナリが存在する体で構築するが、実際の実行は起きない
        // (is_first_token で早期 return するはず)。存在しないダミーパスでも
        // 先頭トークン判定の方が先に効くことを確認する。
        let provider = CarapaceProvider::new(settings_with_binary(Some(PathBuf::from(
            "/no/such/carapace/binary",
        ))));
        let ctx = extract_context("gi", "gi".len());
        assert!(ctx.is_first_token);
        assert_eq!(provider.provide(&ctx), None);
    }

    #[test]
    fn provide_returns_none_for_cd_even_when_binary_would_emit_files() {
        // 防御的ガード（#88 / #89）の証明: たとえ carapace（または将来の
        // ブリッジ実装）が cd 用の spec を持ち、ファイル + ディレクトリ混在の
        // JSON を返す状況になっても、CarapaceProvider は cd を一切担当せず
        // 即座に None を返す（PathProvider の dirs_only フィルタに完全に
        // 委譲する）。これを実証するため、実行されれば file + dir 混在の
        // 合成 JSON フィクスチャを吐く偽 carapace スクリプトを用意し、
        // ガードがスクリプト起動より先に効いて None を返すことを確認する
        // （スクリプトが実際に実行されていれば stdout パース経由で
        // 候補が返り、このテストは失敗するはず）。
        let tmpdir = tempfile::tempdir().unwrap();
        let script_path = tmpdir.path().join("fake-carapace-cd.sh");
        let fixture_json = r#"{"version":"v1.13.0","messages":[],"noprefix":"","nospace":"/","usage":"","values":[{"value":"readme.txt","display":"readme.txt","tag":"files"},{"value":"subdir/","display":"subdir/","tag":"files"}]}"#;
        std::fs::write(
            &script_path,
            format!("#!/bin/sh\ncat <<'EOF'\n{fixture_json}\nEOF\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }

        let provider = CarapaceProvider::new(settings_with_binary(Some(script_path)));
        let ctx = extract_context("cd sub", "cd sub".len());
        assert_eq!(ctx.head_command(), Some("cd"));

        assert_eq!(
            provider.provide(&ctx),
            None,
            "CarapaceProvider must defer cd entirely to PathProvider's dirs_only filter, \
             even when the (would-be) carapace output mixes files and dirs"
        );
    }

    #[test]
    fn provide_returns_none_when_spans_too_short() {
        // spans().len() < 2 は対象外（防御的ガード）。通常の extract_context
        // 経路では !is_first_token のとき spans は必ず 2 要素以上になるため
        // (command_words() が非空 + partial)、このガードへは通常到達しない。
        // ここでは境界を直接検証するため、CompletionContext を手組みして
        // spans() が 1 要素になる状況を人工的に作る。
        let provider = CarapaceProvider::new(settings_with_binary(Some(PathBuf::from(
            "/no/such/carapace/binary",
        ))));

        let mut ctx = extract_context("git", "git".len());
        assert!(ctx.is_first_token);
        // is_first_token を強制的に false にしても spans() は変わらず1要素のまま。
        ctx.is_first_token = false;
        assert_eq!(ctx.spans(), vec!["git"]);

        assert_eq!(provider.provide(&ctx), None);
    }

    // ── ExternalCompletionSettings::resolve ──

    #[test]
    fn resolve_auto_mode_with_carapace_installed_detects_binary() {
        let Ok(_) = which::which("carapace") else {
            eprintln!("skipping: carapace not installed");
            return;
        };
        let config = CompletionConfig {
            external: "auto".to_string(),
            ..CompletionConfig::default()
        };
        let settings = ExternalCompletionSettings::resolve(&config);
        assert_eq!(settings.mode, ExternalMode::Auto);
        assert!(settings.binary.is_some());
    }

    #[test]
    fn resolve_none_mode_never_detects_binary_even_when_installed() {
        let config = CompletionConfig {
            external: "none".to_string(),
            ..CompletionConfig::default()
        };
        let settings = ExternalCompletionSettings::resolve(&config);
        assert_eq!(settings.mode, ExternalMode::None);
        assert!(settings.binary.is_none());
    }

    #[test]
    fn resolve_unknown_mode_falls_back_to_auto() {
        let config = CompletionConfig {
            external: "bogus".to_string(),
            ..CompletionConfig::default()
        };
        let settings = ExternalCompletionSettings::resolve(&config);
        assert_eq!(settings.mode, ExternalMode::Auto);
    }

    // ── ExternalMode::as_str / Display ──

    #[test]
    fn external_mode_as_str_matches_config_toml_values() {
        assert_eq!(ExternalMode::Auto.as_str(), "auto");
        assert_eq!(ExternalMode::Carapace.as_str(), "carapace");
        assert_eq!(ExternalMode::None.as_str(), "none");
    }

    #[test]
    fn external_mode_display_matches_as_str() {
        assert_eq!(ExternalMode::Auto.to_string(), "auto");
        assert_eq!(ExternalMode::Carapace.to_string(), "carapace");
        assert_eq!(ExternalMode::None.to_string(), "none");
    }

    // ── format_external_summary（`source` サマリーの external: 行）──
    //
    // `Shell` は構築せず、`ExternalCompletionSettings` を直接組み立てて
    // 純粋関数のみを検証する。

    #[test]
    fn format_external_summary_known_value_shows_resolved_mode_only() {
        let settings = ExternalCompletionSettings {
            mode: ExternalMode::Carapace,
            timeout: Duration::from_millis(400),
            binary: Some(PathBuf::from("/usr/local/bin/carapace")),
        };
        assert_eq!(format_external_summary("carapace", &settings), "carapace");
    }

    #[test]
    fn format_external_summary_known_auto_value_shows_auto_without_fallback_marker() {
        let settings = ExternalCompletionSettings {
            mode: ExternalMode::Auto,
            timeout: Duration::from_millis(400),
            binary: None,
        };
        let out = format_external_summary("auto", &settings);
        assert_eq!(out, "auto");
        assert!(!out.contains("未対応"));
    }

    #[test]
    fn format_external_summary_none_mode_shows_none() {
        let settings = ExternalCompletionSettings {
            mode: ExternalMode::None,
            timeout: Duration::from_millis(400),
            binary: None,
        };
        assert_eq!(format_external_summary("none", &settings), "none");
    }

    #[test]
    fn format_external_summary_unknown_value_shows_fallback_marker_with_raw_value() {
        // resolve() は未知の値を auto にフォールバックさせるため、
        // settings.mode は Auto になっている前提。
        let settings = ExternalCompletionSettings {
            mode: ExternalMode::Auto,
            timeout: Duration::from_millis(400),
            binary: None,
        };
        let out = format_external_summary("bogus", &settings);
        assert!(
            out.contains("auto"),
            "fallback summary should mention the resolved mode: {out:?}"
        );
        assert!(
            out.contains("bogus"),
            "fallback summary should mention the raw unknown value: {out:?}"
        );
        assert!(
            out.contains("未対応"),
            "fallback summary should carry a visible fallback marker: {out:?}"
        );
    }

    #[test]
    fn format_external_summary_unknown_value_end_to_end_via_resolve() {
        // resolve() が実際に fallback した結果を format_external_summary に
        // 渡す統合的な確認（raw と settings の食い違いを実際の呼び出し経路で検証）。
        let config = CompletionConfig {
            external: "typo-value".to_string(),
            ..CompletionConfig::default()
        };
        let settings = ExternalCompletionSettings::resolve(&config);
        let out = format_external_summary(&config.external, &settings);
        assert!(out.contains("auto"));
        assert!(out.contains("typo-value"));
    }

    #[test]
    #[serial]
    fn resolve_carapace_mode_missing_binary_disables_without_panic() {
        // PATH に無いことを保証するため、空の PATH で解決する。
        let original_path = std::env::var("PATH").ok();
        // SAFETY: テスト単体プロセス内で一時的に環境変数を書き換える。
        // 他のテストと並行実行されると PATH 汚染で誤検知しうるため #[serial] を付与。
        unsafe {
            std::env::set_var("PATH", "");
        }

        let config = CompletionConfig {
            external: "carapace".to_string(),
            ..CompletionConfig::default()
        };
        let settings = ExternalCompletionSettings::resolve(&config);

        unsafe {
            match original_path {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }

        assert_eq!(settings.mode, ExternalMode::Carapace);
        assert!(settings.binary.is_none());
    }

    #[test]
    fn resolve_timeout_converts_millis_to_duration() {
        let config = CompletionConfig {
            external: "none".to_string(),
            external_timeout_ms: 1234,
            ..CompletionConfig::default()
        };
        let settings = ExternalCompletionSettings::resolve(&config);
        assert_eq!(settings.timeout, Duration::from_millis(1234));
    }

    // ── hot-reload 伝播（`Shell::reload_config` の書き込み経路を模擬） ──
    //
    // 完全な `Shell` は構築せず、`Shell::new` / `reload_config` が行うのと
    // 同じ `Arc<RwLock<ExternalCompletionSettings>>` の生成・書き換えのみを
    // 直接シミュレートする（git_branch_commands の hot-reload テストと同じ方針）。

    #[test]
    fn reload_write_path_updates_shared_settings_timeout_and_mode() {
        let initial = ExternalCompletionSettings::resolve(&CompletionConfig {
            external: "none".to_string(),
            external_timeout_ms: 400,
            ..CompletionConfig::default()
        });
        let shared = Arc::new(RwLock::new(initial));

        // `Shell::reload_config` と同じ書き込み経路: 新しい config から再解決し、
        // 書き込みロックで丸ごと置き換える。
        let reloaded_config = CompletionConfig {
            external: "none".to_string(),
            external_timeout_ms: 900,
            ..CompletionConfig::default()
        };
        let resolved = ExternalCompletionSettings::resolve(&reloaded_config);
        {
            let mut guard = shared.write().unwrap();
            *guard = resolved;
        }

        let after = shared.read().unwrap();
        assert_eq!(after.mode, ExternalMode::None);
        assert_eq!(after.timeout, Duration::from_millis(900));
    }

    #[test]
    fn reload_write_path_installing_carapace_mid_session_enables_it() {
        // 「セッション中に carapace をインストールしてから source する」ケースの
        // 模擬: 最初は external = "none" 相当（binary なし）で開始し、reload 後に
        // "auto"（実機に carapace があれば検出される）へ切り替える。
        let Ok(expected_binary) = which::which("carapace") else {
            eprintln!("skipping: carapace not installed");
            return;
        };

        let initial = ExternalCompletionSettings::resolve(&CompletionConfig {
            external: "none".to_string(),
            ..CompletionConfig::default()
        });
        let shared = Arc::new(RwLock::new(initial));
        assert!(
            shared.read().unwrap().binary.is_none(),
            "external = \"none\" should never resolve a binary"
        );

        let resolved = ExternalCompletionSettings::resolve(&CompletionConfig {
            external: "auto".to_string(),
            ..CompletionConfig::default()
        });
        {
            let mut guard = shared.write().unwrap();
            *guard = resolved;
        }

        let after = shared.read().unwrap();
        assert_eq!(
            after.binary.as_deref(),
            Some(expected_binary.as_path()),
            "reload with external = \"auto\" should re-detect the now-installed carapace binary"
        );
    }

    #[test]
    #[serial]
    fn reload_write_path_provider_sees_reload_via_same_shared_arc() {
        // reload_write_path_installing_carapace_mid_session_enables_it は
        // 「共有 settings が更新されること」までを検証する。このテストは
        // さらに一歩進め、その*同じ* Arc から構築した CarapaceProvider が
        // provide() 呼び出しごとに settings を読み直していることを証明する
        // （construction-time キャッシュではなく per-call read であることの
        // 直接証拠）。CarapaceProvider::new 呼び出しは reload の**前**に
        // 一度だけ行い、reload 後に同じ provider インスタンスへ provide()
        // することで、Provider 構築後の設定変更が反映されることを示す。
        let Ok(_) = which::which("carapace") else {
            eprintln!("skipping: carapace not installed");
            return;
        };

        let tmpdir = create_test_git_repo();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        // reload 前: external = "none" 相当（binary なし）で settings を構築し、
        // provider は reload の前に一度だけ、この Arc から構築する。
        let initial = ExternalCompletionSettings::resolve(&CompletionConfig {
            external: "none".to_string(),
            ..CompletionConfig::default()
        });
        let shared = Arc::new(RwLock::new(initial));
        let provider = CarapaceProvider::new(Arc::clone(&shared));

        let ctx = extract_context("git checkout test-", "git checkout test-".len());
        let before_reload = provider.provide(&ctx);
        assert_eq!(
            before_reload, None,
            "before reload (external = \"none\"), provider should not produce candidates"
        );

        // reload: `Shell::reload_config` と同じ書き込み経路で、同じ Arc の
        // 中身を "auto"（carapace 検出込み）へ丸ごと置き換える。
        let resolved = ExternalCompletionSettings::resolve(&CompletionConfig {
            external: "auto".to_string(),
            ..CompletionConfig::default()
        });
        {
            let mut guard = shared.write().unwrap();
            *guard = resolved;
        }

        // provider インスタンス自体は再構築していない。同じインスタンスへの
        // 呼び出しが reload 後の設定を拾えていれば、construction-time
        // キャッシュではなく per-call read である証拠になる。
        let after_reload = provider.provide(&ctx);

        env::set_current_dir(&original_dir).unwrap();

        let candidates =
            after_reload.expect("provider should produce candidates after mid-session reload");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(
            values.contains(&"test-feature"),
            "reloaded provider should suggest 'test-feature' via the same shared Arc: {values:?}"
        );
    }

    // ── 統合テスト（実行時 skip: which carapace が失敗する環境では skip） ──

    /// `[completion] external = "auto"` 相当の実 detect で settings を構築する。
    fn auto_detected_settings() -> Arc<RwLock<ExternalCompletionSettings>> {
        Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig::default(),
        )))
    }

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

        let provider = CarapaceProvider::new(auto_detected_settings());
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

        let provider = CarapaceProvider::new(auto_detected_settings());
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
