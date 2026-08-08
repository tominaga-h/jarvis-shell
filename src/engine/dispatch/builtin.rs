//! ビルトインコマンドのディスパッチ。

use tracing::debug;

use crate::engine::{builtins, expand, CommandResult};

/// ビルトインコマンドのみを試行する。
/// ビルトインでなければ None を返す（AI ルーティング前のチェック用）。
///
/// 先頭ワードがビルトインキーワード（cd, cwd, exit）でない場合は
/// パースを行わず即座に None を返す。これにより、自然言語中の
/// アポストロフィ等によるパースエラーが AI ルーティングをブロックしない。
pub fn try_builtin(input: &str) -> Option<CommandResult> {
    let input = input.trim();
    if input.is_empty() {
        return Some(CommandResult::success(String::new()));
    }

    let first_word = input.split_whitespace().next().unwrap_or("");
    if !builtins::is_builtin(first_word) {
        debug!(
            command = %first_word,
            is_builtin = false,
            "try_builtin check"
        );
        return None;
    }

    let tokens = match expand::split_quoted(input) {
        Ok(tokens) => tokens,
        Err(e) => {
            let msg = format!("jarvish: parse error: {e}\n");
            eprint!("{msg}");
            return Some(CommandResult::error(msg, 1));
        }
    };

    if tokens.is_empty() {
        return Some(CommandResult::success(String::new()));
    }

    if tokens
        .iter()
        .any(|t| matches!(t.value.as_str(), "|" | ">" | ">>" | "<" | "&&" | "||" | ";"))
    {
        debug!(
            command = %first_word,
            "try_builtin: contains pipe/redirect/connector, deferring to execute()"
        );
        return None;
    }

    let expanded = match super::expansion::expand_tokens(tokens, false) {
        Ok(expanded) => expanded,
        Err(expand::ExpandError::NoMatches(p)) => {
            let msg = format!("jarvish: no matches found: {p}\n");
            eprint!("{msg}");
            return Some(CommandResult::error(msg, 1));
        }
        Err(expand::ExpandError::Substitution(m)) => {
            let msg = format!("jarvish: {m}\n");
            eprint!("{msg}");
            return Some(CommandResult::error(msg, 1));
        }
    };
    if expanded.is_empty() {
        return Some(CommandResult::success(String::new()));
    }
    let cmd = &expanded[0];
    let args: Vec<&str> = expanded[1..].iter().map(|s| s.as_str()).collect();

    let result = builtins::dispatch_builtin(cmd, &args);
    debug!(
        command = %cmd,
        is_builtin = result.is_some(),
        "try_builtin check"
    );
    result
}
