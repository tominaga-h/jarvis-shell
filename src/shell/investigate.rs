//! エラー調査フロー
//!
//! コマンドが異常終了した場合に、AI にエラーの原因と修正案を調査させる。
//! Tool Call からの失敗は自動調査、ユーザー直接入力は確認プロンプト後に調査する。

use std::io::IsTerminal;
use std::sync::atomic::Ordering;

use tracing::{debug, info, warn};

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

        // 非対話モード（-c オプション等）ではユーザー確認ができないため調査をスキップ。
        // stdin が EOF を返すと jarvis_ask_investigate が自動承認してしまう問題を防ぐ。
        if !from_tool_call && !std::io::stdin().is_terminal() {
            info!("Skipping investigation (non-interactive mode)");
            return;
        }

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

        // Tool Call 由来の失敗で既存の会話コンテキストがある場合は、
        // 新規調査ではなく会話を継続する。これにより AI は前回の修正試行を
        // 把握した上で別のアプローチを試みることができる。
        if from_tool_call {
            if let Some(mut conv) = self.conversation_state.take() {
                debug!("Continuing existing conversation for error investigation");

                let error_msg = build_error_follow_up(line, result);

                match ai.continue_conversation(&mut conv, &error_msg).await {
                    Ok(response) => {
                        self.handle_investigation_response(response, Some(conv));
                        return;
                    }
                    Err(e) => {
                        warn!(error = %e, "Conversation continuation for investigation failed, falling back to new investigation");
                        eprintln!("jarvish: investigation follow-up failed: {e}");
                    }
                }
            }
        }

        // 新規調査（会話コンテキストがない場合、または会話継続が失敗した場合）
        let bb_context = self
            .black_box
            .as_ref()
            .and_then(|bb| bb.get_recent_context(5).ok())
            .unwrap_or_default();

        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        let context = format!("Current working directory: {cwd}\n\n{bb_context}");

        match ai.investigate_error(line, result, &context).await {
            Ok(conv_result) => {
                let response = conv_result.response.clone();
                self.handle_investigation_response(response, Some(conv_result.conversation));
            }
            Err(e) => {
                warn!(error = %e, "Error investigation failed");
                eprintln!("jarvish: investigation failed: {e}");
            }
        }
    }

    /// 調査結果の AI レスポンスを処理する共通ヘルパー。
    fn handle_investigation_response(
        &mut self,
        response: AiResponse,
        conversation: Option<crate::ai::ConversationState>,
    ) {
        match response {
            AiResponse::Command(ref fix_cmd) => {
                jarvis_notice(fix_cmd);
                let fix_result = execute(fix_cmd);
                self.last_exit_code
                    .store(fix_result.exit_code, Ordering::Relaxed);
                println!();

                if fix_result.action == LoopAction::Continue {
                    if let Some(ref bb) = self.black_box {
                        if let Err(e) = bb.record(fix_cmd, &fix_result) {
                            warn!("Failed to record fix command history: {e}");
                        }
                    }
                }

                // 修正コマンド実行後も会話コンテキストを保持し、
                // 再失敗時の会話継続に備える
                self.conversation_state = conversation;
            }
            AiResponse::NaturalLanguage(_) => {
                self.conversation_state = conversation;
                println!();
            }
        }
    }
}

/// ツール実行コマンドの失敗情報をフォローアップメッセージとして構築する。
fn build_error_follow_up(command: &str, result: &CommandResult) -> String {
    let mut msg = format!(
        "The fix command I just ran has failed.\n\
         Command: {command}\n\
         Exit code: {}\n",
        result.exit_code
    );
    if !result.stdout.is_empty() {
        msg.push_str(&format!("\nstdout:\n{}\n", result.stdout));
    }
    if !result.stderr.is_empty() {
        msg.push_str(&format!("\nstderr:\n{}\n", result.stderr));
    }
    msg.push_str("\nPlease investigate and try a different approach to fix this.");
    msg
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
