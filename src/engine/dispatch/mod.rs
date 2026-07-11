//! 入力ディスパッチ
//!
//! ユーザー入力をビルトインコマンドまたは外部コマンドとして実行する。
//! トークン分割、シェル展開、パイプラインパースを経て、
//! 適切な実行パスに振り分ける。

mod ai_pipe;

pub use ai_pipe::{try_execute_ai_pipe, AiPipeMode, AiPipeRequest};

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

    let first_word = input.split_whitespace().next().unwrap_or("");
    if !builtins::is_builtin(first_word) {
        debug!(
            command = %first_word,
            is_builtin = false,
            "try_builtin check"
        );
        return None;
    }

    let tokens = match expand::split_quoted(input) {
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

    if tokens
        .iter()
        .any(|t| matches!(t.value.as_str(), "|" | ">" | ">>" | "<" | "&&" | "||" | ";"))
    {
        debug!(
            command = %first_word,
            "try_builtin: contains pipe/redirect/connector, deferring to execute()"
        );
        return None;
    }

    let mut expanded: Vec<String> = Vec::with_capacity(tokens.len());
    for tok in tokens {
        if tok.quoted && !tok.has_subst {
            // クォート済みトークン（コマンド置換を含まない）はグロブ/ブレース展開の対象外。
            // チルダ/env も bash 互換でシングル/ダブルクォート内では展開されないため
            // ここでは値をそのまま使用する。
            expanded.push(tok.value);
            continue;
        }
        let expanded_result = if tok.quoted && tok.has_subst {
            // クォート内の置換: 置換のみ行い glob/brace は適用しない（bash 準拠）。
            expand::expand_token_subst_only(&tok.value, tok.subst_quoting)
        } else if tok.has_subst {
            expand::expand_token_globs_with_quoting(&tok.value, tok.subst_quoting)
        } else {
            expand::expand_token_globs(&tok.value)
        };
        match expanded_result {
            Ok(parts) => expanded.extend(parts),
            Err(expand::ExpandError::NoMatches(p)) => {
                let msg = format!("jarvish: no matches found: {p}\n");
                eprint!("{msg}");
                return Some(CommandResult::error(msg, 1));
            }
            Err(expand::ExpandError::Substitution(m)) => {
                let msg = format!("jarvish: {m}\n");
                eprint!("{msg}");
                return Some(CommandResult::error(msg, 1));
            }
        }
    }
    if expanded.is_empty() {
        return Some(CommandResult::success(String::new()));
    }
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

    let tokens = match expand::split_quoted(input) {
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

    let mut expanded: Vec<String> = Vec::with_capacity(tokens.len());
    for tok in tokens {
        if matches!(
            tok.value.as_str(),
            "|" | ">" | ">>" | "<" | "&&" | "||" | ";"
        ) {
            expanded.push(tok.value);
            continue;
        }
        if tok.quoted && !tok.has_subst {
            expanded.push(tok.value);
            continue;
        }
        let expanded_result = if tok.quoted && tok.has_subst {
            // クォート内の置換: 置換のみ行い glob/brace は適用しない（bash 準拠）。
            expand::expand_token_subst_only(&tok.value, tok.subst_quoting)
        } else if tok.has_subst {
            expand::expand_token_globs_with_quoting(&tok.value, tok.subst_quoting)
        } else {
            expand::expand_token_globs(&tok.value)
        };
        match expanded_result {
            Ok(parts) => expanded.extend(parts),
            Err(expand::ExpandError::NoMatches(p)) => {
                let msg = format!("jarvish: no matches found: {p}\n");
                eprint!("{msg}");
                return CommandResult::error(msg, 1);
            }
            Err(expand::ExpandError::Substitution(m)) => {
                let msg = format!("jarvish: {m}\n");
                eprint!("{msg}");
                return CommandResult::error(msg, 1);
            }
        }
    }

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

    if command_list.rest.is_empty() {
        let pipeline = &command_list.first;
        return execute_pipeline(pipeline);
    }

    run_command_list_with_builtins(&command_list)
}

/// 単一パイプラインを実行する（ビルトイン最適化パス付き）。
fn execute_pipeline(pipeline: &parser::Pipeline) -> CommandResult {
    if pipeline.commands.len() == 1 && pipeline.commands[0].redirects.is_empty() {
        let simple = &pipeline.commands[0];
        let args: Vec<&str> = simple.args.iter().map(|s| s.as_str()).collect();
        if let Some(result) = builtins::dispatch_builtin(&simple.cmd, &args) {
            debug!(command = %simple.cmd, "Dispatched as builtin command");
            return result;
        }
    }

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

    exec::run_pipeline(pipeline)
}

/// コマンドリストをビルトイン対応で実行する。
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
    fn try_builtin_apostrophe_returns_none() {
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

    #[test]
    fn try_builtin_with_pipe_returns_none() {
        assert!(try_builtin("history | less").is_none());
        assert!(try_builtin("export | grep PATH").is_none());
        assert!(try_builtin("cwd | cat").is_none());
    }

    #[test]
    fn try_builtin_with_redirect_returns_none() {
        assert!(try_builtin("history > /tmp/hist.txt").is_none());
        assert!(try_builtin("export >> /tmp/env.txt").is_none());
    }

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

    #[test]
    #[serial]
    fn execute_builtin_pipe_to_cat() {
        let _guard = CwdGuard::new();
        let expected = env::current_dir().unwrap();
        let result = execute("cwd | cat");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), expected.display().to_string());
    }

    #[test]
    #[serial]
    fn execute_builtin_pipe_to_grep() {
        let result = execute("export | grep PATH");
        assert_eq!(result.exit_code, 0);
        assert!(
            result.stdout.contains("PATH"),
            "expected stdout to contain PATH, got: {}",
            result.stdout
        );
    }

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

    #[test]
    fn execute_semi_always_runs() {
        let result = execute("false ; echo always");
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("always"));
    }

    #[test]
    fn execute_and_then_or() {
        let result = execute("false && echo skip || echo rescue");
        assert_eq!(result.exit_code, 0);
        assert!(!result.stdout.contains("skip"));
        assert!(result.stdout.contains("rescue"));
    }

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

    #[test]
    #[serial]
    fn execute_builtin_and_command() {
        let _guard = CwdGuard::new();
        let result = execute("cd /tmp && echo done");
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("done"));
    }

    // ── グロブ / ブレース展開 E2E テスト (#126) ──

    #[test]
    fn execute_brace_expansion_via_echo() {
        let result = execute("echo {a,b,c}");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "a b c");
    }

    #[test]
    fn execute_brace_numeric_range_via_echo() {
        let result = execute("echo {1..3}");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "1 2 3");
    }

    #[test]
    #[serial]
    fn execute_glob_star_via_ls() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        std::fs::write(dir.path().join("c.md"), "").unwrap();

        let result = execute("ls *.txt");
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("a.txt"));
        assert!(result.stdout.contains("b.txt"));
        assert!(!result.stdout.contains("c.md"));
    }

    #[test]
    #[serial]
    fn execute_glob_brace_combined() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        std::fs::write(dir.path().join("a.md"), "").unwrap();
        std::fs::write(dir.path().join("b.log"), "").unwrap();

        let result = execute("ls *.{txt,md}");
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("a.txt"));
        assert!(result.stdout.contains("a.md"));
        assert!(!result.stdout.contains("b.log"));
    }

    #[test]
    #[serial]
    fn execute_quoted_glob_not_expanded() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();
        let result = execute("echo '*'");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "*");
    }

    #[test]
    #[serial]
    fn execute_quoted_brace_not_expanded() {
        let _guard = CwdGuard::new();
        let result = execute("echo \"{a,b}\"");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "{a,b}");
    }

    #[test]
    #[serial]
    fn execute_glob_no_match_errors_and_blocks_chain() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();

        let result = execute("ls *.nonexistent_xyz && echo OK");
        assert_ne!(result.exit_code, 0);
        assert!(!result.stdout.contains("OK"));
        // stderr もしくは stdout に no matches found が含まれる
        let combined = format!("{}{}", result.stdout, result.stderr);
        assert!(
            combined.contains("no matches found") || combined.contains("nonexistent_xyz"),
            "expected error output to mention no matches, got stdout={:?} stderr={:?}",
            result.stdout,
            result.stderr
        );
    }

    #[test]
    #[serial]
    fn execute_glob_with_pipe() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();

        let result = execute("ls *.txt | head -n 1");
        assert_eq!(result.exit_code, 0);
        let lines: Vec<&str> = result.stdout.lines().collect();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    #[serial]
    fn execute_glob_with_redirect() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = CwdGuard::new();
        env::set_current_dir(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let out_path = dir.path().join("out.dat");

        let cmd = format!("cat *.txt > {}", out_path.display());
        let result = execute(&cmd);
        assert_eq!(result.exit_code, 0);
        let contents = std::fs::read_to_string(&out_path).unwrap();
        assert_eq!(contents, "hello");
    }

    #[test]
    fn try_builtin_brace_expansion_for_echo_returns_none() {
        // echo はビルトインではない（externalにフォールスルー）
        assert!(try_builtin("echo {a,b}").is_none());
    }

    // ── コマンド置換 E2E テスト (#266) ──

    #[test]
    fn cmdsubst_basic() {
        let result = execute("echo $(echo hello)");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "hello");
    }

    #[test]
    fn cmdsubst_word_split_multiple_args() {
        let result = execute("echo $(echo a b c)");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "a b c");
    }

    #[test]
    fn cmdsubst_backtick() {
        let result = execute("echo `echo hi`");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "hi");
    }

    #[test]
    fn cmdsubst_double_quoted_preserves_whitespace() {
        let result = execute("echo \"$(printf 'a   b')\"");
        assert_eq!(result.exit_code, 0);
        // ダブルクォート内なので内部の連続空白が保持される
        assert_eq!(result.stdout.trim_end_matches('\n'), "a   b");
    }

    #[test]
    fn cmdsubst_unquoted_collapses_whitespace() {
        let result = execute("echo $(printf 'a   b')");
        assert_eq!(result.exit_code, 0);
        // クォート外なので単語分割され、echo が 1 空白で連結する
        assert_eq!(result.stdout.trim(), "a b");
    }

    #[test]
    fn cmdsubst_single_quoted_is_literal() {
        let result = execute("echo '$(echo X)'");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "$(echo X)");
    }

    #[test]
    fn cmdsubst_embedded_in_word() {
        let result = execute("echo prefix-$(echo mid)-suffix");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "prefix-mid-suffix");
    }

    #[test]
    fn cmdsubst_nested() {
        let result = execute("echo $(echo $(echo deep))");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "deep");
    }

    #[test]
    fn cmdsubst_nonexistent_command_no_panic_nonzero_exit() {
        let result = execute("echo $(this_command_does_not_exist_zzz)");
        assert_ne!(result.exit_code, 0);
    }

    #[test]
    fn cmdsubst_unterminated_is_parse_error() {
        let result = execute("echo $(echo unclosed");
        assert_ne!(result.exit_code, 0);
        let combined = format!("{}{}", result.stdout, result.stderr);
        assert!(
            combined.contains("parse error")
                || combined.contains("unterminated")
                || combined.contains("substitution"),
            "expected parse/unterminated error, got stdout={:?} stderr={:?}",
            result.stdout,
            result.stderr
        );
    }

    #[test]
    fn cmdsubst_with_pipe() {
        let result = execute("echo $(echo foo) | cat");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "foo");
    }

    #[test]
    fn cmdsubst_trailing_newlines_stripped() {
        // ダブルクォートで囲み glob（`[...]`）を抑止しつつ、末尾改行の全除去を検証する。
        // unquoted 版だと `[x]` がグロブパターンとして解釈され no-match になるため。
        let result = execute("echo \"[$(printf 'x\\n\\n')]\"");
        assert_eq!(result.exit_code, 0);
        // 末尾改行が全除去され、`[x]` になる
        assert_eq!(result.stdout.trim(), "[x]");
    }

    #[test]
    fn cmdsubst_operator_inside_span_is_a_pipeline() {
        // span 内の `|` はサブシェルのパイプとして実行される
        let result = execute("echo $(echo foo | cat)");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "foo");
    }

    // ── complete ビルトイン: dispatch_builtin スタブ経由での data-loss 修正
    // (#89 A1) ──
    //
    // `try_shell_builtins`（実レジストリ）を経由しないルート（`;` を含む
    // コマンドリスト、パイプライン内の非先頭コマンド、ai_pipe 経由）では
    // `complete` の register/list/erase は「使い捨てレジストリへ静かに
    // 成功する」のではなく、明確なエラーとして観測可能でなければならない。

    #[test]
    fn complete_register_in_semicolon_list_surfaces_error_not_silent_noop() {
        // `complete -c x -a y; ls` 形式: `;` を含むため
        // try_shell_builtins が None を返し、execute() → dispatch_builtin
        // スタブ経由になる。修正前はここで register が「成功」し
        // データが消えるだけだった（観測不能な data loss）。
        // 修正後は complete 呼び出し自体がエラー終了として観測できる。
        let result = execute("complete -c x -a y ; echo after");
        // 最終的な終了コードは最後のコマンド（echo）の結果で上書きされるが、
        // complete 単体の失敗は stderr に必ず現れる（無音の成功ではない）。
        assert!(
            result.stderr.contains("standalone command"),
            "expected complete's stub error to surface in stderr, got stdout={:?} stderr={:?}",
            result.stdout,
            result.stderr
        );
        // `; echo after` 自体は独立して実行されるため、続行はする。
        assert!(result.stdout.contains("after"));
    }

    #[test]
    fn complete_erase_in_and_list_surfaces_error() {
        let result = execute("complete -e -c x && echo unreachable");
        assert!(result.stderr.contains("standalone command"));
        assert_ne!(result.exit_code, 0);
        // -c 付き -e はスタブ経路でエラー終了するため && の後続は実行されない。
        assert!(!result.stdout.contains("unreachable"));
    }

    #[test]
    fn complete_list_as_pipeline_head_surfaces_error_not_silent_empty_success() {
        // パイプライン内（複数コマンド）の先頭ビルトインとして complete が
        // 呼ばれるケース。dispatch_builtin スタブ経由になるため、
        // 一覧表示相当の「空文字列で成功」ではなくエラーになる。
        let result = execute("complete | cat");
        assert_ne!(
            result.exit_code, 0,
            "complete at pipeline head must not silently succeed with empty output"
        );
    }

    #[test]
    fn complete_help_still_works_through_command_list() {
        // --help は standalone 経路を経由しない状況でも動き続ける必要がある
        // （help.rs の `dispatch_builtin(cmd, ["--help"])` 委譲との整合）。
        let result = execute("complete --help ; echo after");
        assert!(result.stdout.contains("complete"));
        assert!(result.stdout.contains("after"));
    }
}
