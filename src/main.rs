mod banner;
mod color;
mod engine;
mod jarvis;
mod prompt;
mod storage;

use engine::{execute, LoopAction};
use prompt::JarvisPrompt;
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

    banner::print_welcome();

    loop {
        match editor.read_line(&prompt) {
            Ok(Signal::Success(line)) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }

                let result = execute(&line);
                println!(); // 実行結果の後に空行を追加

                match result.action {
                    LoopAction::Continue => {
                        if let Some(ref bb) = black_box {
                            if let Err(e) = bb.record(&line, &result) {
                                eprintln!("jarvish: warning: failed to record history: {e}");
                            }
                        }
                    }
                    LoopAction::Exit => break,
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

    banner::print_goodbye();
}
