//! Git 補完 — ブランチ名補完 + エイリアス解決（CWD キャッシュ付き）

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use super::context::CompletionContext;
use super::provider::{Candidate, CompletionProvider};

/// カレントブランチ名を `git2` 経由で取得する。Git リポジトリ外では `None`。
fn current_branch() -> Option<String> {
    let repo = git2::Repository::discover(".").ok()?;
    let head = repo.head().ok()?;
    head.shorthand().map(str::to_string)
}

/// Git ブランチ名補完プロバイダ。
///
/// 先頭コマンドが `git` かつサブコマンドが `git_branch_commands`（または
/// その git エイリアス解決結果）に含まれる場合のみ `Some` を返す。
/// それ以外は `None`（対象外、次のプロバイダへ）。
pub(super) struct GitProvider {
    /// ブランチ名補完を提供する git サブコマンド（config.toml で設定可能）
    pub(super) git_branch_commands: Arc<RwLock<Vec<String>>>,
    /// CWD ごとの Git エイリアスマップ: `{ CWD: { "co": "checkout", "b": "branch", ... } }`
    pub(super) git_aliases_cache: RwLock<HashMap<PathBuf, HashMap<String, String>>>,
}

impl GitProvider {
    pub(super) fn new(git_branch_commands: Arc<RwLock<Vec<String>>>) -> Self {
        Self {
            git_branch_commands,
            git_aliases_cache: RwLock::new(HashMap::new()),
        }
    }
}

impl CompletionProvider for GitProvider {
    fn provide(&self, ctx: &CompletionContext) -> Option<Vec<Candidate>> {
        let words = ctx.command_words();
        if words.first().copied() != Some("git") || words.len() < 2 {
            return None;
        }

        let subcmd = words[1];

        let commands = self.git_branch_commands.read().ok()?;

        if commands.iter().any(|c| c == subcmd) {
            return Some(complete_git_branch(&ctx.partial));
        }

        if let Some(resolved) = self.resolve_git_alias(subcmd) {
            let main_cmd = resolved.split_whitespace().next().unwrap_or("");
            if commands.iter().any(|c| c == main_cmd) {
                return Some(complete_git_branch(&ctx.partial));
            }
        }

        None
    }
}

impl GitProvider {
    /// Git エイリアスを解決する（CWD ごとの遅延評価キャッシュ付き）。
    pub(super) fn resolve_git_alias(&self, alias: &str) -> Option<String> {
        let cwd = std::env::current_dir().ok()?;

        if let Ok(cache) = self.git_aliases_cache.read() {
            if let Some(aliases) = cache.get(&cwd) {
                return aliases.get(alias).cloned();
            }
        }

        let aliases_map = fetch_git_aliases();
        let result = aliases_map.get(alias).cloned();

        if let Ok(mut cache) = self.git_aliases_cache.write() {
            cache.insert(cwd, aliases_map);
        }

        result
    }
}

/// Git ブランチ名補完
///
/// `git branch --format=%(refname:short)` を実行してローカルブランチ一覧を取得し、
/// `partial` に前方一致するものを候補として返す。
fn complete_git_branch(partial: &str) -> Vec<Candidate> {
    let output = match std::process::Command::new("git")
        .args(["branch", "--format=%(refname:short)"])
        .stderr(std::process::Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };

    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut branches: Vec<&str> = stdout.lines().filter(|b| b.starts_with(partial)).collect();

    branches.sort_unstable();
    branches.dedup();

    if let Some(ref current) = current_branch() {
        if let Some(pos) = branches.iter().position(|b| *b == current.as_str()) {
            let branch = branches.remove(pos);
            branches.insert(0, branch);
        }
    }

    branches
        .into_iter()
        .map(|branch| Candidate {
            value: branch.to_string(),
            description: None,
            append_whitespace: true,
        })
        .collect()
}

/// `git config --get-regexp '^alias\.'` を実行し、エイリアスマップを構築する。
fn fetch_git_aliases() -> HashMap<String, String> {
    let output = match std::process::Command::new("git")
        .args(["config", "--get-regexp", "^alias\\."])
        .stderr(std::process::Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return HashMap::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut map = HashMap::new();

    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("alias.") {
            if let Some((name, value)) = rest.split_once(' ') {
                map.insert(name.to_string(), value.to_string());
            }
        }
    }

    map
}

#[cfg(test)]
mod tests {
    use std::env;

    use serial_test::serial;

    use super::*;
    use crate::config::CompletionConfig;

    fn test_provider() -> GitProvider {
        let commands = CompletionConfig::default().git_branch_commands;
        GitProvider::new(Arc::new(RwLock::new(commands)))
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

    #[test]
    #[serial]
    fn complete_git_branch_returns_candidates() {
        let tmpdir = create_test_git_repo();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let candidates = complete_git_branch("");

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(
            values.contains(&"test-feature"),
            "test-feature branch should be in suggestions: {values:?}"
        );
    }

    #[test]
    #[serial]
    fn complete_git_branch_filters_by_prefix() {
        let tmpdir = create_test_git_repo();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let candidates = complete_git_branch("test-");

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"test-feature"));
        for v in &values {
            assert!(v.starts_with("test-"), "'{v}' should start with 'test-'");
        }
    }

    #[test]
    fn complete_git_branch_nonexistent_prefix_returns_empty() {
        let candidates = complete_git_branch("zzz_no_such_branch_");
        assert!(candidates.is_empty());
    }

    #[test]
    #[serial]
    fn resolve_git_alias_returns_target() {
        let tmpdir = create_test_git_repo_with_aliases();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let provider = test_provider();
        let result = provider.resolve_git_alias("co");

        env::set_current_dir(&original_dir).unwrap();

        assert_eq!(result, Some("checkout".to_string()));
    }

    #[test]
    #[serial]
    fn resolve_git_alias_nonexistent_returns_none() {
        let tmpdir = create_test_git_repo_with_aliases();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let provider = test_provider();
        let result = provider.resolve_git_alias("zzz_no_such_alias");

        env::set_current_dir(&original_dir).unwrap();

        assert_eq!(result, None);
    }

    #[test]
    #[serial]
    fn resolve_git_alias_multi_word() {
        let tmpdir = create_test_git_repo_with_aliases();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let provider = test_provider();
        let result = provider.resolve_git_alias("nb");

        env::set_current_dir(&original_dir).unwrap();

        assert_eq!(result, Some("checkout -b".to_string()));
    }

    #[test]
    #[serial]
    fn complete_git_branch_current_branch_comes_first() {
        let tmpdir = create_test_git_repo();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let candidates = complete_git_branch("");

        env::set_current_dir(&original_dir).unwrap();

        assert!(
            candidates.len() >= 2,
            "should have at least 2 branches (main/master + test-feature): {candidates:?}"
        );
        let first = &candidates[0].value;
        let current = &["main", "master"];
        assert!(
            current.contains(&first.as_str()),
            "first suggestion should be the current branch (main or master), got: {first}"
        );
    }

    #[test]
    #[serial]
    fn cache_is_populated_after_first_call() {
        let tmpdir = create_test_git_repo_with_aliases();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let canonical_cwd = env::current_dir().unwrap();

        let provider = test_provider();

        {
            let cache = provider.git_aliases_cache.read().unwrap();
            assert!(cache.is_empty(), "cache should be empty before first call");
        }

        let _ = provider.resolve_git_alias("co");

        env::set_current_dir(&original_dir).unwrap();

        let cache = provider.git_aliases_cache.read().unwrap();
        assert_eq!(cache.len(), 1, "cache should have one CWD entry");
        let aliases = cache.get(&canonical_cwd).unwrap();
        assert_eq!(aliases.get("co"), Some(&"checkout".to_string()));
        assert_eq!(aliases.get("nb"), Some(&"checkout -b".to_string()));
    }
}
