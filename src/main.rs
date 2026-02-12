mod banner;
mod color;
mod engine;
mod jarvis;
mod prompt;

use engine::{execute, LoopAction};
use prompt::JarvisPrompt;
use reedline::{Highlighter, Reedline, Signal, StyledText};
use nu_ansi_term::{Color, Style};

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
                        // Phase 2: ここで result を Black Box に永続化する
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
