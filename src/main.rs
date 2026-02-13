mod ai;
mod cli;
mod engine;
mod storage;

use ai::client::{AiResponse, JarvisAI};
use engine::{execute, try_builtin, CommandResult, LoopAction};
use cli::prompt::JarvisPrompt;
use reedline::{Highlighter, Reedline, Signal, StyledText};
use nu_ansi_term::{Color, Style};
use storage::BlackBox;

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

    let mut editor = Reedline::create().with_highlighter(Box::new(WhiteHighlighter));
    let prompt = JarvisPrompt::new();

    // Black Box（履歴永続化）の初期化
    let black_box = match BlackBox::open() {
        Ok(bb) => Some(bb),
        Err(e) => {
            eprintln!("jarvish: warning: failed to initialize black box: {e}");
            None
        }
    };

    // AI クライアントの初期化
    let ai_client = match JarvisAI::new() {
        Ok(ai) => Some(ai),
        Err(e) => {
            eprintln!("jarvish: warning: AI disabled: {e}");
            None // API キー未設定時は AI 機能を無効化
        }
    };

    cli::banner::print_welcome();

    loop {
        match editor.read_line(&prompt) {
            Ok(Signal::Success(line)) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }

                // 1. ビルトインコマンドをチェック（cd, cwd, exit は AI を介さず直接実行）
                if let Some(result) = try_builtin(&line) {
                    println!(); // 実行結果の後に空行を追加

                    match result.action {
                        LoopAction::Continue => {
                            if let Some(ref bb) = black_box {
                                if let Err(e) = bb.record(&line, &result) {
                                    eprintln!(
                                        "jarvish: warning: failed to record history: {e}"
                                    );
                                }
                            }
                        }
                        LoopAction::Exit => break,
                    }
                    continue;
                }

                // 2. AI 処理（AI 無効時は従来の execute にフォールバック）
                let result = if let Some(ref ai) = ai_client {
                    // BlackBox から直近 5 件のコマンド履歴をコンテキストとして取得
                    let context = black_box
                        .as_ref()
                        .and_then(|bb| bb.get_recent_context(5).ok())
                        .unwrap_or_default();

                    match ai.process_input(&line, &context).await {
                        Ok(AiResponse::Command(cmd)) => {
                            // AI がコマンドと判定 → 実行
                            execute(&cmd)
                        }
                        Ok(AiResponse::NaturalLanguage(text)) => {
                            // AI が自然言語と判定 → ストリーミング表示済み、結果を記録
                            CommandResult::success(text)
                        }
                        Err(_e) => {
                            // AI エラー時はコマンドとして直接実行にフォールバック
                            execute(&line)
                        }
                    }
                } else {
                    // AI 無効時は従来通り実行
                    execute(&line)
                };

                println!(); // 実行結果の後に空行を追加

                // 履歴を記録
                if result.action == LoopAction::Continue {
                    if let Some(ref bb) = black_box {
                        if let Err(e) = bb.record(&line, &result) {
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
                break;
            }
            Err(e) => {
                eprintln!("jarvish: error: {e}");
                break;
            }
        }
    }

    cli::banner::print_goodbye();
}
