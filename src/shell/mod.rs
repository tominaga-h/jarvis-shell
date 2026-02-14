//! Shell モジュール — REPL ループとシェル状態管理
//!
//! `Shell` 構造体にすべてのシェル状態を集約し、
//! 入力ハンドリング、AI ルーティング、エラー調査の各責務をサブモジュールに分離する。

mod ai_router;
mod editor;
mod input;
mod investigate;

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use reedline::{Reedline, Signal};
use tracing::{info, warn};

use crate::ai::{ConversationState, JarvisAI};
use crate::cli::prompt::{JarvisPrompt, EXIT_CODE_NONE};
use crate::engine::classifier::InputClassifier;
use crate::storage::BlackBox;

/// Jarvis Shell の状態を管理する構造体。
/// エディタ、AI クライアント、履歴ストレージ、会話状態を保持する。
pub struct Shell {
    editor: Reedline,
    prompt: JarvisPrompt,
    ai_client: Option<JarvisAI>,
    black_box: Option<BlackBox>,
    conversation_state: Option<ConversationState>,
    last_exit_code: Arc<AtomicI32>,
    classifier: Arc<InputClassifier>,
    /// Farewell メッセージが既に表示済みかどうか（AI goodbye 等で表示済みの場合 true）
    farewell_shown: bool,
}

impl Shell {
    /// 新しい Shell インスタンスを作成する。
    ///
    /// 入力分類器、エディタ、プロンプト、BlackBox、AI クライアントを初期化する。
    pub fn new() -> Self {
        // 入力分類器の初期化（PATH キャッシュを構築）
        // ハイライターと REPL ループの両方で共有するため Arc で包む
        let classifier = Arc::new(InputClassifier::new());

        // 履歴 DB のパスを決定（BlackBox と同じ history.db を共有）
        let db_path = BlackBox::data_dir()
            .map(|dir| dir.join("history.db"))
            .unwrap_or_else(|_| {
                warn!("Failed to determine data directory for history, using fallback");
                std::path::PathBuf::from(".jarvish_history.db")
            });

        let reedline = editor::build_editor(Arc::clone(&classifier), db_path);

        // 直前コマンドの終了コードを共有するアトミック変数
        // 初期値は EXIT_CODE_NONE（未設定）。コマンド実行時に実際の終了コードで上書きされる。
        let last_exit_code = Arc::new(AtomicI32::new(EXIT_CODE_NONE));

        let prompt = JarvisPrompt::new(Arc::clone(&last_exit_code));

        // Black Box（履歴永続化）の初期化
        let black_box = match BlackBox::open() {
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

        // AI クライアントの初期化
        let ai_client = match JarvisAI::new() {
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
            classifier,
            farewell_shown: false,
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
        crate::cli::banner::print_welcome();

        let mut repl_error = false;

        loop {
            match self.editor.read_line(&self.prompt) {
                Ok(Signal::Success(line)) => {
                    if !self.handle_input(&line).await {
                        break;
                    }
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
