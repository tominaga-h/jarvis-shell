//! AI フィルターの抽出

use super::Pipeline;

impl Pipeline {
    /// パイプラインの最後のコマンドが `ai` であれば、その引数（プロンプト）と、
    /// AI コマンドを除いた新しい Pipeline を返す。
    ///
    /// 以下の場合は `None` を返す:
    /// - 末尾のコマンドが `ai` でない
    /// - `ai` に引数（プロンプト）が指定されていない
    /// - `ai` の手前にコマンドがない（`ai` 単独）
    pub fn extract_ai_filter(&self) -> Option<(String, Pipeline)> {
        let last = self.commands.last()?;
        if last.cmd != "ai" {
            return None;
        }
        let prompt = last.args.join(" ");
        if prompt.is_empty() {
            return None;
        }
        let remaining = Pipeline {
            commands: self.commands[..self.commands.len() - 1].to_vec(),
        };
        if remaining.commands.is_empty() {
            return None;
        }
        Some((prompt, remaining))
    }
}
