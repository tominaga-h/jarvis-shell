use anyhow::Result;
use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestUserMessage,
    ChatCompletionRequestUserMessageContent,
};
use tracing::debug;

use crate::ai::types::{AiResponse, ConversationState};

use super::JarvisAI;

impl JarvisAI {
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
