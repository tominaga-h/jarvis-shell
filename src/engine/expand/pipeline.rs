//! 展開パイプライン
//!
//! チルダ/環境変数展開 → ブレース展開 → グロブ展開の順に適用し、
//! 展開結果のベクタを返す。
//!
//! グロブ展開で 1 件もマッチしなければ `ExpandError::NoMatches` を返す
//! （zsh 互換）。呼び出し側は終了コード 1 でエラーメッセージを表示すること。

use super::basic::expand_token;
use super::brace::expand_braces;
use super::glob::{expand_glob, has_glob_meta, NoMatches};

/// 展開エラー
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpandError {
    /// グロブパターンがどのファイルにもマッチしなかった
    NoMatches(String),
}

impl From<NoMatches> for ExpandError {
    fn from(nm: NoMatches) -> Self {
        ExpandError::NoMatches(nm.0)
    }
}

/// トークンに対してチルダ/env → ブレース → グロブの順で展開を行う。
///
/// グロブ展開段で `NoMatches` が発生した場合、その時点で `Err` を返す
/// （ブレース展開で複数候補が出ても、ひとつでも no-match があれば失敗扱い）。
pub fn expand_token_globs(token: &str) -> Result<Vec<String>, ExpandError> {
    // 1. tilde + env
    let basic = expand_token(token);

    // 2. brace
    let after_brace = expand_braces(&basic);

    // 3. glob
    let mut results: Vec<String> = Vec::new();
    for piece in after_brace {
        if has_glob_meta(&piece) {
            let matches = expand_glob(&piece)?;
            results.extend(matches);
        } else {
            results.push(piece);
        }
    }

    Ok(results)
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
    fn plain_text_returns_single_element() {
        let result = expand_token_globs("hello").unwrap();
        assert_eq!(result, vec!["hello".to_string()]);
    }

    #[test]
    #[serial]
    fn tilde_then_brace() {
        // `~/{foo,bar}` → `$HOME/foo`, `$HOME/bar`
        let home = env::var("HOME").unwrap();
        let result = expand_token_globs("~/{foo,bar}").unwrap();
        assert_eq!(result, vec![format!("{home}/foo"), format!("{home}/bar")]);
    }

    #[test]
    #[serial]
    fn env_var_then_brace() {
        env::set_var("JARVISH_PIPELINE_TEST", "/tmp/jp");
        let result = expand_token_globs("$JARVISH_PIPELINE_TEST/{a,b}").unwrap();
        assert_eq!(
            result,
            vec!["/tmp/jp/a".to_string(), "/tmp/jp/b".to_string()]
        );
        env::remove_var("JARVISH_PIPELINE_TEST");
    }

    #[test]
    #[serial]
    fn brace_then_glob_combination() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        fs::write(dir.path().join("a.md"), "").unwrap();
        fs::write(dir.path().join("b.log"), "").unwrap();

        let mut result = expand_token_globs("*.{txt,md}").unwrap();
        result.sort();
        assert_eq!(result, vec!["a.md".to_string(), "a.txt".to_string()]);
    }

    #[test]
    #[serial]
    fn glob_no_match_propagates() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();

        let err = expand_token_globs("*.nonexistent_xyz").unwrap_err();
        assert_eq!(err, ExpandError::NoMatches("*.nonexistent_xyz".to_string()));
    }

    #[test]
    #[serial]
    fn partial_match_in_brace_glob_still_errors_on_missing() {
        // `*.{txt,nonexistent_xyz}` で `.txt` だけマッチし `.nonexistent_xyz` は無い場合、
        // 後者で NoMatches を返す（zsh 互換: ひとつでも no-match があれば失敗）
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();

        let err = expand_token_globs("*.{txt,nonexistent_xyz}").unwrap_err();
        match err {
            ExpandError::NoMatches(p) => assert_eq!(p, "*.nonexistent_xyz"),
        }
    }

    #[test]
    fn numeric_brace_range_alone() {
        let result = expand_token_globs("{1..3}").unwrap();
        assert_eq!(
            result,
            vec!["1".to_string(), "2".to_string(), "3".to_string()]
        );
    }
}
