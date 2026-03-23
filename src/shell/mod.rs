//! Shell モジュール — REPL ループとシェル状態管理
//!
//! `Shell` 構造体にすべてのシェル状態を集約し、
//! 入力ハンドリング、AI ルーティング、エラー調査の各責務をサブモジュールに分離する。

mod ai_router;
mod editor;
mod input;
mod investigate;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use reedline::{Reedline, Signal};
use tracing::{info, warn};

use crate::ai::{ConversationState, JarvisAI};
use crate::cli::prompt::starship::CMD_DURATION_NONE;
use crate::cli::prompt::{ShellPrompt, EXIT_CODE_NONE};
use crate::config::JarvishConfig;
use crate::engine::classifier::InputClassifier;
use crate::engine::expand;
use crate::storage::BlackBox;

/// Jarvis Shell の状態を管理する構造体。
/// エディタ、AI クライアント、履歴ストレージ、会話状態を保持する。
pub struct Shell {
    editor: Reedline,
    prompt: ShellPrompt,
    ai_client: Option<JarvisAI>,
    black_box: Option<BlackBox>,
    conversation_state: Option<ConversationState>,
    last_exit_code: Arc<AtomicI32>,
    /// 直前コマンドの実行時間（ミリ秒）。Starship プロンプトの `--cmd-duration` に使用。
    cmd_duration_ms: Arc<AtomicU64>,
    classifier: Arc<InputClassifier>,
    /// 設定ファイルで定義されたコマンドエイリアス
    aliases: HashMap<String, String>,
    /// 異常終了時に自動調査をスキップするコマンドの前方一致パターン
    ignore_auto_investigation_cmds: Vec<String>,
    /// pushd / popd / cd で管理されるディレクトリスタック
    dir_stack: Vec<PathBuf>,
    /// Farewell メッセージが既に表示済みかどうか（AI goodbye 等で表示済みの場合 true）
    farewell_shown: bool,
    /// コマンド履歴（reedline 矢印キー・ヒンター）が利用可能か
    history_available: bool,
    /// ロギングシステムがファイルに書き込み可能か
    logging_operational: bool,
    /// ブランチ名補完対象の git サブコマンド（JarvishCompleter と共有）
    git_branch_commands: Arc<RwLock<Vec<String>>>,
}

impl Shell {
    /// 新しい Shell インスタンスを作成する。
    ///
    /// 設定ファイル、入力分類器、エディタ、プロンプト、BlackBox、AI クライアントを初期化する。
    pub fn new(logging_operational: bool, session_id: i64) -> Self {
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

        let db_path = data_dir.join("history.db");
        let (reedline, history_available) = editor::build_editor(
            Arc::clone(&classifier),
            db_path,
            session_id,
            Arc::clone(&git_branch_commands),
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
            aliases: config.alias,
            ignore_auto_investigation_cmds: config.ai.ignore_auto_investigation_cmds,
            dir_stack: Vec::new(),
            farewell_shown: false,
            history_available,
            logging_operational,
            git_branch_commands,
        }
    }

    /// 設定ファイルの `[export]` セクションを環境変数に適用する。
    ///
    /// 値に含まれる環境変数参照（`$PATH` 等）は展開してから設定する。
    fn apply_exports(config: &JarvishConfig) {
        for (key, value) in &config.export {
            let expanded = expand::expand_token(value);
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
    fn build_prompt(
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
    fn detect_starship() -> Option<PathBuf> {
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

    /// 指定されたパスから設定ファイルを再読み込みし、Shell の状態に反映する。
    ///
    /// `source` ビルトインコマンドから呼び出される。
    /// `[alias]`、`[export]`、`[ai]` セクションを反映する。
    pub(super) fn reload_config(&mut self, path: &std::path::Path) -> crate::engine::CommandResult {
        use crate::engine::CommandResult;

        let config = match JarvishConfig::load_from(path) {
            Ok(c) => c,
            Err(msg) => {
                let err = format!("jarvish: source: {msg}\n");
                eprint!("{err}");
                return CommandResult::error(err, 1);
            }
        };

        // [alias] を反映
        self.aliases = config.alias.clone();

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

        // サマリー出力（config.toml のセクション順: ai, alias, export, prompt, completion）
        let ignore_cmds_display = if config.ai.ignore_auto_investigation_cmds.is_empty() {
            "none".to_string()
        } else {
            format!("{:?}", config.ai.ignore_auto_investigation_cmds)
        };
        let summary = format!(
            "Loaded {}\n\
             \x20 [ai]\n\
             \x20\x20 model: {}\n\
             \x20\x20 max_rounds: {}\n\
             \x20\x20 markdown_rendering: {}\n\
             \x20\x20 ai_pipe_max_chars: {}\n\
             \x20\x20 ai_redirect_max_chars: {}\n\
             \x20\x20 temperature: {}\n\
             \x20\x20 ignore_auto_investigation_cmds: {}\n\
             \x20 [alias]   {} {}\n\
             \x20 [export]  {} {}\n\
             \x20 [prompt]  nerd_font: {}, starship: {}\n\
             \x20 [completion]  git_branch_commands: {} {}\n",
            path.display(),
            config.ai.model,
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
        );
        print!("{summary}");

        CommandResult::success(summary)
    }

    /// `-c` オプションで渡されたコマンド文字列を非対話的に実行する。
    ///
    /// REPL ループには入らず、文字列を行ごとに `handle_input()` で処理して終了する。
    /// ウェルカムバナー・Farewell メッセージは表示しない。
    ///
    /// 戻り値: 最後に実行したコマンドの終了コード。
    pub async fn run_command(&mut self, command: &str) -> i32 {
        for line in command.lines() {
            if !self.handle_input(line).await {
                break;
            }
        }

        if let Some(ref bb) = self.black_box {
            bb.release_session();
        }

        let code = self.last_exit_code.load(Ordering::Relaxed);
        if code == EXIT_CODE_NONE {
            0
        } else {
            code
        }
    }

    /// REPL ループを実行する。
    ///
    /// ユーザー入力を受け取り、ビルトイン/コマンド/自然言語を処理する。
    /// Ctrl-D、exit コマンド、または goodbye 入力で終了する。
    ///
    /// 戻り値: シェルの終了コード。
    /// - 通常: 直前に実行したコマンドの終了コードを返す（bash/zsh と同じ挙動）
    /// - `exit N`: 引数で指定された終了コード
    /// - REPL 内部エラー: `1`
    pub async fn run(&mut self) -> i32 {
        let mut offline = Vec::new();
        if !self.logging_operational {
            offline.push("Logging offline");
        }
        if !self.history_available {
            offline.push("Command History offline");
        }
        if self.black_box.is_none() {
            offline.push("Black Box offline");
        }
        if self.ai_client.is_none() {
            offline.push("AI module offline");
        }
        crate::cli::banner::print_welcome(&offline);

        let mut repl_error = false;

        loop {
            let signal = tokio::task::block_in_place(|| self.editor.read_line(&self.prompt));
            match signal {
                Ok(Signal::Success(line)) => {
                    if !self.handle_input(&line).await {
                        break;
                    }
                    self.prompt.refresh_git_status();
                }
                Ok(Signal::CtrlC) => {
                    info!("\n!!!! Ctrl-C received: do it nothing !!!!!\n");
                    // なにもしない
                    println!(); // 改行して次のプロンプトを見やすくする
                }
                Ok(Signal::CtrlD) => {
                    // EOF → シェル終了
                    info!("\n!!!! Ctrl-D received: exiting shell !!!!!\n");
                    break;
                }
                Err(e) => {
                    warn!(error = %e, "REPL error, exiting");
                    eprintln!("jarvish: error: {e}");
                    repl_error = true;
                    break;
                }
            }
        }

        // Farewell メッセージ表示（AI goodbye で既に表示済みの場合はスキップ）
        if !self.farewell_shown {
            crate::cli::banner::print_goodbye();
        }

        // セッション終了: session_id を NULL に解放し、次回起動時に履歴を辿れるようにする
        if let Some(ref bb) = self.black_box {
            bb.release_session();
        }

        // 終了コードを決定
        if repl_error {
            1
        } else {
            let code = self.last_exit_code.load(Ordering::Relaxed);
            if code == EXIT_CODE_NONE {
                0
            } else {
                code
            }
        }
    }
}
