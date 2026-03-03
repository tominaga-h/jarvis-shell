//! トークンスタイリング — 単語の種類に応じたスタイル適用

use nu_ansi_term::{Color, Style};
use reedline::StyledText;

/// 単語の種類に応じてスタイルを適用して StyledText に追加するヘルパー
pub(super) fn style_word(styled: &mut StyledText, word: &str, is_command: &mut bool) {
    let style = if *is_command {
        *is_command = false;
        Style::new().fg(Color::Magenta).bold()
    } else if word.starts_with('-') {
        Style::new().fg(Color::Blue)
    } else if word.contains('=') && !word.starts_with('\'') && !word.starts_with('"') {
        Style::new().fg(Color::DarkGray)
    } else {
        Style::new().fg(Color::LightGray)
    };

    styled.push((style, word.to_string()));
}
