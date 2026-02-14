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
