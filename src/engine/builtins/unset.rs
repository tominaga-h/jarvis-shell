use std::env;

use clap::Parser;

use crate::engine::CommandResult;

/// unset: 環境変数を削除する。
#[derive(Parser)]
#[command(name = "unset", about = "環境変数を削除する")]
struct UnsetArgs {
    /// 削除する変数名 (1つ以上必須)
    #[arg(required = true)]
    names: Vec<String>,
}

/// unset: 環境変数を削除する。
/// - `unset VAR [VAR2 ...]` → 指定された変数を削除
/// - `unset` (引数なし) → clap がエラー表示
/// - 存在しない変数の unset はサイレントに成功 (bash 互換)
pub(super) fn execute(args: &[&str]) -> CommandResult {
    let parsed = match super::parse_args::<UnsetArgs>("unset", args) {
        Ok(a) => a,
        Err(result) => return result,
    };

    for name in &parsed.names {
        // SAFETY: シェルプロセス内でシングルスレッドで呼ばれるため安全
        unsafe {
            env::remove_var(name);
        }
    }

    CommandResult::success(String::new())
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
    fn unset_removes_variable() {
        let _guard = EnvGuard::new("JARVISH_TEST_UNSET");
        unsafe {
            env::set_var("JARVISH_TEST_UNSET", "to_be_removed");
        }
        assert!(env::var("JARVISH_TEST_UNSET").is_ok());

        let result = execute(&["JARVISH_TEST_UNSET"]);
        assert_eq!(result.exit_code, 0);
        assert!(env::var("JARVISH_TEST_UNSET").is_err());
    }

    #[test]
    #[serial]
    fn unset_multiple_variables() {
        let _guard1 = EnvGuard::new("JARVISH_TEST_MULTI_A");
        let _guard2 = EnvGuard::new("JARVISH_TEST_MULTI_B");
        unsafe {
            env::set_var("JARVISH_TEST_MULTI_A", "a");
            env::set_var("JARVISH_TEST_MULTI_B", "b");
        }

        let result = execute(&["JARVISH_TEST_MULTI_A", "JARVISH_TEST_MULTI_B"]);
        assert_eq!(result.exit_code, 0);
        assert!(env::var("JARVISH_TEST_MULTI_A").is_err());
        assert!(env::var("JARVISH_TEST_MULTI_B").is_err());
    }

    #[test]
    #[serial]
    fn unset_nonexistent_variable_succeeds() {
        // bash 互換: 存在しない変数の unset は成功
        let _guard = EnvGuard::new("JARVISH_TEST_NONEXISTENT");
        unsafe {
            env::remove_var("JARVISH_TEST_NONEXISTENT");
        }
        let result = execute(&["JARVISH_TEST_NONEXISTENT"]);
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn unset_no_args_returns_error() {
        let result = execute(&[]);
        assert_ne!(result.exit_code, 0);
    }

    #[test]
    fn unset_help_returns_success() {
        let result = execute(&["--help"]);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("unset"));
    }
}
