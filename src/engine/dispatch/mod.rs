//! 入力ディスパッチ
//!
//! ユーザー入力をビルトインコマンドまたは外部コマンドとして実行する。
//! トークン分割、シェル展開、パイプラインパースを経て、
//! 適切な実行パスに振り分ける。

mod ai_pipe;
mod builtin;
mod expansion;
mod external;
mod parse;

pub use ai_pipe::{try_execute_ai_pipe, AiPipeMode, AiPipeRequest};
pub use builtin::try_builtin;
pub use parse::execute;

#[cfg(test)]
mod tests;
