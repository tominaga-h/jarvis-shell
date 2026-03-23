//! 入力ハンドリング
//!
//! ユーザー入力を受け取り、ビルトイン/コマンド/自然言語を分類し、
//! 適切な実行パスに振り分ける。

use std::sync::atomic::Ordering;
use std::time::Instant;

use tracing::{debug, info, warn};

use reedline::HistoryItem;

use std::path::PathBuf;

use crate::cli::prompt::starship::CMD_DURATION_NONE;

use crate::engine::builtins::{alias, cd, dirstack, source, unalias, which_type};
use crate::engine::classifier::{is_ai_goodbye_response, InputType};
use crate::engine::dispatch::{AiPipeMode, AiPipeRequest};
use crate::engine::expand;
use crate::engine::{execute, try_builtin, try_execute_ai_pipe, CommandResult, LoopAction};

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
        // 履歴にはユーザーが実際に入力した文字列を記録するため、展開前の入力を保持する
        let original_line = line.clone();
        let line = if let Some(expanded) = expand::expand_alias(&line, &self.aliases) {
            debug!(original = %line, expanded = %expanded, "Alias expanded");
            expanded
        } else {
            line
        };

        debug!(input = %line, "User input received");

        // 0.5. alias / unalias / source は Shell 状態を操作するためインターセプト
        if let Some(result) = self.try_shell_builtins(&line) {
            return self.handle_builtin(&original_line, &line, result);
        }

        // 1. ビルトインコマンドをチェック（cd, cwd, exit, export 等は AI を介さず直接実行）
        if let Some(result) = try_builtin(&line) {
            return self.handle_builtin(&original_line, &line, result);
        }

        // 2. アルゴリズムで入力を分類（AI を呼ばず瞬時に判定）
        let input_type = self.classifier.classify(&line);
        debug!(input = %line, classification = ?input_type, "Input classified");

        // 3. 入力タイプに応じて実行（実行時間を計測）
        let start = Instant::now();
        let (result, from_tool_call, should_update_exit_code, executed_command) = match input_type {
            InputType::Goodbye => {
                // Goodbye → シェル終了（farewell メッセージは run() 側で表示）
                info!("Goodbye input detected, exiting shell");
                return false;
            }
            InputType::Command => {
                // AI パイプ / リダイレクト検出:
                // `cmd | ai "prompt"` または `cmd > ai "prompt"` をインターセプト
                if let Some(ai_pipe_req) = try_execute_ai_pipe(&line) {
                    debug!(input = %line, mode = ?ai_pipe_req.mode, "AI pipe/redirect detected");
                    let result = self.handle_ai_pipe(ai_pipe_req).await;
                    (result, false, true, None)
                } else {
                    debug!(input = %line, "Executing as command (no AI)");
                    (execute(&line), false, true, None)
                }
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
        let elapsed_ms = start.elapsed().as_millis() as u64;
        self.cmd_duration_ms.store(elapsed_ms, Ordering::Relaxed);

        // 4. プロンプト表示用に終了コードを更新
        // AI の NaturalLanguage 応答時はコマンド未実行のためスキップ
        if should_update_exit_code {
            self.last_exit_code
                .store(result.exit_code, Ordering::Relaxed);
        }

        // 4.5. Alternate Screen 復元後、カーソルを旧プロンプト領域の下に押し下げる。
        // ターミナルが復元した旧画面（旧プロンプト+コマンド）はそのまま残し、
        // 追加の改行で reedline の新プロンプトが旧内容を上書きしないようにする。
        if result.used_alt_screen {
            println!();
        }

        println!(); // 実行結果の後に空行を追加

        // 5. 履歴を記録（エイリアス展開前の入力を記録する）
        self.record_history(&original_line, &result);

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
    /// - `original_line`: エイリアス展開前のユーザー入力（履歴記録用）
    /// - `line`: エイリアス展開後のコマンド（ログ用）
    ///
    /// 戻り値: `true` = REPL ループ続行、`false` = シェル終了
    fn handle_builtin(&mut self, original_line: &str, line: &str, result: CommandResult) -> bool {
        debug!(
            command = %line,
            exit_code = result.exit_code,
            action = ?result.action,
            "Builtin command executed"
        );

        // プロンプト表示用に終了コードを更新
        self.last_exit_code
            .store(result.exit_code, Ordering::Relaxed);
        self.cmd_duration_ms
            .store(CMD_DURATION_NONE, Ordering::Relaxed);
        println!(); // 実行結果の後に空行を追加

        match result.action {
            LoopAction::Continue => {
                self.record_history(original_line, &result);
                true
            }
            LoopAction::Exit => {
                info!("Exit command received");
                false
            }
        }
    }

    /// Shell 状態を操作するビルトインをインターセプトする。
    ///
    /// 対象: alias / unalias / source / cd / pushd / popd / dirs
    ///
    /// 先頭ワードが対象コマンドであり、かつパイプ・リダイレクト等を
    /// 含まない単純なコマンドの場合に `Some(CommandResult)` を返す。
    /// それ以外は `None` を返し、通常の実行パスに委ねる。
    fn try_shell_builtins(&mut self, input: &str) -> Option<CommandResult> {
        let first_word = input.split_whitespace().next().unwrap_or("");
        if !matches!(
            first_word,
            "alias" | "unalias" | "source" | "cd" | "pushd" | "popd" | "dirs" | "which" | "type"
        ) {
            return None;
        }

        let tokens = match shell_words::split(input) {
            Ok(t) => t,
            Err(e) => {
                let msg = format!("jarvish: parse error: {e}\n");
                eprint!("{msg}");
                return Some(CommandResult::error(msg, 1));
            }
        };

        if tokens.is_empty() {
            return Some(CommandResult::success(String::new()));
        }

        // パイプ・リダイレクト・接続演算子を含む場合は通常パスに委ねる
        if tokens
            .iter()
            .any(|t| matches!(t.as_str(), "|" | ">" | ">>" | "<" | "&&" | "||" | ";"))
        {
            return None;
        }

        let expanded: Vec<String> = tokens
            .into_iter()
            .map(|t| expand::expand_token(&t))
            .collect();
        let args: Vec<&str> = expanded[1..].iter().map(|s| s.as_str()).collect();

        let result = match first_word {
            "alias" => alias::execute_with_aliases(&args, &mut self.aliases),
            "unalias" => unalias::execute_with_aliases(&args, &mut self.aliases),
            "source" => {
                let path_str = match source::parse(&args) {
                    Ok(p) => p,
                    Err(cmd_result) => return Some(cmd_result),
                };
                let path = PathBuf::from(&path_str);
                self.reload_config(&path)
            }
            "cd" => cd::execute(&args, &mut self.dir_stack),
            "pushd" => dirstack::execute_pushd(&args, &mut self.dir_stack),
            "popd" => dirstack::execute_popd(&args, &mut self.dir_stack),
            "dirs" => dirstack::execute_dirs(&args, &mut self.dir_stack),
            "which" => which_type::execute_which(&args, &self.aliases),
            "type" => which_type::execute_type(&args, &self.aliases),
            _ => unreachable!(),
        };

        debug!(
            command = %first_word,
            exit_code = result.exit_code,
            "shell builtin executed"
        );

        Some(result)
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

    /// AI パイプ / リダイレクトリクエストを処理する。
    ///
    /// 手前パイプラインの stdout キャプチャ結果を AI に渡し、結果を返す。
    /// - `Filter` モード: テキストフィルタとして動作（`| ai`）
    /// - `Redirect` モード: Jarvis が対話的に応答（`> ai`）
    async fn handle_ai_pipe(&self, req: AiPipeRequest) -> CommandResult {
        let ai = match self.ai_client {
            Some(ref ai) => ai,
            None => {
                let msg = "jarvish: AI pipe requires OPENAI_API_KEY to be set.\n";
                eprint!("{msg}");
                return CommandResult::error(msg.to_string(), 1);
            }
        };

        if req.stdin_text.is_empty() {
            debug!(
                exit_code = req.exit_code,
                mode = ?req.mode,
                "AI pipe: source pipeline produced no stdout, skipping AI"
            );
            let msg = "jarvish: AI pipe: no input received from the source pipeline.\n";
            eprint!("{msg}");
            return CommandResult::error(msg.to_string(), req.exit_code.max(1));
        }

        debug!(
            prompt = %req.prompt,
            input_chars = req.stdin_text.chars().count(),
            source_exit_code = req.exit_code,
            mode = ?req.mode,
            "Processing AI pipe"
        );

        let result = match req.mode {
            AiPipeMode::Filter => ai.process_ai_pipe(&req.stdin_text, &req.prompt).await,
            AiPipeMode::Redirect => ai.process_ai_redirect(&req.stdin_text, &req.prompt).await,
        };

        match result {
            Ok(output) => CommandResult::success(output),
            Err(e) => {
                let msg = format!("jarvish: {e}\n");
                eprint!("{msg}");
                CommandResult::error(msg, 1)
            }
        }
    }
}
