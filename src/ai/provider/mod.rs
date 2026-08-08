//! Provider abstraction for AI requests.

use anyhow::Result;

pub mod anthropic;
pub mod openai_compat;
pub mod opencode;
pub mod types;

use anthropic::AnthropicBackend;
use openai_compat::{ChatChunkStream, OpenAiCompatBackend};
use opencode::OpenCodeBackend;
use types::{ChatChunk, ChatRequest};

/// Provider backend selected by the AI client.
pub enum AiBackend {
    /// OpenAI and OpenAI-compatible Chat Completions APIs.
    OpenAiCompat(OpenAiCompatBackend),
    /// Anthropic Messages API with native SSE support.
    Anthropic(AnthropicBackend),
    /// OpenCode Zen and Go OpenAI-compatible APIs.
    OpenCode(OpenCodeBackend),
}

impl AiBackend {
    pub async fn create_stream(&self, request: ChatRequest) -> Result<ChatChunkStream> {
        match self {
            Self::OpenAiCompat(backend) => backend.create_stream(request).await,
            Self::Anthropic(backend) => backend.create_stream(request).await,
            Self::OpenCode(backend) => backend.create_stream(request).await,
        }
    }
}

#[allow(dead_code)]
fn _type_check(_: ChatChunk) {}
