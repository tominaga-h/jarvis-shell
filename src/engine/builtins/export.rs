use std::env;

use clap::Parser;

use crate::engine::CommandResult;

/// export: 環境変数を設定・表示する。
#[derive(Parser)]
#[command(name = "export", about = "環境変数を設定・表示する")]
struct ExportArgs {
    /// KEY=VALUE 形式の変数代入、または表示する変数名
    assignments: Vec<String>,
}

/// export: 環境変数を設定・表示する。
/// - 引数なし → 全環境変数をソート済みで表示
/// - `export KEY=VALUE` → 環境変数を設定
/// - `export KEY` → 該当変数の値を表示
pub(super) fn execute(args: &[&str]) -> CommandResult {
    let parsed = match super::parse_args::<ExportArgs>("export", args) {
        Ok(a) => a,
        Err(result) => return result,
    };

    // 引数なし → 全環境変数を表示
    if parsed.assignments.is_empty() {
        return list_all_vars();
    }

    let mut output = String::new();

    for assignment in &parsed.assignments {
        if let Some(eq_pos) = assignment.find('=') {
            // KEY=VALUE 形式 → 環境変数を設定
            let key = &assignment[..eq_pos];
            let value = &assignment[eq_pos + 1..];

            if key.is_empty() {
                let msg = format!("jarvish: export: `{assignment}`: not a valid identifier\n");
                eprint!("{msg}");
                return CommandResult::error(msg, 1);
            }

            // SAFETY: シェルプロセス内でシングルスレッドで呼ばれるため安全
            unsafe {
                env::set_var(key, value);
            }
        } else {
            // KEY のみ → 該当変数の値を表示
            match env::var(assignment) {
                Ok(value) => {
                    let line = format!("{assignment}={value}\n");
                    print!("{line}");
                    output.push_str(&line);
                }
                Err(_) => {
                    let msg = format!("jarvish: export: `{assignment}`: not set\n");
                    eprint!("{msg}");
                    return CommandResult::error(msg, 1);
                }
            }
        }
    }

    CommandResult::success(output)
}

/// 全環境変数をソート済みで `KEY=VALUE` 形式で表示する。
fn list_all_vars() -> CommandResult {
    let mut vars: Vec<(String, String)> = env::vars().collect();
    vars.sort_by(|a, b| a.0.cmp(&b.0));

    let mut output = String::new();
    for (key, value) in &vars {
        let line = format!("{key}={value}\n");
        output.push_str(&line);
    }
    print!("{output}");

    CommandResult::success(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    /// テスト中に環境変数を安全に設定・復元するガード。
    struct EnvGuard {
        key: String,
        original: Option<String>,
    }

    impl EnvGuard {
        fn new(key: &str) -> Self {
            let original = env::var(key).ok();
            Self {
                key: key.to_string(),
                original,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(ref val) = self.original {
                    env::set_var(&self.key, val);
                } else {
                    env::remove_var(&self.key);
                }
            }
        }
    }

    #[test]
    #[serial]
    fn export_set_variable() {
        let _guard = EnvGuard::new("JARVISH_TEST_EXPORT");
        let result = execute(&["JARVISH_TEST_EXPORT=hello"]);
        assert_eq!(result.exit_code, 0);
        assert_eq!(env::var("JARVISH_TEST_EXPORT").unwrap(), "hello");
    }

    #[test]
    #[serial]
    fn export_set_empty_value() {
        let _guard = EnvGuard::new("JARVISH_TEST_EMPTY");
        let result = execute(&["JARVISH_TEST_EMPTY="]);
        assert_eq!(result.exit_code, 0);
        assert_eq!(env::var("JARVISH_TEST_EMPTY").unwrap(), "");
    }

    #[test]
    #[serial]
    fn export_show_variable() {
        let _guard = EnvGuard::new("JARVISH_TEST_SHOW");
        unsafe {
            env::set_var("JARVISH_TEST_SHOW", "world");
        }
        let result = execute(&["JARVISH_TEST_SHOW"]);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("JARVISH_TEST_SHOW=world"));
    }

    #[test]
    #[serial]
    fn export_show_unset_variable_returns_error() {
        let _guard = EnvGuard::new("JARVISH_TEST_UNSET_VAR");
        unsafe {
            env::remove_var("JARVISH_TEST_UNSET_VAR");
        }
        let result = execute(&["JARVISH_TEST_UNSET_VAR"]);
        assert_ne!(result.exit_code, 0);
        assert!(result.stderr.contains("not set"));
    }

    #[test]
    fn export_no_args_lists_all() {
        let result = execute(&[]);
        assert_eq!(result.exit_code, 0);
        // PATH は必ず存在するはず
        assert!(result.stdout.contains("PATH="));
    }

    #[test]
    fn export_invalid_identifier() {
        let result = execute(&["=value"]);
        assert_ne!(result.exit_code, 0);
        assert!(result.stderr.contains("not a valid identifier"));
    }

    #[test]
    fn export_help_returns_success() {
        let result = execute(&["--help"]);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("export"));
    }
}
