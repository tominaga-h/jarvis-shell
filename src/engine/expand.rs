//! シェル展開機能
//!
//! - チルダ展開: `~` → `$HOME`
//! - 環境変数展開: `$VAR`, `${VAR}`

use std::env;

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
            Ok(home) => format!("{}/{}", home, rest),
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
