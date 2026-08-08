//! 設定値の型定義。

use std::collections::HashMap;

use serde::Deserialize;

/// Jarvis Shell の設定全体
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct JarvishConfig {
    /// AI 関連設定
    pub ai: AiConfig,
    /// コマンドエイリアス（キー: エイリアス名、値: 展開先コマンド文字列）
    pub alias: HashMap<String, String>,
    /// 起動時に設定する環境変数（キー: 変数名、値: 値）
    pub export: HashMap<String, String>,
    /// プロンプト表示設定
    pub prompt: PromptConfig,
    /// 補完設定
    pub completion: CompletionConfig,
    /// 起動時に実行するコマンド
    pub startup: StartupConfig,
}

/// AI 関連の設定
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    /// 使用する AI プロバイダ（`openai` / `anthropic` / `opencode-zen` / `opencode-go`）
    pub provider: String,
    /// 使用する AI モデル名
    pub model: String,
    /// 生成する最大トークン数。未指定時はプロバイダごとの既定値を使用する。
    pub max_tokens: Option<u32>,
    /// API エンドポイントの上書き（主にテスト・互換 API 用）
    pub base_url: Option<String>,
    /// API キーを読む環境変数名の上書き
    pub api_key_env: Option<String>,
    /// エージェントループの最大ラウンド数
    pub max_rounds: usize,
    /// AI レスポンスを Markdown としてレンダリングするか
    pub markdown_rendering: bool,
    /// AI パイプ (`cmd | ai "..."`) の入力テキスト文字数上限
    pub ai_pipe_max_chars: usize,
    /// AI リダイレクト (`cmd > ai "..."`) の入力テキスト文字数上限
    pub ai_redirect_max_chars: usize,
    /// 回答のランダム性（0.0 = 決定的、2.0 = 最大ランダム）
    pub temperature: f32,
    /// 異常終了時に自動調査をスキップするコマンドの前方一致パターン
    pub ignore_auto_investigation_cmds: Vec<String>,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            max_tokens: None,
            base_url: None,
            api_key_env: None,
            max_rounds: 10,
            markdown_rendering: true,
            ai_pipe_max_chars: 50_000,
            ai_redirect_max_chars: 50_000,
            temperature: 0.5,
            ignore_auto_investigation_cmds: Vec::new(),
        }
    }
}

/// プロバイダごとの `max_tokens` 既定値を返す。
pub fn default_max_tokens_for(provider: &str) -> u32 {
    match provider {
        "anthropic" => 16_384,
        "openai" | "opencode-zen" | "opencode-go" => 8_192,
        _ => 8_192,
    }
}

/// プロンプト表示の設定
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PromptConfig {
    /// NerdFont アイコンを使用するか（false の場合は ASCII/Unicode フォールバック）
    pub nerd_font: bool,
    /// Starship プロンプトを使用するか（要: starship コマンド + starship.toml）
    pub starship: bool,
}

impl Default for PromptConfig {
    fn default() -> Self {
        Self {
            nerd_font: true,
            starship: false,
        }
    }
}

/// 補完に関する設定
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CompletionConfig {
    /// ブランチ名補完を提供する git サブコマンド
    pub git_branch_commands: Vec<String>,
    /// 外部補完（carapace / zsh ブリッジ）の使用方針。
    ///
    /// TOML 上では文字列（`"auto"` / `"carapace"` / `"zsh"` / `"none"`）と
    /// 配列（例: `["zsh", "carapace"]`、優先順を明示指定）のどちらでも書ける
    /// （[`ExternalSetting`] の untagged パース）。実際の有効化判定・
    /// バイナリ検出は `cli::completer::carapace::ExternalCompletionSettings::resolve`
    /// が行う。
    pub external: ExternalSetting,
    /// 外部補完プロセスのタイムアウト（ミリ秒）
    pub external_timeout_ms: u64,
    /// zsh 補完ブリッジを常駐デーモン化するかどうか。
    ///
    /// `true`（デフォルト）: `zsh -i` を jarvish の子プロセスとして 1 本
    /// spawn し、以後のセッション中は使い回す（Tab ごとの `zsh --no-rcs`
    /// 再起動コストを避ける）。`Shell::new` がシェル起動直後にバックグラウンド
    /// スレッドから事前ウォームアップする（[`prewarm_zsh_daemon`]
    /// (crate::cli::completer::zsh_bridge::prewarm_zsh_daemon)）ため、通常は
    /// 最初の Tab 押下時点で既にウォーム状態になっている。プリウォームが
    /// 間に合わなかった場合（または zsh 未検出等でスキップされた場合）は、
    /// 最初にデーモンを必要とする Tab 押下で遅延 spawn する経路が
    /// フォールバックとして機能する。ウォームリクエストのタイムアウトは
    /// 2000ms を下限とし（`tmuxinator` 等インタプリタ起動を伴う遅い補完関数
    /// を許容するため）、1回のタイムアウトでは kill しない（次の Tab で
    /// 残留応答を排水するグレースドレイン）。連続2回のタイムアウトで
    /// 初めてハングと判定し、デーモンをバックグラウンドで kill して次の
    /// Tab で遅延 respawn する（サーキットブレーカー）。
    /// `false`: 常に [`ExternalKind::Zsh`](crate::cli::completer::ExternalKind)
    /// のワンショット経路（`zsh --no-rcs -c capture.zsh`）を使う（従来動作）。
    ///
    /// `source` ビルトインでホットリロードされる — `false` に切り替えると
    /// 稼働中のデーモンは**その `source` 実行時点で**即座に shutdown され
    /// （次回 Tab はワンショットにフォールバック）、`true` に戻すと次回
    /// zsh 補完リクエストで遅延 spawn される。稼働中のデーモンは Jarvish の
    /// 終了時・再起動時（`restart` ビルトイン経由を含む）にも必ず明示的に
    /// shutdown される。
    pub external_zsh_daemon: bool,
}

impl Default for CompletionConfig {
    fn default() -> Self {
        Self {
            git_branch_commands: [
                "checkout",
                "switch",
                "merge",
                "rebase",
                "branch",
                "diff",
                "log",
                "cherry-pick",
                "reset",
                "push",
                "fetch",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            external: ExternalSetting::default(),
            external_timeout_ms: 400,
            external_zsh_daemon: true,
        }
    }
}

/// `[completion] external` の生設定値（TOML パース直後の未解決形）。
///
/// 文字列 1 個（`"auto"` / `"carapace"` / `"zsh"` / `"none"`）と、配列
/// （例: `["zsh", "carapace"]` — プロバイダの優先順を明示指定）の両方の
/// TOML 表現を受け付ける untagged enum。バイナリ検出やフォールバック判定は
/// 行わない（それは `ExternalCompletionSettings::resolve` の責務）。
///
/// `#[serde(default)]` の `CompletionConfig` から参照されるため、この型自体も
/// `Default` を実装する（値は `Single("auto")` — 後方互換の起点）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum ExternalSetting {
    /// `external = "auto"` のような単一文字列形式（既存の後方互換形式）。
    Single(String),
    /// `external = ["zsh", "carapace"]` のような配列形式（明示的な優先順）。
    List(Vec<String>),
}

impl Default for ExternalSetting {
    fn default() -> Self {
        ExternalSetting::Single("auto".to_string())
    }
}

impl ExternalSetting {
    /// 解決前の生の値を文字列のリストとして返す（`Single` は 1 要素）。
    ///
    /// `resolve()` 側で「既知の値かどうか」の判定や警告メッセージ組み立てに使う。
    pub(crate) fn raw_entries(&self) -> Vec<&str> {
        match self {
            ExternalSetting::Single(s) => vec![s.as_str()],
            ExternalSetting::List(list) => list.iter().map(String::as_str).collect(),
        }
    }
}

impl std::fmt::Display for ExternalSetting {
    /// ログ出力・`source` サマリーの raw 値表示に使う。
    /// `Single` はそのまま、`List` は TOML の配列表記に近い `["a", "b"]` 形式。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExternalSetting::Single(s) => write!(f, "{s}"),
            ExternalSetting::List(list) => {
                write!(f, "[")?;
                for (i, entry) in list.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{entry:?}")?;
                }
                write!(f, "]")
            }
        }
    }
}

impl PartialEq<&str> for ExternalSetting {
    /// テスト・呼び出し側の可読性のための比較補助
    /// （`Single("auto") == "auto"`）。配列形式とは常に不一致。
    fn eq(&self, other: &&str) -> bool {
        matches!(self, ExternalSetting::Single(s) if s == other)
    }
}

/// 起動時コマンドの設定
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct StartupConfig {
    /// シェル起動時に順次実行するコマンドのリスト
    pub commands: Vec<String>,
}
