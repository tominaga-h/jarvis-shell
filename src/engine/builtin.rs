use std::env;
use std::path::PathBuf;

#[allow(unused_imports)]
use super::{CommandResult, LoopAction};

/// 指定されたコマンド名がビルトインかどうかを判定する（軽量チェック用）。
pub fn is_builtin(cmd: &str) -> bool {
    matches!(cmd, "cd" | "cwd" | "exit")
}

/// ビルトインコマンドを振り分ける。
/// ビルトインでない場合は `None` を返し、呼び出し元が外部コマンドとして実行する。
pub fn dispatch_builtin(cmd: &str, args: &[&str]) -> Option<CommandResult> {
    match cmd {
        "cd" => Some(builtin_cd(args)),
        "cwd" => Some(builtin_cwd()),
        "exit" => Some(builtin_exit()),
        _ => None,
    }
}

/// cd: カレントディレクトリを変更する。
/// - 引数なし → `$HOME` へ移動
/// - 引数あり → 指定パスへ移動
/// 展開は execute 側で実施済み
fn builtin_cd(args: &[&str]) -> CommandResult {
    let target: PathBuf = if let Some(path) = args.first() {
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

/// cwd: 現在のカレントディレクトリを出力する。
fn builtin_cwd() -> CommandResult {
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

/// exit: REPL ループを終了する。
fn builtin_exit() -> CommandResult {
    CommandResult::exit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    /// テスト中にカレントディレクトリを安全に変更・復元するヘルパー
    struct CwdGuard {
        original: PathBuf,
    }

    impl CwdGuard {
        fn new() -> Self {
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

    #[test]
    #[serial]
    fn cd_to_specified_directory() {
        let _guard = CwdGuard::new();
        let tmpdir = tempfile::tempdir().expect("failed to create tempdir");
        let target = tmpdir.path().to_path_buf();

        let result = dispatch_builtin("cd", &[target.to_str().unwrap()]).unwrap();
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.action, LoopAction::Continue);

        let cwd = env::current_dir().unwrap();
        // macOS では /tmp → /private/tmp にシンボリックリンクされるため canonicalize する
        assert_eq!(
            cwd.canonicalize().unwrap(),
            target.canonicalize().unwrap()
        );
    }

    #[test]
    #[serial]
    fn cd_no_args_goes_home() {
        let _guard = CwdGuard::new();
        if let Some(home) = env::var_os("HOME") {
            let result = dispatch_builtin("cd", &[]).unwrap();
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
        let result = dispatch_builtin("cd", &["/nonexistent_path_that_does_not_exist"]).unwrap();
        assert_ne!(result.exit_code, 0);
        assert!(result.stderr.contains("cd:"));
    }

    #[test]
    #[serial]
    fn cwd_returns_current_directory() {
        let _guard = CwdGuard::new();
        let expected = env::current_dir().unwrap();
        let result = dispatch_builtin("cwd", &[]).unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(!result.stdout.trim().is_empty());
        assert_eq!(
            PathBuf::from(result.stdout.trim()).canonicalize().unwrap(),
            expected.canonicalize().unwrap()
        );
    }

    #[test]
    fn exit_returns_exit_action() {
        let result = dispatch_builtin("exit", &[]).unwrap();
        assert_eq!(result.action, LoopAction::Exit);
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn unknown_command_returns_none() {
        assert!(dispatch_builtin("ls", &[]).is_none());
        assert!(dispatch_builtin("git", &["status"]).is_none());
    }
}
