//! 展開パイプライン
//!
//! コマンド置換 → チルダ/環境変数展開 → ブレース展開 → グロブ展開の順に適用し、
//! 展開結果のベクタを返す。
//!
//! コマンド置換段を最初に置くことで、置換結果テキストに対して
//! 後続の basic/brace/glob が適用される。`$(...)` の中の `$VAR` 等は
//! 置換実行時（サブシェル側）に展開されるため、ここで二重展開はしない。
//!
//! グロブ展開で 1 件もマッチしなければ `ExpandError::NoMatches` を返す
//! （zsh 互換）。呼び出し側は終了コード 1 でエラーメッセージを表示すること。

use super::basic::expand_token;
use super::brace::expand_braces;
use super::command_subst::{expand_command_subst, CmdSubstError, SubstQuoting};
use super::glob::{expand_glob, has_glob_meta, NoMatches};

/// 展開エラー
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpandError {
    /// グロブパターンがどのファイルにもマッチしなかった
    NoMatches(String),
    /// コマンド置換の実行・パースに失敗した
    Substitution(String),
}

impl From<NoMatches> for ExpandError {
    fn from(nm: NoMatches) -> Self {
        ExpandError::NoMatches(nm.0)
    }
}

impl From<CmdSubstError> for ExpandError {
    fn from(e: CmdSubstError) -> Self {
        ExpandError::Substitution(e.to_string())
    }
}

impl std::fmt::Display for ExpandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExpandError::NoMatches(p) => write!(f, "no matches found: {p}"),
            ExpandError::Substitution(msg) => write!(f, "{msg}"),
        }
    }
}

/// トークンに対してコマンド置換 → チルダ/env → ブレース → グロブの順で展開を行う。
///
/// コマンド置換のクォート文脈は [`SubstQuoting::Unquoted`] 固定。
/// ダブルクォート内のトークンは [`expand_token_globs_with_quoting`] を使うこと。
pub fn expand_token_globs(token: &str) -> Result<Vec<String>, ExpandError> {
    expand_token_globs_with_quoting(token, SubstQuoting::Unquoted)
}

/// [`expand_token_globs`] のコマンド置換クォート文脈指定版。
///
/// `q` はトークン内のコマンド置換 span に適用する文脈
/// （[`SubstQuoting::Unquoted`] なら結果を単語分割、
/// [`SubstQuoting::DoubleQuoted`] なら分割しない）。
pub fn expand_token_globs_with_quoting(
    token: &str,
    q: SubstQuoting,
) -> Result<Vec<String>, ExpandError> {
    // 0. command substitution（最初に適用）
    let words = expand_command_subst(token, q)?;

    // 各置換結果語に対して basic → brace → glob を適用して flatten する。
    let mut results: Vec<String> = Vec::new();
    for word in words {
        results.extend(expand_basic_brace_glob(&word)?);
    }

    Ok(results)
}

/// トークンに対してコマンド置換のみを適用する（basic/brace/glob は行わない）。
///
/// ダブルクォート内に置換 span を含むトークン（例: `"[$(...)]"`）用。
/// 引用符内のリテラル文字（`[`, `*` 等）をグロブ/ブレースとして解釈させない
/// ため、置換結果を含むテキストをそのまま返す（bash 準拠）。
pub fn expand_token_subst_only(token: &str, q: SubstQuoting) -> Result<Vec<String>, ExpandError> {
    Ok(expand_command_subst(token, q)?)
}

/// 単一の語に対してチルダ/env → ブレース → グロブの順で展開を行う。
fn expand_basic_brace_glob(token: &str) -> Result<Vec<String>, ExpandError> {
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
            other => panic!("expected NoMatches, got {other:?}"),
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

    // ── コマンド置換 → 後続展開の統合 (#266) ──

    #[test]
    fn command_subst_then_word_split() {
        let result = expand_token_globs("$(echo a b c)").unwrap();
        assert_eq!(
            result,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn command_subst_double_quoted_no_split() {
        let result =
            expand_token_globs_with_quoting("$(printf 'a   b')", SubstQuoting::DoubleQuoted)
                .unwrap();
        assert_eq!(result, vec!["a   b".to_string()]);
    }

    #[test]
    fn command_subst_nested_resolves_via_pipeline() {
        let result = expand_token_globs("$(echo $(echo deep))").unwrap();
        assert_eq!(result, vec!["deep".to_string()]);
    }

    #[test]
    fn command_subst_error_maps_to_substitution() {
        let err = expand_token_globs("$(false)").unwrap_err();
        assert!(matches!(err, ExpandError::Substitution(_)));
    }

    #[test]
    #[serial]
    fn command_subst_result_then_glob() {
        // 置換結果テキストにグロブメタが含まれる場合、glob 段が適用される。
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        fs::write(dir.path().join("b.txt"), "").unwrap();

        // `$(printf '*.txt')` → `*.txt` → glob → a.txt b.txt
        let mut result = expand_token_globs("$(printf '*.txt')").unwrap();
        result.sort();
        assert_eq!(result, vec!["a.txt".to_string(), "b.txt".to_string()]);
    }
}
