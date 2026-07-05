//! コマンド補完 — Tab キーで PATH コマンド名・ビルトイン・ファイルパスを補完
//!
//! - 先頭トークン: PATH 内の実行可能コマンド + ビルトイン (cd, cwd, exit)
//! - 先頭トークンがパスらしい場合 (`./` `../` `/` `~/`): ファイル / ディレクトリ補完
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
use std::sync::{Arc, RwLock};

use reedline::{Completer, Span, Suggestion};

/// Jarvish 用の補完エンジン
///
/// `$PATH` の走査はキャッシュレスだが、Git エイリアスの解決結果は
/// CWD ごとにインメモリキャッシュする（`includeIf` 等のディレクトリ依存設定に対応）。
///
/// `git_branch_commands` は `Shell` と共有され、`source` コマンドで動的に更新される。
pub struct JarvishCompleter {
    /// CWD ごとの Git エイリアスマップ: `{ CWD: { "co": "checkout", "b": "branch", ... } }`
    git_aliases_cache: RwLock<HashMap<PathBuf, HashMap<String, String>>>,
    /// ブランチ名補完を提供する git サブコマンド（config.toml で設定可能）
    pub(super) git_branch_commands: Arc<RwLock<Vec<String>>>,
}

impl JarvishCompleter {
    pub fn new(git_branch_commands: Arc<RwLock<Vec<String>>>) -> Self {
        Self {
            git_aliases_cache: RwLock::new(HashMap::new()),
            git_branch_commands,
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

    /// 先頭トークンがパスらしいかを判定する。
    ///
    /// `/` を含む (`./target/debug/`, `bin/foo`, `/usr/bin/ls`, `~/bin/x`)、
    /// または `~` で始まる (`~` 単体もホーム基準) 場合にファイル補完へ回す。
    fn looks_like_path(token: &str) -> bool {
        token.contains('/') || token.starts_with('~')
    }
}

impl Completer for JarvishCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let start = Self::token_start(line, pos);
        let partial = &line[start..pos];
        let span = Span::new(start, pos);

        if Self::is_first_token(line, pos) {
            if Self::looks_like_path(partial) {
                self.complete_path(partial, span, false)
            } else {
                self.complete_command(partial, span)
            }
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

    use crate::config::CompletionConfig;

    fn test_completer() -> JarvishCompleter {
        let commands = CompletionConfig::default().git_branch_commands;
        JarvishCompleter::new(Arc::new(RwLock::new(commands)))
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

    // ── looks_like_path テスト ──

    #[test]
    fn looks_like_path_true_cases() {
        for token in [
            "./",
            "../",
            "./target/debug/",
            "/usr/bin/ls",
            "~/",
            "~",
            "sub/foo",
        ] {
            assert!(
                JarvishCompleter::looks_like_path(token),
                "'{token}' should look like a path"
            );
        }
    }

    #[test]
    fn looks_like_path_false_cases() {
        for token in ["ls", "cargo", "git", ""] {
            assert!(
                !JarvishCompleter::looks_like_path(token),
                "'{token}' should not look like a path"
            );
        }
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

    // ── 先頭トークンがパスのときファイル補完へ回る統合テスト (#321) ──

    #[test]
    fn complete_first_token_absolute_path_shows_entries() {
        let (_tmpdir, path) = create_test_tree();
        let mut completer = test_completer();
        let line = format!("{path}/");
        let pos = line.len();

        let suggestions = completer.complete(&line, pos);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        // dirs_only=false の証明: ディレクトリとファイルの両方が候補に出る
        assert!(
            values.contains(&format!("{path}/Documents/").as_str()),
            "should include Documents/ dir: {values:?}"
        );
        assert!(
            values.contains(&format!("{path}/readme.txt").as_str()),
            "should include readme.txt file: {values:?}"
        );
    }

    #[test]
    #[serial]
    fn complete_first_token_relative_path_shows_entries() {
        let (tmpdir, _path) = create_test_tree();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = "./";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(!suggestions.is_empty(), "./ should produce suggestions");
        assert!(
            values.iter().any(|v| v.contains("Documents/")),
            "./ should include Documents/: {values:?}"
        );
    }

    #[test]
    #[serial]
    fn complete_first_token_tilde_expands_home() {
        let mut completer = test_completer();
        let line = "~/";
        let pos = line.len();

        let suggestions = completer.complete(line, pos);

        assert!(!suggestions.is_empty(), "~/ should produce suggestions");
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
    fn complete_first_token_plain_command_uses_path() {
        let (tmpdir, _path) = create_test_tree();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = "c";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        // コマンド補完に流れている証明: ビルトインが出る
        assert!(
            values.contains(&"cd"),
            "plain 'c' should suggest builtin 'cd': {values:?}"
        );
        assert!(
            values.contains(&"cwd"),
            "plain 'c' should suggest builtin 'cwd': {values:?}"
        );
        // ファイル補完へは流れていない: temp ディレクトリのファイルは出ない
        assert!(
            !values.iter().any(|v| v.contains("readme.txt")),
            "plain 'c' should not suggest files from CWD: {values:?}"
        );
    }

    #[test]
    #[serial]
    fn complete_first_token_parent_relative_path_shows_entries() {
        let (tmpdir, _path) = create_test_tree();
        // tree ルート直下の Documents へ cd し、`../` で親（tree ルート）を補完する
        let subdir = tmpdir.path().join("Documents");
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(&subdir).unwrap();

        let mut completer = test_completer();
        let line = "../";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(!suggestions.is_empty(), "../ should produce suggestions");
        assert!(
            values.iter().any(|v| v.contains("Desktop/")),
            "../ from Documents should include sibling Desktop/: {values:?}"
        );
    }

    #[test]
    fn complete_first_token_mid_slash_prefix_filters() {
        let (_tmpdir, path) = create_test_tree();
        let mut completer = test_completer();
        // 絶対パス + 中間スラッシュ + 末尾プレフィックス `Do` → 前方一致フィルタ
        let line = format!("{path}/Do");
        let pos = line.len();

        let suggestions = completer.complete(&line, pos);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&format!("{path}/Documents/").as_str()),
            "should include Documents/: {values:?}"
        );
        assert!(
            values.contains(&format!("{path}/Downloads/").as_str()),
            "should include Downloads/: {values:?}"
        );
        assert!(
            !values.iter().any(|v| v.contains("Desktop")),
            "'Do' prefix should exclude Desktop: {values:?}"
        );
        assert!(
            !values.iter().any(|v| v.contains("readme.txt")),
            "'Do' prefix should exclude readme.txt: {values:?}"
        );
    }
}
