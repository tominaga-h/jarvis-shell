//! 補完専用の寛容トークナイザ + `CompletionContext` 抽出
//!
//! [`crate::engine::expand::split_quoted`] は実行系専用であり、未閉クォートや
//! dangling backslash、未閉 `$(...)` を**エラー**として扱う（`shell/input.rs` の
//! 継続行入力がこれに依存する）。一方、補完は「入力途中の不完全な行」を
//! 常に相手にするため、同じ意味論を寛容化した別実装が必要になる
//! （両者の要求は正反対のため、`split_quoted` 自体を拡張しない）。
//!
//! このモジュールは `line[..pos]` を 1 パスで走査し、未閉クォート等があっても
//! 「開いていたトークンをそのまま確定する」ことで**絶対にエラーにしない**
//! 寛容スキャナ [`lex_lenient`] を提供する。演算子表は `quote.rs` の
//! [`crate::engine::expand::operator_prefix_len`] を共有し、実行系と補完系の
//! トークナイズが乖離しないようにする（乖離防止はパリティテストで担保）。

use reedline::Span;

use crate::engine::expand::operator_prefix_len;

/// 寛容スキャナが生成する 1 トークン。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LexToken {
    /// クォート・エスケープを剥がした後の値。
    pub value: String,
    /// 元の行における raw なバイト範囲の開始（クォート文字等を含む）。
    pub start: usize,
    /// 元の行における raw なバイト範囲の終了。
    pub end: usize,
    /// 演算子トークン（`|` `&&` `||` `;` `>` `>>` `<`）かどうか。
    pub is_operator: bool,
    /// シングル/ダブルクォートまたはバックスラッシュエスケープを
    /// 少なくとも一部含んでいた場合 true。
    pub quoted: bool,
}

/// Tab 補完のためにカーソル位置から抽出した文脈。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompletionContext {
    /// カーソルを含むパイプラインセグメントのトークン列（演算子含む）。
    /// カーソル位置に未確定の partial トークンがあれば最後の要素として含む。
    pub tokens: Vec<LexToken>,
    /// カーソル位置の partial トークン（クォート/エスケープ剥がし済み）。
    /// 末尾が空白の場合は空文字列。
    pub partial: String,
    /// 補完確定時に置き換えるべき、元の行における raw バイト範囲。
    pub span: Span,
    /// カーソルのトークンがセグメントの先頭（コマンド位置）かどうか。
    pub is_first_token: bool,
    /// alias 展開後の先頭コマンドの単語列。`extract_context` は常に `None`
    /// を返し、`JarvishCompleter::complete` が `apply_shell_alias`（`mod.rs`）
    /// でシェルエイリアス解決後に設定する。
    pub expanded_head: Option<Vec<String>>,
}

impl CompletionContext {
    /// コマンド判定に使う単語列を返す。
    ///
    /// `expanded_head` が `Some` ならその単語列を、`None` なら
    /// `tokens[0].value` を先頭として、以降の非演算子トークンの値を続ける。
    pub(crate) fn command_words(&self) -> Vec<&str> {
        if let Some(head) = &self.expanded_head {
            return head.iter().map(String::as_str).collect();
        }
        let mut words = Vec::new();
        let mut iter = self.tokens.iter();
        if let Some(first) = iter.next() {
            words.push(first.value.as_str());
        }
        for tok in iter {
            if !tok.is_operator {
                words.push(tok.value.as_str());
            }
        }
        words
    }

    /// セグメントの先頭コマンド名を返す（`command_words()` の最初の要素）。
    pub(crate) fn head_command(&self) -> Option<&str> {
        self.command_words().into_iter().next()
    }
}

/// 走査状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Normal,
    InSingle,
    InDouble,
}

/// `input` を寛容にトークナイズする。
///
/// 返り値の第 2 要素は、走査終了時点で未閉の `$(` があった場合に
/// その開始バイト位置（`$` の位置）を返す（`extract_context` の再帰用）。
/// 未閉クォート・dangling backslash・未閉 `$(` はいずれもエラーにせず、
/// 開いていたトークンをそのまま確定して返す。
// 走査終了直後の flush! でも tok_start/quoted をリセットするが、以後読まれない
// （関数を抜けるため）。ループ内の共有ロジックとして flush! を保つための代償。
#[allow(unused_assignments)]
fn lex_lenient(input: &str) -> (Vec<LexToken>, Option<usize>) {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut tokens: Vec<LexToken> = Vec::new();

    let mut state = State::Normal;
    let mut value = String::new();
    let mut tok_start: Option<usize> = None;
    let mut quoted = false;
    let mut unclosed_subst_start: Option<usize> = None;

    let chars: Vec<(usize, char)> = input.char_indices().collect();
    let mut idx = 0usize;

    macro_rules! flush {
        ($end:expr) => {{
            if let Some(start) = tok_start {
                tokens.push(LexToken {
                    value: std::mem::take(&mut value),
                    start,
                    end: $end,
                    is_operator: false,
                    quoted,
                });
            }
            tok_start = None;
            quoted = false;
        }};
    }

    while idx < chars.len() {
        let (byte_pos, ch) = chars[idx];

        match state {
            State::Normal => {
                if ch.is_whitespace() {
                    flush!(byte_pos);
                    idx += 1;
                    continue;
                }

                // 演算子判定（Normal かつクォート外のみ）
                let rest = &input[byte_pos..];
                let op_len = operator_prefix_len(rest);
                if op_len > 0 {
                    // 既存トークンを確定してから演算子を単独トークンとして追加。
                    flush!(byte_pos);
                    let op_end = byte_pos + op_len;
                    tokens.push(LexToken {
                        value: input[byte_pos..op_end].to_string(),
                        start: byte_pos,
                        end: op_end,
                        is_operator: true,
                        quoted: false,
                    });
                    // op_len はバイト長。対応する char 数だけ idx を進める。
                    let advanced_chars = input[byte_pos..op_end].chars().count();
                    idx += advanced_chars;
                    continue;
                }

                match ch {
                    '\'' => {
                        if tok_start.is_none() {
                            tok_start = Some(byte_pos);
                        }
                        quoted = true;
                        state = State::InSingle;
                        idx += 1;
                    }
                    '"' => {
                        if tok_start.is_none() {
                            tok_start = Some(byte_pos);
                        }
                        quoted = true;
                        state = State::InDouble;
                        idx += 1;
                    }
                    '$' if peek_char(&chars, idx + 1) == Some('(') => {
                        if tok_start.is_none() {
                            tok_start = Some(byte_pos);
                        }
                        match scan_paren_span_lenient(&chars, idx + 2, input, len) {
                            Some(end_byte) => {
                                value.push_str(&input[byte_pos..end_byte]);
                                let advanced_chars = input[byte_pos..end_byte].chars().count();
                                idx += advanced_chars;
                            }
                            None => {
                                // 未閉 $( : 呼び出し元 (extract_context) が再帰処理する。
                                unclosed_subst_start = Some(byte_pos);
                                value.push_str(&input[byte_pos..len]);
                                idx = chars.len();
                            }
                        }
                    }
                    '\\' => {
                        if tok_start.is_none() {
                            tok_start = Some(byte_pos);
                        }
                        quoted = true;
                        if let Some((_, next_ch)) = chars.get(idx + 1).copied() {
                            value.push(next_ch);
                            idx += 2;
                        } else {
                            // dangling backslash: 末尾。バックスラッシュ自体は破棄し、
                            // これ以上進めるものがないので走査終了。
                            idx += 1;
                        }
                    }
                    _ => {
                        if tok_start.is_none() {
                            tok_start = Some(byte_pos);
                        }
                        value.push(ch);
                        idx += 1;
                    }
                }
            }
            State::InSingle => {
                if ch == '\'' {
                    state = State::Normal;
                    idx += 1;
                } else {
                    value.push(ch);
                    idx += 1;
                }
            }
            State::InDouble => match ch {
                '"' => {
                    state = State::Normal;
                    idx += 1;
                }
                '\\' => match chars.get(idx + 1).copied() {
                    Some((_, next_ch)) if matches!(next_ch, '"' | '\\' | '$' | '`') => {
                        value.push(next_ch);
                        idx += 2;
                    }
                    Some((_, next_ch)) => {
                        // split_quoted と同じく、エスケープ対象外の文字は
                        // バックスラッシュ込みでリテラル化する。
                        value.push('\\');
                        value.push(next_ch);
                        idx += 2;
                    }
                    None => {
                        // dangling backslash（ダブルクォート内で末尾）。寛容に許容。
                        value.push('\\');
                        idx += 1;
                    }
                },
                '$' if peek_char(&chars, idx + 1) == Some('(') => {
                    match scan_paren_span_lenient(&chars, idx + 2, input, len) {
                        Some(end_byte) => {
                            value.push_str(&input[byte_pos..end_byte]);
                            let advanced_chars = input[byte_pos..end_byte].chars().count();
                            idx += advanced_chars;
                        }
                        None => {
                            unclosed_subst_start = Some(byte_pos);
                            value.push_str(&input[byte_pos..len]);
                            idx = chars.len();
                        }
                    }
                }
                _ => {
                    value.push(ch);
                    idx += 1;
                }
            },
        }
    }

    // 末尾での確定: 開いていたクォート/置換/トークンをそのまま flush する。
    // 未閉クォートでもエラーにしない（寛容スキャナの要）。
    flush!(len);

    (tokens, unclosed_subst_start)
}

/// `chars[idx]` があればその文字を返す（範囲外なら `None`）。
fn peek_char(chars: &[(usize, char)], idx: usize) -> Option<char> {
    chars.get(idx).map(|(_, c)| *c)
}

/// `$(` の直後（`chars` 上のインデックス `start_idx`）から括弧バランスで
/// 対応する `)` を探し、閉じ括弧の次のバイト位置を返す。
/// ネスト対応。閉じられていなければ `None`（寛容: エラーにしない）。
fn scan_paren_span_lenient(
    chars: &[(usize, char)],
    start_idx: usize,
    input: &str,
    input_len: usize,
) -> Option<usize> {
    let mut depth = 1usize;
    let mut i = start_idx;
    while i < chars.len() {
        let (_, ch) = chars[i];
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    // 次の文字の開始バイト位置（無ければ入力終端）を返す。
                    return Some(chars.get(i + 1).map(|(p, _)| *p).unwrap_or(input_len));
                }
            }
            _ => {}
        }
        i += 1;
    }
    let _ = input;
    None
}

/// `line` の `pos`（バイト位置）における補完文脈を抽出する。
///
/// `pos` が char 境界でない場合は安全側に floor する
/// （本番では常に `line.len()` が渡されるため防御的措置）。
pub(crate) fn extract_context(line: &str, pos: usize) -> CompletionContext {
    let pos = floor_to_char_boundary(line, pos);
    extract_context_inner(line, pos, 0)
}

/// `extract_context` の内部実装。`offset` は再帰時に加算するバイトオフセット
/// （`$( ... ` の中身を再帰抽出する際、内側の相対バイト位置に外側の
/// `$(` の中身開始位置を足し戻すために使う）。
fn extract_context_inner(line: &str, pos: usize, offset: usize) -> CompletionContext {
    let scanned = &line[..pos];
    let (tokens, unclosed_subst_start) = lex_lenient(scanned);

    if let Some(dollar_paren_start) = unclosed_subst_start {
        // 未閉 $( : 中身 (`$(` の 2 バイト分だけ先) を再帰的に抽出し、
        // オフセットを足し戻す。
        let inner_start = dollar_paren_start + 2; // "$(" の 2 バイト
        let inner = &line[inner_start..pos];
        let inner_pos = inner.len();
        let mut ctx = extract_context_inner(inner, inner_pos, offset + inner_start);
        // 中身の抽出結果は既に extract_context_inner 内で offset 加算済み。
        // is_first_token 等はそのまま中身のセグメントの判定を使う。
        // span, tokens の start/end は既に offset 込みなのでそのまま。
        ctx.expanded_head = None;
        return ctx;
    }

    build_context_from_tokens(tokens, pos, offset)
}

/// 寛容スキャナの生トークン列から `CompletionContext` を組み立てる。
///
/// `offset` は呼び出し元（`$(` 再帰）から渡される、raw バイト位置への
/// 加算量。トップレベル呼び出しでは 0。
fn build_context_from_tokens(
    tokens: Vec<LexToken>,
    pos: usize,
    offset: usize,
) -> CompletionContext {
    // セグメント切断: 最後の非演算子後方にある unquoted `| && || ;` 以降を
    // セグメントとして採用する。`> >> <` はセグメント境界にしない。
    let cut_index = tokens
        .iter()
        .rposition(|t| t.is_operator && is_segment_cut(&t.value))
        .map(|i| i + 1)
        .unwrap_or(0);

    let mut segment: Vec<LexToken> = tokens[cut_index..].to_vec();

    // trailing space 判定: scanned の末尾が空白（または空）なら、
    // 開いている partial トークンは存在しない → partial は空文字列。
    let ends_with_open_token = !segment.is_empty() && {
        let last = segment.last().unwrap();
        last.end == pos && !last.is_operator
    };

    let (partial_value, span_start) = if ends_with_open_token {
        let last = segment.pop().unwrap();
        (last.value, last.start)
    } else {
        (String::new(), pos)
    };

    // is_first_token: セグメント内に非演算子トークンが 1 つも残っていなければ
    // カーソルのトークンはコマンド位置。
    let is_first_token = !segment.iter().any(|t| !t.is_operator);

    // partial トークンを segment に戻す（仕様: tokens は「partial を含む」）。
    // trailing space の場合（ends_with_open_token = false）は
    // 開いているトークンが存在しないため何も追加しない。
    if ends_with_open_token {
        segment.push(LexToken {
            value: partial_value.clone(),
            start: span_start,
            end: pos,
            is_operator: false,
            quoted: false,
        });
    }

    let span = Span::new(span_start + offset, pos + offset);

    let shifted_tokens: Vec<LexToken> = segment
        .into_iter()
        .map(|t| LexToken {
            value: t.value,
            start: t.start + offset,
            end: t.end + offset,
            is_operator: t.is_operator,
            quoted: t.quoted,
        })
        .collect();

    CompletionContext {
        tokens: shifted_tokens,
        partial: partial_value,
        span,
        is_first_token,
        expanded_head: None,
    }
}

/// 演算子値がセグメント切断対象か（`| && || ;` のみ。リダイレクトは対象外）。
fn is_segment_cut(op_value: &str) -> bool {
    matches!(op_value, "|" | "&&" | "||" | ";")
}

/// `pos` が `s` の char 境界でなければ、境界内側（手前）に floor する。
fn floor_to_char_boundary(s: &str, pos: usize) -> usize {
    let mut p = pos.min(s.len());
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::expand::split_quoted;

    fn ctx(line: &str, pos: usize) -> CompletionContext {
        extract_context(line, pos)
    }

    fn tok_values(c: &CompletionContext) -> Vec<&str> {
        c.tokens.iter().map(|t| t.value.as_str()).collect()
    }

    // ── パイプライン切断 ──

    #[test]
    fn pipeline_cut_head_is_git() {
        let line = "ls | git checkout ";
        let c = ctx(line, line.len());
        assert_eq!(c.head_command(), Some("git"));
        assert_eq!(c.partial, "");
        assert!(!c.is_first_token);
        assert_eq!(tok_values(&c), vec!["git", "checkout"]);
    }

    #[test]
    fn and_or_semi_cut() {
        // `;` 以降のセグメントは `c ` = 先頭コマンド `c` が既に確定済みで
        // カーソルはその次のトークン位置（trailing space）にある。
        let line = "a && b; c ";
        let c = ctx(line, line.len());
        assert_eq!(c.head_command(), Some("c"));
        assert!(!c.is_first_token);
        assert_eq!(c.partial, "");
    }

    #[test]
    fn and_or_semi_cut_mid_command_is_first_token() {
        // カーソルがまだセグメント先頭トークンを打ち込んでいる途中の場合。
        let line = "a && b; c";
        let c = ctx(line, line.len());
        assert_eq!(c.partial, "c");
        assert!(c.is_first_token);
    }

    #[test]
    fn pipe_then_space_is_first_token() {
        let line = "ls | ";
        let c = ctx(line, line.len());
        assert!(c.is_first_token);
        assert_eq!(c.partial, "");
        assert!(c.tokens.is_empty());
    }

    #[test]
    fn pipe_immediately_before_cursor_no_space_is_first_token() {
        // 演算子の直後にカーソル（空白なし）。演算子自体は partial 扱いされない。
        let line = "ls |";
        let c = ctx(line, line.len());
        assert!(c.is_first_token);
        assert_eq!(c.partial, "");
        assert!(c.tokens.is_empty());
    }

    #[test]
    fn redirect_is_not_a_cut() {
        let line = "ls > out ";
        let c = ctx(line, line.len());
        assert_eq!(c.head_command(), Some("ls"));
        assert!(!c.is_first_token);
        assert_eq!(tok_values(&c), vec!["ls", ">", "out"]);
    }

    // ── クォート ──

    #[test]
    fn double_quote_partial_strips_quote_span_starts_at_quote() {
        let line = r#"echo "fo"#;
        let quote_byte = line.find('"').unwrap();
        let c = ctx(line, line.len());
        assert_eq!(c.partial, "fo");
        assert_eq!(c.span, Span::new(quote_byte, line.len()));
    }

    #[test]
    fn single_quote_partial_strips_quote() {
        let line = "echo 'fo";
        let quote_byte = line.find('\'').unwrap();
        let c = ctx(line, line.len());
        assert_eq!(c.partial, "fo");
        assert_eq!(c.span, Span::new(quote_byte, line.len()));
    }

    #[test]
    fn escaped_space_in_token_is_stripped_and_joined() {
        let line = r"foo\ bar";
        let c = ctx(line, line.len());
        assert_eq!(c.partial, "foo bar");
        assert_eq!(c.span, Span::new(0, line.len()));
    }

    #[test]
    fn closed_quote_then_space_trailing_partial_empty() {
        let line = r#"echo "foo" "#;
        let c = ctx(line, line.len());
        assert_eq!(c.partial, "");
        assert_eq!(c.span, Span::new(line.len(), line.len()));
        assert_eq!(tok_values(&c), vec!["echo", "foo"]);
        assert!(!c.is_first_token);
    }

    // ── trailing space vs escaped trailing space ──

    #[test]
    fn trailing_space_is_boundary() {
        let line = "git checkout ";
        let c = ctx(line, line.len());
        assert_eq!(c.partial, "");
        assert_eq!(c.span, Span::new(line.len(), line.len()));
        assert_eq!(tok_values(&c), vec!["git", "checkout"]);
    }

    #[test]
    fn escaped_trailing_space_is_not_a_boundary() {
        let line = r"foo\ ";
        let c = ctx(line, line.len());
        // "foo " (末尾スペース込み) が partial として継続する。
        assert_eq!(c.partial, "foo ");
        assert_eq!(c.span, Span::new(0, line.len()));
    }

    // ── is_first_token 特殊系 ──

    #[test]
    fn empty_line_is_first_token() {
        let c = ctx("", 0);
        assert!(c.is_first_token);
        assert_eq!(c.span, Span::new(0, 0));
        assert_eq!(c.partial, "");
    }

    // ── command_words / head_command ──

    #[test]
    fn command_words_skips_operators_and_partial_included() {
        let line = "ls | git checkout fo";
        let c = ctx(line, line.len());
        assert_eq!(c.command_words(), vec!["git", "checkout", "fo"]);
        assert_eq!(c.head_command(), Some("git"));
    }

    #[test]
    fn command_words_empty_tokens_returns_empty_and_head_none() {
        let line = "ls | ";
        let c = ctx(line, line.len());
        assert!(c.command_words().is_empty());
        assert_eq!(c.head_command(), None);
    }

    #[test]
    fn command_words_uses_expanded_head_when_present() {
        let mut c = ctx("g checkout fo", "g checkout fo".len());
        // Phase 1.5 が alias 展開結果を格納する想定のフィールドを直接セットして検証。
        c.expanded_head = Some(vec!["git".to_string(), "checkout".to_string()]);
        assert_eq!(c.command_words(), vec!["git", "checkout"]);
        assert_eq!(c.head_command(), Some("git"));
    }

    #[test]
    fn mid_line_cursor_ignores_trailing_content() {
        // pos < line.len() で、カーソル後方に実コンテンツがある場合。
        // "git checkout main && ls" の "che" の直後にカーソルを置く。
        let line = "git checkout main && ls";
        let pos = line.find("che").unwrap() + "che".len();
        let c = ctx(line, pos);

        // tokens/partial は line[..pos] ("git checkout main"[.."che"]) のみから
        // 導出され、カーソル以降の "ckout main && ls" は一切見えない。
        assert_eq!(c.partial, "che");
        assert_eq!(tok_values(&c), vec!["git", "che"]);
        assert_eq!(c.head_command(), Some("git"));
        assert!(!c.is_first_token);

        // span はちょうど pos で終わる（後方の "ckout" 等は含まれない）。
        assert_eq!(c.span, Span::new(pos - "che".len(), pos));
        assert_eq!(c.span.end, pos);
    }

    #[test]
    fn pos_zero_is_first_token() {
        let line = "git checkout";
        let c = ctx(line, 0);
        assert!(c.is_first_token);
        assert_eq!(c.span, Span::new(0, 0));
    }

    #[test]
    fn whitespace_only_line_is_first_token() {
        let line = "   ";
        let c = ctx(line, line.len());
        assert!(c.is_first_token);
        assert_eq!(c.partial, "");
        assert_eq!(c.span, Span::new(line.len(), line.len()));
    }

    // ── UTF-8 ──

    #[test]
    fn utf8_partial_exact_byte_start() {
        let line = "vim 日本語ファ";
        let c = ctx(line, line.len());
        assert_eq!(c.partial, "日本語ファ");
        assert_eq!(c.span, Span::new(4, line.len()));
        assert!(!c.is_first_token);
    }

    #[test]
    fn utf8_mid_multibyte_pos_floors_safely() {
        let line = "vim 日本語ファ";
        // '日' はバイト 4..7 を占める。境界でない 5 を渡す。
        let c = ctx(line, 5);
        // floor されるので char 境界内側（4）で切られる。
        assert_eq!(c.span.end, 4);
        assert_eq!(c.partial, "");
    }

    // ── $( ) ──

    #[test]
    fn closed_dollar_paren_is_atomic_single_token() {
        let line = "echo $(git checkout) ";
        let c = ctx(line, line.len());
        assert_eq!(tok_values(&c), vec!["echo", "$(git checkout)"]);
        assert!(!c.tokens[1].quoted);
        assert!(!c.tokens[1].is_operator);
    }

    #[test]
    fn unclosed_dollar_paren_recurses_with_shifted_offsets() {
        let line = "echo $(git checkout fo";
        let inner_start = line.find("git").unwrap();
        let c = ctx(line, line.len());
        assert_eq!(c.head_command(), Some("git"));
        assert_eq!(c.partial, "fo");
        assert_eq!(c.span, Span::new(line.len() - 2, line.len()));
        // tokens には partial ("fo") も最後の要素として含まれる。
        assert_eq!(tok_values(&c), vec!["git", "checkout", "fo"]);
        assert!(!c.is_first_token);
        // 全トークンのオフセットが inner_start を基準にシフトされていること。
        assert_eq!(c.tokens[0].start, inner_start);
        assert_eq!(c.tokens[0].end, inner_start + "git".len());
    }

    #[test]
    fn nested_unclosed_dollar_paren_recurses_via_recursion() {
        let line = "echo $(echo $(git checkout fo";
        let c = ctx(line, line.len());
        assert_eq!(c.head_command(), Some("git"));
        assert_eq!(c.partial, "fo");
    }

    #[test]
    fn unclosed_backtick_is_not_special_cased() {
        // backtick には専用のクォート状態を設けない（$( のような再帰処理をしない）。
        // 通常文字として扱われるため、後続の空白は通常どおりトークン境界になる。
        let line = "echo `git checkout fo";
        let c = ctx(line, line.len());
        assert!(!c.is_first_token);
        assert_eq!(c.partial, "fo");
        assert_eq!(c.head_command(), Some("echo"));
        assert_eq!(tok_values(&c), vec!["echo", "`git", "checkout", "fo"]);
    }

    // ── PARITY: 整形式コーパスで lex_lenient と split_quoted が一致 ──

    #[test]
    fn parity_with_split_quoted_on_well_formed_corpus() {
        let corpus = [
            "echo hello world",
            "ls -la /tmp",
            "git checkout -b feature/x",
            "echo 'a b' c",
            r#"echo "a b" c"#,
            "ls | grep foo",
            "a && b || c ; d",
            "echo a >> file",
            "cmd1 < in > out",
            "echo $(echo a b)",
            "echo prefix-$(echo mid)-suffix",
            "echo $(echo $(echo x))",
            r#"echo "$(echo a b)""#,
            "git commit -m 'msg here'",
            "ls -la | grep -i foo | wc -l",
        ];

        for line in corpus {
            let expected: Vec<String> = split_quoted(line)
                .unwrap_or_else(|e| panic!("split_quoted failed on {line:?}: {e}"))
                .into_iter()
                .map(|t| t.value)
                .collect();

            let (lenient_tokens, unclosed) = lex_lenient(line);
            assert!(
                unclosed.is_none(),
                "well-formed corpus line should not report unclosed $(: {line:?}"
            );
            let actual: Vec<String> = lenient_tokens.into_iter().map(|t| t.value).collect();

            assert_eq!(actual, expected, "parity mismatch for line {line:?}");
        }
    }
}
