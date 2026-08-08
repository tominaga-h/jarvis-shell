//! 外部コマンドとパイプラインの実行。

use tracing::debug;

use crate::engine::{builtins, exec, parser, CommandResult, LoopAction};

/// 単一パイプラインを実行する（ビルトイン最適化パス付き）。
pub(super) fn execute_pipeline(pipeline: &parser::Pipeline) -> CommandResult {
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
pub(super) fn run_command_list_with_builtins(list: &parser::CommandList) -> CommandResult {
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
