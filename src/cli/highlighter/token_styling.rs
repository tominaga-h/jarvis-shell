use nu_ansi_term::{Color, Style};
use reedline::StyledText;

use super::{env_styling, tokenizer};

pub(super) fn flush_word(styled: &mut StyledText, current_word: &str, is_command: &mut bool) {
    if *is_command || !env_styling::is_assignment(current_word) {
        tokenizer::style_word(styled, current_word, is_command);
    } else {
        styled.push((Style::new().fg(Color::DarkGray), current_word.to_string()));
    }
}
