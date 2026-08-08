use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use tracing::{debug, info};

use crate::cli::prompt::EXIT_CODE_NONE;
use crate::engine::classifier::InputClassifier;
use crate::engine::expand;
use crate::engine::{execute, try_builtin, CommandResult, LoopAction};
use crate::shell::Shell;

use super::file_reading::read_rc_file_guarded;
use super::options::RcOutcome;
use super::parsing::parse_rc_lines;
use super::resolution::{is_toml_source_path, plan_rc_bootstrap, RcBootstrapPlan};
use super::template::ensure_default_rc;

/// `source` によるネストしたスクリプト実行の最大深さ。
pub(in crate::shell) const MAX_SOURCE_DEPTH: usize = 8;

impl Shell {
    /// rc スクリプトファイルを実行する（`run()` / `run_command()` 用の
    /// `async fn` ラッパー）。
    pub(in crate::shell) async fn run_rc_script(
        &mut self,
        path: &Path,
        display_name: &str,
        depth: usize,
    ) -> RcOutcome {
        self.run_rc_script_sync(path, display_name, depth)
    }

    /// rc スクリプトファイルを実行する同期コア。
    pub(in crate::shell) fn run_rc_script_sync(
        &mut self,
        path: &Path,
        display_name: &str,
        depth: usize,
    ) -> RcOutcome {
        debug!(path = %path.display(), display_name, depth, "Running rc script");

        if depth > MAX_SOURCE_DEPTH {
            eprintln!("jarvish: {display_name}: source nesting too deep");
            return RcOutcome::Continue { had_failure: true };
        }

        let content = match read_rc_file_guarded(path) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                eprintln!("jarvish: {display_name}: no such file or directory: {e}");
                return RcOutcome::Continue { had_failure: true };
            }
            Err(e) => {
                eprintln!("jarvish: {display_name}: {e}");
                return RcOutcome::Continue { had_failure: true };
            }
        };

        let previous_depth = self.source_depth;
        self.source_depth = depth;

        let lines = parse_rc_lines(&content);
        let mut had_failure = false;
        let mut outcome = RcOutcome::Continue { had_failure: false };
        for rc_line in lines {
            match self.run_rc_line(&rc_line.text) {
                RcLineOutcome::Ran(result) => {
                    self.last_exit_code
                        .store(result.exit_code, Ordering::Relaxed);
                    let is_exit_requested =
                        matches!(result.action, LoopAction::Exit | LoopAction::Restart);
                    if result.exit_code != 0 && !is_exit_requested {
                        had_failure = true;
                        eprintln!(
                            "jarvish: {display_name}:{}: command exited with status {}",
                            rc_line.lineno, result.exit_code
                        );
                    }
                    match result.action {
                        LoopAction::Exit => {
                            outcome = RcOutcome::ExitRequested;
                            break;
                        }
                        LoopAction::Restart => {
                            self.restart_requested.store(true, Ordering::Relaxed);
                            outcome = RcOutcome::ExitRequested;
                            break;
                        }
                        LoopAction::Continue => {}
                    }
                }
                RcLineOutcome::Exit => {
                    outcome = RcOutcome::ExitRequested;
                    break;
                }
            }
        }

        self.source_depth = previous_depth;
        match outcome {
            RcOutcome::ExitRequested => RcOutcome::ExitRequested,
            RcOutcome::Continue { .. } => RcOutcome::Continue { had_failure },
        }
    }

    /// `self.rc_options`（`--rcfile` / `--no-rc`）を解決して rc スクリプトを
    /// 実行する、`run()` / `run_command()` 共通のエントリポイント。
    pub(in crate::shell) async fn run_configured_rc(&mut self) -> RcOutcome {
        match plan_rc_bootstrap(self.rc_options.resolve()) {
            RcBootstrapPlan::Skip => RcOutcome::Continue { had_failure: false },
            RcBootstrapPlan::BootstrapAndRun { path, display_name } => {
                ensure_default_rc(&path);
                info!(path = %path.display(), "Executing rc.jsh");
                self.run_rc_script(&path, display_name, 0).await
            }
            RcBootstrapPlan::RunExplicit { path, display_name } => {
                info!(path = %path.display(), "Executing explicit --rcfile");
                self.run_rc_script(&path, &display_name, 0).await
            }
            RcBootstrapPlan::ExplicitMissing { path } => {
                eprintln!("jarvish: rcfile not found: {}", path.display());
                RcOutcome::Continue { had_failure: false }
            }
        }
    }

    /// `source <path>` ビルトインの本体。
    pub(in crate::shell) fn dispatch_source(&mut self, path_str: &str) -> CommandResult {
        if let Ok(metadata) = std::fs::metadata(path_str) {
            if metadata.is_dir() {
                let msg = format!("jarvish: source: {path_str} is a directory\n");
                eprint!("{msg}");
                return CommandResult::error(msg, 1);
            }
        }

        if is_toml_source_path(path_str) {
            let path = PathBuf::from(path_str);
            return self.reload_config(&path);
        }

        let next_depth = self.source_depth + 1;
        if next_depth > MAX_SOURCE_DEPTH {
            let msg = format!("jarvish: {path_str}: source nesting too deep\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }

        let path = PathBuf::from(path_str);
        if !path.exists() {
            let msg = format!("jarvish: source: no such file: {path_str}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }

        match self.run_rc_script_sync(&path, path_str, next_depth) {
            RcOutcome::ExitRequested => {
                let exit_code = self.last_exit_code.load(Ordering::Relaxed);
                let exit_code = if exit_code == EXIT_CODE_NONE {
                    0
                } else {
                    exit_code
                };
                CommandResult::exit_with(exit_code)
            }
            RcOutcome::Continue { had_failure } => {
                if had_failure {
                    CommandResult::error(String::new(), 1)
                } else {
                    CommandResult::success(String::new())
                }
            }
        }
    }

    /// 1行を分類器を経由せずに実行する決定コア。
    fn run_rc_line(&mut self, line: &str) -> RcLineOutcome {
        let expanded = match self.aliases.read() {
            Ok(guard) => expand::expand_aliases_in_line(line, &guard),
            Err(_) => None,
        };
        let line = expanded.as_deref().unwrap_or(line);

        if InputClassifier::is_goodbye_pattern(line) {
            return RcLineOutcome::Exit;
        }
        if let Some(result) = self.try_shell_builtins(line) {
            return RcLineOutcome::Ran(result);
        }
        if let Some(result) = try_builtin(line) {
            return RcLineOutcome::Ran(result);
        }
        RcLineOutcome::Ran(execute(line))
    }
}

/// `run_rc_line` の内部結果。goodbye パターンは `try_builtin`/`execute` を
/// 経由しないため `CommandResult` を持たない特別扱いにしている。
enum RcLineOutcome {
    Ran(CommandResult),
    Exit,
}
