use std::collections::HashMap;

use clap::Parser;

use crate::engine::CommandResult;

/// `which` / `type` の出力フォーマットを切り替えるモード。
pub(super) enum WhichMode {
    Which,
    Type,
}

/// which: コマンドの場所・種類を表示する。
#[derive(Parser)]
#[command(
    name = "which",
    about = "Locate a command (builtin, alias, or external)"
)]
struct WhichArgs {
    /// Command names to look up
    #[arg(required = true)]
    commands: Vec<String>,
}

/// type: コマンドの種類を表示する。
#[derive(Parser)]
#[command(name = "type", about = "Display information about command type")]
struct TypeArgs {
    /// Command names to look up
    #[arg(required = true)]
    commands: Vec<String>,
}

/// `which` / `type` 共通のコマンド解決結果。
enum Resolution {
    Alias(String),
    Builtin,
    External(std::path::PathBuf),
    NotFound,
}

/// コマンド名を解決する。優先順位: エイリアス > ビルトイン > 外部コマンド。
fn resolve(cmd: &str, aliases: &HashMap<String, String>) -> Resolution {
    if let Some(value) = aliases.get(cmd) {
        return Resolution::Alias(value.clone());
    }
    if super::is_builtin(cmd) {
        return Resolution::Builtin;
    }
    match which::which(cmd) {
        Ok(path) => Resolution::External(path),
        Err(_) => Resolution::NotFound,
    }
}

/// 解決結果を `which` 形式でフォーマットする。
fn format_which(cmd: &str, resolution: &Resolution) -> String {
    match resolution {
        Resolution::Alias(value) => format!("{cmd}: aliased to '{value}'\n"),
        Resolution::Builtin => format!("{cmd}: jarvish built-in command\n"),
        Resolution::External(path) => format!("{}\n", path.display()),
        Resolution::NotFound => format!("{cmd} not found\n"),
    }
}

/// 解決結果を `type` 形式でフォーマットする。
fn format_type(cmd: &str, resolution: &Resolution) -> String {
    match resolution {
        Resolution::Alias(value) => format!("{cmd} is aliased to '{value}'\n"),
        Resolution::Builtin => format!("{cmd} is a jarvish built-in command\n"),
        Resolution::External(path) => format!("{cmd} is {}\n", path.display()),
        Resolution::NotFound => format!("jarvish: type: {cmd}: not found\n"),
    }
}

/// `which` / `type` の共通実行ロジック。
fn run(mode: &WhichMode, commands: &[String], aliases: &HashMap<String, String>) -> CommandResult {
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut any_not_found = false;

    for cmd in commands {
        let resolution = resolve(cmd, aliases);
        let line = match mode {
            WhichMode::Which => format_which(cmd, &resolution),
            WhichMode::Type => format_type(cmd, &resolution),
        };

        if matches!(resolution, Resolution::NotFound) {
            any_not_found = true;
            eprint!("{line}");
            stderr.push_str(&line);
        } else {
            print!("{line}");
            stdout.push_str(&line);
        }
    }

    if any_not_found {
        CommandResult {
            stdout,
            stderr,
            exit_code: 1,
            action: crate::engine::LoopAction::Continue,
            used_alt_screen: false,
        }
    } else {
        CommandResult::success(stdout)
    }
}

/// `which` ビルトインを実行する。
pub(crate) fn execute_which(args: &[&str], aliases: &HashMap<String, String>) -> CommandResult {
    let parsed = match super::parse_args::<WhichArgs>("which", args) {
        Ok(a) => a,
        Err(result) => return result,
    };
    run(&WhichMode::Which, &parsed.commands, aliases)
}

/// `type` ビルトインを実行する。
pub(crate) fn execute_type(args: &[&str], aliases: &HashMap<String, String>) -> CommandResult {
    let parsed = match super::parse_args::<TypeArgs>("type", args) {
        Ok(a) => a,
        Err(result) => return result,
    };
    run(&WhichMode::Type, &parsed.commands, aliases)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_aliases() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("g".to_string(), "git".to_string());
        m.insert("ll".to_string(), "ls -la".to_string());
        m
    }

    // ── which ──

    #[test]
    fn which_builtin_command() {
        let aliases = HashMap::new();
        let result = execute_which(&["cd"], &aliases);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "cd: jarvish built-in command\n");
    }

    #[test]
    fn which_alias() {
        let aliases = make_aliases();
        let result = execute_which(&["g"], &aliases);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "g: aliased to 'git'\n");
    }

    #[test]
    fn which_external_command() {
        let aliases = HashMap::new();
        let result = execute_which(&["ls"], &aliases);
        assert_eq!(result.exit_code, 0);
        assert!(
            result.stdout.contains("/ls\n"),
            "expected path ending with /ls, got: {}",
            result.stdout
        );
    }

    #[test]
    fn which_not_found() {
        let aliases = HashMap::new();
        let result = execute_which(&["__nonexistent_command_xyz__"], &aliases);
        assert_eq!(result.exit_code, 1);
        assert!(result.stderr.contains("not found"));
    }

    #[test]
    fn which_multiple_mixed() {
        let aliases = make_aliases();
        let result = execute_which(&["g", "cd", "__nonexistent_command_xyz__"], &aliases);
        assert_eq!(result.exit_code, 1);
        assert!(result.stdout.contains("g: aliased to 'git'"));
        assert!(result.stdout.contains("cd: jarvish built-in command"));
        assert!(result.stderr.contains("not found"));
    }

    #[test]
    fn which_alias_shadows_builtin() {
        let mut aliases = HashMap::new();
        aliases.insert("cd".to_string(), "my-cd-wrapper".to_string());
        let result = execute_which(&["cd"], &aliases);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "cd: aliased to 'my-cd-wrapper'\n");
    }

    #[test]
    fn which_no_args_is_error() {
        let aliases = HashMap::new();
        let result = execute_which(&[], &aliases);
        assert_ne!(result.exit_code, 0);
    }

    #[test]
    fn which_help_returns_success() {
        let aliases = HashMap::new();
        let result = execute_which(&["--help"], &aliases);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("which"));
    }

    // ── type ──

    #[test]
    fn type_builtin_command() {
        let aliases = HashMap::new();
        let result = execute_type(&["cd"], &aliases);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "cd is a jarvish built-in command\n");
    }

    #[test]
    fn type_alias() {
        let aliases = make_aliases();
        let result = execute_type(&["ll"], &aliases);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "ll is aliased to 'ls -la'\n");
    }

    #[test]
    fn type_external_command() {
        let aliases = HashMap::new();
        let result = execute_type(&["ls"], &aliases);
        assert_eq!(result.exit_code, 0);
        assert!(
            result.stdout.starts_with("ls is /"),
            "expected 'ls is /...', got: {}",
            result.stdout
        );
    }

    #[test]
    fn type_not_found() {
        let aliases = HashMap::new();
        let result = execute_type(&["__nonexistent_command_xyz__"], &aliases);
        assert_eq!(result.exit_code, 1);
        assert!(result.stderr.contains("not found"));
    }

    #[test]
    fn type_no_args_is_error() {
        let aliases = HashMap::new();
        let result = execute_type(&[], &aliases);
        assert_ne!(result.exit_code, 0);
    }

    #[test]
    fn type_help_returns_success() {
        let aliases = HashMap::new();
        let result = execute_type(&["--help"], &aliases);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("type"));
    }
}
