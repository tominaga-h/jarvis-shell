mod git;
mod jarvis;
pub mod starship;

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, AtomicU64};
use std::sync::Arc;

use reedline::{Color, Prompt, PromptEditMode, PromptHistorySearch};

use crate::config::PromptConfig;
use jarvis::JarvisPrompt;
use starship::StarshipPrompt;

pub use jarvis::EXIT_CODE_NONE;

/// ビルトインプロンプトと Starship プロンプトを切り替える列挙型。
///
/// `Shell` のプロンプトフィールドとして保持され、
/// `reedline::Prompt` の各メソッドを内部バリアントに委譲する。
pub enum ShellPrompt {
    Builtin(JarvisPrompt),
    Starship(StarshipPrompt),
}

impl ShellPrompt {
    /// ビルトインプロンプト（デフォルト）を構築する。
    pub fn builtin(last_exit_code: Arc<AtomicI32>, config: PromptConfig) -> Self {
        Self::Builtin(JarvisPrompt::new(last_exit_code, config))
    }

    /// Starship プロンプトを構築する。
    pub fn starship(
        last_exit_code: Arc<AtomicI32>,
        cmd_duration_ms: Arc<AtomicU64>,
        starship_path: PathBuf,
    ) -> Self {
        Self::Starship(StarshipPrompt::new(
            last_exit_code,
            cmd_duration_ms,
            starship_path,
        ))
    }

    /// Git ステータスをバックグラウンドで再取得する。
    ///
    /// Starship モードでは Starship 自身が Git 情報を描画するため no-op。
    pub fn refresh_git_status(&self) {
        match self {
            Self::Builtin(ref p) => p.refresh_git_status(),
            Self::Starship(ref p) => p.mark_dirty(),
        }
    }
}

impl Prompt for ShellPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        match self {
            Self::Builtin(p) => p.render_prompt_left(),
            Self::Starship(p) => p.render_prompt_left(),
        }
    }

    fn get_prompt_color(&self) -> Color {
        match self {
            Self::Builtin(p) => p.get_prompt_color(),
            Self::Starship(p) => p.get_prompt_color(),
        }
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        match self {
            Self::Builtin(p) => p.render_prompt_right(),
            Self::Starship(p) => p.render_prompt_right(),
        }
    }

    fn render_prompt_indicator(&self, edit_mode: PromptEditMode) -> Cow<'_, str> {
        match self {
            Self::Builtin(p) => p.render_prompt_indicator(edit_mode),
            Self::Starship(p) => p.render_prompt_indicator(edit_mode),
        }
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        match self {
            Self::Builtin(p) => p.render_prompt_multiline_indicator(),
            Self::Starship(p) => p.render_prompt_multiline_indicator(),
        }
    }

    fn render_prompt_history_search_indicator(
        &self,
        history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        match self {
            Self::Builtin(p) => p.render_prompt_history_search_indicator(history_search),
            Self::Starship(p) => p.render_prompt_history_search_indicator(history_search),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::jarvis::{dirs_home, shorten_path};
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
