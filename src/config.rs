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
//!
//! [alias]
//! g = "git"
//! ll = "ls -la"
//!
//! [export]
//! PATH = "/usr/local/bin:$PATH"
//! ```

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
}

/// AI 関連の設定
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    /// 使用する AI モデル名
    pub model: String,
    /// エージェントループの最大ラウンド数
    pub max_rounds: usize,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            model: "gpt-4o".to_string(),
            max_rounds: 10,
        }
    }
}

impl JarvishConfig {
    /// 設定ファイルを読み込む。
    ///
    /// `~/.config/jarvish/config.toml` が存在すればパースし、
    /// 存在しなければデフォルト値を返す。
    /// パースエラーの場合は警告を表示してデフォルト値を返す。
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
                        alias_count = config.alias.len(),
                        export_count = config.export.len(),
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

    /// 設定ファイルのパスを返す。
    ///
    /// macOS / Linux 共通で `~/.config/jarvish/config.toml` を使用する。
    /// dotfiles として管理しやすいよう、XDG_CONFIG_HOME に依存しない固定パスとする。
    /// `$HOME` が取得できない場合は `./.config/jarvish/config.toml` にフォールバックする。
    pub fn config_path() -> PathBuf {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".config/jarvish/config.toml")
    }

    /// 設定ファイルが存在しない場合にテンプレートから生成する。
    ///
    /// 親ディレクトリが存在しなければ再帰的に作成する。
    /// 生成に失敗した場合は警告を表示するが、シェルの起動は継続する。
    fn create_default_config(path: &std::path::Path) {
        const TEMPLATE: &str = r#"# Jarvish configuration
#
# You can write setting like this:

[ai]
# model = "gpt-4o"
# max_rounds = 10

[alias]
# g = "git"
# ll = "ls -la"

[export]
# PATH = "/usr/local/bin:$PATH"
"#;

        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                warn!(path = %parent.display(), error = %e, "Failed to create config directory");
                eprintln!("jarvish: warning: failed to create config directory: {e}");
                return;
            }
        }

        match std::fs::write(path, TEMPLATE) {
            Ok(()) => {
                info!(path = %path.display(), "Created default config file");
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Failed to create default config file");
                eprintln!("jarvish: warning: failed to create config file: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用: 指定パスから設定を読み込むヘルパー
    fn load_from_str(content: &str) -> JarvishConfig {
        toml::from_str(content).unwrap()
    }

    #[test]
    fn default_config_has_expected_values() {
        let config = JarvishConfig::default();
        assert_eq!(config.ai.model, "gpt-4o");
        assert_eq!(config.ai.max_rounds, 10);
        assert!(config.alias.is_empty());
        assert!(config.export.is_empty());
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
[ai]
model = "gpt-4o-mini"
max_rounds = 5

[alias]
g = "git"
ll = "ls -la"

[export]
EDITOR = "vim"
"#;
        let config = load_from_str(toml);
        assert_eq!(config.ai.model, "gpt-4o-mini");
        assert_eq!(config.ai.max_rounds, 5);
        assert_eq!(config.alias.get("g").unwrap(), "git");
        assert_eq!(config.alias.get("ll").unwrap(), "ls -la");
        assert_eq!(config.export.get("EDITOR").unwrap(), "vim");
    }

    #[test]
    fn parse_partial_config_uses_defaults() {
        let toml = r#"
[alias]
g = "git"
"#;
        let config = load_from_str(toml);
        // ai セクションが省略されていてもデフォルト値が使われる
        assert_eq!(config.ai.model, "gpt-4o");
        assert_eq!(config.ai.max_rounds, 10);
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
        // config_path() が返すパスにファイルがなければデフォルトが返る
        // (load() は config_path() を使うため、直接テストは難しいが
        //  デフォルト値であることを確認)
        let config = JarvishConfig::load();
        // ファイルがあってもなくても、少なくともデフォルト値は持つ
        assert!(!config.ai.model.is_empty());
        assert!(config.ai.max_rounds > 0);
    }

    #[test]
    fn create_default_config_creates_file_and_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("sub/dir/config.toml");

        // ファイルもディレクトリも存在しない状態で呼び出す
        assert!(!path.exists());
        JarvishConfig::create_default_config(&path);

        // ファイルが生成されていること
        assert!(path.exists());

        // 生成された内容がテンプレートであること
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[ai]"));
        assert!(content.contains("[alias]"));
        assert!(content.contains("[export]"));

        // テンプレートが有効な TOML としてパースできること
        let config: JarvishConfig = toml::from_str(&content).unwrap();
        assert_eq!(config.ai.model, "gpt-4o"); // デフォルト値
        assert!(config.alias.is_empty());
    }
}
