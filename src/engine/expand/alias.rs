//! エイリアス展開
//!
//! 入力行の先頭トークンがエイリアスに一致する場合、対応するコマンド
//! 文字列に置換する。

use std::collections::HashMap;

/// 入力行の先頭トークンがエイリアスに一致する場合、展開した文字列を返す。
///
/// エイリアスの値は先頭トークンのみを置き換える。
/// 例: aliases = {"g": "git"}, input = "g status" → "git status"
///
/// 一致しない場合は `None` を返す。
pub fn expand_alias(input: &str, aliases: &HashMap<String, String>) -> Option<String> {
    if aliases.is_empty() {
        return None;
    }

    let trimmed = input.trim_start();
    // 先頭トークン（空白またはEOLまで）を取得
    let first_end = trimmed
        .find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(trimmed.len());
    let first_token = &trimmed[..first_end];

    aliases.get(first_token).map(|replacement| {
        let rest = &trimmed[first_end..];
        format!("{replacement}{rest}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_expands_single_token() {
        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), "git".to_string());
        assert_eq!(expand_alias("g", &aliases).unwrap(), "git");
    }

    #[test]
    fn alias_expands_with_args() {
        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), "git".to_string());
        assert_eq!(expand_alias("g status", &aliases).unwrap(), "git status");
    }

    #[test]
    fn alias_expands_multi_word_value() {
        let mut aliases = HashMap::new();
        aliases.insert("ll".to_string(), "ls -la".to_string());
        assert_eq!(expand_alias("ll /tmp", &aliases).unwrap(), "ls -la /tmp");
    }

    #[test]
    fn alias_returns_none_for_no_match() {
        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), "git".to_string());
        assert!(expand_alias("echo hello", &aliases).is_none());
    }

    #[test]
    fn alias_returns_none_for_empty_aliases() {
        let aliases = HashMap::new();
        assert!(expand_alias("g status", &aliases).is_none());
    }

    #[test]
    fn alias_returns_none_for_empty_input() {
        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), "git".to_string());
        assert!(expand_alias("", &aliases).is_none());
    }
}
