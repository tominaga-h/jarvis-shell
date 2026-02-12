mod engine;
mod prompt;

use engine::{execute, LoopAction};
use prompt::JarvisPrompt;
use reedline::{Reedline, Signal};

#[tokio::main]
async fn main() {
    let mut editor = Reedline::create();
    let prompt = JarvisPrompt::new();

    loop {
        match editor.read_line(&prompt) {
            Ok(Signal::Success(line)) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }

                let result = execute(&line);

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
}
