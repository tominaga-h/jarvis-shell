//! 単一コマンドのパース

use super::redirects::parse_redirect;
use super::{ParseError, Redirect, SimpleCommand};

/// トークンのスライスからリダイレクトを抽出し、SimpleCommand を構築する。
pub(super) fn parse_simple_command(tokens: &[String]) -> Result<SimpleCommand, ParseError> {
    let mut args: Vec<String> = Vec::new();
    let mut redirects: Vec<Redirect> = Vec::new();
    let mut iter = tokens.iter().peekable();

    while let Some(token) = iter.next() {
        let target = match token.as_str() {
            ">>" | ">" | "<" => iter.next().cloned(),
            _ => None,
        };
        if let Some(redirect) = parse_redirect(token, target)? {
            redirects.push(redirect);
        } else {
            args.push(token.clone());
        }
    }

    if args.is_empty() {
        return Err(ParseError("syntax error: missing command".to_string()));
    }

    let cmd = args.remove(0);
    Ok(SimpleCommand {
        cmd,
        args,
        redirects,
    })
}
