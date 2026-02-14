use crate::engine::CommandResult;

/// exit: REPL ループを終了する。
/// - 引数なし → 終了コード 0
/// - `exit N` → 終了コード N（0〜255。範囲外は 255 にクランプ）
/// - `exit foo` → エラー（数値でない引数）
pub(super) fn execute(args: &[&str]) -> CommandResult {
    match args.first() {
        None => CommandResult::exit_with(0),
        Some(code_str) => match code_str.parse::<i32>() {
            Ok(code) => {
                // bash と同様に 0〜255 の範囲にクランプ
                let code = code.clamp(0, 255);
                CommandResult::exit_with(code)
            }
            Err(_) => {
                let msg = format!("jarvish: exit: {code_str}: numeric argument required\n");
                eprint!("{msg}");
                // bash と同様に不正な引数では終了コード 2 で終了する
                CommandResult::exit_with(2)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::LoopAction;

    #[test]
    fn exit_returns_exit_action() {
        let result = execute(&[]);
        assert_eq!(result.action, LoopAction::Exit);
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn exit_with_code_returns_specified_code() {
        let result = execute(&["1"]);
        assert_eq!(result.action, LoopAction::Exit);
        assert_eq!(result.exit_code, 1);

        let result = execute(&["127"]);
        assert_eq!(result.action, LoopAction::Exit);
        assert_eq!(result.exit_code, 127);

        let result = execute(&["0"]);
        assert_eq!(result.action, LoopAction::Exit);
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn exit_clamps_out_of_range_code() {
        let result = execute(&["999"]);
        assert_eq!(result.action, LoopAction::Exit);
        assert_eq!(result.exit_code, 255);

        let result = execute(&["-1"]);
        assert_eq!(result.action, LoopAction::Exit);
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn exit_non_numeric_returns_error() {
        let result = execute(&["foo"]);
        assert_eq!(result.action, LoopAction::Exit);
        assert_eq!(result.exit_code, 2);
    }
}
