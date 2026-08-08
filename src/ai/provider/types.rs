//! Provider-neutral request and response types.

use serde_json::Value;

/// A message in a provider-neutral chat history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatMessage {
    System(String),
    User(String),
    Assistant {
        text: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    ToolResult {
        tool_call_id: String,
        content: String,
    },
}

/// A completed tool call emitted by an assistant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// A tool definition shared by provider implementations.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// A provider-neutral chat completion request.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Option<Vec<ToolSpec>>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

/// A fragment of a tool call in a streaming response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallDelta {
    pub index: u32,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

/// A provider-neutral streaming response chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatChunk {
    pub text_delta: Option<String>,
    pub tool_call_deltas: Vec<ToolCallDelta>,
}
