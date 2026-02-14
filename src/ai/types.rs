//! AI モジュールの公開型定義

use async_openai::types::ChatCompletionRequestMessage;

/// AI の判定結果
#[derive(Debug, Clone)]
pub enum AiResponse {
    /// ユーザー入力はシェルコマンドである。AI が返したコマンド文字列を含む。
    Command(String),
    /// ユーザー入力は自然言語である。AI の回答テキストを含む（ストリーミング済み）。
    NaturalLanguage(String),
}

/// 会話の状態を保持する構造体。会話コンテキストの継続に使用。
pub struct ConversationState {
    pub(crate) messages: Vec<ChatCompletionRequestMessage>,
}

/// AI との会話結果。応答と会話コンテキスト（継続用）を含む。
pub struct ConversationResult {
    /// AI の応答
    pub response: AiResponse,
    /// 会話の状態（会話コンテキストの継続に使用）
    pub conversation: ConversationState,
}
