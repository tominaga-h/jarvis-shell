//! OpenAI Chat Completions provider implementation.

use std::pin::Pin;

use anyhow::{Context, Result};
use async_openai::config::OpenAIConfig;
use async_openai::types::{
    ChatCompletionMessageToolCall, ChatCompletionRequestAssistantMessage,
    ChatCompletionRequestAssistantMessageContent, ChatCompletionRequestMessage,
    ChatCompletionRequestSystemMessage, ChatCompletionRequestSystemMessageContent,
    ChatCompletionRequestToolMessage, ChatCompletionRequestToolMessageContent,
    ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent, ChatCompletionTool,
    ChatCompletionToolType, CreateChatCompletionRequest, FunctionCall, FunctionCallStream,
};
use async_openai::Client;
use futures_util::{Stream, StreamExt};

use super::types::{ChatChunk, ChatMessage, ChatRequest, ToolCall, ToolCallDelta, ToolSpec};

/// A client for OpenAI and OpenAI-compatible Chat Completions APIs.
pub struct OpenAiCompatBackend {
    pub(crate) client: Client<OpenAIConfig>,
}

impl OpenAiCompatBackend {
    pub fn new(api_key: &str) -> Self {
        let config = OpenAIConfig::new().with_api_key(api_key);
        Self {
            client: Client::with_config(config),
        }
    }

    pub async fn create_stream(&self, request: ChatRequest) -> Result<ChatChunkStream> {
        let request = to_openai_request(request);
        let stream = self
            .client
            .chat()
            .create_stream(request)
            .await
            .context("Failed to create chat stream")?;

        Ok(Box::pin(stream.map(|result| {
            result
                .map(convert_response)
                .map_err(|error| anyhow::anyhow!(error))
        })))
    }
}

pub type ChatChunkStream = Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>;

pub(crate) fn to_openai_request(request: ChatRequest) -> CreateChatCompletionRequest {
    CreateChatCompletionRequest {
        model: request.model,
        messages: request.messages.iter().map(to_openai_message).collect(),
        tools: request
            .tools
            .as_ref()
            .map(|tools| tools.iter().map(to_openai_tool).collect()),
        stream: Some(true),
        temperature: request.temperature,
        #[allow(deprecated)]
        max_tokens: request.max_tokens,
        ..Default::default()
    }
}

#[allow(dead_code, deprecated)]
pub(crate) fn from_openai_request(request: CreateChatCompletionRequest) -> Result<ChatRequest> {
    Ok(ChatRequest {
        model: request.model,
        messages: request
            .messages
            .into_iter()
            .map(from_openai_message)
            .collect::<Result<Vec<_>>>()?,
        tools: request
            .tools
            .map(|tools| tools.into_iter().map(from_openai_tool).collect())
            .transpose()?,
        temperature: request.temperature,
        max_tokens: request.max_tokens,
    })
}

pub(crate) fn to_openai_message(message: &ChatMessage) -> ChatCompletionRequestMessage {
    match message {
        ChatMessage::System(content) => {
            ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(content.clone()),
                name: None,
            })
        }
        ChatMessage::User(content) => {
            ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(content.clone()),
                name: None,
            })
        }
        ChatMessage::Assistant { text, tool_calls } => {
            ChatCompletionRequestMessage::Assistant(ChatCompletionRequestAssistantMessage {
                content: text
                    .clone()
                    .map(ChatCompletionRequestAssistantMessageContent::Text),
                refusal: None,
                name: None,
                audio: None,
                tool_calls: (!tool_calls.is_empty()).then(|| {
                    tool_calls
                        .iter()
                        .map(|tool_call| ChatCompletionMessageToolCall {
                            id: tool_call.id.clone(),
                            r#type: ChatCompletionToolType::Function,
                            function: FunctionCall {
                                name: tool_call.name.clone(),
                                arguments: tool_call.arguments.clone(),
                            },
                        })
                        .collect()
                }),
                #[allow(deprecated)]
                function_call: None,
            })
        }
        ChatMessage::ToolResult {
            tool_call_id,
            content,
        } => ChatCompletionRequestMessage::Tool(ChatCompletionRequestToolMessage {
            content: ChatCompletionRequestToolMessageContent::Text(content.clone()),
            tool_call_id: tool_call_id.clone(),
        }),
    }
}

#[allow(dead_code)]
pub(crate) fn from_openai_message(message: ChatCompletionRequestMessage) -> Result<ChatMessage> {
    match message {
        ChatCompletionRequestMessage::System(message) => match message.content {
            ChatCompletionRequestSystemMessageContent::Text(content) => {
                Ok(ChatMessage::System(content))
            }
            ChatCompletionRequestSystemMessageContent::Array(_) => {
                anyhow::bail!("Only text system messages are supported")
            }
        },
        ChatCompletionRequestMessage::User(message) => match message.content {
            ChatCompletionRequestUserMessageContent::Text(content) => {
                Ok(ChatMessage::User(content))
            }
            ChatCompletionRequestUserMessageContent::Array(_) => {
                anyhow::bail!("Only text user messages are supported")
            }
        },
        ChatCompletionRequestMessage::Assistant(message) => {
            let text = match message.content {
                None => None,
                Some(ChatCompletionRequestAssistantMessageContent::Text(content)) => Some(content),
                Some(ChatCompletionRequestAssistantMessageContent::Array(_)) => {
                    anyhow::bail!("Only text assistant messages are supported")
                }
            };
            let tool_calls = message
                .tool_calls
                .unwrap_or_default()
                .into_iter()
                .map(|tool_call| ToolCall {
                    id: tool_call.id,
                    name: tool_call.function.name,
                    arguments: tool_call.function.arguments,
                })
                .collect();
            Ok(ChatMessage::Assistant { text, tool_calls })
        }
        ChatCompletionRequestMessage::Tool(message) => match message.content {
            ChatCompletionRequestToolMessageContent::Text(content) => Ok(ChatMessage::ToolResult {
                tool_call_id: message.tool_call_id,
                content,
            }),
            ChatCompletionRequestToolMessageContent::Array(_) => {
                anyhow::bail!("Only text tool messages are supported")
            }
        },
        ChatCompletionRequestMessage::Developer(_) | ChatCompletionRequestMessage::Function(_) => {
            anyhow::bail!("Unsupported OpenAI message role")
        }
    }
}

pub(crate) fn to_openai_tool(tool: &ToolSpec) -> ChatCompletionTool {
    ChatCompletionTool {
        r#type: ChatCompletionToolType::Function,
        function: async_openai::types::FunctionObject {
            name: tool.name.clone(),
            description: Some(tool.description.clone()),
            parameters: Some(tool.parameters.clone()),
            strict: None,
        },
    }
}

#[allow(dead_code)]
fn from_openai_tool(tool: ChatCompletionTool) -> Result<ToolSpec> {
    Ok(ToolSpec {
        name: tool.function.name,
        description: tool.function.description.unwrap_or_default(),
        parameters: tool.function.parameters.unwrap_or(serde_json::Value::Null),
    })
}

fn convert_response(
    response: async_openai::types::CreateChatCompletionStreamResponse,
) -> ChatChunk {
    let mut text = String::new();
    let mut has_content = false;
    let mut tool_call_deltas = Vec::new();

    // Keep the existing multi-choice behavior: every choice is processed in
    // order, rather than selecting choices[0].
    for choice in &response.choices {
        if let Some(content) = &choice.delta.content {
            has_content = true;
            text.push_str(content);
        }
        if let Some(tool_calls) = &choice.delta.tool_calls {
            tool_call_deltas.extend(tool_calls.iter().map(convert_tool_call_delta));
        }
    }

    ChatChunk {
        text_delta: has_content.then_some(text),
        tool_call_deltas,
    }
}

fn convert_tool_call_delta(
    chunk: &async_openai::types::ChatCompletionMessageToolCallChunk,
) -> ToolCallDelta {
    let (name, arguments) = chunk
        .function
        .as_ref()
        .map(|function: &FunctionCallStream| (function.name.clone(), function.arguments.clone()))
        .unwrap_or((None, None));
    ToolCallDelta {
        index: chunk.index,
        id: chunk.id.clone(),
        name,
        arguments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::to_string;

    fn round_trip(messages: Vec<ChatCompletionRequestMessage>) {
        let original = to_string(&messages).unwrap();
        let neutral = messages
            .clone()
            .into_iter()
            .map(from_openai_message)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let converted = neutral.iter().map(to_openai_message).collect::<Vec<_>>();
        assert_eq!(original, to_string(&converted).unwrap());
    }

    #[test]
    fn converts_text_messages() {
        round_trip(vec![
            to_openai_message(&ChatMessage::System("Be concise.".into())),
            to_openai_message(&ChatMessage::User("What failed?".into())),
            to_openai_message(&ChatMessage::Assistant {
                text: Some("The command failed.".into()),
                tool_calls: vec![],
            }),
        ]);
    }

    #[test]
    fn converts_tool_call_messages() {
        round_trip(vec![
            to_openai_message(&ChatMessage::Assistant {
                text: Some("I will inspect it.".into()),
                tool_calls: vec![ToolCall {
                    id: "call_123".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"src/main.rs"}"#.into(),
                }],
            }),
            to_openai_message(&ChatMessage::ToolResult {
                tool_call_id: "call_123".into(),
                content: "fn main() {}".into(),
            }),
        ]);
    }

    #[test]
    fn preserves_system_and_multi_message_history() {
        round_trip(vec![
            to_openai_message(&ChatMessage::System("System".into())),
            to_openai_message(&ChatMessage::User("First".into())),
            to_openai_message(&ChatMessage::Assistant {
                text: Some("Second".into()),
                tool_calls: vec![],
            }),
            to_openai_message(&ChatMessage::User("Third".into())),
        ]);
    }

    #[test]
    fn request_round_trip_is_byte_identical() {
        let neutral_request = ChatRequest {
            model: "gpt-4o".into(),
            messages: vec![
                ChatMessage::System("System".into()),
                ChatMessage::User("Hello".into()),
                ChatMessage::Assistant {
                    text: Some("I will inspect it.".into()),
                    tool_calls: vec![ToolCall {
                        id: "call_123".into(),
                        name: "read_file".into(),
                        arguments: r#"{"path":"src/main.rs"}"#.into(),
                    }],
                },
                ChatMessage::ToolResult {
                    tool_call_id: "call_123".into(),
                    content: "fn main() {}".into(),
                },
            ],
            tools: Some(vec![ToolSpec {
                name: "read_file".into(),
                description: "Read a file".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }),
            }]),
            temperature: Some(0.5),
            max_tokens: None,
        };
        let original = to_openai_request(neutral_request);
        let neutral = from_openai_request(original.clone()).unwrap();
        let converted = to_openai_request(neutral);
        assert_eq!(
            to_string(&original).unwrap(),
            to_string(&converted).unwrap()
        );
    }

    #[test]
    fn response_conversion_keeps_all_choices() {
        use async_openai::types::{
            ChatChoiceStream, ChatCompletionStreamResponseDelta, CreateChatCompletionStreamResponse,
        };

        #[allow(deprecated)]
        let response = CreateChatCompletionStreamResponse {
            id: "id".into(),
            choices: vec![
                ChatChoiceStream {
                    index: 0,
                    delta: ChatCompletionStreamResponseDelta {
                        content: Some("first".into()),
                        function_call: None,
                        tool_calls: None,
                        role: None,
                        refusal: None,
                    },
                    finish_reason: None,
                    logprobs: None,
                },
                ChatChoiceStream {
                    index: 1,
                    delta: ChatCompletionStreamResponseDelta {
                        content: Some("second".into()),
                        function_call: None,
                        tool_calls: None,
                        role: None,
                        refusal: None,
                    },
                    finish_reason: None,
                    logprobs: None,
                },
            ],
            created: 0,
            model: "gpt-4o".into(),
            service_tier: None,
            system_fingerprint: None,
            object: "chat.completion.chunk".into(),
            usage: None,
        };

        assert_eq!(
            convert_response(response).text_delta,
            Some("firstsecond".into())
        );
    }
}
