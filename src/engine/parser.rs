//! シェル構文パーサー
//!
//! `shell_words::split()` で得たトークン列を、パイプライン（`|`）と
//! リダイレクト（`>`, `>>`, `<`）を含む構造化された `Pipeline` に変換する。

/// I/O リダイレクト
#[derive(Debug, Clone, PartialEq)]
pub enum Redirect {
    /// `> file` — stdout を上書き
    StdoutOverwrite(String),
    /// `>> file` — stdout に追記
    StdoutAppend(String),
    /// `< file` — stdin をファイルから読み込み
    StdinFrom(String),
}

/// パイプラインの 1 セグメント（単一コマンド）
#[derive(Debug, Clone, PartialEq)]
pub struct SimpleCommand {
    /// コマンド名（例: "git"）
    pub cmd: String,
    /// コマンド引数（例: ["log", "--oneline"]）
    pub args: Vec<String>,
    /// このコマンドに付与されたリダイレクト
    pub redirects: Vec<Redirect>,
}

/// パイプ（`|`）で接続された一連のコマンド
#[derive(Debug, Clone, PartialEq)]
pub struct Pipeline {
    pub commands: Vec<SimpleCommand>,
}

/// パースエラー
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// トークン列をパイプラインにパースする。
///
/// `shell_words::split()` で分割済みのトークンを受け取り、
/// `|` でパイプライン分割し、各セグメントからリダイレクト演算子を抽出する。
pub fn parse_pipeline(tokens: Vec<String>) -> Result<Pipeline, ParseError> {
    if tokens.is_empty() {
        return Err(ParseError("empty command".to_string()));
    }

    // トークン列を "|" で分割
    let segments = split_by_pipe(&tokens)?;

    let mut commands = Vec::new();
    for segment in segments {
        let cmd = parse_simple_command(segment)?;
        commands.push(cmd);
    }

    Ok(Pipeline { commands })
}

/// トークン列を `|` で分割し、各セグメントを返す。
fn split_by_pipe<'a>(tokens: &'a [String]) -> Result<Vec<&'a [String]>, ParseError> {
    let mut segments: Vec<&[String]> = Vec::new();
    let mut start = 0;

    for (i, token) in tokens.iter().enumerate() {
        if token == "|" {
            if i == start {
                return Err(ParseError(
                    "syntax error: unexpected token '|'".to_string(),
                ));
            }
            segments.push(&tokens[start..i]);
            start = i + 1;
        }
    }

    // 最後のセグメント
    if start >= tokens.len() {
        return Err(ParseError(
            "syntax error: unexpected end of command after '|'".to_string(),
        ));
    }
    segments.push(&tokens[start..]);

    Ok(segments)
}

/// トークンのスライスからリダイレクトを抽出し、SimpleCommand を構築する。
fn parse_simple_command(tokens: &[String]) -> Result<SimpleCommand, ParseError> {
    let mut args: Vec<String> = Vec::new();
    let mut redirects: Vec<Redirect> = Vec::new();
    let mut iter = tokens.iter().peekable();

    while let Some(token) = iter.next() {
        match token.as_str() {
            ">>" => {
                let target = iter.next().ok_or_else(|| {
                    ParseError("syntax error: expected filename after '>>'".to_string())
                })?;
                redirects.push(Redirect::StdoutAppend(target.clone()));
            }
            ">" => {
                let target = iter.next().ok_or_else(|| {
                    ParseError("syntax error: expected filename after '>'".to_string())
                })?;
                redirects.push(Redirect::StdoutOverwrite(target.clone()));
            }
            "<" => {
                let target = iter.next().ok_or_else(|| {
                    ParseError("syntax error: expected filename after '<'".to_string())
                })?;
                redirects.push(Redirect::StdinFrom(target.clone()));
            }
            _ => {
                args.push(token.clone());
            }
        }
    }

    if args.is_empty() {
        return Err(ParseError("syntax error: missing command".to_string()));
    }

    let cmd = args.remove(0);
    Ok(SimpleCommand {
        cmd,
        args,
        redirects,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_pipeline: 基本 ──

    #[test]
    fn single_command_no_args() {
        let tokens = vec!["ls".into()];
        let pipeline = parse_pipeline(tokens).unwrap();
        assert_eq!(pipeline.commands.len(), 1);
        assert_eq!(pipeline.commands[0].cmd, "ls");
        assert!(pipeline.commands[0].args.is_empty());
        assert!(pipeline.commands[0].redirects.is_empty());
    }

    #[test]
    fn single_command_with_args() {
        let tokens = vec!["git".into(), "log".into(), "--oneline".into()];
        let pipeline = parse_pipeline(tokens).unwrap();
        assert_eq!(pipeline.commands.len(), 1);
        assert_eq!(pipeline.commands[0].cmd, "git");
        assert_eq!(pipeline.commands[0].args, vec!["log", "--oneline"]);
    }

    // ── parse_pipeline: パイプ ──

    #[test]
    fn two_commands_piped() {
        let tokens = vec![
            "git".into(),
            "log".into(),
            "|".into(),
            "head".into(),
        ];
        let pipeline = parse_pipeline(tokens).unwrap();
        assert_eq!(pipeline.commands.len(), 2);
        assert_eq!(pipeline.commands[0].cmd, "git");
        assert_eq!(pipeline.commands[0].args, vec!["log"]);
        assert_eq!(pipeline.commands[1].cmd, "head");
        assert!(pipeline.commands[1].args.is_empty());
    }

    #[test]
    fn three_commands_piped() {
        let tokens = vec![
            "cat".into(),
            "file.txt".into(),
            "|".into(),
            "grep".into(),
            "error".into(),
            "|".into(),
            "wc".into(),
            "-l".into(),
        ];
        let pipeline = parse_pipeline(tokens).unwrap();
        assert_eq!(pipeline.commands.len(), 3);
        assert_eq!(pipeline.commands[0].cmd, "cat");
        assert_eq!(pipeline.commands[1].cmd, "grep");
        assert_eq!(pipeline.commands[1].args, vec!["error"]);
        assert_eq!(pipeline.commands[2].cmd, "wc");
        assert_eq!(pipeline.commands[2].args, vec!["-l"]);
    }

    // ── parse_pipeline: リダイレクト ──

    #[test]
    fn stdout_overwrite_redirect() {
        let tokens = vec![
            "echo".into(),
            "hello".into(),
            ">".into(),
            "out.txt".into(),
        ];
        let pipeline = parse_pipeline(tokens).unwrap();
        assert_eq!(pipeline.commands.len(), 1);
        assert_eq!(pipeline.commands[0].cmd, "echo");
        assert_eq!(pipeline.commands[0].args, vec!["hello"]);
        assert_eq!(
            pipeline.commands[0].redirects,
            vec![Redirect::StdoutOverwrite("out.txt".into())]
        );
    }

    #[test]
    fn stdout_append_redirect() {
        let tokens = vec![
            "echo".into(),
            "hello".into(),
            ">>".into(),
            "out.txt".into(),
        ];
        let pipeline = parse_pipeline(tokens).unwrap();
        assert_eq!(
            pipeline.commands[0].redirects,
            vec![Redirect::StdoutAppend("out.txt".into())]
        );
    }

    #[test]
    fn stdin_redirect() {
        let tokens = vec![
            "cat".into(),
            "<".into(),
            "input.txt".into(),
        ];
        let pipeline = parse_pipeline(tokens).unwrap();
        assert_eq!(
            pipeline.commands[0].redirects,
            vec![Redirect::StdinFrom("input.txt".into())]
        );
    }

    #[test]
    fn pipe_with_redirect() {
        // echo hello | cat > out.txt
        let tokens = vec![
            "echo".into(),
            "hello".into(),
            "|".into(),
            "cat".into(),
            ">".into(),
            "out.txt".into(),
        ];
        let pipeline = parse_pipeline(tokens).unwrap();
        assert_eq!(pipeline.commands.len(), 2);
        assert!(pipeline.commands[0].redirects.is_empty());
        assert_eq!(
            pipeline.commands[1].redirects,
            vec![Redirect::StdoutOverwrite("out.txt".into())]
        );
    }

    // ── parse_pipeline: エラーケース ──

    #[test]
    fn empty_tokens_returns_error() {
        let result = parse_pipeline(vec![]);
        assert!(result.is_err());
    }

    #[test]
    fn leading_pipe_returns_error() {
        let tokens = vec!["|".into(), "head".into()];
        let result = parse_pipeline(tokens);
        assert!(result.is_err());
    }

    #[test]
    fn trailing_pipe_returns_error() {
        let tokens = vec!["ls".into(), "|".into()];
        let result = parse_pipeline(tokens);
        assert!(result.is_err());
    }

    #[test]
    fn redirect_without_target_returns_error() {
        let tokens = vec!["echo".into(), "hello".into(), ">".into()];
        let result = parse_pipeline(tokens);
        assert!(result.is_err());
    }

    #[test]
    fn append_redirect_without_target_returns_error() {
        let tokens = vec!["echo".into(), "hello".into(), ">>".into()];
        let result = parse_pipeline(tokens);
        assert!(result.is_err());
    }
}
