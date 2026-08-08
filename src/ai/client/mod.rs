//! OpenAI API クライアント — J.A.R.V.I.S. Brain
//!
//! ユーザー入力を AI に送信し、コマンドか自然言語かを判定する。
//! ストリーミングレスポンスに対応し、Tool Calling でコマンド実行を支援する。
//! エージェントループにより、複数ステップのファイル操作（読み取り→編集→書き込み）が可能。

mod agent;
mod config_update;
mod conversation;
mod core;
mod input_processing;
mod investigation;
mod pipe;

#[cfg(test)]
mod tests;

pub use core::JarvisAI;
