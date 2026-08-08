//! Native Anthropic Messages API backend.

mod request;
mod sse;

use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tracing::warn;

use super::openai_compat::ChatChunkStream;
use super::types::ChatRequest;
use request::AnthropicRequest;

/// Anthropic Messages API client.
pub struct AnthropicBackend {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl AnthropicBackend {
    /// Creates an Anthropic client with the required long read timeout for thinking streams.
    pub fn new(api_key: &str, base_url: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .read_timeout(Duration::from_secs(120))
            .build()
            .context("Failed to build Anthropic HTTP client")?;
        Ok(Self {
            client,
            api_key: api_key.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    pub async fn create_stream(&self, request: ChatRequest) -> Result<ChatChunkStream> {
        let request = AnthropicRequest::try_from(request)?;
        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to connect to Anthropic API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(sse::error_from_body(status.as_u16(), &body));
        }

        let byte_stream = response.bytes_stream();
        let stream = sse::stream_events(byte_stream);
        Ok(Box::pin(stream.map(|result| {
            result.map_err(|error| {
                warn!(error = %error, "Anthropic SSE stream error");
                error
            })
        })))
    }
}
