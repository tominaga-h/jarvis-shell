//! グロブ展開
//!
//! `glob = "0.3"` クレートを薄くラップし、jarvish のシェル展開向けの
//! インタフェースを提供する。
//!
//! - 対応パターン: `*`, `?`, `[abc]`, `[a-z]`
//! - マッチしない場合は `Err(NoMatches)` を返す（呼び出し側で zsh 互換エラーを生成）
//! - メタ文字を含まないトークンには `Ok([token])` を返す

use glob::glob;

/// マッチ無しを示すエラー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoMatches(pub String);

/// トークンにグロブメタ文字が含まれるかを判定する。
///
/// 厳密な glob 構文判定ではなく、いずれかのメタ文字が現れたら true を返す
/// 安価なフィルタ。クォート/エスケープは shell-words 段で既に剥がされている
/// 前提のため、ここでは現れた文字をそのまま扱う。
pub fn has_glob_meta(token: &str) -> bool {
    token.contains('*') || token.contains('?') || contains_char_class(token)
}

/// `[...]` 形式の文字クラスらしき構造が含まれるかを判定する。
fn contains_char_class(token: &str) -> bool {
    let bytes = token.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            // 対応する `]` が同じトークン内に存在し、間に 1 文字以上ある場合のみ有効
            if let Some(close_off) = bytes[i + 1..].iter().position(|&b| b == b']') {
                if close_off > 0 {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

/// トークンに対してグロブ展開を適用する。
///
/// - メタ文字を含まないトークンは `Ok(vec![token])` を返す
/// - マッチがあれば結果を sorted 順で返す（`glob` クレート既定）
/// - マッチが無ければ `Err(NoMatches)`
pub fn expand_glob(token: &str) -> Result<Vec<String>, NoMatches> {
    if !has_glob_meta(token) {
        return Ok(vec![token.to_string()]);
    }

    // glob クレートはパターンエラー時に空イテレータを返す挙動ではなく
    // Err を返すため、pattern 自体のパース失敗時はリテラル扱いに fallback する。
    let iter = match glob(token) {
        Ok(it) => it,
        Err(_) => return Ok(vec![token.to_string()]),
    };

    let mut matches: Vec<String> = Vec::new();
    for entry in iter {
        match entry {
            Ok(path) => matches.push(path.to_string_lossy().into_owned()),
            Err(_) => {
                // IO エラーは個別にスキップ（権限エラー等）
                continue;
            }
        }
    }

    if matches.is_empty() {
        Err(NoMatches(token.to_string()))
    } else {
        Ok(matches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;
    use std::fs;
    use std::path::PathBuf;

    struct CwdGuard {
        original: PathBuf,
    }

    impl CwdGuard {
        fn new() -> Self {
            Self {
                original: env::current_dir().expect("failed to get current dir"),
            }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = env::set_current_dir(&self.original);
        }
    }

    #[test]
    fn has_glob_meta_detects_star() {
        assert!(has_glob_meta("*.txt"));
        assert!(has_glob_meta("foo*"));
    }

    #[test]
    fn has_glob_meta_detects_question_mark() {
        assert!(has_glob_meta("foo?.txt"));
    }

    #[test]
    fn has_glob_meta_detects_char_class() {
        assert!(has_glob_meta("[abc].txt"));
        assert!(has_glob_meta("foo[0-9]"));
    }

    #[test]
    fn has_glob_meta_ignores_plain_text() {
        assert!(!has_glob_meta("hello"));
        assert!(!has_glob_meta("/tmp/file.txt"));
        assert!(!has_glob_meta(""));
    }

    #[test]
    fn has_glob_meta_ignores_empty_brackets() {
        // 空 `[]` や閉じなしはメタ扱いしない
        assert!(!has_glob_meta("foo[]bar"));
        assert!(!has_glob_meta("foo[bar"));
    }

    #[test]
    fn expand_glob_passthrough_for_non_meta() {
        let result = expand_glob("/etc/hostname").unwrap();
        assert_eq!(result, vec!["/etc/hostname".to_string()]);
    }

    #[test]
    #[serial]
    fn expand_glob_star_matches_files() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        fs::write(dir.path().join("b.txt"), "").unwrap();
        fs::write(dir.path().join("c.md"), "").unwrap();

        let mut result = expand_glob("*.txt").unwrap();
        result.sort();
        assert_eq!(result, vec!["a.txt".to_string(), "b.txt".to_string()]);
    }

    #[test]
    #[serial]
    fn expand_glob_question_mark() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        fs::write(dir.path().join("ab.txt"), "").unwrap();

        let result = expand_glob("?.txt").unwrap();
        assert_eq!(result, vec!["a.txt".to_string()]);
    }

    #[test]
    #[serial]
    fn expand_glob_char_class() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        fs::write(dir.path().join("b.txt"), "").unwrap();
        fs::write(dir.path().join("c.txt"), "").unwrap();

        let mut result = expand_glob("[ab].txt").unwrap();
        result.sort();
        assert_eq!(result, vec!["a.txt".to_string(), "b.txt".to_string()]);
    }

    #[test]
    #[serial]
    fn expand_glob_char_range() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        fs::write(dir.path().join("b.txt"), "").unwrap();
        fs::write(dir.path().join("z.txt"), "").unwrap();

        let mut result = expand_glob("[a-b].txt").unwrap();
        result.sort();
        assert_eq!(result, vec!["a.txt".to_string(), "b.txt".to_string()]);
    }

    #[test]
    #[serial]
    fn expand_glob_no_match_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();
        let err = expand_glob("*.nonexistent_xyz").unwrap_err();
        assert_eq!(err, NoMatches("*.nonexistent_xyz".to_string()));
    }
}
