use std::collections::HashMap;

use clap::Parser;

use crate::engine::CommandResult;

/// alias: エイリアスを設定・表示する。
#[derive(Parser)]
#[command(name = "alias", about = "Set or display aliases")]
struct AliasArgs {
    /// Alias definition in NAME=VALUE format, or alias name to display
    assignments: Vec<String>,
}

/// alias: エイリアスを設定・表示する。
/// - 引数なし → 全エイリアスをソート済みで表示
/// - `alias A=B` → エイリアスを設定
/// - `alias A` → 該当エイリアスの値を表示
///
/// Shell 側から `&mut aliases` を渡して呼び出す。
pub(crate) fn execute_with_aliases(
    args: &[&str],
    aliases: &mut HashMap<String, String>,
) -> CommandResult {
    let parsed = match super::parse_args::<AliasArgs>("alias", args) {
        Ok(a) => a,
        Err(result) => return result,
    };

    if parsed.assignments.is_empty() {
        return list_all_aliases(aliases);
    }

    let mut output = String::new();

    for assignment in &parsed.assignments {
        if let Some(eq_pos) = assignment.find('=') {
            let name = &assignment[..eq_pos];
            let value = &assignment[eq_pos + 1..];

            if name.is_empty() {
                let msg = format!("jarvish: alias: `{assignment}`: not a valid alias name\n");
                eprint!("{msg}");
                return CommandResult::error(msg, 1);
            }

            aliases.insert(name.to_string(), value.to_string());
        } else {
            match aliases.get(assignment.as_str()) {
                Some(value) => {
                    let line = format!("alias {assignment}='{value}'\n");
                    print!("{line}");
                    output.push_str(&line);
                }
                None => {
                    let msg = format!("jarvish: alias: {assignment}: not found\n");
                    eprint!("{msg}");
                    return CommandResult::error(msg, 1);
                }
            }
        }
    }

    CommandResult::success(output)
}

/// 全エイリアスをソート済みで `alias NAME='VALUE'` 形式で表示する。
fn list_all_aliases(aliases: &HashMap<String, String>) -> CommandResult {
    let mut entries: Vec<(&String, &String)> = aliases.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));

    let mut output = String::new();
    for (name, value) in &entries {
        let line = format!("alias {name}='{value}'\n");
        output.push_str(&line);
    }
    print!("{output}");

    CommandResult::success(output)
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
    fn alias_no_args_lists_all() {
        let mut aliases = make_aliases();
        let result = execute_with_aliases(&[], &mut aliases);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("alias g='git'"));
        assert!(result.stdout.contains("alias ll='ls -la'"));
    }

    #[test]
    fn alias_no_args_sorted() {
        let mut aliases = make_aliases();
        let result = execute_with_aliases(&[], &mut aliases);
        assert_eq!(result.exit_code, 0);
        let g_pos = result.stdout.find("g=").unwrap();
        let ll_pos = result.stdout.find("ll=").unwrap();
        assert!(g_pos < ll_pos, "一覧はアルファベット順であるべき");
    }

    #[test]
    fn alias_set_new() {
        let mut aliases = HashMap::new();
        let result = execute_with_aliases(&["gs=git status"], &mut aliases);
        assert_eq!(result.exit_code, 0);
        assert_eq!(aliases.get("gs").unwrap(), "git status");
    }

    #[test]
    fn alias_set_overwrite() {
        let mut aliases = make_aliases();
        let result = execute_with_aliases(&["g=git --no-pager"], &mut aliases);
        assert_eq!(result.exit_code, 0);
        assert_eq!(aliases.get("g").unwrap(), "git --no-pager");
    }

    #[test]
    fn alias_show_existing() {
        let mut aliases = make_aliases();
        let result = execute_with_aliases(&["g"], &mut aliases);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("alias g='git'"));
    }

    #[test]
    fn alias_show_not_found() {
        let mut aliases = make_aliases();
        let result = execute_with_aliases(&["nonexistent"], &mut aliases);
        assert_ne!(result.exit_code, 0);
        assert!(result.stderr.contains("not found"));
    }

    #[test]
    fn alias_empty_name_is_error() {
        let mut aliases = HashMap::new();
        let result = execute_with_aliases(&["=value"], &mut aliases);
        assert_ne!(result.exit_code, 0);
        assert!(result.stderr.contains("not a valid alias name"));
    }

    #[test]
    fn alias_empty_value_is_ok() {
        let mut aliases = HashMap::new();
        let result = execute_with_aliases(&["empty="], &mut aliases);
        assert_eq!(result.exit_code, 0);
        assert_eq!(aliases.get("empty").unwrap(), "");
    }

    #[test]
    fn alias_help_returns_success() {
        let mut aliases = HashMap::new();
        let result = execute_with_aliases(&["--help"], &mut aliases);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("alias"));
    }
}
