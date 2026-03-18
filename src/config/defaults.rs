//! デフォルト設定ファイルの生成

use tracing::{info, warn};

impl super::JarvishConfig {
    /// 設定ファイルが存在しない場合にテンプレートから生成する。
    ///
    /// 親ディレクトリが存在しなければ再帰的に作成する。
    /// 生成に失敗した場合は警告を表示するが、シェルの起動は継続する。
    pub(super) fn create_default_config(path: &std::path::Path) {
        const TEMPLATE: &str = r#"# Jarvish configuration
#
# You can write setting like this:

[ai]
# model = "gpt-4o"
# max_rounds = 10
# markdown_rendering = true  # false にすると Markdown レンダリングを無効化
# ai_pipe_max_chars = 50000
# ai_redirect_max_chars = 50000
# temperature = 0.5          # 回答のランダム性 (0.0=決定的, 2.0=最大ランダム)
# ignore_auto_investigation_cmds = ["git log", "git diff"]  # 自動調査をスキップするコマンド

[alias]
# g = "git"
# ll = "ls -la"

[export]
# PATH = "/usr/local/bin:$PATH"

[prompt]
# nerd_font = true  # false にすると NerdFont アイコンを使わない

[completion]
# git_branch_commands = ["checkout", "switch", "merge", "rebase", "branch", "diff", "log", "cherry-pick", "reset", "push", "fetch"]
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
