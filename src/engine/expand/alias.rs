//! エイリアス展開
//!
//! 入力行を `|` `&&` `||` `;` で区切られたセグメントに分解し、
//! 各セグメントの先頭トークン（コマンド名の位置）がエイリアスに
//! 一致すれば対応するコマンド文字列に置換する。
//!
//! `>` `>>` `<` はセグメント境界ではない（リダイレクトはパイプライン/
//! コネクタの区切りではないため、次のトークンをエイリアス展開の対象に
//! しない）。

use std::collections::HashMap;

use super::quote::{operator_prefix_len, split_quoted_spans};

/// セグメント境界となる演算子（パイプ/コネクタ）。
/// `>` `>>` `<`（リダイレクト）は含まない。
fn is_segment_boundary(value: &str) -> bool {
    matches!(value, "|" | "&&" | "||" | ";")
}

/// 値がトークナイザの演算子表に一致する（＝制御演算子トークンである）か。
fn is_operator_token(value: &str) -> bool {
    operator_prefix_len(value) == value.len()
}

/// 入力行の各パイプライン/コネクタセグメントの先頭トークンがエイリアスに
/// 一致する場合、それぞれ対応するコマンド文字列に置換した行を返す。
///
/// 例: aliases = {"grep": "rg"}, input = "cat x | grep y"
///     → "cat x | rg y"
///
/// - セグメント境界: `|`, `&&`, `||`, `;`（`>`, `>>`, `<` は境界にならない）
/// - クォートされたトークンおよびコマンド置換を含むトークンは
///   セグメント先頭であってもエイリアス展開しない（bash と同様）
/// - 展開は one-shot（置換後の値を再展開しない）
///
/// どのセグメント先頭も一致しない場合、または展開不要な場合は `None`。
pub fn expand_aliases_in_line(input: &str, aliases: &HashMap<String, String>) -> Option<String> {
    if aliases.is_empty() {
        return None;
    }

    let toks = match split_quoted_spans(input) {
        Ok(toks) => toks,
        Err(_) => return None,
    };

    // (byte_range, alias_value) のリスト。開始位置順（トークン出現順）に積む。
    let mut replacements: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    let mut at_segment_head = true;

    for (tok, range) in &toks {
        // 演算子トークンは quoted/has_subst を持たない（トークナイザが
        // 制御演算子を専用トークンとして分離するため）ので、head 判定より
        // 先にこの分岐で処理される。境界演算子なら次トークンが head、
        // リダイレクト演算子（`>` 等）なら次トークンは head ではない。
        if is_operator_token(&tok.value) {
            at_segment_head = is_segment_boundary(&tok.value);
            continue;
        }

        if at_segment_head && !tok.quoted && !tok.has_subst {
            if let Some(replacement) = aliases.get(&tok.value) {
                replacements.push((range.clone(), replacement.clone()));
            }
        }
        at_segment_head = false;
    }

    if replacements.is_empty() {
        return None;
    }

    let mut result = String::with_capacity(input.len());
    let mut cursor = 0usize;
    for (range, replacement) in &replacements {
        result.push_str(&input[cursor..range.start]);
        result.push_str(replacement);
        cursor = range.end;
    }
    result.push_str(&input[cursor..]);

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aliases_basic() -> HashMap<String, String> {
        let mut aliases = HashMap::new();
        aliases.insert("grep".to_string(), "rg".to_string());
        aliases.insert("ll".to_string(), "ls -la".to_string());
        aliases.insert("g".to_string(), "git".to_string());
        aliases
    }

    #[test]
    fn alias_expands_single_token() {
        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), "git".to_string());
        assert_eq!(expand_aliases_in_line("g", &aliases).unwrap(), "git");
    }

    #[test]
    fn alias_expands_with_args() {
        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), "git".to_string());
        assert_eq!(
            expand_aliases_in_line("g status", &aliases).unwrap(),
            "git status"
        );
    }

    #[test]
    fn alias_expands_multi_word_value() {
        let mut aliases = HashMap::new();
        aliases.insert("ll".to_string(), "ls -la".to_string());
        assert_eq!(
            expand_aliases_in_line("ll /tmp", &aliases).unwrap(),
            "ls -la /tmp"
        );
    }

    #[test]
    fn alias_returns_none_for_no_match() {
        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), "git".to_string());
        assert!(expand_aliases_in_line("echo hello", &aliases).is_none());
    }

    #[test]
    fn alias_returns_none_for_empty_aliases() {
        let aliases = HashMap::new();
        assert!(expand_aliases_in_line("g status", &aliases).is_none());
    }

    #[test]
    fn alias_returns_none_for_empty_input() {
        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), "git".to_string());
        assert!(expand_aliases_in_line("", &aliases).is_none());
    }

    // ── セグメント境界を跨いだエイリアス展開 ──

    #[test]
    fn pipe_second_segment_expands() {
        let aliases = aliases_basic();
        assert_eq!(
            expand_aliases_in_line("cat x | grep y", &aliases).unwrap(),
            "cat x | rg y"
        );
    }

    #[test]
    fn both_segments_expand() {
        let aliases = aliases_basic();
        assert_eq!(
            expand_aliases_in_line("g log | grep y", &aliases).unwrap(),
            "git log | rg y"
        );
    }

    #[test]
    fn and_operator_second_segment_expands() {
        let aliases = aliases_basic();
        assert_eq!(
            expand_aliases_in_line("g add && grep y", &aliases).unwrap(),
            "git add && rg y"
        );
    }

    #[test]
    fn or_operator_second_segment_expands() {
        let aliases = aliases_basic();
        assert_eq!(
            expand_aliases_in_line("false || grep y", &aliases).unwrap(),
            "false || rg y"
        );
    }

    #[test]
    fn semicolon_second_segment_expands() {
        let aliases = aliases_basic();
        assert_eq!(
            expand_aliases_in_line("echo a ; grep b", &aliases).unwrap(),
            "echo a ; rg b"
        );
    }

    #[test]
    fn quoted_pipe_is_not_a_boundary() {
        let aliases = aliases_basic();
        assert!(expand_aliases_in_line("echo \"a|b\"", &aliases).is_none());
    }

    #[test]
    fn head_alias_with_quoted_arg_preserved() {
        let aliases = aliases_basic();
        assert_eq!(
            expand_aliases_in_line("grep \"a|b\"", &aliases).unwrap(),
            "rg \"a|b\""
        );
    }

    #[test]
    fn pipe_inside_command_subst_is_not_a_boundary() {
        let aliases = aliases_basic();
        assert!(expand_aliases_in_line("echo $(cat | grep y)", &aliases).is_none());
    }

    #[test]
    fn alias_only_second_segment() {
        let aliases = aliases_basic();
        assert_eq!(
            expand_aliases_in_line("ls | grep z", &aliases).unwrap(),
            "ls | rg z"
        );
    }

    #[test]
    fn multi_word_value_with_second_segment() {
        let aliases = aliases_basic();
        assert_eq!(
            expand_aliases_in_line("ll /tmp | grep y", &aliases).unwrap(),
            "ls -la /tmp | rg y"
        );
    }

    #[test]
    fn redirect_is_not_a_segment_boundary() {
        let aliases = aliases_basic();
        assert_eq!(
            expand_aliases_in_line("grep y > out", &aliases).unwrap(),
            "rg y > out"
        );
    }

    #[test]
    fn redirect_target_is_not_a_head() {
        let aliases = aliases_basic();
        assert!(expand_aliases_in_line("cat a > grep", &aliases).is_none());
    }

    #[test]
    fn no_alias_passthrough() {
        let aliases = aliases_basic();
        assert!(expand_aliases_in_line("cat f | sort", &aliases).is_none());
    }

    #[test]
    fn whitespace_only_input_returns_none() {
        let aliases = aliases_basic();
        assert!(expand_aliases_in_line("   ", &aliases).is_none());
    }

    #[test]
    fn syntax_error_passthrough() {
        let aliases = aliases_basic();
        assert!(expand_aliases_in_line("echo 'unclosed", &aliases).is_none());
    }

    #[test]
    fn quoted_head_not_expanded() {
        let aliases = aliases_basic();
        assert!(expand_aliases_in_line("\"grep\" x", &aliases).is_none());
    }

    #[test]
    fn self_reference_one_shot() {
        let mut aliases = HashMap::new();
        aliases.insert("grep".to_string(), "grep --color".to_string());
        assert_eq!(
            expand_aliases_in_line("grep x", &aliases).unwrap(),
            "grep --color x"
        );
    }

    #[test]
    fn head_value_is_another_alias_no_reexpand() {
        let mut aliases = HashMap::new();
        aliases.insert("a".to_string(), "grep".to_string());
        aliases.insert("grep".to_string(), "rg".to_string());
        assert_eq!(expand_aliases_in_line("a x", &aliases).unwrap(), "grep x");
    }

    #[test]
    fn operator_in_alias_value_is_bash_compliant() {
        let mut aliases = HashMap::new();
        aliases.insert("x".to_string(), "a | b".to_string());
        assert_eq!(
            expand_aliases_in_line("x | c", &aliases).unwrap(),
            "a | b | c"
        );
    }

    #[test]
    fn multibyte_round_trip() {
        let aliases = aliases_basic();
        assert_eq!(
            expand_aliases_in_line("grep 検索 | ll", &aliases).unwrap(),
            "rg 検索 | ls -la"
        );
    }

    #[test]
    fn leading_whitespace_is_handled() {
        let aliases = aliases_basic();
        assert_eq!(
            expand_aliases_in_line("  grep y", &aliases).unwrap(),
            "  rg y"
        );
    }
}
