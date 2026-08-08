//! OpenCode Zen and Go compatibility backend.
//!
//! OpenCode uses the OpenAI Chat Completions protocol, but some responses may
//! contain a non-conforming trailing cost chunk. That compatibility behavior
//! belongs here rather than in the generic OpenAI backend.

use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use anyhow::Result;
use async_openai::error::OpenAIError;
use async_openai::types::CreateChatCompletionStreamResponse;
use futures_util::Stream;

use super::openai_compat::{convert_response, ChatChunkStream, OpenAiCompatBackend};
use super::types::{ChatChunk, ChatRequest};

const ZEN_BASE_URL: &str = "https://opencode.ai/zen/v1";
const GO_BASE_URL: &str = "https://opencode.ai/zen/go/v1";

/// OpenCode-compatible backend composed from the generic OpenAI backend.
pub struct OpenCodeBackend {
    pub(crate) inner: OpenAiCompatBackend,
    #[cfg(test)]
    pub(crate) base_url: String,
    #[cfg(test)]
    pub(crate) user_agent: Option<String>,
}

impl OpenCodeBackend {
    /// Creates an OpenCode backend for a custom OpenAI-compatible endpoint.
    pub fn new(api_key: &str, base_url: &str) -> Result<Self> {
        let base_url = base_url.trim_end_matches('/');
        let user_agent = format!("jarvish/{}", env!("CARGO_PKG_VERSION"));
        let inner = OpenAiCompatBackend::new(api_key, Some(base_url), Some(&user_agent))?;
        Ok(Self {
            inner,
            #[cfg(test)]
            base_url: base_url.to_string(),
            #[cfg(test)]
            user_agent: Some(user_agent),
        })
    }

    /// Creates an OpenCode Zen backend.
    pub fn zen(api_key: &str) -> Result<Self> {
        Self::new(api_key, ZEN_BASE_URL)
    }

    /// Creates an OpenCode Go backend.
    pub fn go(api_key: &str) -> Result<Self> {
        Self::new(api_key, GO_BASE_URL)
    }

    pub async fn create_stream(&self, request: ChatRequest) -> Result<ChatChunkStream> {
        let inner = self.inner.create_openai_stream(request).await?;
        Ok(Box::pin(OpenCodeStream {
            inner,
            pending_consequence_suppress: false,
        }))
    }
}

/// Filters OpenCode's non-conforming trailing chunks and their parser error.
struct OpenCodeStream {
    inner: Pin<
        Box<
            dyn Stream<Item = std::result::Result<CreateChatCompletionStreamResponse, OpenAIError>>
                + Send,
        >,
    >,
    pending_consequence_suppress: bool,
}

impl Stream for OpenCodeStream {
    type Item = Result<ChatChunk>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(response))) => {
                    this.pending_consequence_suppress = false;
                    return Poll::Ready(Some(Ok(convert_response(response))));
                }
                Poll::Ready(Some(Err(error))) => {
                    if should_skip_stream_error(&error) {
                        this.pending_consequence_suppress = true;
                        tracing::debug!(
                            target: "jarvish::ai::opencode",
                            "suppressing non-conforming trailing chunk: {error}"
                        );
                        continue;
                    }
                    if this.pending_consequence_suppress {
                        this.pending_consequence_suppress = false;
                        tracing::debug!(
                            target: "jarvish::ai::opencode",
                            "suppressing consequence error after non-conforming chunk: {error}"
                        );
                        continue;
                    }
                    return Poll::Ready(Some(Err(anyhow::anyhow!(error))));
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Returns the inner serde error message for JSON deserialization failures.
fn json_deserialize_message(error: &OpenAIError) -> Option<String> {
    if let OpenAIError::JSONDeserialize(serde_err) = error {
        Some(serde_err.to_string())
    } else {
        None
    }
}

/// Detects OpenCode's valid-but-non-conforming trailing cost chunk error.
fn should_skip_stream_error(error: &OpenAIError) -> bool {
    json_deserialize_message(error).is_some_and(|message| message.contains("missing field"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zen_resolves_expected_base_url_and_user_agent() {
        let backend = OpenCodeBackend::zen("test-key").unwrap();
        assert_eq!(backend.base_url, ZEN_BASE_URL);
        assert_eq!(backend.user_agent.as_deref(), Some("jarvish/1.15.6"));
    }

    #[test]
    fn go_resolves_expected_base_url_and_user_agent() {
        let backend = OpenCodeBackend::go("test-key").unwrap();
        assert_eq!(backend.base_url, GO_BASE_URL);
        assert_eq!(backend.user_agent.as_deref(), Some("jarvish/1.15.6"));
    }

    #[test]
    fn custom_backend_trims_base_url() {
        let backend = OpenCodeBackend::new("test-key", "http://localhost:9000/v1/").unwrap();
        assert_eq!(backend.base_url, "http://localhost:9000/v1");
    }

    #[test]
    fn json_deserialize_message_extracts_inner_serde_error() {
        #[derive(serde::Deserialize, Debug)]
        struct NeedsId {
            #[allow(dead_code)]
            id: String,
        }
        let serde_err: serde_json::Error = serde_json::from_slice::<NeedsId>(b"{}").unwrap_err();
        let expected = serde_err.to_string();
        let error = OpenAIError::JSONDeserialize(serde_err);
        assert_eq!(json_deserialize_message(&error), Some(expected));
    }

    #[test]
    fn json_deserialize_message_returns_none_for_other_variants() {
        let error = OpenAIError::StreamError("connection lost".into());
        assert!(json_deserialize_message(&error).is_none());
        let error = OpenAIError::InvalidArgument("bad input".into());
        assert!(json_deserialize_message(&error).is_none());
    }

    #[test]
    fn should_skip_stream_error_skips_missing_field_deserialize_failures() {
        #[derive(serde::Deserialize, Debug)]
        struct NeedsId {
            #[allow(dead_code)]
            id: String,
        }
        let serde_err: serde_json::Error = serde_json::from_slice::<NeedsId>(b"{}").unwrap_err();
        let error = OpenAIError::JSONDeserialize(serde_err);
        assert!(should_skip_stream_error(&error));
    }

    #[test]
    fn should_skip_stream_error_does_not_skip_stream_errors() {
        let error = OpenAIError::StreamError("connection lost".into());
        assert!(!should_skip_stream_error(&error));
    }

    #[test]
    fn should_skip_stream_error_does_not_skip_other_deserialize_failures() {
        let serde_err: serde_json::Error =
            serde_json::from_slice::<i32>(br#""not a number""#).unwrap_err();
        let error = OpenAIError::JSONDeserialize(serde_err);
        assert!(!should_skip_stream_error(&error));
    }

    #[test]
    fn composition_keeps_generic_backend_inside_opencode_backend() {
        let backend = OpenCodeBackend::zen("test-key").unwrap();
        assert_eq!(backend.inner.base_url, ZEN_BASE_URL);
        assert_eq!(backend.inner.user_agent.as_deref(), Some("jarvish/1.15.6"));
    }
}
