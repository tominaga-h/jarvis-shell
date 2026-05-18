//! ブレース展開
//!
//! bash/zsh 互換のブレース展開を提供する。
//!
//! - `{a,b,c}` → `a b c`
//! - `{1..5}` → `1 2 3 4 5`
//! - `{01..03}` → `01 02 03`（ゼロパディング保持）
//! - `{5..1}` → `5 4 3 2 1`（降順）
//! - `{1..10..2}` → `1 3 5 7 9`（ステップ）
//! - `{a..e}` → `a b c d e`（単一文字レンジ）
//! - ネスト: `{a,b{1,2}}` → `a b1 b2`
//! - エスケープ: `\{` `\,` `\}` はリテラル扱い
//! - 不正/単要素 `{a}`, `{` → そのまま返す（bash 互換）

/// トークンに対してブレース展開を適用し、展開結果の配列を返す。
///
/// 展開不要なトークンは長さ 1 の配列 `[token]` を返す。
pub fn expand_braces(token: &str) -> Vec<String> {
    let parsed = parse_segment(token);
    let mut results: Vec<String> = Vec::new();
    expand_into(&parsed, &mut String::new(), &mut results);
    if results.is_empty() {
        vec![token.to_string()]
    } else {
        results
    }
}

/// ブレース展開後のセグメント表現。
#[derive(Debug, Clone)]
enum Segment {
    /// リテラル文字列
    Literal(String),
    /// 選択（`{a,b,c}` 形式）。各分岐はさらにセグメント列を持てる
    Alternation(Vec<Vec<Segment>>),
    /// 数値レンジ `{N..M}` または `{N..M..S}`
    NumericRange {
        start: i64,
        end: i64,
        step: i64,
        width: usize,
    },
    /// 単一文字レンジ `{a..z}`
    CharRange { start: char, end: char },
}

/// 文字列をセグメント列にパースする。
fn parse_segment(input: &str) -> Vec<Segment> {
    let chars: Vec<char> = input.chars().collect();
    let (segments, _) = parse_until(&chars, 0, None);
    segments
}

/// `chars[start..]` を消費しながらセグメント列を組み立てる。
///
/// `stop` が `Some(c)` の場合、`c` に到達したらそこで終了し、消費位置を返す。
/// `stop` が `None` の場合は文字列末尾まで読む。
fn parse_until(chars: &[char], start: usize, stop: Option<char>) -> (Vec<Segment>, usize) {
    let mut segments: Vec<Segment> = Vec::new();
    let mut literal = String::new();
    let mut i = start;

    while i < chars.len() {
        let c = chars[i];

        // 終端文字に到達
        if let Some(s) = stop {
            if c == s {
                if !literal.is_empty() {
                    segments.push(Segment::Literal(std::mem::take(&mut literal)));
                }
                return (segments, i);
            }
        }

        // エスケープ: ブレース展開のメタ文字 (`{`, `}`, `,`, `\`) のみ対象
        if c == '\\' && i + 1 < chars.len() {
            let next = chars[i + 1];
            if matches!(next, '{' | '}' | ',' | '\\') {
                literal.push(next);
                i += 2;
                continue;
            }
            // それ以外は `\` も残す（外部コマンドが解釈する余地を残す）
            literal.push(c);
            i += 1;
            continue;
        }

        // ブレース開始
        if c == '{' {
            if let Some((seg, next)) = try_parse_brace(chars, i) {
                if !literal.is_empty() {
                    segments.push(Segment::Literal(std::mem::take(&mut literal)));
                }
                segments.push(seg);
                i = next;
                continue;
            }
            // ブレースとして解釈できない場合はリテラル扱い
            literal.push(c);
            i += 1;
            continue;
        }

        literal.push(c);
        i += 1;
    }

    if !literal.is_empty() {
        segments.push(Segment::Literal(std::mem::take(&mut literal)));
    }
    (segments, i)
}

/// `chars[start]` が `{` のとき、対応するブレース構造をパースする。
///
/// パースに成功すれば `(Segment, next_index)` を返す。
/// 不正な構造（閉じなし、単要素など）の場合は `None` を返し、呼び出し側でリテラル扱いとする。
fn try_parse_brace(chars: &[char], start: usize) -> Option<(Segment, usize)> {
    debug_assert_eq!(chars[start], '{');

    // 対応する閉じ `}` を探す（ネストとエスケープ考慮）
    let close = find_matching_brace(chars, start)?;
    let inner: String = chars[start + 1..close].iter().collect();

    // レンジ判定 (`..` を含み、トップレベルにカンマがないこと)
    if !contains_top_level_comma(&inner) {
        if let Some(seg) = try_parse_range(&inner) {
            return Some((seg, close + 1));
        }
        // 単要素 `{a}` 等は展開しない → None で呼び出し側にリテラル扱いさせる
        return None;
    }

    // カンマ区切りの選択を分割
    let parts = split_top_level_commas(&inner);
    if parts.len() < 2 {
        return None;
    }

    // 各分岐を再帰的にパース
    let alternatives: Vec<Vec<Segment>> = parts.into_iter().map(|p| parse_segment(&p)).collect();

    Some((Segment::Alternation(alternatives), close + 1))
}

/// `chars[start]` の `{` に対応する `}` の位置を返す。
/// ネストとエスケープを考慮する。
fn find_matching_brace(chars: &[char], start: usize) -> Option<usize> {
    debug_assert_eq!(chars[start], '{');
    let mut depth = 0;
    let mut i = start;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() && matches!(chars[i + 1], '{' | '}' | ',' | '\\') {
            i += 2;
            continue;
        }
        if c == '{' {
            depth += 1;
        } else if c == '}' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// 入力文字列のトップレベル（ネストされていない位置）にカンマがあるか判定。
fn contains_top_level_comma(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let mut depth = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() && matches!(chars[i + 1], '{' | '}' | ',' | '\\') {
            i += 2;
            continue;
        }
        if c == '{' {
            depth += 1;
        } else if c == '}' {
            depth -= 1;
        } else if c == ',' && depth == 0 {
            return true;
        }
        i += 1;
    }
    false
}

/// トップレベルのカンマで分割する。エスケープとネストを尊重する。
fn split_top_level_commas(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut result: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() && matches!(chars[i + 1], '{' | '}' | ',' | '\\') {
            current.push(c);
            current.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '{' {
            depth += 1;
            current.push(c);
        } else if c == '}' {
            depth -= 1;
            current.push(c);
        } else if c == ',' && depth == 0 {
            result.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
        i += 1;
    }
    result.push(current);
    result
}

/// `{N..M}` `{N..M..S}` `{a..z}` 形式のレンジをパースする。
/// 形式が違えば `None`。
fn try_parse_range(inner: &str) -> Option<Segment> {
    let parts: Vec<&str> = inner.split("..").collect();
    if parts.len() != 2 && parts.len() != 3 {
        return None;
    }
    let start_str = parts[0];
    let end_str = parts[1];
    let step_str = parts.get(2).copied();

    // 数値レンジ
    if let (Ok(start_n), Ok(end_n)) = (start_str.parse::<i64>(), end_str.parse::<i64>()) {
        let step: i64 = match step_str {
            None => {
                if start_n <= end_n {
                    1
                } else {
                    -1
                }
            }
            Some(s) => {
                let parsed: i64 = s.parse().ok()?;
                if parsed == 0 {
                    return None;
                }
                // 方向を自動補正（bash 互換: 絶対値を方向に適用）
                let abs = parsed.abs();
                if start_n <= end_n {
                    abs
                } else {
                    -abs
                }
            }
        };

        // ゼロパディング幅: 先頭が `0` または `-0` で始まる場合に保持
        let width = compute_pad_width(start_str, end_str);

        return Some(Segment::NumericRange {
            start: start_n,
            end: end_n,
            step,
            width,
        });
    }

    // 単一文字レンジ
    if step_str.is_none()
        && start_str.chars().count() == 1
        && end_str.chars().count() == 1
        && start_str.is_ascii()
        && end_str.is_ascii()
    {
        let s = start_str.chars().next().unwrap();
        let e = end_str.chars().next().unwrap();
        if s.is_ascii_alphabetic() && e.is_ascii_alphabetic() {
            return Some(Segment::CharRange { start: s, end: e });
        }
    }

    None
}

/// ゼロパディング幅を計算する。
///
/// bash の挙動: 端点いずれかが `0` または `-0` で始まる場合（かつ 1 文字目以降にも桁がある場合）
/// に、両端のうち最大幅でパディングする。
fn compute_pad_width(start_str: &str, end_str: &str) -> usize {
    fn padded(s: &str) -> bool {
        let bytes = s.as_bytes();
        if bytes.is_empty() {
            return false;
        }
        if bytes[0] == b'0' && bytes.len() > 1 {
            return true;
        }
        if bytes[0] == b'-' && bytes.len() > 2 && bytes[1] == b'0' {
            return true;
        }
        false
    }
    if padded(start_str) || padded(end_str) {
        // 符号を除いた数字部分の長さの最大値を採用
        let len = |s: &str| -> usize {
            if let Some(rest) = s.strip_prefix('-') {
                rest.len()
            } else {
                s.len()
            }
        };
        len(start_str).max(len(end_str))
    } else {
        0
    }
}

/// セグメント列を展開して `results` に push する。
fn expand_into(segments: &[Segment], prefix: &mut String, results: &mut Vec<String>) {
    if segments.is_empty() {
        results.push(prefix.clone());
        return;
    }
    let (head, tail) = segments.split_first().unwrap();
    match head {
        Segment::Literal(s) => {
            let len = prefix.len();
            prefix.push_str(s);
            expand_into(tail, prefix, results);
            prefix.truncate(len);
        }
        Segment::Alternation(alts) => {
            for alt in alts {
                let len = prefix.len();
                // 一時的に各分岐を展開しつつ tail も同時展開
                let mut inner_results: Vec<String> = Vec::new();
                let mut inner_prefix = String::new();
                expand_into(alt, &mut inner_prefix, &mut inner_results);
                for piece in inner_results {
                    prefix.push_str(&piece);
                    expand_into(tail, prefix, results);
                    prefix.truncate(len);
                }
            }
        }
        Segment::NumericRange {
            start,
            end,
            step,
            width,
        } => {
            let mut n = *start;
            let step = *step;
            loop {
                // 範囲外なら停止（step が end を跨ぐ場合に対応）
                if (step > 0 && n > *end) || (step < 0 && n < *end) {
                    break;
                }
                let formatted = if *width > 0 {
                    if n < 0 {
                        format!("-{:0>width$}", n.unsigned_abs(), width = *width)
                    } else {
                        format!("{:0>width$}", n, width = *width)
                    }
                } else {
                    n.to_string()
                };
                let len = prefix.len();
                prefix.push_str(&formatted);
                expand_into(tail, prefix, results);
                prefix.truncate(len);
                n += step;
            }
        }
        Segment::CharRange { start, end } => {
            let s = *start as u32;
            let e = *end as u32;
            let (lo, hi, reverse) = if s <= e { (s, e, false) } else { (e, s, true) };
            let mut chars: Vec<char> = (lo..=hi).filter_map(char::from_u32).collect();
            if reverse {
                chars.reverse();
            }
            for ch in chars {
                let len = prefix.len();
                prefix.push(ch);
                expand_into(tail, prefix, results);
                prefix.truncate(len);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_comma_list() {
        assert_eq!(expand_braces("{a,b,c}"), vec!["a", "b", "c"]);
    }

    #[test]
    fn comma_list_with_prefix_suffix() {
        assert_eq!(expand_braces("pre{a,b}post"), vec!["preapost", "prebpost"]);
    }

    #[test]
    fn numeric_range_ascending() {
        assert_eq!(expand_braces("{1..5}"), vec!["1", "2", "3", "4", "5"]);
    }

    #[test]
    fn numeric_range_descending() {
        assert_eq!(expand_braces("{5..1}"), vec!["5", "4", "3", "2", "1"]);
    }

    #[test]
    fn numeric_range_with_step() {
        assert_eq!(expand_braces("{1..10..2}"), vec!["1", "3", "5", "7", "9"]);
    }

    #[test]
    fn numeric_range_with_step_descending() {
        assert_eq!(expand_braces("{10..1..2}"), vec!["10", "8", "6", "4", "2"]);
    }

    #[test]
    fn numeric_range_zero_padded() {
        assert_eq!(expand_braces("{01..03}"), vec!["01", "02", "03"]);
    }

    #[test]
    fn numeric_range_zero_padded_wider() {
        // 端点の最大幅でパディングされる
        assert_eq!(
            expand_braces("{08..12}"),
            vec!["08", "09", "10", "11", "12"]
        );
    }

    #[test]
    fn char_range_ascending() {
        assert_eq!(expand_braces("{a..e}"), vec!["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn char_range_descending() {
        assert_eq!(expand_braces("{e..a}"), vec!["e", "d", "c", "b", "a"]);
    }

    #[test]
    fn nested_brace_at_end() {
        // `{a,b{1,2}}` → `a b1 b2`
        assert_eq!(expand_braces("{a,b{1,2}}"), vec!["a", "b1", "b2"]);
    }

    #[test]
    fn nested_combination_with_suffix() {
        // `{a,b}{1,2}` → 4 通り
        assert_eq!(expand_braces("{a,b}{1,2}"), vec!["a1", "a2", "b1", "b2"]);
    }

    #[test]
    fn escape_brace_literal() {
        // `\{a,b\}` → リテラルとして展開しない
        assert_eq!(expand_braces("\\{a,b\\}"), vec!["{a,b}"]);
    }

    #[test]
    fn escape_comma_inside_brace() {
        // `{a\,b,c}` → "a,b" と "c" の 2 要素
        assert_eq!(expand_braces("{a\\,b,c}"), vec!["a,b", "c"]);
    }

    #[test]
    fn single_element_no_expansion() {
        // 単要素 `{a}` は展開しない
        assert_eq!(expand_braces("{a}"), vec!["{a}"]);
    }

    #[test]
    fn unmatched_brace_is_literal() {
        assert_eq!(expand_braces("{"), vec!["{"]);
        assert_eq!(expand_braces("{a,b"), vec!["{a,b"]);
    }

    #[test]
    fn plain_text_passthrough() {
        assert_eq!(expand_braces("hello"), vec!["hello"]);
        assert_eq!(expand_braces(""), vec![""]);
    }

    #[test]
    fn numeric_range_negative() {
        assert_eq!(expand_braces("{-2..2}"), vec!["-2", "-1", "0", "1", "2"]);
    }

    #[test]
    fn nested_with_outer_suffix() {
        // `{a,{b,c}d}` → ["a", "bd", "cd"]
        assert_eq!(expand_braces("{a,{b,c}d}"), vec!["a", "bd", "cd"]);
    }
}
