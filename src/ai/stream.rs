//! AI ストリーミングレスポンス処理
//!
//! OpenAI API からのストリーミングレスポンスを処理し、
//! テキスト応答と Tool Call を分離して返す。
//! Ctrl-C (SIGINT) による中断にも対応する。

use std::io::{self, Write};

use anyhow::{Context, Result};
use async_openai::{config::OpenAIConfig, types::CreateChatCompletionRequest, Client};
use futures_util::StreamExt;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{debug, info, warn};

use crate::cli::color::red;
use crate::cli::jarvis::{
    jarvis_print_chunk, jarvis_print_end, jarvis_print_prefix, jarvis_spinner,
};

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
    let mut spinner_cleared = false;
    let mut chunk_count: u32 = 0;
    let mut interrupted = false;

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
                        if !spinner_cleared {
                            spinner.finish_and_clear();
                        }
                        if started_text {
                            jarvis_print_end();
                        }
                        anyhow::bail!("Stream error: {e}");
                    }
                };

                for choice in &response.choices {
                    let delta = &choice.delta;

                    // テキスト応答の処理
                    if let Some(ref content) = delta.content {
                        debug!(
                            chunk = chunk_count,
                            content_length = content.len(),
                            has_content = true,
                            content = %content,
                            "Received text chunk"
                        );
                        if !started_text {
                            if !spinner_cleared {
                                spinner.finish_and_clear();
                                spinner_cleared = true;
                            }
                            jarvis_print_prefix();
                            started_text = true;
                        }
                        jarvis_print_chunk(content);
                        let _ = io::stdout().flush();
                        full_text.push_str(content);
                    }

                    // Tool Call の処理
                    if let Some(ref tc_chunks) = delta.tool_calls {
                        if !spinner_cleared {
                            spinner.finish_and_clear();
                            spinner_cleared = true;
                        }
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

    // ストリーム完了 or 中断: スピナーがまだ残っていればクリア
    if !spinner_cleared {
        spinner.finish_and_clear();
    }

    if started_text {
        if interrupted {
            // 中断時は末尾に [中断] を表示
            jarvis_print_chunk(&red(" [interrupted]"));
            let _ = io::stdout().flush();
        }
        jarvis_print_end();
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
