//! クォート対応トークナイザ
//!
//! `shell_words` はクォートを剥がした結果のみを返すため、
//! クォートされたトークン（例: `'*'`、`"{a,b}"`）が
//! グロブ/ブレース展開の対象外であることを判別できない。
//!
//! このモジュールは入力文字列を 1 トークンずつ走査し、
//! 各トークンに対して `(value, quoted)` を返す。
//! `quoted = true` のトークンはシェル展開の対象外とする。
//!
//! POSIX 互換の制御演算子（`|`, `>`, `>>`, `<`, `&&`, `||`, `;`）は
//! 専用トークンとして分離する。

/// 1 つのトークンとそのクォート状態
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub value: String,
    /// 当該トークンの少なくとも一部がシングル/ダブルクォートで囲まれていた場合 true
    pub quoted: bool,
}

/// パースエラー
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitError {
    UnmatchedSingleQuote,
    UnmatchedDoubleQuote,
    DanglingBackslash,
}

impl std::fmt::Display for SplitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SplitError::UnmatchedSingleQuote => write!(f, "unmatched single quote"),
            SplitError::UnmatchedDoubleQuote => write!(f, "unmatched double quote"),
            SplitError::DanglingBackslash => write!(f, "dangling backslash"),
        }
    }
}

/// 入力文字列を `Token` 列に分割する。
///
/// shell_words::split と同じ意味論で、
/// - シングルクォート内は完全にリテラル（エスケープなし）
/// - ダブルクォート内は `\` で `"` `\` `$` `\`` をエスケープ可能
/// - クォート外は `\` で次の 1 文字をエスケープ
/// - 制御演算子 `|`, `>`, `>>`, `<`, `&&`, `||`, `;` は単独トークンに分離
pub fn split_quoted(input: &str) -> Result<Vec<Token>, SplitError> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut quoted = false;

    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        if !in_token && c.is_whitespace() {
            i += 1;
            continue;
        }

        // 演算子: 既存トークンを flush してから演算子を 1 トークンとして追加
        let op_len = operator_at(&chars, i);
        if op_len > 0 {
            if in_token {
                tokens.push(Token {
                    value: std::mem::take(&mut current),
                    quoted,
                });
                in_token = false;
                quoted = false;
            }
            let op: String = chars[i..i + op_len].iter().collect();
            tokens.push(Token {
                value: op,
                quoted: false,
            });
            i += op_len;
            continue;
        }

        match c {
            '\'' => {
                in_token = true;
                quoted = true;
                i += 1;
                let mut found = false;
                while i < chars.len() {
                    if chars[i] == '\'' {
                        i += 1;
                        found = true;
                        break;
                    }
                    current.push(chars[i]);
                    i += 1;
                }
                if !found {
                    return Err(SplitError::UnmatchedSingleQuote);
                }
            }
            '"' => {
                in_token = true;
                quoted = true;
                i += 1;
                let mut found = false;
                while i < chars.len() {
                    let ch = chars[i];
                    if ch == '"' {
                        i += 1;
                        found = true;
                        break;
                    }
                    if ch == '\\' && i + 1 < chars.len() {
                        let next = chars[i + 1];
                        if matches!(next, '"' | '\\' | '$' | '`') {
                            current.push(next);
                            i += 2;
                            continue;
                        }
                    }
                    current.push(ch);
                    i += 1;
                }
                if !found {
                    return Err(SplitError::UnmatchedDoubleQuote);
                }
            }
            '\\' => {
                if i + 1 >= chars.len() {
                    return Err(SplitError::DanglingBackslash);
                }
                // クォート外の `\X` → `X` をリテラル化し quoted フラグを立てる
                in_token = true;
                quoted = true;
                current.push(chars[i + 1]);
                i += 2;
            }
            ch if ch.is_whitespace() => {
                tokens.push(Token {
                    value: std::mem::take(&mut current),
                    quoted,
                });
                in_token = false;
                quoted = false;
                i += 1;
            }
            ch => {
                in_token = true;
                current.push(ch);
                i += 1;
            }
        }
    }

    if in_token {
        tokens.push(Token {
            value: current,
            quoted,
        });
    }

    Ok(tokens)
}

/// `chars[i..]` の先頭が演算子なら長さを返す。なければ 0。
fn operator_at(chars: &[char], i: usize) -> usize {
    if i >= chars.len() {
        return 0;
    }
    // 2 文字演算子
    if i + 1 < chars.len() {
        let two: String = chars[i..i + 2].iter().collect();
        if matches!(two.as_str(), "&&" | "||" | ">>") {
            return 2;
        }
    }
    // 1 文字演算子
    match chars[i] {
        '|' | '<' | '>' | ';' => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(value: &str, quoted: bool) -> Token {
        Token {
            value: value.to_string(),
            quoted,
        }
    }

    #[test]
    fn simple_tokens_unquoted() {
        let toks = split_quoted("echo hello world").unwrap();
        assert_eq!(
            toks,
            vec![t("echo", false), t("hello", false), t("world", false)]
        );
    }

    #[test]
    fn single_quoted_glob_is_marked_quoted() {
        let toks = split_quoted("echo '*'").unwrap();
        assert_eq!(toks, vec![t("echo", false), t("*", true)]);
    }

    #[test]
    fn double_quoted_brace_is_marked_quoted() {
        let toks = split_quoted("echo \"{a,b}\"").unwrap();
        assert_eq!(toks, vec![t("echo", false), t("{a,b}", true)]);
    }

    #[test]
    fn backslash_escape_marks_token_as_quoted() {
        let toks = split_quoted("echo \\*").unwrap();
        assert_eq!(toks, vec![t("echo", false), t("*", true)]);
    }

    #[test]
    fn operators_are_separated() {
        let toks = split_quoted("ls *.txt | head").unwrap();
        assert_eq!(
            toks,
            vec![
                t("ls", false),
                t("*.txt", false),
                t("|", false),
                t("head", false),
            ]
        );
    }

    #[test]
    fn double_redirect_operator() {
        let toks = split_quoted("echo a >> file").unwrap();
        assert_eq!(
            toks,
            vec![
                t("echo", false),
                t("a", false),
                t(">>", false),
                t("file", false),
            ]
        );
    }

    #[test]
    fn and_or_semi_operators() {
        let toks = split_quoted("a && b || c ; d").unwrap();
        assert_eq!(
            toks,
            vec![
                t("a", false),
                t("&&", false),
                t("b", false),
                t("||", false),
                t("c", false),
                t(";", false),
                t("d", false),
            ]
        );
    }

    #[test]
    fn unmatched_single_quote_errors() {
        assert_eq!(
            split_quoted("echo 'unclosed"),
            Err(SplitError::UnmatchedSingleQuote)
        );
    }

    #[test]
    fn unmatched_double_quote_errors() {
        assert_eq!(
            split_quoted("echo \"unclosed"),
            Err(SplitError::UnmatchedDoubleQuote)
        );
    }

    #[test]
    fn mixed_quoted_unquoted_concatenation() {
        // `foo'bar'` → `foobar`, quoted=true（部分的にクォートされていれば全体を quoted 扱い）
        let toks = split_quoted("foo'bar'").unwrap();
        assert_eq!(toks, vec![t("foobar", true)]);
    }

    #[test]
    fn double_quote_escapes() {
        let toks = split_quoted(r#""hello \"world\"""#).unwrap();
        assert_eq!(toks, vec![t(r#"hello "world""#, true)]);
    }
}
