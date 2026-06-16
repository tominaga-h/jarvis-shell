//! コマンド置換展開（`$(...)` / backtick `` `...` ``）
//!
//! トークン内に現れる `$(cmd)` または `` `cmd` `` を検出し、
//! 内部コマンドを実行してその標準出力で置換する。
//!
//! 展開順序（pipeline）では **最初** に適用される段であり、
//! ここで得たテキストはその後 basic(tilde/env) → brace → glob に流れる。
//!
//! ## word-split の挙動
//! - クォート外（[`SubstQuoting::Unquoted`]）の置換結果は空白
//!   （` \t\n`）で単語分割する（連続空白は畳み、空要素は除去）。
//! - ダブルクォート内（[`SubstQuoting::DoubleQuoted`]）の置換結果は分割せず、
//!   1 要素として返す。
//!
//! ## 内部コマンド失敗時の挙動
//! 内部コマンドの起動失敗・非ゼロ終了は [`CmdSubstError::Exec`] としてエラー化し、
//! 展開を中断する（bash の「空文字で続行」ではなく中断を選択）。
//!
//! ## V1 の限界（既知の非対応事項）
//! - `$?`（直前の終了ステータス）の伝播は行わない。
//! - backtick 内の `` \$ ``・`` \` ``・`` \\ `` エスケープは未対応
//!   （素朴に次の backtick で閉じる）。
//! - 置換結果テキストに対する再ブレース展開・再チルダ展開は行わない
//!   （basic/glob は適用されるが、テキスト由来の brace/tilde は展開対象外）。
//! - 混在クォート（例: `"$(a)"$(b)`）の span ごとの厳密なクォート文脈処理は行わず、
//!   トークン基底の文脈（[`SubstQuoting`]）で近似する。
//!   関連して、span 境界スキャナはクォートを認識しないため、`$(...)` 内側の
//!   クォート文字列に含まれるリテラルの `)`（例: `echo $(echo ")")`）では
//!   最初の `)` で span を早期クローズしてしまい parse error になる
//!   （panic はせず安全に停止する）。
//! - `$(yes)` のような無限出力コマンドに対するタイムアウトは設けない。

use std::cell::Cell;

use crate::engine::expand;
use crate::engine::parser;

/// 置換結果に適用するクォート文脈
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubstQuoting {
    /// クォート外: 置換結果を空白で単語分割する
    Unquoted,
    /// ダブルクォート内: 置換結果を分割しない
    DoubleQuoted,
}

/// コマンド置換のエラー
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdSubstError {
    /// `$(...)` または backtick が閉じられていない（保持する内容は未終端の断片）
    Unterminated(String),
    /// 置換のネストが深すぎる
    NestingTooDeep,
    /// 内部コマンドの実行に失敗した（起動失敗・非ゼロ終了など）
    Exec(String),
}

impl std::fmt::Display for CmdSubstError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CmdSubstError::Unterminated(frag) => {
                write!(f, "unterminated command substitution: {frag}")
            }
            CmdSubstError::NestingTooDeep => write!(f, "command substitution nested too deep"),
            CmdSubstError::Exec(msg) => write!(f, "command substitution failed: {msg}"),
        }
    }
}

/// コマンド置換のネスト上限。これを超えると [`CmdSubstError::NestingTooDeep`]。
const MAX_SUBST_DEPTH: usize = 32;

thread_local! {
    /// 現在のコマンド置換ネスト深さ。pipeline 経由の再入で引数が分断されるため
    /// thread-local に保持する。
    static SUBST_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// `SUBST_DEPTH` を RAII でインクリメント/デクリメントするガード。
/// パニック時も `Drop` で確実に元に戻す。
struct DepthGuard;

impl DepthGuard {
    /// 深さをインクリメントしてガードを返す。上限超過時は `None`。
    fn enter() -> Option<Self> {
        SUBST_DEPTH.with(|d| {
            let next = d.get() + 1;
            if next > MAX_SUBST_DEPTH {
                None
            } else {
                d.set(next);
                Some(DepthGuard)
            }
        })
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        SUBST_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// トークンに含まれる `$(...)` / backtick のコマンド置換を展開する。
///
/// 置換が含まれない場合は高速パスで `vec![token]` を即返す。
/// 置換を実行して 1 本の文字列を組み立てた後、`ctx` に応じて
/// 単語分割（[`SubstQuoting::Unquoted`]）または非分割
/// （[`SubstQuoting::DoubleQuoted`]）で結果を返す。
pub fn expand_command_subst(token: &str, ctx: SubstQuoting) -> Result<Vec<String>, CmdSubstError> {
    // 高速パス: 置換構文を含まなければそのまま返す。
    if !token.contains("$(") && !token.contains('`') {
        return Ok(vec![token.to_string()]);
    }

    let assembled = substitute_spans(token)?;

    match ctx {
        SubstQuoting::Unquoted => Ok(word_split(&assembled)),
        SubstQuoting::DoubleQuoted => Ok(vec![assembled]),
    }
}

/// token を走査し、各置換 span をその実行結果テキストに差し替えた
/// 1 本の文字列を組み立てる。
fn substitute_spans(token: &str) -> Result<String, CmdSubstError> {
    let chars: Vec<char> = token.chars().collect();
    let mut out = String::with_capacity(token.len());
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        // `$(...)` 形式
        if c == '$' && i + 1 < chars.len() && chars[i + 1] == '(' {
            let (inner, next) = take_paren_span(&chars, i + 2)?;
            let output = capture_subshell(&inner)?;
            out.push_str(output.trim_end_matches('\n'));
            i = next;
            continue;
        }

        // backtick 形式
        if c == '`' {
            let (inner, next) = take_backtick_span(&chars, i + 1)?;
            let output = capture_subshell(&inner)?;
            out.push_str(output.trim_end_matches('\n'));
            i = next;
            continue;
        }

        out.push(c);
        i += 1;
    }

    Ok(out)
}

/// `$(` の直後（`start`）から括弧バランスで対応する `)` を探し、
/// 内側の文字列と「`)` の次のインデックス」を返す。ネスト対応。
///
/// 閉じられていなければ [`CmdSubstError::Unterminated`]。
fn take_paren_span(chars: &[char], start: usize) -> Result<(String, usize), CmdSubstError> {
    let mut depth = 1usize;
    let mut inner = String::new();
    let mut i = start;

    while i < chars.len() {
        let c = chars[i];
        match c {
            '(' => {
                depth += 1;
                inner.push(c);
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok((inner, i + 1));
                }
                inner.push(c);
            }
            _ => inner.push(c),
        }
        i += 1;
    }

    Err(CmdSubstError::Unterminated(format!("$({inner}")))
}

/// 最初の backtick の直後（`start`）から次の backtick までを span とする。
/// V1 はエスケープ未対応のため素朴に次の `` ` `` で閉じる。
///
/// 閉じられていなければ [`CmdSubstError::Unterminated`]。
fn take_backtick_span(chars: &[char], start: usize) -> Result<(String, usize), CmdSubstError> {
    let mut inner = String::new();
    let mut i = start;

    while i < chars.len() {
        if chars[i] == '`' {
            return Ok((inner, i + 1));
        }
        inner.push(chars[i]);
        i += 1;
    }

    Err(CmdSubstError::Unterminated(format!("`{inner}")))
}

/// 置換結果の文字列を空白（` \t\n`）で単語分割する。
/// 連続空白は畳み、空要素は除去する。
fn word_split(s: &str) -> Vec<String> {
    s.split([' ', '\t', '\n'])
        .filter(|piece| !piece.is_empty())
        .map(|piece| piece.to_string())
        .collect()
}

/// 内側コマンド文字列をサブシェルとして実行し、stdout を返す。
///
/// 1. クォート対応トークナイズ（[`expand::split_quoted`]）
/// 2. 各トークンを pipeline 展開（[`expand::expand_token_globs`]）
///    — ここでネストした `$(...)` も再帰的に解決される（常に Unquoted 文脈）
/// 3. [`parser::parse_pipeline`] で AST 化
/// 4. [`crate::engine::exec::run_pipeline_captured`] で stdout を取得
///
/// 非ゼロ終了は [`CmdSubstError::Exec`] としてエラー化する。
fn capture_subshell(inner: &str) -> Result<String, CmdSubstError> {
    // 再帰ガード: ネストが深すぎる場合は中断。
    let _guard = DepthGuard::enter().ok_or(CmdSubstError::NestingTooDeep)?;

    let tokens = expand::split_quoted(inner)
        .map_err(|e| CmdSubstError::Exec(format!("parse error: {e}")))?;

    let mut expanded: Vec<String> = Vec::with_capacity(tokens.len());
    for tok in tokens {
        if matches!(
            tok.value.as_str(),
            "|" | ">" | ">>" | "<" | "&&" | "||" | ";"
        ) {
            expanded.push(tok.value);
            continue;
        }
        if tok.quoted && !tok.has_subst {
            expanded.push(tok.value);
            continue;
        }
        let expanded_result = if tok.quoted && tok.has_subst {
            // クォート内の置換: 置換のみ行い glob/brace は適用しない（bash 準拠）。
            expand::expand_token_subst_only(&tok.value, tok.subst_quoting)
        } else if tok.has_subst {
            expand::expand_token_globs_with_quoting(&tok.value, tok.subst_quoting)
        } else {
            expand::expand_token_globs(&tok.value)
        };
        match expanded_result {
            Ok(parts) => expanded.extend(parts),
            Err(e) => return Err(CmdSubstError::Exec(e.to_string())),
        }
    }

    if expanded.is_empty() {
        return Ok(String::new());
    }

    let pipeline = parser::parse_pipeline(expanded)
        .map_err(|e| CmdSubstError::Exec(format!("parse error: {e}")))?;

    let result = crate::engine::exec::run_pipeline_captured(&pipeline);
    if result.exit_code != 0 {
        return Err(CmdSubstError::Exec(format!(
            "command exited with status {}",
            result.exit_code
        )));
    }

    Ok(result.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // ── 高速パス ──

    #[test]
    fn no_substitution_fast_path() {
        let result = expand_command_subst("hello", SubstQuoting::Unquoted).unwrap();
        assert_eq!(result, vec!["hello".to_string()]);
    }

    #[test]
    fn no_substitution_with_dollar_only() {
        // `$VAR` のような env 参照は置換構文ではないので素通し（高速パス）。
        let result = expand_command_subst("$VAR", SubstQuoting::Unquoted).unwrap();
        assert_eq!(result, vec!["$VAR".to_string()]);
    }

    // ── 基本展開 ──

    #[test]
    fn basic_command_subst() {
        let result = expand_command_subst("$(echo hello)", SubstQuoting::Unquoted).unwrap();
        assert_eq!(result, vec!["hello".to_string()]);
    }

    #[test]
    fn word_split_multiple_words() {
        let result = expand_command_subst("$(echo a b c)", SubstQuoting::Unquoted).unwrap();
        assert_eq!(
            result,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn word_split_collapses_consecutive_whitespace() {
        let result = expand_command_subst("$(printf 'a   b')", SubstQuoting::Unquoted).unwrap();
        assert_eq!(result, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn double_quoted_no_split() {
        let result = expand_command_subst("$(printf 'a   b')", SubstQuoting::DoubleQuoted).unwrap();
        assert_eq!(result, vec!["a   b".to_string()]);
    }

    // ── trailing newline ──

    #[test]
    fn trailing_newlines_all_stripped() {
        // printf 'x\n\n' → 末尾改行を全除去 → "x"
        let result =
            expand_command_subst("$(printf 'x\\n\\n')", SubstQuoting::DoubleQuoted).unwrap();
        assert_eq!(result, vec!["x".to_string()]);
    }

    #[test]
    fn internal_newline_preserved_in_double_quote() {
        // 中間の改行は保持される（末尾のみ除去）。
        let result =
            expand_command_subst("$(printf 'a\\nb\\n')", SubstQuoting::DoubleQuoted).unwrap();
        assert_eq!(result, vec!["a\nb".to_string()]);
    }

    // ── 空出力 ──

    #[test]
    #[serial]
    fn empty_output_unquoted_yields_no_words() {
        // unquoted な空出力は単語分割で 0 語になる（bash 準拠）。
        let result = expand_command_subst("$(true)", SubstQuoting::Unquoted).unwrap();
        assert_eq!(result, Vec::<String>::new());
    }

    #[test]
    #[serial]
    fn empty_output_double_quoted_yields_one_empty_word() {
        // ダブルクォート内の空出力は分割されず 1 つの空文字列語になる（bash 準拠）。
        let result = expand_command_subst("$(true)", SubstQuoting::DoubleQuoted).unwrap();
        assert_eq!(result, vec![String::new()]);
    }

    // ── 埋め込み連結 ──

    #[test]
    fn embedded_concatenation() {
        let result =
            expand_command_subst("prefix-$(echo mid)-suffix", SubstQuoting::Unquoted).unwrap();
        assert_eq!(result, vec!["prefix-mid-suffix".to_string()]);
    }

    // ── backtick ──

    #[test]
    fn backtick_basic() {
        let result = expand_command_subst("`echo hi`", SubstQuoting::Unquoted).unwrap();
        assert_eq!(result, vec!["hi".to_string()]);
    }

    // ── ネスト ──

    #[test]
    fn nested_command_subst() {
        let result = expand_command_subst("$(echo $(echo deep))", SubstQuoting::Unquoted).unwrap();
        assert_eq!(result, vec!["deep".to_string()]);
    }

    // ── 未終端 ──

    #[test]
    fn unterminated_paren_errors() {
        let err = expand_command_subst("$(echo unclosed", SubstQuoting::Unquoted).unwrap_err();
        assert!(matches!(err, CmdSubstError::Unterminated(_)));
    }

    #[test]
    fn unterminated_backtick_errors() {
        let err = expand_command_subst("`echo unclosed", SubstQuoting::Unquoted).unwrap_err();
        assert!(matches!(err, CmdSubstError::Unterminated(_)));
    }

    #[test]
    #[serial]
    fn literal_paren_inside_inner_quote_closes_span_early() {
        // V1 既知限界: span スキャナはクォート非認識のため、内側のクォート文字列に
        // 含まれるリテラル `)` で span が早期クローズする（`$(echo ")` で閉じ、
        // 残りの `")` がサブシェルのパース時に未終端ダブルクォートになる）。
        // panic せず安全にエラー停止することを固定する。
        let err = expand_command_subst("$(echo \")\")", SubstQuoting::Unquoted).unwrap_err();
        assert!(matches!(err, CmdSubstError::Exec(_)));
    }

    // ── 起動失敗 ──

    #[test]
    #[serial]
    fn nonexistent_command_returns_exec_error_without_panic() {
        let err =
            expand_command_subst("$(this_command_does_not_exist_zzz)", SubstQuoting::Unquoted)
                .unwrap_err();
        assert!(matches!(err, CmdSubstError::Exec(_)));
    }

    #[test]
    fn nonzero_exit_returns_exec_error() {
        let err = expand_command_subst("$(false)", SubstQuoting::Unquoted).unwrap_err();
        assert!(matches!(err, CmdSubstError::Exec(_)));
    }

    // ── 深さ超過 ──

    #[test]
    fn depth_guard_blocks_excessive_nesting() {
        // 直接 capture_subshell を MAX_SUBST_DEPTH 回ネストさせ、超過で NestingTooDeep。
        // ガード自体の単体検証（実コマンドは走らせない）。
        let mut guards = Vec::new();
        for _ in 0..MAX_SUBST_DEPTH {
            guards.push(DepthGuard::enter().expect("within limit"));
        }
        assert!(DepthGuard::enter().is_none(), "should exceed limit");
        // ガードを drop すると深さが戻る。
        drop(guards);
        assert!(DepthGuard::enter().is_some(), "depth should reset on drop");
    }

    #[test]
    #[serial]
    fn deep_nesting_runtime_returns_nesting_error() {
        // capture_subshell を実際にネスト超過させ、ネスト超過に起因する失敗を確認する。
        // MAX_SUBST_DEPTH を超えるまで `$(echo ...)` を入れ子にする。
        // 最内で発生した NestingTooDeep は各層の pipeline 経由で Exec にラップされて
        // 伝播するため、最終的なエラー種別は Exec（メッセージにネスト超過を含む）。
        let mut s = String::from("x");
        for _ in 0..(MAX_SUBST_DEPTH + 1) {
            s = format!("$(echo {s})");
        }
        let err = expand_command_subst(&s, SubstQuoting::Unquoted).unwrap_err();
        match err {
            CmdSubstError::NestingTooDeep => {}
            CmdSubstError::Exec(msg) => assert!(
                msg.contains("nested too deep"),
                "expected nesting error to propagate, got: {msg}"
            ),
            other => panic!("expected nesting-related error, got: {other:?}"),
        }
    }

    // ── word_split ヘルパ ──

    #[test]
    fn word_split_helper_behavior() {
        assert_eq!(word_split(""), Vec::<String>::new());
        assert_eq!(word_split("  "), Vec::<String>::new());
        assert_eq!(
            word_split("a\tb\nc"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }
}
