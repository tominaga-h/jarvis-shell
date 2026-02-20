use std::borrow::Cow;
use std::env;
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use chrono::Local;
use reedline::{Color, Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus};

use super::color::{cyan, green, red, white, yellow};
use crate::config::PromptConfig;

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

/// Git リポジトリ内のファイルステータスを集計する。
///
/// 戻り値: `(added, modified, deleted)` のタプル。
/// - added: 新規ファイル数（ステージ済み + untracked）
/// - modified: 変更ファイル数（ステージ済み + ワーキングツリー）
/// - deleted: 削除ファイル数（ステージ済み + ワーキングツリー）
fn git_file_status_counts() -> Option<(usize, usize, usize)> {
    let cwd = env::current_dir().ok()?;
    let repo = git2::Repository::discover(cwd).ok()?;
    let statuses = repo.statuses(None).ok()?;

    let mut added = 0usize;
    let mut modified = 0usize;
    let mut deleted = 0usize;

    for entry in statuses.iter() {
        let s = entry.status();
        if s.intersects(git2::Status::INDEX_NEW | git2::Status::WT_NEW) {
            added += 1;
        }
        if s.intersects(git2::Status::INDEX_MODIFIED | git2::Status::WT_MODIFIED) {
            modified += 1;
        }
        if s.intersects(git2::Status::INDEX_DELETED | git2::Status::WT_DELETED) {
            deleted += 1;
        }
    }

    Some((added, modified, deleted))
}

/// Git ステータスのカウントを色付き文字列にフォーマットする。
///
/// - `+N` (緑): 追加ファイル
/// - `~N` (黄): 変更ファイル
/// - `-N` (赤): 削除ファイル
///
/// 全て 0 の場合は空文字列を返す。
/// `nerd_font` が false の場合、NerdFont アイコンの代わりに ASCII 記号を使用する。
fn format_git_status(nerd_font: bool) -> String {
    let (added, modified, deleted) = match git_file_status_counts() {
        Some(counts) => counts,
        None => return String::new(),
    };

    if added == 0 && modified == 0 && deleted == 0 {
        return String::new();
    }

    let (modified_prefix, added_prefix, deleted_prefix) = if nerd_font {
        ("\u{ea73} ", "\u{f067} ", "\u{f068} ") // ファイル、プラス、マイナスアイコン
    } else {
        ("~", "+", "-")
    };

    let mut parts = Vec::new();
    if modified > 0 {
        parts.push(yellow(&format!("{modified_prefix}{modified}")));
    }
    if added > 0 {
        parts.push(green(&format!("{added_prefix}{added}")));
    }
    if deleted > 0 {
        parts.push(red(&format!("{deleted_prefix}{deleted}")));
    }

    parts.join(" ")
}

/// ホームディレクトリを取得する。
fn dirs_home() -> Option<std::path::PathBuf> {
    env::var_os("HOME").map(std::path::PathBuf::from)
}

/// Jarvis Shell のカスタムプロンプト。
///
/// NerdFont あり（デフォルト）:
/// ```text
/// ✔︎ jarvis in [icon] ~/dev/project on [icon] main
/// ❯
/// ```
///
/// NerdFont なし:
/// ```text
/// ✔︎ jarvis in ~/dev/project on main
/// ❯
/// ```
pub struct JarvisPrompt {
    /// 直前コマンドの終了コード。メインループから共有される。
    last_exit_code: Arc<AtomicI32>,
    /// プロンプト表示設定
    config: PromptConfig,
}

impl JarvisPrompt {
    pub fn new(last_exit_code: Arc<AtomicI32>, config: PromptConfig) -> Self {
        Self {
            last_exit_code,
            config,
        }
    }

    /// プロンプト設定を更新する（`source` コマンドによる設定再読み込み用）。
    pub fn update_config(&mut self, config: PromptConfig) {
        self.config = config;
    }
}

impl Prompt for JarvisPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        let cwd = env::current_dir()
            .map(|p| shorten_path(&p))
            .unwrap_or_else(|_| "?".to_string());

        let git_part = match current_git_branch() {
            Some(branch) => {
                let status = format_git_status(self.config.nerd_font);
                let branch_label = if self.config.nerd_font {
                    cyan(&format!("\u{e725} {branch}")) // gitアイコン
                } else {
                    cyan(&branch)
                };
                format!("on {branch_label} {status}")
            }
            None => String::new(),
        };

        let code = self.last_exit_code.load(Ordering::Relaxed);

        let label = if code != 0 && code != EXIT_CODE_NONE {
            red("\u{2717} jarvis") // ×マーク
        } else if code == 0 {
            cyan("\u{2714}\u{fe0e} jarvis") // ✓マーク
        } else {
            cyan("jarvis")
        };

        let cwd_label = if self.config.nerd_font {
            yellow(&format!("\u{f4d3} {cwd}")) // フォルダアイコン
        } else {
            yellow(&cwd)
        };

        Cow::Owned(format!("{label} in {cwd_label} {git_part}\n"))
    }

    fn get_prompt_color(&self) -> Color {
        Color::White
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        let now = Local::now().format("%H:%M:%S").to_string();
        Cow::Owned(white(&now))
    }

    fn render_prompt_indicator(&self, _edit_mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Owned(green("\u{276f} "))
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
