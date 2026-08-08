use anyhow::{Context, Result};

use crate::ai::provider::openai_compat::OpenAiCompatBackend;
use crate::ai::provider::types::ChatMessage;
use crate::ai::provider::AiBackend;
use crate::config::AiConfig;

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
}

/// テキストのみのアシスタントメッセージを構築する。
pub(crate) fn build_text_assistant_message(text: String) -> ChatMessage {
    ChatMessage::Assistant {
        text: Some(text),
        tool_calls: Vec::new(),
    }
}

impl JarvisAI {
    /// OPENAI_API_KEY 環境変数から AI クライアントを初期化する。
    pub fn new(ai_config: &AiConfig) -> Result<Self> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .context("OPENAI_API_KEY is not set. AI features are disabled.")?;

        if api_key.is_empty() || api_key == "your_openai_api_key" {
            anyhow::bail!("OPENAI_API_KEY is not configured. Please set a valid API key in .env");
        }

        let backend = AiBackend::OpenAiCompat(OpenAiCompatBackend::new(&api_key));
        Ok(Self {
            backend,
            model: ai_config.model.clone(),
            max_rounds: ai_config.max_rounds,
            markdown_rendering: ai_config.markdown_rendering,
            ai_pipe_max_chars: ai_config.ai_pipe_max_chars,
            ai_redirect_max_chars: ai_config.ai_redirect_max_chars,
            temperature: ai_config.temperature,
        })
    }
}
