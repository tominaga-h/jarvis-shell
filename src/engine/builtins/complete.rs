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

/// `dispatch_builtin` スタブ経由（パイプライン内・コマンドリスト内・
/// `ai_pipe` 経由など「standalone シェルビルトイン」経路を通らない全て）
/// から `complete` が呼ばれたときのエントリポイント。
///
/// これらの経路には `Shell` が保持する実 [`CompletionRegistry`] への
/// アクセスがなく、`&mut CompletionRegistry::new()` のような使い捨て
/// レジストリに対して register/list/erase を行うと、変更が静かに消える
/// （#89 レビュー指摘 A1）。よってここでは **`--help`/`-h` 以外は
/// すべて拒否**し、実データに触れず明確なエラーを返す。
///
/// `--help` だけは `help complete`（`help.rs` が
/// `dispatch_builtin(cmd, ["--help"])` に委譲する）で使われるため、
/// 副作用なしに動き続ける必要がある。
pub(crate) fn execute_standalone_only(args: &[&str]) -> CommandResult {
    if is_help_only(args) {
        // --help/-h は clap のヘルプ生成のみを行い、レジストリには一切触れない。
        // register/list/erase 用の使い捨てレジストリを渡さないよう、
        // 実際のパースは既存の `execute_with_registry` に委譲しつつ、
        // 呼び出し元では --help 以外の args を通さないことで安全性を担保する。
        return execute_with_registry(args, &mut CompletionRegistry::new());
    }

    let msg = "jarvish: complete: can only be used as a standalone command\n".to_string();
    eprint!("{msg}");
    CommandResult::error(msg, 1)
}

/// `args` が `--help` / `-h` のみ（他の引数を伴わない）かどうかを判定する。
///
/// clap は `--help`/`-h` が他の引数と混在していても最優先で処理するが、
/// ここでは意図を明確にするため「実質的にヘルプ要求のみ」の形に限定する
/// （他の複数引数と `--help` が混在するケースは register/list/erase 相当の
/// 操作を意図している可能性があるため standalone エラーに倒す）。
fn is_help_only(args: &[&str]) -> bool {
    args == ["--help"] || args == ["-h"]
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
        if let Err(msg) = validate_short(s) {
            eprint!("{msg}");
            return CommandResult::error(msg, 2);
        }
    }

    for l in &parsed.long {
        if let Err(msg) = validate_long(l) {
            eprint!("{msg}");
            return CommandResult::error(msg, 2);
        }
    }

    // round-trip 不能な値（改行 / NUL）は `list_all` の「1 spec = 1 行」
    // 契約を壊す（埋め込み改行は行を分断し、NUL はそもそも文字列として
    // 扱えない）ため、登録時点で拒否する（#89 A2）。
    for (flag, value) in [
        ("-c", Some(command)),
        ("-a", parsed.arguments.as_deref()),
        ("-d", parsed.description.as_deref()),
        ("-n", parsed.condition.as_deref()),
    ] {
        if let Some(v) = value {
            if let Err(msg) = validate_round_trippable(flag, v) {
                eprint!("{msg}");
                return CommandResult::error(msg, 2);
            }
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

/// `-s` の値を検証する: ちょうど 1 文字の ASCII 印字可能文字のみ許可する。
///
/// クォート文字（`'` `"`）・空白・バックスラッシュを許すと、`list_all` が
/// 出力する行を `split_quoted` で再パースしたときに `-s` の値が
/// クォート/エスケープなしでそのまま埋め込まれる（`format_spec_line` 参照）
/// ため、行全体の構造を破壊できてしまう（#89 A2）。
fn validate_short(s: &str) -> Result<(), String> {
    let mut chars = s.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return Err(format!(
            "jarvish: complete: -s expects a single character, got '{s}'\n"
        ));
    };
    if !c.is_ascii_graphic() || c == '\'' || c == '"' || c == '\\' {
        return Err(format!(
            "jarvish: complete: -s must be a single ASCII graphic character \
             excluding quotes/backslash, got '{s}'\n"
        ));
    }
    Ok(())
}

/// `-l` の値を検証する: 空白・クォート・バックスラッシュを含まないことを要求する。
///
/// `-s` と同じ理由（`format_spec_line` が `-l` の値を `quote_if_needed`
/// 経由で出力するため通常は安全だが、制御文字混入によるトークナイザ破壊を
/// 早期に拒否する）。
fn validate_long(l: &str) -> Result<(), String> {
    if l.is_empty() {
        return Err("jarvish: complete: -l value must not be empty\n".to_string());
    }
    if l.chars().any(|c| c.is_whitespace() || c == '\\') {
        return Err(format!(
            "jarvish: complete: -l must not contain whitespace or backslash, got '{l}'\n"
        ));
    }
    Ok(())
}

/// 改行・NUL を含む値を拒否する。
///
/// `list_all` は 1 spec を 1 行で出力する契約のため、値に埋め込み改行が
/// あると出力行が分断され `-c`/`-a`/`-d`/`-n` の境界が壊れる。NUL はそもそも
/// シェルの引数として表現できない。
fn validate_round_trippable(flag: &str, value: &str) -> Result<(), String> {
    if value.contains('\n') || value.contains('\r') || value.contains('\0') {
        return Err(format!(
            "jarvish: complete: {flag} value must not contain newline/NUL characters (would break round-trip listing)\n"
        ));
    }
    Ok(())
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
        line.push_str(&quote_if_needed(s));
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
/// `list_all` が出力する行は、実シェル（jarvish 自身）の
/// [`crate::engine::expand::split_quoted`] で再パースされて初めて
/// round-trip が成立する。このトークナイザは：
/// - クォート外の裸の `\` を「次の 1 文字をリテラル化するエスケープ」として
///   消費する（例: `\U` -> `U`）。したがって `-d 'C:\Users\name'`
///   のようにバックスラッシュを含む値を無クォートのまま出力すると、
///   再パース時に `\U` `\n` 等が消えて全く別の文字列になる（サイレントな
///   意味破壊）。
/// - シングルクォート内は完全にリテラル（エスケープなし、`'` 自体は
///   表現できない）。
///
/// この 2 点により、**安全に無クォートで出力してよい文字集合はごく限定的**
/// （英数字と、シェルのメタ文字・エスケープ文字・クォート文字のいずれでもない
/// 一部の記号のみ）でなければならない。それ以外の文字を 1 個でも含む値は
/// 単一引用符で丸ごと囲み、内包する `'` は `'\''`（クォートを閉じて
/// エスケープ済み `'` を挿入し再度開く）で表現する。空文字列は `''` とする。
///
/// `split_quoted` 側の対応する挙動（シングルクォート内は無条件でリテラル、
/// `'\''` は「クォート終了」+「バックスラッシュエスケープされた `'`」+
/// 「クォート再開」の 3 トークンとして連結される）と組み合わせることで、
/// このシングルクォートラップは `split_quoted` の逆関数になる
/// （改行 / NUL は `validate_round_trippable` により登録時点で既に拒否済み
/// のため、ここでは扱わない）。
fn quote_if_needed(value: &str) -> String {
    if !value.is_empty() && value.chars().all(is_safe_unquoted_char) {
        return value.to_string();
    }
    let escaped = value.replace('\'', r"'\''");
    format!("'{escaped}'")
}

/// クォートなしで出力しても `split_quoted` で安全に往復できる文字か。
///
/// 保守的な安全集合: 英数字 + `_ . / : = + , @ % ^ -`。
/// 空白・タブ・引用符・バックスラッシュ・`$`・バッククォート・シェル制御
/// 演算子（`|` `>` `<` `;` `&` `(` `)` 等）はすべて対象外とし、少しでも
/// 該当すれば呼び出し元（[`quote_if_needed`]）が単一引用符で丸ごと囲む。
fn is_safe_unquoted_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '_' | '.' | '/' | ':' | '=' | '+' | ',' | '@' | '%' | '^' | '-'
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::expand::split_quoted;

    // ── 登録・一覧の round-trip ──

    /// `list_all` が出力した行を、実シェルが使うのと同じ
    /// [`split_quoted`] で再トークナイズし、`complete` に再投入するための
    /// 引数列（先頭の `complete` トークンを除く）を返す。
    ///
    /// テスト専用の簡易パーサではなく実物のトークナイザを使うことで、
    /// 「一覧表示 → 実シェルでの再パース → 再登録」という実際の利用者の
    /// フローを検証する（#89 A2: round-trip fidelity は `split_quoted`
    /// に対して証明されなければならない）。
    fn retokenize_listed_line(line: &str) -> Vec<String> {
        let tokens = split_quoted(line).expect("listed line must be valid shell syntax");
        tokens.into_iter().map(|t| t.value).collect()
    }

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

        // round-trip: 一覧の行を実物の split_quoted で再パースしてから
        // そのまま再投入したら同一のレジストリになる。
        let reparsed_args = retokenize_listed_line(line);
        let mut registry2 = CompletionRegistry::new();
        let args_without_cmd: Vec<&str> = reparsed_args[1..].iter().map(String::as_str).collect();
        let reg_result = execute_with_registry(&args_without_cmd, &mut registry2);
        assert_eq!(reg_result.exit_code, 0);

        assert_eq!(registry.specs_for("mycmd"), registry2.specs_for("mycmd"));
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

    // ── バックスラッシュを含む値の round-trip（#89 A2, 修正前は corruption）──

    #[test]
    fn backslash_containing_value_round_trips_through_split_quoted() {
        // 修正前は quote_if_needed がバックスラッシュを検出せず無クォートで
        // 出力していたため、split_quoted による再パースで `\U` `\n` 等が
        // エスケープとして消費され "C:Usersname" に化けていた（-n 値で最悪）。
        let mut registry = CompletionRegistry::new();
        execute_with_registry(&["-c", "mycmd", "-n", r"C:\Users\name"], &mut registry);

        let listed = list_all(&registry);
        let line = listed.stdout.trim();

        let reparsed_args = retokenize_listed_line(line);
        let mut registry2 = CompletionRegistry::new();
        let args_without_cmd: Vec<&str> = reparsed_args[1..].iter().map(String::as_str).collect();
        execute_with_registry(&args_without_cmd, &mut registry2);

        assert_eq!(registry.specs_for("mycmd"), registry2.specs_for("mycmd"));
        let spec = &registry2.specs_for("mycmd")[0];
        assert_eq!(spec.condition.as_deref(), Some(r"C:\Users\name"));
    }

    #[test]
    fn backslash_value_is_single_quoted_in_listing() {
        let mut registry = CompletionRegistry::new();
        execute_with_registry(&["-c", "mycmd", "-d", r"a\b"], &mut registry);
        let listed = list_all(&registry);
        assert_eq!(listed.stdout.trim(), r"complete -c mycmd -d 'a\b'");
    }

    // ── PROPERTY: 拷問テーブル round-trip（#89 A2）──
    //
    // register -> list_all -> split_quoted で再パース -> 新しいレジストリへ
    // 再登録 -> spec が完全一致することを、多様な値のテーブルに対して検証する。
    // 改行 / NUL は登録時点で拒否されるため別テスト（reject_*）で扱う。

    #[test]
    fn round_trip_property_torture_table() {
        let torture_values: &[&str] = &[
            "simple",
            r"back\slash",
            r"C:\Users\name",
            r"trailing\",
            "it's got a quote",
            "\"double quoted\"",
            "mixed 'single' and \"double\"",
            "$HOME and $(cmd) and `cmd`",
            "space separated words",
            "\ttab\tseparated",
            "unicode: 日本語 🎉 café",
            "",
            "-leading-hyphen",
            "--looks-like-a-flag",
            "a'b'c'd",
            r"\'",
            r"\\",
            "semi;colon;pipe|amp&and&&or||redirect><",
            "trailing space ",
            " leading space",
        ];

        for &value in torture_values {
            for flag in ["-a", "-d", "-n"] {
                let mut registry = CompletionRegistry::new();
                let reg_result =
                    execute_with_registry(&["-c", "mycmd", flag, value], &mut registry);
                assert_eq!(
                    reg_result.exit_code, 0,
                    "registration should succeed for flag={flag} value={value:?}"
                );

                let listed = list_all(&registry);
                let line = listed.stdout.trim();

                let reparsed_args = retokenize_listed_line(line);
                let mut registry2 = CompletionRegistry::new();
                let args_without_cmd: Vec<&str> =
                    reparsed_args[1..].iter().map(String::as_str).collect();
                let reparse_result = execute_with_registry(&args_without_cmd, &mut registry2);
                assert_eq!(
                    reparse_result.exit_code, 0,
                    "re-registration should succeed for flag={flag} value={value:?} line={line:?}"
                );

                assert_eq!(
                    registry.specs_for("mycmd"),
                    registry2.specs_for("mycmd"),
                    "round-trip mismatch for flag={flag} value={value:?} listed_line={line:?}"
                );
            }
        }
    }

    #[test]
    fn round_trip_property_command_name_torture_table() {
        // -c（コマンド名）自体も同じ危険な文字集合を含みうる。
        let torture_values: &[&str] = &["simple-cmd", r"back\slash-cmd", "with space cmd"];

        for &value in torture_values {
            let mut registry = CompletionRegistry::new();
            let reg_result = execute_with_registry(&["-c", value, "-s", "v"], &mut registry);
            assert_eq!(reg_result.exit_code, 0);

            let listed = list_all(&registry);
            let line = listed.stdout.trim();
            let reparsed_args = retokenize_listed_line(line);
            let mut registry2 = CompletionRegistry::new();
            let args_without_cmd: Vec<&str> =
                reparsed_args[1..].iter().map(String::as_str).collect();
            execute_with_registry(&args_without_cmd, &mut registry2);

            assert_eq!(
                registry.specs_for(value),
                registry2.specs_for(value),
                "command-name round-trip mismatch for value={value:?} listed_line={line:?}"
            );
        }
    }

    // ── 改行 / NUL は登録時点で拒否される（round-trip 不能, #89 A2）──

    #[test]
    fn newline_in_argument_value_is_rejected() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["-c", "mycmd", "-a", "a\nb"], &mut registry);
        assert_eq!(result.exit_code, 2);
        assert!(result.stderr.contains("newline"));
        assert!(registry.specs_for("mycmd").is_empty());
    }

    #[test]
    fn carriage_return_in_description_value_is_rejected() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["-c", "mycmd", "-d", "a\rb"], &mut registry);
        assert_eq!(result.exit_code, 2);
        assert!(registry.specs_for("mycmd").is_empty());
    }

    #[test]
    fn nul_in_condition_value_is_rejected() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["-c", "mycmd", "-n", "a\0b"], &mut registry);
        assert_eq!(result.exit_code, 2);
        assert!(registry.specs_for("mycmd").is_empty());
    }

    #[test]
    fn newline_in_command_name_is_rejected() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["-c", "my\ncmd", "-s", "v"], &mut registry);
        assert_eq!(result.exit_code, 2);
        assert!(registry.specs_for("my\ncmd").is_empty());
    }

    // ── -s / -l 検証の強化（#89 A2）──

    #[test]
    fn short_option_with_quote_char_is_rejected() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["-c", "mycmd", "-s", "'"], &mut registry);
        assert_eq!(result.exit_code, 2);
        assert!(registry.specs_for("mycmd").is_empty());
    }

    #[test]
    fn short_option_with_double_quote_char_is_rejected() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["-c", "mycmd", "-s", "\""], &mut registry);
        assert_eq!(result.exit_code, 2);
        assert!(registry.specs_for("mycmd").is_empty());
    }

    #[test]
    fn short_option_with_backslash_is_rejected() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["-c", "mycmd", "-s", "\\"], &mut registry);
        assert_eq!(result.exit_code, 2);
        assert!(registry.specs_for("mycmd").is_empty());
    }

    #[test]
    fn short_option_with_whitespace_is_rejected() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["-c", "mycmd", "-s", " "], &mut registry);
        assert_eq!(result.exit_code, 2);
        assert!(registry.specs_for("mycmd").is_empty());
    }

    #[test]
    fn short_option_single_safe_char_is_accepted() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["-c", "mycmd", "-s", "v"], &mut registry);
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn long_option_with_whitespace_is_rejected() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["-c", "mycmd", "-l", "long opt"], &mut registry);
        assert_eq!(result.exit_code, 2);
        assert!(registry.specs_for("mycmd").is_empty());
    }

    #[test]
    fn long_option_with_backslash_is_rejected() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["-c", "mycmd", "-l", r"long\opt"], &mut registry);
        assert_eq!(result.exit_code, 2);
        assert!(registry.specs_for("mycmd").is_empty());
    }

    #[test]
    fn long_option_empty_is_rejected() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["-c", "mycmd", "-l", ""], &mut registry);
        assert_eq!(result.exit_code, 2);
        assert!(registry.specs_for("mycmd").is_empty());
    }

    #[test]
    fn long_option_safe_value_is_accepted() {
        let mut registry = CompletionRegistry::new();
        let result = execute_with_registry(&["-c", "mycmd", "-l", "verbose-mode"], &mut registry);
        assert_eq!(result.exit_code, 0);
    }
}
