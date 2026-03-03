//! AI パイプ (`cmd | ai "prompt"`) の検出と実行

use tracing::debug;

use crate::engine::{builtins, exec, expand, parser};

/// AI パイプ (`cmd | ai "prompt"`) の検出結果。
/// 手前パイプラインの実行結果と AI に渡すプロンプトを保持する。
pub struct AiPipeRequest {
    /// AI に渡すユーザー指示（`ai` の引数）
    pub prompt: String,
    /// 手前パイプラインの stdout キャプチャ結果
    pub stdin_text: String,
    /// 手前パイプラインの終了コード
    pub exit_code: i32,
}

/// ユーザー入力が AI パイプ (`cmd | ai "prompt"`) であるかを判定し、
/// 該当する場合は手前のパイプラインを実行して stdout をキャプチャする。
///
/// v1 制約: 接続演算子（`&&`, `||`, `;`）との組み合わせは非対応。
pub fn try_execute_ai_pipe(input: &str) -> Option<AiPipeRequest> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    let tokens = shell_words::split(input).ok()?;
    if tokens.is_empty() {
        return None;
    }

    if tokens
        .iter()
        .any(|t| matches!(t.as_str(), "&&" | "||" | ";"))
    {
        return None;
    }

    let expanded: Vec<String> = tokens
        .into_iter()
        .map(|t| {
            if matches!(t.as_str(), "|" | ">" | ">>" | "<") {
                t
            } else {
                expand::expand_token(&t)
            }
        })
        .collect();

    let pipeline = parser::parse_pipeline(expanded).ok()?;
    let (prompt, remaining) = pipeline.extract_ai_filter()?;

    debug!(prompt = %prompt, "AI pipe detected, executing source pipeline");

    let remaining = if remaining.commands.len() > 1 {
        let first = &remaining.commands[0];
        let args: Vec<&str> = first.args.iter().map(|s| s.as_str()).collect();
        if let Some(result) = builtins::dispatch_builtin(&first.cmd, &args) {
            if result.exit_code != 0 {
                return Some(AiPipeRequest {
                    prompt,
                    stdin_text: result.stdout,
                    exit_code: result.exit_code,
                });
            }
            let mut new_commands = remaining.commands.clone();
            new_commands[0] = parser::SimpleCommand {
                cmd: "printf".to_string(),
                args: vec!["%s".to_string(), result.stdout],
                redirects: vec![],
            };
            parser::Pipeline {
                commands: new_commands,
            }
        } else {
            remaining
        }
    } else {
        let first = &remaining.commands[0];
        let args: Vec<&str> = first.args.iter().map(|s| s.as_str()).collect();
        if let Some(result) = builtins::dispatch_builtin(&first.cmd, &args) {
            return Some(AiPipeRequest {
                prompt,
                stdin_text: result.stdout,
                exit_code: result.exit_code,
            });
        }
        remaining
    };

    let result = exec::run_pipeline_captured(&remaining);

    Some(AiPipeRequest {
        prompt,
        stdin_text: result.stdout,
        exit_code: result.exit_code,
    })
}
