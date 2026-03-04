use std::env;
use std::path::{Path, PathBuf};

use clap::Parser;

use crate::engine::CommandResult;

/// pushd: ディレクトリをスタックに積んで移動する。
#[derive(Parser)]
#[command(name = "pushd", about = "Push directory onto stack and change to it")]
struct PushdArgs {
    /// Target directory (swaps top two entries if omitted)
    dir: Option<String>,
}

/// popd: スタックからディレクトリを取り出して移動する。
#[derive(Parser)]
#[command(name = "popd", about = "Pop directory from stack and change to it")]
struct PopdArgs {}

/// dirs: ディレクトリスタックを表示する。
#[derive(Parser)]
#[command(name = "dirs", about = "Display directory stack")]
struct DirsArgs {
    /// Clear the directory stack
    #[arg(short = 'c')]
    clear: bool,
}

/// pushd: ディレクトリをスタックに積んで移動する。
///
/// - 引数あり → カレントディレクトリをスタックに push し、指定ディレクトリに cd
/// - 引数なし → カレントディレクトリとスタック先頭を swap し、旧スタック先頭に cd
pub(crate) fn execute_pushd(args: &[&str], dir_stack: &mut Vec<PathBuf>) -> CommandResult {
    let parsed = match super::parse_args::<PushdArgs>("pushd", args) {
        Ok(a) => a,
        Err(result) => return result,
    };

    let current = match env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("jarvish: pushd: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    match parsed.dir {
        Some(dir) => {
            let target = PathBuf::from(&dir);
            if let Err(result) = change_dir(&target) {
                return result;
            }
            dir_stack.push(current);
        }
        None => {
            let top = match dir_stack.pop() {
                Some(d) => d,
                None => {
                    let msg = "jarvish: pushd: no other directory\n".to_string();
                    eprint!("{msg}");
                    return CommandResult::error(msg, 1);
                }
            };
            if let Err(result) = change_dir(&top) {
                dir_stack.push(top);
                return result;
            }
            dir_stack.push(current);
        }
    }

    CommandResult::success(String::new())
}

/// popd: スタック先頭を pop し、そのディレクトリに cd する。
pub(crate) fn execute_popd(args: &[&str], dir_stack: &mut Vec<PathBuf>) -> CommandResult {
    if let Err(result) = super::parse_args::<PopdArgs>("popd", args) {
        return result;
    }

    let target = match dir_stack.pop() {
        Some(d) => d,
        None => {
            let msg = "jarvish: popd: directory stack empty\n".to_string();
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    if let Err(result) = change_dir(&target) {
        dir_stack.push(target);
        return result;
    }

    CommandResult::success(String::new())
}

/// dirs: ディレクトリスタックを表示する。
///
/// - `-c` → スタックをクリア
/// - `-v` → インデックス番号付きで1行ずつ表示
/// - `-p` → 1行ずつ表示
pub(crate) fn execute_dirs(args: &[&str], dir_stack: &mut Vec<PathBuf>) -> CommandResult {
    let parsed = match super::parse_args::<DirsArgs>("dirs", args) {
        Ok(a) => a,
        Err(result) => return result,
    };

    if parsed.clear {
        dir_stack.clear();
        return CommandResult::success(String::new());
    }

    let current = env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut entries: Vec<String> = Vec::with_capacity(dir_stack.len() + 1);
    entries.push(current);
    for path in dir_stack.iter().rev() {
        entries.push(path.to_string_lossy().into_owned());
    }

    let mut output = String::from("Directory Stacks:\n");
    for (i, e) in entries.iter().enumerate() {
        output.push_str(&format!("  {}. {e}\n", i + 1));
    }

    print!("{output}");
    CommandResult::success(output)
}

/// ディレクトリを変更し、PWD / OLDPWD 環境変数を更新する。
fn change_dir(target: &Path) -> Result<(), CommandResult> {
    let old_pwd = env::var("PWD").ok().or_else(|| {
        env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    });

    env::set_current_dir(target).map_err(|e| {
        let msg = format!("jarvish: {}: {e}\n", target.display());
        eprint!("{msg}");
        CommandResult::error(msg, 1)
    })?;

    if let Some(old) = old_pwd {
        env::set_var("OLDPWD", &old);
    }
    if let Ok(new_pwd) = env::current_dir() {
        env::set_var("PWD", &new_pwd);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::builtins::cwd::test_helpers::CwdGuard;
    use crate::engine::LoopAction;
    use serial_test::serial;

    // ── pushd ──

    #[test]
    #[serial]
    fn pushd_changes_directory_and_pushes_old() {
        let _guard = CwdGuard::new();
        let original = env::current_dir().unwrap();
        let tmpdir = tempfile::tempdir().unwrap();
        let target = tmpdir.path().to_path_buf();

        let mut stack = Vec::new();
        let result = execute_pushd(&[target.to_str().unwrap()], &mut stack);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.action, LoopAction::Continue);

        let cwd = env::current_dir().unwrap();
        assert_eq!(cwd.canonicalize().unwrap(), target.canonicalize().unwrap());

        assert_eq!(stack.len(), 1);
        assert_eq!(
            stack[0].canonicalize().unwrap(),
            original.canonicalize().unwrap()
        );
    }

    #[test]
    #[serial]
    fn pushd_no_args_swaps_top() {
        let _guard = CwdGuard::new();
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();

        env::set_current_dir(dir1.path()).unwrap();
        let mut stack = vec![dir2.path().to_path_buf()];

        let result = execute_pushd(&[], &mut stack);
        assert_eq!(result.exit_code, 0);

        let cwd = env::current_dir().unwrap();
        assert_eq!(
            cwd.canonicalize().unwrap(),
            dir2.path().canonicalize().unwrap()
        );

        assert_eq!(stack.len(), 1);
        assert_eq!(
            stack[0].canonicalize().unwrap(),
            dir1.path().canonicalize().unwrap()
        );
    }

    #[test]
    #[serial]
    fn pushd_no_args_empty_stack_errors() {
        let _guard = CwdGuard::new();
        let mut stack = Vec::new();
        let result = execute_pushd(&[], &mut stack);
        assert_ne!(result.exit_code, 0);
        assert!(result.stderr.contains("no other directory"));
    }

    #[test]
    fn pushd_help_returns_success() {
        let mut stack = Vec::new();
        let result = execute_pushd(&["--help"], &mut stack);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("pushd"));
    }

    // ── popd ──

    #[test]
    #[serial]
    fn popd_changes_to_stack_top() {
        let _guard = CwdGuard::new();
        let tmpdir = tempfile::tempdir().unwrap();
        let target = tmpdir.path().to_path_buf();

        let mut stack = vec![target.clone()];
        let result = execute_popd(&[], &mut stack);
        assert_eq!(result.exit_code, 0);

        let cwd = env::current_dir().unwrap();
        assert_eq!(cwd.canonicalize().unwrap(), target.canonicalize().unwrap());
        assert!(stack.is_empty());
    }

    #[test]
    #[serial]
    fn popd_empty_stack_errors() {
        let _guard = CwdGuard::new();
        let mut stack = Vec::new();
        let result = execute_popd(&[], &mut stack);
        assert_ne!(result.exit_code, 0);
        assert!(result.stderr.contains("directory stack empty"));
    }

    #[test]
    fn popd_help_returns_success() {
        let mut stack = Vec::new();
        let result = execute_popd(&["--help"], &mut stack);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("popd"));
    }

    // ── dirs ──

    #[test]
    #[serial]
    fn dirs_shows_current_and_stack() {
        let _guard = CwdGuard::new();
        let current = env::current_dir().unwrap();
        let dir1 = tempfile::tempdir().unwrap();

        let mut stack = vec![dir1.path().to_path_buf()];
        let result = execute_dirs(&[], &mut stack);
        assert_eq!(result.exit_code, 0);
        assert!(result
            .stdout
            .contains(&current.to_string_lossy().to_string()));
        assert!(result
            .stdout
            .contains(&dir1.path().to_string_lossy().to_string()));
    }

    #[test]
    #[serial]
    fn dirs_clear_empties_stack() {
        let _guard = CwdGuard::new();
        let tmpdir = tempfile::tempdir().unwrap();
        let mut stack = vec![tmpdir.path().to_path_buf()];

        let result = execute_dirs(&["-c"], &mut stack);
        assert_eq!(result.exit_code, 0);
        assert!(stack.is_empty());
    }

    #[test]
    #[serial]
    fn dirs_shows_numbered_list() {
        let _guard = CwdGuard::new();
        let tmpdir = tempfile::tempdir().unwrap();
        let mut stack = vec![tmpdir.path().to_path_buf()];

        let result = execute_dirs(&[], &mut stack);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.starts_with("Directory Stacks:\n"));
        assert!(result.stdout.contains("  1. "));
        assert!(result.stdout.contains("  2. "));
    }

    #[test]
    fn dirs_help_returns_success() {
        let mut stack = Vec::new();
        let result = execute_dirs(&["--help"], &mut stack);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("dirs"));
    }
}
