use std::borrow::Cow;
use std::env;
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use reedline::{Color, Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus};

use super::color::{cyan, green, red, white, yellow};

/// `last_exit_code` が未設定（コマンド未実行）であることを示すセンチネル値。
/// `AtomicI32` は `Option<i32>` を直接保持できないため、
/// 通常の終了コード（0〜255）と衝突しない `i32::MIN` を使用する。
pub const EXIT_CODE_NONE: i32 = i32::MIN;

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
/// 表示形式（通常モード・成功時）:
/// ```text
/// ✔︎ jarvis in ~/dev/project on  main
/// ❯
/// ```
///
/// 表示形式（通常モード・異常終了時）:
/// ```text
/// ✗ jarvis in ~/dev/project on  main
/// ❯
/// ```
///
pub struct JarvisPrompt {
    /// 直前コマンドの終了コード。メインループから共有される。
    last_exit_code: Arc<AtomicI32>,
}

impl JarvisPrompt {
    pub fn new(last_exit_code: Arc<AtomicI32>) -> Self {
        Self { last_exit_code }
    }
}

impl Prompt for JarvisPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        let cwd = env::current_dir()
            .map(|p| shorten_path(&p))
            .unwrap_or_else(|_| "?".to_string());

        let git_part = match current_git_branch() {
            Some(branch) => format!(" {} {}", white("on"), cyan(&format!("\u{e0a0} {branch}"))),
            None => String::new(),
        };

        let code = self.last_exit_code.load(Ordering::Relaxed);

        // 判定: エラー > 成功 > 初期状態
        // エラー時（code != 0 かつ未設定でない）: ✗ jarvis
        // コマンド成功（code == 0）:              ✔︎ jarvis
        // 初期状態（コマンド未実行）:              jarvis
        let label = if code != 0 && code != EXIT_CODE_NONE {
            red("✗ jarvis")
        } else if code == 0 {
            cyan("✔︎ jarvis")
        } else {
            // EXIT_CODE_NONE → 初期状態
            cyan("jarvis")
        };

        Cow::Owned(format!(
            "{label} {} {}{git_part}\n",
            white("in"),
            yellow(&cwd),
        ))
    }

    fn get_prompt_color(&self) -> Color {
        Color::White
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _edit_mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Owned(green("❯ "))
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed(" :: ")
    }

    fn render_prompt_history_search_indicator(
        &self,
        history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
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
