pub mod builtin;
pub mod classifier;
pub mod exec;
pub mod expand;
pub mod parser;

use tracing::debug;

/// REPL ループの制御アクション
#[derive(Debug, Clone, PartialEq)]
pub enum LoopAction {
    /// ループを続行する
    Continue,
    /// ループを終了する（exit コマンド等）
    Exit,
}

/// コマンド実行の結果を格納する構造体。
/// Phase 2 以降で stdout/stderr を Black Box に永続化する際に使用する。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CommandResult {
    /// キャプチャされた標準出力
    pub stdout: String,
    /// キャプチャされた標準エラー出力
    pub stderr: String,
    /// 終了コード (0 = 成功)
    pub exit_code: i32,
    /// REPL ループの制御アクション
    pub action: LoopAction,
    /// 子プロセスが Alternate Screen Buffer を使用したかどうか。
    /// true の場合、stdout は TUI の画面制御シーケンスであり、
    /// Black Box への保存をスキップすべきことを示す。
    pub used_alt_screen: bool,
}

impl CommandResult {
    /// 成功結果（Continue）を返すヘルパー
    pub fn success(stdout: String) -> Self {
        Self {
            stdout,
            stderr: String::new(),
            exit_code: 0,
            action: LoopAction::Continue,
            used_alt_screen: false,
        }
    }

    /// エラー結果（Continue）を返すヘルパー
    pub fn error(stderr: String, exit_code: i32) -> Self {
        Self {
            stdout: String::new(),
            stderr,
            exit_code,
            action: LoopAction::Continue,
            used_alt_screen: false,
        }
    }

    /// Exit アクションを返すヘルパー
    pub fn exit() -> Self {
        Self {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            action: LoopAction::Exit,
            used_alt_screen: false,
        }
    }
}

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
    if !builtin::is_builtin(first_word) {
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

    let expanded: Vec<String> = tokens.into_iter().map(|t| expand::expand_token(&t)).collect();
    let cmd = &expanded[0];
    let args: Vec<&str> = expanded[1..].iter().map(|s| s.as_str()).collect();

    let result = builtin::dispatch_builtin(cmd, &args);
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
            if t == "|" || t == ">" || t == ">>" || t == "<" {
                t
            } else {
                expand::expand_token(&t)
            }
        })
        .collect();

    // 3. パイプラインにパース
    let pipeline = match parser::parse_pipeline(expanded) {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("jarvish: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    debug!(
        pipeline_length = pipeline.commands.len(),
        first_cmd = %pipeline.commands[0].cmd,
        "execute() parsed pipeline"
    );

    // 4. 単一コマンドの場合はビルトインを試行
    if pipeline.commands.len() == 1 && pipeline.commands[0].redirects.is_empty() {
        let simple = &pipeline.commands[0];
        let args: Vec<&str> = simple.args.iter().map(|s| s.as_str()).collect();
        if let Some(result) = builtin::dispatch_builtin(&simple.cmd, &args) {
            debug!(command = %simple.cmd, "Dispatched as builtin command");
            return result;
        }
    }

    // 5. 外部コマンドまたはパイプラインを実行
    exec::run_pipeline(&pipeline)
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
