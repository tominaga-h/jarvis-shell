//! REPL ループの実行ロジックを切り出したサブモジュール。
//!
//! `Shell::run`（対話 REPL）と `Shell::run_command`（`-c` 単体実行）を集約する。
//! 振る舞いは元の `src/shell/mod.rs` から一切変更していない。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use reedline::Signal;
use tracing::{info, warn};

use crate::cli::prompt::EXIT_CODE_NONE;
use crate::engine::LoopAction;

use super::rc::{self, RcOutcome};
use super::Shell;

impl Shell {
    /// `-c` オプションで渡されたコマンド文字列を非対話的に実行する。
    ///
    /// REPL ループには入らず、文字列を行ごとに `handle_input()` で処理して終了する。
    /// ウェルカムバナー・Farewell メッセージは表示しない。
    ///
    /// `--rcfile` が明示的に指定されている場合のみ、`-c` のコマンドを実行する
    /// 前に rc スクリプトを読み込む（デフォルトパス・`--no-rc` では
    /// `-c` 単体では rc は一切読み込まれない、既存の Phase 4.1 の挙動を維持）。
    /// rc スクリプト側で `exit`/goodbye が要求された場合は `-c` のコマンドを
    /// 実行せず、rc の終了コードをそのまま返す。
    ///
    /// 戻り値: 最後に実行したコマンドの終了コード。
    pub async fn run_command(&mut self, command: &str) -> i32 {
        if self.rc_options.rcfile.is_some()
            && rc::RcOutcome::ExitRequested == self.run_configured_rc().await
        {
            if let Some(ref bb) = self.black_box {
                bb.release_session();
            }
            let code = self.last_exit_code.load(Ordering::Relaxed);
            return if code == EXIT_CODE_NONE { 0 } else { code };
        }

        for line in command.lines() {
            if !self.handle_input(line).await {
                break;
            }
        }

        if let Some(ref bb) = self.black_box {
            bb.release_session();
        }

        let code = self.last_exit_code.load(Ordering::Relaxed);
        if code == EXIT_CODE_NONE {
            0
        } else {
            code
        }
    }

    /// REPL ループを実行する。
    ///
    /// ユーザー入力を受け取り、ビルトイン/コマンド/自然言語を処理する。
    /// Ctrl-D、exit コマンド、または goodbye 入力で終了する。
    /// SIGUSR1 受信時は再起動を行う。
    ///
    /// 戻り値: `(終了コード, LoopAction)` のタプル。
    /// - `LoopAction::Exit`: 通常終了
    /// - `LoopAction::Restart`: exec() による再起動が必要
    pub async fn run(&mut self) -> (i32, LoopAction) {
        let mut offline = Vec::new();
        if !self.logging_operational {
            offline.push("Logging offline");
        }
        if !self.history_available {
            offline.push("Command History offline");
        }
        if self.black_box.is_none() {
            offline.push("Black Box offline");
        }
        if self.ai_client.is_none() {
            offline.push("AI module offline");
        }
        let ai_info = self
            .ai_client
            .as_ref()
            .map(|ai| (ai.provider.as_str(), ai.model.as_str()));
        crate::cli::banner::print_welcome(&offline, ai_info);

        // バックグラウンドでバージョンチェックを実行（24時間キャッシュ付き）
        let update_check = tokio::spawn(crate::cli::update_check::check_for_update_notification());

        let mut repl_error = false;
        let mut action = LoopAction::Exit;

        // SIGUSR1 ハンドラの登録（AtomicBool フラグを共有）
        Self::register_sigusr1_handler(Arc::clone(&self.restart_requested));

        // 最初のプロンプト表示前にバージョンチェック結果を表示（最大1秒待機）
        if let Ok(Ok(Some(notification))) =
            tokio::time::timeout(std::time::Duration::from_secs(1), update_check).await
        {
            println!("{notification}");
            println!();
        }

        // rc.jsh の実行（[startup].commands より前、対話モード限定）。
        // `--rcfile` / `--no-rc` に応じてデフォルトパス／明示パス／スキップを
        // 解決する（Phase 4.2, `RcOptions::resolve`）。デフォルトパスのみ
        // 初回起動時にコメントのみのテンプレートを自動生成する。
        if RcOutcome::ExitRequested == self.run_configured_rc().await {
            info!("rc.jsh triggered shell exit");
            if let Some(ref bb) = self.black_box {
                bb.release_session();
            }
            let exit_code = self.last_exit_code.load(Ordering::Relaxed);
            return (
                if exit_code == EXIT_CODE_NONE {
                    0
                } else {
                    exit_code
                },
                if self.restart_requested.load(Ordering::Relaxed) {
                    LoopAction::Restart
                } else {
                    LoopAction::Exit
                },
            );
        }
        self.prompt.refresh_git_status();

        // 起動時コマンドの実行（config.toml [startup] commands）
        if !self.startup_commands.is_empty() {
            info!(
                count = self.startup_commands.len(),
                "Executing startup commands"
            );
            let commands = self.startup_commands.clone();
            for cmd in &commands {
                info!(command = %cmd, "Running startup command");
                if !self.handle_input(cmd).await {
                    // exit 等でシェル終了が要求された場合
                    info!("Startup command triggered shell exit");
                    if let Some(ref bb) = self.black_box {
                        bb.release_session();
                    }
                    let exit_code = self.last_exit_code.load(Ordering::Relaxed);
                    return (
                        if exit_code == EXIT_CODE_NONE {
                            0
                        } else {
                            exit_code
                        },
                        LoopAction::Exit,
                    );
                }
                self.prompt.refresh_git_status();
            }
        }

        loop {
            // 別プロセスの update コマンドによるフラグファイルを検出し、通知を表示
            if let Some(notification) = crate::engine::builtins::update::check_update_flag() {
                println!("  {notification}");
                println!();
            }

            // SIGUSR1 による再起動リクエストがフラグに残っている場合（コマンド実行中に受信した場合）
            if self.restart_requested.load(Ordering::Relaxed) {
                info!("Deferred restart triggered (SIGUSR1 received during command execution)");
                println!("Restarting jarvish (deferred SIGUSR1)...");
                action = LoopAction::Restart;
                break;
            }

            let signal = tokio::task::block_in_place(|| self.editor.read_line(&self.prompt));

            // read_line の完了後にシグナルフラグをチェック
            if self.restart_requested.load(Ordering::Relaxed) {
                info!("SIGUSR1 received during read_line: restarting shell");
                println!("\nRestarting jarvish (SIGUSR1)...");
                action = LoopAction::Restart;
                break;
            }

            match signal {
                Ok(Signal::Success(line)) => {
                    let result = self.handle_input(&line).await;
                    if !result {
                        // handle_input が false を返した場合、restart か exit かを判別
                        // restart ビルトインが呼ばれた場合は last action を確認
                        if self.restart_requested.load(Ordering::Relaxed) {
                            action = LoopAction::Restart;
                        }
                        break;
                    }
                    self.prompt.refresh_git_status();
                }
                Ok(Signal::CtrlC) => {
                    info!("\n!!!! Ctrl-C received: do it nothing !!!!!\n");
                    // なにもしない
                    println!(); // 改行して次のプロンプトを見やすくする
                }
                Ok(Signal::CtrlD) => {
                    // EOF → シェル終了
                    info!("\n!!!! Ctrl-D received: exiting shell !!!!!\n");
                    break;
                }
                Err(e) => {
                    warn!(error = %e, "REPL error, exiting");
                    eprintln!("jarvish: error: {e}");
                    repl_error = true;
                    break;
                }
            }
        }

        // Farewell メッセージ表示（再起動時と AI goodbye 表示済みの場合はスキップ）
        if action != LoopAction::Restart && !self.farewell_shown {
            crate::cli::banner::print_goodbye();
        }

        // セッション終了: session_id を NULL に解放し、次回起動時に履歴を辿れるようにする
        if let Some(ref bb) = self.black_box {
            bb.release_session();
        }

        // 終了コードを決定
        let exit_code = if repl_error {
            1
        } else {
            let code = self.last_exit_code.load(Ordering::Relaxed);
            if code == EXIT_CODE_NONE {
                0
            } else {
                code
            }
        };

        (exit_code, action)
    }
}
