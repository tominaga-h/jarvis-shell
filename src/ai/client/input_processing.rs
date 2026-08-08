use anyhow::Result;
use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
    ChatCompletionRequestSystemMessageContent, ChatCompletionRequestUserMessage,
    ChatCompletionRequestUserMessageContent,
};
use tracing::debug;

use crate::ai::prompts::SYSTEM_PROMPT;
use crate::ai::types::{ConversationOrigin, ConversationResult, ConversationState};

use super::JarvisAI;

impl JarvisAI {
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
}
