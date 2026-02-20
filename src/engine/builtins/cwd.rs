use std::env;

use clap::Parser;

use crate::engine::CommandResult;

/// cwd: 現在のカレントディレクトリを表示する。
#[derive(Parser)]
#[command(name = "cwd", about = "Print the current working directory")]
struct CwdArgs {}

/// cwd: 現在のカレントディレクトリを出力する。
pub(super) fn execute(args: &[&str]) -> CommandResult {
    if let Err(result) = super::parse_args::<CwdArgs>("cwd", args) {
        return result;
    }

    match env::current_dir() {
        Ok(path) => {
            let output = format!("{}\n", path.display());
            print!("{output}");
            CommandResult::success(output)
        }
        Err(e) => {
            let msg = format!("jarvish: cwd: {e}\n");
            eprint!("{msg}");
            CommandResult::error(msg, 1)
        }
    }
}

/// テスト用ヘルパー（兄弟モジュールからも参照可能）
#[cfg(test)]
pub(crate) mod test_helpers {
    use std::env;
    use std::path::PathBuf;

    /// テスト中にカレントディレクトリを安全に変更・復元するガード。
    /// Drop 時に元のディレクトリおよび $PWD / $OLDPWD 環境変数を自動復元する。
    pub(crate) struct CwdGuard {
        original: PathBuf,
        original_pwd: Option<String>,
        original_oldpwd: Option<String>,
    }

    impl CwdGuard {
        pub(crate) fn new() -> Self {
            Self {
                original: env::current_dir().expect("failed to get current dir"),
                original_pwd: env::var("PWD").ok(),
                original_oldpwd: env::var("OLDPWD").ok(),
            }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = env::set_current_dir(&self.original);
            // PWD 環境変数を復元
            match &self.original_pwd {
                Some(pwd) => env::set_var("PWD", pwd),
                None => env::remove_var("PWD"),
            }
            // OLDPWD 環境変数を復元
            match &self.original_oldpwd {
                Some(oldpwd) => env::set_var("OLDPWD", oldpwd),
                None => env::remove_var("OLDPWD"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_helpers::CwdGuard;
    use super::*;
    use serial_test::serial;
    use std::env;
    use std::path::PathBuf;

    #[test]
    #[serial]
    fn cwd_returns_current_directory() {
        let _guard = CwdGuard::new();
        let expected = env::current_dir().unwrap();
        let result = execute(&[]);
        assert_eq!(result.exit_code, 0);
        assert!(!result.stdout.trim().is_empty());
        assert_eq!(
            PathBuf::from(result.stdout.trim()).canonicalize().unwrap(),
            expected.canonicalize().unwrap()
        );
    }

    #[test]
    fn cwd_help_returns_success() {
        let result = execute(&["--help"]);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("cwd"));
    }
}
