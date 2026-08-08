//! 設定ファイルの読み込み。

use std::path::PathBuf;

use tracing::{debug, info, warn};

use super::JarvishConfig;

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
