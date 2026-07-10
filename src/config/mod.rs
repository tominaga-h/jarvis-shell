//! 設定ファイル管理
//!
//! `~/.config/jarvish/config.toml` から TOML 形式の設定を読み込む。
//! ファイルが存在しない場合はデフォルト値を使用する。
//!
//! # 設定ファイル例
//!
//! ```toml
//! [ai]
//! model = "gpt-4o"
//! max_rounds = 10
//! markdown_rendering = true
//! ai_pipe_max_chars = 50000
//! ai_redirect_max_chars = 50000
//! temperature = 0.5
//! ignore_auto_investigation_cmds = ["git log", "git diff"]
//!
//! [alias]
//! g = "git"
//! ll = "ls -la"
//!
//! [export]
//! PATH = "/usr/local/bin:$PATH"
//!
//! [prompt]
//! nerd_font = true
//! starship = false
//!
//! [completion]
//! git_branch_commands = ["checkout", "switch", "merge", "rebase", "branch", "diff", "log", "cherry-pick", "reset", "push", "fetch"]
//! external = "auto"             # "auto" | "carapace" | "zsh" | "none" | ["carapace", "zsh"]（配列で優先順を明示指定）
//! external_timeout_ms = 400     # 外部補完プロセスのタイムアウト（ミリ秒）
//! external_zsh_daemon = true    # zsh ブリッジを常駐デーモン化するか（Tab ごとの起動コストを削減）
//!
//! [startup]
//! commands = ["echo 'Welcome to jarvish!'", "export JAVA_HOME=/usr/lib/jvm/default"]
//! ```

mod defaults;

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;
use tracing::{debug, info, warn};

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
    /// 使用する AI モデル名
    pub model: String,
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
            model: "gpt-4o".to_string(),
            max_rounds: 10,
            markdown_rendering: true,
            ai_pipe_max_chars: 50_000,
            ai_redirect_max_chars: 50_000,
            temperature: 0.5,
            ignore_auto_investigation_cmds: Vec::new(),
        }
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
    /// zsh 補完ブリッジを常駐デーモン化するかどうか（Task 2b.3, #89）。
    ///
    /// `true`（デフォルト）: 初回の zsh 補完リクエスト時に `zsh -i` を
    /// jarvish の子プロセスとして 1 本 spawn し、以後のセッション中は
    /// 使い回す（Tab ごとの `zsh --no-rcs` 再起動コストを避ける）。
    /// `false`: 常に [`ExternalKind::Zsh`](crate::cli::completer::ExternalKind)
    /// のワンショット経路（`zsh --no-rcs -c capture.zsh`）を使う（従来動作）。
    ///
    /// `source` ビルトインでホットリロードされる — `false` に切り替えると
    /// 稼働中のデーモンは即座に shutdown され（次回 Tab はワンショットに
    /// フォールバック）、`true` に戻すと次回 zsh 補完リクエストで
    /// 遅延 spawn される。
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

impl JarvishConfig {
    /// 設定ファイルを読み込む。
    ///
    /// `~/.config/jarvish/config.toml` が存在すればパースし、
    /// 存在しなければデフォルト値を返す。
    pub fn load() -> Self {
        let path = Self::config_path();
        debug!(path = %path.display(), "Loading config file");

        if !path.exists() {
            Self::create_default_config(&path);
            return Self::default();
        }

        match std::fs::read_to_string(&path) {
            Ok(content) => match toml::from_str::<JarvishConfig>(&content) {
                Ok(config) => {
                    info!(
                        path = %path.display(),
                        model = %config.ai.model,
                        max_rounds = config.ai.max_rounds,
                        markdown_rendering = config.ai.markdown_rendering,
                        ignore_auto_investigation_cmds = config.ai.ignore_auto_investigation_cmds.len(),
                        alias_count = config.alias.len(),
                        export_count = config.export.len(),
                        nerd_font = config.prompt.nerd_font,
                        starship = config.prompt.starship,
                        git_branch_commands = config.completion.git_branch_commands.len(),
                        completion_external = %config.completion.external,
                        completion_external_timeout_ms = config.completion.external_timeout_ms,
                        completion_external_zsh_daemon = config.completion.external_zsh_daemon,
                        startup_commands = config.startup.commands.len(),
                        "Config loaded successfully"
                    );
                    config
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "Failed to parse config file");
                    eprintln!("jarvish: warning: failed to parse config file: {e}");
                    Self::default()
                }
            },
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Failed to read config file");
                eprintln!("jarvish: warning: failed to read config file: {e}");
                Self::default()
            }
        }
    }

    /// 指定されたパスから設定ファイルを読み込む。
    pub fn load_from(path: &std::path::Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let config = toml::from_str::<JarvishConfig>(&content)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
        info!(
            path = %path.display(),
            model = %config.ai.model,
            alias_count = config.alias.len(),
            export_count = config.export.len(),
            "Config loaded from file"
        );
        Ok(config)
    }

    /// 設定ファイルのパスを返す。
    pub fn config_path() -> PathBuf {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".config/jarvish/config.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_from_str(content: &str) -> JarvishConfig {
        toml::from_str(content).unwrap()
    }

    #[test]
    fn default_config_has_expected_values() {
        let config = JarvishConfig::default();
        assert_eq!(config.ai.model, "gpt-4o");
        assert_eq!(config.ai.max_rounds, 10);
        assert!(config.ai.markdown_rendering);
        assert!(config.ai.ignore_auto_investigation_cmds.is_empty());
        assert!(config.alias.is_empty());
        assert!(config.export.is_empty());
        assert!(config.prompt.nerd_font);
        assert!(!config.prompt.starship);
        assert!(config
            .completion
            .git_branch_commands
            .contains(&"checkout".to_string()));
        assert!(config
            .completion
            .git_branch_commands
            .contains(&"fetch".to_string()));
        assert_eq!(config.completion.git_branch_commands.len(), 11);
        assert_eq!(config.completion.external, "auto");
        assert_eq!(config.completion.external_timeout_ms, 400);
        assert!(config.completion.external_zsh_daemon);
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
[ai]
model = "gpt-4o-mini"
max_rounds = 5
markdown_rendering = false
ignore_auto_investigation_cmds = ["git log", "git diff"]

[alias]
g = "git"
ll = "ls -la"

[export]
EDITOR = "vim"

[prompt]
nerd_font = false
starship = true
"#;
        let config = load_from_str(toml);
        assert_eq!(config.ai.model, "gpt-4o-mini");
        assert_eq!(config.ai.max_rounds, 5);
        assert!(!config.ai.markdown_rendering);
        assert_eq!(
            config.ai.ignore_auto_investigation_cmds,
            vec!["git log", "git diff"]
        );
        assert_eq!(config.alias.get("g").unwrap(), "git");
        assert_eq!(config.alias.get("ll").unwrap(), "ls -la");
        assert_eq!(config.export.get("EDITOR").unwrap(), "vim");
        assert!(!config.prompt.nerd_font);
        assert!(config.prompt.starship);
    }

    #[test]
    fn parse_partial_config_uses_defaults() {
        let toml = r#"
[alias]
g = "git"
"#;
        let config = load_from_str(toml);
        assert_eq!(config.ai.model, "gpt-4o");
        assert_eq!(config.ai.max_rounds, 10);
        assert!(config.ai.markdown_rendering);
        assert!(config.ai.ignore_auto_investigation_cmds.is_empty());
        assert!(config.prompt.nerd_font);
        assert_eq!(config.alias.get("g").unwrap(), "git");
        assert!(config.export.is_empty());
    }

    #[test]
    fn parse_empty_config() {
        let config = load_from_str("");
        assert_eq!(config.ai.model, "gpt-4o");
        assert_eq!(config.ai.max_rounds, 10);
        assert!(config.alias.is_empty());
        assert!(config.export.is_empty());
    }

    #[test]
    fn parse_ignore_auto_investigation_cmds_single_entry() {
        let toml = r#"
[ai]
ignore_auto_investigation_cmds = ["git"]
"#;
        let config = load_from_str(toml);
        assert_eq!(config.ai.ignore_auto_investigation_cmds, vec!["git"]);
    }

    #[test]
    fn config_path_contains_expected_components() {
        let path = JarvishConfig::config_path();
        let path_str = path.to_string_lossy();
        assert!(path_str.contains(".config/jarvish/config.toml"));
    }

    #[test]
    fn load_returns_default_when_file_missing() {
        let config = JarvishConfig::load();
        assert!(!config.ai.model.is_empty());
        assert!(config.ai.max_rounds > 0);
    }

    #[test]
    fn load_from_valid_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("test.toml");
        std::fs::write(
            &path,
            r#"
[alias]
g = "git"

[export]
EDITOR = "vim"
"#,
        )
        .unwrap();

        let config = JarvishConfig::load_from(&path).unwrap();
        assert_eq!(config.alias.get("g").unwrap(), "git");
        assert_eq!(config.export.get("EDITOR").unwrap(), "vim");
        assert_eq!(config.ai.model, "gpt-4o");
    }

    #[test]
    fn load_from_nonexistent_file_returns_error() {
        let result = JarvishConfig::load_from(std::path::Path::new("/nonexistent/config.toml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed to read"));
    }

    #[test]
    fn load_from_invalid_toml_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("bad.toml");
        std::fs::write(&path, "this is not valid toml [[[").unwrap();

        let result = JarvishConfig::load_from(&path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed to parse"));
    }

    #[test]
    fn create_default_config_creates_file_and_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("sub/dir/config.toml");

        assert!(!path.exists());
        JarvishConfig::create_default_config(&path);

        assert!(path.exists());

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[ai]"));
        assert!(content.contains("[alias]"));
        assert!(content.contains("[export]"));
        assert!(content.contains("[completion]"));
        assert!(content.contains("external = \"auto\""));
        assert!(content.contains("external_timeout_ms = 400"));
        assert!(content.contains("external_zsh_daemon = true"));

        let config: JarvishConfig = toml::from_str(&content).unwrap();
        assert_eq!(config.ai.model, "gpt-4o");
        assert!(config.alias.is_empty());
    }

    #[test]
    fn parse_completion_config_custom_commands() {
        let toml = r#"
[completion]
git_branch_commands = ["checkout", "fetch", "pull"]
"#;
        let config = load_from_str(toml);
        assert_eq!(
            config.completion.git_branch_commands,
            vec!["checkout", "fetch", "pull"]
        );
    }

    #[test]
    fn parse_completion_config_empty_commands() {
        let toml = r#"
[completion]
git_branch_commands = []
"#;
        let config = load_from_str(toml);
        assert!(config.completion.git_branch_commands.is_empty());
    }

    #[test]
    fn parse_no_completion_section_uses_default() {
        let config = load_from_str("");
        assert_eq!(config.completion.git_branch_commands.len(), 11);
        assert!(config
            .completion
            .git_branch_commands
            .contains(&"fetch".to_string()));
        assert_eq!(config.completion.external, "auto");
        assert_eq!(config.completion.external_timeout_ms, 400);
    }

    #[test]
    fn parse_completion_config_external_carapace() {
        let toml = r#"
[completion]
external = "carapace"
"#;
        let config = load_from_str(toml);
        assert_eq!(config.completion.external, "carapace");
    }

    #[test]
    fn parse_completion_config_external_none() {
        let toml = r#"
[completion]
external = "none"
"#;
        let config = load_from_str(toml);
        assert_eq!(config.completion.external, "none");
    }

    #[test]
    fn parse_completion_config_external_unknown_value_kept_as_is() {
        // TOML パース自体は文字列をそのまま受け入れる。
        // "auto" への読み替えと警告は ExternalCompletionSettings 構築時（実行時）の責務。
        let toml = r#"
[completion]
external = "bogus"
"#;
        let config = load_from_str(toml);
        assert_eq!(config.completion.external, "bogus");
    }

    // ── ExternalSetting: 文字列 / 配列両対応 (Task 2b.4) ──

    #[test]
    fn parse_completion_config_external_zsh_string() {
        let toml = r#"
[completion]
external = "zsh"
"#;
        let config = load_from_str(toml);
        assert_eq!(config.completion.external, "zsh");
    }

    #[test]
    fn parse_completion_config_external_array_form_explicit_order() {
        let toml = r#"
[completion]
external = ["zsh", "carapace"]
"#;
        let config = load_from_str(toml);
        assert_eq!(
            config.completion.external,
            ExternalSetting::List(vec!["zsh".to_string(), "carapace".to_string()])
        );
    }

    #[test]
    fn parse_completion_config_external_array_form_single_entry() {
        let toml = r#"
[completion]
external = ["carapace"]
"#;
        let config = load_from_str(toml);
        assert_eq!(
            config.completion.external,
            ExternalSetting::List(vec!["carapace".to_string()])
        );
    }

    #[test]
    fn parse_completion_config_external_array_form_with_invalid_entry() {
        // 不正な要素を含む配列でも TOML パース自体は成功する（要素単位の
        // 妥当性検査・警告は resolve() の責務、raw_entries() 経由）。
        let toml = r#"
[completion]
external = ["zsh", "bogus", "carapace"]
"#;
        let config = load_from_str(toml);
        assert_eq!(
            config.completion.external,
            ExternalSetting::List(vec![
                "zsh".to_string(),
                "bogus".to_string(),
                "carapace".to_string()
            ])
        );
    }

    #[test]
    fn external_setting_default_is_single_auto_string_form() {
        // 既存の文字列形式との後方互換の起点: デフォルトは Single("auto")。
        assert_eq!(
            ExternalSetting::default(),
            ExternalSetting::Single("auto".to_string())
        );
        assert_eq!(ExternalSetting::default(), "auto");
    }

    #[test]
    fn external_setting_raw_entries_single_returns_one_element() {
        let setting = ExternalSetting::Single("carapace".to_string());
        assert_eq!(setting.raw_entries(), vec!["carapace"]);
    }

    #[test]
    fn external_setting_raw_entries_list_returns_all_elements_in_order() {
        let setting = ExternalSetting::List(vec!["zsh".to_string(), "carapace".to_string()]);
        assert_eq!(setting.raw_entries(), vec!["zsh", "carapace"]);
    }

    #[test]
    fn external_setting_display_single_matches_raw_string() {
        assert_eq!(
            ExternalSetting::Single("auto".to_string()).to_string(),
            "auto"
        );
    }

    #[test]
    fn external_setting_display_list_shows_bracketed_order() {
        let setting = ExternalSetting::List(vec!["zsh".to_string(), "carapace".to_string()]);
        assert_eq!(setting.to_string(), r#"["zsh", "carapace"]"#);
    }

    #[test]
    fn parse_completion_config_custom_external_timeout_ms() {
        let toml = r#"
[completion]
external_timeout_ms = 1500
"#;
        let config = load_from_str(toml);
        assert_eq!(config.completion.external_timeout_ms, 1500);
    }

    // ── external_zsh_daemon (Task 2b.3, #89) ──

    #[test]
    fn external_zsh_daemon_defaults_to_true() {
        assert!(CompletionConfig::default().external_zsh_daemon);
    }

    #[test]
    fn parse_completion_config_external_zsh_daemon_explicit_false() {
        let toml = r#"
[completion]
external_zsh_daemon = false
"#;
        let config = load_from_str(toml);
        assert!(!config.completion.external_zsh_daemon);
    }

    #[test]
    fn parse_completion_config_external_zsh_daemon_explicit_true() {
        let toml = r#"
[completion]
external_zsh_daemon = true
"#;
        let config = load_from_str(toml);
        assert!(config.completion.external_zsh_daemon);
    }

    #[test]
    fn parse_completion_config_without_external_zsh_daemon_key_defaults_true() {
        // 後方互換: 既存の config.toml（キー未記載）でもパースが失敗せず、
        // デフォルト値 true が使われる。
        let toml = r#"
[completion]
external_timeout_ms = 1500
"#;
        let config = load_from_str(toml);
        assert!(config.completion.external_zsh_daemon);
        assert_eq!(config.completion.external_timeout_ms, 1500);
    }

    #[test]
    fn parse_config_without_completion_section_at_all_defaults_zsh_daemon_true() {
        // completion セクション自体が存在しない設定ファイル（旧バージョン）
        // でもパースが失敗しないことの確認。
        let config = load_from_str("");
        assert!(config.completion.external_zsh_daemon);
    }

    // ── startup ──

    #[test]
    fn parse_startup_commands() {
        let toml = r#"
[startup]
commands = ["echo hello", "cd /tmp"]
"#;
        let config = load_from_str(toml);
        assert_eq!(config.startup.commands.len(), 2);
        assert_eq!(config.startup.commands[0], "echo hello");
        assert_eq!(config.startup.commands[1], "cd /tmp");
    }

    #[test]
    fn parse_startup_commands_empty() {
        let toml = r#"
[startup]
commands = []
"#;
        let config = load_from_str(toml);
        assert!(config.startup.commands.is_empty());
    }

    #[test]
    fn parse_no_startup_section_uses_default() {
        let config = load_from_str("");
        assert!(config.startup.commands.is_empty());
    }

    #[test]
    fn default_config_has_empty_startup_commands() {
        let config = JarvishConfig::default();
        assert!(config.startup.commands.is_empty());
    }

    #[test]
    fn default_template_contains_startup_section() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        JarvishConfig::create_default_config(&path);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[startup]"));
    }
}
