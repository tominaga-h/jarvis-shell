//! リダイレクトヘルパー
//!
//! `>`, `>>`, `<` リダイレクトの処理を提供する。

use std::fs::{File, OpenOptions};

use super::parser::Redirect;
use super::CommandResult;

/// リダイレクトリストから stdout リダイレクト先ファイルを開く。
pub(super) fn find_stdout_redirect(redirects: &[Redirect]) -> Option<File> {
    let mut result = None;
    for r in redirects {
        match r {
            Redirect::StdoutOverwrite(path) => {
                result = File::create(path).ok();
            }
            Redirect::StdoutAppend(path) => {
                result = OpenOptions::new().create(true).append(true).open(path).ok();
            }
            _ => {}
        }
    }
    result
}

/// リダイレクトリストから stdin リダイレクト元ファイルを開く。
pub(super) fn find_stdin_redirect(redirects: &[Redirect]) -> Result<Option<File>, CommandResult> {
    for r in redirects {
        if let Redirect::StdinFrom(path) = r {
            return match File::open(path) {
                Ok(f) => Ok(Some(f)),
                Err(e) => {
                    let msg = format!("jarvish: {path}: {e}\n");
                    eprint!("{msg}");
                    Err(CommandResult::error(msg, 1))
                }
            };
        }
    }
    Ok(None)
}
