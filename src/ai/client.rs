//! OpenAI API クライアント — J.A.R.V.I.S. Brain
//!
//! ユーザー入力を AI に送信し、コマンドか自然言語かを判定する。
//! ストリーミングレスポンスに対応し、Tool Calling でコマンド実行を支援する。
//! エージェントループにより、複数ステップのファイル操作（読み取り→編集→書き込み）が可能。

use anyhow::{Context, Result};
use async_openai::{
    config::OpenAIConfig,
    types::{
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestSystemMessageContent, ChatCompletionRequestToolMessage,
        ChatCompletionRequestToolMessageContent, ChatCompletionRequestUserMessage,
        ChatCompletionRequestUserMessageContent, CreateChatCompletionRequest,
    },
    Client,
};
use tracing::{debug, info, warn};

use crate::engine::CommandResult;

use super::prompts::{ERROR_INVESTIGATION_PROMPT, SYSTEM_PROMPT};
use super::stream::process_stream;
use super::tools;
use super::types::{AiResponse, ConversationResult, ConversationState};
use crate::config::AiConfig;

/// J.A.R.V.I.S. AI クライアント
pub struct JarvisAI {
    client: Client<OpenAIConfig>,
    /// 使用する AI モデル名
    model: String,
    /// エージェントループの最大ラウンド数
    max_rounds: usize,
}

impl JarvisAI {
    /// OPENAI_API_KEY 環境変数から AI クライアントを初期化する。
    ///
    /// `ai_config` で使用するモデル名やエージェントループの最大ラウンド数を指定する。
    pub fn new(ai_config: &AiConfig) -> Result<Self> {
        // async-openai は OPENAI_API_KEY 環境変数を自動で読み取る
        let api_key = std::env::var("OPENAI_API_KEY")
            .context("OPENAI_API_KEY is not set. AI features are disabled.")?;

        if api_key.is_empty() || api_key == "your_openai_api_key" {
            anyhow::bail!("OPENAI_API_KEY is not configured. Please set a valid API key in .env");
        }

        let config = OpenAIConfig::new().with_api_key(&api_key);
        let client = Client::with_config(config);
        Ok(Self {
            client,
            model: ai_config.model.clone(),
            max_rounds: ai_config.max_rounds,
        })
    }

    /// ユーザー入力を AI に送信し、コマンドか自然言語かを判定する。
    /// エージェントループにより、複数ステップのツール呼び出し（read_file → write_file 等）を処理する。
    /// 自然言語の場合はストリーミングでターミナルに表示しつつ、全文を返す。
    /// 会話履歴も返却し、会話コンテキストの継続に使用できる。
    pub async fn process_input(&self, input: &str, context: &str) -> Result<ConversationResult> {
        debug!(
            user_input = %input,
            context_length = context.len(),
            context_empty = context.is_empty(),
            "process_input() called"
        );

        let system_content = if context.is_empty() {
            SYSTEM_PROMPT.to_string()
        } else {
            format!("{SYSTEM_PROMPT}\n\n{context}")
        };

        debug!(
            system_prompt_length = system_content.len(),
            "System prompt assembled"
        );
        debug!(system_prompt = %system_content, "Full system prompt content");

        // 会話履歴を構築（エージェントループで蓄積される）
        let mut messages: Vec<ChatCompletionRequestMessage> = vec![
            ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(system_content),
                name: None,
            }),
            ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(input.to_string()),
                name: None,
            }),
        ];

        let response = self.run_agent_loop(&mut messages).await?;
        Ok(ConversationResult {
            response,
            conversation: ConversationState { messages },
        })
    }

    /// コマンド異常終了時にエラーを調査する。
    ///
    /// 失敗したコマンドの情報（コマンド文字列、exit code、stdout、stderr）を
    /// AI に送信し、原因の分析と修正案の提示を行う。
    /// AI がコマンドを提案した場合は `AiResponse::Command` を返す。
    /// 会話履歴も返却し、会話コンテキストの継続に使用できる。
    pub async fn investigate_error(
        &self,
        command: &str,
        result: &CommandResult,
        context: &str,
    ) -> Result<ConversationResult> {
        debug!(
            command = %command,
            exit_code = result.exit_code,
            stdout_len = result.stdout.len(),
            stderr_len = result.stderr.len(),
            "investigate_error() called"
        );

        // エラー情報をユーザーメッセージとして構築
        let mut error_details = format!(
            "The following command failed:\n\
             Command: {command}\n\
             Exit code: {}\n",
            result.exit_code
        );
        if !result.stdout.is_empty() {
            error_details.push_str(&format!("\nstdout:\n{}\n", result.stdout));
        }
        if !result.stderr.is_empty() {
            error_details.push_str(&format!("\nstderr:\n{}\n", result.stderr));
        }
        error_details.push_str("\nPlease investigate the error and suggest a fix.");

        // システムプロンプトにコンテキスト（直近の履歴）を付加
        let system_content = if context.is_empty() {
            ERROR_INVESTIGATION_PROMPT.to_string()
        } else {
            format!("{ERROR_INVESTIGATION_PROMPT}\n\n{context}")
        };

        let mut messages: Vec<ChatCompletionRequestMessage> = vec![
            ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(system_content),
                name: None,
            }),
            ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(error_details),
                name: None,
            }),
        ];

        let response = self.run_agent_loop(&mut messages).await?;
        Ok(ConversationResult {
            response,
            conversation: ConversationState { messages },
        })
    }

    /// 既存の会話コンテキストを使って会話を継続する。
    ///
    /// 既存の会話状態にユーザーの新しい入力を追加し、エージェントループを実行する。
    /// 会話コンテキストが保持されるため、AI は前の会話を踏まえた応答を返す。
    pub async fn continue_conversation(
        &self,
        state: &mut ConversationState,
        input: &str,
    ) -> Result<AiResponse> {
        debug!(
            user_input = %input,
            messages_count = state.messages.len(),
            "continue_conversation() called"
        );

        state.messages.push(ChatCompletionRequestMessage::User(
            ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(input.to_string()),
                name: None,
            },
        ));

        self.run_agent_loop(&mut state.messages).await
    }

    /// エージェントループを実行する共通メソッド。
    ///
    /// 会話履歴（messages）に対して API リクエスト → ストリーム処理 → ツール実行を繰り返す。
    /// 最終応答の NaturalLanguage テキストもアシスタントメッセージとして messages に追加する
    /// （会話継続のため）。
    async fn run_agent_loop(
        &self,
        messages: &mut Vec<ChatCompletionRequestMessage>,
    ) -> Result<AiResponse> {
        let tool_defs = tools::build_tools();

        for round in 0..self.max_rounds {
            debug!(
                round = round,
                messages_count = messages.len(),
                "Agent loop round"
            );

            let request = CreateChatCompletionRequest {
                model: self.model.clone(),
                messages: messages.clone(),
                tools: Some(tool_defs.clone()),
                stream: Some(true),
                ..Default::default()
            };

            debug!(
                model = %self.model,
                message_count = messages.len(),
                tools_count = tool_defs.len(),
                stream = true,
                round = round,
                "Sending API request to OpenAI"
            );

            // ストリーム処理
            let stream_result = process_stream(&self.client, request, round == 0).await?;

            // Ctrl-C で中断された場合は、部分テキストをそのまま返す
            if stream_result.interrupted {
                info!(
                    round = round,
                    text_length = stream_result.full_text.len(),
                    "Stream interrupted by Ctrl-C, returning partial result"
                );
                // 部分テキストがあれば会話履歴に追加（会話継続のため）
                if !stream_result.full_text.is_empty() {
                    messages.push(ChatCompletionRequestMessage::Assistant(
                        ChatCompletionRequestAssistantMessage {
                            content: Some(ChatCompletionRequestAssistantMessageContent::Text(
                                stream_result.full_text.clone(),
                            )),
                            refusal: None,
                            name: None,
                            audio: None,
                            tool_calls: None,
                            #[allow(deprecated)]
                            function_call: None,
                        },
                    ));
                }
                return Ok(AiResponse::NaturalLanguage(stream_result.full_text));
            }

            // Tool Call がなければ最終応答として返す
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

                // 会話履歴にアシスタントの最終応答を追加（会話継続のため）
                if !stream_result.full_text.is_empty() {
                    messages.push(ChatCompletionRequestMessage::Assistant(
                        ChatCompletionRequestAssistantMessage {
                            content: Some(ChatCompletionRequestAssistantMessageContent::Text(
                                stream_result.full_text.clone(),
                            )),
                            refusal: None,
                            name: None,
                            audio: None,
                            tool_calls: None,
                            #[allow(deprecated)]
                            function_call: None,
                        },
                    ));
                }

                return Ok(AiResponse::NaturalLanguage(stream_result.full_text));
            }

            // Tool Call を処理
            // execute_shell_command があればコマンドとして即座に返す
            if let Some(cmd) = tools::call::extract_shell_command(&stream_result.tool_calls) {
                info!(
                    response_type = "Command",
                    command = %cmd,
                    round = round,
                    "AI response: execute command"
                );
                return Ok(AiResponse::Command(cmd));
            }

            // ファイル操作ツールを処理し、会話履歴に追加してループ続行
            let assistant_tool_calls =
                tools::call::build_assistant_tool_calls(&stream_result.tool_calls);

            // アシスタントメッセージ（tool_calls 付き）を会話履歴に追加
            messages.push(ChatCompletionRequestMessage::Assistant(
                ChatCompletionRequestAssistantMessage {
                    content: if stream_result.full_text.is_empty() {
                        None
                    } else {
                        Some(ChatCompletionRequestAssistantMessageContent::Text(
                            stream_result.full_text,
                        ))
                    },
                    refusal: None,
                    name: None,
                    audio: None,
                    tool_calls: Some(assistant_tool_calls),
                    #[allow(deprecated)]
                    function_call: None,
                },
            ));

            // 各ツールをローカルで実行し、結果を会話履歴に追加
            for tc in &stream_result.tool_calls {
                let result = tools::executor::execute_tool(&tc.function_name, &tc.arguments);

                debug!(
                    tool = %tc.function_name,
                    tool_call_id = %tc.id,
                    result_length = result.len(),
                    round = round,
                    "Tool executed locally"
                );

                messages.push(ChatCompletionRequestMessage::Tool(
                    ChatCompletionRequestToolMessage {
                        content: ChatCompletionRequestToolMessageContent::Text(result),
                        tool_call_id: tc.id.clone(),
                    },
                ));
            }
        }

        // ラウンド上限に達した場合
        warn!(
            max_rounds = self.max_rounds,
            "Agent loop reached maximum rounds"
        );
        Ok(AiResponse::NaturalLanguage(
            "I apologize, sir. I've reached the maximum number of processing steps.".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_fails_without_api_key() {
        // OPENAI_API_KEY が空の場合にエラーを返すことを確認
        let original = std::env::var("OPENAI_API_KEY").ok();
        std::env::remove_var("OPENAI_API_KEY");

        let result = JarvisAI::new(&AiConfig::default());
        assert!(result.is_err());

        // 元に戻す
        if let Some(key) = original {
            std::env::set_var("OPENAI_API_KEY", key);
        }
    }
}
