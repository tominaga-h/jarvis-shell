use clap::Parser;

use crate::engine::CommandResult;

/// restart: シェルプロセスを exec() で再起動する。
#[derive(Parser)]
#[command(name = "restart", about = "Restart the shell process")]
struct RestartArgs {}

/// restart: 現在のシェルプロセスを exec() で再起動する。
///
/// クリーンアップ（ターミナル復元、SQLite クローズ、セッション解放）後、
/// 同じバイナリで exec() によるプロセス置換を行う。
/// PID は維持され、ターミナルセッションも継続する。
pub(super) fn execute(args: &[&str]) -> CommandResult {
    let _parsed = match super::parse_args::<RestartArgs>("restart", args) {
        Ok(a) => a,
        Err(result) => return result,
    };

    println!("Restarting jarvish...");
    CommandResult::restart()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::LoopAction;

    #[test]
    fn restart_returns_restart_action() {
        let result = execute(&[]);
        assert_eq!(result.action, LoopAction::Restart);
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn restart_help_does_not_restart() {
        let result = execute(&["--help"]);
        assert_eq!(result.action, LoopAction::Continue);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("restart"));
    }

    #[test]
    fn restart_ignores_extra_args() {
        // 不明な引数はエラー
        let result = execute(&["--unknown"]);
        assert_eq!(result.action, LoopAction::Continue);
        assert_ne!(result.exit_code, 0);
    }

    #[test]
    fn restart_stdout_is_empty() {
        // restart コマンドの CommandResult は stdout を持たない
        let result = execute(&[]);
        assert!(result.stdout.is_empty());
    }

    #[test]
    fn restart_stderr_is_empty() {
        let result = execute(&[]);
        assert!(result.stderr.is_empty());
    }
}
