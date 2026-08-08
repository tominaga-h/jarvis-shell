//! `source` ビルトインから呼ばれる設定再読み込みロジックを切り出した
//! サブモジュール。
//!
//! `Shell::reload_config`（`source` の本体）と、reload 経路で使う
//! free 関数 `reload_external_completion` / `apply_zsh_daemon_lifecycle_for_reload`
//! を集約する。振る舞いは元の `src/shell/mod.rs` から一切変更していない。

use std::sync::{Arc, RwLock};

use crate::cli::completer::{
    format_external_binaries_display, format_external_summary, shutdown_shared_daemon,
    ExternalCompletionSettings, SharedDaemonSlot,
};
use crate::config::{CompletionConfig, JarvishConfig};
use crate::engine::CommandResult;

use super::Shell;

impl Shell {
    /// 指定されたパスから設定ファイルを再読み込みし、Shell の状態に反映する。
    ///
    /// `source` ビルトインコマンドから呼び出される。
    /// `[ai]`、`[alias]`、`[export]`、`[prompt]`、`[completion]`、`[startup]`
    /// の各セクションを反映する（`[startup]` は値の更新のみで再実行はしない）。
    pub(super) fn reload_config(&mut self, path: &std::path::Path) -> CommandResult {
        let config = match JarvishConfig::load_from(path) {
            Ok(c) => c,
            Err(msg) => {
                let err = format!("jarvish: source: {msg}\n");
                eprint!("{err}");
                return CommandResult::error(err, 1);
            }
        };

        // [alias] を反映
        if let Ok(mut a) = self.aliases.write() {
            *a = config.alias.clone();
        }

        // [export] を反映
        Self::apply_exports(&config);

        // [ai] を反映
        if let Some(ref mut ai) = self.ai_client {
            ai.update_config(&config.ai);
        }
        self.ignore_auto_investigation_cmds = config.ai.ignore_auto_investigation_cmds.clone();

        // [prompt] を反映（starship フラグ変更時はプロンプト自体を入れ替え）
        self.prompt = Self::build_prompt(
            &config,
            Arc::clone(&self.last_exit_code),
            Arc::clone(&self.cmd_duration_ms),
        );
        self.prompt.refresh_git_status();

        // [completion] を反映
        if let Ok(mut cmds) = self.git_branch_commands.write() {
            *cmds = config.completion.git_branch_commands.clone();
        }
        // 外部補完（carapace / zsh ブリッジ）は which() の再検出込みで反映する。
        // これによりセッション中に carapace/zsh をインストールしてから
        // `source` するだけで再起動なしに有効化できる。
        let resolved_external =
            reload_external_completion(&self.external_completion, &config.completion);

        // 新しい設定の下で温存 zsh デーモンが稼働禁止（フラグ off、または
        // zsh が enabled-kinds リストから外れた）なら、`provide()` の次回
        // 呼び出しを待たず**その場**で shutdown する（A3/A4, #89 レビュー
        // 指摘 — README の「immediately shuts down」を実際に真にする）。
        apply_zsh_daemon_lifecycle_for_reload(&resolved_external, &self.zsh_daemon);

        // [startup] を反映（再実行はしない、値の更新のみ）
        self.startup_commands = config.startup.commands.clone();

        // サマリー出力（config.toml のセクション順: ai, alias, export, prompt, completion, startup）
        let ignore_cmds_display = if config.ai.ignore_auto_investigation_cmds.is_empty() {
            "none".to_string()
        } else {
            format!("{:?}", config.ai.ignore_auto_investigation_cmds)
        };
        let external_mode_display =
            format_external_summary(&config.completion.external.to_string(), &resolved_external);
        // 解決済みの優先順に沿って、各プロバイダのバイナリパス（未検出なら
        // "not found"）を1行ずつ列挙する。`enabled` が空（external = "none"
        // または全プロバイダ無効化）の場合は空行なし。
        let external_binaries_display = format_external_binaries_display(&resolved_external);
        let summary = format!(
            "Loaded {}\n\
             \x20 [ai]\n\
             \x20\x20 provider: {}\n\
             \x20\x20 model: {}\n\
             \x20\x20 max_tokens: {}\n\
             \x20\x20 max_rounds: {}\n\
             \x20\x20 markdown_rendering: {}\n\
             \x20\x20 ai_pipe_max_chars: {}\n\
             \x20\x20 ai_redirect_max_chars: {}\n\
             \x20\x20 temperature: {}\n\
             \x20\x20 ignore_auto_investigation_cmds: {}\n\
             \x20 [alias]   {} {}\n\
             \x20 [export]  {} {}\n\
             \x20 [prompt]  nerd_font: {}, starship: {}\n\
             \x20 [completion]  git_branch_commands: {} {}\n\
             \x20\x20 external: {}\n\
             {}\
             \x20\x20 external_timeout_ms: {}\n\
             \x20\x20 external_zsh_daemon: {}\n\
             \x20 [startup]  {} {}\n",
            path.display(),
            config.ai.provider.as_str(),
            config.ai.model,
            config
                .ai
                .max_tokens
                .unwrap_or_else(|| crate::config::default_max_tokens_for(&config.ai.provider)),
            config.ai.max_rounds,
            config.ai.markdown_rendering,
            config.ai.ai_pipe_max_chars,
            config.ai.ai_redirect_max_chars,
            config.ai.temperature,
            ignore_cmds_display,
            config.alias.len(),
            if config.alias.len() == 1 {
                "entry"
            } else {
                "entries"
            },
            config.export.len(),
            if config.export.len() == 1 {
                "entry"
            } else {
                "entries"
            },
            config.prompt.nerd_font,
            config.prompt.starship,
            config.completion.git_branch_commands.len(),
            if config.completion.git_branch_commands.len() == 1 {
                "command"
            } else {
                "commands"
            },
            external_mode_display,
            external_binaries_display,
            config.completion.external_timeout_ms,
            resolved_external.zsh_daemon_enabled,
            config.startup.commands.len(),
            if config.startup.commands.len() == 1 {
                "command"
            } else {
                "commands"
            },
        );
        print!("{summary}");

        CommandResult::success(summary)
    }
}

/// `[completion]` の外部補完設定を再解決し、共有 `Arc<RwLock<_>>` へ
/// 書き込む。`Shell::reload_config`（`source` ビルトイン）が呼び出す
/// 「resolve + 共有 Arc への書き込み」ステップを切り出したもの（D1, #89
/// レビュー指摘）。
///
/// `Shell` 全体を構築せずに `Arc<RwLock<ExternalCompletionSettings>>` と
/// `CompletionConfig` だけでテストできるようにする狙い。書き込み後の
/// 解決結果（reload 後の状態）をそのまま返すため、呼び出し側
/// （`reload_config`）はサマリー表示にこれを使い、`Arc` の中身と表示が
/// 常に同じ「reload 後」の値を参照していることを保証する。
pub(super) fn reload_external_completion(
    external_completion: &Arc<RwLock<ExternalCompletionSettings>>,
    completion_config: &CompletionConfig,
) -> ExternalCompletionSettings {
    let resolved = ExternalCompletionSettings::resolve(completion_config);
    if let Ok(mut ext) = external_completion.write() {
        *ext = resolved.clone();
    }
    resolved
}

/// `source` による reload 直後の温存 zsh デーモンのライフサイクル反映。
///
/// 新しく解決された `resolved`（reload 後の `ExternalCompletionSettings`）
/// の下でデーモンが稼働禁止（フラグ off、または zsh が enabled-kinds
/// リストから外れた）なら、`provide()` の次回呼び出しを待たず**その場**で
/// shutdown する（A3/A4, #89 レビュー指摘: README の「turning it off
/// immediately shuts down any running daemon」を実際に真にする）。
///
/// `reload_external_completion` と同じ理由（`Shell` 全体を構築せず
/// `Arc<RwLock<ExternalCompletionSettings>>` + `SharedDaemonSlot` だけで
/// テストできるようにする）で切り出した純粋寄りのヘルパー。デーモンが
/// 元々稼働していなければ [`shutdown_shared_daemon`] が no-op を保証する。
pub(super) fn apply_zsh_daemon_lifecycle_for_reload(
    resolved: &ExternalCompletionSettings,
    zsh_daemon: &SharedDaemonSlot,
) {
    if !resolved.should_run_zsh_daemon() {
        shutdown_shared_daemon(zsh_daemon);
    }
}
