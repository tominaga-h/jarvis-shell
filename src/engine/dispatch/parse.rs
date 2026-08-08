//! 入力のパースと実行経路の選択。

use tracing::debug;

use crate::engine::{expand, parser, CommandResult};

/// ユーザー入力をパースし、ビルトインまたは外部コマンドとして実行する。
///
/// パイプライン（`|`）やリダイレクト（`>`, `>>`, `<`）を含むコマンドに対応。
/// 単一コマンドでビルトインの場合はビルトインとして処理し、
/// それ以外は `exec::run_pipeline()` でパイプライン実行する。
pub fn execute(input: &str) -> CommandResult {
    let input = input.trim();
    if input.is_empty() {
        return CommandResult::success(String::new());
    }

    let tokens = match expand::split_quoted(input) {
        Ok(tokens) => tokens,
        Err(e) => {
            let msg = format!("jarvish: parse error: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    if tokens.is_empty() {
        return CommandResult::success(String::new());
    }

    let expanded = match super::expansion::expand_tokens(tokens, true) {
        Ok(expanded) => expanded,
        Err(expand::ExpandError::NoMatches(p)) => {
            let msg = format!("jarvish: no matches found: {p}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
        Err(expand::ExpandError::Substitution(m)) => {
            let msg = format!("jarvish: {m}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    let command_list = match parser::parse_command_list(expanded) {
        Ok(cl) => cl,
        Err(e) => {
            let msg = format!("jarvish: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    debug!(
        pipeline_count = command_list.rest.len() + 1,
        first_cmd = %command_list.first.commands[0].cmd,
        "execute() parsed command list"
    );

    if command_list.rest.is_empty() {
        let pipeline = &command_list.first;
        return super::external::execute_pipeline(pipeline);
    }

    super::external::run_command_list_with_builtins(&command_list)
}
