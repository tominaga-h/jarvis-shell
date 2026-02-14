use std::env;
use std::path::PathBuf;

use clap::Parser;

use crate::engine::CommandResult;

/// cd: カレントディレクトリを変更する。
#[derive(Parser)]
#[command(name = "cd", about = "カレントディレクトリを変更する")]
struct CdArgs {
    /// 移動先のパス (省略時は $HOME)
    path: Option<String>,
}

/// cd: カレントディレクトリを変更する。
/// - 引数なし → `$HOME` へ移動
/// - 引数あり → 指定パスへ移動
///   展開は execute 側で実施済み
pub(super) fn execute(args: &[&str]) -> CommandResult {
    let parsed = match super::parse_args::<CdArgs>("cd", args) {
        Ok(a) => a,
        Err(result) => return result,
    };

    let target: PathBuf = if let Some(path) = parsed.path {
        PathBuf::from(path)
    } else {
        // 引数なしの場合は $HOME へ
        match env::var_os("HOME") {
            Some(home) => PathBuf::from(home),
            None => {
                let msg = "jarvish: cd: HOME not set\n".to_string();
                eprint!("{msg}");
                return CommandResult::error(msg, 1);
            }
        }
    };

    match env::set_current_dir(&target) {
        Ok(()) => CommandResult::success(String::new()),
        Err(e) => {
            let msg = format!("jarvish: cd: {}: {e}\n", target.display());
            eprint!("{msg}");
            CommandResult::error(msg, 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::builtins::cwd::test_helpers::CwdGuard;
    use crate::engine::LoopAction;
    use serial_test::serial;
    use std::env;
    use std::path::PathBuf;

    #[test]
    #[serial]
    fn cd_to_specified_directory() {
        let _guard = CwdGuard::new();
        let tmpdir = tempfile::tempdir().expect("failed to create tempdir");
        let target = tmpdir.path().to_path_buf();

        let result = execute(&[target.to_str().unwrap()]);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.action, LoopAction::Continue);

        let cwd = env::current_dir().unwrap();
        // macOS では /tmp → /private/tmp にシンボリックリンクされるため canonicalize する
        assert_eq!(cwd.canonicalize().unwrap(), target.canonicalize().unwrap());
    }

    #[test]
    #[serial]
    fn cd_no_args_goes_home() {
        let _guard = CwdGuard::new();
        if let Some(home) = env::var_os("HOME") {
            let result = execute(&[]);
            assert_eq!(result.exit_code, 0);

            let cwd = env::current_dir().unwrap();
            assert_eq!(
                cwd.canonicalize().unwrap(),
                PathBuf::from(&home).canonicalize().unwrap()
            );
        }
    }

    #[test]
    #[serial]
    fn cd_nonexistent_path_returns_error() {
        let _guard = CwdGuard::new();
        let result = execute(&["/nonexistent_path_that_does_not_exist"]);
        assert_ne!(result.exit_code, 0);
        assert!(result.stderr.contains("cd:"));
    }

    #[test]
    fn cd_help_returns_success() {
        let result = execute(&["--help"]);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("cd"));
    }
}
