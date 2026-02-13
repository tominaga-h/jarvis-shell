//! OpenAI API クライアント — J.A.R.V.I.S. Brain
//!
//! ユーザー入力を AI に送信し、コマンドか自然言語かを判定する。
//! ストリーミングレスポンスに対応し、Tool Calling でコマンド実行を支援する。
//! エージェントループにより、複数ステップのファイル操作（読み取り→編集→書き込み）が可能。

use std::io::{self, Write};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};
use async_openai::{
    config::OpenAIConfig,
    types::{
        ChatCompletionMessageToolCall, ChatCompletionMessageToolCallChunk,
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestSystemMessageContent, ChatCompletionRequestToolMessage,
        ChatCompletionRequestToolMessageContent, ChatCompletionRequestUserMessage,
        ChatCompletionRequestUserMessageContent, ChatCompletionTool, ChatCompletionToolType,
        CreateChatCompletionRequest, FunctionCall, FunctionObject,
    },
    Client,
};
use futures_util::StreamExt;

use crate::cli::jarvis::{jarvis_print_chunk, jarvis_print_end, jarvis_print_prefix, jarvis_spinner, jarvis_talk};

/// AI の判定結果
#[derive(Debug, Clone)]
pub enum AiResponse {
    /// ユーザー入力はシェルコマンドである。AI が返したコマンド文字列を含む。
    Command(String),
    /// ユーザー入力は自然言語である。AI の回答テキストを含む（ストリーミング済み）。
    NaturalLanguage(String),
}

const MODEL: &str = "gpt-4o";

/// エージェントループの最大ラウンド数（無限ループ防止）
const MAX_AGENT_ROUNDS: usize = 10;

const SYSTEM_PROMPT: &str = r#"You are J.A.R.V.I.S., an AI assistant integrated into the terminal shell "jarvish".
You serve as the user's intelligent shell companion, like Tony Stark's AI butler.

Your role:
1. If the user's input is clearly a shell command (like `ls`, `git status`, `grep`, `cat`, `echo`, `mkdir`, `rm`, `cd`, `pwd`, `docker`, `cargo`, `npm`, `python`, etc.), call the `execute_shell_command` tool with the exact command. Do NOT add explanation — just call the tool.
2. If the user's input is natural language (a question, a request for help, a greeting, etc.), respond helpfully and concisely as Jarvis. Maintain the persona of an intelligent, loyal AI assistant.
3. When the user asks about errors or previous commands, use the provided command history context to give accurate, specific advice.
4. If the user asks in a specific language, respond in that same language.

### File Operations

You have `read_file` and `write_file` tools. Use them when the user asks you to read, create, edit, or modify files.

**Best practices for file editing:**
- ALWAYS call `read_file` first to understand the current file contents and structure before making changes.
- When editing, preserve the existing formatting and conventions of the file.
- When writing, include the COMPLETE file contents (not just the changed parts).

**Markdown awareness:**
- Recognize and preserve Markdown structures: headings (`#`, `##`), lists (`-`, `*`, `1.`), checkboxes (`- [ ]`, `- [x]`), code blocks, etc.
- When adding items to a list, follow the existing numbering/formatting conventions.
- For TODO lists with `- [ ] [#N]` patterns, assign the next sequential number.

**File paths:**
- All file paths are relative to the user's current working directory (CWD).
- The CWD is shown in the command history context.

Important guidelines:
- Be concise. Terminal output should be short and actionable.
- When suggesting fixes, provide the exact command the user should run.
- Maintain the "Iron Man J.A.R.V.I.S." persona: professional, helpful, with subtle dry wit.
- Address the user as "sir" occasionally."#;

/// J.A.R.V.I.S. AI クライアント
pub struct JarvisAI {
    client: Client<OpenAIConfig>,
}

/// ストリーム処理の結果
struct StreamResult {
    /// ストリーミングで受信したテキスト全文
    full_text: String,
    /// 蓄積された Tool Call
    tool_calls: Vec<ToolCallAccumulator>,
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
    /// エージェントループにより、複数ステップのツール呼び出し（read_file → write_file 等）を処理する。
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

        let tools = Self::build_tools();

        // 会話履歴を構築（エージェントループで蓄積される）
        let mut messages: Vec<ChatCompletionRequestMessage> = vec![
            ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(system_content),
                name: None,
            }),
            ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(input.to_string()),
                name: None,
            }),
        ];

        // --- エージェントループ ---
        for round in 0..MAX_AGENT_ROUNDS {
            debug!(round = round, messages_count = messages.len(), "Agent loop round");

            let request = CreateChatCompletionRequest {
                model: MODEL.to_string(),
                messages: messages.clone(),
                tools: Some(tools.clone()),
                stream: Some(true),
                ..Default::default()
            };

            debug!(
                model = MODEL,
                message_count = messages.len(),
                tools_count = tools.len(),
                stream = true,
                round = round,
                "Sending API request to OpenAI"
            );

            // ストリーム処理
            let stream_result = self.process_stream(request, round == 0).await?;

            // Tool Call がなければ最終応答として返す
            if stream_result.tool_calls.is_empty() {
                if stream_result.full_text.is_empty() {
                    warn!(
                        user_input = %input,
                        round = round,
                        "AI returned empty response (no text, no tool calls)"
                    );
                } else {
                    info!(
                        response_type = "NaturalLanguage",
                        response_length = stream_result.full_text.len(),
                        round = round,
                        "AI response: natural language"
                    );
                }
                return Ok(AiResponse::NaturalLanguage(stream_result.full_text));
            }

            // Tool Call を処理
            // execute_shell_command があればコマンドとして即座に返す
            if let Some(cmd) = Self::extract_shell_command(&stream_result.tool_calls) {
                info!(
                    response_type = "Command",
                    command = %cmd,
                    round = round,
                    "AI response: execute command"
                );
                return Ok(AiResponse::Command(cmd));
            }

            // ファイル操作ツールを処理し、会話履歴に追加してループ続行
            let assistant_tool_calls = Self::build_assistant_tool_calls(&stream_result.tool_calls);

            // アシスタントメッセージ（tool_calls 付き）を会話履歴に追加
            messages.push(ChatCompletionRequestMessage::Assistant(
                ChatCompletionRequestAssistantMessage {
                    content: if stream_result.full_text.is_empty() {
                        None
                    } else {
                        Some(ChatCompletionRequestAssistantMessageContent::Text(
                            stream_result.full_text,
                        ))
                    },
                    refusal: None,
                    name: None,
                    audio: None,
                    tool_calls: Some(assistant_tool_calls),
                    #[allow(deprecated)]
                    function_call: None,
                },
            ));

            // 各ツールをローカルで実行し、結果を会話履歴に追加
            for tc in &stream_result.tool_calls {
                let result = self.execute_tool(&tc.function_name, &tc.arguments).await;

                debug!(
                    tool = %tc.function_name,
                    tool_call_id = %tc.id,
                    result_length = result.len(),
                    round = round,
                    "Tool executed locally"
                );

                messages.push(ChatCompletionRequestMessage::Tool(
                    ChatCompletionRequestToolMessage {
                        content: ChatCompletionRequestToolMessageContent::Text(result),
                        tool_call_id: tc.id.clone(),
                    },
                ));
            }
        }

        // ラウンド上限に達した場合
        warn!("Agent loop reached maximum rounds ({MAX_AGENT_ROUNDS})");
        Ok(AiResponse::NaturalLanguage(
            "I apologize, sir. I've reached the maximum number of processing steps.".to_string(),
        ))
    }

    /// ストリーミングレスポンスを処理し、テキストと Tool Call を分離して返す。
    ///
    /// `show_spinner`: true の場合、初回ラウンドでスピナーを表示する。
    /// 後続ラウンドではツール実行中のメッセージを表示する。
    async fn process_stream(
        &self,
        request: CreateChatCompletionRequest,
        is_first_round: bool,
    ) -> Result<StreamResult> {
        // ローディングスピナーを開始
        let spinner = jarvis_spinner();

        let mut stream = match self.client.chat().create_stream(request).await {
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
            is_first_round = is_first_round,
            "Stream processing completed"
        );

        Ok(StreamResult {
            full_text,
            tool_calls,
        })
    }

    // ========== ツール定義 ==========

    /// すべてのツール定義を構築する
    fn build_tools() -> Vec<ChatCompletionTool> {
        vec![
            Self::shell_command_tool(),
            Self::read_file_tool(),
            Self::write_file_tool(),
        ]
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

    /// read_file ツールの定義
    fn read_file_tool() -> ChatCompletionTool {
        ChatCompletionTool {
            r#type: ChatCompletionToolType::Function,
            function: FunctionObject {
                name: "read_file".to_string(),
                description: Some(
                    "Read the contents of a file. Use this to inspect a file before editing it. The path is relative to the user's current working directory."
                        .to_string(),
                ),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The file path to read (relative to CWD)"
                        }
                    },
                    "required": ["path"]
                })),
                strict: None,
            },
        }
    }

    /// write_file ツールの定義
    fn write_file_tool() -> ChatCompletionTool {
        ChatCompletionTool {
            r#type: ChatCompletionToolType::Function,
            function: FunctionObject {
                name: "write_file".to_string(),
                description: Some(
                    "Write content to a file, creating it if it doesn't exist or overwriting if it does. Always read_file first before writing to preserve existing content. The path is relative to the user's current working directory."
                        .to_string(),
                ),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The file path to write to (relative to CWD)"
                        },
                        "content": {
                            "type": "string",
                            "description": "The complete file content to write"
                        }
                    },
                    "required": ["path", "content"]
                })),
                strict: None,
            },
        }
    }

    // ========== ツール実行 ==========

    /// ツール名と引数に基づいてローカルでツールを実行する。
    /// execute_shell_command はこのメソッドでは処理しない（呼び出し前にフィルタ済み）。
    async fn execute_tool(&self, function_name: &str, arguments: &str) -> String {
        debug!(
            function_name = %function_name,
            arguments = %arguments,
            "Executing tool locally"
        );

        match function_name {
            "read_file" => self.execute_read_file(arguments),
            "write_file" => self.execute_write_file(arguments),
            other => {
                warn!(tool = %other, "Unknown tool called");
                format!("Error: Unknown tool '{other}'")
            }
        }
    }

    /// read_file ツールのローカル実行
    fn execute_read_file(&self, arguments: &str) -> String {
        let parsed: serde_json::Value = match serde_json::from_str(arguments) {
            Ok(v) => v,
            Err(e) => return format!("Error parsing arguments: {e}"),
        };

        let path = match parsed.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: 'path' parameter is required".to_string(),
        };

        jarvis_talk(&format!("Reading file: {path}"));

        match std::fs::read_to_string(path) {
            Ok(content) => {
                info!(path = %path, content_length = content.len(), "File read successfully");
                content
            }
            Err(e) => {
                warn!(path = %path, error = %e, "Failed to read file");
                format!("Error reading file '{path}': {e}")
            }
        }
    }

    /// write_file ツールのローカル実行
    fn execute_write_file(&self, arguments: &str) -> String {
        let parsed: serde_json::Value = match serde_json::from_str(arguments) {
            Ok(v) => v,
            Err(e) => return format!("Error parsing arguments: {e}"),
        };

        let path = match parsed.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return "Error: 'path' parameter is required".to_string(),
        };

        let content = match parsed.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return "Error: 'content' parameter is required".to_string(),
        };

        jarvis_talk(&format!("Writing file: {path}"));

        // 親ディレクトリが存在しない場合は作成
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    warn!(path = %path, error = %e, "Failed to create parent directory");
                    return format!("Error creating directory for '{path}': {e}");
                }
            }
        }

        match std::fs::write(path, content) {
            Ok(()) => {
                info!(path = %path, content_length = content.len(), "File written successfully");
                format!("Successfully wrote {} bytes to '{path}'", content.len())
            }
            Err(e) => {
                warn!(path = %path, error = %e, "Failed to write file");
                format!("Error writing file '{path}': {e}")
            }
        }
    }

    // ========== Tool Call ヘルパー ==========

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

    /// 蓄積した Tool Call から execute_shell_command のコマンド文字列を抽出する。
    /// read_file / write_file はここでは抽出しない。
    fn extract_shell_command(tool_calls: &[ToolCallAccumulator]) -> Option<String> {
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

    /// ToolCallAccumulator から ChatCompletionMessageToolCall を構築する（会話履歴に追加用）
    fn build_assistant_tool_calls(
        accumulators: &[ToolCallAccumulator],
    ) -> Vec<ChatCompletionMessageToolCall> {
        accumulators
            .iter()
            .map(|tc| ChatCompletionMessageToolCall {
                id: tc.id.clone(),
                r#type: ChatCompletionToolType::Function,
                function: FunctionCall {
                    name: tc.function_name.clone(),
                    arguments: tc.arguments.clone(),
                },
            })
            .collect()
    }
}

/// Tool Call のストリーミングチャンクを蓄積するための構造体
#[derive(Debug, Default, Clone)]
struct ToolCallAccumulator {
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
    fn extract_shell_command_from_tool_calls() {
        let tool_calls = vec![ToolCallAccumulator {
            id: "call_123".to_string(),
            function_name: "execute_shell_command".to_string(),
            arguments: r#"{"command": "ls -la"}"#.to_string(),
        }];

        let cmd = JarvisAI::extract_shell_command(&tool_calls);
        assert_eq!(cmd, Some("ls -la".to_string()));
    }

    #[test]
    fn extract_shell_command_returns_none_for_empty() {
        let tool_calls: Vec<ToolCallAccumulator> = Vec::new();
        let cmd = JarvisAI::extract_shell_command(&tool_calls);
        assert!(cmd.is_none());
    }

    #[test]
    fn extract_shell_command_handles_invalid_json() {
        let tool_calls = vec![ToolCallAccumulator {
            id: "call_456".to_string(),
            function_name: "execute_shell_command".to_string(),
            arguments: "invalid json".to_string(),
        }];

        let cmd = JarvisAI::extract_shell_command(&tool_calls);
        assert!(cmd.is_none());
    }

    #[test]
    fn extract_shell_command_ignores_file_tools() {
        let tool_calls = vec![
            ToolCallAccumulator {
                id: "call_1".to_string(),
                function_name: "read_file".to_string(),
                arguments: r#"{"path": "test.txt"}"#.to_string(),
            },
            ToolCallAccumulator {
                id: "call_2".to_string(),
                function_name: "write_file".to_string(),
                arguments: r#"{"path": "test.txt", "content": "hello"}"#.to_string(),
            },
        ];

        let cmd = JarvisAI::extract_shell_command(&tool_calls);
        assert!(cmd.is_none());
    }

    #[test]
    fn build_assistant_tool_calls_works() {
        let accumulators = vec![ToolCallAccumulator {
            id: "call_123".to_string(),
            function_name: "read_file".to_string(),
            arguments: r#"{"path": "test.txt"}"#.to_string(),
        }];

        let result = JarvisAI::build_assistant_tool_calls(&accumulators);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "call_123");
        assert_eq!(result[0].function.name, "read_file");
        assert_eq!(result[0].function.arguments, r#"{"path": "test.txt"}"#);
    }
}
