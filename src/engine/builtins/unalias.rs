use std::collections::HashMap;

use clap::Parser;

use crate::engine::CommandResult;

/// unalias: エイリアスを削除する。
#[derive(Parser)]
#[command(name = "unalias", about = "エイリアスを削除する")]
struct UnaliasArgs {
    /// 全エイリアスを削除する
    #[arg(short = 'a', long = "all")]
    all: bool,

    /// 削除するエイリアス名
    names: Vec<String>,
}

/// unalias: エイリアスを削除する。
/// - `unalias A` → エイリアス A を削除
/// - `unalias -a` → 全エイリアスを削除
/// - 存在しないエイリアスの削除はエラー（bash 互換）
///
/// Shell 側から `&mut aliases` を渡して呼び出す。
pub(crate) fn execute_with_aliases(
    args: &[&str],
    aliases: &mut HashMap<String, String>,
) -> CommandResult {
    let parsed = match super::parse_args::<UnaliasArgs>("unalias", args) {
        Ok(a) => a,
        Err(result) => return result,
    };

    if parsed.all {
        aliases.clear();
        return CommandResult::success(String::new());
    }

    if parsed.names.is_empty() {
        let msg = "jarvish: unalias: usage: unalias [-a] name [name ...]\n".to_string();
        eprint!("{msg}");
        return CommandResult::error(msg, 1);
    }

    for name in &parsed.names {
        if aliases.remove(name).is_none() {
            let msg = format!("jarvish: unalias: {name}: not found\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    }

    CommandResult::success(String::new())
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

    #[test]
    fn unalias_removes_existing() {
        let mut aliases = make_aliases();
        let result = execute_with_aliases(&["g"], &mut aliases);
        assert_eq!(result.exit_code, 0);
        assert!(!aliases.contains_key("g"));
        assert!(aliases.contains_key("ll"));
    }

    #[test]
    fn unalias_removes_multiple() {
        let mut aliases = make_aliases();
        let result = execute_with_aliases(&["g", "ll"], &mut aliases);
        assert_eq!(result.exit_code, 0);
        assert!(aliases.is_empty());
    }

    #[test]
    fn unalias_not_found_is_error() {
        let mut aliases = make_aliases();
        let result = execute_with_aliases(&["nonexistent"], &mut aliases);
        assert_ne!(result.exit_code, 0);
        assert!(result.stderr.contains("not found"));
    }

    #[test]
    fn unalias_all_clears_everything() {
        let mut aliases = make_aliases();
        let result = execute_with_aliases(&["-a"], &mut aliases);
        assert_eq!(result.exit_code, 0);
        assert!(aliases.is_empty());
    }

    #[test]
    fn unalias_no_args_returns_error() {
        let mut aliases = make_aliases();
        let result = execute_with_aliases(&[], &mut aliases);
        assert_ne!(result.exit_code, 0);
    }

    #[test]
    fn unalias_help_returns_success() {
        let mut aliases = HashMap::new();
        let result = execute_with_aliases(&["--help"], &mut aliases);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("unalias"));
    }
}
