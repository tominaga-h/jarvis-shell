mod cd;
mod cwd;
mod exit;

use super::CommandResult;

/// 指定されたコマンド名がビルトインかどうかを判定する（軽量チェック用）。
pub fn is_builtin(cmd: &str) -> bool {
    matches!(cmd, "cd" | "cwd" | "exit")
}

/// ビルトインコマンドを振り分ける。
/// ビルトインでない場合は `None` を返し、呼び出し元が外部コマンドとして実行する。
pub fn dispatch_builtin(cmd: &str, args: &[&str]) -> Option<CommandResult> {
    match cmd {
        "cd" => Some(cd::execute(args)),
        "cwd" => Some(cwd::execute()),
        "exit" => Some(exit::execute(args)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::cwd::test_helpers::CwdGuard;
    use super::*;
    use serial_test::serial;
    use std::env;
    use std::path::PathBuf;

    #[test]
    fn unknown_command_returns_none() {
        assert!(dispatch_builtin("ls", &[]).is_none());
        assert!(dispatch_builtin("git", &["status"]).is_none());
    }

    // ── cd + cwd 結合テスト ──

    #[test]
    #[serial]
    fn cwd_reflects_cd_change() {
        let _guard = CwdGuard::new();
        let tmpdir = tempfile::tempdir().expect("failed to create tempdir");
        let target = tmpdir.path().to_path_buf();

        // cd で移動
        let cd_result = dispatch_builtin("cd", &[target.to_str().unwrap()]).unwrap();
        assert_eq!(cd_result.exit_code, 0);

        // cwd が移動先を返すことを検証
        let cwd_result = dispatch_builtin("cwd", &[]).unwrap();
        assert_eq!(cwd_result.exit_code, 0);
        assert_eq!(
            PathBuf::from(cwd_result.stdout.trim())
                .canonicalize()
                .unwrap(),
            target.canonicalize().unwrap()
        );
    }

    #[test]
    #[serial]
    fn cwd_unchanged_after_cd_failure() {
        let _guard = CwdGuard::new();
        let before = env::current_dir().unwrap();

        // 存在しないパスへの cd は失敗する
        let cd_result = dispatch_builtin("cd", &["/nonexistent_path_that_does_not_exist"]).unwrap();
        assert_ne!(cd_result.exit_code, 0);

        // cwd は cd 前と同じディレクトリを返すことを検証
        let cwd_result = dispatch_builtin("cwd", &[]).unwrap();
        assert_eq!(cwd_result.exit_code, 0);
        assert_eq!(
            PathBuf::from(cwd_result.stdout.trim())
                .canonicalize()
                .unwrap(),
            before.canonicalize().unwrap()
        );
    }

    #[test]
    #[serial]
    fn cd_sequential_moves_tracked_by_cwd() {
        let _guard = CwdGuard::new();
        let dir1 = tempfile::tempdir().expect("failed to create tempdir");
        let dir2 = tempfile::tempdir().expect("failed to create tempdir");

        // 1回目の cd
        dispatch_builtin("cd", &[dir1.path().to_str().unwrap()]).unwrap();
        let cwd1 = dispatch_builtin("cwd", &[]).unwrap();
        assert_eq!(
            PathBuf::from(cwd1.stdout.trim()).canonicalize().unwrap(),
            dir1.path().canonicalize().unwrap()
        );

        // 2回目の cd（別のディレクトリへ）
        dispatch_builtin("cd", &[dir2.path().to_str().unwrap()]).unwrap();
        let cwd2 = dispatch_builtin("cwd", &[]).unwrap();
        assert_eq!(
            PathBuf::from(cwd2.stdout.trim()).canonicalize().unwrap(),
            dir2.path().canonicalize().unwrap()
        );
    }
}
