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
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use reedline::{Reedline, Signal};
use tracing::{info, warn};

use std::sync::atomic::AtomicBool as StaticAtomicBool;

/// SIGUSR1 シグナルハンドラが設定するグローバルフラグ。
/// シグナルハンドラ内では async-signal-safe な操作のみ許可されるため、
/// `AtomicBool::store` を使用する。
static RESTART_FLAG: StaticAtomicBool = StaticAtomicBool::new(false);

use crate::ai::{ConversationState, JarvisAI};
use crate::cli::prompt::starship::CMD_DURATION_NONE;
use crate::cli::prompt::{ShellPrompt, EXIT_CODE_NONE};
use crate::config::JarvishConfig;
use crate::engine::classifier::InputClassifier;
use crate::engine::expand;
use crate::engine::LoopAction;
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
    /// SIGUSR1 受信時に再起動をリクエストするフラグ。
    /// コマンド実行中・PTY 使用中は即座に再起動せず、次の REPL idle 時に遅延実行する。
    restart_requested: Arc<AtomicBool>,
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
            restart_requested: Arc::new(AtomicBool::new(false)),
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
    /// SIGUSR1 受信時は再起動を行う。
    ///
    /// 戻り値: `(終了コード, LoopAction)` のタプル。
    /// - `LoopAction::Exit`: 通常終了
    /// - `LoopAction::Restart`: exec() による再起動が必要
    pub async fn run(&mut self) -> (i32, LoopAction) {
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

        // バックグラウンドでバージョンチェックを実行（24時間キャッシュ付き）
        let update_check = tokio::spawn(crate::cli::update_check::check_for_update_notification());

        let mut repl_error = false;
        let mut action = LoopAction::Exit;

        // SIGUSR1 ハンドラの登録（AtomicBool フラグを共有）
        Self::register_sigusr1_handler(Arc::clone(&self.restart_requested));

        // 最初のプロンプト表示前にバージョンチェック結果を表示（最大1秒待機）
        if let Ok(Ok(Some(notification))) =
            tokio::time::timeout(std::time::Duration::from_secs(1), update_check).await
        {
            println!("{notification}");
            println!();
        }

        loop {
            // 別プロセスの update コマンドによるフラグファイルを検出し、通知を表示
            if let Some(notification) = crate::engine::builtins::update::check_update_flag() {
                println!("  {notification}");
                println!();
            }

            // SIGUSR1 による再起動リクエストがフラグに残っている場合（コマンド実行中に受信した場合）
            if self.restart_requested.load(Ordering::Relaxed) {
                info!("Deferred restart triggered (SIGUSR1 received during command execution)");
                println!("Restarting jarvish (deferred SIGUSR1)...");
                action = LoopAction::Restart;
                break;
            }

            let signal = tokio::task::block_in_place(|| self.editor.read_line(&self.prompt));

            // read_line の完了後にシグナルフラグをチェック
            if self.restart_requested.load(Ordering::Relaxed) {
                info!("SIGUSR1 received during read_line: restarting shell");
                println!("\nRestarting jarvish (SIGUSR1)...");
                action = LoopAction::Restart;
                break;
            }

            match signal {
                Ok(Signal::Success(line)) => {
                    let result = self.handle_input(&line).await;
                    if !result {
                        // handle_input が false を返した場合、restart か exit かを判別
                        // restart ビルトインが呼ばれた場合は last action を確認
                        if self.restart_requested.load(Ordering::Relaxed) {
                            action = LoopAction::Restart;
                        }
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

        // Farewell メッセージ表示（再起動時と AI goodbye 表示済みの場合はスキップ）
        if action != LoopAction::Restart && !self.farewell_shown {
            crate::cli::banner::print_goodbye();
        }

        // セッション終了: session_id を NULL に解放し、次回起動時に履歴を辿れるようにする
        if let Some(ref bb) = self.black_box {
            bb.release_session();
        }

        // 終了コードを決定
        let exit_code = if repl_error {
            1
        } else {
            let code = self.last_exit_code.load(Ordering::Relaxed);
            if code == EXIT_CODE_NONE {
                0
            } else {
                code
            }
        };

        (exit_code, action)
    }

    /// SIGUSR1 シグナルハンドラを登録する。
    ///
    /// 受信時に `RESTART_FLAG` グローバルフラグを立てる。
    /// reedline の `read_line()` は同期ブロッキング呼び出しのため、
    /// シグナルハンドラでフラグを立て、次の REPL ループイテレーションでチェックする。
    fn register_sigusr1_handler(restart_flag: Arc<AtomicBool>) {
        extern "C" fn handle_sigusr1(_: libc::c_int) {
            // シグナルハンドラ内では async-signal-safe な操作のみ許可
            RESTART_FLAG.store(true, Ordering::Relaxed);
        }

        // グローバルフラグをリセット
        RESTART_FLAG.store(false, Ordering::Relaxed);

        // グローバル RESTART_FLAG を Shell の restart_requested に転送するスレッド
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if RESTART_FLAG.load(Ordering::Relaxed) {
                restart_flag.store(true, Ordering::Relaxed);
                break;
            }
        });

        // libc の sigaction で SIGUSR1 ハンドラを登録
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handle_sigusr1 as *const () as usize;
            sa.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut sa.sa_mask);

            if libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut()) == 0 {
                info!("SIGUSR1 handler registered for self-restart");
            } else {
                let e = std::io::Error::last_os_error();
                warn!(error = %e, "Failed to register SIGUSR1 handler");
                eprintln!("jarvish: warning: SIGUSR1 handler unavailable: {e}");
            }
        }
    }

    /// exec() によるプロセス再起動を実行する。
    ///
    /// クリーンアップ後、現在のバイナリで exec() を呼び出しプロセスを置換する。
    /// 成功時はこの関数から戻らない。失敗時はエラーを返す。
    pub fn exec_restart(&mut self) -> std::io::Error {
        use std::os::unix::process::CommandExt;

        // stdout/stderr をフラッシュ
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let _ = std::io::Write::flush(&mut std::io::stderr());

        info!("exec_restart: executing self-restart");

        let (exe, args) = match build_restart_command() {
            Ok(pair) => pair,
            Err(e) => return e,
        };

        // exec() — 成功時はこの行に到達しない
        std::process::Command::new(exe).args(&args).exec()
    }
}

/// exec_restart 用のコマンド情報を構築する。
///
/// 現在のバイナリパスと引数を取得する。テスト可能な純粋関数として分離。
fn build_restart_command() -> Result<(PathBuf, Vec<String>), std::io::Error> {
    let exe = std::env::current_exe().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("failed to get current exe path: {e}"),
        )
    })?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    Ok((exe, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── build_restart_command ──

    #[test]
    fn build_restart_command_returns_valid_exe() {
        let result = build_restart_command();
        assert!(result.is_ok());
        let (exe, _args) = result.unwrap();
        assert!(exe.exists(), "current_exe path should exist");
    }

    #[test]
    fn build_restart_command_args_exclude_binary_name() {
        let (_, args) = build_restart_command().unwrap();
        // テストバイナリのパスが引数に含まれないことを確認
        for arg in &args {
            assert!(
                !arg.contains("jarvish-") && !arg.ends_with("jarvish"),
                "args should not contain binary name, got: {arg}"
            );
        }
    }

    // ── RESTART_FLAG (global AtomicBool) ──

    #[test]
    fn restart_flag_initial_state_is_false() {
        // テスト間の副作用を避けるためリセット
        RESTART_FLAG.store(false, Ordering::Relaxed);
        assert!(!RESTART_FLAG.load(Ordering::Relaxed));
    }

    #[test]
    fn restart_flag_can_be_set_and_read() {
        RESTART_FLAG.store(true, Ordering::Relaxed);
        assert!(RESTART_FLAG.load(Ordering::Relaxed));
        // クリーンアップ
        RESTART_FLAG.store(false, Ordering::Relaxed);
    }

    // ── register_sigusr1_handler + flag propagation ──

    #[test]
    fn sigusr1_handler_propagates_to_restart_flag() {
        let restart_flag = Arc::new(AtomicBool::new(false));

        // ハンドラを登録
        Shell::register_sigusr1_handler(Arc::clone(&restart_flag));

        // 自プロセスに SIGUSR1 を送信
        unsafe {
            libc::kill(libc::getpid(), libc::SIGUSR1);
        }

        // フラグが伝播するまで待機（最大2秒）
        for _ in 0..40 {
            if restart_flag.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        assert!(
            restart_flag.load(Ordering::Relaxed),
            "SIGUSR1 should propagate to restart_flag via polling thread"
        );

        // グローバルフラグをリセット
        RESTART_FLAG.store(false, Ordering::Relaxed);
    }

    // ── restart_requested flag monitoring ──

    #[test]
    fn restart_requested_flag_default_is_false() {
        let flag = Arc::new(AtomicBool::new(false));
        assert!(!flag.load(Ordering::Relaxed));
    }

    #[test]
    fn restart_requested_flag_set_triggers_restart() {
        let flag = Arc::new(AtomicBool::new(false));
        flag.store(true, Ordering::Relaxed);
        // REPL ループと同じチェックロジック
        assert!(flag.load(Ordering::Relaxed));
    }

    // ── update flag file notification in REPL ──

    #[test]
    fn check_update_flag_returns_none_without_flag_file() {
        use crate::engine::builtins::update;
        // 念のため既存フラグを削除
        let _ = update::check_update_flag();
        assert!(update::check_update_flag().is_none());
    }

    #[test]
    fn check_update_flag_returns_notification_with_flag_file() {
        use crate::engine::builtins::update;
        // 念のため既存フラグを削除
        let _ = update::check_update_flag();

        update::write_update_flag_for_test("2.0.0");
        let msg = update::check_update_flag();
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("v2.0.0"));
        // 読み取り後は削除されている
        assert!(update::check_update_flag().is_none());
    }
}
