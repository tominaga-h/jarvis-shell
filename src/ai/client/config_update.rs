use tracing::info;

use crate::config::AiConfig;

use super::JarvisAI;

impl JarvisAI {
    /// AI 設定（モデル名・最大ラウンド数）を更新する。
    pub fn update_config(&mut self, ai_config: &AiConfig) {
        self.model = ai_config.model.clone();
        self.max_rounds = ai_config.max_rounds;
        self.markdown_rendering = ai_config.markdown_rendering;
        self.ai_pipe_max_chars = ai_config.ai_pipe_max_chars;
        self.ai_redirect_max_chars = ai_config.ai_redirect_max_chars;
        self.temperature = ai_config.temperature;
        self.max_tokens = ai_config
            .max_tokens
            .unwrap_or_else(|| crate::config::default_max_tokens_for(&ai_config.provider));
        info!(
            model = %self.model,
            max_rounds = self.max_rounds,
            markdown_rendering = self.markdown_rendering,
            ai_pipe_max_chars = self.ai_pipe_max_chars,
            ai_redirect_max_chars = self.ai_redirect_max_chars,
            temperature = self.temperature,
            max_tokens = self.max_tokens,
            "AI config updated"
        );
    }
}
