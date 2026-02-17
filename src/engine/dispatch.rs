//! 入力ディスパッチ
//!
//! ユーザー入力をビルトインコマンドまたは外部コマンドとして実行する。
//! トークン分割、シェル展開、パイプラインパースを経て、
//! 適切な実行パスに振り分ける。

use tracing::debug;

use super::{builtins, exec, expand, parser, CommandResult};

/// ビルトインコマンドのみを試行する。
/// ビルトインでなければ None を返す（AI ルーティング前のチェック用）。
///
/// 先頭ワードがビルトインキーワード（cd, cwd, exit）でない場合は
/// パースを行わず即座に None を返す。これにより、自然言語中の
/// アポストロフィ等によるパースエラーが AI ルーティングをブロックしない。
pub fn try_builtin(input: &str) -> Option<CommandResult> {
    let input = input.trim();
    if input.is_empty() {
        return Some(CommandResult::success(String::new()));
    }

    // 先頭ワードがビルトインでなければ即 None → AI に回す
    let first_word = input.split_whitespace().next().unwrap_or("");
    if !builtins::is_builtin(first_word) {
        debug!(
            command = %first_word,
            is_builtin = false,
            "try_builtin check"
        );
        return None;
    }

    // ビルトインのみフルパース
    let tokens = match shell_words::split(input) {
        Ok(tokens) => tokens,
        Err(e) => {
            let msg = format!("jarvish: parse error: {e}\n");
            eprint!("{msg}");
            return Some(CommandResult::error(msg, 1));
        }
    };

    if tokens.is_empty() {
        return Some(CommandResult::success(String::new()));
    }

    // パイプ/リダイレクト/接続演算子を含む場合は None を返す。
    // → execute() 側でコマンドリスト処理される。
    if tokens
        .iter()
        .any(|t| matches!(t.as_str(), "|" | ">" | ">>" | "<" | "&&" | "||" | ";"))
    {
        debug!(
            command = %first_word,
            "try_builtin: contains pipe/redirect/connector, deferring to execute()"
        );
        return None;
    }

    let expanded: Vec<String> = tokens
        .into_iter()
        .map(|t| expand::expand_token(&t))
        .collect();
    let cmd = &expanded[0];
    let args: Vec<&str> = expanded[1..].iter().map(|s| s.as_str()).collect();

    let result = builtins::dispatch_builtin(cmd, &args);
    debug!(
        command = %cmd,
        is_builtin = result.is_some(),
        "try_builtin check"
    );
    result
}

/// ユーザー入力をパースし、ビルトインまたは外部コマンドとして実行する。
///
/// パイプライン（`|`）やリダイレクト（`>`, `>>`, `<`）を含むコマンドに対応。
/// 単一コマンドでビルトインの場合はビルトインとして処理し、
/// それ以外は `exec::run_pipeline()` でパイプライン実行する。
pub fn execute(input: &str) -> CommandResult {
    let input = input.trim();
    if input.is_empty() {
        return CommandResult::success(String::new());
    }

    // 1. トークン分割
    let tokens = match shell_words::split(input) {
        Ok(tokens) => tokens,
        Err(e) => {
            let msg = format!("jarvish: parse error: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    if tokens.is_empty() {
        return CommandResult::success(String::new());
    }

    // 2. 各トークンにシェル展開を適用（ただし演算子は展開しない）
    let expanded: Vec<String> = tokens
        .into_iter()
        .map(|t| {
            if matches!(t.as_str(), "|" | ">" | ">>" | "<" | "&&" | "||" | ";") {
                t
            } else {
                expand::expand_token(&t)
            }
        })
        .collect();

    // 3. コマンドリストにパース
    let command_list = match parser::parse_command_list(expanded) {
        Ok(cl) => cl,
        Err(e) => {
            let msg = format!("jarvish: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    debug!(
        pipeline_count = command_list.rest.len() + 1,
        first_cmd = %command_list.first.commands[0].cmd,
        "execute() parsed command list"
    );

    // 4. 単一パイプラインの場合（接続演算子なし）は従来のビルトイン最適化パスを使用
    if command_list.rest.is_empty() {
        let pipeline = &command_list.first;
        return execute_pipeline(pipeline);
    }

    // 5. 複数パイプライン: run_command_list() で実行
    //    各パイプラインのビルトインも処理するため、ディスパッチ付きで実行
    run_command_list_with_builtins(&command_list)
}

/// 単一パイプラインを実行する（ビルトイン最適化パス付き）。
fn execute_pipeline(pipeline: &parser::Pipeline) -> CommandResult {
    // 単一コマンドの場合はビルトインを試行
    if pipeline.commands.len() == 1 && pipeline.commands[0].redirects.is_empty() {
        let simple = &pipeline.commands[0];
        let args: Vec<&str> = simple.args.iter().map(|s| s.as_str()).collect();
        if let Some(result) = builtins::dispatch_builtin(&simple.cmd, &args) {
            debug!(command = %simple.cmd, "Dispatched as builtin command");
            return result;
        }
    }

    // パイプラインの先頭がビルトインの場合、実行して出力を後続に渡す
    if pipeline.commands.len() > 1 {
        let first = &pipeline.commands[0];
        let args: Vec<&str> = first.args.iter().map(|s| s.as_str()).collect();
        if let Some(result) = builtins::dispatch_builtin(&first.cmd, &args) {
            debug!(
                command = %first.cmd,
                exit_code = result.exit_code,
                "Builtin at pipeline head, replacing with printf"
            );
            if result.exit_code != 0 {
                return result;
            }
            // ビルトインの stdout を printf で出力するコマンドに置き換え
            let mut new_commands = pipeline.commands.clone();
            new_commands[0] = parser::SimpleCommand {
                cmd: "printf".to_string(),
                args: vec!["%s".to_string(), result.stdout],
                redirects: vec![],
            };
            let new_pipeline = parser::Pipeline {
                commands: new_commands,
            };
            return exec::run_pipeline(&new_pipeline);
        }
    }

    // 外部コマンドまたはパイプラインを実行
    exec::run_pipeline(pipeline)
}

/// コマンドリストをビルトイン対応で実行する。
///
/// `exec::run_command_list()` と異なり、各パイプラインの実行時に
/// ビルトインコマンドの最適化パスを適用する。
fn run_command_list_with_builtins(list: &parser::CommandList) -> CommandResult {
    use super::LoopAction;
    use parser::Connector;

    let mut result = execute_pipeline(&list.first);

    if result.action == LoopAction::Exit {
        return result;
    }

    for (connector, pipeline) in &list.rest {
        let should_run = match connector {
            Connector::And => result.exit_code == 0,
            Connector::Or => result.exit_code != 0,
            Connector::Semi => true,
        };

        if should_run {
            let next = execute_pipeline(pipeline);
            result.stdout.push_str(&next.stdout);
            result.stderr.push_str(&next.stderr);
            result.exit_code = next.exit_code;
            result.used_alt_screen = result.used_alt_screen || next.used_alt_screen;

            if next.action == LoopAction::Exit {
                result.action = LoopAction::Exit;
                return result;
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::LoopAction;
    use serial_test::serial;
    use std::env;
    use std::path::PathBuf;

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

    // ── try_builtin: アポストロフィ問題の修正テスト ──

    #[test]
    fn try_builtin_apostrophe_returns_none() {
        // "I'm tired, Jarvis." のようなアポストロフィ入力は
        // ビルトインではないので None を返し、AI ルーティングに進むべき
        assert!(try_builtin("I'm tired, Jarvis.").is_none());
    }

    #[test]
    fn try_builtin_natural_language_returns_none() {
        assert!(try_builtin("jarvis, how are you doing?").is_none());
        assert!(try_builtin("J, please commit").is_none());
        assert!(try_builtin("What's the error?").is_none());
    }

    #[test]
    #[serial]
    fn try_builtin_cd_still_works() {
        let _guard = CwdGuard::new();
        let result = try_builtin("cd /tmp");
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn try_builtin_exit_still_works() {
        let result = try_builtin("exit");
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.action, LoopAction::Exit);
    }

    #[test]
    fn try_builtin_non_builtin_command_returns_none() {
        assert!(try_builtin("git status").is_none());
        assert!(try_builtin("ls -la").is_none());
        assert!(try_builtin("echo hello").is_none());
    }

    // ── try_builtin: パイプ/リダイレクト演算子を含む場合は None ──

    #[test]
    fn try_builtin_with_pipe_returns_none() {
        // ビルトインでもパイプを含む場合は execute() に委譲する
        assert!(try_builtin("history | less").is_none());
        assert!(try_builtin("export | grep PATH").is_none());
        assert!(try_builtin("cwd | cat").is_none());
    }

    #[test]
    fn try_builtin_with_redirect_returns_none() {
        // リダイレクトを含む場合も execute() に委譲する
        assert!(try_builtin("history > /tmp/hist.txt").is_none());
        assert!(try_builtin("export >> /tmp/env.txt").is_none());
    }

    // ── execute: パイプ ──

    #[test]
    fn execute_pipe_two_commands() {
        let result = execute("echo hello | cat");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "hello");
    }

    #[test]
    fn execute_pipe_with_grep() {
        let result = execute("printf 'aaa\\nbbb\\nccc\\n' | grep bbb");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "bbb");
    }

    // ── execute: リダイレクト ──

    #[test]
    fn execute_redirect_stdout_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let cmd = format!("echo redirected > {}", path.display());

        let result = execute(&cmd);
        assert_eq!(result.exit_code, 0);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.trim(), "redirected");
    }

    #[test]
    fn execute_redirect_stdout_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        std::fs::write(&path, "first\n").unwrap();

        let cmd = format!("echo second >> {}", path.display());
        let result = execute(&cmd);
        assert_eq!(result.exit_code, 0);

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("first"));
        assert!(contents.contains("second"));
    }

    #[test]
    fn execute_redirect_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.txt");
        std::fs::write(&path, "from_file\n").unwrap();

        let cmd = format!("cat < {}", path.display());
        let result = execute(&cmd);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "from_file");
    }

    // ── execute: ビルトイン + パイプライン統合 ──

    #[test]
    #[serial]
    fn execute_cd_still_works() {
        let _guard = CwdGuard::new();
        let result = execute("cd /tmp");
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn execute_simple_command() {
        let result = execute("echo test123");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "test123");
    }

    // ── execute: ビルトイン先頭パイプライン ──

    #[test]
    #[serial]
    fn execute_builtin_pipe_to_cat() {
        let _guard = CwdGuard::new();
        // cwd | cat → 現在のディレクトリが出力される
        let expected = env::current_dir().unwrap();
        let result = execute("cwd | cat");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), expected.display().to_string());
    }

    #[test]
    #[serial]
    fn execute_builtin_pipe_to_grep() {
        // export | grep PATH → PATH を含む行が出力される
        let result = execute("export | grep PATH");
        assert_eq!(result.exit_code, 0);
        assert!(
            result.stdout.contains("PATH"),
            "expected stdout to contain PATH, got: {}",
            result.stdout
        );
    }

    // ── execute: && 演算子 ──

    #[test]
    fn execute_and_both_succeed() {
        let result = execute("echo hello && echo world");
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello"));
        assert!(result.stdout.contains("world"));
    }

    #[test]
    fn execute_and_first_fails() {
        let result = execute("false && echo skipped");
        assert_eq!(result.exit_code, 1);
        assert!(!result.stdout.contains("skipped"));
    }

    #[test]
    fn execute_and_three_commands() {
        let result = execute("echo a && echo b && echo c");
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("a"));
        assert!(result.stdout.contains("b"));
        assert!(result.stdout.contains("c"));
    }

    // ── execute: || 演算子 ──

    #[test]
    fn execute_or_first_fails() {
        let result = execute("false || echo fallback");
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("fallback"));
    }

    #[test]
    fn execute_or_first_succeeds() {
        let result = execute("true || echo skipped");
        assert_eq!(result.exit_code, 0);
        assert!(!result.stdout.contains("skipped"));
    }

    // ── execute: ; 演算子 ──

    #[test]
    fn execute_semi_always_runs() {
        let result = execute("false ; echo always");
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("always"));
    }

    // ── execute: 混合 ──

    #[test]
    fn execute_and_then_or() {
        // false && echo skip || echo rescue
        let result = execute("false && echo skip || echo rescue");
        assert_eq!(result.exit_code, 0);
        assert!(!result.stdout.contains("skip"));
        assert!(result.stdout.contains("rescue"));
    }

    // ── try_builtin: 接続演算子を含む場合は None ──

    #[test]
    fn try_builtin_with_and_returns_none() {
        assert!(try_builtin("cd /tmp && echo done").is_none());
    }

    #[test]
    fn try_builtin_with_or_returns_none() {
        assert!(try_builtin("cd /nonexistent || echo fail").is_none());
    }

    #[test]
    fn try_builtin_with_semi_returns_none() {
        assert!(try_builtin("cd /tmp ; echo done").is_none());
    }

    // ── execute: ビルトイン + && ──

    #[test]
    #[serial]
    fn execute_builtin_and_command() {
        let _guard = CwdGuard::new();
        let result = execute("cd /tmp && echo done");
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("done"));
    }
}
