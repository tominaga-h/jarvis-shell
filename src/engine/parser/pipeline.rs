//! パイプラインのパース

use super::simple_command::parse_simple_command;
use super::{ParseError, Pipeline};

/// トークン列をパイプラインにパースする。
///
/// `shell_words::split()` で分割済みのトークンを受け取り、
/// `|` でパイプライン分割し、各セグメントからリダイレクト演算子を抽出する。
pub fn parse_pipeline(tokens: Vec<String>) -> Result<Pipeline, ParseError> {
    if tokens.is_empty() {
        return Err(ParseError("empty command".to_string()));
    }

    let segments = split_by_pipe(&tokens)?;

    let mut commands = Vec::new();
    for segment in segments {
        let cmd = parse_simple_command(segment)?;
        commands.push(cmd);
    }

    Ok(Pipeline { commands })
}

/// トークン列を `|` で分割し、各セグメントを返す。
fn split_by_pipe(tokens: &[String]) -> Result<Vec<&[String]>, ParseError> {
    let mut segments: Vec<&[String]> = Vec::new();
    let mut start = 0;

    for (i, token) in tokens.iter().enumerate() {
        if token == "|" {
            if i == start {
                return Err(ParseError("syntax error: unexpected token '|'".to_string()));
            }
            segments.push(&tokens[start..i]);
            start = i + 1;
        }
    }

    if start >= tokens.len() {
        return Err(ParseError(
            "syntax error: unexpected end of command after '|'".to_string(),
        ));
    }
    segments.push(&tokens[start..]);

    Ok(segments)
}
