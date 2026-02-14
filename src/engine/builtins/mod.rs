mod cd;
mod cwd;
mod exit;
mod export;
mod help;
mod history;
mod unset;

use super::CommandResult;

/// clap の `try_parse_from` を使って引数をパースする共通ヘルパー。
///
/// - パース成功 → `Ok(T)`
/// - `--help` → stdout に出力し `Err(CommandResult::success(...))`
/// - 引数エラー → stderr に出力し `Err(CommandResult::error(..., 2))`
fn parse_args<T: clap::Parser>(cmd: &str, args: &[&str]) -> Result<T, CommandResult> {
    T::try_parse_from(std::iter::once(cmd).chain(args.iter().copied())).map_err(|e| {
        let msg = e.to_string();
        if e.use_stderr() {
            eprint!("{msg}");
            CommandResult::error(msg, 2)
        } else {
            print!("{msg}");
            CommandResult::success(msg)
        }
    })
}

/// 指定されたコマンド名がビルトインかどうかを判定する（軽量チェック用）。
pub fn is_builtin(cmd: &str) -> bool {
    matches!(
        cmd,
        "cd" | "cwd" | "exit" | "export" | "help" | "unset" | "history"
    )
}

/// ビルトインコマンドを振り分ける。
/// ビルトインでない場合は `None` を返し、呼び出し元が外部コマンドとして実行する。
pub fn dispatch_builtin(cmd: &str, args: &[&str]) -> Option<CommandResult> {
    match cmd {
        "cd" => Some(cd::execute(args)),
        "cwd" => Some(cwd::execute(args)),
        "exit" => Some(exit::execute(args)),
        "export" => Some(export::execute(args)),
        "help" => Some(help::execute(args)),
        "unset" => Some(unset::execute(args)),
        "history" => Some(history::execute(args)),
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

    // ── 新規ビルトイン登録テスト ──

    #[test]
    fn new_builtins_are_registered() {
        assert!(is_builtin("export"));
        assert!(is_builtin("help"));
        assert!(is_builtin("unset"));
        assert!(is_builtin("history"));
    }

    #[test]
    fn new_builtins_dispatch_returns_some() {
        // export（引数なし → 全変数表示、正常終了するはず）
        assert!(dispatch_builtin("export", &[]).is_some());
        // history --help → 正常終了
        assert!(dispatch_builtin("history", &["--help"]).is_some());
    }
}
