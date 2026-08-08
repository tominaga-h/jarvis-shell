//! リダイレクト演算子のパース

use super::{ParseError, Redirect};

pub(super) fn parse_redirect(
    token: &str,
    target: Option<String>,
) -> Result<Option<Redirect>, ParseError> {
    match token {
        ">>" => {
            let target = target.ok_or_else(|| {
                ParseError("syntax error: expected filename after '>>'".to_string())
            })?;
            Ok(Some(Redirect::StdoutAppend(target)))
        }
        ">" => {
            let target = target.ok_or_else(|| {
                ParseError("syntax error: expected filename after '>'".to_string())
            })?;
            Ok(Some(Redirect::StdoutOverwrite(target)))
        }
        "<" => {
            let target = target.ok_or_else(|| {
                ParseError("syntax error: expected filename after '<'".to_string())
            })?;
            Ok(Some(Redirect::StdinFrom(target)))
        }
        _ => Ok(None),
    }
}
