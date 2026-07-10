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

/// 個々の外部補完プロバイダの種別。
///
/// [`ExternalCompletionSettings`] が [`super::JarvishCompleter::new`]（`pub`）の
/// 引数型に現れるため `pub` にしている（`private_interfaces` lint 対応）。
/// 実際の生成箇所は `Shell::new` / `reload_config` に限られ、外部クレートからの
/// 利用は想定していない。
///
/// バリアントの追加順（`ALL` の並び）が `"auto"` 解決時のデフォルト優先順
/// （carapace → zsh）を兼ねる。carapace の方が起動コストが低く description
/// が付きやすいため先に試す（`mod.rs` のプロバイダチェーンと同じ理由）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalKind {
    /// carapace-bin ブリッジ（[`CarapaceProvider`]）。
    Carapace,
    /// zsh compsys ブリッジ（[`super::zsh_bridge::ZshBridgeProvider`]）。
    Zsh,
}

impl ExternalKind {
    /// `"auto"` 解決時に試す既定の優先順（carapace → zsh）。
    const ALL: [ExternalKind; 2] = [ExternalKind::Carapace, ExternalKind::Zsh];

    /// `config.toml` の `external` 値として書ける正規の文字列表現を返す。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ExternalKind::Carapace => "carapace",
            ExternalKind::Zsh => "zsh",
        }
    }

    /// `which()` で検出する実行ファイル名。
    fn binary_name(self) -> &'static str {
        match self {
            ExternalKind::Carapace => "carapace",
            ExternalKind::Zsh => "zsh",
        }
    }

    /// `config.toml` の文字列表現から対応する種別を引く（`"auto"` / `"none"` は含まない）。
    fn from_str(value: &str) -> Option<Self> {
        match value {
            "carapace" => Some(ExternalKind::Carapace),
            "zsh" => Some(ExternalKind::Zsh),
            _ => None,
        }
    }
}

impl fmt::Display for ExternalKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 解決済みの外部補完プロバイダ 1 件（優先順のうちの 1 エントリ）。
///
/// `binary` が `None` の場合、そのプロバイダはバイナリ未検出のため無効
/// （`CarapaceProvider` / `ZshBridgeProvider` は `binary_path()` 経由でこれを
/// 見て自身を無効化する）。無効なエントリもリストからは削除せず残す —
/// `source` サマリーで「carapace: not found」のように可視化するため。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedExternal {
    pub(crate) kind: ExternalKind,
    pub(crate) binary: Option<PathBuf>,
}

/// `[completion]` の外部補完（carapace / zsh ブリッジ）関連設定を解決した実行時状態。
///
/// `Shell::new` で構築し、`Arc<RwLock<_>>` として `editor::build_editor` 経由で
/// [`CarapaceProvider`] / [`super::zsh_bridge::ZshBridgeProvider`] と共有する
/// （`git_branch_commands` と同じ配管パターン）。`Shell::reload_config`
/// （`source` ビルトイン）が `which()` 再検出込みで更新するため、セッション中に
/// carapace/zsh をインストールしてから `source` するだけで再起動なしに
/// 有効化できる。
#[derive(Debug, Clone)]
pub struct ExternalCompletionSettings {
    pub(crate) timeout: Duration,
    /// 解決済みの有効プロバイダ列（優先順）。`resolve()` が構築する。
    /// `"none"` の場合は空。
    pub(crate) enabled: Vec<ResolvedExternal>,
}

impl ExternalCompletionSettings {
    /// `[completion]` 設定から実行時状態を解決する。
    ///
    /// `external`（[`ExternalSetting`](crate::config::ExternalSetting)）の
    /// 形式に応じて以下のように優先順リストを組み立てる:
    /// - 文字列 `"auto"`（デフォルト）: [`ExternalKind::ALL`] の順（carapace →
    ///   zsh）で、それぞれのバイナリが検出できたものだけを有効化する
    ///   （検出できなくても警告は出さない — 未インストールは通常運用）。
    /// - 文字列 `"none"`: 全プロバイダ無効（`which()` すら呼ばない）。
    /// - 文字列 `"carapace"` / `"zsh"`: そのプロバイダのみを対象にする。
    ///   バイナリ未検出なら警告を出し、`binary = None` のエントリとして残す
    ///   （「明示指定したのに無効」という事実を隠さない）。
    /// - 配列（例: `["zsh", "carapace"]`）: 要素の記載順をそのまま優先順として
    ///   採用する。各要素は `"carapace"` / `"zsh"` のみ有効 — それ以外の要素
    ///   （`"auto"` / `"none"` / 不正な値）は警告を出してその要素だけ
    ///   スキップする（配列全体は無効にしない）。
    /// - 文字列の未知の値: `"auto"` として扱い警告を出す。
    pub(crate) fn resolve(config: &CompletionConfig) -> Self {
        let timeout = Duration::from_millis(config.external_timeout_ms);
        let enabled = resolve_enabled_kinds(&config.external);
        Self { timeout, enabled }
    }

    /// 指定した種別のプロバイダが有効化されており、かつバイナリが検出済みなら
    /// そのパスを返す。無効化されている・リストに存在しない・バイナリ未検出の
    /// いずれの場合も `None`。
    pub(crate) fn binary_path(&self, kind: ExternalKind) -> Option<&PathBuf> {
        self.enabled
            .iter()
            .find(|entry| entry.kind == kind)
            .and_then(|entry| entry.binary.as_ref())
    }
}

/// 各外部補完プロバイダの `provide()` 冒頭で共通する「read ロック → 有効化判定
/// → timeout 取得」ゲートを一本化したヘルパー。
///
/// [`CarapaceProvider::provide`] と
/// [`super::zsh_bridge::ZshBridgeProvider::provide`] はどちらも同じ手順
/// （短命な read ロックを取り、`kind` が優先順リストに含まれ、かつバイナリが
/// 検出済みか確認し、実効タイムアウトを求める）を踏む。以前はこの手順が
/// 両ファイルにコピペされており、`zsh_bridge.rs` 側にだけ `MIN_TIMEOUT_MS`
/// フロアが後付けされた結果 2 箇所の実装が drift していた（#89 レビュー
/// 指摘）。このヘルパーに一本化することで、今後どちらかを変更すれば
/// もう一方にも自動的に反映される。
///
/// `min_timeout` に `Some(floor)` を渡すと、共有設定の `timeout` と `floor`
/// の大きい方を実効タイムアウトとして使う（zsh ブリッジの
/// [`super::zsh_bridge::MIN_TIMEOUT_MS`] 用途）。`None` を渡すと共有設定の
/// `timeout` をそのまま使う（carapace は起動コストが低く、下限フロアを
/// 必要としない）。
///
/// 戻り値は `(binary_path, effective_timeout)`。無効化されている・バイナリ
/// 未検出の場合は `None`（呼び出し元はこれを受けて `provide()` 全体を
/// `None` に縮退する）。
pub(super) fn gate(
    settings: &Arc<RwLock<ExternalCompletionSettings>>,
    kind: ExternalKind,
    min_timeout: Option<Duration>,
) -> Option<(PathBuf, Duration)> {
    let settings = settings.read().ok()?;
    let binary = settings.binary_path(kind)?.clone();
    let timeout = match min_timeout {
        Some(floor) => settings.timeout.max(floor),
        None => settings.timeout,
    };
    Some((binary, timeout))
}

/// `"auto"` 相当の優先順（[`ExternalKind::ALL`]）で、実機に検出できた
/// バイナリのプロバイダだけを有効化する。検出できなくても警告は出さない
/// （未インストールは通常運用のため — `resolve_enabled_kinds` の "auto" /
/// 未知の値フォールバックの両方から共有される）。
fn resolve_auto_order() -> Vec<ResolvedExternal> {
    ExternalKind::ALL
        .iter()
        .filter_map(|&kind| {
            which::which(kind.binary_name())
                .ok()
                .map(|binary| ResolvedExternal {
                    kind,
                    binary: Some(binary),
                })
        })
        .collect()
}

/// `"carapace"` / `"zsh"` の単一種別を明示指定した場合の解決。
/// バイナリ未検出なら警告を出しつつ、エントリ自体は
/// `binary = None` で残す（明示指定したのに無効という事実を隠さない）。
fn resolve_single_kind(kind: ExternalKind, raw: &str) -> ResolvedExternal {
    let binary = which::which(kind.binary_name()).ok();
    if binary.is_none() {
        warn!(
            value = %raw,
            "[completion] external = \"{raw}\" but its binary was not found \
             on PATH; external completion disabled for this provider"
        );
    }
    ResolvedExternal { kind, binary }
}

/// [`crate::config::ExternalSetting`] を実際の優先順リストへ解決する。
///
/// `ExternalCompletionSettings::resolve` から切り出した純粋寄りのヘルパー
/// （`which()` の呼び出しは残るため完全な純粋関数ではないが、`Duration` 計算
/// を含まないぶん `resolve()` 本体よりテストしやすい）。
///
/// 単一文字列（[`ExternalSetting::Single`]）と配列（[`ExternalSetting::List`]）
/// のどちらも [`ExternalSetting::raw_entries`] 経由でいったん `&str` 列に
/// 揃えてから解決するが、`"auto"` / `"none"` はスカラー文字列専用の特別扱い
/// （配列内に書いても無効な要素として skip される — 配列は優先順の明示指定
/// 専用の記法という設計）のため、`Single` と `List` を分けて処理する。
fn resolve_enabled_kinds(external: &crate::config::ExternalSetting) -> Vec<ResolvedExternal> {
    use crate::config::ExternalSetting;

    match external {
        ExternalSetting::Single(_) => {
            let entries = external.raw_entries();
            let raw = entries
                .first()
                .copied()
                .expect("ExternalSetting::Single always yields exactly one raw entry");
            match raw {
                "auto" => resolve_auto_order(),
                "none" => Vec::new(),
                other => match ExternalKind::from_str(other) {
                    Some(kind) => vec![resolve_single_kind(kind, other)],
                    None => {
                        warn!(
                            value = %other,
                            "Unknown [completion] external value; falling back to \"auto\""
                        );
                        resolve_auto_order()
                    }
                },
            }
        }
        ExternalSetting::List(_) => external
            .raw_entries()
            .into_iter()
            .filter_map(|raw| match ExternalKind::from_str(raw) {
                Some(kind) => Some(resolve_single_kind(kind, raw)),
                None => {
                    warn!(
                        value = %raw,
                        "Unknown [completion] external array entry; skipping it"
                    );
                    None
                }
            })
            .collect(),
    }
}

/// `source` ビルトインのサマリーに載せる `external:` 行の右辺を組み立てる純粋関数。
///
/// `raw`（`config.toml` の `[completion] external` の生の [`Display`]
/// 表現）と、[`ExternalCompletionSettings::resolve`] が実際に解決した結果
/// （`settings.enabled` の優先順リスト）を突き合わせ、以下を返す:
/// - 有効なプロバイダが 1 つ以上あれば `"carapace, zsh"` のように種別名を
///   優先順にカンマ区切りで列挙する（各プロバイダのバイナリパス自体は
///   呼び出し側 — `Shell::reload_config` — が別行で表示する）。
/// - 有効なプロバイダが 0 件なら `"none"`。
/// - `raw` が既知の値（`"auto"` / `"carapace"` / `"zsh"` / `"none"` /
///   これらのみからなる配列）でない場合は、`resolve()` の暗黙フォールバック
///   （`auto` 相当の解決）を隠さず、その旨を明示するマーカー付きで表示する
///   （例: `auto (未対応の値 "bogus" のため auto を使用)`）。
///
/// `Shell` 全体を組み立てずにユニットテストできるよう、`&str` と
/// `ExternalCompletionSettings` のみを引数に取る形にしている（#88 / #89）。
///
/// [`ExternalCompletionSettings`] と同じ理由（`mod.rs` の `pub use` 経由で
/// `Shell::reload_config` から利用するため）で `pub` にしている。
pub fn format_external_summary(raw: &str, settings: &ExternalCompletionSettings) -> String {
    let resolved = resolved_order_display(settings);
    let is_known_value = is_known_external_value(raw);
    if is_known_value {
        resolved
    } else {
        format!("{resolved} (未対応の値 \"{raw}\" のため auto を使用)")
    }
}

/// `settings.enabled` の優先順を `"carapace, zsh"` のようなカンマ区切り文字列
/// にする。空なら `"none"`。
fn resolved_order_display(settings: &ExternalCompletionSettings) -> String {
    if settings.enabled.is_empty() {
        return "none".to_string();
    }
    settings
        .enabled
        .iter()
        .map(|entry| entry.kind.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `raw`（`config.toml` の `external` 値の [`Display`] 表現）が既知の値かどうか
/// を判定する: `"auto"` / `"carapace"` / `"zsh"` / `"none"`、または
/// `"carapace"` / `"zsh"` のみからなる配列表記（`format_external_summary` の
/// 呼び出し元が渡す raw は `ExternalSetting` の `Display` 実装が生成した文字列
/// のため、配列は `["carapace", "zsh"]` の形で渡ってくる）。
fn is_known_external_value(raw: &str) -> bool {
    if matches!(raw, "auto" | "carapace" | "zsh" | "none") {
        return true;
    }
    // 配列表記 `["a", "b"]` の各要素が carapace/zsh のみで構成されているかを見る。
    let Some(inner) = raw.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
        return false;
    };
    if inner.trim().is_empty() {
        return false;
    }
    inner.split(',').all(|entry| {
        let trimmed = entry.trim().trim_matches('"');
        matches!(trimmed, "carapace" | "zsh")
    })
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
        // 短命な read ロック（`gate` 内部で取得・即座に drop する — `mod.rs`
        // の aliases スナップショットと同じ方針）。carapace は起動コストが
        // 低いため `min_timeout` フロアは適用しない（`None`）。
        let (binary, timeout) = gate(&self.settings, ExternalKind::Carapace, None)?;

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
        let enabled = binary
            .map(|b| {
                vec![ResolvedExternal {
                    kind: ExternalKind::Carapace,
                    binary: Some(b),
                }]
            })
            .unwrap_or_default();
        Arc::new(RwLock::new(ExternalCompletionSettings {
            timeout: CARAPACE_TIMEOUT,
            enabled,
        }))
    }

    fn settings_with_binary_and_timeout(
        binary: PathBuf,
        timeout: Duration,
    ) -> Arc<RwLock<ExternalCompletionSettings>> {
        Arc::new(RwLock::new(ExternalCompletionSettings {
            timeout,
            enabled: vec![ResolvedExternal {
                kind: ExternalKind::Carapace,
                binary: Some(binary),
            }],
        }))
    }

    fn settings_disabled_with_dangling_binary(
        binary: PathBuf,
        timeout: Duration,
    ) -> Arc<RwLock<ExternalCompletionSettings>> {
        // enabled が空 = carapace は無効化されている、という状態を意図的に
        // 手組みする（通常の resolve() 経路では到達しないが、provide() 側の
        // 「無効なら enabled に存在しないので None」というガード自体を
        // 検証するために使う）。binary 引数は「誤って spawn されたら
        // 大きな声で失敗する」ためのダミーパスとして受け取るが、この
        // ヘルパー自体は enabled に含めないため使用しない。
        let _ = binary;
        Arc::new(RwLock::new(ExternalCompletionSettings {
            timeout,
            enabled: Vec::new(),
        }))
    }

    #[test]
    fn provide_returns_none_when_binary_absent() {
        let provider = CarapaceProvider::new(settings_with_binary(None));
        let ctx = extract_context("git checkout ma", "git checkout ma".len());
        assert_eq!(provider.provide(&ctx), None);
    }

    #[test]
    fn provide_disabled_returns_none_without_spawning_even_with_binary_set() {
        // carapace が enabled リストに含まれていない（= 無効化されている）場合、
        // provide() は binary_path() が None を返すことで即座に return する
        // べきで、バイナリを spawn してはならない。存在しないダミーパスを
        // 渡すことで、万一 spawn されれば大きな声で失敗する（Command::spawn
        // が Err を返し、run_external_capped 経由で None にはなるが、この
        // テストの主眼は「enabled に無い時点で早期 return し、そもそも
        // run_external_capped にすら到達しない」ことの確認）。
        let provider = CarapaceProvider::new(settings_disabled_with_dangling_binary(
            PathBuf::from("/no/such/carapace/binary/would-fail-loudly"),
            CARAPACE_TIMEOUT,
        ));
        let ctx = extract_context("git checkout ma", "git checkout ma".len());
        assert_eq!(provider.provide(&ctx), None);
    }

    #[test]
    fn provide_returns_none_when_only_zsh_is_enabled() {
        // enabled に zsh のみが含まれ carapace が含まれない場合、
        // CarapaceProvider::binary_path(Carapace) は None を返し provide() は
        // 早期 return する（他プロバイダの設定に巻き込まれてはならない）。
        let settings = Arc::new(RwLock::new(ExternalCompletionSettings {
            timeout: CARAPACE_TIMEOUT,
            enabled: vec![ResolvedExternal {
                kind: ExternalKind::Zsh,
                binary: Some(PathBuf::from("/bin/zsh")),
            }],
        }));
        let provider = CarapaceProvider::new(settings);
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
        let provider =
            CarapaceProvider::new(settings_with_binary_and_timeout(script_path, short_timeout));
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

    use crate::config::ExternalSetting;

    fn config_with_external(external: ExternalSetting) -> CompletionConfig {
        CompletionConfig {
            external,
            ..CompletionConfig::default()
        }
    }

    #[test]
    fn resolve_auto_string_with_carapace_installed_detects_binary() {
        let Ok(_) = which::which("carapace") else {
            eprintln!("skipping: carapace not installed");
            return;
        };
        let config = config_with_external(ExternalSetting::Single("auto".to_string()));
        let settings = ExternalCompletionSettings::resolve(&config);
        assert!(settings.binary_path(ExternalKind::Carapace).is_some());
    }

    #[test]
    fn resolve_none_string_never_detects_any_binary_even_when_installed() {
        let config = config_with_external(ExternalSetting::Single("none".to_string()));
        let settings = ExternalCompletionSettings::resolve(&config);
        assert!(settings.enabled.is_empty());
        assert!(settings.binary_path(ExternalKind::Carapace).is_none());
        assert!(settings.binary_path(ExternalKind::Zsh).is_none());
    }

    #[test]
    fn resolve_unknown_string_falls_back_to_auto_order() {
        let config = config_with_external(ExternalSetting::Single("bogus".to_string()));
        let settings = ExternalCompletionSettings::resolve(&config);
        // auto と同じ解決になるはず: 有効化されたプロバイダは carapace → zsh の
        // 優先順のうち実機に存在するものだけ。
        let auto_settings = ExternalCompletionSettings::resolve(&config_with_external(
            ExternalSetting::Single("auto".to_string()),
        ));
        let kinds: Vec<ExternalKind> = settings.enabled.iter().map(|e| e.kind).collect();
        let auto_kinds: Vec<ExternalKind> = auto_settings.enabled.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, auto_kinds);
    }

    #[test]
    fn resolve_carapace_string_only_targets_carapace() {
        let config = config_with_external(ExternalSetting::Single("carapace".to_string()));
        let settings = ExternalCompletionSettings::resolve(&config);
        assert_eq!(settings.enabled.len(), 1);
        assert_eq!(settings.enabled[0].kind, ExternalKind::Carapace);
    }

    #[test]
    fn resolve_zsh_string_only_targets_zsh() {
        let config = config_with_external(ExternalSetting::Single("zsh".to_string()));
        let settings = ExternalCompletionSettings::resolve(&config);
        assert_eq!(settings.enabled.len(), 1);
        assert_eq!(settings.enabled[0].kind, ExternalKind::Zsh);
    }

    #[test]
    fn resolve_array_form_preserves_explicit_order() {
        let config = config_with_external(ExternalSetting::List(vec![
            "zsh".to_string(),
            "carapace".to_string(),
        ]));
        let settings = ExternalCompletionSettings::resolve(&config);
        let kinds: Vec<ExternalKind> = settings.enabled.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec![ExternalKind::Zsh, ExternalKind::Carapace]);
    }

    #[test]
    fn resolve_array_form_single_entry_only_enables_that_kind() {
        let config = config_with_external(ExternalSetting::List(vec!["zsh".to_string()]));
        let settings = ExternalCompletionSettings::resolve(&config);
        let kinds: Vec<ExternalKind> = settings.enabled.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec![ExternalKind::Zsh]);
    }

    #[test]
    fn resolve_array_form_invalid_entry_is_skipped_others_kept() {
        // 不正な要素 ("bogus") は警告のうえスキップされ、有効な要素だけが残る。
        let config = config_with_external(ExternalSetting::List(vec![
            "zsh".to_string(),
            "bogus".to_string(),
            "carapace".to_string(),
        ]));
        let settings = ExternalCompletionSettings::resolve(&config);
        let kinds: Vec<ExternalKind> = settings.enabled.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec![ExternalKind::Zsh, ExternalKind::Carapace]);
    }

    #[test]
    fn resolve_array_form_all_invalid_entries_yields_empty() {
        let config = config_with_external(ExternalSetting::List(vec![
            "auto".to_string(),
            "none".to_string(),
        ]));
        let settings = ExternalCompletionSettings::resolve(&config);
        assert!(
            settings.enabled.is_empty(),
            "array form only accepts \"carapace\"/\"zsh\" entries; \
             \"auto\"/\"none\" inside an array should be skipped entirely"
        );
    }

    // ── ExternalKind::as_str / Display ──

    #[test]
    fn external_kind_as_str_matches_config_toml_values() {
        assert_eq!(ExternalKind::Carapace.as_str(), "carapace");
        assert_eq!(ExternalKind::Zsh.as_str(), "zsh");
    }

    #[test]
    fn external_kind_display_matches_as_str() {
        assert_eq!(ExternalKind::Carapace.to_string(), "carapace");
        assert_eq!(ExternalKind::Zsh.to_string(), "zsh");
    }

    #[test]
    fn external_kind_all_order_is_carapace_then_zsh() {
        // "auto" 解決順の前提（carapace の方が起動コストが低いため先）。
        assert_eq!(
            ExternalKind::ALL,
            [ExternalKind::Carapace, ExternalKind::Zsh]
        );
    }

    // ── binary_path ──

    #[test]
    fn binary_path_returns_none_for_kind_not_in_enabled_list() {
        let settings = ExternalCompletionSettings {
            timeout: Duration::from_millis(400),
            enabled: vec![ResolvedExternal {
                kind: ExternalKind::Carapace,
                binary: Some(PathBuf::from("/usr/local/bin/carapace")),
            }],
        };
        assert!(settings.binary_path(ExternalKind::Zsh).is_none());
    }

    #[test]
    fn binary_path_returns_none_when_entry_present_but_binary_not_found() {
        let settings = ExternalCompletionSettings {
            timeout: Duration::from_millis(400),
            enabled: vec![ResolvedExternal {
                kind: ExternalKind::Carapace,
                binary: None,
            }],
        };
        assert!(settings.binary_path(ExternalKind::Carapace).is_none());
    }

    #[test]
    fn binary_path_returns_path_when_enabled_and_detected() {
        let settings = ExternalCompletionSettings {
            timeout: Duration::from_millis(400),
            enabled: vec![ResolvedExternal {
                kind: ExternalKind::Zsh,
                binary: Some(PathBuf::from("/bin/zsh")),
            }],
        };
        assert_eq!(
            settings.binary_path(ExternalKind::Zsh),
            Some(&PathBuf::from("/bin/zsh"))
        );
    }

    // ── gate（carapace / zsh ブリッジ共通の read-lock/有効化/timeout ゲート）──
    //
    // C2 (#89): 以前は同じ手順が CarapaceProvider::provide と
    // ZshBridgeProvider::provide にコピペされ、MIN_TIMEOUT_MS フロアの
    // 有無で drift していた。ここでは共有ヘルパー自体の契約
    // （無効化 kind -> None、フロアは Some のときのみ適用）を直接検証する。

    #[test]
    fn gate_returns_none_when_kind_disabled() {
        // enabled リストが空（= 全プロバイダ無効化）なら、どの kind を
        // 指定しても None。
        let settings = Arc::new(RwLock::new(ExternalCompletionSettings {
            timeout: Duration::from_millis(400),
            enabled: Vec::new(),
        }));
        assert_eq!(gate(&settings, ExternalKind::Carapace, None), None);
        assert_eq!(gate(&settings, ExternalKind::Zsh, None), None);
    }

    #[test]
    fn gate_returns_none_when_kind_not_in_enabled_list() {
        // enabled に別の kind (carapace) だけがある状態で zsh を問い合わせると None。
        let settings = Arc::new(RwLock::new(ExternalCompletionSettings {
            timeout: Duration::from_millis(400),
            enabled: vec![ResolvedExternal {
                kind: ExternalKind::Carapace,
                binary: Some(PathBuf::from("/usr/local/bin/carapace")),
            }],
        }));
        assert_eq!(gate(&settings, ExternalKind::Zsh, None), None);
    }

    #[test]
    fn gate_returns_none_when_binary_not_detected() {
        // エントリはあるが binary が None（明示指定したのに未検出のケース）。
        let settings = Arc::new(RwLock::new(ExternalCompletionSettings {
            timeout: Duration::from_millis(400),
            enabled: vec![ResolvedExternal {
                kind: ExternalKind::Carapace,
                binary: None,
            }],
        }));
        assert_eq!(gate(&settings, ExternalKind::Carapace, None), None);
    }

    #[test]
    fn gate_without_floor_uses_configured_timeout_verbatim() {
        // min_timeout = None のとき、設定 timeout がどれだけ短くてもそのまま使う
        // （carapace の実際の呼び出し方: フロアなし）。
        let settings = Arc::new(RwLock::new(ExternalCompletionSettings {
            timeout: Duration::from_millis(50),
            enabled: vec![ResolvedExternal {
                kind: ExternalKind::Carapace,
                binary: Some(PathBuf::from("/usr/local/bin/carapace")),
            }],
        }));
        let (binary, timeout) = gate(&settings, ExternalKind::Carapace, None).unwrap();
        assert_eq!(binary, PathBuf::from("/usr/local/bin/carapace"));
        assert_eq!(
            timeout,
            Duration::from_millis(50),
            "without a floor, the configured timeout must be used verbatim even if very short"
        );
    }

    #[test]
    fn gate_with_floor_raises_timeout_below_floor() {
        // min_timeout = Some(floor) かつ設定 timeout がそれ未満のとき、
        // floor まで引き上げられる（zsh ブリッジの実際の呼び出し方）。
        let settings = Arc::new(RwLock::new(ExternalCompletionSettings {
            timeout: Duration::from_millis(50),
            enabled: vec![ResolvedExternal {
                kind: ExternalKind::Zsh,
                binary: Some(PathBuf::from("/bin/zsh")),
            }],
        }));
        let floor = Duration::from_millis(2000);
        let (binary, timeout) = gate(&settings, ExternalKind::Zsh, Some(floor)).unwrap();
        assert_eq!(binary, PathBuf::from("/bin/zsh"));
        assert_eq!(
            timeout, floor,
            "configured timeout below the floor must be raised to the floor"
        );
    }

    #[test]
    fn gate_with_floor_does_not_lower_timeout_above_floor() {
        // 設定 timeout が floor を上回るときは floor に切り下げない（max の
        // 意味論をそのまま検証する）。
        let settings = Arc::new(RwLock::new(ExternalCompletionSettings {
            timeout: Duration::from_millis(5000),
            enabled: vec![ResolvedExternal {
                kind: ExternalKind::Zsh,
                binary: Some(PathBuf::from("/bin/zsh")),
            }],
        }));
        let floor = Duration::from_millis(2000);
        let (_binary, timeout) = gate(&settings, ExternalKind::Zsh, Some(floor)).unwrap();
        assert_eq!(
            timeout,
            Duration::from_millis(5000),
            "configured timeout above the floor must not be lowered to the floor"
        );
    }

    // ── format_external_summary（`source` サマリーの external: 行）──
    //
    // `Shell` は構築せず、`ExternalCompletionSettings` を直接組み立てて
    // 純粋関数のみを検証する。

    #[test]
    fn format_external_summary_known_single_value_shows_resolved_kind_only() {
        let settings = ExternalCompletionSettings {
            timeout: Duration::from_millis(400),
            enabled: vec![ResolvedExternal {
                kind: ExternalKind::Carapace,
                binary: Some(PathBuf::from("/usr/local/bin/carapace")),
            }],
        };
        assert_eq!(format_external_summary("carapace", &settings), "carapace");
    }

    #[test]
    fn format_external_summary_known_auto_value_shows_resolved_order_without_fallback_marker() {
        let settings = ExternalCompletionSettings {
            timeout: Duration::from_millis(400),
            enabled: vec![
                ResolvedExternal {
                    kind: ExternalKind::Carapace,
                    binary: Some(PathBuf::from("/usr/local/bin/carapace")),
                },
                ResolvedExternal {
                    kind: ExternalKind::Zsh,
                    binary: Some(PathBuf::from("/bin/zsh")),
                },
            ],
        };
        let out = format_external_summary("auto", &settings);
        assert_eq!(out, "carapace, zsh");
        assert!(!out.contains("未対応"));
    }

    #[test]
    fn format_external_summary_none_value_shows_none() {
        let settings = ExternalCompletionSettings {
            timeout: Duration::from_millis(400),
            enabled: Vec::new(),
        };
        assert_eq!(format_external_summary("none", &settings), "none");
    }

    #[test]
    fn format_external_summary_known_array_value_shows_resolved_order() {
        let settings = ExternalCompletionSettings {
            timeout: Duration::from_millis(400),
            enabled: vec![
                ResolvedExternal {
                    kind: ExternalKind::Zsh,
                    binary: Some(PathBuf::from("/bin/zsh")),
                },
                ResolvedExternal {
                    kind: ExternalKind::Carapace,
                    binary: Some(PathBuf::from("/usr/local/bin/carapace")),
                },
            ],
        };
        let raw = ExternalSetting::List(vec!["zsh".to_string(), "carapace".to_string()]);
        let out = format_external_summary(&raw.to_string(), &settings);
        assert_eq!(out, "zsh, carapace");
        assert!(!out.contains("未対応"));
    }

    #[test]
    fn format_external_summary_unknown_value_shows_fallback_marker_with_raw_value() {
        // resolve() は未知の値を auto として解決する。ここでは carapace のみ
        // 検出された想定の settings を手組みして検証する。
        let settings = ExternalCompletionSettings {
            timeout: Duration::from_millis(400),
            enabled: vec![ResolvedExternal {
                kind: ExternalKind::Carapace,
                binary: Some(PathBuf::from("/usr/local/bin/carapace")),
            }],
        };
        let out = format_external_summary("bogus", &settings);
        assert!(
            out.contains("carapace"),
            "fallback summary should mention the resolved order: {out:?}"
        );
        assert!(
            out.contains("bogus"),
            "fallback summary should mention the raw unknown value: {out:?}"
        );
        assert!(
            out.contains("未対応"),
            "fallback summary should carry a visible fallback marker: {out:?}"
        );
        assert!(
            out.contains("auto"),
            "fallback summary should mention that auto was used: {out:?}"
        );
    }

    #[test]
    fn format_external_summary_unknown_value_end_to_end_via_resolve() {
        // resolve() が実際に fallback した結果を format_external_summary に
        // 渡す統合的な確認（raw と settings の食い違いを実際の呼び出し経路で検証）。
        let config = config_with_external(ExternalSetting::Single("typo-value".to_string()));
        let settings = ExternalCompletionSettings::resolve(&config);
        let out = format_external_summary(&config.external.to_string(), &settings);
        assert!(out.contains("typo-value"));
        assert!(out.contains("未対応"));
    }

    #[test]
    #[serial]
    fn resolve_carapace_string_missing_binary_disables_without_panic() {
        // PATH に無いことを保証するため、空の PATH で解決する。
        let original_path = std::env::var("PATH").ok();
        // SAFETY: テスト単体プロセス内で一時的に環境変数を書き換える。
        // 他のテストと並行実行されると PATH 汚染で誤検知しうるため #[serial] を付与。
        unsafe {
            std::env::set_var("PATH", "");
        }

        let config = config_with_external(ExternalSetting::Single("carapace".to_string()));
        let settings = ExternalCompletionSettings::resolve(&config);

        unsafe {
            match original_path {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }

        // エントリ自体は残り（「明示指定したのに無効」であることが可視化される）、
        // バイナリだけが未検出になる。
        assert_eq!(settings.enabled.len(), 1);
        assert_eq!(settings.enabled[0].kind, ExternalKind::Carapace);
        assert!(settings.enabled[0].binary.is_none());
        assert!(settings.binary_path(ExternalKind::Carapace).is_none());
    }

    #[test]
    fn resolve_timeout_converts_millis_to_duration() {
        let config = CompletionConfig {
            external: ExternalSetting::Single("none".to_string()),
            external_timeout_ms: 1234,
            ..CompletionConfig::default()
        };
        let settings = ExternalCompletionSettings::resolve(&config);
        assert_eq!(settings.timeout, Duration::from_millis(1234));
    }

    // ── is_known_external_value ──

    #[test]
    fn is_known_external_value_recognizes_scalar_keywords() {
        assert!(is_known_external_value("auto"));
        assert!(is_known_external_value("carapace"));
        assert!(is_known_external_value("zsh"));
        assert!(is_known_external_value("none"));
    }

    #[test]
    fn is_known_external_value_recognizes_valid_array_display() {
        let raw = ExternalSetting::List(vec!["zsh".to_string(), "carapace".to_string()]);
        assert!(is_known_external_value(&raw.to_string()));
    }

    #[test]
    fn is_known_external_value_rejects_unknown_scalar() {
        assert!(!is_known_external_value("bogus"));
    }

    #[test]
    fn is_known_external_value_rejects_array_with_invalid_entry() {
        let raw = ExternalSetting::List(vec!["zsh".to_string(), "bogus".to_string()]);
        assert!(!is_known_external_value(&raw.to_string()));
    }

    #[test]
    fn is_known_external_value_rejects_empty_array() {
        assert!(!is_known_external_value("[]"));
    }

    // ── hot-reload 伝播（`Shell::reload_config` の書き込み経路を模擬） ──
    //
    // 完全な `Shell` は構築せず、`Shell::new` / `reload_config` が行うのと
    // 同じ `Arc<RwLock<ExternalCompletionSettings>>` の生成・書き換えのみを
    // 直接シミュレートする（git_branch_commands の hot-reload テストと同じ方針）。

    #[test]
    fn reload_write_path_updates_shared_settings_timeout_and_enabled() {
        let initial = ExternalCompletionSettings::resolve(&config_with_external(
            ExternalSetting::Single("none".to_string()),
        ));
        let initial = ExternalCompletionSettings {
            timeout: Duration::from_millis(400),
            ..initial
        };
        let shared = Arc::new(RwLock::new(initial));

        // `Shell::reload_config` と同じ書き込み経路: 新しい config から再解決し、
        // 書き込みロックで丸ごと置き換える。
        let reloaded_config = CompletionConfig {
            external: ExternalSetting::Single("none".to_string()),
            external_timeout_ms: 900,
            ..CompletionConfig::default()
        };
        let resolved = ExternalCompletionSettings::resolve(&reloaded_config);
        {
            let mut guard = shared.write().unwrap();
            *guard = resolved;
        }

        let after = shared.read().unwrap();
        assert!(after.enabled.is_empty());
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

        let initial = ExternalCompletionSettings::resolve(&config_with_external(
            ExternalSetting::Single("none".to_string()),
        ));
        let shared = Arc::new(RwLock::new(initial));
        assert!(
            shared
                .read()
                .unwrap()
                .binary_path(ExternalKind::Carapace)
                .is_none(),
            "external = \"none\" should never resolve a binary"
        );

        let resolved = ExternalCompletionSettings::resolve(&config_with_external(
            ExternalSetting::Single("auto".to_string()),
        ));
        {
            let mut guard = shared.write().unwrap();
            *guard = resolved;
        }

        let after = shared.read().unwrap();
        assert_eq!(
            after.binary_path(ExternalKind::Carapace),
            Some(&expected_binary),
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
        let initial = ExternalCompletionSettings::resolve(&config_with_external(
            ExternalSetting::Single("none".to_string()),
        ));
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
        let resolved = ExternalCompletionSettings::resolve(&config_with_external(
            ExternalSetting::Single("auto".to_string()),
        ));
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
