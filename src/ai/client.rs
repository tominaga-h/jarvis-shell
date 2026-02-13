//! OpenAI API クライアント — J.A.R.V.I.S. Brain
//!
//! ユーザー入力を AI に送信し、コマンドか自然言語かを判定する。
//! ストリーミングレスポンスに対応し、Tool Calling でコマンド実行を支援する。

use std::io::{self, Write};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};
use async_openai::{
    config::OpenAIConfig,
    types::{
        ChatCompletionMessageToolCallChunk, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessage, ChatCompletionRequestSystemMessageContent,
        ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent,
        ChatCompletionTool, ChatCompletionToolType, CreateChatCompletionRequest, FunctionObject,
    },
    Client,
};
use futures_util::StreamExt;

use crate::cli::jarvis::{jarvis_print_chunk, jarvis_print_end, jarvis_print_prefix, jarvis_spinner};

/// AI の判定結果
#[derive(Debug, Clone)]
pub enum AiResponse {
    /// ユーザー入力はシェルコマンドである。AI が返したコマンド文字列を含む。
    Command(String),
    /// ユーザー入力は自然言語である。AI の回答テキストを含む（ストリーミング済み）。
    NaturalLanguage(String),
}

const MODEL: &str = "gpt-4o-mini";

const SYSTEM_PROMPT: &str = r#"You are J.A.R.V.I.S., an AI assistant integrated into the terminal shell "jarvish".
You serve as the user's intelligent shell companion, like Tony Stark's AI butler.

Your role:
1. If the user's input is clearly a shell command (like `ls`, `git status`, `grep`, `cat`, `echo`, `mkdir`, `rm`, `cd`, `pwd`, `docker`, `cargo`, `npm`, `python`, etc.), call the `execute_shell_command` tool with the exact command. Do NOT add explanation — just call the tool.
2. If the user's input is natural language (a question, a request for help, a greeting, etc.), respond helpfully and concisely as Jarvis. Maintain the persona of an intelligent, loyal AI assistant.
3. When the user asks about errors or previous commands, use the provided command history context to give accurate, specific advice.
4. If the user asks in a specific language, respond in that same language.

Important guidelines:
- Be concise. Terminal output should be short and actionable.
- When suggesting fixes, provide the exact command the user should run.
- Maintain the "Iron Man J.A.R.V.I.S." persona: professional, helpful, with subtle dry wit.
- Address the user as "sir" occasionally."#;

/// J.A.R.V.I.S. AI クライアント
pub struct JarvisAI {
    client: Client<OpenAIConfig>,
}

impl JarvisAI {
    /// OPENAI_API_KEY 環境変数から AI クライアントを初期化する。
    pub fn new() -> Result<Self> {
        // async-openai は OPENAI_API_KEY 環境変数を自動で読み取る
        let api_key = std::env::var("OPENAI_API_KEY")
            .context("OPENAI_API_KEY is not set. AI features are disabled.")?;

        if api_key.is_empty() || api_key == "your_openai_api_key" {
            anyhow::bail!("OPENAI_API_KEY is not configured. Please set a valid API key in .env");
        }

        let config = OpenAIConfig::new().with_api_key(&api_key);
        let client = Client::with_config(config);
        Ok(Self { client })
    }

    /// ユーザー入力を AI に送信し、コマンドか自然言語かを判定する。
    /// 自然言語の場合はストリーミングでターミナルに表示しつつ、全文を返す。
    pub async fn process_input(&self, input: &str, context: &str) -> Result<AiResponse> {
        debug!(
            user_input = %input,
            context_length = context.len(),
            context_empty = context.is_empty(),
            "process_input() called"
        );

        let system_content = if context.is_empty() {
            SYSTEM_PROMPT.to_string()
        } else {
            format!("{SYSTEM_PROMPT}\n\n{context}")
        };

        debug!(
            system_prompt_length = system_content.len(),
            "System prompt assembled"
        );
        debug!(system_prompt = %system_content, "Full system prompt content");

        let messages = vec![
            ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(system_content),
                name: None,
            }),
            ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(input.to_string()),
                name: None,
            }),
        ];

        let tools = vec![Self::shell_command_tool()];

        let request = CreateChatCompletionRequest {
            model: MODEL.to_string(),
            messages,
            tools: Some(tools),
            stream: Some(true),
            ..Default::default()
        };

        debug!(
            model = MODEL,
            message_count = 2,
            tools_count = 1,
            stream = true,
            "Sending API request to OpenAI"
        );

        // ローディングスピナーを開始
        let spinner = jarvis_spinner();

        // スピナー表示確認用に3秒スリープ
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let mut stream = match self
            .client
            .chat()
            .create_stream(request)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                spinner.finish_and_clear();
                return Err(anyhow::anyhow!(e).context("Failed to create chat stream"));
            }
        };

        debug!("Stream created successfully, starting to process chunks");

        // ストリーミング処理: テキスト応答と Tool Call を分離して処理
        let mut full_text = String::new();
        let mut tool_calls: Vec<ToolCallAccumulator> = Vec::new();
        let mut started_text = false;
        let mut spinner_cleared = false;
        let mut chunk_count: u32 = 0;

        while let Some(result) = stream.next().await {
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
                        Self::accumulate_tool_call(&mut tool_calls, chunk);
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

        // ストリーム完了: スピナーがまだ残っていればクリア
        if !spinner_cleared {
            spinner.finish_and_clear();
        }

        if started_text {
            jarvis_print_end();
        }

        debug!(
            total_chunks = chunk_count,
            full_text_length = full_text.len(),
            tool_calls_count = tool_calls.len(),
            started_text = started_text,
            "Stream processing completed"
        );

        // Tool Call があればコマンドとして返す
        if let Some(cmd) = Self::extract_command(&tool_calls) {
            info!(
                response_type = "Command",
                command = %cmd,
                "AI response: execute command"
            );
            return Ok(AiResponse::Command(cmd));
        }

        // テキスト応答を返す
        if full_text.is_empty() {
            warn!(
                user_input = %input,
                total_chunks = chunk_count,
                tool_calls_count = tool_calls.len(),
                "AI returned empty response (no text, no tool calls) — this may be the cause of TODO #5"
            );
        } else {
            info!(
                response_type = "NaturalLanguage",
                response_length = full_text.len(),
                "AI response: natural language"
            );
        }

        Ok(AiResponse::NaturalLanguage(full_text))
    }

    /// execute_shell_command ツールの定義
    fn shell_command_tool() -> ChatCompletionTool {
        ChatCompletionTool {
            r#type: ChatCompletionToolType::Function,
            function: FunctionObject {
                name: "execute_shell_command".to_string(),
                description: Some(
                    "Execute a shell command. Use this when the user's input is a shell command."
                        .to_string(),
                ),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The full shell command to execute"
                        }
                    },
                    "required": ["command"]
                })),
                strict: None,
            },
        }
    }

    /// ストリーミングで受信した Tool Call チャンクを蓄積する
    fn accumulate_tool_call(
        accumulators: &mut Vec<ToolCallAccumulator>,
        chunk: &ChatCompletionMessageToolCallChunk,
    ) {
        let idx = chunk.index as usize;

        // 必要に応じてアキュムレータを拡張
        while accumulators.len() <= idx {
            accumulators.push(ToolCallAccumulator::default());
        }

        let acc = &mut accumulators[idx];

        if let Some(ref id) = chunk.id {
            acc.id = id.clone();
        }
        if let Some(ref func) = chunk.function {
            if let Some(ref name) = func.name {
                acc.function_name = name.clone();
            }
            if let Some(ref args) = func.arguments {
                acc.arguments.push_str(args);
            }
        }
    }

    /// 蓄積した Tool Call からコマンド文字列を抽出する
    fn extract_command(tool_calls: &[ToolCallAccumulator]) -> Option<String> {
        for tc in tool_calls {
            debug!(
                function_name = %tc.function_name,
                arguments = %tc.arguments,
                id = %tc.id,
                "Processing tool call"
            );
            if tc.function_name == "execute_shell_command" {
                // arguments は JSON 文字列: {"command": "ls -la"}
                match serde_json::from_str::<serde_json::Value>(&tc.arguments) {
                    Ok(parsed) => {
                        if let Some(cmd) = parsed.get("command").and_then(|v| v.as_str()) {
                            debug!(extracted_command = %cmd, "Successfully extracted command from tool call");
                            return Some(cmd.to_string());
                        }
                        warn!(parsed = %parsed, "Tool call JSON parsed but 'command' field not found");
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            raw_arguments = %tc.arguments,
                            "Failed to parse tool call arguments as JSON"
                        );
                    }
                }
            }
        }
        None
    }
}

/// Tool Call のストリーミングチャンクを蓄積するための構造体
#[derive(Debug, Default, Clone)]
struct ToolCallAccumulator {
    #[allow(dead_code)]
    id: String,
    function_name: String,
    arguments: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_fails_without_api_key() {
        // OPENAI_API_KEY が空の場合にエラーを返すことを確認
        let original = std::env::var("OPENAI_API_KEY").ok();
        std::env::remove_var("OPENAI_API_KEY");

        let result = JarvisAI::new();
        assert!(result.is_err());

        // 元に戻す
        if let Some(key) = original {
            std::env::set_var("OPENAI_API_KEY", key);
        }
    }

    #[test]
    fn extract_command_from_tool_calls() {
        let tool_calls = vec![ToolCallAccumulator {
            id: "call_123".to_string(),
            function_name: "execute_shell_command".to_string(),
            arguments: r#"{"command": "ls -la"}"#.to_string(),
        }];

        let cmd = JarvisAI::extract_command(&tool_calls);
        assert_eq!(cmd, Some("ls -la".to_string()));
    }

    #[test]
    fn extract_command_returns_none_for_empty() {
        let tool_calls: Vec<ToolCallAccumulator> = Vec::new();
        let cmd = JarvisAI::extract_command(&tool_calls);
        assert!(cmd.is_none());
    }

    #[test]
    fn extract_command_handles_invalid_json() {
        let tool_calls = vec![ToolCallAccumulator {
            id: "call_456".to_string(),
            function_name: "execute_shell_command".to_string(),
            arguments: "invalid json".to_string(),
        }];

        let cmd = JarvisAI::extract_command(&tool_calls);
        assert!(cmd.is_none());
    }
}
