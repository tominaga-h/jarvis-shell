//! Provider abstraction for AI requests.

use anyhow::Result;

pub mod openai_compat;
pub mod types;

use openai_compat::{ChatChunkStream, OpenAiCompatBackend};
use types::{ChatChunk, ChatRequest};

/// Provider backend selected by the AI client.
pub enum AiBackend {
    /// OpenAI and OpenAI-compatible Chat Completions APIs.
    OpenAiCompat(OpenAiCompatBackend),
}

impl AiBackend {
    pub async fn create_stream(&self, request: ChatRequest) -> Result<ChatChunkStream> {
        match self {
            Self::OpenAiCompat(backend) => backend.create_stream(request).await,
        }
    }
}

#[allow(dead_code)]
fn _type_check(_: ChatChunk) {}
