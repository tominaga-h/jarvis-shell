use std::env;

use clap::Parser;

use crate::engine::CommandResult;

/// cwd: 現在のカレントディレクトリを表示する。
#[derive(Parser)]
#[command(name = "cwd", about = "現在のカレントディレクトリを表示する")]
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
    /// Drop 時に元のディレクトリへ自動復元する。
    pub(crate) struct CwdGuard {
        original: PathBuf,
    }

    impl CwdGuard {
        pub(crate) fn new() -> Self {
            Self {
                original: env::current_dir().expect("failed to get current dir"),
            }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = env::set_current_dir(&self.original);
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
