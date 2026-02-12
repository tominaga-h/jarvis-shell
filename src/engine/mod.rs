pub mod builtin;
pub mod exec;

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
pub struct CommandResult {
    /// キャプチャされた標準出力
    pub stdout: String,
    /// キャプチャされた標準エラー出力
    pub stderr: String,
    /// 終了コード (0 = 成功)
    pub exit_code: i32,
    /// REPL ループの制御アクション
    pub action: LoopAction,
}

impl CommandResult {
    /// 成功結果（Continue）を返すヘルパー
    pub fn success(stdout: String) -> Self {
        Self {
            stdout,
            stderr: String::new(),
            exit_code: 0,
            action: LoopAction::Continue,
        }
    }

    /// エラー結果（Continue）を返すヘルパー
    pub fn error(stderr: String, exit_code: i32) -> Self {
        Self {
            stdout: String::new(),
            stderr,
            exit_code,
            action: LoopAction::Continue,
        }
    }

    /// Exit アクションを返すヘルパー
    pub fn exit() -> Self {
        Self {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            action: LoopAction::Exit,
        }
    }
}

/// ユーザー入力をパースし、ビルトインまたは外部コマンドとして実行する。
pub fn execute(input: &str) -> CommandResult {
    let input = input.trim();
    if input.is_empty() {
        return CommandResult::success(String::new());
    }

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

    let cmd = &tokens[0];
    let args: Vec<&str> = tokens[1..].iter().map(|s| s.as_str()).collect();

    // ビルトインコマンドを試行
    if let Some(result) = builtin::dispatch_builtin(cmd, &args) {
        return result;
    }

    // 外部コマンドを実行
    exec::run_external(cmd, &args)
}
