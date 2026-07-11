//! `complete` ビルトイン — fish 風のユーザー定義補完を登録・一覧・消去する。
//!
//! CLI 面（fish 準拠、最小構成）:
//! - `complete -c CMD [-s X]... [-l LONG]... [-a 'WORDS'] [-d DESC] [-n COND]`
//!   → CMD に対して 1 個の [`CompletionSpec`] を登録（累積、`-c` 必須）。
//! - `complete`（引数なし）→ 登録済みの全 spec を、round-trip 可能な
//!   `complete -c cmd -s x -l long -a '...' -d '...'` 形式で 1 行 1 spec で列挙。
//! - `complete -e -c CMD` → CMD の全 spec を消去（`-c` なしの `-e` はエラー）。
//!
//! 実際の Tab 補完への反映は `RegistryProvider`（`src/cli/completer/`）が
//! 同じ `CompletionRegistry` を読み取ることで行う。このファイルは
//! 引数パースとレジストリへの変更（mutation）のみを担当する
//! （単一責任: データ構造は `registry.rs` 側）。`-a "$(...)"` の動的候補
//! 実行や `-n` の条件評価（`__fish_use_subcommand` /
//! `__fish_seen_subcommand_from`、Task 3.3）もすべて `RegistryProvider`
//! 側の責務であり、このファイルは受け取った文字列をそのまま
//! [`CompletionSpec`] に格納するだけで解釈しない。

use clap::Parser;

use crate::cli::completer::registry::{CompletionRegistry, CompletionSpec};
use crate::engine::CommandResult;

/// complete: fish 風のユーザー定義補完を登録・一覧・消去する。
#[derive(Parser, Debug, Default)]
#[command(name = "complete", about = "Define custom completions for a command")]
struct CompleteArgs {
    /// Command name to register/erase completions for
    #[arg(short = 'c', long = "command")]
    command: Option<String>,

    /// Short option (single character, e.g. -s v)
    #[arg(short = 's', action = clap::ArgAction::Append)]
    short: Vec<String>,

    /// Long option (e.g. -l verbose)
    #[arg(short = 'l', long = "long-option", action = clap::ArgAction::Append)]
    long: Vec<String>,

    /// Static candidate words (space-separated, quote to include spaces),
    /// or a single dynamic source "$(command)"
    #[arg(short = 'a', long = "arguments", allow_hyphen_values = true)]
    arguments: Option<String>,

    /// Fallback description shown alongside candidates
    #[arg(short = 'd', long = "description", allow_hyphen_values = true)]
    description: Option<String>,

    /// Condition expression; only __fish_use_subcommand and
    /// __fish_seen_subcommand_from are evaluated, others keep the spec
    /// registered but inactive for completion
    #[arg(short = 'n', long = "condition", allow_hyphen_values = true)]
    condition: Option<String>,

    /// Erase all completions registered for the given command
    #[arg(short = 'e', long = "erase")]
    erase: bool,
}

/// `complete` ビルトインの本体。`registry` に対して登録・一覧・消去を行う。
///
/// Shell 側から `&mut registry`（共有 `Arc<RwLock<CompletionRegistry>>` の
/// 書き込みロックガード）を渡して呼び出す想定（`alias` ビルトインと同じ配線）。
pub(crate) fn execute_with_registry(
    args: &[&str],
    registry: &mut CompletionRegistry,
) -> CommandResult {
    let parsed = match super::parse_args::<CompleteArgs>("complete", args) {
        Ok(a) => a,
        Err(result) => return result,
    };

    if parsed.erase {
        return erase(&parsed, registry);
    }

    if parsed.command.is_none()
        && parsed.short.is_empty()
        && parsed.long.is_empty()
        && parsed.arguments.is_none()
        && parsed.description.is_none()
        && parsed.condition.is_none()
    {
        return list_all(registry);
    }

    register(&parsed, registry)
}

/// 登録処理: `-c` は必須。`-s` は単一文字のみ許可する。
fn register(parsed: &CompleteArgs, registry: &mut CompletionRegistry) -> CommandResult {
    let Some(command) = parsed.command.as_deref() else {
        let msg =
            "jarvish: complete: -c/--command is required to register a completion\n".to_string();
        eprint!("{msg}");
        return CommandResult::error(msg, 2);
    };

    for s in &parsed.short {
        if s.chars().count() != 1 {
            let msg = format!("jarvish: complete: -s expects a single character, got '{s}'\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 2);
        }
    }

    let spec = CompletionSpec {
        short: parsed.short.clone(),
        long: parsed.long.clone(),
        arguments: parsed.arguments.clone(),
        description: parsed.description.clone(),
        condition: parsed.condition.clone(),
    };

    registry.register(command, spec);
    CommandResult::success(String::new())
}

/// 消去処理: `-c` なしの `-e` はエラー。
fn erase(parsed: &CompleteArgs, registry: &mut CompletionRegistry) -> CommandResult {
    let Some(command) = parsed.command.as_deref() else {
        let msg = "jarvish: complete: -e requires -c/--command\n".to_string();
        eprint!("{msg}");
        return CommandResult::error(msg, 2);
    };

    registry.erase(command);
    CommandResult::success(String::new())
}

/// 全 spec を round-trip 可能な `complete -c cmd ...` 形式で一覧表示する。
///
/// コマンド名は決定的な順序（`CompletionRegistry::iter_sorted`）で、
/// 各コマンド内の spec は登録順で出力する。
fn list_all(registry: &CompletionRegistry) -> CommandResult {
    let mut output = String::new();
    for (command, specs) in registry.iter_sorted() {
        for spec in specs {
            output.push_str(&format_spec_line(command, spec));
            output.push('\n');
        }
    }
    print!("{output}");
    CommandResult::success(output)
}

/// 1 個の spec を round-trip 可能な `complete -c cmd -s x -l long -a '...' -d '...'`
/// 形式の 1 行に整形する（末尾改行は含まない）。
fn format_spec_line(command: &str, spec: &CompletionSpec) -> String {
    let mut line = format!("complete -c {}", quote_if_needed(command));
    for s in &spec.short {
        line.push_str(" -s ");
        line.push_str(s);
    }
    for l in &spec.long {
        line.push_str(" -l ");
        line.push_str(&quote_if_needed(l));
    }
    if let Some(a) = &spec.arguments {
        line.push_str(" -a ");
        line.push_str(&quote_if_needed(a));
    }
    if let Some(d) = &spec.description {
        line.push_str(" -d ");
        line.push_str(&quote_if_needed(d));
    }
    if let Some(n) = &spec.condition {
        line.push_str(" -n ");
        line.push_str(&quote_if_needed(n));
    }
    line
}

/// 値を単一引用符で囲む必要があれば囲み、内包する `'` はエスケープする。
///
/// fish/POSIX シェルにはシングルクォート内のエスケープシーケンスがないため、
/// `'` -> `'\''`（クォートを閉じてエスケープ済み `'` を挿入し再度開く）方式を使う。
/// 空白・シングルクォート・二重引用符のいずれも含まない値はクォートせずそのまま返す
/// （fish の一覧出力の慣習に合わせ、単純な値は読みやすさを優先する）。
fn quote_if_needed(value: &str) -> String {
    let needs_quoting = value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || c == '\'' || c == '"');
    if !needs_quoting {
        return value.to_string();
    }
    let escaped = value.replace('\'', r"'\''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 登録・一覧の round-trip ──

    #[test]
    fn register_then_list_round_trips() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(
            &[
                "-c",
                "mycmd",
                "-s",
                "v",
                "-l",
                "verbose",
                "-d",
                "Verbose output",
            ],
            &mut registry,
        );
        assert_eq!(result.exit_code, 0);

        let listed = list_all(&registry);
        assert_eq!(listed.exit_code, 0);
        let line = listed.stdout.trim();
        assert_eq!(
            line,
            "complete -c mycmd -s v -l verbose -d 'Verbose output'"
        );

        // round-trip: 一覧の行をそのまま再投入したら同一のレジストリになる。
        let reparsed_args = shell_words_naive(line);
        let mut registry2 = CompletionRegistry::new();
        let args_without_cmd: Vec<&str> = reparsed_args[1..].iter().map(String::as_str).collect();
        let reg_result = execute_with_registry(&args_without_cmd, &mut registry2);
        assert_eq!(reg_result.exit_code, 0);

        assert_eq!(registry.specs_for("mycmd"), registry2.specs_for("mycmd"));
    }

    /// テスト専用の簡易シェル単語分割（シングルクォートのみ対応）。
    /// `execute_with_registry` に再投入するための最小限のトークナイザ。
    fn shell_words_naive(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut current = String::new();
        let mut in_quote = false;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\'' if in_quote => {
                    // `'\''` エスケープパターンをチェック
                    if chars.peek() == Some(&'\\') {
                        let mut lookahead = chars.clone();
                        lookahead.next(); // consume '\\'
                        if lookahead.next() == Some('\'') {
                            chars.next(); // consume '\\'
                            chars.next(); // consume '\''
                            current.push('\'');
                            continue;
                        }
                    }
                    in_quote = false;
                }
                '\'' => in_quote = true,
                c if c.is_whitespace() && !in_quote => {
                    if !current.is_empty() {
                        words.push(std::mem::take(&mut current));
                    }
                }
                c => current.push(c),
            }
        }
        if !current.is_empty() {
            words.push(current);
        }
        words
    }

    #[test]
    fn multiple_specs_accumulate_in_order() {
        let mut registry = CompletionRegistry::new();
        execute_with_registry(&["-c", "mycmd", "-s", "v", "-d", "verbose"], &mut registry);
        execute_with_registry(&["-c", "mycmd", "-s", "q", "-d", "quiet"], &mut registry);

        let specs = registry.specs_for("mycmd");
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].short, vec!["v"]);
        assert_eq!(specs[1].short, vec!["q"]);
    }

    #[test]
    fn erase_removes_only_named_command() {
        let mut registry = CompletionRegistry::new();
        execute_with_registry(&["-c", "mycmd", "-s", "v"], &mut registry);
        execute_with_registry(&["-c", "othercmd", "-s", "x"], &mut registry);

        let result = execute_with_registry(&["-e", "-c", "mycmd"], &mut registry);
        assert_eq!(result.exit_code, 0);
        assert!(registry.specs_for("mycmd").is_empty());
        assert_eq!(registry.specs_for("othercmd").len(), 1);
    }

    // ── エラーパス（全て exit 2） ──

    #[test]
    fn register_without_command_is_error() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["-s", "v"], &mut registry);
        assert_eq!(result.exit_code, 2);
        assert!(result.stderr.contains("-c"));
    }

    #[test]
    fn erase_without_command_is_error() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["-e"], &mut registry);
        assert_eq!(result.exit_code, 2);
        assert!(result.stderr.contains("-c"));
    }

    #[test]
    fn short_option_longer_than_one_char_is_error() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["-c", "mycmd", "-s", "verbose"], &mut registry);
        assert_eq!(result.exit_code, 2);
        assert!(result.stderr.contains("single character"));
        assert!(registry.specs_for("mycmd").is_empty());
    }

    // ── クォート処理 ──

    #[test]
    fn listing_quotes_values_with_spaces_and_embedded_single_quote() {
        let mut registry = CompletionRegistry::new();
        execute_with_registry(
            &["-c", "mycmd", "-d", "it's a value with spaces"],
            &mut registry,
        );

        let listed = list_all(&registry);
        let line = listed.stdout.trim();
        assert_eq!(line, r"complete -c mycmd -d 'it'\''s a value with spaces'");
    }

    #[test]
    fn listing_simple_values_unquoted() {
        let mut registry = CompletionRegistry::new();
        execute_with_registry(&["-c", "mycmd", "-l", "verbose"], &mut registry);

        let listed = list_all(&registry);
        assert_eq!(listed.stdout.trim(), "complete -c mycmd -l verbose");
    }

    #[test]
    fn no_args_lists_nothing_when_empty() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&[], &mut registry);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "");
    }

    #[test]
    fn list_is_deterministic_across_commands() {
        let mut registry = CompletionRegistry::new();
        execute_with_registry(&["-c", "zeta", "-s", "z"], &mut registry);
        execute_with_registry(&["-c", "alpha", "-s", "a"], &mut registry);

        let listed = list_all(&registry);
        let lines: Vec<&str> = listed.stdout.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("complete -c alpha"));
        assert!(lines[1].starts_with("complete -c zeta"));
    }

    // ── --help ──

    #[test]
    fn help_returns_success() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["--help"], &mut registry);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("complete"));
    }
}
