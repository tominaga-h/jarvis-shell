mod tokenizer;

use std::sync::Arc;

use nu_ansi_term::{Color, Style};
use reedline::{Highlighter, StyledText};

use crate::engine::classifier::{InputClassifier, InputType};
use tokenizer::style_word;

/// Jarvis Shell 用のシンタックスハイライター
///
/// 入力されたコマンドラインを解析し、以下のルールで色分けを行う：
/// - コマンド名（先頭単語）: Magenta + Bold
/// - パイプ (`|`) 後の先頭コマンド: Magenta + Bold
/// - パイプ演算子 (`|`): Cyan + Bold
/// - リダイレクト演算子 (`>`, `>>`, `<`): Cyan + Bold
/// - オプションフラグ (`-f`, `--force`): Blue
/// - 環境変数設定 (`VAR=VAL`): DarkGray
/// - 文字列リテラル (`"..."`, `'...'`): Yellow
/// - 閉じられていないクオート: Red (警告)
/// - その他（引数など）: White
///
/// 自然言語入力時はハイライトを適用せず、プレーンテキストとして表示する。
pub struct JarvisHighlighter {
    classifier: Arc<InputClassifier>,
}

impl JarvisHighlighter {
    /// InputClassifier を共有して新しいハイライターを作成する。
    pub fn new(classifier: Arc<InputClassifier>) -> Self {
        Self { classifier }
    }
}

impl Highlighter for JarvisHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        if self.classifier.classify(line) == InputType::NaturalLanguage {
            let mut styled = StyledText::new();
            styled.push((Style::default(), line.to_string()));
            return styled;
        }

        let mut styled = StyledText::new();
        let mut chars = line.chars().peekable();
        let mut current_word = String::new();
        let mut is_command = true;
        let mut in_quote = None;

        while let Some(c) = chars.next() {
            if let Some(quote) = in_quote {
                current_word.push(c);
                if c == quote {
                    styled.push((Style::new().fg(Color::Yellow), current_word.clone()));
                    current_word.clear();
                    in_quote = None;
                }
            } else if c == '"' || c == '\'' {
                if !current_word.is_empty() {
                    style_word(&mut styled, &current_word, &mut is_command);
                    current_word.clear();
                }
                current_word.push(c);
                in_quote = Some(c);
            } else if c == '&' && chars.peek() == Some(&'&') {
                if !current_word.is_empty() {
                    style_word(&mut styled, &current_word, &mut is_command);
                    current_word.clear();
                }
                chars.next();
                styled.push((Style::new().fg(Color::Cyan).bold(), "&&".to_string()));
                is_command = true;
            } else if c == '|' {
                if !current_word.is_empty() {
                    style_word(&mut styled, &current_word, &mut is_command);
                    current_word.clear();
                }
                if chars.peek() == Some(&'|') {
                    chars.next();
                    styled.push((Style::new().fg(Color::Cyan).bold(), "||".to_string()));
                } else {
                    styled.push((Style::new().fg(Color::Cyan).bold(), c.to_string()));
                }
                is_command = true;
            } else if c == '>' || c == '<' {
                if !current_word.is_empty() {
                    style_word(&mut styled, &current_word, &mut is_command);
                    current_word.clear();
                }

                let mut op = c.to_string();
                if c == '>' && chars.peek() == Some(&'>') {
                    if let Some(next_ch) = chars.next() {
                        op.push(next_ch);
                    }
                }
                styled.push((Style::new().fg(Color::Cyan).bold(), op));
                is_command = false;
            } else if c == ';' {
                if !current_word.is_empty() {
                    style_word(&mut styled, &current_word, &mut is_command);
                    current_word.clear();
                }
                styled.push((Style::new().fg(Color::Cyan).bold(), c.to_string()));
                is_command = true;
            } else if c.is_whitespace() {
                if !current_word.is_empty() {
                    style_word(&mut styled, &current_word, &mut is_command);
                    current_word.clear();
                }
                styled.push((Style::default(), c.to_string()));
            } else {
                current_word.push(c);
            }
        }

        if !current_word.is_empty() {
            if in_quote.is_some() {
                styled.push((Style::new().fg(Color::Red).bold(), current_word));
            } else {
                style_word(&mut styled, &current_word, &mut is_command);
            }
        }

        styled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_highlighter() -> JarvisHighlighter {
        JarvisHighlighter::new(Arc::new(InputClassifier::new()))
    }

    fn highlight_segments(input: &str) -> Vec<(Style, String)> {
        let h = test_highlighter();
        let styled = h.highlight(input, 0);
        styled.buffer.clone()
    }

    fn cmd_style() -> Style {
        Style::new().fg(Color::Magenta).bold()
    }
    fn flag_style() -> Style {
        Style::new().fg(Color::Blue)
    }
    fn arg_style() -> Style {
        Style::new().fg(Color::LightGray)
    }
    fn pipe_style() -> Style {
        Style::new().fg(Color::Cyan).bold()
    }
    fn redirect_style() -> Style {
        Style::new().fg(Color::Cyan).bold()
    }
    fn quote_style() -> Style {
        Style::new().fg(Color::Yellow)
    }
    fn unclosed_quote_style() -> Style {
        Style::new().fg(Color::Red).bold()
    }
    fn env_style() -> Style {
        Style::new().fg(Color::DarkGray)
    }
    fn ws() -> Style {
        Style::default()
    }

    #[test]
    fn test_simple_command() {
        let segs = highlight_segments("ls");
        assert_eq!(segs, vec![(cmd_style(), "ls".into())]);
    }

    #[test]
    fn test_command_with_flag_and_arg() {
        let segs = highlight_segments("ls -la /tmp");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "ls".into()),
                (ws(), " ".into()),
                (flag_style(), "-la".into()),
                (ws(), " ".into()),
                (arg_style(), "/tmp".into()),
            ]
        );
    }

    #[test]
    fn test_long_flag() {
        let segs = highlight_segments("git --version");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "git".into()),
                (ws(), " ".into()),
                (flag_style(), "--version".into()),
            ]
        );
    }

    #[test]
    fn test_pipe_highlights_both_commands() {
        let segs = highlight_segments("cat file | grep error");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "cat".into()),
                (ws(), " ".into()),
                (arg_style(), "file".into()),
                (ws(), " ".into()),
                (pipe_style(), "|".into()),
                (ws(), " ".into()),
                (cmd_style(), "grep".into()),
                (ws(), " ".into()),
                (arg_style(), "error".into()),
            ]
        );
    }

    #[test]
    fn test_multiple_pipes() {
        let segs = highlight_segments("cat f | grep x | wc -l");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "cat".into()),
                (ws(), " ".into()),
                (arg_style(), "f".into()),
                (ws(), " ".into()),
                (pipe_style(), "|".into()),
                (ws(), " ".into()),
                (cmd_style(), "grep".into()),
                (ws(), " ".into()),
                (arg_style(), "x".into()),
                (ws(), " ".into()),
                (pipe_style(), "|".into()),
                (ws(), " ".into()),
                (cmd_style(), "wc".into()),
                (ws(), " ".into()),
                (flag_style(), "-l".into()),
            ]
        );
    }

    #[test]
    fn test_redirect_single() {
        let segs = highlight_segments("echo hi > out.txt");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "echo".into()),
                (ws(), " ".into()),
                (arg_style(), "hi".into()),
                (ws(), " ".into()),
                (redirect_style(), ">".into()),
                (ws(), " ".into()),
                (arg_style(), "out.txt".into()),
            ]
        );
    }

    #[test]
    fn test_redirect_append() {
        let segs = highlight_segments("echo hi >> log.txt");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "echo".into()),
                (ws(), " ".into()),
                (arg_style(), "hi".into()),
                (ws(), " ".into()),
                (redirect_style(), ">>".into()),
                (ws(), " ".into()),
                (arg_style(), "log.txt".into()),
            ]
        );
    }

    #[test]
    fn test_redirect_input() {
        let segs = highlight_segments("sort < data.txt");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "sort".into()),
                (ws(), " ".into()),
                (redirect_style(), "<".into()),
                (ws(), " ".into()),
                (arg_style(), "data.txt".into()),
            ]
        );
    }

    #[test]
    fn test_quoted_string_double() {
        let segs = highlight_segments("echo \"hello world\"");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "echo".into()),
                (ws(), " ".into()),
                (quote_style(), "\"hello world\"".into()),
            ]
        );
    }

    #[test]
    fn test_quoted_string_single() {
        let segs = highlight_segments("echo 'hello world'");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "echo".into()),
                (ws(), " ".into()),
                (quote_style(), "'hello world'".into()),
            ]
        );
    }

    #[test]
    fn test_unclosed_quote() {
        let segs = highlight_segments("echo \"hello");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "echo".into()),
                (ws(), " ".into()),
                (unclosed_quote_style(), "\"hello".into()),
            ]
        );
    }

    #[test]
    fn test_env_var_assignment() {
        let segs = highlight_segments("cmd FOO=bar");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "cmd".into()),
                (ws(), " ".into()),
                (env_style(), "FOO=bar".into()),
            ]
        );
    }

    #[test]
    fn test_empty_input() {
        let segs = highlight_segments("");
        assert_eq!(segs, vec![]);
    }

    #[test]
    fn test_only_whitespace() {
        let segs = highlight_segments("   ");
        assert_eq!(
            segs,
            vec![(ws(), " ".into()), (ws(), " ".into()), (ws(), " ".into()),]
        );
    }

    #[test]
    fn test_pipe_without_spaces() {
        let segs = highlight_segments("ls|grep");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "ls".into()),
                (pipe_style(), "|".into()),
                (cmd_style(), "grep".into()),
            ]
        );
    }

    #[test]
    fn test_natural_language_no_highlight() {
        let segs = highlight_segments("what does this error mean?");
        assert_eq!(
            segs,
            vec![(Style::default(), "what does this error mean?".into())]
        );
    }

    #[test]
    fn test_jarvis_trigger_no_highlight() {
        let segs = highlight_segments("jarvis, help me");
        assert_eq!(segs, vec![(Style::default(), "jarvis, help me".into())]);
    }

    #[test]
    fn test_and_operator() {
        let segs = highlight_segments("make build && echo done");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "make".into()),
                (ws(), " ".into()),
                (arg_style(), "build".into()),
                (ws(), " ".into()),
                (pipe_style(), "&&".into()),
                (ws(), " ".into()),
                (cmd_style(), "echo".into()),
                (ws(), " ".into()),
                (arg_style(), "done".into()),
            ]
        );
    }

    #[test]
    fn test_and_without_spaces() {
        let segs = highlight_segments("true&&echo ok");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "true".into()),
                (pipe_style(), "&&".into()),
                (cmd_style(), "echo".into()),
                (ws(), " ".into()),
                (arg_style(), "ok".into()),
            ]
        );
    }

    #[test]
    fn test_or_operator() {
        let segs = highlight_segments("false || echo fallback");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "false".into()),
                (ws(), " ".into()),
                (pipe_style(), "||".into()),
                (ws(), " ".into()),
                (cmd_style(), "echo".into()),
                (ws(), " ".into()),
                (arg_style(), "fallback".into()),
            ]
        );
    }

    #[test]
    fn test_or_without_spaces() {
        let segs = highlight_segments("false||echo ok");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "false".into()),
                (pipe_style(), "||".into()),
                (cmd_style(), "echo".into()),
                (ws(), " ".into()),
                (arg_style(), "ok".into()),
            ]
        );
    }

    #[test]
    fn test_semi_operator() {
        let segs = highlight_segments("echo a ; echo b");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "echo".into()),
                (ws(), " ".into()),
                (arg_style(), "a".into()),
                (ws(), " ".into()),
                (pipe_style(), ";".into()),
                (ws(), " ".into()),
                (cmd_style(), "echo".into()),
                (ws(), " ".into()),
                (arg_style(), "b".into()),
            ]
        );
    }

    #[test]
    fn test_semi_without_spaces() {
        let segs = highlight_segments("echo a;echo b");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "echo".into()),
                (ws(), " ".into()),
                (arg_style(), "a".into()),
                (pipe_style(), ";".into()),
                (cmd_style(), "echo".into()),
                (ws(), " ".into()),
                (arg_style(), "b".into()),
            ]
        );
    }

    #[test]
    fn test_mixed_operators() {
        let segs = highlight_segments("cmd1 && cmd2 || cmd3 ; cmd4");
        assert_eq!(
            segs,
            vec![
                (cmd_style(), "cmd1".into()),
                (ws(), " ".into()),
                (pipe_style(), "&&".into()),
                (ws(), " ".into()),
                (cmd_style(), "cmd2".into()),
                (ws(), " ".into()),
                (pipe_style(), "||".into()),
                (ws(), " ".into()),
                (cmd_style(), "cmd3".into()),
                (ws(), " ".into()),
                (pipe_style(), ";".into()),
                (ws(), " ".into()),
                (cmd_style(), "cmd4".into()),
            ]
        );
    }

    #[test]
    fn test_japanese_natural_language_no_highlight() {
        let segs = highlight_segments("エラーを教えて");
        assert_eq!(segs, vec![(Style::default(), "エラーを教えて".into())]);
    }

    #[test]
    fn test_please_request_no_highlight() {
        let segs = highlight_segments("please explain the output");
        assert_eq!(
            segs,
            vec![(Style::default(), "please explain the output".into())]
        );
    }
}
