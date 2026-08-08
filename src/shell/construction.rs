//! `Shell` の構築・初期化ロジックを切り出したサブモジュール。
//!
//! `Shell::new` およびそこから呼ばれる private ヘルパー
//! （`apply_exports` / `build_prompt` / `detect_starship`）を集約する。
//! 振る舞いは元の `src/shell/mod.rs` から一切変更していない。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64};
use std::sync::{Arc, RwLock};

use tracing::{info, warn};

use crate::ai::JarvisAI;
use crate::cli::completer::{
    new_shared_daemon_slot, registry::CompletionRegistry, DaemonGate, ExternalCompletionSettings,
};
use crate::cli::prompt::starship::CMD_DURATION_NONE;
use crate::cli::prompt::{ShellPrompt, EXIT_CODE_NONE};
use crate::config::JarvishConfig;
use crate::engine::classifier::InputClassifier;
use crate::storage::BlackBox;

use super::prewarm::spawn_prewarm_thread_if_interactive;
use super::rc::RcOptions;
use super::Shell;

impl Shell {
    /// 新しい Shell インスタンスを作成する。
    ///
    /// 設定ファイル、入力分類器、エディタ、プロンプト、BlackBox、AI クライアントを初期化する。
    ///
    /// `interactive` は `main.rs` が `args.command.is_none()`（`-c` 未指定）
    /// かどうかから決める。`false`（`-c` 単体実行）の場合、Tab 補完が
    /// 一切発生しないウォーム zsh 補完デーモンの事前 spawn は純粋な無駄な
    /// うえ、起動〜終了が数ミリ秒で完走することが多くレース
    /// （孤児 `/bin/zsh -i`）を踏みやすいため、prewarm 自体を丸ごとスキップ
    /// する（tombstone ゲートと合わせた二段構えの対策の1つ目）。
    pub fn new(
        logging_operational: bool,
        session_id: i64,
        rc_options: RcOptions,
        interactive: bool,
    ) -> Self {
        // 設定ファイルの読み込み
        let config = JarvishConfig::load();

        // [export] セクションの環境変数を設定
        Self::apply_exports(&config);

        // 入力分類器の初期化（キャッシュレス設計: which クレートでリアルタイム PATH 解決）
        // ハイライターと REPL ループの両方で共有するため Arc で包む
        let classifier = Arc::new(InputClassifier::new());

        // データディレクトリを一度だけ決定し、エディタ履歴と BlackBox の両方で共有する。
        let data_dir = BlackBox::data_dir();

        let git_branch_commands =
            Arc::new(RwLock::new(config.completion.git_branch_commands.clone()));

        // エイリアスは JarvishCompleter と共有するため editor 構築前に確保する
        let aliases = Arc::new(RwLock::new(config.alias.clone()));

        // 外部補完（carapace）の設定を解決する（`which` によるバイナリ検出込み）。
        // JarvishCompleter と共有するため editor 構築前に確保する。
        let external_completion = Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &config.completion,
        )));

        // 温存 zsh 補完デーモンのスロット。`ZshBridgeProvider` と共有し、
        // `Shell` 側からライフサイクルイベント（reload/exit/restart）で
        // 直接 shutdown できるようにする。
        let zsh_daemon = new_shared_daemon_slot();
        // 終端 shutdown の tombstone ゲート。prewarm スレッドと
        // `shutdown_zsh_daemon` の両方に配る（`DaemonGate` のドキュメント
        // 参照）。
        let zsh_daemon_gate = DaemonGate::new();

        // 起動時のバックグラウンド事前ウォームアップ。設定でデーモン
        // が有効（フラグ on + zsh が enabled-kinds に含まれる + zsh バイナリ
        // 検出済み）なら、デタッチしたバックグラウンドスレッドから spawn を
        // 開始し、ユーザーの最初の Tab 押下までに温存デーモンが生きている
        // 状態を狙う（起動そのものはブロックしない）。無効なら
        // `prewarm_zsh_daemon` 内部で即座に no-op として戻る。`provide()`
        // とのレースは `prewarm_zsh_daemon` 側の Mutex 二重チェックで防止
        // 済み（同モジュールのドキュメント参照）。
        //
        // `interactive == false`（`-c` 単体実行）では Tab 補完が
        // 一切発生しないため、prewarm スレッド自体を起動しない（spawn は
        // 純粋な無駄なうえ、起動直後に完走するプロセスでは孤児化レースを
        // 踏みやすい）。判定ロジックは `spawn_prewarm_thread_if_interactive`
        // に切り出し、`Shell` 全体を構築せずに単体テストできるようにする。
        //
        // 戻り値の `Receiver` は `zsh_daemon_prewarm_done` に保持し、
        // `shutdown_zsh_daemon` が「prewarm スレッドの完了」を有界時間で
        // 待つのに使う（`zsh_daemon_prewarm_done` フィールドのドキュメント
        // 参照）。
        let zsh_daemon_prewarm_done = spawn_prewarm_thread_if_interactive(
            interactive,
            &external_completion,
            &zsh_daemon,
            &zsh_daemon_gate,
        );

        // `complete` ビルトインで登録されるユーザー定義補完。
        // JarvishCompleter と共有するため editor 構築前に確保する。
        let complete_registry = Arc::new(RwLock::new(CompletionRegistry::new()));

        let db_path = data_dir.join("history.db");
        let (reedline, history_available) = super::editor::build_editor(
            Arc::clone(&classifier),
            db_path,
            session_id,
            Arc::clone(&git_branch_commands),
            Arc::clone(&aliases),
            Arc::clone(&external_completion),
            Arc::clone(&zsh_daemon),
            Arc::clone(&complete_registry),
        );

        // 直前コマンドの終了コードを共有するアトミック変数
        // 初期値は EXIT_CODE_NONE（未設定）。コマンド実行時に実際の終了コードで上書きされる。
        let last_exit_code = Arc::new(AtomicI32::new(EXIT_CODE_NONE));
        let cmd_duration_ms = Arc::new(AtomicU64::new(CMD_DURATION_NONE));

        let prompt = Self::build_prompt(
            &config,
            Arc::clone(&last_exit_code),
            Arc::clone(&cmd_duration_ms),
        );
        prompt.refresh_git_status();

        // Black Box（履歴永続化）の初期化
        // BlackBox::open() ではなく open_at() を使い、フォールバック時も同じパスを使用する
        let black_box = match BlackBox::open_at(data_dir, session_id) {
            Ok(bb) => {
                info!("BlackBox initialized successfully");
                Some(bb)
            }
            Err(e) => {
                warn!("Failed to initialize BlackBox: {e}");
                eprintln!("jarvish: warning: failed to initialize black box: {e}");
                None
            }
        };

        // AI クライアントの初期化（設定ファイルの [ai] セクションを反映）
        let ai_client = match JarvisAI::new(&config.ai) {
            Ok(ai) => {
                info!("AI client initialized successfully");
                Some(ai)
            }
            Err(e) => {
                warn!("AI disabled: {e}");
                eprintln!("jarvish: warning: AI disabled: {e}");
                None // API キー未設定時は AI 機能を無効化
            }
        };

        Self {
            editor: reedline,
            prompt,
            ai_client,
            black_box,
            conversation_state: None,
            last_exit_code,
            cmd_duration_ms,
            classifier,
            aliases,
            ignore_auto_investigation_cmds: config.ai.ignore_auto_investigation_cmds,
            dir_stack: Vec::new(),
            farewell_shown: false,
            history_available,
            logging_operational,
            git_branch_commands,
            external_completion,
            zsh_daemon,
            zsh_daemon_gate,
            zsh_daemon_prewarm_done,
            complete_registry,
            restart_requested: Arc::new(AtomicBool::new(false)),
            startup_commands: config.startup.commands,
            rc_options,
            source_depth: 0,
            interactive,
        }
    }

    /// 設定ファイルの `[export]` セクションを環境変数に適用する。
    ///
    /// 値に含まれる環境変数参照（`$PATH` 等）は展開してから設定する。
    pub(super) fn apply_exports(config: &JarvishConfig) {
        for (key, value) in &config.export {
            let expanded = crate::engine::expand::expand_token(value);
            let display = format!("{key}={expanded}");
            let masked = if crate::storage::sanitizer::contains_secrets(&display) {
                crate::storage::sanitizer::mask_secrets(&display)
            } else {
                display
            };
            info!(masked = %masked, "Applying export from config");
            // SAFETY: シェル起動時のシングルスレッド初期化で呼ばれるため安全
            unsafe {
                std::env::set_var(key, &expanded);
            }
        }
    }

    /// 設定と環境に基づいてプロンプトを構築する。
    ///
    /// `[prompt] starship = true` かつ `starship` コマンドと設定ファイルが
    /// 存在する場合は Starship プロンプトを返し、それ以外はビルトインを返す。
    pub(super) fn build_prompt(
        config: &JarvishConfig,
        last_exit_code: Arc<AtomicI32>,
        cmd_duration_ms: Arc<AtomicU64>,
    ) -> ShellPrompt {
        if config.prompt.starship {
            if let Some(path) = Self::detect_starship() {
                info!(starship_path = %path.display(), "Starship prompt enabled");
                return ShellPrompt::starship(last_exit_code, cmd_duration_ms, path);
            }
            eprintln!(
                "jarvish: warning: starship = true but starship command or config not found, \
                 falling back to builtin prompt"
            );
        }
        ShellPrompt::builtin(last_exit_code, config.prompt.clone())
    }

    /// Starship の利用可否を検出する。
    ///
    /// 条件:
    /// 1. `starship` コマンドが PATH 上に存在する
    /// 2. `STARSHIP_CONFIG` 環境変数のパス、または `~/.config/starship.toml` が存在する
    ///
    /// 両方満たせば starship バイナリのパスを返す。
    pub(super) fn detect_starship() -> Option<PathBuf> {
        let starship_path = which::which("starship").ok()?;

        let config_path = std::env::var("STARSHIP_CONFIG")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(".config/starship.toml")
            });

        if config_path.exists() {
            Some(starship_path)
        } else {
            info!(
                config_path = %config_path.display(),
                "Starship config file not found"
            );
            None
        }
    }
}
