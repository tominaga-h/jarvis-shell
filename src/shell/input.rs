//! 入力ハンドリング
//!
//! ユーザー入力を受け取り、ビルトイン/コマンド/自然言語を分類し、
//! 適切な実行パスに振り分ける。

use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use tracing::{debug, info, warn};

use reedline::HistoryItem;

use crate::cli::completer::registry::CompletionRegistry;
use crate::cli::prompt::starship::CMD_DURATION_NONE;

use crate::cli::jarvis::{jarvis_ask_typo_correction, TypoAction};
use crate::engine::builtins::{alias, cd, cdj, complete, dirstack, source, unalias, which_type};
use crate::engine::classifier::{is_ai_goodbye_response, InputType};
use crate::engine::dispatch::{AiPipeMode, AiPipeRequest};
use crate::engine::expand;
use crate::engine::typo;
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
        // read ガードは短命スコープで取得し、await を跨いで保持しない
        let expanded = {
            match self.aliases.read() {
                Ok(guard) => expand::expand_aliases_in_line(&line, &guard),
                Err(_) => None,
            }
        };
        let line = if let Some(expanded) = expanded {
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

        // 2.5. タイポ補正チェック（NaturalLanguage 判定かつコマンド名らしい入力に限定）
        let (line, input_type) = if input_type == InputType::NaturalLanguage {
            match check_typo_correction(&line) {
                TypoCorrectionOutcome::UseCommand(corrected) => {
                    let new_type = self.classifier.classify(&corrected);
                    (corrected, new_type)
                }
                TypoCorrectionOutcome::Abort => return true,
                TypoCorrectionOutcome::Proceed => (line, InputType::NaturalLanguage),
            }
        } else {
            (line, input_type)
        };

        // 3. 入力タイプに応じて実行（実行時間を計測）
        //    `is_ai_response`: この出力が AI（Jarvis）の発話かどうか。
        //    goodbye 検出（ステップ8）は AI 応答に対してのみ行うべきで、
        //    人間が打った通常コマンドの stdout を farewell 判定に回してはならない。
        let start = Instant::now();
        let (result, from_tool_call, should_update_exit_code, executed_command, is_ai_response) =
            match input_type {
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
                        // AI パイプの出力は AI の発話なので goodbye 判定の対象
                        (result, false, true, None, true)
                    } else {
                        debug!(input = %line, "Executing as command (no AI)");
                        // 通常コマンドの stdout は人間の打鍵結果。goodbye 判定に回さない。
                        (execute(&line), false, true, None, false)
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
                        true,
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
        //    非対話単体実行（`jarvish -c "<command>"`）では追加しない。これは
        //    reedline の `read_line()` 経由の自動保存（経路A）とは別の、
        //    `handle_input` 内から直接 `BlackBoxHistory::save()` を呼ぶ経路で
        //    あり、`read_line()` を通らない `-c` 実行でも走ってしまう。両者は
        //    同じ `command_history` テーブルに書き込むため、`record_history`
        //    （経路B）だけをガードしても、AI が自然言語入力からツールコールで
        //    コマンドを実行した場合はここで履歴に混入する。`record_history`
        //    と同じ `interactive` 条件で塞ぐ（詳細は `Shell::interactive` の
        //    フィールド定義 `src/shell/mod.rs` 参照）。
        if self.interactive {
            if let Some(ref cmd) = executed_command {
                if let Err(e) = self
                    .editor
                    .history_mut()
                    .save(HistoryItem::from_command_line(cmd))
                {
                    warn!("Failed to save AI-executed command to reedline history: {e}");
                }
            }
        }

        // 7. エラー調査フロー
        if result.exit_code != 0 {
            self.investigate_error(&line, &result, from_tool_call).await;
        }

        // 8. AI Goodbye 検出: AI の応答が farewell を含む場合はシェル終了
        //    AI が既に farewell を言っているためバナーは非表示にする
        if should_exit_on_goodbye(is_ai_response, from_tool_call, &result.stdout) {
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
            LoopAction::Restart => {
                info!("Restart command received");
                self.restart_requested.store(true, Ordering::Relaxed);
                false
            }
        }
    }

    /// Shell 状態を操作するビルトインをインターセプトする。
    ///
    /// 対象: alias / unalias / source / cd / pushd / popd / dirs / complete
    ///
    /// 先頭ワードが対象コマンドであり、かつパイプ・リダイレクト等を
    /// 含まない単純なコマンドの場合に `Some(CommandResult)` を返す。
    /// それ以外は `None` を返し、通常の実行パスに委ねる。
    pub(super) fn try_shell_builtins(&mut self, input: &str) -> Option<CommandResult> {
        let first_word = input.split_whitespace().next().unwrap_or("");
        if !matches!(
            first_word,
            "alias"
                | "unalias"
                | "source"
                | "cd"
                | "cdj"
                | "pushd"
                | "popd"
                | "dirs"
                | "which"
                | "type"
                | "complete"
        ) {
            return None;
        }

        let tokens = match expand::split_quoted(input) {
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
            .any(|t| matches!(t.value.as_str(), "|" | ">" | ">>" | "<" | "&&" | "||" | ";"))
        {
            return None;
        }

        let mut expanded: Vec<String> = Vec::with_capacity(tokens.len());
        for tok in tokens {
            if tok.quoted && !tok.has_subst {
                expanded.push(tok.value);
                continue;
            }
            let expanded_result = if tok.quoted && tok.has_subst {
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
        let args: Vec<&str> = expanded[1..].iter().map(|s| s.as_str()).collect();

        let result = match first_word {
            "alias" => {
                let Ok(mut guard) = self.aliases.write() else {
                    let msg = "jarvish: alias: internal error: lock poisoned\n".to_string();
                    eprint!("{msg}");
                    return Some(CommandResult::error(msg, 1));
                };
                alias::execute_with_aliases(&args, &mut guard)
            }
            "unalias" => {
                let Ok(mut guard) = self.aliases.write() else {
                    let msg = "jarvish: unalias: internal error: lock poisoned\n".to_string();
                    eprint!("{msg}");
                    return Some(CommandResult::error(msg, 1));
                };
                unalias::execute_with_aliases(&args, &mut guard)
            }
            "source" => {
                let path_str = match source::parse(&args) {
                    Ok(p) => p,
                    Err(cmd_result) => return Some(cmd_result),
                };
                self.dispatch_source(&path_str)
            }
            "cd" => cd::execute(&args, &mut self.dir_stack),
            "cdj" => cdj::execute(&args, &mut self.dir_stack),
            "pushd" => dirstack::execute_pushd(&args, &mut self.dir_stack),
            "popd" => dirstack::execute_popd(&args, &mut self.dir_stack),
            "dirs" => dirstack::execute_dirs(&args, &mut self.dir_stack),
            "which" => {
                let Ok(guard) = self.aliases.read() else {
                    let msg = "jarvish: which: internal error: lock poisoned\n".to_string();
                    eprint!("{msg}");
                    return Some(CommandResult::error(msg, 1));
                };
                which_type::execute_which(&args, &guard)
            }
            "type" => {
                let Ok(guard) = self.aliases.read() else {
                    let msg = "jarvish: type: internal error: lock poisoned\n".to_string();
                    eprint!("{msg}");
                    return Some(CommandResult::error(msg, 1));
                };
                which_type::execute_type(&args, &guard)
            }
            "complete" => run_complete_builtin(&self.complete_registry, &args),
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
    ///
    /// 非対話単体実行（`jarvish -c "<command>"`）では記録しない。`nvim`
    /// などの外部ツールがファイル glob 展開のために `jarvish -c
    /// "vimglob() {...}"` を呼ぶと、そのツール由来の一時コマンドが履歴
    /// （上下矢印キーの履歴補完）に混入してしまうため。bash/zsh でも
    /// 非対話実行は履歴対象外であり、それと同じ挙動。`self.interactive`
    /// の詳細は `Shell` 構造体のフィールド定義（`src/shell/mod.rs`）参照。
    fn record_history(&self, line: &str, result: &CommandResult) {
        if !self.interactive {
            return;
        }
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

// ── complete ビルトイン (try_shell_builtins の "complete" 分岐) ──

/// `try_shell_builtins` の `"complete"` 分岐本体。
///
/// `Shell` が保持する実共有 `Arc<RwLock<CompletionRegistry>>`
/// （`complete_registry`）を書き込みロックし、`complete::execute_with_registry`
/// に委譲する。他の分岐（`alias`/`unalias`/`which`/`type`）と同じ
/// poisoned-lock ガードパターンを踏襲する: 書き込みロック取得に失敗した
/// 場合は共有レジストリには一切触れず、exit code 1 のエラーを返す。
///
/// `try_shell_builtins` はメソッド（`&mut Shell` 経由）のためフルの
/// `Shell` を構築しないとテストできないが、この関数は `Arc<RwLock<_>>` と
/// 引数配列だけで直接呼び出せるため、`Shell` 構築コストなしに「実共有
/// registry を実際に mutate する」経路と「poisoned lock から復旧できる」
/// 経路の両方をユニットテストできる（#89 C1）。
fn run_complete_builtin(
    registry: &Arc<RwLock<CompletionRegistry>>,
    args: &[&str],
) -> CommandResult {
    let Ok(mut guard) = registry.write() else {
        let msg = "jarvish: complete: internal error: lock poisoned\n".to_string();
        eprint!("{msg}");
        return CommandResult::error(msg, 1);
    };
    complete::execute_with_registry(args, &mut guard)
}

// ── Goodbye 判定 ──

/// 実行結果を受けてシェルを goodbye 終了すべきかを判定する。
///
/// goodbye 検出は **AI（Jarvis）の発話** に対してのみ行う。人間が打った
/// 通常コマンド（`InputType::Command`）の stdout は、内容がどうあれ
/// farewell 判定に回してはならない。これを怠ると、例えば `git status` の
/// 出力に "farewell" 等を含むパスが現れただけでシェルが終了してしまう
/// （葬祭システム palmo-sousai での実バグ）。
///
/// - `is_ai_response`: 出力が AI の発話か（NaturalLanguage 経路または AI パイプ）
/// - `from_tool_call`: AI がツール呼び出しでコマンドを実行したか
///   （その場合 stdout はコマンド出力であり farewell 文ではないため除外）
/// - `stdout`: 判定対象テキスト
fn should_exit_on_goodbye(is_ai_response: bool, from_tool_call: bool, stdout: &str) -> bool {
    is_ai_response && !from_tool_call && is_ai_goodbye_response(stdout)
}

// ── タイポ補正 ──

/// タイポ補正チェックの結果
enum TypoCorrectionOutcome {
    /// 補正されたコマンドラインで実行する
    UseCommand(String),
    /// 補正せず通常の処理を続ける
    Proceed,
    /// 実行を中止してプロンプトに戻る
    Abort,
}

/// 入力ラインに対してタイポ補正を試みる。
///
/// 先頭トークンがコマンド名らしく、PATH 上に近似コマンドが存在する場合に
/// ユーザーへ確認を求め、応答に応じた `TypoCorrectionOutcome` を返す。
fn check_typo_correction(line: &str) -> TypoCorrectionOutcome {
    let first_token = line.split_whitespace().next().unwrap_or("");
    if !typo::is_command_like(first_token) {
        return TypoCorrectionOutcome::Proceed;
    }
    let Some(suggestion) = typo::find_correction(first_token) else {
        return TypoCorrectionOutcome::Proceed;
    };
    match jarvis_ask_typo_correction(first_token, &suggestion) {
        TypoAction::Accept => {
            // 先頭トークンを補正候補で置き換える（残りの引数はそのまま）
            let rest = &line[first_token.len()..];
            TypoCorrectionOutcome::UseCommand(format!("{suggestion}{rest}"))
        }
        TypoAction::Reject => TypoCorrectionOutcome::Abort,
        TypoAction::Abort => TypoCorrectionOutcome::Abort,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 末尾に farewell パターンを含む goodbye らしい AI 応答テキスト。
    const GOODBYE_TEXT: &str = "承知しました。\nさようなら、サー。";

    /// 回帰テスト: 通常コマンド（`InputType::Command`）の stdout は
    /// goodbye 判定の対象にしてはならない。
    ///
    /// 葬祭システム palmo-sousai で `git status` の出力に
    /// "...corporate-farewell-...WIP.md" というパスが含まれており、
    /// それを goodbye として誤検知してシェルが終了していた。
    #[test]
    fn command_output_with_farewell_does_not_exit() {
        let git_status = "Untracked files:\n\
            \t(use \"git add <file>...\" to include in what will be committed)\n\
            \tdocs/design-notes/2026-06-22/obituary-corporate-farewell-other-venue-WIP.md";
        // is_ai_response=false（通常コマンド）なので、内容に farewell があっても終了しない
        assert!(
            !should_exit_on_goodbye(false, false, git_status),
            "通常コマンドの出力で farewell を含んでもシェルを終了してはならない"
        );
    }

    /// AI 応答（NaturalLanguage / AI パイプ）が farewell を含む場合は終了する。
    #[test]
    fn ai_response_with_farewell_exits() {
        assert!(
            should_exit_on_goodbye(true, false, GOODBYE_TEXT),
            "AI の farewell 応答ではシェルを終了する"
        );
    }

    /// AI 応答であっても from_tool_call の場合は除外する。
    /// （stdout は AI がツールで実行したコマンドの出力であり farewell 文ではない）
    #[test]
    fn ai_tool_call_output_does_not_exit() {
        // ツール実行結果にたまたま farewell パスが含まれていても終了しない
        let tool_output = "ファイル一覧:\n./docs/farewell-template.md";
        assert!(
            !should_exit_on_goodbye(true, true, tool_output),
            "from_tool_call の出力は farewell 判定対象外"
        );
        // goodbye 文そのものでも、from_tool_call なら終了しない
        assert!(!should_exit_on_goodbye(true, true, GOODBYE_TEXT));
    }

    /// AI 応答で farewell を含まない通常応答では終了しない。
    #[test]
    fn ai_response_without_farewell_does_not_exit() {
        assert!(!should_exit_on_goodbye(
            true,
            false,
            "エラーの原因はこちらです。"
        ));
    }

    /// 通常コマンドが goodbye 文そのものを出力しても終了しない
    /// （例: `echo さようなら` や farewell を含むファイルの `cat`）。
    #[test]
    fn command_echoing_goodbye_text_does_not_exit() {
        assert!(
            !should_exit_on_goodbye(false, false, GOODBYE_TEXT),
            "コマンドが goodbye 文を出力してもシェルを終了してはならない"
        );
    }

    // ── run_complete_builtin (try_shell_builtins の "complete" 分岐, #89 C1) ──

    /// register 呼び出しが `Shell::complete_registry` と同じ実共有 Arc を
    /// 実際に mutate することを証明する（#89 C1）。
    #[test]
    fn run_complete_builtin_register_mutates_shared_registry() {
        let registry = Arc::new(RwLock::new(CompletionRegistry::new()));

        let result = run_complete_builtin(&registry, &["-c", "mycmd", "-s", "v", "-l", "verbose"]);
        assert_eq!(result.exit_code, 0);

        // 同じ Arc を通して、呼び出し元スコープから直接見える変更である
        // ことを確認する（使い捨てレジストリではなく共有状態であることの
        // 直接証拠）。
        let guard = registry.read().unwrap();
        let specs = guard.specs_for("mycmd");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].short, vec!["v"]);
        assert_eq!(specs[0].long, vec!["verbose"]);
    }

    /// erase 呼び出しも同じ共有 Arc を実際に mutate する。
    #[test]
    fn run_complete_builtin_erase_mutates_shared_registry() {
        let registry = Arc::new(RwLock::new(CompletionRegistry::new()));
        run_complete_builtin(&registry, &["-c", "mycmd", "-s", "v"]);
        assert_eq!(registry.read().unwrap().specs_for("mycmd").len(), 1);

        let result = run_complete_builtin(&registry, &["-e", "-c", "mycmd"]);
        assert_eq!(result.exit_code, 0);
        assert!(registry.read().unwrap().specs_for("mycmd").is_empty());
    }

    /// list（引数なし）も同じ共有 Arc を読み取り、register 済みの内容を
    /// 反映する。
    #[test]
    fn run_complete_builtin_list_reflects_prior_registrations_on_shared_registry() {
        let registry = Arc::new(RwLock::new(CompletionRegistry::new()));
        run_complete_builtin(&registry, &["-c", "mycmd", "-s", "v", "-d", "verbose"]);

        let result = run_complete_builtin(&registry, &[]);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "complete -c mycmd -s v -d verbose");
    }

    /// poisoned-lock ガード経路: 書き込みロック取得中に別スレッドが panic
    /// して Arc が poison した後でも、`run_complete_builtin` は panic
    /// せず exit code 1 のエラーを返す（他の分岐: alias/unalias/which/type
    /// と同じ復旧パターン）。かつ、poison 状態のレジストリには一切
    /// 触れていないことも確認する。
    #[test]
    fn run_complete_builtin_recovers_from_poisoned_lock() {
        let registry = Arc::new(RwLock::new(CompletionRegistry::new()));

        // 別スレッドで write ロックを保持したまま panic させ、Arc を poison する。
        let poison_registry = Arc::clone(&registry);
        let handle = std::thread::spawn(move || {
            let _guard = poison_registry.write().unwrap();
            panic!("deliberately poisoning the lock for run_complete_builtin_recovers_from_poisoned_lock");
        });
        assert!(
            handle.join().is_err(),
            "spawned thread should have panicked"
        );
        assert!(
            registry.is_poisoned(),
            "Arc<RwLock<_>> should be poisoned after the writer thread panicked while holding the lock"
        );

        // poisoned 状態でも panic せず、exit code 1 のエラーとして復旧する。
        let result = run_complete_builtin(&registry, &["-c", "mycmd", "-s", "v"]);
        assert_eq!(result.exit_code, 1);
        assert!(result.stderr.contains("lock poisoned"));
    }
}
