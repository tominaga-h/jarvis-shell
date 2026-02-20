use clap::Parser;

use crate::engine::CommandResult;

/// ビルトインコマンドの名前と説明の一覧（アルファベット順）。
const BUILTIN_COMMANDS: &[(&str, &str)] = &[
    ("alias", "エイリアスを設定・表示する"),
    ("cd", "カレントディレクトリを変更する"),
    ("cwd", "現在のカレントディレクトリを表示する"),
    ("exit", "シェルを終了する"),
    ("export", "環境変数を設定・表示する"),
    ("help", "ビルトインコマンドのヘルプを表示する"),
    ("history", "コマンド履歴を表示・管理する"),
    ("unalias", "エイリアスを削除する"),
    ("unset", "環境変数を削除する"),
];

/// help: ビルトインコマンドのヘルプを表示する。
#[derive(Parser)]
#[command(name = "help", about = "ビルトインコマンドのヘルプを表示する")]
struct HelpArgs {
    /// ヘルプを表示するコマンド名
    command: Option<String>,
}

/// help: ビルトインコマンドのヘルプを表示する。
/// - 引数なし → 全ビルトインコマンドの一覧を表示
/// - `help <command>` → 指定コマンドの詳細ヘルプを表示
pub(super) fn execute(args: &[&str]) -> CommandResult {
    let parsed = match super::parse_args::<HelpArgs>("help", args) {
        Ok(a) => a,
        Err(result) => return result,
    };

    match parsed.command {
        None => list_builtins(),
        Some(cmd) => show_command_help(&cmd),
    }
}

/// 全ビルトインコマンドの一覧を表示する。
fn list_builtins() -> CommandResult {
    let mut output = String::from("Jarvis Shell builtins:\n");

    for (name, desc) in BUILTIN_COMMANDS {
        output.push_str(&format!("  {name:<10}{desc}\n"));
    }

    print!("{output}");
    CommandResult::success(output)
}

/// 指定コマンドの詳細ヘルプを表示する。
/// ビルトインコマンドの場合は `dispatch_builtin(cmd, &["--help"])` に委譲する。
fn show_command_help(cmd: &str) -> CommandResult {
    if !super::is_builtin(cmd) {
        let msg = format!("jarvish: help: no such builtin: {cmd}\n");
        eprint!("{msg}");
        return CommandResult::error(msg, 1);
    }

    // 対象コマンドの --help を呼び出して詳細ヘルプを表示
    super::dispatch_builtin(cmd, &["--help"]).unwrap_or_else(|| {
        CommandResult::error(format!("jarvish: help: {cmd}: unexpected error\n"), 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::LoopAction;

    #[test]
    fn help_no_args_lists_all_builtins() {
        let result = execute(&[]);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.action, LoopAction::Continue);
        assert!(result.stdout.contains("Jarvis Shell builtins:"));
        assert!(result.stdout.contains("cd"));
        assert!(result.stdout.contains("cwd"));
        assert!(result.stdout.contains("exit"));
        assert!(result.stdout.contains("export"));
        assert!(result.stdout.contains("help"));
        assert!(result.stdout.contains("history"));
        assert!(result.stdout.contains("unset"));
    }

    #[test]
    fn help_specific_command_shows_detail() {
        let result = execute(&["cd"]);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.action, LoopAction::Continue);
        // cd の --help 出力にはコマンド名が含まれるはず
        assert!(result.stdout.contains("cd"));
    }

    #[test]
    fn help_unknown_command_returns_error() {
        let result = execute(&["nonexistent"]);
        assert_ne!(result.exit_code, 0);
        assert!(result.stderr.contains("no such builtin"));
    }

    #[test]
    fn help_help_returns_success() {
        let result = execute(&["--help"]);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("help"));
    }

    #[test]
    fn help_for_each_builtin_succeeds() {
        for (name, _) in BUILTIN_COMMANDS {
            let result = execute(&[name]);
            assert_eq!(result.exit_code, 0, "help {name} should succeed");
        }
    }
}
