//! AI 会話ルーティング
//!
//! 自然言語入力を AI に送信し、新規会話または継続会話を処理する。
//! AI の応答（コマンド or 自然言語）に応じて適切なアクションを実行する。

use tracing::{debug, warn};

use crate::ai::AiResponse;
use crate::cli::jarvis::jarvis_notice;
use crate::engine::{execute, CommandResult};

use super::Shell;

/// AI ルーティングの結果
pub(super) struct AiRoutingResult {
    /// コマンド実行結果
    pub result: CommandResult,
    /// AI の Tool Call から発行されたコマンドかどうか
    pub from_tool_call: bool,
    /// 終了コードを更新すべきかどうか（NaturalLanguage 応答時は false）
    pub should_update_exit_code: bool,
}

impl Shell {
    /// 自然言語入力を AI にルーティングする。
    ///
    /// 既存の会話コンテキストがある場合は継続会話、なければ新規会話を開始する。
    /// AI が無効な場合はコマンドとして直接実行にフォールバックする。
    pub(super) async fn route_to_ai(&mut self, line: &str) -> AiRoutingResult {
        let ai = match self.ai_client {
            Some(ref ai) => ai,
            None => {
                debug!(ai_enabled = false, "AI disabled, executing directly");
                return AiRoutingResult {
                    result: execute(line),
                    from_tool_call: false,
                    should_update_exit_code: true,
                };
            }
        };

        debug!(ai_enabled = true, "Routing natural language to AI");

        // 既存の会話コンテキストがある場合は継続、なければ新規会話
        let existing_conv = self.conversation_state.take();

        if let Some(mut conv) = existing_conv {
            // === 会話継続 ===
            debug!(input = %line, "Continuing existing conversation");

            match ai.continue_conversation(&mut conv, line).await {
                Ok(AiResponse::Command(ref cmd)) => {
                    debug!(
                        ai_response = "Command",
                        command = %cmd,
                        "AI continued conversation with a command"
                    );
                    jarvis_notice(cmd);
                    let mut result = execute(cmd);
                    if result.stdout.is_empty() {
                        result.stdout = format!("[Jarvis executed: {cmd}]");
                    } else {
                        result.stdout = format!("[Jarvis executed: {cmd}]\n{}", result.stdout);
                    }
                    // 会話コンテキストを維持
                    self.conversation_state = Some(conv);
                    AiRoutingResult {
                        result,
                        from_tool_call: true,
                        should_update_exit_code: true,
                    }
                }
                Ok(AiResponse::NaturalLanguage(ref text)) => {
                    debug!(
                        ai_response = "NaturalLanguage",
                        response_length = text.len(),
                        "AI continued conversation with natural language"
                    );
                    // 会話コンテキストを維持
                    self.conversation_state = Some(conv);
                    AiRoutingResult {
                        result: CommandResult::success(text.clone()),
                        from_tool_call: false,
                        // コマンド未実行のため終了コードは更新しない
                        should_update_exit_code: false,
                    }
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        input = %line,
                        "Conversation continuation failed, falling back to direct execution"
                    );
                    AiRoutingResult {
                        result: execute(line),
                        from_tool_call: false,
                        should_update_exit_code: true,
                    }
                }
            }
        } else {
            // === 新規会話 ===
            // BlackBox から直近 5 件のコマンド履歴をコンテキストとして取得
            let context = self
                .black_box
                .as_ref()
                .and_then(|bb| bb.get_recent_context(5).ok())
                .unwrap_or_default();

            debug!(context_length = context.len(), "Context retrieved for AI");

            match ai.process_input(line, &context).await {
                Ok(conv_result) => match conv_result.response {
                    AiResponse::Command(ref cmd) => {
                        debug!(
                            ai_response = "Command",
                            command = %cmd,
                            "AI interpreted natural language as a command"
                        );
                        // AI が自然言語からコマンドを解釈 → 実行前にアナウンス
                        jarvis_notice(cmd);
                        let mut result = execute(cmd);
                        // AI が実行したコマンドをコンテキストとして stdout に記録
                        if result.stdout.is_empty() {
                            result.stdout = format!("[Jarvis executed: {cmd}]");
                        } else {
                            result.stdout = format!("[Jarvis executed: {cmd}]\n{}", result.stdout);
                        }
                        AiRoutingResult {
                            result,
                            from_tool_call: true,
                            should_update_exit_code: true,
                        }
                    }
                    AiResponse::NaturalLanguage(ref text) => {
                        debug!(
                            ai_response = "NaturalLanguage",
                            response_length = text.len(),
                            "AI responded with natural language"
                        );
                        // 会話コンテキストを保存
                        self.conversation_state = Some(conv_result.conversation);
                        AiRoutingResult {
                            result: CommandResult::success(text.clone()),
                            from_tool_call: false,
                            // コマンド未実行のため終了コードは更新しない
                            should_update_exit_code: false,
                        }
                    }
                },
                Err(e) => {
                    warn!(
                        error = %e,
                        input = %line,
                        "AI processing failed, falling back to direct execution"
                    );
                    AiRoutingResult {
                        result: execute(line),
                        from_tool_call: false,
                        should_update_exit_code: true,
                    }
                }
            }
        }
    }
}
