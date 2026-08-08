use super::*;

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

#[test]
fn command_list_with_pipe() {
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
    assert_eq!(list.first.commands.len(), 2);
    assert_eq!(list.rest.len(), 1);
    assert_eq!(list.rest[0].0, Connector::And);
    assert_eq!(list.rest[0].1.commands[0].cmd, "echo");
}

#[test]
fn command_list_single_command() {
    let tokens = vec!["ls".into(), "-la".into()];
    let list = parse_command_list(tokens).unwrap();
    assert_eq!(list.first.commands[0].cmd, "ls");
    assert!(list.rest.is_empty());
}

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
    let tokens = vec!["echo".into(), "hello".into(), ";".into()];
    let result = parse_command_list(tokens);
    assert!(result.is_err());
}

#[test]
fn command_list_empty_returns_error() {
    let result = parse_command_list(vec![]);
    assert!(result.is_err());
}

#[test]
fn extract_ai_filter_single_command_pipe_ai() {
    let tokens = vec![
        "cat".into(),
        "file.txt".into(),
        "|".into(),
        "ai".into(),
        "要約して".into(),
    ];
    let pipeline = parse_pipeline(tokens).unwrap();
    let (prompt, remaining) = pipeline.extract_ai_filter().unwrap();
    assert_eq!(prompt, "要約して");
    assert_eq!(remaining.commands.len(), 1);
    assert_eq!(remaining.commands[0].cmd, "cat");
    assert_eq!(remaining.commands[0].args, vec!["file.txt"]);
}

#[test]
fn extract_ai_filter_multi_pipe() {
    let tokens = vec![
        "cat".into(),
        "log".into(),
        "|".into(),
        "grep".into(),
        "error".into(),
        "|".into(),
        "ai".into(),
        "JSON形式で出力して".into(),
    ];
    let pipeline = parse_pipeline(tokens).unwrap();
    let (prompt, remaining) = pipeline.extract_ai_filter().unwrap();
    assert_eq!(prompt, "JSON形式で出力して");
    assert_eq!(remaining.commands.len(), 2);
    assert_eq!(remaining.commands[0].cmd, "cat");
    assert_eq!(remaining.commands[1].cmd, "grep");
}

#[test]
fn extract_ai_filter_multi_word_prompt() {
    let tokens = vec![
        "echo".into(),
        "hello".into(),
        "|".into(),
        "ai".into(),
        "translate".into(),
        "to".into(),
        "Japanese".into(),
    ];
    let pipeline = parse_pipeline(tokens).unwrap();
    let (prompt, _) = pipeline.extract_ai_filter().unwrap();
    assert_eq!(prompt, "translate to Japanese");
}

#[test]
fn extract_ai_filter_no_ai_command() {
    let tokens = vec![
        "cat".into(),
        "file.txt".into(),
        "|".into(),
        "grep".into(),
        "error".into(),
    ];
    let pipeline = parse_pipeline(tokens).unwrap();
    assert!(pipeline.extract_ai_filter().is_none());
}

#[test]
fn extract_ai_filter_ai_alone() {
    let tokens = vec!["ai".into(), "prompt".into()];
    let pipeline = parse_pipeline(tokens).unwrap();
    assert!(pipeline.extract_ai_filter().is_none());
}

#[test]
fn extract_ai_filter_empty_prompt() {
    let tokens = vec!["echo".into(), "hello".into(), "|".into(), "ai".into()];
    let pipeline = parse_pipeline(tokens).unwrap();
    assert!(pipeline.extract_ai_filter().is_none());
}

#[test]
fn extract_ai_filter_ai_not_last() {
    let tokens = vec!["ai".into(), "prompt".into(), "|".into(), "cat".into()];
    let pipeline = parse_pipeline(tokens).unwrap();
    assert!(pipeline.extract_ai_filter().is_none());
}
