//! AI パイプ / AI リダイレクトの検出と実行
//!
//! - `cmd | ai "prompt"` — フィルタモード（データ変換）
//! - `cmd > ai "prompt"` — リダイレクトモード（Jarvis が対話的に応答）

use tracing::debug;

use crate::engine::{builtins, exec, expand, parser};

/// AI パイプ / リダイレクトの動作モード
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiPipeMode {
    /// `| ai` — テキストフィルタとして動作
    Filter,
    /// `> ai` — Jarvis が対話的にデータを分析・応答
    Redirect,
}

/// AI パイプ / リダイレクトの検出結果。
/// 手前パイプラインの実行結果と AI に渡すプロンプトを保持する。
pub struct AiPipeRequest {
    /// AI に渡すユーザー指示（`ai` の引数）
    pub prompt: String,
    /// 手前パイプラインの stdout キャプチャ結果
    pub stdin_text: String,
    /// 手前パイプラインの終了コード
    pub exit_code: i32,
    /// 動作モード
    pub mode: AiPipeMode,
}

/// ユーザー入力が AI パイプ (`cmd | ai "prompt"`) または
/// AI リダイレクト (`cmd > ai "prompt"`) であるかを判定し、
/// 該当する場合は手前のパイプラインを実行して stdout をキャプチャする。
///
/// v1 制約: 接続演算子（`&&`, `||`, `;`）との組み合わせは非対応。
pub fn try_execute_ai_pipe(input: &str) -> Option<AiPipeRequest> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    let tokens = expand::split_quoted(input).ok()?;
    if tokens.is_empty() {
        return None;
    }

    if tokens
        .iter()
        .any(|t| matches!(t.value.as_str(), "&&" | "||" | ";"))
    {
        return None;
    }

    let mut expanded: Vec<String> = Vec::with_capacity(tokens.len());
    for tok in tokens {
        if matches!(tok.value.as_str(), "|" | ">" | ">>" | "<") {
            expanded.push(tok.value);
            continue;
        }
        if tok.quoted && !tok.has_subst {
            expanded.push(tok.value);
            continue;
        }
        let expanded_result = if tok.quoted && tok.has_subst {
            expand::expand_token_subst_only(&tok.value, tok.subst_quoting)
        } else if tok.has_subst {
            expand::expand_token_globs_with_quoting(&tok.value, tok.subst_quoting)
        } else {
            expand::expand_token_globs(&tok.value)
        };
        match expanded_result {
            Ok(parts) => expanded.extend(parts),
            Err(_) => {
                // AI ルーティング判定中の no-match / 置換失敗は通常パスへフォールスルー
                return None;
            }
        }
    }

    // 1. `| ai "prompt"` パターン（フィルタモード）
    if let Some(req) = try_pipe_ai(&expanded) {
        return Some(req);
    }

    // 2. `> ai "prompt"` パターン（リダイレクトモード）
    if let Some(req) = try_redirect_ai(&expanded) {
        return Some(req);
    }

    None
}

/// `| ai "prompt"` パターンの検出と実行
fn try_pipe_ai(expanded: &[String]) -> Option<AiPipeRequest> {
    let pipeline = parser::parse_pipeline(expanded.to_vec()).ok()?;
    let (prompt, remaining) = pipeline.extract_ai_filter()?;

    debug!(prompt = %prompt, "AI pipe detected, executing source pipeline");
    Some(run_source_pipeline(prompt, remaining, AiPipeMode::Filter))
}

/// `> ai "prompt"` パターンの検出と実行
fn try_redirect_ai(expanded: &[String]) -> Option<AiPipeRequest> {
    let (prompt, source_tokens) = try_extract_ai_redirect(expanded)?;
    let remaining = parser::parse_pipeline(source_tokens).ok()?;

    debug!(prompt = %prompt, "AI redirect detected, executing source pipeline");
    Some(run_source_pipeline(prompt, remaining, AiPipeMode::Redirect))
}

/// トークン列から `> ai "prompt"` パターンを検出する。
///
/// 末尾から `>` + `ai` のペアを探し、`ai` の後ろにプロンプトがあれば
/// `(prompt, source_tokens)` を返す。プロンプトが空、またはソースコマンドが
/// ない場合は通常のファイルリダイレクトとして `None` を返す。
fn try_extract_ai_redirect(tokens: &[String]) -> Option<(String, Vec<String>)> {
    for i in (0..tokens.len().saturating_sub(1)).rev() {
        if tokens[i] == ">" && tokens.get(i + 1).map(|s| s.as_str()) == Some("ai") {
            let prompt_parts: Vec<&str> = tokens[i + 2..].iter().map(|s| s.as_str()).collect();
            let prompt = prompt_parts.join(" ");
            if prompt.is_empty() {
                return None;
            }
            let source = tokens[..i].to_vec();
            if source.is_empty() {
                return None;
            }
            return Some((prompt, source));
        }
    }
    None
}

/// ソースパイプラインを実行し、`AiPipeRequest` を構築する。
///
/// パイプライン先頭がビルトインの場合はシェル内で実行し、
/// その出力を後続パイプラインの stdin として注入する。
fn run_source_pipeline(
    prompt: String,
    remaining: parser::Pipeline,
    mode: AiPipeMode,
) -> AiPipeRequest {
    let remaining = if remaining.commands.len() > 1 {
        let first = &remaining.commands[0];
        let args: Vec<&str> = first.args.iter().map(|s| s.as_str()).collect();
        if let Some(result) = builtins::dispatch_builtin(&first.cmd, &args) {
            if result.exit_code != 0 {
                return AiPipeRequest {
                    prompt,
                    stdin_text: result.stdout,
                    exit_code: result.exit_code,
                    mode,
                };
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
            return AiPipeRequest {
                prompt,
                stdin_text: result.stdout,
                exit_code: result.exit_code,
                mode,
            };
        }
        remaining
    };

    let result = exec::run_pipeline_captured(&remaining);

    AiPipeRequest {
        prompt,
        stdin_text: result.stdout,
        exit_code: result.exit_code,
        mode,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── try_extract_ai_redirect ──

    #[test]
    fn redirect_ai_simple() {
        let tokens: Vec<String> = vec!["echo", "hello", ">", "ai", "要約して"]
            .into_iter()
            .map(Into::into)
            .collect();
        let (prompt, source) = try_extract_ai_redirect(&tokens).unwrap();
        assert_eq!(prompt, "要約して");
        assert_eq!(source, vec!["echo", "hello"]);
    }

    #[test]
    fn redirect_ai_with_pipe_before() {
        let tokens: Vec<String> = vec!["cmd1", "|", "cmd2", ">", "ai", "分析して"]
            .into_iter()
            .map(Into::into)
            .collect();
        let (prompt, source) = try_extract_ai_redirect(&tokens).unwrap();
        assert_eq!(prompt, "分析して");
        assert_eq!(source, vec!["cmd1", "|", "cmd2"]);
    }

    #[test]
    fn redirect_ai_multi_word_prompt() {
        let tokens: Vec<String> = vec!["ls", "-la", ">", "ai", "translate", "to", "Japanese"]
            .into_iter()
            .map(Into::into)
            .collect();
        let (prompt, source) = try_extract_ai_redirect(&tokens).unwrap();
        assert_eq!(prompt, "translate to Japanese");
        assert_eq!(source, vec!["ls", "-la"]);
    }

    #[test]
    fn redirect_ai_no_prompt_returns_none() {
        let tokens: Vec<String> = vec!["echo", "hello", ">", "ai"]
            .into_iter()
            .map(Into::into)
            .collect();
        assert!(try_extract_ai_redirect(&tokens).is_none());
    }

    #[test]
    fn redirect_ai_no_source_returns_none() {
        let tokens: Vec<String> = vec![">", "ai", "prompt"]
            .into_iter()
            .map(Into::into)
            .collect();
        assert!(try_extract_ai_redirect(&tokens).is_none());
    }

    #[test]
    fn redirect_to_file_not_ai() {
        let tokens: Vec<String> = vec!["echo", "hello", ">", "ai_log.txt"]
            .into_iter()
            .map(Into::into)
            .collect();
        assert!(try_extract_ai_redirect(&tokens).is_none());
    }

    #[test]
    fn redirect_to_normal_file() {
        let tokens: Vec<String> = vec!["echo", "hello", ">", "output.txt"]
            .into_iter()
            .map(Into::into)
            .collect();
        assert!(try_extract_ai_redirect(&tokens).is_none());
    }

    #[test]
    fn append_redirect_not_matched() {
        let tokens: Vec<String> = vec!["echo", "hello", ">>", "ai", "prompt"]
            .into_iter()
            .map(Into::into)
            .collect();
        assert!(try_extract_ai_redirect(&tokens).is_none());
    }

    // ── グロブ展開と AI パイプの相互作用 (#126) ──

    use serial_test::serial;
    use std::env;
    use std::fs;
    use std::path::PathBuf;

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
    fn ai_pipe_with_glob_expands_and_runs_source() {
        // 実 FS にマッチするグロブを含む AI パイプは検出され、source 段は実行される
        // （AI 段は API キー無しで失敗するが、source 実行は確認できる）
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();
        fs::write(dir.path().join("a.txt"), "hello").unwrap();

        let req = try_execute_ai_pipe("cat *.txt | ai \"要約して\"");
        assert!(req.is_some(), "AI pipe should be detected and executed");
        let req = req.unwrap();
        assert_eq!(req.prompt, "要約して");
        assert_eq!(req.stdin_text, "hello");
        assert_eq!(req.mode, AiPipeMode::Filter);
    }

    #[test]
    #[serial]
    fn ai_pipe_with_glob_no_match_returns_none() {
        // グロブが no-match の場合は AI パイプ検出は None を返し、
        // 呼び出し側の通常パスで no-match エラーを処理させる
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();

        let req = try_execute_ai_pipe("cat *.nonexistent_xyz | ai \"prompt\"");
        assert!(req.is_none());
    }
}
