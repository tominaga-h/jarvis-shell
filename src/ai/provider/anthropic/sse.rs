//! Anthropic Server-Sent Events framing and conversion.

use std::collections::{HashMap, VecDeque};

use anyhow::{anyhow, Context, Result};
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use tracing::warn;

use crate::ai::provider::types::{ChatChunk, ToolCallDelta};

#[derive(Debug)]
struct SseEvent {
    event: String,
    data: String,
}

/// A strict byte-oriented SSE parser. Keeping incomplete lines as bytes means
/// a UTF-8 scalar split across HTTP chunks is never replaced or corrupted.
#[derive(Debug, Default)]
struct SseParser {
    buffer: Vec<u8>,
    event: String,
    data_lines: Vec<String>,
}

impl SseParser {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line = self.buffer.drain(..=newline).collect::<Vec<_>>();
            let line = line.strip_suffix(b"\n").unwrap_or(&line);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let line =
                std::str::from_utf8(line).context("Anthropic SSE contained invalid UTF-8")?;
            self.process_line(line, &mut events);
        }
        Ok(events)
    }

    fn finish(&mut self) -> Result<Vec<SseEvent>> {
        if !self.buffer.is_empty() {
            let line = std::str::from_utf8(&self.buffer)
                .context("Anthropic SSE ended with invalid UTF-8")?
                .to_string();
            self.buffer.clear();
            let mut events = Vec::new();
            self.process_line(&line, &mut events);
            if !self.data_lines.is_empty() {
                self.process_line("", &mut events);
            }
            return Ok(events);
        }
        let mut events = Vec::new();
        if !self.data_lines.is_empty() {
            self.process_line("", &mut events);
        }
        Ok(events)
    }

    fn process_line(&mut self, line: &str, events: &mut Vec<SseEvent>) {
        if line.is_empty() {
            if !self.data_lines.is_empty() {
                events.push(SseEvent {
                    event: std::mem::take(&mut self.event),
                    data: self.data_lines.join("\n"),
                });
                self.data_lines.clear();
            } else {
                self.event.clear();
            }
            return;
        }
        if line.starts_with(':') {
            return;
        }
        if let Some(value) = line.strip_prefix("event:") {
            self.event = value.strip_prefix(' ').unwrap_or(value).to_string();
        } else if let Some(value) = line.strip_prefix("data:") {
            self.data_lines
                .push(value.strip_prefix(' ').unwrap_or(value).to_string());
        }
    }
}

#[derive(Debug, Default)]
struct EventState {
    last_stop_reason: Option<String>,
    tool_indices: HashMap<u32, u32>,
}

pub(super) fn stream_events<S, B>(bytes: S) -> impl Stream<Item = Result<ChatChunk>> + Send
where
    S: Stream<Item = std::result::Result<B, reqwest::Error>> + Send + Unpin + 'static,
    B: AsRef<[u8]> + Send + 'static,
{
    futures_util::stream::unfold(
        (
            bytes,
            SseParser::default(),
            EventState::default(),
            VecDeque::new(),
        ),
        |(mut bytes, mut parser, mut state, mut pending)| async move {
            loop {
                if let Some(chunk) = pending.pop_front() {
                    return Some((Ok(chunk), (bytes, parser, state, pending)));
                }

                match bytes.next().await {
                    Some(Ok(data)) => match parser.feed(data.as_ref()) {
                        Ok(events) => {
                            for event in events {
                                match event_to_chunk(event, &mut state) {
                                    Ok(Some(chunk)) => pending.push_back(chunk),
                                    Ok(None) => {}
                                    Err(error) => {
                                        return Some((Err(error), (bytes, parser, state, pending)));
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            return Some((Err(error), (bytes, parser, state, pending)));
                        }
                    },
                    Some(Err(error)) => {
                        return Some((
                            Err(anyhow!("Anthropic response stream failed: {error}")),
                            (bytes, parser, state, pending),
                        ));
                    }
                    None => match parser.finish() {
                        Ok(events) => {
                            for event in events {
                                match event_to_chunk(event, &mut state) {
                                    Ok(Some(chunk)) => pending.push_back(chunk),
                                    Ok(None) => {}
                                    Err(error) => {
                                        return Some((Err(error), (bytes, parser, state, pending)));
                                    }
                                }
                            }
                            if let Some(chunk) = pending.pop_front() {
                                return Some((Ok(chunk), (bytes, parser, state, pending)));
                            }
                            return None;
                        }
                        Err(error) => return Some((Err(error), (bytes, parser, state, pending))),
                    },
                }
            }
        },
    )
}

fn event_to_chunk(event: SseEvent, state: &mut EventState) -> Result<Option<ChatChunk>> {
    let value: Value = serde_json::from_str(&event.data)
        .with_context(|| format!("Invalid Anthropic SSE data for {}", event.event))?;
    let event_type = if event.event.is_empty() {
        value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    } else {
        event.event.as_str()
    };

    match event_type {
        "content_block_start" => {
            let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
            let block = value.get("content_block").unwrap_or(&Value::Null);
            if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                let tool_index = if let Some(tool_index) = state.tool_indices.get(&index) {
                    *tool_index
                } else {
                    let tool_index = state.tool_indices.len() as u32;
                    state.tool_indices.insert(index, tool_index);
                    tool_index
                };
                Ok(Some(ChatChunk {
                    text_delta: None,
                    tool_call_deltas: vec![ToolCallDelta {
                        index: tool_index,
                        id: block.get("id").and_then(Value::as_str).map(str::to_string),
                        name: block
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        arguments: None,
                    }],
                }))
            } else {
                Ok(None)
            }
        }
        "content_block_delta" => {
            let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
            let delta = value.get("delta").unwrap_or(&Value::Null);
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => Ok(Some(ChatChunk {
                    text_delta: delta
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    tool_call_deltas: Vec::new(),
                })),
                Some("input_json_delta") => Ok(Some(ChatChunk {
                    text_delta: None,
                    tool_call_deltas: vec![ToolCallDelta {
                        index: state.tool_indices.get(&index).copied().unwrap_or(index),
                        id: None,
                        name: None,
                        arguments: delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    }],
                })),
                // Thinking and signatures are intentionally dropped for now.
                // They are a natural extension point for --debug output later.
                Some("thinking_delta") | Some("signature_delta") | Some("citations_delta") => {
                    Ok(None)
                }
                _ => Ok(None),
            }
        }
        "message_delta" => {
            let reason = value
                .get("delta")
                .and_then(|delta| delta.get("stop_reason"))
                .and_then(Value::as_str)
                .map(str::to_string);
            state.last_stop_reason = reason.clone();
            Ok(reason_message(reason).map(|text| ChatChunk {
                text_delta: Some(text),
                tool_call_deltas: Vec::new(),
            }))
        }
        "error" => Err(error_from_value(&value)),
        "message_start" | "content_block_stop" | "message_stop" | "ping" => Ok(None),
        _ => {
            warn!(event = %event_type, "Ignoring unknown Anthropic SSE event");
            Ok(None)
        }
    }
}

fn reason_message(reason: Option<String>) -> Option<String> {
    match reason.as_deref() {
        Some("refusal") => Some("I’m sorry, but I can’t assist with that request.".to_string()),
        Some("max_tokens") => {
            Some("Response truncated; increase `[ai].max_tokens` (try 16384+).".to_string())
        }
        Some("pause_turn") => {
            Some("The response was paused by the provider; please try again.".to_string())
        }
        Some("model_context_window_exceeded") => Some(
            "The response exceeded the model context window; please shorten the request."
                .to_string(),
        ),
        Some("end_turn") | Some("stop_sequence") | Some("tool_use") | None => None,
        Some(other) => Some(format!("The provider stopped the response ({other}).")),
    }
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: Option<ErrorDetails>,
    request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ErrorDetails {
    message: Option<String>,
    #[allow(dead_code)]
    r#type: Option<String>,
}

fn error_from_value(value: &Value) -> anyhow::Error {
    let envelope = serde_json::from_value::<ErrorEnvelope>(value.clone()).ok();
    if let Some(request_id) = envelope
        .as_ref()
        .and_then(|error| error.request_id.as_deref())
    {
        tracing::warn!(request_id = %request_id, "Anthropic API returned an error event");
    }
    let message = envelope
        .and_then(|error| error.error)
        .and_then(|error| error.message)
        .unwrap_or_else(|| "Anthropic API returned an error".to_string());
    anyhow!("Anthropic API error: {message}")
}

pub(super) fn error_from_body(status: u16, body: &str) -> anyhow::Error {
    match serde_json::from_str::<Value>(body) {
        Ok(value) => {
            let error = error_from_value(&value);
            anyhow!("HTTP {status}: {error}")
        }
        Err(_) => anyhow!("Anthropic API returned HTTP {status}: {body}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    fn event(event: &str, data: &str) -> String {
        format!("event: {event}\ndata: {data}\n\n")
    }

    #[test]
    fn parses_multiline_data_with_spec_compliant_joining() {
        let mut parser = SseParser::default();
        let events = parser
            .feed(b"event: message_start\ndata: {\ndata: \"type\":\"message_start\"}\n\n")
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "{\n\"type\":\"message_start\"}");
    }

    #[tokio::test]
    async fn reassembles_utf8_across_byte_chunks() {
        let text = event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"😀"}}"#,
        );
        let bytes = text.as_bytes();
        let split = bytes
            .windows(4)
            .position(|window| window == "😀".as_bytes())
            .unwrap()
            + 1;
        let chunks = vec![
            Ok::<Vec<u8>, reqwest::Error>(bytes[..split].to_vec()),
            Ok::<Vec<u8>, reqwest::Error>(bytes[split..].to_vec()),
        ];
        let results = stream_events(stream::iter(chunks))
            .collect::<Vec<_>>()
            .await;
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].as_ref().unwrap().text_delta.as_deref(),
            Some("😀")
        );
    }

    #[tokio::test]
    async fn ping_does_not_reset_tool_call_accumulation() {
        let input = [
            event("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_1","name":"read_file","input":{}}}"#),
            event("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#),
            event("ping", r#"{"type":"ping"}"#),
            event("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"Cargo.toml\"}"}}"#),
        ]
        .concat();
        let results = stream_events(stream::iter(vec![Ok::<Vec<u8>, reqwest::Error>(
            input.into_bytes(),
        )]))
        .collect::<Vec<_>>()
        .await;
        let mut accumulators = Vec::new();
        for result in results {
            for delta in result.unwrap().tool_call_deltas {
                crate::ai::tools::call::accumulate_tool_call_delta(&mut accumulators, &delta);
            }
        }
        assert_eq!(accumulators[0].id, "call_1");
        assert_eq!(accumulators[0].function_name, "read_file");
        assert_eq!(accumulators[0].arguments, r#"{"path":"Cargo.toml"}"#);
    }

    #[test]
    fn maps_refusal_and_error_events_without_panicking() {
        let mut state = EventState::default();
        let refusal = event_to_chunk(
            SseEvent {
                event: "message_delta".into(),
                data: r#"{"type":"message_delta","delta":{"stop_reason":"refusal"}}"#.into(),
            },
            &mut state,
        )
        .unwrap()
        .unwrap();
        assert!(refusal.text_delta.unwrap().contains("can’t"));

        let error = event_to_chunk(
            SseEvent {
                event: "error".into(),
                data: r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad request"},"request_id":"req_1"}"#.into(),
            },
            &mut state,
        )
        .unwrap_err();
        assert!(error.to_string().contains("bad request"));
    }
}
