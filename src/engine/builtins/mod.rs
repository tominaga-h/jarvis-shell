pub(crate) mod alias;
pub(crate) mod cd;
pub(crate) mod cdhist;
pub(crate) mod cdj;
mod cwd;
pub(crate) mod dirstack;
mod exit;
mod export;
mod help;
mod history;
mod restart;
pub(crate) mod source;
pub(crate) mod unalias;
mod unset;
pub(crate) mod update;
pub(crate) mod which_type;
mod wrapper;

use super::CommandResult;

/// ビルトインコマンドの名前と説明の一覧（アルファベット順）。
///
/// `is_builtin` の受理判定・`help` の一覧表示・補完エンジンが共通で参照する
/// 単一の情報源（single source of truth）。
pub(crate) const BUILTIN_COMMANDS: &[(&str, &str)] = &[
    ("alias", "Set or display aliases"),
    ("cd", "Change the current directory"),
    ("cdhist", "Print recently visited directories (LRU)"),
    ("cdj", "Jump to a directory from cd history via fzf"),
    ("cwd", "Print the current working directory"),
    ("dirs", "Display directory stack"),
    ("exit", "Exit the shell"),
    ("export", "Set or display environment variables"),
    ("help", "Display help for builtin commands"),
    ("history", "Display or manage command history"),
    ("popd", "Pop directory from stack and change to it"),
    ("pushd", "Push directory onto stack and change to it"),
    ("pwd", "Print the current working directory (alias of cwd)"),
    ("restart", "Restart the shell process"),
    ("source", "Load a configuration file (TOML)"),
    ("type", "Display information about command type"),
    ("unalias", "Remove aliases"),
    ("unset", "Remove environment variables"),
    ("update", "Update jarvish to the latest version"),
    ("which", "Locate a command (builtin, alias, or external)"),
];

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
    BUILTIN_COMMANDS.iter().any(|(name, _)| *name == cmd)
}

/// ビルトインコマンドを振り分ける。
/// ビルトインでない場合は `None` を返し、呼び出し元が外部コマンドとして実行する。
pub fn dispatch_builtin(cmd: &str, args: &[&str]) -> Option<CommandResult> {
    match cmd {
        "alias" => Some(alias::execute_with_aliases(
            args,
            &mut std::collections::HashMap::new(),
        )),
        "cd" => Some(cd::execute(args, &mut Vec::new())),
        "cdhist" => Some(cdhist::execute(args)),
        "cdj" => Some(cdj::execute_stub(args)),
        "cwd" | "pwd" => Some(cwd::execute(args)),
        "dirs" => Some(dirstack::execute_dirs(args, &mut Vec::new())),
        "exit" => Some(exit::execute(args)),
        "export" => Some(export::execute(args)),
        "help" => Some(help::execute(args)),
        "unalias" => Some(unalias::execute_with_aliases(
            args,
            &mut std::collections::HashMap::new(),
        )),
        "source" => {
            Some(source::parse(args).map_or_else(|e| e, |_| CommandResult::success(String::new())))
        }
        "pushd" => Some(dirstack::execute_pushd(args, &mut Vec::new())),
        "popd" => Some(dirstack::execute_popd(args, &mut Vec::new())),
        "unset" => Some(unset::execute(args)),
        "history" => Some(history::execute(args)),
        "restart" => Some(restart::execute(args)),
        "update" => Some(update::execute(args)),
        "which" => Some(which_type::execute_which(
            args,
            &std::collections::HashMap::new(),
        )),
        "type" => Some(which_type::execute_type(
            args,
            &std::collections::HashMap::new(),
        )),
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
    #[serial]
    fn pwd_is_alias_for_cwd() {
        let _guard = CwdGuard::new();
        assert!(is_builtin("pwd"));
        let pwd_result = dispatch_builtin("pwd", &[]).unwrap();
        assert_eq!(pwd_result.exit_code, 0);
        let cwd_result = dispatch_builtin("cwd", &[]).unwrap();
        assert_eq!(pwd_result.stdout, cwd_result.stdout);
    }

    #[test]
    fn new_builtins_are_registered() {
        assert!(is_builtin("alias"));
        assert!(is_builtin("dirs"));
        assert!(is_builtin("export"));
        assert!(is_builtin("help"));
        assert!(is_builtin("popd"));
        assert!(is_builtin("pushd"));
        assert!(is_builtin("source"));
        assert!(is_builtin("unalias"));
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

    // ── cdhist / cdj 登録テスト ──

    #[test]
    fn cdhist_and_cdj_are_registered() {
        assert!(is_builtin("cdhist"));
        assert!(is_builtin("cdj"));
    }

    #[test]
    fn dispatch_cdhist_returns_some() {
        // --help は確実に成功するため、それで Some が返ることを確認
        let result = dispatch_builtin("cdhist", &["--help"]);
        assert!(result.is_some());
        assert_eq!(result.unwrap().exit_code, 0);
    }

    #[test]
    fn dispatch_cdj_returns_interactive_required_stub() {
        // dispatch 経由では cdj はスタブのエラーを返す
        let result = dispatch_builtin("cdj", &[]).expect("cdj should be registered");
        assert_ne!(result.exit_code, 0);
        assert!(result.stderr.contains("requires interactive shell"));
    }

    #[test]
    fn dispatch_cdj_help_still_works() {
        // --help はスタブ前に clap で処理されるため成功する
        let result = dispatch_builtin("cdj", &["--help"]).expect("cdj should be registered");
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("cdj"));
    }

    // ── BUILTIN_COMMANDS 一元化テーブルの検証 ──

    #[test]
    fn is_builtin_accepts_exact_previous_name_list() {
        // is_builtin が旧来受理していた20コマンドすべてを引き続き受理することを確認
        const EXPECTED: &[&str] = &[
            "alias", "cd", "cdhist", "cdj", "cwd", "dirs", "exit", "export", "help", "history",
            "popd", "pushd", "pwd", "restart", "source", "type", "unalias", "unset", "update",
            "which",
        ];
        assert_eq!(EXPECTED.len(), 20);
        for cmd in EXPECTED {
            assert!(is_builtin(cmd), "{cmd} should be recognized as builtin");
        }

        // ビルトインでないコマンドは受理しない
        for cmd in ["ls", "git", "cat", "pwdx", "aliass"] {
            assert!(
                !is_builtin(cmd),
                "{cmd} should not be recognized as builtin"
            );
        }
    }

    #[test]
    fn builtin_commands_table_is_sorted_and_unique() {
        assert_eq!(BUILTIN_COMMANDS.len(), 20);

        let mut names: Vec<&str> = BUILTIN_COMMANDS.iter().map(|(name, _)| *name).collect();
        let sorted_names = {
            let mut v = names.clone();
            v.sort_unstable();
            v
        };
        assert_eq!(
            names, sorted_names,
            "BUILTIN_COMMANDS must be alphabetically sorted"
        );

        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            BUILTIN_COMMANDS.len(),
            "BUILTIN_COMMANDS must not contain duplicate names"
        );
    }
}
