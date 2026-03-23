//! Starship プロンプト統合
//!
//! `starship prompt` をサブプロセスとして呼び出し、その出力を
//! reedline の `Prompt` trait 経由で描画する。

use std::borrow::Cow;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;

use reedline::{Color, Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus};
use tracing::{debug, warn};

use super::EXIT_CODE_NONE;

/// `cmd_duration_ms` が未計測であることを示すセンチネル値。
pub const CMD_DURATION_NONE: u64 = u64::MAX;

/// Starship による外部プロンプト描画。
///
/// 各 `render_*` メソッドで `starship prompt` をサブプロセス実行し、
/// 出力の ANSI 文字列をそのまま reedline に返す。
pub struct StarshipPrompt {
    last_exit_code: Arc<AtomicI32>,
    cmd_duration_ms: Arc<AtomicU64>,
    starship_path: PathBuf,
}

impl StarshipPrompt {
    pub fn new(
        last_exit_code: Arc<AtomicI32>,
        cmd_duration_ms: Arc<AtomicU64>,
        starship_path: PathBuf,
    ) -> Self {
        Self {
            last_exit_code,
            cmd_duration_ms,
            starship_path,
        }
    }

    /// `starship prompt` の共通引数を組み立てて実行する。
    ///
    /// `extra_args` で `--right` や `--continuation` を追加可能。
    fn run_starship(&self, extra_args: &[&str]) -> String {
        let code = self.last_exit_code.load(Ordering::Relaxed);
        let duration = self.cmd_duration_ms.load(Ordering::Relaxed);

        let mut cmd = Command::new(&self.starship_path);
        cmd.arg("prompt");

        for arg in extra_args {
            cmd.arg(arg);
        }

        if code != EXIT_CODE_NONE {
            cmd.arg(format!("--status={code}"));
        }

        if duration != CMD_DURATION_NONE {
            cmd.arg(format!("--cmd-duration={duration}"));
        }

        if let Some(width) = terminal_width() {
            cmd.arg(format!("--terminal-width={width}"));
        }

        match cmd.output() {
            Ok(output) => {
                let text = String::from_utf8_lossy(&output.stdout).to_string();
                debug!(
                    extra_args = ?extra_args,
                    status = code,
                    duration_ms = duration,
                    output_len = text.len(),
                    "starship prompt executed"
                );
                text
            }
            Err(e) => {
                warn!(error = %e, "Failed to execute starship prompt");
                String::from("\u{276f} ")
            }
        }
    }
}

impl Prompt for StarshipPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Owned(self.run_starship(&[]))
    }

    fn get_prompt_color(&self) -> Color {
        Color::White
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Owned(self.run_starship(&["--right"]))
    }

    /// Starship がプロンプトインジケータ（❯ 等）を含むため空文字列を返す。
    fn render_prompt_indicator(&self, _edit_mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Owned(self.run_starship(&["--continuation"]))
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

fn terminal_width() -> Option<u16> {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            Some(ws.ws_col)
        } else {
            None
        }
    }
}
