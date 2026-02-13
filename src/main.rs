mod ai;
mod cli;
mod engine;
mod logging;
mod storage;

use ai::client::{AiResponse, JarvisAI};
use cli::completer::JarvishCompleter;
use cli::highlighter::JarvisHighlighter;
use cli::jarvis::jarvis_command_notice;
use cli::prompt::JarvisPrompt;
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
    let prompt = JarvisPrompt::new();

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

                            // BlackBox から直近 5 件のコマンド履歴をコンテキストとして取得
                            let context = black_box
                                .as_ref()
                                .and_then(|bb| bb.get_recent_context(5).ok())
                                .unwrap_or_default();

                            debug!(context_length = context.len(), "Context retrieved for AI");

                            match ai.process_input(&line, &context).await {
                                Ok(AiResponse::Command(ref cmd)) => {
                                    debug!(
                                        ai_response = "Command",
                                        command = %cmd,
                                        "AI interpreted natural language as a command"
                                    );
                                    // AI が自然言語からコマンドを解釈 → 実行前にアナウンス
                                    jarvis_command_notice(cmd);
                                    let mut result = execute(cmd);
                                    // AI が実行したコマンドをコンテキストとして stdout に記録
                                    // 次回の AI 呼び出しで何が実行されたか把握できるようにする
                                    if result.stdout.is_empty() {
                                        result.stdout =
                                            format!("[Jarvis executed: {cmd}]");
                                    } else {
                                        result.stdout =
                                            format!("[Jarvis executed: {cmd}]\n{}", result.stdout);
                                    }
                                    result
                                }
                                Ok(AiResponse::NaturalLanguage(ref text)) => {
                                    debug!(
                                        ai_response = "NaturalLanguage",
                                        response_length = text.len(),
                                        "AI responded with natural language"
                                    );
                                    // AI が自然言語で応答 → ストリーミング表示済み
                                    CommandResult::success(text.clone())
                                }
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
                        } else {
                            debug!(ai_enabled = false, "AI disabled, executing directly");
                            // AI 無効時は従来通り実行
                            execute(&line)
                        }
                    }
                };

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

                info!("\n==== FINISHED PROCESS ====\n\n");
            }
            Ok(Signal::CtrlC) => {
                // 現在の行をクリアして続行
            }
            Ok(Signal::CtrlD) => {
                // EOF → シェル終了
                info!("Ctrl-D received, exiting");
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
