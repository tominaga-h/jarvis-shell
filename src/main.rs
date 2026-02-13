mod ai;
mod cli;
mod engine;
mod logging;
mod storage;

use ai::client::{AiResponse, JarvisAI};
use cli::jarvis::jarvis_talk;
use engine::classifier::{InputClassifier, InputType};
use engine::{execute, try_builtin, CommandResult, LoopAction};
use cli::prompt::JarvisPrompt;
use reedline::{Highlighter, Reedline, Signal, StyledText};
use nu_ansi_term::{Color, Style};
use storage::BlackBox;
use tracing::{debug, info, warn};

/// ユーザー入力を白色でハイライトするシンプルなハイライター
struct WhiteHighlighter;

impl Highlighter for WhiteHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut styled = StyledText::new();
        // Color::White は ANSI 7 (灰色) になるため、RGB で明るい白を指定
        styled.push((Style::new().fg(Color::Rgb(255, 255, 255)), line.to_string()));
        styled
    }
}

#[tokio::main]
async fn main() {
    // .env ファイルから環境変数を読み込む
    dotenvy::dotenv().ok();

    // ログシステムの初期化（_guard は main 終了まで保持する必要がある）
    let _guard = logging::init_logging();
    info!("jarvish started");

    let mut editor = Reedline::create().with_highlighter(Box::new(WhiteHighlighter));
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
                                    jarvis_talk(&format!(
                                        "Understood, sir. Proceeding to execute: {cmd}"
                                    ));
                                    execute(cmd)
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

    info!("jarvish shutting down");
    cli::banner::print_goodbye();
}
