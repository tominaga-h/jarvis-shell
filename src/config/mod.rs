//! 設定ファイル管理
//!
//! `~/.config/jarvish/config.toml` から TOML 形式の設定を読み込む。
//! ファイルが存在しない場合はデフォルト値を使用する。
//!
//! # 設定ファイル例
//!
//! ```toml
//! [ai]
//! provider = "openai"
//! model = "gpt-4o"
//! max_tokens = 8192
//! base_url = "https://..."
//! api_key_env = "MY_API_KEY"
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
mod loading;
mod types;

#[cfg(test)]
mod tests;

pub use types::{
    default_max_tokens_for, AiConfig, CompletionConfig, ExternalSetting, JarvishConfig,
    PromptConfig, StartupConfig,
};
