//! AI ストリーミングレスポンス処理
//!
//! OpenAI API からのストリーミングレスポンスを処理し、
//! テキスト応答と Tool Call を分離して返す。
//! Ctrl-C (SIGINT) による中断にも対応する。

use anyhow::{Context, Result};
use async_openai::{config::OpenAIConfig, types::CreateChatCompletionRequest, Client};
use futures_util::StreamExt;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{debug, info, warn};

use std::io::Write;
use std::time::Instant;

use crate::cli::color::red;
use crate::cli::jarvis::{
    jarvis_print_plain, jarvis_render_markdown, jarvis_spinner, render_markdown,
};

use super::markdown::is_markdown;
use super::tools::call::{accumulate_tool_call, ToolCallAccumulator};

/// ストリーム処理の結果
pub struct StreamResult {
    /// ストリーミングで受信したテキスト全文
    pub full_text: String,
    /// 蓄積された Tool Call
    pub tool_calls: Vec<ToolCallAccumulator>,
    /// Ctrl-C (SIGINT) でストリームが中断されたかどうか
    pub interrupted: bool,
}

/// ストリーミングレスポンスを処理し、テキストと Tool Call を分離して返す。
///
/// `is_first_round`: true の場合、初回ラウンドでスピナーを表示する。
/// 後続ラウンドではツール実行中のメッセージを表示する。
pub async fn process_stream(
    client: &Client<OpenAIConfig>,
    request: CreateChatCompletionRequest,
    is_first_round: bool,
    markdown_rendering: bool,
) -> Result<StreamResult> {
    // SIGINT (Ctrl-C) リスナーを作成。
    // tokio::signal::unix::signal() は作成時点以降のシグナルのみ受け取るため、
    // コマンド実行中などに発生した過去の SIGINT の影響を受けない。
    let mut sigint =
        signal(SignalKind::interrupt()).context("Failed to register SIGINT handler")?;

    // ローディングスピナーを開始
    let spinner = jarvis_spinner();

    // API 接続待ちも Ctrl-C で中断できるようにする
    let chat = client.chat();
    let mut stream = tokio::select! {
        result = chat.create_stream(request) => {
            match result {
                Ok(s) => s,
                Err(e) => {
                    spinner.finish_and_clear();
                    return Err(anyhow::anyhow!(e).context("Failed to create chat stream"));
                }
            }
        }
        _ = sigint.recv() => {
            info!("Ctrl-C received while waiting for API connection, interrupting");
            spinner.finish_and_clear();
            return Ok(StreamResult {
                full_text: String::new(),
                tool_calls: vec![],
                interrupted: true,
            });
        }
    };

    debug!("Stream created successfully, starting to process chunks");

    // ストリーミング処理: テキスト応答と Tool Call を分離して処理
    let mut full_text = String::new();
    let mut tool_calls: Vec<ToolCallAccumulator> = Vec::new();
    let mut started_text = false;
    let mut chunk_count: u32 = 0;
    let mut interrupted = false;
    let mut last_spinner_update = Instant::now();

    loop {
        tokio::select! {
            chunk = stream.next() => {
                let result = match chunk {
                    Some(r) => r,
                    None => break, // ストリーム終了
                };

                chunk_count += 1;
                let response = match result {
                    Ok(r) => r,
                    Err(e) => {
                        // ストリームエラーは警告を出して中断
                        warn!(
                            error = %e,
                            chunks_received = chunk_count,
                            text_so_far_len = full_text.len(),
                            "Stream error occurred"
                        );
                        spinner.finish_and_clear();
                        anyhow::bail!("Stream error: {e}");
                    }
                };

                for choice in &response.choices {
                    let delta = &choice.delta;

                    // テキスト応答の処理（バッファリング）
                    if let Some(ref content) = delta.content {
                        debug!(
                            chunk = chunk_count,
                            content_length = content.len(),
                            has_content = true,
                            content = %content,
                            "Received text chunk"
                        );
                        full_text.push_str(content);
                        started_text = true;
                        if last_spinner_update.elapsed().as_millis() > 100 {
                            spinner.set_message(
                                format!("Buffering stream... {} bytes", full_text.len()),
                            );
                            last_spinner_update = Instant::now();
                        }
                    }

                    // Tool Call の処理
                    if let Some(ref tc_chunks) = delta.tool_calls {
                        debug!(
                            chunk = chunk_count,
                            tool_call_chunks = tc_chunks.len(),
                            "Received tool call chunk"
                        );
                        for chunk in tc_chunks {
                            accumulate_tool_call(&mut tool_calls, chunk);
                        }
                    }

                    // content も tool_calls もない場合のログ
                    if delta.content.is_none() && delta.tool_calls.is_none() {
                        debug!(
                            chunk = chunk_count,
                            role = ?delta.role,
                            "Received chunk with no content and no tool_calls"
                        );
                    }
                }
            }
            _ = sigint.recv() => {
                info!(
                    chunks_received = chunk_count,
                    text_so_far_len = full_text.len(),
                    "Ctrl-C received during AI streaming, interrupting"
                );
                interrupted = true;
                break;
            }
        }
    }

    // ストリーム完了 or 中断: Markdown レンダリングして表示
    if started_text {
        spinner.set_message("Rendering...");
    }
    spinner.finish_and_clear();

    if started_text {
        let render = if markdown_rendering && is_markdown(&full_text) {
            jarvis_render_markdown
        } else {
            jarvis_print_plain
        };
        if interrupted {
            let display_text = format!("{}\n\n{}", full_text, red("[interrupted]"));
            render(&display_text);
        } else {
            render(&full_text);
        }
    }

    debug!(
        total_chunks = chunk_count,
        full_text_length = full_text.len(),
        tool_calls_count = tool_calls.len(),
        started_text = started_text,
        is_first_round = is_first_round,
        interrupted = interrupted,
        "Stream processing completed"
    );

    Ok(StreamResult {
        full_text,
        tool_calls,
        interrupted,
    })
}

/// AI パイプ用ストリーミングレスポンスを処理する。
///
/// 通常の `process_stream()` とは異なり:
/// - Jarvis ペルソナの装飾なし（🤵 プレフィックスなし）
/// - Tool Call は無視（テキスト応答のみ）
///
/// `markdown_rendering` が `true` の場合:
///   バッファリングモードで動作し、完了後に `is_markdown()` で判定。
///   Markdown であれば `render_markdown()` でレンダリングする。
///
/// `markdown_rendering` が `false` の場合:
///   従来通りチャンクを即時 stdout に流す（tee パターン）。
///
/// 返却値: AI が出力したテキスト全文（`CommandResult.stdout` に格納用）
pub async fn process_ai_pipe_stream(
    client: &Client<OpenAIConfig>,
    request: CreateChatCompletionRequest,
    markdown_rendering: bool,
) -> Result<String> {
    let mut sigint =
        signal(SignalKind::interrupt()).context("Failed to register SIGINT handler")?;

    let spinner = jarvis_spinner();

    let chat = client.chat();
    let mut stream = tokio::select! {
        result = chat.create_stream(request) => {
            match result {
                Ok(s) => s,
                Err(e) => {
                    spinner.finish_and_clear();
                    return Err(anyhow::anyhow!(e).context("Failed to create chat stream"));
                }
            }
        }
        _ = sigint.recv() => {
            info!("Ctrl-C received while waiting for AI pipe API connection");
            spinner.finish_and_clear();
            return Ok(String::new());
        }
    };

    spinner.set_message("Thinking...");

    let mut full_text = String::new();
    let mut started = false;
    let mut interrupted = false;
    let mut last_spinner_update = Instant::now();

    loop {
        tokio::select! {
            chunk = stream.next() => {
                let result = match chunk {
                    Some(r) => r,
                    None => break,
                };

                let response = match result {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(error = %e, "AI pipe stream error");
                        spinner.finish_and_clear();
                        anyhow::bail!("Stream error: {e}");
                    }
                };

                for choice in &response.choices {
                    if let Some(ref content) = choice.delta.content {
                        if !started {
                            if !markdown_rendering {
                                spinner.finish_and_clear();
                            }
                            started = true;
                        }
                        full_text.push_str(content);

                        if markdown_rendering {
                            if last_spinner_update.elapsed().as_millis() > 100 {
                                spinner.set_message(
                                    format!("Buffering stream... {} bytes", full_text.len()),
                                );
                                last_spinner_update = Instant::now();
                            }
                        } else {
                            let mut out = std::io::stdout().lock();
                            let _ = out.write_all(content.as_bytes());
                            let _ = out.flush();
                        }
                    }
                }
            }
            _ = sigint.recv() => {
                info!("Ctrl-C received during AI pipe streaming");
                interrupted = true;
                break;
            }
        }
    }

    if markdown_rendering {
        if started {
            spinner.set_message("Rendering...");
        }
        spinner.finish_and_clear();

        if started {
            if is_markdown(&full_text) {
                if interrupted {
                    let display_text = format!("{}\n\n{}", full_text, red("[interrupted]"));
                    render_markdown(&display_text);
                } else {
                    render_markdown(&full_text);
                }
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

    debug!(
        full_text_length = full_text.len(),
        interrupted = interrupted,
        markdown_rendering = markdown_rendering,
        "AI pipe stream processing completed"
    );

    Ok(full_text)
}
