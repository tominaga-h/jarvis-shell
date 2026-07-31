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
//!
//! また、`$(...)` / backtick `` `...` `` のコマンド置換 span は
//! トークンの一部としてアトミックに取り込む（内部空白や `|` 等の演算子で
//! トークンを分断しない）。span の実展開は [`super::command_subst`] が担う。

use super::command_subst::SubstQuoting;

/// 1 つのトークンとそのクォート状態
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub value: String,
    /// 当該トークンの少なくとも一部がシングル/ダブルクォートで囲まれていた場合 true
    pub quoted: bool,
    /// `value` 内に未処理の `$(...)` / backtick コマンド置換 span を含む場合 true
    pub has_subst: bool,
    /// コマンド置換 span のクォート文脈。
    /// unquoted span を 1 つでも含めば `Unquoted`、全 span が
    /// ダブルクォート内なら `DoubleQuoted`。
    pub subst_quoting: SubstQuoting,
}

/// パースエラー
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitError {
    UnmatchedSingleQuote,
    UnmatchedDoubleQuote,
    DanglingBackslash,
    /// `$(...)` または backtick が閉じられていない
    UnterminatedSubstitution,
}

impl std::fmt::Display for SplitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SplitError::UnmatchedSingleQuote => write!(f, "unmatched single quote"),
            SplitError::UnmatchedDoubleQuote => write!(f, "unmatched double quote"),
            SplitError::DanglingBackslash => write!(f, "dangling backslash"),
            SplitError::UnterminatedSubstitution => {
                write!(f, "unterminated command substitution")
            }
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
///
/// 実装は [`split_quoted_spans`] に委譲し、バイト range を捨てるだけ
/// （トークナイザ実装は 1 箇所に一本化する）。
pub fn split_quoted(input: &str) -> Result<Vec<Token>, SplitError> {
    Ok(split_quoted_spans(input)?
        .into_iter()
        .map(|(t, _)| t)
        .collect())
}

/// `split_quoted` と同じトークン列に加えて、各トークンを生成した
/// `input` 内のバイト range（クォート/エスケープ/演算子の文字を含む）を返す。
///
/// エイリアス展開（[`super::alias::expand_aliases_in_line`]）のように、
/// トークン境界で元の入力文字列をバイト単位で置換したい場合に使う。
pub fn split_quoted_spans(input: &str) -> Result<Vec<(Token, std::ops::Range<usize>)>, SplitError> {
    let mut tokens: Vec<(Token, std::ops::Range<usize>)> = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut quoted = false;
    // 現トークンが未処理のコマンド置換 span を含むか
    let mut has_subst = false;
    // unquoted な span を 1 つでも含んだか（含めば最終的に Unquoted 文脈）
    let mut has_unquoted_subst = false;
    // 現トークンの開始バイトオフセット（in_token が true になった時点で記録）
    let mut token_start_byte = 0usize;

    // char index → byte offset の対応表（`chars[i]` の開始バイト位置）。
    // 末尾に `input.len()`（EOF のバイトオフセット）も追加しておく。
    let mut byte_offsets: Vec<usize> = input.char_indices().map(|(b, _)| b).collect();
    byte_offsets.push(input.len());

    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    // 現トークンを確定して push する。状態リセットは呼び出し側の責務。
    // `end_byte` はトークンを生成した range の終端（exclusive）。
    macro_rules! push_current {
        ($end_byte:expr) => {{
            // subst_quoting は has_subst のトークンでのみ意味を持つ。
            // 置換 span のうち unquoted を 1 つでも含めば Unquoted、
            // 全 span がダブルクォート内なら DoubleQuoted。
            // 置換なしトークンは既定 Unquoted（無視されるフィールド）。
            let subst_quoting = if has_subst && !has_unquoted_subst {
                SubstQuoting::DoubleQuoted
            } else {
                SubstQuoting::Unquoted
            };
            tokens.push((
                Token {
                    value: std::mem::take(&mut current),
                    quoted,
                    has_subst,
                    subst_quoting,
                },
                token_start_byte..$end_byte,
            ));
        }};
    }

    // 現トークンを確定して push し、トークン蓄積状態をリセットする
    // （トークン境界＝空白/演算子で使用）。
    macro_rules! flush_token {
        ($end_byte:expr) => {{
            push_current!($end_byte);
            in_token = false;
            quoted = false;
            has_subst = false;
            has_unquoted_subst = false;
        }};
    }

    while i < chars.len() {
        let c = chars[i];

        if !in_token && c.is_whitespace() {
            i += 1;
            continue;
        }

        if !in_token {
            token_start_byte = byte_offsets[i];
        }

        // 演算子: 既存トークンを flush してから演算子を 1 トークンとして追加。
        // ただしコマンド置換 span 内ではここに到達しない（span は下で
        // アトミックに取り込まれるため）。
        let op_len = operator_at(&chars, i);
        if op_len > 0 {
            if in_token {
                flush_token!(byte_offsets[i]);
            }
            let op: String = chars[i..i + op_len].iter().collect();
            let op_start = byte_offsets[i];
            let op_end = byte_offsets[i + op_len];
            tokens.push((
                Token {
                    value: op,
                    quoted: false,
                    has_subst: false,
                    subst_quoting: SubstQuoting::Unquoted,
                },
                op_start..op_end,
            ));
            i += op_len;
            continue;
        }

        match c {
            // unquoted コンテキストでのコマンド置換 span をアトミックに取り込む。
            '$' if i + 1 < chars.len() && chars[i + 1] == '(' => {
                in_token = true;
                has_subst = true;
                has_unquoted_subst = true;
                let end = scan_paren_span(&chars, i + 2)?;
                current.extend(&chars[i..end]);
                i = end;
            }
            '`' => {
                in_token = true;
                has_subst = true;
                has_unquoted_subst = true;
                let end = scan_backtick_span(&chars, i + 1)?;
                current.extend(&chars[i..end]);
                i = end;
            }
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
                    // ダブルクォート内のコマンド置換 span はリテラル化せず、
                    // 構文ごと取り込んで後段で展開する（DoubleQuoted 文脈）。
                    if ch == '$' && i + 1 < chars.len() && chars[i + 1] == '(' {
                        has_subst = true;
                        let end = scan_paren_span(&chars, i + 2)?;
                        current.extend(&chars[i..end]);
                        i = end;
                        continue;
                    }
                    if ch == '`' {
                        has_subst = true;
                        let end = scan_backtick_span(&chars, i + 1)?;
                        current.extend(&chars[i..end]);
                        i = end;
                        continue;
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
                flush_token!(byte_offsets[i]);
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
        // 入力末尾の確定。以後リセットは不要なので push のみ。
        push_current!(byte_offsets[chars.len()]);
    }

    Ok(tokens)
}

/// `$(` の直後（`start`）から括弧バランスで対応する `)` を探し、
/// 「`)` の次のインデックス」を返す。ネスト対応。
/// 閉じられていなければ [`SplitError::UnterminatedSubstitution`]。
///
/// span 検出ロジックは [`super::command_subst`] 側にも存在するが、
/// トークナイザは「span をリテラルとして丸ごと取り込む」目的、command_subst は
/// 「span を実行して置換する」目的と責務が異なるため、それぞれが独立して
/// span 境界を判定する（DRY より責務分離を優先）。
fn scan_paren_span(chars: &[char], start: usize) -> Result<usize, SplitError> {
    let mut depth = 1usize;
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    Err(SplitError::UnterminatedSubstitution)
}

/// 最初の backtick の直後（`start`）から次の backtick の「次のインデックス」を返す。
/// V1 はエスケープ未対応のため素朴に次の `` ` `` で閉じる。
/// 閉じられていなければ [`SplitError::UnterminatedSubstitution`]。
fn scan_backtick_span(chars: &[char], start: usize) -> Result<usize, SplitError> {
    let mut i = start;
    while i < chars.len() {
        if chars[i] == '`' {
            return Ok(i + 1);
        }
        i += 1;
    }
    Err(SplitError::UnterminatedSubstitution)
}

/// `chars[i..]` の先頭が演算子なら長さを返す。なければ 0。
///
/// 演算子表そのものは [`operator_prefix_len`] に委譲する（表は 1 箇所のみ）。
fn operator_at(chars: &[char], i: usize) -> usize {
    if i >= chars.len() {
        return 0;
    }
    // 演算子は最大 2 文字（ASCII）なので先頭 2 文字だけ切り出せば十分。
    let end = (i + 2).min(chars.len());
    let head: String = chars[i..end].iter().collect();
    operator_prefix_len(&head)
}

/// `s` の先頭が演算子トークンなら、そのバイト長を返す（なければ 0）。
///
/// 対応演算子: `&&` `||` `>>`（2 バイト）、`|` `<` `>` `;`（1 バイト）。
/// 補完系の寛容スキャナ（`cli/completer/context.rs`）と実行系の
/// [`split_quoted`] が同一の演算子表を参照するための共有関数。
pub(crate) fn operator_prefix_len(s: &str) -> usize {
    // 2 文字演算子（ASCII のみなのでバイト長 == 文字数）
    if s.starts_with("&&") || s.starts_with("||") || s.starts_with(">>") {
        return 2;
    }
    // 1 文字演算子
    match s.chars().next() {
        Some('|') | Some('<') | Some('>') | Some(';') => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 通常トークン（コマンド置換なし）を生成するヘルパ。
    fn t(value: &str, quoted: bool) -> Token {
        Token {
            value: value.to_string(),
            quoted,
            has_subst: false,
            subst_quoting: SubstQuoting::Unquoted,
        }
    }

    /// コマンド置換 span を含むトークンを生成するヘルパ。
    fn ts(value: &str, quoted: bool, subst_quoting: SubstQuoting) -> Token {
        Token {
            value: value.to_string(),
            quoted,
            has_subst: true,
            subst_quoting,
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

    // ── コマンド置換 span のトークナイズ (#266) ──

    #[test]
    fn command_subst_span_is_atomic() {
        // `echo $(echo a b)` は 2 トークン。span 内の空白で分断しない。
        let toks = split_quoted("echo $(echo a b)").unwrap();
        assert_eq!(
            toks,
            vec![
                t("echo", false),
                ts("$(echo a b)", false, SubstQuoting::Unquoted)
            ]
        );
    }

    #[test]
    fn backtick_span_is_atomic() {
        let toks = split_quoted("echo `echo a b`").unwrap();
        assert_eq!(
            toks,
            vec![
                t("echo", false),
                ts("`echo a b`", false, SubstQuoting::Unquoted)
            ]
        );
    }

    #[test]
    fn double_quoted_command_subst_is_double_quoted_context() {
        // `"$(echo a b)"` は quoted=true かつ DoubleQuoted 文脈。
        let toks = split_quoted("echo \"$(echo a b)\"").unwrap();
        assert_eq!(
            toks,
            vec![
                t("echo", false),
                ts("$(echo a b)", true, SubstQuoting::DoubleQuoted)
            ]
        );
    }

    #[test]
    fn single_quoted_command_subst_is_literal() {
        // シングルクォート内の `$(...)` はリテラル。has_subst は立たない。
        let toks = split_quoted("echo '$(echo X)'").unwrap();
        assert_eq!(toks, vec![t("echo", false), t("$(echo X)", true)]);
    }

    #[test]
    fn operator_inside_command_subst_not_split() {
        // span 内の `|` でトークン/演算子を切らない。
        let toks = split_quoted("echo $(a | b)").unwrap();
        assert_eq!(
            toks,
            vec![
                t("echo", false),
                ts("$(a | b)", false, SubstQuoting::Unquoted)
            ]
        );
    }

    #[test]
    fn nested_command_subst_span_is_atomic() {
        let toks = split_quoted("echo $(echo $(echo x))").unwrap();
        assert_eq!(
            toks,
            vec![
                t("echo", false),
                ts("$(echo $(echo x))", false, SubstQuoting::Unquoted)
            ]
        );
    }

    #[test]
    fn command_subst_embedded_in_word() {
        // `prefix-$(echo mid)-suffix` は 1 トークンで span を内包。
        let toks = split_quoted("echo prefix-$(echo mid)-suffix").unwrap();
        assert_eq!(
            toks,
            vec![
                t("echo", false),
                ts("prefix-$(echo mid)-suffix", false, SubstQuoting::Unquoted)
            ]
        );
    }

    #[test]
    fn unterminated_paren_substitution_errors() {
        assert_eq!(
            split_quoted("echo $(echo unclosed"),
            Err(SplitError::UnterminatedSubstitution)
        );
    }

    #[test]
    fn unterminated_backtick_substitution_errors() {
        assert_eq!(
            split_quoted("echo `echo unclosed"),
            Err(SplitError::UnterminatedSubstitution)
        );
    }

    #[test]
    fn plain_dollar_paren_not_treated_as_subst() {
        // `$VAR` は置換構文ではないので通常トークン（has_subst=false）。
        let toks = split_quoted("echo $VAR").unwrap();
        assert_eq!(toks, vec![t("echo", false), t("$VAR", false)]);
    }

    // ── operator_prefix_len / operator_at 整合性 (#Phase1 Task1.1) ──

    #[test]
    fn operator_prefix_len_matches_table() {
        // 演算子表が operator_prefix_len に一本化されたことのピン留め。
        assert_eq!(operator_prefix_len("&&"), 2);
        assert_eq!(operator_prefix_len("||"), 2);
        assert_eq!(operator_prefix_len(">>"), 2);
        assert_eq!(operator_prefix_len("|"), 1);
        assert_eq!(operator_prefix_len("<"), 1);
        assert_eq!(operator_prefix_len(">"), 1);
        assert_eq!(operator_prefix_len(";"), 1);
        assert_eq!(operator_prefix_len(""), 0);
        assert_eq!(operator_prefix_len("echo"), 0);
        assert_eq!(operator_prefix_len("&"), 0);
        assert_eq!(operator_prefix_len("|foo"), 1);
        assert_eq!(operator_prefix_len(">>foo"), 2);
    }

    // ── split_quoted_spans バイト range 検証 ──

    #[test]
    fn spans_cover_exact_source_substrings_unquoted() {
        let input = "echo hello world";
        let toks = split_quoted_spans(input).unwrap();
        for (tok, range) in &toks {
            assert_eq!(&input[range.clone()], tok.value.as_str());
        }
    }

    #[test]
    fn spans_include_surrounding_quotes() {
        let input = r#"echo "a b""#;
        let toks = split_quoted_spans(input).unwrap();
        assert_eq!(toks.len(), 2);
        let (second, range) = &toks[1];
        assert_eq!(second.value, "a b");
        assert_eq!(&input[range.clone()], r#""a b""#);
    }

    #[test]
    fn spans_cover_operators() {
        let input = "a && b || c ; d | e";
        let toks = split_quoted_spans(input).unwrap();
        for (tok, range) in &toks {
            assert_eq!(&input[range.clone()], tok.value.as_str());
        }
    }

    #[test]
    fn spans_cover_command_subst() {
        let input = "echo $(echo a b)";
        let toks = split_quoted_spans(input).unwrap();
        assert_eq!(toks.len(), 2);
        let (second, range) = &toks[1];
        assert_eq!(second.value, "$(echo a b)");
        assert_eq!(&input[range.clone()], "$(echo a b)");
    }

    #[test]
    fn spans_are_char_boundary_safe_for_multibyte() {
        // マルチバイト文字（日本語）を含む入力でも byte range が
        // 文字境界上に乗ることを確認する。
        let input = "grep 検索 | ll";
        let toks = split_quoted_spans(input).unwrap();
        let values: Vec<&str> = toks.iter().map(|(t, _)| t.value.as_str()).collect();
        assert_eq!(values, vec!["grep", "検索", "|", "ll"]);
        for (tok, range) in &toks {
            // スライスが char boundary からずれていればここで panic する。
            assert_eq!(&input[range.clone()], tok.value.as_str());
        }
    }

    #[test]
    fn split_quoted_delegates_to_spans() {
        // split_quoted は split_quoted_spans からトークンのみ取り出したもの。
        let input = "ls *.txt | head";
        let via_split: Vec<Token> = split_quoted(input).unwrap();
        let via_spans: Vec<Token> = split_quoted_spans(input)
            .unwrap()
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        assert_eq!(via_split, via_spans);
    }

    #[test]
    fn operator_prefix_len_pinned_against_operator_at() {
        // operator_at が operator_prefix_len へ委譲していることをプローブコーパスで確認。
        let probes = [
            "ls *.txt | head",
            "echo a >> file",
            "a && b || c ; d",
            "echo hello world",
            "echo $(a | b)",
            "cmd1 < in > out",
            "a|b",
            "a||b",
            "a&b",
            "a&&&b",
            ">>>",
            "",
            "   ",
            "echo 'a && b'",
        ];
        for probe in probes {
            let chars: Vec<char> = probe.chars().collect();
            for i in 0..=chars.len() {
                let via_at = operator_at(&chars, i);
                let tail: String = chars[i..].iter().collect();
                let via_prefix = operator_prefix_len(&tail);
                assert_eq!(
                    via_at, via_prefix,
                    "mismatch at probe={probe:?} i={i}: operator_at={via_at} operator_prefix_len={via_prefix}"
                );
            }
        }
    }
}
