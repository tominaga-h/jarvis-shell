//! シェル展開機能
//!
//! - エイリアス展開: 先頭トークンがエイリアスなら対応するコマンド文字列に置換
//! - チルダ展開: `~` → `$HOME`
//! - 環境変数展開: `$VAR`, `${VAR}`

use std::collections::HashMap;
use std::env;

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

/// トークンに対してシェル展開を適用する
pub fn expand_token(token: &str) -> String {
    let expanded = expand_tilde(token);
    expand_env_vars(&expanded)
}

/// チルダ展開: `~` を `$HOME` に置き換える
fn expand_tilde(path: &str) -> String {
    if path == "~" {
        // `~` のみの場合
        env::var("HOME").unwrap_or_else(|_| "~".to_string())
    } else if let Some(rest) = path.strip_prefix("~/") {
        // `~/...` の場合
        match env::var("HOME") {
            Ok(home) => format!("{home}/{rest}"),
            Err(_) => path.to_string(),
        }
    } else {
        path.to_string()
    }
}

/// 環境変数展開: `$VAR` や `${VAR}` を展開する
fn expand_env_vars(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' {
            // `${VAR}` 形式
            if chars.peek() == Some(&'{') {
                chars.next(); // '{' をスキップ
                let var_name: String = chars.by_ref().take_while(|&ch| ch != '}').collect();
                if let Ok(value) = env::var(&var_name) {
                    result.push_str(&value);
                }
            } else {
                // `$VAR` 形式: 英数字とアンダースコアのみ
                let mut var_name = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_ascii_alphanumeric() || ch == '_' {
                        var_name.push(ch);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if !var_name.is_empty() {
                    if let Ok(value) = env::var(&var_name) {
                        result.push_str(&value);
                    }
                } else {
                    result.push('$');
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    // ── エイリアス展開テスト ──

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

    // ── チルダ展開テスト ──

    #[test]
    fn expand_tilde_only() {
        let home = env::var("HOME").unwrap();
        assert_eq!(expand_tilde("~"), home);
    }

    #[test]
    fn expand_tilde_with_subpath() {
        let home = env::var("HOME").unwrap();
        assert_eq!(expand_tilde("~/foo/bar"), format!("{}/foo/bar", home));
    }

    #[test]
    fn expand_tilde_no_expansion_for_other_paths() {
        assert_eq!(expand_tilde("/tmp/test"), "/tmp/test");
        assert_eq!(expand_tilde("relative/path"), "relative/path");
    }

    #[test]
    fn expand_env_var_simple() {
        env::set_var("JARVISH_TEST_VAR", "testvalue");
        assert_eq!(expand_env_vars("$JARVISH_TEST_VAR"), "testvalue");
        env::remove_var("JARVISH_TEST_VAR");
    }

    #[test]
    fn expand_env_var_braces() {
        env::set_var("JARVISH_TEST_VAR2", "bracevalue");
        assert_eq!(expand_env_vars("${JARVISH_TEST_VAR2}"), "bracevalue");
        env::remove_var("JARVISH_TEST_VAR2");
    }

    #[test]
    fn expand_env_var_braces_with_trailing_path() {
        // `${VAR}/path` 形式で閉じブレースの後に文字が続くケースを検証
        env::set_var("JARVISH_TEST_VAR3", "/home/user");
        assert_eq!(
            expand_env_vars("${JARVISH_TEST_VAR3}/file"),
            "/home/user/file"
        );
        env::remove_var("JARVISH_TEST_VAR3");
    }

    #[test]
    fn expand_env_var_in_path() {
        let home = env::var("HOME").unwrap();
        assert_eq!(expand_env_vars("$HOME/foo"), format!("{}/foo", home));
    }

    #[test]
    fn expand_token_combines_both() {
        let home = env::var("HOME").unwrap();
        env::set_var("JARVISH_SUBDIR", "testdir");
        // ~/foo と $VAR 両方が展開される
        assert_eq!(expand_token("~/foo"), format!("{}/foo", home));
        assert_eq!(expand_token("$HOME/bar"), format!("{}/bar", home));
        env::remove_var("JARVISH_SUBDIR");
    }
}
