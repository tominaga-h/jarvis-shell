//! コマンド補完 — Tab キーで PATH コマンド名・ビルトイン・ファイルパス・git ブランチを補完
//!
//! - 先頭トークン: PATH 内の実行可能コマンド + ビルトイン (cd, cwd, exit, ...)
//! - 先頭トークンがパスらしい場合 (`./` `../` `/` `~/`): ファイル / ディレクトリ補完
//! - `git <branch系サブコマンド>`: git ブランチ名補完
//! - それ以降: カレントディレクトリ基準のファイル / ディレクトリ名
//!
//! [`CompletionProvider`] トレイトで補完源をプラグイン化しており、
//! `complete()` は [`Command`](command::CommandProvider) →
//! [`Git`](git::GitProvider) → [`Path`](path::PathProvider) の順に
//! 各プロバイダを走査し、最初に `Some` を返したプロバイダの候補を採用する
//! （`None` = 対象外で次へ、`Some(vec![])` = 担当したが候補なしでそこで確定）。
//!
//! コマンド名補完は fish shell の設計思想に倣い、インメモリキャッシュを持たず、
//! Tab 押下時にリアルタイムで `$PATH` を走査する（キャッシュレス設計）。
//! `brew install` 等で新しいバイナリが追加された直後でも即座に補完候補に出現する。

mod command;
mod context;
mod git;
mod path;
mod provider;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use reedline::{Completer, Suggestion};

use command::CommandProvider;
use context::extract_context;
use git::GitProvider;
use path::PathProvider;
use provider::{escape_for_insert, CompletionProvider};

/// ColumnarMenu は description を持つ候補が 1 件でもあると全幅 1 カラムに
/// 描画が変わってしまうため、候補数がこの件数を超えたら description を
/// 一律で除去する（大きな PATH スキャン結果を守るガード）。
const DESCRIPTION_LIMIT: usize = 30;

/// Jarvish 用の補完エンジン
///
/// `$PATH` の走査はキャッシュレスだが、Git エイリアスの解決結果は
/// CWD ごとにインメモリキャッシュする（`includeIf` 等のディレクトリ依存設定に対応）。
///
/// `git_branch_commands` は `Shell` と共有され、`source` コマンドで動的に更新される。
pub struct JarvishCompleter {
    providers: Vec<Box<dyn CompletionProvider>>,
    /// シェルエイリアス（Shell と共有）
    #[allow(dead_code)] // TODO(Phase1 Task 1.5): alias-aware completion will use this
    aliases: Arc<RwLock<HashMap<String, String>>>,
}

impl JarvishCompleter {
    pub fn new(
        git_branch_commands: Arc<RwLock<Vec<String>>>,
        aliases: Arc<RwLock<HashMap<String, String>>>,
    ) -> Self {
        let providers: Vec<Box<dyn CompletionProvider>> = vec![
            Box::new(CommandProvider),
            Box::new(GitProvider::new(git_branch_commands)),
            Box::new(PathProvider),
        ];
        Self { providers, aliases }
    }
}

impl Completer for JarvishCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let ctx = extract_context(line, pos);

        let candidates = self
            .providers
            .iter()
            .find_map(|provider| provider.provide(&ctx))
            .unwrap_or_default();

        let strip_descriptions = candidates.len() > DESCRIPTION_LIMIT;

        candidates
            .into_iter()
            .map(|candidate| Suggestion {
                value: escape_for_insert(&candidate.value),
                description: if strip_descriptions {
                    None
                } else {
                    candidate.description
                },
                style: None,
                extra: None,
                span: ctx.span,
                append_whitespace: candidate.append_whitespace,
                match_indices: None,
            })
            .collect()
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
        JarvishCompleter::new(
            Arc::new(RwLock::new(commands)),
            Arc::new(RwLock::new(HashMap::new())),
        )
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

    // ── complete (Completer trait) 統合テスト（既存の回帰網。原文の意図・assertion を維持） ──

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

    // ── 新規テスト (Task 1.3) ──

    #[test]
    #[serial]
    fn complete_pipeline_git_checkout_includes_branches() {
        // 'ls | git checkout test-' でパイプ後のセグメントがブランチ補完される。
        let tmpdir = create_test_git_repo();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = "ls | git checkout test-";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"test-feature"),
            "pipeline segment should suggest 'test-feature': {values:?}"
        );
    }

    #[test]
    #[serial]
    fn complete_quoted_partial_completes_files() {
        // 'echo "fo' が一時ディレクトリ内の fo* ファイルを補完する。
        let tmpdir = tempfile::tempdir().unwrap();
        fs::write(tmpdir.path().join("foo.txt"), "").unwrap();
        fs::write(tmpdir.path().join("bar.txt"), "").unwrap();

        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = r#"echo "fo"#;
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"foo.txt"),
            "quoted partial 'fo' should suggest foo.txt: {values:?}"
        );
        assert!(
            !values.iter().any(|v| v.contains("bar")),
            "'fo' prefix should exclude bar.txt: {values:?}"
        );
    }

    #[test]
    #[serial]
    fn complete_file_with_space_is_escaped_on_insert() {
        // 'foo bar.txt' という名前のファイルはエスケープされた値で挿入される。
        let tmpdir = tempfile::tempdir().unwrap();
        fs::write(tmpdir.path().join("foo bar.txt"), "").unwrap();

        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = "cat foo";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&r"foo\ bar.txt"),
            "space in filename should be escaped: {values:?}"
        );
        assert!(
            !values.contains(&"foo bar.txt"),
            "unescaped raw value should not be inserted directly: {values:?}"
        );
    }

    #[test]
    fn complete_pipeline_cd_offers_dirs_only() {
        // 'ls | cd ' はディレクトリのみを候補に出す。
        let (_tmpdir, path) = create_test_tree();
        let mut completer = test_completer();
        let line = format!("ls | cd {path}/");
        let pos = line.len();

        let suggestions = completer.complete(&line, pos);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(values.contains(&format!("{path}/Documents/").as_str()));
        assert!(
            !values.iter().any(|v| v.contains("readme.txt")),
            "cd after pipe should only offer directories: {values:?}"
        );
    }

    #[test]
    fn complete_builtin_suggestions_carry_descriptions() {
        let mut completer = test_completer();
        let line = "cd";
        let pos = line.len();

        let suggestions = completer.complete(line, pos);

        let cd_suggestion = suggestions
            .iter()
            .find(|s| s.value == "cd")
            .expect("'cd' builtin should be suggested");
        assert!(
            cd_suggestion.description.is_some(),
            "builtin 'cd' suggestion should carry a description"
        );
    }

    #[test]
    fn complete_large_candidate_set_strips_descriptions() {
        // DESCRIPTION_LIMIT を超える候補数になる場面では description が全除去される。
        let tmpdir = tempfile::tempdir().unwrap();
        for i in 0..(DESCRIPTION_LIMIT + 5) {
            fs::write(tmpdir.path().join(format!("file{i}.txt")), "").unwrap();
        }

        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = "ls file";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        assert!(
            suggestions.len() > DESCRIPTION_LIMIT,
            "test setup should exceed DESCRIPTION_LIMIT: {}",
            suggestions.len()
        );
        assert!(
            suggestions.iter().all(|s| s.description.is_none()),
            "descriptions should be stripped when candidate count exceeds the limit"
        );
    }
}
