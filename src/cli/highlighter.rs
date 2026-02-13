use nu_ansi_term::{Color, Style};
use reedline::{Highlighter, StyledText};

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
pub struct JarvisHighlighter;

impl Default for JarvisHighlighter {
    fn default() -> Self {
        Self
    }
}

impl Highlighter for JarvisHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut styled = StyledText::new();
        let mut chars = line.chars().peekable();
        let mut current_word = String::new();
        let mut is_command = true;
        let mut in_quote = None; // None, Some('\''), Some('"')

        // 簡易的なトークナイザループ
        while let Some(c) = chars.next() {
            if let Some(quote) = in_quote {
                // クオート内
                current_word.push(c);
                if c == quote {
                    // クオート終了 -> 文字列としてスタイル適用 (Yellow)
                    styled.push((Style::new().fg(Color::Yellow), current_word.clone()));
                    current_word.clear();
                    in_quote = None;
                }
            } else if c == '"' || c == '\'' {
                // クオート開始
                if !current_word.is_empty() {
                    // 直前の単語を確定
                    style_word(&mut styled, &current_word, &mut is_command);
                    current_word.clear();
                }
                current_word.push(c);
                in_quote = Some(c);
            } else if c == '|' {
                // パイプ演算子：直前の単語を確定し、次の単語をコマンドとして扱う
                if !current_word.is_empty() {
                    style_word(&mut styled, &current_word, &mut is_command);
                    current_word.clear();
                }
                styled.push((Style::new().fg(Color::Cyan).bold(), c.to_string()));
                is_command = true;
            } else if c == '>' || c == '<' {
                // リダイレクト演算子：直前の単語を確定し、次の単語のハイライトをやめる
                if !current_word.is_empty() {
                    style_word(&mut styled, &current_word, &mut is_command);
                    current_word.clear();
                }

                // `>>` の場合は2文字まとめてハイライト
                let mut op = c.to_string();
                if c == '>' && chars.peek() == Some(&'>') {
                    op.push(chars.next().unwrap());
                }
                styled.push((Style::new().fg(Color::Cyan).bold(), op));
                is_command = false;
            } else if c == ';' {
            } else if c.is_whitespace() {
                // 空白（単語の区切り）
                if !current_word.is_empty() {
                    style_word(&mut styled, &current_word, &mut is_command);
                    current_word.clear();
                }
                // 空白はそのままの色で
                styled.push((Style::default(), c.to_string()));
            } else {
                // 通常の文字
                current_word.push(c);
            }
        }

        // 残りの単語を処理
        if !current_word.is_empty() {
            if in_quote.is_some() {
                // 閉じられていないクオートは赤く警告
                styled.push((Style::new().fg(Color::Red).bold(), current_word));
            } else {
                style_word(&mut styled, &current_word, &mut is_command);
            }
        }

        styled
    }
}

/// 単語の種類に応じてスタイルを適用して StyledText に追加するヘルパー
fn style_word(styled: &mut StyledText, word: &str, is_command: &mut bool) {
    let style = if *is_command {
        *is_command = false;
        // コマンド名: Magenta + Bold
        Style::new().fg(Color::Magenta).bold()
    } else if word.starts_with('-') {
        // オプションフラグ: Blue
        Style::new().fg(Color::Blue)
    } else if word.contains('=') && !word.starts_with('\'') && !word.starts_with('"') {
        // 環境変数設定のような KV ペア: DarkGray
        Style::new().fg(Color::DarkGray)
    } else {
        // 通常の引数: White
        Style::new().fg(Color::White)
    };

    styled.push((style, word.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ハイライト結果から (Style, String) のペア一覧を取得するヘルパー
    fn highlight_segments(input: &str) -> Vec<(Style, String)> {
        let h = JarvisHighlighter;
        let styled = h.highlight(input, 0);
        styled.buffer.clone()
    }

    /// スタイル定義のショートカット
    fn cmd_style() -> Style {
        Style::new().fg(Color::Magenta).bold()
    }
    fn flag_style() -> Style {
        Style::new().fg(Color::Blue)
    }
    fn arg_style() -> Style {
        Style::new().fg(Color::White)
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
        // 先頭の KEY=VAL はコマンド位置なので Magenta になる
        // 2番目以降の KEY=VAL は DarkGray
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
            vec![
                (ws(), " ".into()),
                (ws(), " ".into()),
                (ws(), " ".into()),
            ]
        );
    }

    #[test]
    fn test_pipe_without_spaces() {
        // `ls|grep x` のようにスペースなしでも正しくハイライトされる
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
}
