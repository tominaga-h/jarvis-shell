use std::sync::Arc;

use nu_ansi_term::Style;
use reedline::{Highlighter, StyledText};

use crate::engine::classifier::{InputClassifier, InputType};

use super::{operator_styling, quote_styling, token_styling};

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
                    quote_styling::style_closed_quote(&mut styled, &current_word);
                    current_word.clear();
                    in_quote = None;
                }
            } else if c == '"' || c == '\'' {
                if !current_word.is_empty() {
                    token_styling::flush_word(&mut styled, &current_word, &mut is_command);
                    current_word.clear();
                }
                current_word.push(c);
                in_quote = Some(c);
            } else if operator_styling::style_operator(
                c,
                &mut chars,
                &mut styled,
                &mut current_word,
                &mut is_command,
            ) {
            } else if c.is_whitespace() {
                if !current_word.is_empty() {
                    token_styling::flush_word(&mut styled, &current_word, &mut is_command);
                    current_word.clear();
                }
                styled.push((Style::default(), c.to_string()));
            } else {
                current_word.push(c);
            }
        }

        if !current_word.is_empty() {
            if in_quote.is_some() {
                quote_styling::style_unclosed_quote(&mut styled, current_word);
            } else {
                token_styling::flush_word(&mut styled, &current_word, &mut is_command);
            }
        }

        styled
    }
}
