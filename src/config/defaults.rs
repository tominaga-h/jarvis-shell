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
#
# ⚠️ SHELL = "/usr/local/bin/jarvish" の設定に注意:
# 外部ツール（Cursor, VS Code 等）がサブシェルとして jarvish を使用するようになり、
# ツール呼び出しフックの失敗が AI 自動調査を大量発火させる可能性があります。
# 対話的シェルとしてのみ jarvish を使用する場合は SHELL を bash/zsh のままにしてください。

[prompt]
# nerd_font = true  # false にすると NerdFont アイコンを使わない
# starship = false   # true にすると Starship プロンプトを使用（要: starship コマンド + ~/.config/starship.toml）

[completion]
# git_branch_commands = ["checkout", "switch", "merge", "rebase", "branch", "diff", "log", "cherry-pick", "reset", "push", "fetch"]
# external = "auto"           # 外部補完（carapace / zsh ブリッジ）の使用方針。
#                              # 文字列で書く場合: "auto"（既定・各バイナリ検出時のみ使用、carapace→zsh の順）
#                              #                   | "carapace"（carapace のみ強制有効）
#                              #                   | "zsh"（zsh ブリッジのみ強制有効）
#                              #                   | "none"（外部補完を無効化）
#                              # 配列で書く場合: ["zsh", "carapace"] のように優先順を明示指定できる
# external_timeout_ms = 400   # 外部補完プロセス（carapace / zsh ブリッジ）のタイムアウト（ミリ秒）
# external_zsh_daemon = true  # zsh ブリッジを常駐デーモン化するか（既定 true）。
#                              # true: `zsh -i` を jarvish の子プロセスとして1本 spawn し、
#                              #       以後の Tab はそれを使い回す（起動コスト削減）。
#                              #       シェル起動直後にバックグラウンドで事前ウォームアップされる
#                              #       ため、通常は最初の Tab の時点で既にウォーム状態になっている。
#                              # false: 毎回 `zsh --no-rcs` を起動するワンショット方式に固定する。

[startup]
# シェル起動時に順次実行するコマンド（-c オプション実行時はスキップ）
# commands = ["echo 'Welcome to jarvish!'", "export JAVA_HOME=/usr/lib/jvm/default"]
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
