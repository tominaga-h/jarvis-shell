use std::io::{self, Write};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

use super::color::{red, white};

/// Jarvis が発話するときに使う共通関数。
/// 先頭に 🤵 絵文字を付与し、白色テキストで表示する。
pub fn jarvis_talk(message: &str) {
    println!("🤵 {}", white(message));
}

/// Jarvis が Tool Call を受信してコマンドを実行するときに使う共通関数。
pub fn jarvis_notice(command: &str) {
    println!("\n👉 {command}\n");
}

/// Jarvis がファイルを読み取るときに使う共通関数。
/// メッセージを `println!` で永続出力し、スピナーを分離して返す。
/// 呼び出し元で `finish_and_clear()` を呼んでスピナーを停止すること。
pub fn jarvis_read_file(path: &str) -> ProgressBar {
    println!("📖 Reading file: {path}");
    let spinner = ProgressBar::new_spinner();
    spinner.enable_steady_tick(Duration::from_millis(80));
    spinner
}

/// Jarvis がファイルを書き込むときに使う共通関数。
/// メッセージを `println!` で永続出力し、スピナーを分離して返す。
/// 呼び出し元で `finish_and_clear()` を呼んでスピナーを停止すること。
pub fn jarvis_write_file(path: &str) -> ProgressBar {
    println!("📝 Writing file: {path}");
    let spinner = ProgressBar::new_spinner();
    spinner.enable_steady_tick(Duration::from_millis(80));
    spinner
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

/// コマンド異常終了時にユーザーへ調査の可否を確認する。
///
/// 「調査しますか？ [Y/n]: 」と表示し、ユーザーが `Y`/`y`/空行（Enter）を
/// 入力した場合に `true` を返す。それ以外は `false`。
pub fn jarvis_ask_investigate(exit_code: i32) -> bool {
    print!(
        "🤵 Sir, {} {}",
        red(&format!(
            "the command exited with an error (code: {exit_code})."
        )),
        white("Would you like to investigate? [Y/n]: ")
    );
    let _ = io::stdout().flush();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return false;
    }

    println!();

    let trimmed = input.trim().to_lowercase();
    trimmed.is_empty() || trimmed == "y" || trimmed == "yes"
}
