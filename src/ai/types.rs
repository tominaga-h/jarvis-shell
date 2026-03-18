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

/// 会話の発生元。エラー調査由来の会話を自然言語入力に流用しないための区別に使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationOrigin {
    /// 自然言語入力から開始された会話
    NaturalLanguage,
    /// エラー調査から開始された会話
    Investigation,
}

/// 会話の状態を保持する構造体。会話コンテキストの継続に使用。
pub struct ConversationState {
    pub(crate) messages: Vec<ChatCompletionRequestMessage>,
    /// この会話がどこで開始されたか
    pub origin: ConversationOrigin,
}

/// AI との会話結果。応答と会話コンテキスト（継続用）を含む。
pub struct ConversationResult {
    /// AI の応答
    pub response: AiResponse,
    /// 会話の状態（会話コンテキストの継続に使用）
    pub conversation: ConversationState,
}
