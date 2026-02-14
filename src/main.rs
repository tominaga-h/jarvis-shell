mod ai;
mod cli;
mod engine;
mod logging;
mod storage;

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;

use ai::client::{AiResponse, ConversationState, JarvisAI};
use cli::completer::JarvishCompleter;
use cli::highlighter::JarvisHighlighter;
use cli::jarvis::{jarvis_ask_investigate, jarvis_command_notice};
use cli::prompt::{JarvisPrompt, EXIT_CODE_NONE};
use engine::classifier::{InputClassifier, InputType};
use engine::{execute, try_builtin, CommandResult, LoopAction};
use reedline::{
    default_emacs_keybindings, ColumnarMenu, Emacs, KeyCode, KeyModifiers, MenuBuilder, Reedline,
    ReedlineEvent, ReedlineMenu, Signal,
};
use storage::BlackBox;
use tracing::{debug, info, warn};

#[tokio::main]
async fn main() {
    // .env ファイルから環境変数を読み込む
    dotenvy::dotenv().ok();

    // ログシステムの初期化（_guard は main 終了まで保持する必要がある）
    let _guard = logging::init_logging();
    info!("\n\n==== J.A.R.V.I.S.H. STARTED ====\n");

    // Tab 補完の設定
    let completer = Box::new(JarvishCompleter::new());
    let completion_menu = Box::new(ColumnarMenu::default().with_name("completion_menu"));

    let mut keybindings = default_emacs_keybindings();
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );

    let mut editor = Reedline::create()
        .with_highlighter(Box::new(JarvisHighlighter::default()))
        .with_completer(completer)
        .with_menu(ReedlineMenu::EngineCompleter(completion_menu))
        .with_edit_mode(Box::new(Emacs::new(keybindings)));

    // 直前コマンドの終了コードを共有するアトミック変数
    // 初期値は EXIT_CODE_NONE（未設定）。コマンド実行時に実際の終了コードで上書きされる。
    let last_exit_code = Arc::new(AtomicI32::new(EXIT_CODE_NONE));

    // 会話コンテキスト（AI との会話継続中）のインジケータ（プロンプト表示用）
    let has_conversation = Arc::new(AtomicBool::new(false));
    let mut conversation_state: Option<ConversationState> = None;

    let prompt = JarvisPrompt::new(Arc::clone(&last_exit_code), Arc::clone(&has_conversation));

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

    // 入力分類器の初期化（PATH キャッシュを構築）
    let classifier = InputClassifier::new();

    cli::banner::print_welcome();

    loop {
        match editor.read_line(&prompt) {
            Ok(Signal::Success(line)) => {

                info!("\n\n==== USER INPUT RECEIVED, START PROCESS ====");

                let line = line.trim().to_string();

                // === 統一入力フロー ===
                // 自然言語もコマンドも常に classifier を通る。
                // 会話コンテキストがある場合は自然言語入力時に continue_conversation で継続する。
                if line.is_empty() {
                    continue;
                }

                debug!(input = %line, "User input received");

                // 1. ビルトインコマンドをチェック（cd, cwd, exit は AI を介さず直接実行）
                if let Some(result) = try_builtin(&line) {
                    debug!(
                        command = %line,
                        exit_code = result.exit_code,
                        action = ?result.action,
                        "Builtin command executed"
                    );
                    // プロンプト表示用に終了コードを更新
                    last_exit_code.store(result.exit_code, Ordering::Relaxed);
                    println!(); // 実行結果の後に空行を追加

                    match result.action {
                        LoopAction::Continue => {
                            if let Some(ref bb) = black_box {
                                if let Err(e) = bb.record(&line, &result) {
                                    warn!("Failed to record builtin history: {e}");
                                    eprintln!(
                                        "jarvish: warning: failed to record history: {e}"
                                    );
                                }
                            }
                        }
                        LoopAction::Exit => {
                            info!("Exit command received");
                            break;
                        }
                    }
                    continue;
                }

                // 2. アルゴリズムで入力を分類（AI を呼ばず瞬時に判定）
                let input_type = classifier.classify(&line);
                debug!(input = %line, classification = ?input_type, "Input classified");

                // コマンドの出自を追跡（AI Tool Call かユーザー直接入力か）
                let mut from_tool_call = false;
                // AI の NaturalLanguage 応答時は last_exit_code を更新しない
                // （コマンド未実行なので終了コードは前回の値を維持する）
                let mut should_update_exit_code = true;

                let result = match input_type {
                    InputType::Command => {
                        // コマンド → AI を介さず直接実行
                        debug!(input = %line, "Executing as command (no AI)");
                        execute(&line)
                    }
                    InputType::NaturalLanguage => {
                        // 自然言語 → AI に送信
                        if let Some(ref ai) = ai_client {
                            debug!(ai_enabled = true, "Routing natural language to AI");

                            // 既存の会話コンテキストがある場合は継続、なければ新規会話
                            let existing_conv = conversation_state.take();

                            if let Some(mut conv) = existing_conv {
                                // === 会話継続 ===
                                debug!(input = %line, "Continuing existing conversation");

                                match ai.continue_conversation(&mut conv, &line).await {
                                    Ok(AiResponse::Command(ref cmd)) => {
                                        debug!(
                                            ai_response = "Command",
                                            command = %cmd,
                                            "AI continued conversation with a command"
                                        );
                                        from_tool_call = true;
                                        jarvis_command_notice(cmd);
                                        let mut result = execute(cmd);
                                        if result.stdout.is_empty() {
                                            result.stdout =
                                                format!("[Jarvis executed: {cmd}]");
                                        } else {
                                            result.stdout = format!(
                                                "[Jarvis executed: {cmd}]\n{}",
                                                result.stdout
                                            );
                                        }
                                        // 会話コンテキストを維持
                                        conversation_state = Some(conv);
                                        result
                                    }
                                    Ok(AiResponse::NaturalLanguage(ref text)) => {
                                        debug!(
                                            ai_response = "NaturalLanguage",
                                            response_length = text.len(),
                                            "AI continued conversation with natural language"
                                        );
                                        // 会話コンテキストを維持
                                        conversation_state = Some(conv);
                                        // コマンド未実行のため終了コードは更新しない
                                        should_update_exit_code = false;
                                        CommandResult::success(text.clone())
                                    }
                                    Err(e) => {
                                        warn!(
                                            error = %e,
                                            input = %line,
                                            "Conversation continuation failed, falling back to direct execution"
                                        );
                                        has_conversation.store(false, Ordering::Relaxed);
                                        // AI エラー時はコマンドとして直接実行にフォールバック
                                        execute(&line)
                                    }
                                }
                            } else {
                                // === 新規会話 ===
                                // BlackBox から直近 5 件のコマンド履歴をコンテキストとして取得
                                let context = black_box
                                    .as_ref()
                                    .and_then(|bb| bb.get_recent_context(5).ok())
                                    .unwrap_or_default();

                                debug!(context_length = context.len(), "Context retrieved for AI");

                                match ai.process_input(&line, &context).await {
                                    Ok(conv_result) => match conv_result.response {
                                        AiResponse::Command(ref cmd) => {
                                            debug!(
                                                ai_response = "Command",
                                                command = %cmd,
                                                "AI interpreted natural language as a command"
                                            );
                                            from_tool_call = true;
                                            // AI が自然言語からコマンドを解釈 → 実行前にアナウンス
                                            jarvis_command_notice(cmd);
                                            let mut result = execute(cmd);
                                            // AI が実行したコマンドをコンテキストとして stdout に記録
                                            if result.stdout.is_empty() {
                                                result.stdout =
                                                    format!("[Jarvis executed: {cmd}]");
                                            } else {
                                                result.stdout = format!(
                                                    "[Jarvis executed: {cmd}]\n{}",
                                                    result.stdout
                                                );
                                            }
                                            result
                                        }
                                        AiResponse::NaturalLanguage(ref text) => {
                                            debug!(
                                                ai_response = "NaturalLanguage",
                                                response_length = text.len(),
                                                "AI responded with natural language"
                                            );
                                            // 会話コンテキストを保存
                                            has_conversation.store(true, Ordering::Relaxed);
                                            conversation_state =
                                                Some(conv_result.conversation);
                                            // コマンド未実行のため終了コードは更新しない
                                            should_update_exit_code = false;
                                            // AI が自然言語で応答 → ストリーミング表示済み
                                            CommandResult::success(text.clone())
                                        }
                                    },
                                    Err(e) => {
                                        warn!(
                                            error = %e,
                                            input = %line,
                                            "AI processing failed, falling back to direct execution"
                                        );
                                        // AI エラー時はコマンドとして直接実行にフォールバック
                                        execute(&line)
                                    }
                                }
                            }
                        } else {
                            debug!(ai_enabled = false, "AI disabled, executing directly");
                            // AI 無効時は従来通り実行
                            execute(&line)
                        }
                    }
                };

                // プロンプト表示用に終了コードを更新
                // AI の NaturalLanguage 応答時はコマンド未実行のためスキップし、
                // 前回の終了コード（または初期値 EXIT_CODE_NONE）を維持する。
                if should_update_exit_code {
                    last_exit_code.store(result.exit_code, Ordering::Relaxed);
                }
                println!(); // 実行結果の後に空行を追加

                // 履歴を記録
                if result.action == LoopAction::Continue {
                    if let Some(ref bb) = black_box {
                        if let Err(e) = bb.record(&line, &result) {
                            warn!("Failed to record history: {e}");
                            eprintln!("jarvish: warning: failed to record history: {e}");
                        }
                    }
                }

                // === エラー調査フロー ===
                // コマンドが異常終了し、AI が利用可能な場合にエラー調査を実行する
                if result.exit_code != 0 {
                    if let Some(ref ai) = ai_client {
                        // 調査開始の判定:
                        // - Tool Call（AI 発信のコマンド）→ ユーザー確認なしで自動調査
                        // - ユーザー直接入力 → 確認プロンプト後に調査
                        let should_investigate = if from_tool_call {
                            info!("Tool Call command failed, auto-investigating");
                            true
                        } else {
                            jarvis_ask_investigate(result.exit_code)
                        };

                        if should_investigate {
                            // BlackBox から最新コンテキストを取得（失敗したコマンドも含む）
                            let context = black_box
                                .as_ref()
                                .and_then(|bb| bb.get_recent_context(5).ok())
                                .unwrap_or_default();

                            match ai.investigate_error(&line, &result, &context).await {
                                Ok(conv_result) => match conv_result.response {
                                    AiResponse::Command(ref fix_cmd) => {
                                        // AI が修正コマンドを提案 → 実行
                                        jarvis_command_notice(fix_cmd);
                                        let fix_result = execute(fix_cmd);
                                        last_exit_code
                                            .store(fix_result.exit_code, Ordering::Relaxed);
                                        println!();

                                        // 修正コマンドの結果も履歴に記録
                                        if fix_result.action == LoopAction::Continue {
                                            if let Some(ref bb) = black_box {
                                                if let Err(e) =
                                                    bb.record(fix_cmd, &fix_result)
                                                {
                                                    warn!(
                                                        "Failed to record fix command history: {e}"
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    AiResponse::NaturalLanguage(_) => {
                                        // 会話コンテキストを保存
                                        has_conversation.store(true, Ordering::Relaxed);
                                        conversation_state =
                                            Some(conv_result.conversation);
                                        // AI が自然言語で説明 → ストリーミング表示済み
                                        println!();
                                    }
                                },
                                Err(e) => {
                                    warn!(error = %e, "Error investigation failed");
                                }
                            }
                        }
                    }
                }

                info!("\n==== FINISHED PROCESS ====\n\n");
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
                break;
            }
        }
    }

    info!("\n\n==== J.A.R.V.I.S.H. SHUTTING DOWN ====\n\n");
    cli::banner::print_goodbye();
}
