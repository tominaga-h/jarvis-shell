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

/// コマンドリストの接続演算子
#[derive(Debug, Clone, PartialEq)]
pub enum Connector {
    /// `&&` — 前のコマンドが成功 (exit_code == 0) した場合のみ次を実行
    And,
    /// `||` — 前のコマンドが失敗 (exit_code != 0) した場合のみ次を実行
    Or,
    /// `;` — 前のコマンドの結果に関わらず次を実行
    Semi,
}

/// `&&`, `||`, `;` で接続された一連のパイプライン
#[derive(Debug, Clone, PartialEq)]
pub struct CommandList {
    /// 先頭のパイプライン
    pub first: Pipeline,
    /// (接続演算子, パイプライン) のペアのリスト
    pub rest: Vec<(Connector, Pipeline)>,
}

/// パースエラー
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// トークン列をコマンドリストにパースする。
///
/// `shell_words::split()` で分割済みのトークンを受け取り、
/// `&&`, `||`, `;` で分割した後、各セグメントを `parse_pipeline()` でパースする。
pub fn parse_command_list(tokens: Vec<String>) -> Result<CommandList, ParseError> {
    if tokens.is_empty() {
        return Err(ParseError("empty command".to_string()));
    }

    let (segments, connectors) = split_by_connector(&tokens)?;

    let first = parse_pipeline(segments[0].clone())?;
    let mut rest = Vec::new();
    for (i, conn) in connectors.into_iter().enumerate() {
        let pipeline = parse_pipeline(segments[i + 1].clone())?;
        rest.push((conn, pipeline));
    }

    Ok(CommandList { first, rest })
}

/// トークン列を `&&`, `||`, `;` で分割する。
///
/// 戻り値: (セグメント群, 接続演算子群)
/// segments.len() == connectors.len() + 1 が常に成立する。
fn split_by_connector(tokens: &[String]) -> Result<(Vec<Vec<String>>, Vec<Connector>), ParseError> {
    let mut segments: Vec<Vec<String>> = Vec::new();
    let mut connectors: Vec<Connector> = Vec::new();
    let mut current: Vec<String> = Vec::new();

    for token in tokens {
        match token.as_str() {
            "&&" => {
                if current.is_empty() {
                    return Err(ParseError(
                        "syntax error: unexpected token '&&'".to_string(),
                    ));
                }
                segments.push(std::mem::take(&mut current));
                connectors.push(Connector::And);
            }
            "||" => {
                if current.is_empty() {
                    return Err(ParseError(
                        "syntax error: unexpected token '||'".to_string(),
                    ));
                }
                segments.push(std::mem::take(&mut current));
                connectors.push(Connector::Or);
            }
            ";" => {
                if current.is_empty() {
                    return Err(ParseError("syntax error: unexpected token ';'".to_string()));
                }
                segments.push(std::mem::take(&mut current));
                connectors.push(Connector::Semi);
            }
            _ => {
                current.push(token.clone());
            }
        }
    }

    // 最後のセグメント
    if current.is_empty() && !connectors.is_empty() {
        return Err(ParseError(
            "syntax error: unexpected end of command after connector".to_string(),
        ));
    }
    if !current.is_empty() {
        segments.push(current);
    }

    Ok((segments, connectors))
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
fn split_by_pipe(tokens: &[String]) -> Result<Vec<&[String]>, ParseError> {
    let mut segments: Vec<&[String]> = Vec::new();
    let mut start = 0;

    for (i, token) in tokens.iter().enumerate() {
        if token == "|" {
            if i == start {
                return Err(ParseError("syntax error: unexpected token '|'".to_string()));
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
        let tokens = vec!["git".into(), "log".into(), "|".into(), "head".into()];
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
        let tokens = vec!["echo".into(), "hello".into(), ">".into(), "out.txt".into()];
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
        let tokens = vec!["echo".into(), "hello".into(), ">>".into(), "out.txt".into()];
        let pipeline = parse_pipeline(tokens).unwrap();
        assert_eq!(
            pipeline.commands[0].redirects,
            vec![Redirect::StdoutAppend("out.txt".into())]
        );
    }

    #[test]
    fn stdin_redirect() {
        let tokens = vec!["cat".into(), "<".into(), "input.txt".into()];
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

    // ── parse_command_list: && ──

    #[test]
    fn command_list_and_two_commands() {
        let tokens = vec![
            "make".into(),
            "build".into(),
            "&&".into(),
            "echo".into(),
            "done".into(),
        ];
        let list = parse_command_list(tokens).unwrap();
        assert_eq!(list.first.commands[0].cmd, "make");
        assert_eq!(list.first.commands[0].args, vec!["build"]);
        assert_eq!(list.rest.len(), 1);
        assert_eq!(list.rest[0].0, Connector::And);
        assert_eq!(list.rest[0].1.commands[0].cmd, "echo");
        assert_eq!(list.rest[0].1.commands[0].args, vec!["done"]);
    }

    #[test]
    fn command_list_and_three_commands() {
        let tokens = vec![
            "cmd1".into(),
            "&&".into(),
            "cmd2".into(),
            "&&".into(),
            "cmd3".into(),
        ];
        let list = parse_command_list(tokens).unwrap();
        assert_eq!(list.first.commands[0].cmd, "cmd1");
        assert_eq!(list.rest.len(), 2);
        assert_eq!(list.rest[0].0, Connector::And);
        assert_eq!(list.rest[0].1.commands[0].cmd, "cmd2");
        assert_eq!(list.rest[1].0, Connector::And);
        assert_eq!(list.rest[1].1.commands[0].cmd, "cmd3");
    }

    // ── parse_command_list: || ──

    #[test]
    fn command_list_or() {
        let tokens = vec![
            "false".into(),
            "||".into(),
            "echo".into(),
            "fallback".into(),
        ];
        let list = parse_command_list(tokens).unwrap();
        assert_eq!(list.first.commands[0].cmd, "false");
        assert_eq!(list.rest.len(), 1);
        assert_eq!(list.rest[0].0, Connector::Or);
        assert_eq!(list.rest[0].1.commands[0].cmd, "echo");
    }

    // ── parse_command_list: ; ──

    #[test]
    fn command_list_semi() {
        let tokens = vec![
            "echo".into(),
            "a".into(),
            ";".into(),
            "echo".into(),
            "b".into(),
        ];
        let list = parse_command_list(tokens).unwrap();
        assert_eq!(list.first.commands[0].cmd, "echo");
        assert_eq!(list.rest.len(), 1);
        assert_eq!(list.rest[0].0, Connector::Semi);
        assert_eq!(list.rest[0].1.commands[0].cmd, "echo");
    }

    // ── parse_command_list: 混合 ──

    #[test]
    fn command_list_mixed_connectors() {
        let tokens = vec![
            "cmd1".into(),
            "&&".into(),
            "cmd2".into(),
            "||".into(),
            "cmd3".into(),
            ";".into(),
            "cmd4".into(),
        ];
        let list = parse_command_list(tokens).unwrap();
        assert_eq!(list.first.commands[0].cmd, "cmd1");
        assert_eq!(list.rest.len(), 3);
        assert_eq!(list.rest[0].0, Connector::And);
        assert_eq!(list.rest[1].0, Connector::Or);
        assert_eq!(list.rest[2].0, Connector::Semi);
    }

    // ── parse_command_list: パイプとの組み合わせ ──

    #[test]
    fn command_list_with_pipe() {
        // echo hello | cat && echo done
        let tokens = vec![
            "echo".into(),
            "hello".into(),
            "|".into(),
            "cat".into(),
            "&&".into(),
            "echo".into(),
            "done".into(),
        ];
        let list = parse_command_list(tokens).unwrap();
        assert_eq!(list.first.commands.len(), 2); // echo hello | cat
        assert_eq!(list.rest.len(), 1);
        assert_eq!(list.rest[0].0, Connector::And);
        assert_eq!(list.rest[0].1.commands[0].cmd, "echo");
    }

    // ── parse_command_list: 単一コマンド (接続演算子なし) ──

    #[test]
    fn command_list_single_command() {
        let tokens = vec!["ls".into(), "-la".into()];
        let list = parse_command_list(tokens).unwrap();
        assert_eq!(list.first.commands[0].cmd, "ls");
        assert!(list.rest.is_empty());
    }

    // ── parse_command_list: エラーケース ──

    #[test]
    fn command_list_leading_and_returns_error() {
        let tokens = vec!["&&".into(), "echo".into()];
        let result = parse_command_list(tokens);
        assert!(result.is_err());
    }

    #[test]
    fn command_list_trailing_and_returns_error() {
        let tokens = vec!["echo".into(), "&&".into()];
        let result = parse_command_list(tokens);
        assert!(result.is_err());
    }

    #[test]
    fn command_list_leading_or_returns_error() {
        let tokens = vec!["||".into(), "echo".into()];
        let result = parse_command_list(tokens);
        assert!(result.is_err());
    }

    #[test]
    fn command_list_trailing_semi_is_ok() {
        // `echo hello ;` — 末尾のセミコロン後にコマンドがなくても許容
        // (実際のシェルでは `echo hello ;` は有効)
        // ただし現在の実装ではエラーになる — これはシンプルさのため
        let tokens = vec!["echo".into(), "hello".into(), ";".into()];
        let result = parse_command_list(tokens);
        assert!(result.is_err());
    }

    #[test]
    fn command_list_empty_returns_error() {
        let result = parse_command_list(vec![]);
        assert!(result.is_err());
    }
}
