//! rc.jsh — シェル起動時に実行されるスクリプトファイル（Phase 4）
//!
//! `~/.config/jarvish/rc.jsh` は、対話起動のたびに `[startup].commands`
//! （config.toml）より前に読み込まれるプレーンテキストのコマンドスクリプト。
//! `complete` ビルトイン等、セッション限りだった状態をファイルとして
//! 永続化する受け皿になる。
//!
//! 実行される各行は **分類器（AI ルーティング）を一切経由しない**。
//! `try_shell_builtins` → `try_builtin` → `execute` の順に試すだけの
//! 純粋なコマンド実行パスであり、自然言語行が誤って AI に送られることはない。
//!
//! この実行器（[`Shell::run_rc_script`]）はファイルパス・表示名・ネスト深さを
//! パラメータ化しているため、Phase 4.3 の `source` ビルトイン統合
//! （`.toml` 以外の拡張子を rc スクリプトとして実行する）からもそのまま
//! 再利用できる。

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use tracing::{debug, info, warn};

use crate::engine::classifier::InputClassifier;
use crate::engine::{execute, try_builtin, CommandResult, LoopAction};

use super::Shell;

/// CLI から渡される rc スクリプトの読み込みオプション（Phase 4.2）。
///
/// `--rcfile <PATH>` と `--no-rc` は clap 側で `conflicts_with` により
/// 同時指定を拒否されるため、ここでは両方 unset（デフォルト）/
/// `rcfile` のみ / `no_rc` のみ、の3状態のみを想定する。
#[derive(Debug, Clone, Default)]
pub struct RcOptions {
    /// 明示的に指定された rc スクリプトのパス。デフォルトパス
    /// （[`rc_path`]）の代わりに使用し、存在しなくても自動生成しない。
    pub rcfile: Option<PathBuf>,
    /// rc スクリプトの読み込みを完全に無効化する（テンプレート生成も含む）。
    pub no_rc: bool,
}

/// 実行すべき rc スクリプトの解決結果。
#[derive(Debug)]
pub(super) enum ResolvedRc {
    /// デフォルトパス。存在しなければテンプレートを自動生成してよい。
    Default(PathBuf),
    /// `--rcfile` で明示指定されたパス。自動生成は行わない。
    Explicit(PathBuf),
    /// `--no-rc` 指定、または明示パスが見つからず読み込むものがない。
    None,
}

impl RcOptions {
    /// CLI オプションから実行すべき rc スクリプトを解決する。
    ///
    /// - `no_rc` が真なら常に [`ResolvedRc::None`]（`rcfile` が同時指定されて
    ///   いてもここには来ない — clap の `conflicts_with` が先に弾く）。
    /// - `rcfile` が `Some` ならそれを [`ResolvedRc::Explicit`] として返す
    ///   （存在確認は呼び出し側が行う）。
    /// - どちらも未指定ならデフォルトパスを [`ResolvedRc::Default`] として返す。
    pub(super) fn resolve(&self) -> ResolvedRc {
        if self.no_rc {
            return ResolvedRc::None;
        }
        if let Some(ref path) = self.rcfile {
            return ResolvedRc::Explicit(path.clone());
        }
        ResolvedRc::Default(rc_path())
    }
}

/// rc スクリプト中の実行対象1行（コメント・空行を除去済み）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RcLine {
    /// ファイル内の行番号（1始まり、コメント・空行を含む元の行番号）
    pub(super) lineno: usize,
    /// トリム済みの実行対象テキスト
    pub(super) text: String,
}

/// rc スクリプト実行後の制御結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RcOutcome {
    /// 全行を実行し終え、REPL ループを継続してよい
    Continue,
    /// `exit` / goodbye 相当の行によりシェル終了が要求された
    ExitRequested,
}

/// rc.jsh の初回自動生成テンプレート。
///
/// すべての行がコメントのみで構成され、実行可能な行を一切含まない
/// （[`parse_rc_lines`] を通すと空の `Vec` になる）。
pub(super) const TEMPLATE: &str = r#"# jarvish rc.jsh — startup script
#
# This file runs once, every time jarvish starts interactively — before
# the [startup].commands section of config.toml, and before the first
# prompt is shown. One command per line; blank lines and lines whose
# first non-whitespace character is '#' are skipped. There is no line
# continuation syntax — keep each command on a single line.
#
# IMPORTANT: every line here is executed through the same builtin path
# as typing it at the prompt (alias / export / complete / cd / source /
# ...), but it NEVER goes through the AI natural-language classifier.
# A line that would normally be routed to the AI assistant is instead
# run as a plain command and will simply fail as "command not found" —
# this file is for deterministic setup, not conversation.
#
# ── alias: define a shorthand for a command ──────────────────────────
# alias gs="git status"
#
# ── export: set an environment variable (expands $VARS) ─────────────
# export EDITOR="nvim"
#
# ── complete: register a fish-style completion for your own command ──
# (see the "Custom Completions" section of the README for the full
# flag reference: -c/-s/-l/-a/-d/-n)
# complete -c mycmd -s v -l verbose -d 'Verbose output'
#
# A failing line prints its error and line number but does NOT stop the
# rest of the script — every remaining line still runs.
"#;

/// rc.jsh のデフォルトパスを解決する。
///
/// `config_path()`（`~/.config/jarvish/config.toml`）と同じ規則で
/// `$HOME`（未設定時は `.` にフォールバック）配下の
/// `.config/jarvish/rc.jsh` を返す。
pub(super) fn rc_path() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".config/jarvish/rc.jsh")
}

/// rc.jsh が存在しなければコメントのみのテンプレートを生成する。
///
/// 既存ファイルは絶対に上書きしない（create-new 相当のセマンティクス）。
/// 生成に失敗した場合は警告を表示してシェルの起動は継続する
/// （`config.toml` の `create_default_config` と同じ warn-and-continue 方針）。
///
/// 明示的な `--rcfile` パス（Phase 4.2）に対しては呼び出さないこと —
/// この関数はデフォルトパスの初回起動時ブートストラップ専用。
pub(super) fn ensure_default_rc(path: &Path) {
    if path.exists() {
        return;
    }

    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(path = %parent.display(), error = %e, "Failed to create rc.jsh directory");
            eprintln!("jarvish: warning: failed to create rc.jsh directory: {e}");
            return;
        }
    }

    match std::fs::write(path, TEMPLATE) {
        Ok(()) => {
            info!(path = %path.display(), "Created default rc.jsh file");
        }
        Err(e) => {
            warn!(path = %path.display(), error = %e, "Failed to create default rc.jsh file");
            eprintln!("jarvish: warning: failed to create rc.jsh file: {e}");
        }
    }
}

/// rc スクリプトの内容を実行対象行のリストへパースする。
///
/// - 空行（空白のみを含む）はスキップする
/// - 先頭の非空白文字が `#` である行（インデントされたコメント含む）はスキップする
/// - 行中の `#`（コメントではない位置）は無視せず、行全体をそのまま残す
/// - CRLF（`\r\n`）はトリムで吸収される
/// - 行継続構文は存在しない（各行は独立して扱われる）
/// - `lineno` はコメント・空行を含む元のファイル内の行番号（1始まり）を保持する
pub(super) fn parse_rc_lines(content: &str) -> Vec<RcLine> {
    let mut lines = Vec::new();
    for (idx, raw) in content.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        lines.push(RcLine {
            lineno: idx + 1,
            text: trimmed.to_string(),
        });
    }
    lines
}

/// rc スクリプトの1行を分類器を経由せずに実行する。
///
/// `try_shell_builtins` → `try_builtin` → `execute` の順に試す
/// （`Shell::handle_input` と同じ優先順位だが、`InputClassifier::classify`
/// を一切呼び出さない）。
impl Shell {
    /// rc スクリプトファイルを実行する。
    ///
    /// - `path`: 実行するファイルの実パス
    /// - `display_name`: エラー表示に使うファイル名（例: `rc.jsh`）。
    ///   `source` 経由のネストしたスクリプト（Phase 4.3）でも同じ実行器を
    ///   再利用できるよう、ファイルパスと表示名を分離している。
    /// - `depth`: ネスト深さ（`source` からの再帰呼び出し用、Phase 4.3 で使用）。
    ///   このメソッド自体は深さの上限チェックを行わない
    ///   （呼び出し元 — Phase 4.3 の `source` 統合 — がガードする）。
    ///
    /// 各行は `last_exit_code` を更新し、失敗した行は
    /// `jarvish: {display_name}:{lineno}: ...` 形式でエラーを報告した上で
    /// 次の行へ継続する。`exit` / goodbye 相当の行が現れた場合は即座に
    /// `RcOutcome::ExitRequested` を返す。
    pub(super) async fn run_rc_script(
        &mut self,
        path: &Path,
        display_name: &str,
        depth: usize,
    ) -> RcOutcome {
        debug!(path = %path.display(), display_name, depth, "Running rc script");

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("jarvish: {display_name}: no such file or directory: {e}");
                return RcOutcome::Continue;
            }
        };

        let lines = parse_rc_lines(&content);
        for rc_line in lines {
            match self.run_rc_line(&rc_line.text) {
                RcLineOutcome::Ran(result) => {
                    self.last_exit_code
                        .store(result.exit_code, Ordering::Relaxed);
                    if result.exit_code != 0 {
                        eprintln!(
                            "jarvish: {display_name}:{}: command exited with status {}",
                            rc_line.lineno, result.exit_code
                        );
                    }
                    match result.action {
                        LoopAction::Exit => return RcOutcome::ExitRequested,
                        LoopAction::Restart => {
                            self.restart_requested.store(true, Ordering::Relaxed);
                            return RcOutcome::ExitRequested;
                        }
                        LoopAction::Continue => {}
                    }
                }
                RcLineOutcome::Exit => return RcOutcome::ExitRequested,
            }
        }

        RcOutcome::Continue
    }

    /// `self.rc_options`（`--rcfile` / `--no-rc`）を解決して rc スクリプトを
    /// 実行する、`run()` / `run_command()` 共通のエントリポイント。
    ///
    /// - [`ResolvedRc::None`][]（`--no-rc`）: 何もせず `RcOutcome::Continue` を返す。
    ///   デフォルトパスのテンプレート自動生成も行わない。
    /// - [`ResolvedRc::Default`][]: デフォルトパスが存在しなければテンプレートを
    ///   自動生成してから実行する（[`ensure_default_rc`]）。
    /// - [`ResolvedRc::Explicit`][]: 指定パスをそのまま実行する。自動生成は
    ///   行わない。ファイルが存在しない場合は
    ///   `jarvish: rcfile not found: {path}` を stderr に出して
    ///   `RcOutcome::Continue` を返す（rc なしで後続処理を継続させる）。
    pub(super) async fn run_configured_rc(&mut self) -> RcOutcome {
        match self.rc_options.resolve() {
            ResolvedRc::None => RcOutcome::Continue,
            ResolvedRc::Default(path) => {
                ensure_default_rc(&path);
                info!(path = %path.display(), "Executing rc.jsh");
                self.run_rc_script(&path, "rc.jsh", 0).await
            }
            ResolvedRc::Explicit(path) => {
                if !path.exists() {
                    eprintln!("jarvish: rcfile not found: {}", path.display());
                    return RcOutcome::Continue;
                }
                let display_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                info!(path = %path.display(), "Executing explicit --rcfile");
                self.run_rc_script(&path, &display_name, 0).await
            }
        }
    }

    /// 1行を分類器を経由せずに実行する決定コア。
    ///
    /// 優先順位: goodbye パターン → `try_shell_builtins` →
    /// `try_builtin` → `execute`。`handle_input` と違い
    /// `InputClassifier::classify` は一切呼ばれない。
    fn run_rc_line(&mut self, line: &str) -> RcLineOutcome {
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_rc_lines ──

    #[test]
    fn parse_rc_lines_skips_blank_lines() {
        let content = "alias g=git\n\n\nexport FOO=bar\n";
        let lines = parse_rc_lines(content);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "alias g=git");
        assert_eq!(lines[1].text, "export FOO=bar");
    }

    #[test]
    fn parse_rc_lines_skips_whitespace_only_lines() {
        let content = "alias g=git\n   \n\t\nexport FOO=bar\n";
        let lines = parse_rc_lines(content);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn parse_rc_lines_skips_comment_lines() {
        let content = "# a comment\nalias g=git\n# another comment\n";
        let lines = parse_rc_lines(content);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "alias g=git");
    }

    #[test]
    fn parse_rc_lines_skips_indented_comment_lines() {
        let content = "    # indented comment\nalias g=git\n\t# tab-indented comment\n";
        let lines = parse_rc_lines(content);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "alias g=git");
    }

    #[test]
    fn parse_rc_lines_mid_line_hash_is_not_a_comment() {
        // '#' がある位置が「行頭の非空白文字」でなければコメントではない。
        let content = "echo 'hello #world'\nalias grep='grep --color=auto # colorize'\n";
        let lines = parse_rc_lines(content);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "echo 'hello #world'");
        assert_eq!(lines[1].text, "alias grep='grep --color=auto # colorize'");
    }

    #[test]
    fn parse_rc_lines_preserves_line_numbers_across_skipped_lines() {
        let content = "# comment line 1\n\nalias g=git\n\n# comment line 5\nexport FOO=bar\n";
        let lines = parse_rc_lines(content);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].lineno, 3);
        assert_eq!(lines[1].lineno, 6);
    }

    #[test]
    fn parse_rc_lines_tolerates_crlf() {
        let content = "alias g=git\r\n# comment\r\nexport FOO=bar\r\n";
        let lines = parse_rc_lines(content);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "alias g=git");
        assert_eq!(lines[1].text, "export FOO=bar");
        // CR が末尾に残っていないことを確認する
        assert!(!lines[0].text.contains('\r'));
        assert!(!lines[1].text.contains('\r'));
    }

    #[test]
    fn parse_rc_lines_trims_leading_and_trailing_whitespace() {
        let content = "   alias g=git   \n";
        let lines = parse_rc_lines(content);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "alias g=git");
    }

    #[test]
    fn parse_rc_lines_empty_content_returns_empty_vec() {
        assert!(parse_rc_lines("").is_empty());
    }

    #[test]
    fn parse_rc_lines_only_comments_and_blanks_returns_empty_vec() {
        let content = "# only comments\n\n   \n# more comments\n";
        assert!(parse_rc_lines(content).is_empty());
    }

    // ── TEMPLATE ──

    #[test]
    fn template_parses_to_zero_executable_lines() {
        let lines = parse_rc_lines(TEMPLATE);
        assert!(
            lines.is_empty(),
            "TEMPLATE must be comments-only, got executable lines: {lines:?}"
        );
    }

    // ── rc_path ──

    #[test]
    fn rc_path_uses_home_env_var() {
        let original = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", "/tmp/jarvish-rc-path-test-home");
        }
        let path = rc_path();
        assert_eq!(
            path,
            PathBuf::from("/tmp/jarvish-rc-path-test-home/.config/jarvish/rc.jsh")
        );
        unsafe {
            match original {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    // ── RcOptions::resolve (Phase 4.2) ──

    #[test]
    fn resolve_no_rc_wins_regardless_of_rcfile() {
        // clap の conflicts_with で通常同時指定はできないが、resolve() 自体は
        // no_rc を最優先でチェックする防御的な順序になっていることを確認する。
        let opts = RcOptions {
            rcfile: Some(PathBuf::from("/tmp/should-be-ignored.jsh")),
            no_rc: true,
        };
        assert!(matches!(opts.resolve(), ResolvedRc::None));
    }

    #[test]
    fn resolve_no_rc_alone() {
        let opts = RcOptions {
            rcfile: None,
            no_rc: true,
        };
        assert!(matches!(opts.resolve(), ResolvedRc::None));
    }

    #[test]
    fn resolve_explicit_rcfile_returns_the_given_path() {
        let opts = RcOptions {
            rcfile: Some(PathBuf::from("/tmp/custom.jsh")),
            no_rc: false,
        };
        match opts.resolve() {
            ResolvedRc::Explicit(path) => assert_eq!(path, PathBuf::from("/tmp/custom.jsh")),
            other => panic!("expected ResolvedRc::Explicit, got {other:?}"),
        }
    }

    #[test]
    fn resolve_default_when_both_unset() {
        let original = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", "/tmp/jarvish-rc-resolve-test-home");
        }
        let opts = RcOptions::default();
        match opts.resolve() {
            ResolvedRc::Default(path) => assert_eq!(
                path,
                PathBuf::from("/tmp/jarvish-rc-resolve-test-home/.config/jarvish/rc.jsh")
            ),
            other => panic!("expected ResolvedRc::Default, got {other:?}"),
        }
        unsafe {
            match original {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    // ── ensure_default_rc ──

    #[test]
    fn ensure_default_rc_creates_file_and_parent_dirs() {
        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path().join("nested/dir/rc.jsh");
        assert!(!path.exists());

        ensure_default_rc(&path);

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, TEMPLATE);
    }

    #[test]
    fn ensure_default_rc_never_overwrites_existing_file() {
        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path().join("rc.jsh");
        std::fs::write(&path, "alias custom=echo\n").unwrap();

        ensure_default_rc(&path);

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "alias custom=echo\n");
    }

    #[test]
    fn ensure_default_rc_is_idempotent_create_once() {
        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path().join("rc.jsh");

        ensure_default_rc(&path);
        let first = std::fs::read_to_string(&path).unwrap();

        // 2回目の呼び出し前に手動編集を加え、上書きされないことを確認する
        std::fs::write(&path, "export EDITED=1\n").unwrap();
        ensure_default_rc(&path);
        let second = std::fs::read_to_string(&path).unwrap();

        assert_eq!(first, TEMPLATE);
        assert_eq!(second, "export EDITED=1\n");
    }

    // ── RcOutcome / RcLineOutcome の判別ロジック（Shell 構築なしのユニット部分）──
    //
    // `run_rc_line` / `run_rc_script` はメソッドのため `Shell` の構築を要するが、
    // その中核の分岐判断（goodbye 検出 → try_builtin の exit 検出 →
    // 通常継続）は、依拠する各関数（`InputClassifier::is_goodbye_pattern`,
    // `try_builtin`）を直接呼ぶことで `Shell` 抜きに検証できる
    // （テスタビリティ規約: 完全な `Shell` をテストで構築しない）。

    #[test]
    fn goodbye_pattern_detection_used_by_run_rc_line_matches_classifier() {
        assert!(InputClassifier::is_goodbye_pattern("goodbye"));
        assert!(InputClassifier::is_goodbye_pattern("bye"));
        assert!(InputClassifier::is_goodbye_pattern("さようなら"));
        assert!(!InputClassifier::is_goodbye_pattern(
            "echo goodbye-file.txt"
        ));
    }

    #[test]
    fn try_builtin_exit_line_signals_exit_action() {
        // run_rc_line が exit 行を RcLineOutcome::Ran として受け取り、
        // その action が LoopAction::Exit であることを直接確認する
        // (run_rc_script はこれを見て ExitRequested を返す)。
        let result = try_builtin("exit").expect("exit must be a recognized builtin");
        assert_eq!(result.action, LoopAction::Exit);
    }

    #[test]
    fn try_builtin_normal_command_continues() {
        let result = try_builtin("cd /tmp").expect("cd must be a recognized builtin");
        assert_eq!(result.action, LoopAction::Continue);
    }

    #[test]
    fn execute_unknown_command_line_is_nonzero_exit_but_continues() {
        // 分類器を経由しないため、自然言語らしい行はただの「不明なコマンド」
        // として失敗する（AI には絶対にルーティングされない）。
        let result = execute("please explain this error to me");
        assert_ne!(result.exit_code, 0);
    }
}
