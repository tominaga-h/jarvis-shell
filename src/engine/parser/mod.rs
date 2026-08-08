//! シェル構文パーサー
//!
//! `shell_words::split()` で得たトークン列を、パイプライン（`|`）と
//! リダイレクト（`>`, `>>`, `<`）を含む構造化された `Pipeline` に変換する。

mod ai_filter;
mod command_list;
mod pipeline;
mod redirects;
mod simple_command;
mod types;

pub use command_list::parse_command_list;
pub use pipeline::parse_pipeline;
pub use types::*;

#[cfg(test)]
mod tests;
