//! AI 会話ルーティング
//!
//! 自然言語入力を AI に送信し、新規会話または継続会話を処理する。
//! AI の応答（コマンド or 自然言語）に応じて適切なアクションを実行する。

use tracing::{debug, warn};

use crate::ai::{AiResponse, ConversationOrigin};
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
    /// AI が実際に実行したコマンド文字列（矢印キー履歴に追加するため）
    pub executed_command: Option<String>,
}

/// AI が提案したコマンドを実行し、stdout に実行記録を付与する。
fn execute_ai_command(cmd: &str) -> CommandResult {
    jarvis_notice(cmd);
    let mut result = execute(cmd);
    if result.stdout.is_empty() {
        result.stdout = format!("[Jarvis executed: {cmd}]");
    } else {
        result.stdout = format!("[Jarvis executed: {cmd}]\n{}", result.stdout);
    }
    result
}

impl Shell {
    /// 自然言語入力を AI にルーティングする。
    ///
    /// 既存の会話コンテキストがある場合は継続会話、なければ新規会話を開始する。
    /// AI が無効な場合や処理失敗時はエラーメッセージを返す。
    pub(super) async fn route_to_ai(&mut self, line: &str) -> AiRoutingResult {
        if self.ai_client.is_none() {
            debug!(ai_enabled = false, "AI disabled, returning error");
            let msg = "jarvish: AI is not available (API key not configured)\n".to_string();
            eprint!("{msg}");
            return AiRoutingResult {
                result: CommandResult::error(msg, 1),
                from_tool_call: false,
                should_update_exit_code: false,
                executed_command: None,
            };
        }

        debug!(ai_enabled = true, "Routing natural language to AI");

        // 既存の会話コンテキストがある場合は継続、なければ新規会話。
        // ただしエラー調査由来の会話は自然言語入力に流用しない。
        let existing_conv = self.conversation_state.take();

        if let Some(mut conv) = existing_conv {
            if conv.origin == ConversationOrigin::Investigation {
                debug!("Discarding investigation conversation, starting fresh");
            } else {
                // === 会話継続 ===
                debug!(input = %line, "Continuing existing conversation");
                let ai = self.ai_client.as_ref().unwrap();

                match ai.continue_conversation(&mut conv, line).await {
                    Ok(AiResponse::Command(ref cmd)) => {
                        debug!(
                            ai_response = "Command",
                            command = %cmd,
                            "AI continued conversation with a command"
                        );
                        let result = execute_ai_command(cmd);
                        self.conversation_state = Some(conv);
                        return AiRoutingResult {
                            result,
                            from_tool_call: true,
                            should_update_exit_code: true,
                            executed_command: Some(cmd.clone()),
                        };
                    }
                    Ok(AiResponse::NaturalLanguage(ref text)) => {
                        debug!(
                            ai_response = "NaturalLanguage",
                            response_length = text.len(),
                            "AI continued conversation with natural language"
                        );
                        self.conversation_state = Some(conv);
                        return AiRoutingResult {
                            result: CommandResult::success(text.clone()),
                            from_tool_call: false,
                            should_update_exit_code: false,
                            executed_command: None,
                        };
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            input = %line,
                            "Conversation continuation failed, falling back to new conversation"
                        );
                    }
                }
            }
        }

        // === 新規会話 ===
        self.start_new_ai_conversation(line).await
    }

    /// BlackBox コンテキストを取得して新規 AI 会話を開始する。
    async fn start_new_ai_conversation(&mut self, line: &str) -> AiRoutingResult {
        let ai = self.ai_client.as_ref().unwrap();
        let bb_context = self
            .black_box
            .as_ref()
            .and_then(|bb| bb.get_recent_context(5).ok())
            .unwrap_or_default();

        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        let context = format!("Current working directory: {cwd}\n\n{bb_context}");

        debug!(context_length = context.len(), cwd = %cwd, "Context retrieved for AI");

        match ai.process_input(line, &context).await {
            Ok(conv_result) => match conv_result.response {
                AiResponse::Command(ref cmd) => {
                    debug!(
                        ai_response = "Command",
                        command = %cmd,
                        "AI interpreted natural language as a command"
                    );
                    let result = execute_ai_command(cmd);
                    AiRoutingResult {
                        result,
                        from_tool_call: true,
                        should_update_exit_code: true,
                        executed_command: Some(cmd.clone()),
                    }
                }
                AiResponse::NaturalLanguage(ref text) => {
                    debug!(
                        ai_response = "NaturalLanguage",
                        response_length = text.len(),
                        "AI responded with natural language"
                    );
                    self.conversation_state = Some(conv_result.conversation);
                    AiRoutingResult {
                        result: CommandResult::success(text.clone()),
                        from_tool_call: false,
                        should_update_exit_code: false,
                        executed_command: None,
                    }
                }
            },
            Err(e) => {
                warn!(
                    error = %e,
                    input = %line,
                    "AI processing failed"
                );
                let msg = format!("jarvish: AI processing failed: {e}\n");
                eprint!("{msg}");
                AiRoutingResult {
                    result: CommandResult::error(msg, 1),
                    from_tool_call: false,
                    should_update_exit_code: false,
                    executed_command: None,
                }
            }
        }
    }
}
