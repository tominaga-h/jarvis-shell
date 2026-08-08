//! エージェントループ — ツールコール付き複数ステップ処理

use anyhow::Result;
use tracing::{debug, info, warn};

use crate::ai::provider::types::{ChatMessage, ChatRequest};
use crate::ai::stream::process_stream;
use crate::ai::tools;
use crate::ai::types::AiResponse;

impl super::JarvisAI {
    /// エージェントループを実行する共通メソッド。
    ///
    /// 会話履歴（messages）に対して API リクエスト → ストリーム処理 → ツール実行を繰り返す。
    /// 最終応答の NaturalLanguage テキストもアシスタントメッセージとして messages に追加する
    /// （会話継続のため）。
    pub(super) async fn run_agent_loop(
        &self,
        messages: &mut Vec<ChatMessage>,
    ) -> Result<AiResponse> {
        let model = self.model.clone();
        let tool_defs = tools::build_tools();

        for round in 0..self.max_rounds {
            debug!(
                round = round,
                messages_count = messages.len(),
                "Agent loop round"
            );

            let request = ChatRequest {
                model: model.clone(),
                messages: messages.clone(),
                tools: Some(tool_defs.clone()),
                temperature: Some(self.temperature),
                max_tokens: None,
            };

            debug!(
                model = %model,
                message_count = messages.len(),
                tools_count = tool_defs.len(),
                stream = true,
                round = round,
                "Sending API request to OpenAI"
            );

            let stream_result =
                process_stream(&self.backend, request, round == 0, self.markdown_rendering).await?;

            if stream_result.interrupted {
                info!(
                    round = round,
                    text_length = stream_result.full_text.len(),
                    "Stream interrupted by Ctrl-C, returning partial result"
                );
                if !stream_result.full_text.is_empty() {
                    messages.push(super::core::build_text_assistant_message(
                        stream_result.full_text.clone(),
                    ));
                }
                return Ok(AiResponse::NaturalLanguage(stream_result.full_text));
            }

            if stream_result.tool_calls.is_empty() {
                if stream_result.full_text.is_empty() {
                    warn!(
                        round = round,
                        "AI returned empty response (no text, no tool calls)"
                    );
                } else {
                    info!(
                        response_type = "NaturalLanguage",
                        response_length = stream_result.full_text.len(),
                        round = round,
                        "AI response: natural language"
                    );
                }

                if !stream_result.full_text.is_empty() {
                    messages.push(super::core::build_text_assistant_message(
                        stream_result.full_text.clone(),
                    ));
                }

                return Ok(AiResponse::NaturalLanguage(stream_result.full_text));
            }

            if let Some(cmd) = tools::call::extract_shell_command(&stream_result.tool_calls) {
                // execute_shell_command と同時に返された他のツール（read_file, write_file,
                // search_replace 等）を先に実行する。これにより、AI が「ファイル修正 → ビルド」
                // を1ラウンドで返した場合でもファイル修正が確実に適用される。
                let non_shell = tools::call::extract_non_shell_tools(&stream_result.tool_calls);
                for tc in &non_shell {
                    let result = tools::executor::execute_tool(&tc.function_name, &tc.arguments);
                    debug!(
                        tool = %tc.function_name,
                        tool_call_id = %tc.id,
                        result_length = result.len(),
                        round = round,
                        "Pre-command tool executed locally"
                    );
                }

                info!(
                    response_type = "Command",
                    command = %cmd,
                    round = round,
                    "AI response: execute command"
                );
                return Ok(AiResponse::Command(cmd));
            }

            messages.push(ChatMessage::Assistant {
                text: (!stream_result.full_text.is_empty()).then_some(stream_result.full_text),
                tool_calls: tools::call::build_assistant_tool_calls(&stream_result.tool_calls),
            });

            for tc in &stream_result.tool_calls {
                let result = tools::executor::execute_tool(&tc.function_name, &tc.arguments);

                debug!(
                    tool = %tc.function_name,
                    tool_call_id = %tc.id,
                    result_length = result.len(),
                    round = round,
                    "Tool executed locally"
                );

                messages.push(ChatMessage::ToolResult {
                    tool_call_id: tc.id.clone(),
                    content: result,
                });
            }
        }

        warn!(
            max_rounds = self.max_rounds,
            "Agent loop reached maximum rounds"
        );
        Ok(AiResponse::NaturalLanguage(
            "I apologize, sir. I've reached the maximum number of processing steps.".to_string(),
        ))
    }
}
