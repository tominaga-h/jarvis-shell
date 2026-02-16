//! 入力ハンドリング
//!
//! ユーザー入力を受け取り、ビルトイン/コマンド/自然言語を分類し、
//! 適切な実行パスに振り分ける。

use std::sync::atomic::Ordering;

use tracing::{debug, info, warn};

use reedline::HistoryItem;

use crate::engine::classifier::{is_ai_goodbye_response, InputType};
use crate::engine::expand;
use crate::engine::{execute, try_builtin, CommandResult, LoopAction};

use super::Shell;

impl Shell {
    /// ユーザー入力を処理する。
    ///
    /// 戻り値: `true` = REPL ループ続行、`false` = シェル終了
    pub(super) async fn handle_input(&mut self, line: &str) -> bool {
        info!("\n\n==== USER INPUT RECEIVED, START PROCESS ====");

        let line = line.trim().to_string();

        if line.is_empty() {
            return true;
        }

        // 0. エイリアス展開（先頭トークンがエイリアスに一致すれば置換）
        let line = if let Some(expanded) = expand::expand_alias(&line, &self.aliases) {
            debug!(original = %line, expanded = %expanded, "Alias expanded");
            expanded
        } else {
            line
        };

        debug!(input = %line, "User input received");

        // 1. ビルトインコマンドをチェック（cd, cwd, exit, export 等は AI を介さず直接実行）
        //    PATH 変更を検出するため、ビルトイン実行前の PATH を保存する
        let path_before = std::env::var("PATH").ok();
        if let Some(result) = try_builtin(&line) {
            return self.handle_builtin(&line, result, path_before);
        }

        // 2. アルゴリズムで入力を分類（AI を呼ばず瞬時に判定）
        let input_type = self.classifier.classify(&line);
        debug!(input = %line, classification = ?input_type, "Input classified");

        // 3. 入力タイプに応じて実行
        let (result, from_tool_call, should_update_exit_code, executed_command) = match input_type {
            InputType::Goodbye => {
                // Goodbye → シェル終了（farewell メッセージは run() 側で表示）
                info!("Goodbye input detected, exiting shell");
                return false;
            }
            InputType::Command => {
                // コマンド → AI を介さず直接実行
                debug!(input = %line, "Executing as command (no AI)");
                (execute(&line), false, true, None)
            }
            InputType::NaturalLanguage => {
                // 自然言語 → AI にルーティング
                let ai_result = self.route_to_ai(&line).await;
                (
                    ai_result.result,
                    ai_result.from_tool_call,
                    ai_result.should_update_exit_code,
                    ai_result.executed_command,
                )
            }
        };

        // 4. プロンプト表示用に終了コードを更新
        // AI の NaturalLanguage 応答時はコマンド未実行のためスキップ
        if should_update_exit_code {
            self.last_exit_code
                .store(result.exit_code, Ordering::Relaxed);
        }
        println!(); // 実行結果の後に空行を追加

        // 5. 履歴を記録
        self.record_history(&line, &result);

        // 6. AI が実行したコマンドを reedline 履歴に追加（矢印キーで辿れるようにする）
        if let Some(ref cmd) = executed_command {
            if let Err(e) = self
                .editor
                .history_mut()
                .save(HistoryItem::from_command_line(cmd))
            {
                warn!("Failed to save AI-executed command to reedline history: {e}");
            }
        }

        // 7. エラー調査フロー
        if result.exit_code != 0 {
            self.investigate_error(&line, &result, from_tool_call).await;
        }

        // 8. AI Goodbye 検出: AI の応答が farewell を含む場合はシェル終了
        //    AI が既に farewell を言っているためバナーは非表示にする
        if !from_tool_call && is_ai_goodbye_response(&result.stdout) {
            info!("AI goodbye response detected, exiting shell");
            self.farewell_shown = true;
            return false;
        }

        info!("\n==== FINISHED PROCESS ====\n\n");
        true
    }

    /// ビルトインコマンドの結果を処理する。
    ///
    /// `path_before` はビルトイン実行前の PATH 環境変数の値。
    /// ビルトイン実行後に PATH が変更されていれば、分類器の PATH キャッシュをリロードする。
    ///
    /// 戻り値: `true` = REPL ループ続行、`false` = シェル終了
    fn handle_builtin(
        &mut self,
        line: &str,
        result: CommandResult,
        path_before: Option<String>,
    ) -> bool {
        debug!(
            command = %line,
            exit_code = result.exit_code,
            action = ?result.action,
            "Builtin command executed"
        );

        // PATH 変更検出: export PATH=... や unset PATH でキャッシュをリロード
        let path_after = std::env::var("PATH").ok();
        if path_before != path_after {
            info!("PATH changed, reloading classifier cache");
            self.classifier.reload_path_cache();
        }

        // プロンプト表示用に終了コードを更新
        self.last_exit_code
            .store(result.exit_code, Ordering::Relaxed);
        println!(); // 実行結果の後に空行を追加

        match result.action {
            LoopAction::Continue => {
                self.record_history(line, &result);
                true
            }
            LoopAction::Exit => {
                info!("Exit command received");
                false
            }
        }
    }

    /// 履歴を BlackBox に記録する。
    fn record_history(&self, line: &str, result: &CommandResult) {
        if result.action == LoopAction::Continue {
            if let Some(ref bb) = self.black_box {
                if let Err(e) = bb.record(line, result) {
                    warn!("Failed to record history: {e}");
                    eprintln!("jarvish: warning: failed to record history: {e}");
                }
            }
        }
    }
}
