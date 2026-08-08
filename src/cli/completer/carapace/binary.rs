//! External completion settings, binary detection, and display helpers.

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tracing::warn;

use crate::config::CompletionConfig;

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
    pub(super) const ALL: [ExternalKind; 2] = [ExternalKind::Carapace, ExternalKind::Zsh];

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
    pub(super) fn from_str(value: &str) -> Option<Self> {
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
    /// `[completion] external_zsh_daemon`。`true` なら
    /// [`super::zsh_bridge::ZshBridgeProvider`] は温存デーモン
    /// （[`super::zsh_daemon::ZshDaemon`]）経由でリクエストを処理し、
    /// `false` なら常にワンショット経路（`zsh --no-rcs -c capture.zsh`）を
    /// 使う。`reload_external_completion`（`source` ビルトイン）が
    /// 都度読み直すため、セッション中のホットリロードに対応する。
    pub(crate) zsh_daemon_enabled: bool,
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
        Self {
            timeout,
            enabled,
            zsh_daemon_enabled: config.external_zsh_daemon,
        }
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

    /// この設定の下で温存 zsh 補完デーモンが稼働してよいかどうか。
    /// `false` を返す条件は2つ:
    /// - `[completion] external_zsh_daemon = false`（デーモン機能自体が
    ///   フラグで無効化されている）
    /// - `zsh` が `enabled`（優先順リスト）に存在しない（`external =
    ///   "carapace"` や `["carapace"]` のように zsh 自体が候補から外れて
    ///   いる — バイナリが検出できているかどうかは問わない。zsh が優先順に
    ///   すら載っていない時点で `ZshBridgeProvider::provide` は
    ///   `gate()`（`binary_path` 経由）で早期 return し、デーモンを
    ///   使う機会がないため）。
    ///
    /// `Shell::reload_config` が `source` 実行の**その場**でデーモンを
    /// shutdown すべきかどうかを判定するために使う:
    /// 「フラグ off」と「zsh が enabled-kinds から外れる」の両方を
    /// reload 時点で確実に検知する）。
    pub(crate) fn should_run_zsh_daemon(&self) -> bool {
        self.zsh_daemon_enabled
            && self
                .enabled
                .iter()
                .any(|entry| entry.kind == ExternalKind::Zsh)
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
/// フロアが後付けされた結果 2 箇所の実装が drift していた。
/// このヘルパーに一本化することで、今後どちらかを変更すれば
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
pub(crate) fn gate(
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
/// `ExternalCompletionSettings` のみを引数に取る形にしている。
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

/// `source` ビルトインのサマリーに載せる、各外部補完プロバイダのバイナリパス
/// 一覧行を組み立てる純粋関数。
///
/// [`ExternalCompletionSettings::resolve`] が解決した優先順（`settings.enabled`）
/// に沿って、プロバイダごとに `"   {kind}: {path}\n"` の行を 1 つずつ列挙する。
/// バイナリが未検出（`entry.binary == None`）の場合は `"not found"` を表示する。
/// `enabled` が空（`external = "none"` または全プロバイダ無効化）の場合は
/// 空文字列を返す（サマリーに空行を出さないため）。
///
/// `Shell::reload_config` の中でインラインに組み立てられていたロジックを
/// 切り出したもの。`Shell` を構築せずに
/// `ExternalCompletionSettings` だけでユニットテストできるようにする狙いは
/// [`format_external_summary`] と同じ。
pub fn format_external_binaries_display(settings: &ExternalCompletionSettings) -> String {
    if settings.enabled.is_empty() {
        return String::new();
    }
    settings
        .enabled
        .iter()
        .map(|entry| {
            let binary_display = entry
                .binary
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "not found".to_string());
            format!("\x20\x20\x20 {}: {binary_display}\n", entry.kind)
        })
        .collect::<String>()
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
pub(super) fn is_known_external_value(raw: &str) -> bool {
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
