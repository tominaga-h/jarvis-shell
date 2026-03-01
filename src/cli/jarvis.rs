use std::io::{self, Write};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};
use termimad::crossterm::style::Attribute;
use termimad::{rgb, CompoundStyle, MadSkin, StyledChar};

use super::color::{red, white};

/// スピナーを生成・開始する共通ヘルパー。
///
/// `template` に `{spinner}` と `{msg}` を含むテンプレート文字列を渡す。
/// テンプレートが不正な場合はデフォルトスタイルにフォールバックする。
fn create_spinner(template: &str, message: &str) -> ProgressBar {
    let spinner = ProgressBar::new_spinner();
    let style = ProgressStyle::default_spinner()
        .template(template)
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏");
    spinner.set_style(style);
    spinner.set_message(message.to_string());
    spinner.enable_steady_tick(Duration::from_millis(80));
    spinner
}

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
    create_spinner("📖 {spinner} Reading file: {msg}", path)
}

/// Jarvis がファイルを書き込むときに使う共通関数。
/// 呼び出し元で `finish_and_clear()` を呼んでスピナーを停止すること。
pub fn jarvis_write_file(path: &str) -> ProgressBar {
    create_spinner("📝 {spinner} Writing file: {msg}", path)
}

/// AI 処理中に表示するスピナーを生成・開始する。
/// `{msg}` を含むテンプレートにより、進捗メッセージを動的に更新できる。
pub fn jarvis_spinner() -> ProgressBar {
    create_spinner("🤵 {spinner} {msg}", "Thinking...")
}

/// Jarvish 専用の Markdown スキンを構築する。
///
/// Iron Man の赤と金をベースにした配色:
/// - 見出し: ゴールド系グラデーション
/// - 太字: クリムゾンレッド
/// - イタリック: ソフトレッド
/// - コード: ウォームゴールド文字 on ダークレッド背景
/// - 引用/弾丸: クリムゾン/ゴールド
fn jarvish_skin() -> MadSkin {
    let gold = rgb(255, 184, 0);
    let light_gold = rgb(255, 210, 100);
    let warm_gold = rgb(220, 180, 100);
    let crimson = rgb(220, 50, 50);
    let soft_red = rgb(230, 130, 120);
    let code_fg = rgb(240, 210, 170);
    let code_bg = rgb(40, 20, 20);
    let dark_red = rgb(140, 40, 40);
    let deep_gold = rgb(180, 140, 50);

    let mut skin = MadSkin::default_dark();

    skin.headers[0].set_fg(gold);
    skin.headers[0].add_attr(Attribute::Bold);
    skin.headers[1].set_fg(light_gold);
    skin.headers[2].set_fg(warm_gold);

    skin.bold.set_fg(crimson);
    skin.bold.add_attr(Attribute::Bold);

    skin.italic.set_fg(soft_red);

    skin.inline_code.set_fgbg(code_fg, code_bg);
    skin.code_block.set_fgbg(code_fg, code_bg);

    skin.bullet = StyledChar::new(CompoundStyle::with_fg(gold), '•');
    skin.quote_mark = StyledChar::new(
        CompoundStyle::new(Some(crimson), None, Attribute::Bold.into()),
        '▐',
    );
    skin.horizontal_rule = StyledChar::new(CompoundStyle::with_fg(dark_red), '―');
    skin.table.set_fg(deep_gold);

    skin
}

/// termimad を使って Markdown テキストをレンダリングし、ターミナルに表示する。
pub fn jarvis_render_markdown(text: &str) {
    print!("🤵 ");
    let skin = jarvish_skin();
    skin.print_text(text);
}

/// Markdown をレンダリングせず、プレーンテキストとしてそのまま表示する。
pub fn jarvis_print_plain(text: &str) {
    println!("🤵 {text}");
}

/// Jarvis ペルソナなしで Markdown テキストをレンダリングする。
/// AI パイプなど、🤵 プレフィックスが不要な場面で使用する。
pub fn render_markdown(text: &str) {
    let skin = jarvish_skin();
    skin.print_text(text);
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
