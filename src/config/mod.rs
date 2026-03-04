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
        }
    }
}

/// プロンプト表示の設定
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PromptConfig {
    /// NerdFont アイコンを使用するか（false の場合は ASCII/Unicode フォールバック）
    pub nerd_font: bool,
}

impl Default for PromptConfig {
    fn default() -> Self {
        Self { nerd_font: true }
    }
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
                        alias_count = config.alias.len(),
                        export_count = config.export.len(),
                        nerd_font = config.prompt.nerd_font,
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
        assert!(config.alias.is_empty());
        assert!(config.export.is_empty());
        assert!(config.prompt.nerd_font);
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
[ai]
model = "gpt-4o-mini"
max_rounds = 5
markdown_rendering = false

[alias]
g = "git"
ll = "ls -la"

[export]
EDITOR = "vim"

[prompt]
nerd_font = false
"#;
        let config = load_from_str(toml);
        assert_eq!(config.ai.model, "gpt-4o-mini");
        assert_eq!(config.ai.max_rounds, 5);
        assert!(!config.ai.markdown_rendering);
        assert_eq!(config.alias.get("g").unwrap(), "git");
        assert_eq!(config.alias.get("ll").unwrap(), "ls -la");
        assert_eq!(config.export.get("EDITOR").unwrap(), "vim");
        assert!(!config.prompt.nerd_font);
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

        let config: JarvishConfig = toml::from_str(&content).unwrap();
        assert_eq!(config.ai.model, "gpt-4o");
        assert!(config.alias.is_empty());
    }
}
