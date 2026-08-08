use anyhow::{Context, Result};

use crate::ai::provider::anthropic::AnthropicBackend;
use crate::ai::provider::openai_compat::OpenAiCompatBackend;
use crate::ai::provider::types::ChatMessage;
use crate::ai::provider::AiBackend;
use crate::config::{default_max_tokens_for, AiConfig};

/// J.A.R.V.I.S. AI クライアント
pub struct JarvisAI {
    pub(crate) backend: AiBackend,
    /// 使用する AI モデル名
    pub(crate) model: String,
    /// エージェントループの最大ラウンド数
    pub(crate) max_rounds: usize,
    /// AI レスポンスを Markdown としてレンダリングするか
    pub(crate) markdown_rendering: bool,
    /// AI パイプの入力テキスト文字数上限
    pub(crate) ai_pipe_max_chars: usize,
    /// AI リダイレクトの入力テキスト文字数上限
    pub(crate) ai_redirect_max_chars: usize,
    /// 回答のランダム性（0.0 = 決定的、2.0 = 最大ランダム）
    pub(crate) temperature: f32,
    /// プロバイダへ送信する最大トークン数
    pub(crate) max_tokens: u32,
}

/// テキストのみのアシスタントメッセージを構築する。
pub(crate) fn build_text_assistant_message(text: String) -> ChatMessage {
    ChatMessage::Assistant {
        text: Some(text),
        tool_calls: Vec::new(),
    }
}

impl JarvisAI {
    /// 設定されたプロバイダの API クライアントを初期化する。
    pub fn new(ai_config: &AiConfig) -> Result<Self> {
        let provider = ai_config.provider.as_str();
        let default_key_env = match provider {
            "anthropic" => "ANTHROPIC_API_KEY",
            "opencode-zen" | "opencode-go" => "OPENCODE_API_KEY",
            _ => "OPENAI_API_KEY",
        };
        let key_env = ai_config.api_key_env.as_deref().unwrap_or(default_key_env);
        let api_key = std::env::var(key_env)
            .with_context(|| format!("{key_env} is not set. AI features are disabled."))?;

        let placeholder = match provider {
            "anthropic" => "your_anthropic_api_key",
            "opencode-zen" | "opencode-go" => "your_opencode_api_key",
            _ => "your_openai_api_key",
        };
        if api_key.is_empty() || api_key == placeholder {
            anyhow::bail!("{key_env} is not configured. Please set a valid API key in .env");
        }

        let backend = match provider {
            "anthropic" => {
                let base_url = ai_config
                    .base_url
                    .as_deref()
                    .unwrap_or("https://api.anthropic.com");
                AiBackend::Anthropic(AnthropicBackend::new(&api_key, base_url)?)
            }
            "openai" => AiBackend::OpenAiCompat(OpenAiCompatBackend::new(
                &api_key,
                ai_config.base_url.as_deref(),
                None,
            )?),
            "opencode-zen" | "opencode-go" => {
                let default_base_url = match provider {
                    "opencode-zen" => "https://opencode.ai/zen/v1",
                    "opencode-go" => "https://opencode.ai/zen/go/v1",
                    _ => unreachable!(),
                };
                let base_url = ai_config
                    .base_url
                    .as_deref()
                    .unwrap_or(default_base_url);
                let user_agent = format!("jarvish/{}", env!("CARGO_PKG_VERSION"));
                AiBackend::OpenAiCompat(OpenAiCompatBackend::new(
                    &api_key,
                    Some(base_url),
                    Some(&user_agent),
                )?)
            }
            other => anyhow::bail!(
                "Unsupported AI provider '{other}'. Supported providers: openai, anthropic, opencode-zen, opencode-go"
            ),
        };
        Ok(Self {
            backend,
            model: ai_config.model.clone(),
            max_rounds: ai_config.max_rounds,
            markdown_rendering: ai_config.markdown_rendering,
            ai_pipe_max_chars: ai_config.ai_pipe_max_chars,
            ai_redirect_max_chars: ai_config.ai_redirect_max_chars,
            temperature: ai_config.temperature,
            max_tokens: ai_config
                .max_tokens
                .unwrap_or_else(|| default_max_tokens_for(provider)),
        })
    }
}
