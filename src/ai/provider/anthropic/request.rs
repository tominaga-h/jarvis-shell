//! Conversion from neutral chat requests to Anthropic Messages JSON.

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::ai::provider::types::{ChatMessage, ChatRequest, ToolCall, ToolSpec};

#[derive(Debug, Serialize)]
pub(super) struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: &'static str,
    content: AnthropicContent,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicBlock>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum AnthropicBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: Value,
}

impl TryFrom<ChatRequest> for AnthropicRequest {
    type Error = anyhow::Error;

    fn try_from(request: ChatRequest) -> Result<Self> {
        let max_tokens = request
            .max_tokens
            .context("Anthropic requests require max_tokens")?;
        let mut system_parts = Vec::new();
        let mut messages = Vec::new();
        let mut pending_tool_results = Vec::new();

        for message in request.messages {
            match message {
                ChatMessage::System(content) => system_parts.push(content),
                ChatMessage::ToolResult {
                    tool_call_id,
                    content,
                } => pending_tool_results.push(AnthropicBlock::ToolResult {
                    tool_use_id: tool_call_id,
                    content,
                }),
                ChatMessage::User(content) => {
                    flush_tool_results(&mut messages, &mut pending_tool_results);
                    messages.push(AnthropicMessage {
                        role: "user",
                        content: AnthropicContent::Text(content),
                    });
                }
                ChatMessage::Assistant { text, tool_calls } => {
                    flush_tool_results(&mut messages, &mut pending_tool_results);
                    messages.push(assistant_message(text, tool_calls)?);
                }
            }
        }
        flush_tool_results(&mut messages, &mut pending_tool_results);

        Ok(Self {
            model: request.model,
            max_tokens,
            messages,
            system: (!system_parts.is_empty()).then(|| system_parts.join("\n\n")),
            tools: request
                .tools
                .map(|tools| tools.into_iter().map(AnthropicTool::try_from).collect())
                .transpose()?,
            // Temperature is deliberately not represented here. Anthropic rejects it
            // for current Claude models; the neutral request value is ignored.
            stream: true,
        })
    }
}

fn flush_tool_results(messages: &mut Vec<AnthropicMessage>, pending: &mut Vec<AnthropicBlock>) {
    if !pending.is_empty() {
        messages.push(AnthropicMessage {
            role: "user",
            content: AnthropicContent::Blocks(std::mem::take(pending)),
        });
    }
}

fn assistant_message(text: Option<String>, tool_calls: Vec<ToolCall>) -> Result<AnthropicMessage> {
    if tool_calls.is_empty() {
        return Ok(AnthropicMessage {
            role: "assistant",
            content: AnthropicContent::Text(text.unwrap_or_default()),
        });
    }

    let mut blocks = Vec::with_capacity(tool_calls.len() + usize::from(text.is_some()));
    if let Some(text) = text {
        blocks.push(AnthropicBlock::Text { text });
    }
    for tool_call in tool_calls {
        let tool_call_id = tool_call.id;
        blocks.push(AnthropicBlock::ToolUse {
            id: tool_call_id.clone(),
            name: tool_call.name,
            input: serde_json::from_str(&tool_call.arguments)
                .with_context(|| format!("Invalid JSON arguments for tool call {tool_call_id}"))?,
        });
    }
    Ok(AnthropicMessage {
        role: "assistant",
        content: AnthropicContent::Blocks(blocks),
    })
}

impl TryFrom<ToolSpec> for AnthropicTool {
    type Error = anyhow::Error;

    fn try_from(tool: ToolSpec) -> Result<Self> {
        Ok(Self {
            name: tool.name,
            description: tool.description,
            input_schema: tool.parameters,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(messages: Vec<ChatMessage>) -> AnthropicRequest {
        AnthropicRequest::try_from(ChatRequest {
            model: "claude-sonnet-5".into(),
            messages,
            tools: None,
            temperature: Some(0.7),
            max_tokens: Some(16_384),
        })
        .unwrap()
    }

    #[test]
    fn extracts_system_message_to_top_level() {
        let value = serde_json::to_value(request(vec![
            ChatMessage::System("Be helpful".into()),
            ChatMessage::User("Hello".into()),
        ]))
        .unwrap();
        assert_eq!(value["system"], "Be helpful");
        assert_eq!(value["messages"].as_array().unwrap().len(), 1);
        assert!(value["messages"][0]["role"] == "user");
    }

    #[test]
    fn bundles_consecutive_tool_results() {
        let value = serde_json::to_value(request(vec![
            ChatMessage::Assistant {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call".into(),
                    name: "read_file".into(),
                    arguments: "{}".into(),
                }],
            },
            ChatMessage::ToolResult {
                tool_call_id: "one".into(),
                content: "first".into(),
            },
            ChatMessage::ToolResult {
                tool_call_id: "two".into(),
                content: "second".into(),
            },
            ChatMessage::ToolResult {
                tool_call_id: "three".into(),
                content: "third".into(),
            },
        ]))
        .unwrap();
        let messages = value["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn does_not_bundle_tool_results_across_user_message() {
        let value = serde_json::to_value(request(vec![
            ChatMessage::ToolResult {
                tool_call_id: "one".into(),
                content: "first".into(),
            },
            ChatMessage::User("intervening".into()),
            ChatMessage::ToolResult {
                tool_call_id: "two".into(),
                content: "second".into(),
            },
        ]))
        .unwrap();
        let messages = value["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["content"].as_array().unwrap().len(), 1);
        assert_eq!(messages[2]["content"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn converts_tools_to_input_schema_without_changing_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        });
        let value = serde_json::to_value(
            AnthropicRequest::try_from(ChatRequest {
                model: "claude".into(),
                messages: vec![ChatMessage::User("read".into())],
                tools: Some(vec![ToolSpec {
                    name: "read_file".into(),
                    description: "Read".into(),
                    parameters: schema.clone(),
                }]),
                temperature: None,
                max_tokens: Some(1),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(value["tools"][0]["input_schema"], schema);
        assert!(value["tools"][0].get("parameters").is_none());
    }

    #[test]
    fn omits_temperature_and_requires_max_tokens() {
        let value = serde_json::to_value(request(vec![ChatMessage::User("Hi".into())])).unwrap();
        assert_eq!(value["max_tokens"], 16_384);
        assert!(value.get("temperature").is_none());
        assert!(AnthropicRequest::try_from(ChatRequest {
            model: "claude".into(),
            messages: vec![ChatMessage::User("Hi".into())],
            tools: None,
            temperature: None,
            max_tokens: None,
        })
        .is_err());
    }

    #[test]
    fn converts_assistant_tool_calls_to_tool_use_blocks() {
        let value = serde_json::to_value(request(vec![ChatMessage::Assistant {
            text: Some("I will inspect it".into()),
            tool_calls: vec![ToolCall {
                id: "call_123".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"Cargo.toml"}"#.into(),
            }],
        }]))
        .unwrap();
        let blocks = value["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "call_123");
        assert_eq!(blocks[1]["input"]["path"], "Cargo.toml");
    }
}
