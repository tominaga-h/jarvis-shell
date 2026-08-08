use std::iter::Peekable;
use std::str::Chars;

use nu_ansi_term::{Color, Style};
use reedline::StyledText;

use super::token_styling::flush_word;

pub(super) fn style_operator(
    c: char,
    chars: &mut Peekable<Chars<'_>>,
    styled: &mut StyledText,
    current_word: &mut String,
    is_command: &mut bool,
) -> bool {
    if c == '&' && chars.peek() == Some(&'&') {
        if !current_word.is_empty() {
            flush_word(styled, current_word, is_command);
            current_word.clear();
        }
        chars.next();
        styled.push((Style::new().fg(Color::Cyan).bold(), "&&".to_string()));
        *is_command = true;
        true
    } else if c == '|' {
        if !current_word.is_empty() {
            flush_word(styled, current_word, is_command);
            current_word.clear();
        }
        if chars.peek() == Some(&'|') {
            chars.next();
            styled.push((Style::new().fg(Color::Cyan).bold(), "||".to_string()));
        } else {
            styled.push((Style::new().fg(Color::Cyan).bold(), c.to_string()));
        }
        *is_command = true;
        true
    } else if c == '>' || c == '<' {
        if !current_word.is_empty() {
            flush_word(styled, current_word, is_command);
            current_word.clear();
        }

        let mut op = c.to_string();
        if c == '>' && chars.peek() == Some(&'>') {
            if let Some(next_ch) = chars.next() {
                op.push(next_ch);
            }
        }
        styled.push((Style::new().fg(Color::Cyan).bold(), op));
        *is_command = false;
        true
    } else if c == ';' {
        if !current_word.is_empty() {
            flush_word(styled, current_word, is_command);
            current_word.clear();
        }
        styled.push((Style::new().fg(Color::Cyan).bold(), c.to_string()));
        *is_command = true;
        true
    } else {
        false
    }
}
