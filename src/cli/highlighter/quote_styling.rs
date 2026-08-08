use nu_ansi_term::{Color, Style};
use reedline::StyledText;

pub(super) fn style_closed_quote(styled: &mut StyledText, word: &str) {
    styled.push((Style::new().fg(Color::Yellow), word.to_string()));
}

pub(super) fn style_unclosed_quote(styled: &mut StyledText, word: String) {
    styled.push((Style::new().fg(Color::Red).bold(), word));
}
