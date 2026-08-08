//! Built-in evaluation of completion conditions.

use super::super::context::{CompletionContext, LexToken};
use super::super::registry::CompletionSpec;

/// spec の `-n`（condition）が、この `ctx` の下でアクティブかどうか。
/// `condition` が `None`（未設定）なら常にアクティブ。
pub(super) fn condition_is_active(spec: &CompletionSpec, ctx: &CompletionContext) -> bool {
    match &spec.condition {
        None => true,
        Some(cond) => evaluate_condition(cond, ctx),
    }
}

/// `-n` に設定された条件式を評価する。
///
/// サブプロセスを一切起動しない、組み込みの評価器のみをサポートする:
/// - `__fish_use_subcommand`: ここまでのコマンド単語がヘッドコマンドのみ
///   （= ヘッドコマンドの後ろにサブコマンド相当の単語がまだ無い）場合に
///   `true`。
/// - `__fish_seen_subcommand_from w1 w2 ...`: 挙げられた単語のいずれかが
///   ヘッドコマンドより後ろのコマンド単語列に出現していれば `true`。
///
/// 上記いずれの形式にも一致しない条件式は常に `false`（非アクティブ）を
/// 返す — その spec は `complete` の一覧表示には出るが Tab 補完には
/// 反映されない（README/README_JA に明記する既知の制限）。
fn evaluate_condition(condition: &str, ctx: &CompletionContext) -> bool {
    let trimmed = condition.trim();

    if trimmed == "__fish_use_subcommand" {
        return use_subcommand(ctx);
    }

    if let Some(rest) = trimmed.strip_prefix("__fish_seen_subcommand_from") {
        // "__fish_seen_subcommand_from" の直後が空白または文字列終端であること
        // （"__fish_seen_subcommand_fromXXX" のような偶然の前方一致を除外する）。
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let wanted: Vec<&str> = rest.split_whitespace().collect();
            return seen_subcommand_from(&wanted, ctx);
        }
    }

    false
}

/// ここまでのコマンド単語（partial を除く、ヘッドコマンドより後ろ）に、
/// フラグ（`-` 始まり）以外の単語が 1 つも無ければ `true`。
///
/// 「フラグの後にはまだサブコマンドが来ていない」を表現するため、
/// `-v` のようなオプションはサブコマンド判定から除外する
/// （`cmd -v <Tab>` は依然として use_subcommand = true）。
fn use_subcommand(ctx: &CompletionContext) -> bool {
    let words = confirmed_command_words(ctx);
    !words.iter().skip(1).any(|w| !w.starts_with('-'))
}

/// `wanted` のいずれかが、ヘッドコマンドより後ろの確定済みコマンド単語
/// （partial を除く）に含まれていれば `true`。
fn seen_subcommand_from(wanted: &[&str], ctx: &CompletionContext) -> bool {
    if wanted.is_empty() {
        return false;
    }
    let words = confirmed_command_words(ctx);
    words.iter().skip(1).any(|w| wanted.contains(w))
}

/// リダイレクト演算子（寛容スキャナ/`split_quoted` が単独トークンとして
/// 認識するもの: `<` `>` `>>`）かどうか。
///
/// `engine/expand/quote.rs::operator_prefix_len` の演算子表と同期を保つ
/// （B6）。このテーブルは fd 番号プレフィックス付き（`2>` `&>` 等）を単独
/// トークンとしては扱わない — `2>` は寛容スキャナ上「単語 `2`」+「演算子
/// `>`」の 2 トークンに分かれる（`operator_prefix_len` が 2 文字演算子として
/// 認識するのは `&&` `||` `>>` のみ）。そのため fd 番号自体は本関数の対象外
/// だが、後続のリダイレクト対象語は `>` 単体の直後語として本関数のスキップ
/// 対象に含まれる。
fn is_redirect_operator(op_value: &str) -> bool {
    matches!(op_value, "<" | ">" | ">>")
}

/// `ctx.tokens[skip_from..]` を走査し、演算子トークンとその直後の 1 語
/// （リダイレクト対象語）を除いた単語列を末尾に積む（B6 の中核）。
///
/// `confirmed_command_words` から、`expanded_head` の有無で開始位置だけを
/// 変えて呼び出せるように共通化してある。
fn push_redirect_aware_words<'a>(out: &mut Vec<&'a str>, tokens: &'a [LexToken]) {
    let mut skip_next = false;
    for tok in tokens {
        if skip_next {
            skip_next = false;
            continue;
        }
        if tok.is_operator {
            if is_redirect_operator(&tok.value) {
                skip_next = true;
            }
            continue;
        }
        out.push(tok.value.as_str());
    }
}

/// `ctx.tokens`（または `expanded_head` 適用後）から、末尾の「今まさに
/// 入力中の partial」と、リダイレクト対象語（`>` `>>` `<` の直後の 1 単語）
/// を除いた確定済みの単語列を返す（B6）。
///
/// `ctx.command_words()`（`context.rs`）は演算子トークンそのものは除外する
/// ものの、リダイレクト対象語（例: `mycmd > start.log` の `start.log`）は
/// 普通の単語として残してしまう。これにより `mycmd > start.log <Tab>` が
/// `__fish_use_subcommand` を誤って false にしたり、`start.log` のような
/// 語がたまたま `__fish_seen_subcommand_from` の対象語と一致して誤検知
/// したりする（本 finding の再現条件）。本関数は `ctx.tokens` を直接走査し、
/// 演算子トークンとその直後の 1 語（リダイレクト対象）をともにスキップする。
///
/// `expanded_head`（シェルエイリアス展開）が設定されている場合は、
/// `mod.rs::apply_shell_alias` が組み立てた値（展開後の先頭コマンド語群 +
/// `ctx.tokens[1..]` の非演算子語）をそのまま使うのではなく、展開後の
/// 先頭語群はそのまま採用しつつ、`ctx.tokens[1..]` 側は本関数と同じ
/// リダイレクト対応スキップを再適用する（`apply_shell_alias` 自体は
/// スコープ外のため、ここで同等のスキップを効かせることで整合を取る）。
/// `apply_shell_alias` は演算子を含むエイリアス値そのものは展開しない
/// 設計のため、展開後の先頭語群自体にリダイレクト演算子が混じることはない。
fn confirmed_command_words(ctx: &CompletionContext) -> Vec<&str> {
    let mut words: Vec<&str> = Vec::with_capacity(ctx.tokens.len());

    if let Some(head) = &ctx.expanded_head {
        words.extend(head.iter().map(String::as_str));
        // `apply_shell_alias` は先頭語群の後ろに `ctx.tokens[1..]`
        // （非演算子のみ）を継ぎ足しているため、こちらも同じ範囲を対象に
        // リダイレクト対応スキップを適用する。ここで `head` の語数ではなく
        // 常に `tokens[1..]` を使うのは、`apply_shell_alias` の実装と同じ
        // 前提（先頭トークン 1 個だけを展開元として消費する）に合わせるため。
        if ctx.tokens.len() > 1 {
            push_redirect_aware_words(&mut words, &ctx.tokens[1..]);
        }
    } else {
        push_redirect_aware_words(&mut words, &ctx.tokens);
    }

    // partial（今まさに入力中の末尾トークン）を除く。partial がリダイレクト
    // 対象語の位置にある場合（例: "mycmd > st<Tab>"）は上の走査で既に
    // スキップ済みのため、ここでの pop は二重にはならない（その場合 words
    // の末尾には既に partial は積まれていない）。
    if !ctx.partial.is_empty() && words.last() == Some(&ctx.partial.as_str()) {
        words.pop();
    }

    words
}
