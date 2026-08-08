use anyhow::Result;
use tracing::debug;

use crate::ai::prompts::ERROR_INVESTIGATION_PROMPT;
use crate::ai::provider::types::ChatMessage;
use crate::ai::types::{ConversationOrigin, ConversationResult, ConversationState};
use crate::engine::CommandResult;

use super::JarvisAI;

impl JarvisAI {
    /// コマンド異常終了時にエラーを調査する。
    pub async fn investigate_error(
        &self,
        command: &str,
        result: &CommandResult,
        context: &str,
    ) -> Result<ConversationResult> {
        debug!(
            command = %command,
            exit_code = result.exit_code,
            stdout_len = result.stdout.len(),
            stderr_len = result.stderr.len(),
            "investigate_error() called"
        );

        let mut error_details = format!(
            "The following command failed:\n\
             Command: {command}\n\
             Exit code: {}\n",
            result.exit_code
        );
        if !result.stdout.is_empty() {
            error_details.push_str(&format!("\nstdout:\n{}\n", result.stdout));
        }
        if !result.stderr.is_empty() {
            error_details.push_str(&format!("\nstderr:\n{}\n", result.stderr));
        }
        error_details.push_str("\nPlease investigate the error and suggest a fix.");

        let system_content = if context.is_empty() {
            ERROR_INVESTIGATION_PROMPT.to_string()
        } else {
            format!("{ERROR_INVESTIGATION_PROMPT}\n\n{context}")
        };

        let mut messages = vec![
            ChatMessage::System(system_content),
            ChatMessage::User(error_details),
        ];

        let response = self.run_agent_loop(&mut messages).await?;
        Ok(ConversationResult {
            response,
            conversation: ConversationState {
                messages,
                origin: ConversationOrigin::Investigation,
            },
        })
    }
}
