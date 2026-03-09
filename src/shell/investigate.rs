//! エラー調査フロー
//!
//! コマンドが異常終了した場合に、AI にエラーの原因と修正案を調査させる。
//! Tool Call からの失敗は自動調査、ユーザー直接入力は確認プロンプト後に調査する。

use std::sync::atomic::Ordering;

use tracing::{info, warn};

use crate::ai::AiResponse;
use crate::cli::jarvis::{jarvis_ask_investigate, jarvis_notice};
use crate::engine::{execute, CommandResult, LoopAction};

use super::Shell;

/// コマンドが ignore リストのいずれかのパターンに前方一致するかを判定する。
///
/// パターンがコマンドと完全一致するか、コマンドが「パターン + スペース」で始まる場合に true。
/// 例: パターン `"git log"` は `"git log"`, `"git log --oneline"` にマッチするが、
///      `"git logx"` にはマッチしない。
fn matches_ignore_pattern(line: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .any(|pattern| line == pattern || line.starts_with(&format!("{pattern} ")))
}

impl Shell {
    /// コマンドが異常終了した場合にエラー調査を実行する。
    ///
    /// - `from_tool_call`: true の場合、ユーザー確認なしで自動調査
    /// - `from_tool_call`: false の場合、確認プロンプト後に調査
    pub(super) async fn investigate_error(
        &mut self,
        line: &str,
        result: &CommandResult,
        from_tool_call: bool,
    ) {
        let ai = match self.ai_client {
            Some(ref ai) => ai,
            None => return,
        };

        // ignore_auto_investigation_cmds に前方一致するコマンドは調査をスキップ
        // （Tool Call からの自動調査は常に実行する）
        if !from_tool_call && matches_ignore_pattern(line, &self.ignore_auto_investigation_cmds) {
            info!(command = %line, "Skipping investigation (matched ignore_auto_investigation_cmds)");
            return;
        }

        // 調査開始の判定
        let should_investigate = if from_tool_call {
            info!("Tool Call command failed, auto-investigating");
            true
        } else {
            jarvis_ask_investigate(result.exit_code)
        };

        if !should_investigate {
            return;
        }

        // BlackBox から最新コンテキストを取得（失敗したコマンドも含む）
        let context = self
            .black_box
            .as_ref()
            .and_then(|bb| bb.get_recent_context(5).ok())
            .unwrap_or_default();

        match ai.investigate_error(line, result, &context).await {
            Ok(conv_result) => match conv_result.response {
                AiResponse::Command(ref fix_cmd) => {
                    // AI が修正コマンドを提案 → 実行
                    jarvis_notice(fix_cmd);
                    let fix_result = execute(fix_cmd);
                    self.last_exit_code
                        .store(fix_result.exit_code, Ordering::Relaxed);
                    println!();

                    // 修正コマンドの結果も履歴に記録
                    if fix_result.action == LoopAction::Continue {
                        if let Some(ref bb) = self.black_box {
                            if let Err(e) = bb.record(fix_cmd, &fix_result) {
                                warn!("Failed to record fix command history: {e}");
                            }
                        }
                    }
                }
                AiResponse::NaturalLanguage(_) => {
                    // 会話コンテキストを保存
                    self.conversation_state = Some(conv_result.conversation);
                    // AI が自然言語で説明 → ストリーミング表示済み
                    println!();
                }
            },
            Err(e) => {
                warn!(error = %e, "Error investigation failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_patterns_never_matches() {
        assert!(!matches_ignore_pattern("git log", &[]));
    }

    #[test]
    fn exact_match() {
        let p = patterns(&["git log"]);
        assert!(matches_ignore_pattern("git log", &p));
    }

    #[test]
    fn prefix_match_with_args() {
        let p = patterns(&["git log"]);
        assert!(matches_ignore_pattern("git log --oneline", &p));
    }

    #[test]
    fn no_match_without_word_boundary() {
        let p = patterns(&["git log"]);
        assert!(!matches_ignore_pattern("git logx", &p));
    }

    #[test]
    fn broad_pattern_matches_all_subcommands() {
        let p = patterns(&["git"]);
        assert!(matches_ignore_pattern("git log", &p));
        assert!(matches_ignore_pattern("git status", &p));
        assert!(matches_ignore_pattern("git", &p));
    }

    #[test]
    fn multiple_patterns() {
        let p = patterns(&["git log", "make test"]);
        assert!(matches_ignore_pattern("git log --oneline", &p));
        assert!(matches_ignore_pattern("make test", &p));
        assert!(!matches_ignore_pattern("cargo test", &p));
    }

    #[test]
    fn no_partial_match_in_middle() {
        let p = patterns(&["log"]);
        assert!(!matches_ignore_pattern("git log", &p));
        assert!(matches_ignore_pattern("log something", &p));
    }
}
