//! コマンドリストのパース

use super::pipeline::parse_pipeline;
use super::{CommandList, Connector, ParseError};

/// トークン列をコマンドリストにパースする。
///
/// `shell_words::split()` で分割済みのトークンを受け取り、
/// `&&`, `||`, `;` で分割した後、各セグメントを `parse_pipeline()` でパースする。
pub fn parse_command_list(tokens: Vec<String>) -> Result<CommandList, ParseError> {
    if tokens.is_empty() {
        return Err(ParseError("empty command".to_string()));
    }

    let (segments, connectors) = split_by_connector(&tokens)?;

    let first = parse_pipeline(segments[0].clone())?;
    let mut rest = Vec::new();
    for (i, conn) in connectors.into_iter().enumerate() {
        let pipeline = parse_pipeline(segments[i + 1].clone())?;
        rest.push((conn, pipeline));
    }

    Ok(CommandList { first, rest })
}

/// トークン列を `&&`, `||`, `;` で分割する。
fn split_by_connector(tokens: &[String]) -> Result<(Vec<Vec<String>>, Vec<Connector>), ParseError> {
    let mut segments: Vec<Vec<String>> = Vec::new();
    let mut connectors: Vec<Connector> = Vec::new();
    let mut current: Vec<String> = Vec::new();

    for token in tokens {
        match token.as_str() {
            "&&" => {
                if current.is_empty() {
                    return Err(ParseError(
                        "syntax error: unexpected token '&&'".to_string(),
                    ));
                }
                segments.push(std::mem::take(&mut current));
                connectors.push(Connector::And);
            }
            "||" => {
                if current.is_empty() {
                    return Err(ParseError(
                        "syntax error: unexpected token '||'".to_string(),
                    ));
                }
                segments.push(std::mem::take(&mut current));
                connectors.push(Connector::Or);
            }
            ";" => {
                if current.is_empty() {
                    return Err(ParseError("syntax error: unexpected token ';'".to_string()));
                }
                segments.push(std::mem::take(&mut current));
                connectors.push(Connector::Semi);
            }
            _ => {
                current.push(token.clone());
            }
        }
    }

    if current.is_empty() && !connectors.is_empty() {
        return Err(ParseError(
            "syntax error: unexpected end of command after connector".to_string(),
        ));
    }
    if !current.is_empty() {
        segments.push(current);
    }

    Ok((segments, connectors))
}
