use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

use super::color::white;

/// Jarvis が発話するときに使う共通関数。
/// 先頭に 🤵 絵文字を付与し、白色テキストで表示する。
pub fn jarvis_talk(message: &str) {
    println!("🤵 {}", white(message));
}

/// Jarvis が Tool Call を受信してコマンドを実行するときに使う共通関数。
pub fn jarvis_command_notice(command: &str) {
    println!("\n👉 {command}\n");
}

/// AI 処理中に表示するスピナーを生成・開始する。
/// メッセージなしのシンプルなスピナーを表示する。
pub fn jarvis_spinner() -> ProgressBar {
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("🤵 {spinner}")
            .expect("Invalid spinner template"),
    );
    spinner.enable_steady_tick(Duration::from_millis(80));
    spinner
}

/// ストリーミング開始時のプレフィックスを表示する（改行なし）。
pub fn jarvis_print_prefix() {
    print!("🤵 ");
}

/// ストリーミング中のテキスト片を表示する（改行なし）。
pub fn jarvis_print_chunk(chunk: &str) {
    print!("{}", white(chunk));
}

/// ストリーミング終了時の改行を出力する。
pub fn jarvis_print_end() {
    println!();
}
