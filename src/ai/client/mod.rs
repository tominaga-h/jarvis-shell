//! OpenAI API クライアント — J.A.R.V.I.S. Brain
//!
//! ユーザー入力を AI に送信し、コマンドか自然言語かを判定する。
//! ストリーミングレスポンスに対応し、Tool Calling でコマンド実行を支援する。
//! エージェントループにより、複数ステップのファイル操作（読み取り→編集→書き込み）が可能。

mod agent;
mod pipe;

use anyhow::{Context, Result};
use async_openai::{
    config::OpenAIConfig,
    types::{
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestSystemMessageContent, ChatCompletionRequestUserMessage,
        ChatCompletionRequestUserMessageContent,
    },
    Client,
};
use tracing::{debug, info};

use crate::config::AiConfig;
use crate::engine::CommandResult;

use super::prompts::{ERROR_INVESTIGATION_PROMPT, SYSTEM_PROMPT};
use super::types::{AiResponse, ConversationOrigin, ConversationResult, ConversationState};

/// テキストのみのアシスタントメッセージを構築する。
fn build_text_assistant_message(text: String) -> ChatCompletionRequestMessage {
    ChatCompletionRequestMessage::Assistant(ChatCompletionRequestAssistantMessage {
        content: Some(ChatCompletionRequestAssistantMessageContent::Text(text)),
        refusal: None,
        name: None,
        audio: None,
        tool_calls: None,
        #[allow(deprecated)]
        function_call: None,
    })
}

/// J.A.R.V.I.S. AI クライアント
pub struct JarvisAI {
    client: Client<OpenAIConfig>,
    /// 使用する AI モデル名
    model: String,
    /// エージェントループの最大ラウンド数
    max_rounds: usize,
    /// AI レスポンスを Markdown としてレンダリングするか
    markdown_rendering: bool,
    /// AI パイプの入力テキスト文字数上限
    ai_pipe_max_chars: usize,
    /// AI リダイレクトの入力テキスト文字数上限
    ai_redirect_max_chars: usize,
    /// 回答のランダム性（0.0 = 決定的、2.0 = 最大ランダム）
    temperature: f32,
}

impl JarvisAI {
    /// OPENAI_API_KEY 環境変数から AI クライアントを初期化する。
    pub fn new(ai_config: &AiConfig) -> Result<Self> {
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
            markdown_rendering: ai_config.markdown_rendering,
            ai_pipe_max_chars: ai_config.ai_pipe_max_chars,
            ai_redirect_max_chars: ai_config.ai_redirect_max_chars,
            temperature: ai_config.temperature,
        })
    }

    /// AI 設定（モデル名・最大ラウンド数）を更新する。
    pub fn update_config(&mut self, ai_config: &AiConfig) {
        self.model = ai_config.model.clone();
        self.max_rounds = ai_config.max_rounds;
        self.markdown_rendering = ai_config.markdown_rendering;
        self.ai_pipe_max_chars = ai_config.ai_pipe_max_chars;
        self.ai_redirect_max_chars = ai_config.ai_redirect_max_chars;
        self.temperature = ai_config.temperature;
        info!(
            model = %self.model,
            max_rounds = self.max_rounds,
            markdown_rendering = self.markdown_rendering,
            ai_pipe_max_chars = self.ai_pipe_max_chars,
            ai_redirect_max_chars = self.ai_redirect_max_chars,
            temperature = self.temperature,
            "AI config updated"
        );
    }

    /// ユーザー入力を AI に送信し、コマンドか自然言語かを判定する。
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
            conversation: ConversationState {
                messages,
                origin: ConversationOrigin::NaturalLanguage,
            },
        })
    }

    /// コマンド異常終了時にエラーを調査する。
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
            conversation: ConversationState {
                messages,
                origin: ConversationOrigin::Investigation,
            },
        })
    }

    /// 既存の会話コンテキストを使って会話を継続する。
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_fails_without_api_key() {
        let original = std::env::var("OPENAI_API_KEY").ok();
        std::env::remove_var("OPENAI_API_KEY");

        let result = JarvisAI::new(&AiConfig::default());
        assert!(result.is_err());

        if let Some(key) = original {
            std::env::set_var("OPENAI_API_KEY", key);
        }
    }
}
