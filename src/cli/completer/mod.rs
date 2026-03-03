//! コマンド補完 — Tab キーで PATH コマンド名・ビルトイン・ファイルパスを補完
//!
//! - 先頭トークン: PATH 内の実行可能コマンド + ビルトイン (cd, cwd, exit)
//! - それ以降: カレントディレクトリ基準のファイル / ディレクトリ名
//!
//! fish shell の設計思想に倣い、インメモリキャッシュを持たず、
//! Tab 押下時にリアルタイムで `$PATH` を走査する（キャッシュレス設計）。
//! `brew install` 等で新しいバイナリが追加された直後でも即座に補完候補に出現する。

mod command;
mod git;
mod path;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

use reedline::{Completer, Span, Suggestion};

/// Jarvish 用の補完エンジン
///
/// `$PATH` の走査はキャッシュレスだが、Git エイリアスの解決結果は
/// CWD ごとにインメモリキャッシュする（`includeIf` 等のディレクトリ依存設定に対応）。
pub struct JarvishCompleter {
    /// CWD ごとの Git エイリアスマップ: `{ CWD: { "co": "checkout", "b": "branch", ... } }`
    git_aliases_cache: RwLock<HashMap<PathBuf, HashMap<String, String>>>,
}

impl JarvishCompleter {
    pub fn new() -> Self {
        Self {
            git_aliases_cache: RwLock::new(HashMap::new()),
        }
    }

    /// カーソルより前の文字列から、補完対象トークンの開始位置を返す。
    fn token_start(line: &str, pos: usize) -> usize {
        let before = &line[..pos];
        before.rfind(' ').map(|i| i + 1).unwrap_or(0)
    }

    /// カーソルが先頭トークン上にあるかを判定する。
    fn is_first_token(line: &str, pos: usize) -> bool {
        !line[..pos].contains(' ')
    }
}

impl Completer for JarvishCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let start = Self::token_start(line, pos);
        let partial = &line[start..pos];
        let span = Span::new(start, pos);

        if Self::is_first_token(line, pos) {
            self.complete_command(partial, span)
        } else {
            let tokens: Vec<&str> = line[..pos].split_whitespace().collect();

            if let Some(git_suggestions) = self.try_complete_git(&tokens, partial, span) {
                return git_suggestions;
            }

            let first_token = tokens.first().copied().unwrap_or("");
            let dirs_only = first_token == "cd";
            self.complete_path(partial, span, dirs_only)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;
    use std::fs;

    fn test_completer() -> JarvishCompleter {
        JarvishCompleter::new()
    }

    fn create_test_tree() -> (tempfile::TempDir, String) {
        let tmpdir = tempfile::tempdir().expect("failed to create tempdir");
        let base = tmpdir.path();

        fs::create_dir(base.join("Documents")).unwrap();
        fs::create_dir(base.join("Desktop")).unwrap();
        fs::create_dir(base.join("Downloads")).unwrap();
        fs::create_dir(base.join(".hidden_dir")).unwrap();

        fs::write(base.join("readme.txt"), "").unwrap();
        fs::write(base.join(".dotfile"), "").unwrap();

        let path = base.to_str().unwrap().to_string();
        (tmpdir, path)
    }

    fn create_test_git_repo() -> tempfile::TempDir {
        use std::process::Command;

        let tmpdir = tempfile::tempdir().unwrap();
        let dir = tmpdir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["branch", "test-feature"])
            .current_dir(dir)
            .output()
            .unwrap();

        tmpdir
    }

    fn create_test_git_repo_with_aliases() -> tempfile::TempDir {
        use std::process::Command;

        let tmpdir = create_test_git_repo();
        let dir = tmpdir.path();

        Command::new("git")
            .args(["config", "alias.co", "checkout"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "alias.nb", "checkout -b"])
            .current_dir(dir)
            .output()
            .unwrap();

        tmpdir
    }

    // ── ヘルパーメソッドテスト ──

    #[test]
    fn token_start_no_space() {
        assert_eq!(JarvishCompleter::token_start("ls", 2), 0);
    }

    #[test]
    fn token_start_after_command() {
        assert_eq!(JarvishCompleter::token_start("cd /tmp", 7), 3);
    }

    #[test]
    fn is_first_token_true() {
        assert!(JarvishCompleter::is_first_token("ls", 2));
    }

    #[test]
    fn is_first_token_false() {
        assert!(!JarvishCompleter::is_first_token("cd /tmp", 7));
    }

    // ── complete (Completer trait) 統合テスト ──

    #[test]
    fn complete_cd_dirs_only_via_trait() {
        let (_tmpdir, path) = create_test_tree();
        let mut completer = test_completer();
        let line = format!("cd {path}/");
        let pos = line.len();

        let suggestions = completer.complete(&line, pos);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(values.contains(&format!("{path}/Documents/").as_str()));
        assert!(!values.iter().any(|v| v.contains("readme.txt")));
    }

    #[test]
    fn complete_ls_shows_files_and_dirs() {
        let (_tmpdir, path) = create_test_tree();
        let mut completer = test_completer();
        let line = format!("ls {path}/");
        let pos = line.len();

        let suggestions = completer.complete(&line, pos);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(values.contains(&format!("{path}/Documents/").as_str()));
        assert!(values.contains(&format!("{path}/readme.txt").as_str()));
    }

    #[test]
    #[serial]
    fn complete_tilde_alone_expands_home() {
        let mut completer = test_completer();
        let line = "cd ~";
        let pos = line.len();

        let suggestions = completer.complete(line, pos);

        assert!(!suggestions.is_empty(), "cd ~ should produce suggestions");
        for s in &suggestions {
            assert!(
                s.value.starts_with("~/"),
                "suggestion '{}' should start with ~/",
                s.value
            );
        }
    }

    #[test]
    #[serial]
    fn complete_tilde_slash_expands_home() {
        let mut completer = test_completer();
        let line = "cd ~/";
        let pos = line.len();

        let suggestions = completer.complete(line, pos);

        assert!(!suggestions.is_empty(), "cd ~/ should produce suggestions");
        for s in &suggestions {
            assert!(
                s.value.starts_with("~/"),
                "suggestion '{}' should start with ~/",
                s.value
            );
        }
    }

    #[test]
    #[serial]
    fn complete_git_checkout_includes_branches() {
        let tmpdir = create_test_git_repo();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = "git checkout test-";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"test-feature"),
            "git checkout should suggest 'test-feature': {values:?}"
        );
    }

    #[test]
    fn complete_git_non_branch_subcommand_no_branches() {
        let mut completer = test_completer();
        let line = "git add zzz_no_such_";
        let pos = line.len();

        let suggestions = completer.complete(line, pos);
        assert!(
            suggestions.is_empty(),
            "git add should not suggest anything for nonexistent prefix: {suggestions:?}"
        );
    }

    #[test]
    #[serial]
    fn complete_git_alias_triggers_branch_completion() {
        let tmpdir = create_test_git_repo_with_aliases();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = "git co test-";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"test-feature"),
            "git co (alias for checkout) should suggest 'test-feature': {values:?}"
        );
    }

    #[test]
    #[serial]
    fn complete_git_multi_word_alias_triggers_branch_completion() {
        let tmpdir = create_test_git_repo_with_aliases();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = "git nb test-";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"test-feature"),
            "git nb (alias for checkout -b) should suggest 'test-feature': {values:?}"
        );
    }
}
