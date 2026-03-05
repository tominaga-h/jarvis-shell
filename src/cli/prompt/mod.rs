mod git;

use std::borrow::Cow;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, RwLock};

use chrono::Local;
use reedline::{Color, Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus};

use super::color::{cyan, green, red, white, yellow};
use crate::config::PromptConfig;
use git::{current_git_branch_at, format_branch_label, format_git_status_at};

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

/// ホームディレクトリを取得する。
fn dirs_home() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

/// バックグラウンドスレッドによる Git ステータスの非同期取得状態。
///
/// Stale-While-Revalidate パターン:
/// - 初回は `Outdated` → `Loading` → `Ready`
/// - 2回目以降は `Ready` → `Revalidating`（stale表示）→ `Ready`（更新）
enum AsyncGitState {
    /// 再計算が必要な状態（初期状態）
    Outdated,
    /// バックグラウンドスレッドでステータスを計算中（初回ロード、staleデータなし）
    Loading { branch: String },
    /// 計算完了。フォーマット済みの git_part 文字列と取得時の CWD をキャッシュ
    Ready { formatted: String, cwd: PathBuf },
    /// BGスレッドで再取得中。前回の Ready データを stale として表示し続ける
    Revalidating { stale: String },
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
    /// バックグラウンドで取得する Git ステータスの共有状態。
    /// `RwLock` により `&self` のまま内部状態を更新可能。
    git_state: Arc<RwLock<AsyncGitState>>,
}

impl JarvisPrompt {
    pub fn new(last_exit_code: Arc<AtomicI32>, config: PromptConfig) -> Self {
        Self {
            last_exit_code,
            config,
            git_state: Arc::new(RwLock::new(AsyncGitState::Outdated)),
        }
    }

    /// プロンプト設定を更新する（`source` コマンドによる設定再読み込み用）。
    pub fn update_config(&mut self, config: PromptConfig) {
        self.config = config;
    }

    /// Git ステータスを即座にバックグラウンドスレッドで再取得する。
    ///
    /// Stale-While-Revalidate パターン:
    /// - `Ready` かつ同一 CWD → `Revalidating`（stale 表示を維持）
    /// - `Ready` かつ CWD 変更 / `Outdated` → `Loading`（キャッシュ破棄）
    /// - `Loading` / `Revalidating` → 多重起動防止でスキップ
    ///
    /// CWD は呼び出し時点でキャプチャし、BGスレッドに move する（cd 競合防止）。
    pub fn refresh_git_status(&self) {
        let cwd = env::current_dir().unwrap_or_default();
        let nerd_font = self.config.nerd_font;

        let Ok(mut state) = self.git_state.write() else {
            return;
        };

        if matches!(
            &*state,
            AsyncGitState::Loading { .. } | AsyncGitState::Revalidating { .. }
        ) {
            return;
        }

        let prev = std::mem::replace(&mut *state, AsyncGitState::Outdated);
        match prev {
            AsyncGitState::Ready {
                formatted,
                cwd: cached_cwd,
            } if cached_cwd == cwd => {
                *state = AsyncGitState::Revalidating { stale: formatted };
            }
            AsyncGitState::Ready { .. } | AsyncGitState::Outdated => {
                match current_git_branch_at(&cwd) {
                    Some(branch_name) => {
                        *state = AsyncGitState::Loading {
                            branch: branch_name,
                        };
                    }
                    None => {
                        *state = AsyncGitState::Ready {
                            formatted: String::new(),
                            cwd,
                        };
                        return;
                    }
                }
            }
            _ => unreachable!(),
        }

        let git_state = Arc::clone(&self.git_state);
        let cwd_for_thread = cwd.clone();
        drop(state);

        std::thread::spawn(move || {
            let formatted = match current_git_branch_at(&cwd_for_thread) {
                Some(branch_name) => {
                    let status = format_git_status_at(&cwd_for_thread, nerd_font);
                    let branch_label = format_branch_label(&branch_name, nerd_font);
                    format!("on {branch_label} {status}")
                }
                None => String::new(),
            };
            if let Ok(mut s) = git_state.write() {
                *s = AsyncGitState::Ready {
                    formatted,
                    cwd: cwd_for_thread,
                };
            }
        });
    }

    /// プロンプトが占めるターミナル上の行数を返す。
    ///
    /// Alternate Screen 復元後の残像消去で、カーソルを何行上に移動すべきかを算出するために使用する。
    /// `render_prompt_left` 内の改行数 + 1（インジケータ行）で動的に決定する。
    pub fn prompt_height(&self) -> usize {
        let left = self.render_prompt_left();
        let newlines = left.chars().filter(|&c| c == '\n').count();
        newlines + 1
    }

    /// 現在の `AsyncGitState` を読み取り、git_part 文字列を返す。
    ///
    /// 純粋な読み取り専用メソッド。スレッドのスポーンは一切行わない。
    /// `try_read()` のみを使い、メインスレッドを絶対にブロックしない。
    fn resolve_git_part(&self) -> String {
        let nerd_font = self.config.nerd_font;

        let Ok(state) = self.git_state.try_read() else {
            return String::new();
        };

        match &*state {
            AsyncGitState::Outdated => String::new(),
            AsyncGitState::Loading { branch } => {
                let branch_label = format_branch_label(branch, nerd_font);
                format!("on {branch_label}")
            }
            AsyncGitState::Ready { formatted, .. } => formatted.clone(),
            AsyncGitState::Revalidating { stale } => stale.clone(),
        }
    }
}

impl Prompt for JarvisPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        let cwd = env::current_dir().unwrap_or_default();
        let cwd_display = shorten_path(&cwd);

        let git_part = self.resolve_git_part();

        let code = self.last_exit_code.load(Ordering::Relaxed);

        let label = if code != 0 && code != EXIT_CODE_NONE {
            red("\u{2717} jarvis") // ×マーク
        } else if code == 0 {
            cyan("\u{2714}\u{fe0e} jarvis") // ✓マーク
        } else {
            cyan("jarvis")
        };

        let cwd_label = if self.config.nerd_font {
            yellow(&format!("\u{f4d3} {cwd_display}")) // フォルダアイコン
        } else {
            yellow(&cwd_display)
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
