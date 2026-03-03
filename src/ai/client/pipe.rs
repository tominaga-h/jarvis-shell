//! AI パイプ / リダイレクト処理
//!
//! - `cmd | ai "prompt"` — フィルタモード（データ変換）
//! - `cmd > ai "prompt"` — リダイレクトモード（Jarvis が対話的に応答）

use anyhow::Result;
use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
    ChatCompletionRequestSystemMessageContent, ChatCompletionRequestUserMessage,
    ChatCompletionRequestUserMessageContent, CreateChatCompletionRequest,
};
use tracing::debug;

use crate::ai::prompts::{AI_PIPE_PROMPT, AI_REDIRECT_PROMPT};
use crate::ai::stream::process_ai_pipe_stream;

impl super::JarvisAI {
    /// AI パイプ (`cmd | ai "prompt"`) を処理する。
    ///
    /// 手前パイプラインの stdout（`stdin_text`）とユーザー指示（`prompt`）を
    /// AI に送信し、フィルタリング結果をプレーンテキストで返す。
    pub async fn process_ai_pipe(&self, stdin_text: &str, prompt: &str) -> Result<String> {
        let char_count = stdin_text.chars().count();
        let limit = self.ai_pipe_max_chars;

        debug!(
            prompt = %prompt,
            input_chars = char_count,
            limit = limit,
            "process_ai_pipe() called"
        );

        if char_count > limit {
            anyhow::bail!(
                "input text exceeds the {limit} chars limit ({char_count} chars). \
                 Use 'head' or 'tail' to reduce input, or increase 'ai_pipe_max_chars' in config.toml."
            );
        }

        let user_message = format!("[User Instruction]\n{prompt}\n\n[Input Text]\n{stdin_text}");

        let messages: Vec<ChatCompletionRequestMessage> = vec![
            ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(
                    AI_PIPE_PROMPT.to_string(),
                ),
                name: None,
            }),
            ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(user_message),
                name: None,
            }),
        ];

        let request = CreateChatCompletionRequest {
            model: self.model.clone(),
            messages,
            stream: Some(true),
            temperature: Some(self.temperature),
            ..Default::default()
        };

        let raw = process_ai_pipe_stream(&self.client, request, self.markdown_rendering).await?;
        Ok(sanitize_ai_pipe_output(&raw))
    }

    /// AI リダイレクト (`cmd > ai "prompt"`) を処理する。
    ///
    /// フィルタモードとは異なり、Jarvis が対話的にデータを分析・応答する。
    /// マークダウンを保持し、サニタイズは行わない。
    pub async fn process_ai_redirect(&self, stdin_text: &str, prompt: &str) -> Result<String> {
        let char_count = stdin_text.chars().count();
        let limit = self.ai_redirect_max_chars;

        debug!(
            prompt = %prompt,
            input_chars = char_count,
            limit = limit,
            "process_ai_redirect() called"
        );

        if char_count > limit {
            anyhow::bail!(
                "input text exceeds the {limit} chars limit ({char_count} chars). \
                 Use 'head' or 'tail' to reduce input, or increase 'ai_redirect_max_chars' in config.toml."
            );
        }

        let user_message = format!("[User Instruction]\n{prompt}\n\n[Input Text]\n{stdin_text}");

        let messages: Vec<ChatCompletionRequestMessage> = vec![
            ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(
                    AI_REDIRECT_PROMPT.to_string(),
                ),
                name: None,
            }),
            ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(user_message),
                name: None,
            }),
        ];

        let request = CreateChatCompletionRequest {
            model: self.model.clone(),
            messages,
            stream: Some(true),
            temperature: Some(self.temperature),
            ..Default::default()
        };

        let raw = process_ai_pipe_stream(&self.client, request, self.markdown_rendering).await?;
        Ok(raw)
    }
}

/// AI パイプ出力のサニタイズ。
///
/// LLM が指示に反して Markdown コードフェンスを出力した場合に除去する。
fn sanitize_ai_pipe_output(text: &str) -> String {
    let trimmed = text.trim();

    if trimmed.starts_with("```") && trimmed.ends_with("```") && trimmed.len() > 6 {
        let inner = &trimmed[3..trimmed.len() - 3];
        if let Some(newline_pos) = inner.find('\n') {
            return inner[newline_pos + 1..].trim().to_string();
        }
    }

    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_code_fence_with_language() {
        let input = "```json\n{\"key\": \"value\"}\n```";
        assert_eq!(sanitize_ai_pipe_output(input), "{\"key\": \"value\"}");
    }

    #[test]
    fn sanitize_strips_code_fence_without_language() {
        let input = "```\nhello world\n```";
        assert_eq!(sanitize_ai_pipe_output(input), "hello world");
    }

    #[test]
    fn sanitize_preserves_plain_text() {
        let input = "hello world\nsecond line";
        assert_eq!(sanitize_ai_pipe_output(input), "hello world\nsecond line");
    }

    #[test]
    fn sanitize_trims_whitespace() {
        let input = "  \n  hello world  \n  ";
        assert_eq!(sanitize_ai_pipe_output(input), "hello world");
    }

    #[test]
    fn sanitize_handles_empty_string() {
        assert_eq!(sanitize_ai_pipe_output(""), "");
        assert_eq!(sanitize_ai_pipe_output("  "), "");
    }
}
