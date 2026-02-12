use std::borrow::Cow;
use std::env;
use std::path::Path;

use reedline::{Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus};

/// ホームディレクトリのパスを `~` に短縮する。
///
/// - `$HOME` そのもの → `~`
/// - `$HOME/foo/bar` → `~/foo/bar`
/// - ホーム外のパス → そのまま返す
pub fn shorten_path(path: &Path) -> String {
    if let Some(home) = dirs_home() {
        if path == home {
            return "~".to_string();
        }
        if let Ok(rel) = path.strip_prefix(&home) {
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

/// 現在の Git ブランチ名を取得する。Git リポジトリ外の場合は None を返す。
fn current_git_branch() -> Option<String> {
    let cwd = env::current_dir().ok()?;
    let repo = git2::Repository::discover(cwd).ok()?;
    let head = repo.head().ok()?;
    head.shorthand().map(|s| s.to_string())
}

/// ホームディレクトリを取得する。
fn dirs_home() -> Option<std::path::PathBuf> {
    env::var_os("HOME").map(std::path::PathBuf::from)
}

/// Jarvis Shell のカスタムプロンプト。
///
/// 表示形式:
/// ```text
/// ⚡jarvish in ~/dev/project on  main
///  ❯
/// ```
pub struct JarvisPrompt;

impl JarvisPrompt {
    pub fn new() -> Self {
        Self
    }
}

impl Default for JarvisPrompt {
    fn default() -> Self {
        Self::new()
    }
}

impl Prompt for JarvisPrompt {
    fn render_prompt_left(&self) -> Cow<str> {
        let cwd = env::current_dir()
            .map(|p| shorten_path(&p))
            .unwrap_or_else(|_| "?".to_string());

        let git_part = match current_git_branch() {
            Some(branch) => format!(" on \u{e0a0} {branch}"),
            None => String::new(),
        };

        Cow::Owned(format!("⚡jarvish in {cwd}{git_part}"))
    }

    fn render_prompt_right(&self) -> Cow<str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _edit_mode: PromptEditMode) -> Cow<str> {
        Cow::Borrowed("\n ❯ ")
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<str> {
        Cow::Borrowed(" :: ")
    }

    fn render_prompt_history_search_indicator(
        &self,
        history_search: PromptHistorySearch,
    ) -> Cow<str> {
        let prefix = match history_search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "(failed) ",
        };
        Cow::Owned(format!("{prefix}(search: '{}') ", history_search.term))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn shorten_home_dir_itself() {
        if let Some(home) = dirs_home() {
            assert_eq!(shorten_path(&home), "~");
        }
    }

    #[test]
    fn shorten_home_subdir() {
        if let Some(home) = dirs_home() {
            let sub = home.join("dev").join("project");
            assert_eq!(shorten_path(&sub), "~/dev/project");
        }
    }

    #[test]
    fn shorten_outside_home() {
        let path = PathBuf::from("/tmp");
        assert_eq!(shorten_path(&path), "/tmp");
    }
}
