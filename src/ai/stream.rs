//! AI ストリーミングレスポンス処理
//!
//! プロバイダ中立のストリームを消費し、テキスト応答と Tool Call を分離して返す。
//! Ctrl-C (SIGINT) による中断にも対応する。

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{debug, info, warn};

use std::io::{IsTerminal, Write};
use std::time::Instant;

use crate::ai::provider::types::{ChatChunk, ChatRequest};
use crate::ai::provider::AiBackend;
use crate::cli::color::red;
use crate::cli::jarvis::{
    jarvis_print_plain, jarvis_render_markdown, jarvis_spinner, render_markdown, JarvisSpinner,
};

use super::markdown::is_markdown;
use super::tools::call::{accumulate_tool_call_delta, ToolCallAccumulator};

/// ストリーム処理の結果
pub struct StreamResult {
    /// ストリーミングで受信したテキスト全文
    pub full_text: String,
    /// 蓄積された Tool Call
    pub tool_calls: Vec<ToolCallAccumulator>,
    /// Ctrl-C (SIGINT) でストリームが中断されたかどうか
    pub interrupted: bool,
}

/// エージェント用ストリームを処理する。
pub async fn process_stream(
    backend: &AiBackend,
    request: ChatRequest,
    is_first_round: bool,
    markdown_rendering: bool,
) -> Result<StreamResult> {
    process_stream_common(
        backend,
        request,
        is_first_round,
        markdown_rendering,
        Some(Vec::new()),
        false,
    )
    .await
}

/// AI パイプ用ストリームを処理する。
pub async fn process_ai_pipe_stream(
    backend: &AiBackend,
    request: ChatRequest,
    markdown_rendering: bool,
) -> Result<String> {
    let result =
        process_stream_common(backend, request, false, markdown_rendering, None, true).await?;
    Ok(result.full_text)
}

/// エージェントとパイプで共有する SSE 消費処理。
///
/// `tools` が `Some` の場合は Tool Call を蓄積し、`None` の場合は従来の
/// パイプ動作どおり Tool Call を無視する。
async fn process_stream_common(
    backend: &AiBackend,
    request: ChatRequest,
    is_first_round: bool,
    markdown_rendering: bool,
    tools: Option<Vec<ToolCallAccumulator>>,
    pipe_mode: bool,
) -> Result<StreamResult> {
    let stream_start_time = std::time::Instant::now();
    let mut sigint =
        signal(SignalKind::interrupt()).context("Failed to register SIGINT handler")?;
    let mut spinner = jarvis_spinner();
    tracing::debug!(
        target: "jarvish::ai::stream",
        stdout_is_tty = std::io::stdout().is_terminal(),
        stderr_is_tty = std::io::stderr().is_terminal(),
        "stream processing environment check"
    );

    let mut stream = tokio::select! {
        result = backend.create_stream(request) => {
            match result {
                Ok(stream) => stream,
                Err(error) => {
                    spinner.finish_and_clear();
                    return Err(error);
                }
            }
        }
        _ = sigint.recv() => {
            info!("Ctrl-C received while waiting for API connection, interrupting");
            spinner.finish_and_clear();
            return Ok(StreamResult {
                full_text: String::new(),
                tool_calls: tools.unwrap_or_default(),
                interrupted: true,
            });
        }
    };

    if pipe_mode {
        spinner.set_message("Thinking...");
    }

    let mut state = StreamState {
        full_text: String::new(),
        started_text: false,
        tools,
        spinner,
        last_spinner_update: Instant::now(),
        markdown_rendering,
        pipe_mode,
        chunk_count: 0,
        stream_start_time,
    };
    let mut interrupted = false;

    loop {
        tokio::select! {
            chunk = stream.next() => {
                let result = match chunk {
                    Some(result) => result,
                    None => break,
                };
                state.chunk_count += 1;
                let chunk = match result {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        warn!(
                            error = %error,
                            chunks_received = state.chunk_count,
                            text_so_far_len = state.full_text.len(),
                            "Stream error occurred"
                        );
                        state.spinner.finish_and_clear();
                        anyhow::bail!("Stream error: {error}");
                    }
                };
                consume_chunk(&mut state, chunk);
            }
            _ = sigint.recv() => {
                info!(
                    interrupted_at_chunk = state.chunk_count,
                    interrupted_at_text_len = state.full_text.len(),
                    elapsed_ms = stream_start_time.elapsed().as_millis() as u64,
                    "Ctrl-C received during AI streaming, interrupting"
                );
                interrupted = true;
                break;
            }
        }
    }

    if pipe_mode {
        finish_pipe_output(
            &mut state.spinner,
            &state.full_text,
            state.started_text,
            interrupted,
            markdown_rendering,
        );
    } else {
        if state.started_text {
            state.spinner.set_message("Rendering...");
        }
        state.spinner.finish_and_clear();
        if state.started_text {
            let render: fn(&str) = if markdown_rendering && is_markdown(&state.full_text) {
                jarvis_render_markdown
            } else {
                jarvis_print_plain
            };
            let display_text = if interrupted {
                format!("{}\n\n{}", state.full_text, red("[interrupted]"))
            } else {
                state.full_text.clone()
            };
            render(&display_text);
            let mut stdout_handle = std::io::stdout();
            let flush_result = stdout_handle.flush();
            tracing::debug!(
                target: "jarvish::ai::stream",
                flush_ok = flush_result.is_ok(),
                "post-render flush attempted"
            );
        }
    }

    debug!(
        total_chunks = state.chunk_count,
        full_text_length = state.full_text.len(),
        duration_ms = stream_start_time.elapsed().as_millis() as u64,
        tool_calls_count = state.tools.as_ref().map_or(0, Vec::len),
        started_text = state.started_text,
        is_first_round,
        interrupted,
        pipe_mode = state.pipe_mode,
        "Stream processing completed"
    );

    Ok(StreamResult {
        full_text: state.full_text,
        tool_calls: state.tools.unwrap_or_default(),
        interrupted,
    })
}

struct StreamState {
    full_text: String,
    started_text: bool,
    tools: Option<Vec<ToolCallAccumulator>>,
    spinner: JarvisSpinner,
    last_spinner_update: Instant,
    markdown_rendering: bool,
    pipe_mode: bool,
    chunk_count: u32,
    stream_start_time: Instant,
}

fn consume_chunk(state: &mut StreamState, chunk: ChatChunk) {
    if let Some(content) = chunk.text_delta {
        if state.pipe_mode && !state.started_text && !state.markdown_rendering {
            state.spinner.finish_and_clear();
        }
        state.full_text.push_str(&content);
        state.started_text = true;
        if (!state.pipe_mode || state.markdown_rendering)
            && state.last_spinner_update.elapsed().as_millis() > 100
        {
            let elapsed_secs = state.stream_start_time.elapsed().as_secs();
            let message = format!(
                "Buffering... {}s elapsed, {} bytes ({} chunks)",
                elapsed_secs,
                state.full_text.len(),
                state.chunk_count
            );
            state.spinner.set_message(&message);
            state.last_spinner_update = Instant::now();
        } else if state.pipe_mode && !state.markdown_rendering {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(content.as_bytes());
            let _ = out.flush();
        }
    }

    if let Some(tool_calls) = &mut state.tools {
        for delta in chunk.tool_call_deltas {
            accumulate_tool_call_delta(tool_calls, &delta);
        }
    }
}

fn finish_pipe_output(
    spinner: &mut JarvisSpinner,
    full_text: &str,
    started: bool,
    interrupted: bool,
    markdown_rendering: bool,
) {
    if markdown_rendering {
        if started {
            spinner.set_message("Rendering...");
        }
        spinner.finish_and_clear();
        if started {
            if is_markdown(full_text) {
                let display_text = if interrupted {
                    format!("{}\n\n{}", full_text, red("[interrupted]"))
                } else {
                    full_text.to_string()
                };
                render_markdown(&display_text);
            } else {
                print!("{full_text}");
                if !full_text.ends_with('\n') {
                    println!();
                }
                if interrupted {
                    eprintln!("{}", red("[interrupted]"));
                }
            }
        }
    } else {
        if !started {
            spinner.finish_and_clear();
        }
        if started {
            let mut out = std::io::stdout().lock();
            if !full_text.ends_with('\n') {
                let _ = out.write_all(b"\n");
            }
            let _ = out.flush();
        }
        if interrupted {
            eprintln!("{}", red("[interrupted]"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::provider::types::ToolCallDelta;

    #[test]
    fn shared_consumer_produces_agent_stream_result_shape() {
        let mut state = StreamState {
            full_text: String::new(),
            started_text: false,
            tools: Some(Vec::new()),
            spinner: JarvisSpinner::new("test"),
            last_spinner_update: Instant::now(),
            markdown_rendering: false,
            pipe_mode: false,
            chunk_count: 1,
            stream_start_time: Instant::now(),
        };

        consume_chunk(
            &mut state,
            ChatChunk {
                text_delta: Some("answer".into()),
                tool_call_deltas: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_1".into()),
                    name: Some("read_file".into()),
                    arguments: Some(r#"{"path":"a.txt"}"#.into()),
                }],
            },
        );

        let result = StreamResult {
            full_text: state.full_text,
            tool_calls: state.tools.unwrap(),
            interrupted: false,
        };
        assert_eq!(result.full_text, "answer");
        assert!(!result.interrupted);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].function_name, "read_file");
        assert_eq!(result.tool_calls[0].arguments, r#"{"path":"a.txt"}"#);
    }
}
