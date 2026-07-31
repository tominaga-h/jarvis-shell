//! `CompletionProvider` トレイト — 補完源をプラグイン化する共通契約
//!
//! 各補完源（コマンド名・git ブランチ・パス等）は `CompletionProvider` を
//! 実装し、[`super::context::CompletionContext`] を受け取って候補を返す。
//!
//! 契約:
//! - `None` = 「このプロバイダの対象ではない、次のプロバイダへ」
//! - `Some(vec![])` = 「このプロバイダが担当したが候補なし」（フォールバックしない）
//!
//! Span（補完確定時に置き換える raw バイト範囲）は `Candidate` には持たせない。
//! orchestrator（`mod.rs`）が `ctx.span` から一括で `Suggestion` を組み立てる。
//!
//! ## `is_responsible` — 「対象外」と「担当したが失敗」の区別（perf/completion-latency）
//!
//! 上記の `None` の意味論には長らく問題があった: carapace / zsh ブリッジの
//! ような外部補完プロバイダは、「このコマンドは自分の対象外」の場合も
//! 「対象のコマンドだがタイムアウト/実行エラーで結果を得られなかった」場合も
//! 区別なく `None` を返していた。orchestrator（`mod.rs`）の `find_map` は
//! `None` を「次のプロバイダへ」としか解釈できないため、後者のケースでも
//! 最終的に `PathProvider`（常に `Some` を返す終端フォールバック）まで
//! 落ちてしまい、ユーザーには無関係なファイルパス候補が一瞬表示されたのち
//! （タイムアウト明けの再 Tab で）正しい候補に「切り替わる」という体験に
//! なっていた（fish shell の調査で判明した反面教師: fish は Tab を完全に
//! 同期実行し、待ってでも一度で正しい結果だけを見せる。詳細は `mod.rs` の
//! モジュールドキュメント参照）。
//!
//! `is_responsible` はこの区別を明示的な問い合わせとして切り出したもの。
//! 既定実装は `false`（「担当外」判定を持たないプロバイダは今までどおり
//! 単純な `None` フォールスルーのみで扱われる — 既存プロバイダの契約は
//! 一切変更しない）。外部補完プロバイダ（`CarapaceProvider` /
//! `ZshBridgeProvider`）だけがこれをオーバーライドし、「もし実行すれば
//! 自分が担当するはずか」を実際の外部プロセス起動なしに安価に判定する
//! （`provide()` 冒頭のガード条件 — バイナリ検出済み・先頭トークンでない・
//! `cd` でない・spans 十分、と同じ条件を流用）。
//!
//! orchestrator は `provide()` が `None` を返したプロバイダそれぞれについて
//! `is_responsible(ctx)` を確認し、一つでも true だったら「対象コマンドの
//! 責任者がいたが結果を出せなかった」と判定して `PathProvider` への
//! フォールスルーを抑止する（＝候補なしを返す）。「waiting は許容するが
//! 誤った結果を見せない」という設計判断（タスク背景参照）をトレイトの
//! 語彙として表現したもの。

use super::context::CompletionContext;

/// 補完候補 1 件。
///
/// `value` はクォート・エスケープを剥がした「生の」値を持つ。
/// 挿入時のエスケープ（[`escape_for_insert`]）は orchestrator が一括で行う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    /// 補完候補の値（raw, unescaped）。
    pub value: String,
    /// 候補の説明文（ColumnarMenu の description 列に表示される）。
    pub description: Option<String>,
    /// 確定後にスペースを追記するかどうか（ディレクトリ末尾などは false）。
    pub append_whitespace: bool,
}

/// 補完源プラグインの共通契約。
pub(crate) trait CompletionProvider: Send {
    /// `ctx` に対する補完候補を返す。
    ///
    /// `None` はこのプロバイダの担当外を意味し、orchestrator は次のプロバイダを試す。
    /// `Some(vec![])` は担当したが候補なしを意味し、以降のプロバイダは試さない。
    fn provide(&self, ctx: &CompletionContext) -> Option<Vec<Candidate>>;

    /// `ctx` に対してこのプロバイダが**責任を持つ**かどうかを、実際に
    /// `provide()` を実行せずに安価に判定する。
    ///
    /// 既定値は `false`。「担当外 (`None`) = パス補完へフォールスルーして
    /// 構わない」という従来どおりの単純な意味論のプロバイダはこれを
    /// オーバーライドする必要がない。外部補完プロバイダ（carapace / zsh
    /// ブリッジ）のように「対象コマンドなのに失敗した場合はパス補完へ
    /// フォールスルーしてはならない」という強い契約を持つプロバイダのみ
    /// オーバーライドする（モジュールドキュメントの `is_responsible` 節参照）。
    fn is_responsible(&self, ctx: &CompletionContext) -> bool {
        let _ = ctx;
        false
    }
}

/// 補完確定時に挿入する値をエスケープする。
///
/// Unicode 空白（`char::is_whitespace()` が true を返す全文字。U+00A0 NBSP 等を
/// 含む）および `' " \ | & ; < > ( ) ` (バックタイム) をバックスラッシュで
/// エスケープする。クォートで包む方式を採らないのは、クォートされたトークンが
/// 実行系でチルダ・環境変数展開をスキップしてしまうため（`quote.rs` / `expand.rs`
/// の展開はクォート外のトークンにのみ適用される）。
///
/// Unicode 空白全体をエスケープ対象にしているのは、このエスケープが
/// round-trip すべき相手（`context.rs` の `lex_lenient` と
/// `engine/expand/quote.rs` の `split_quoted`）がどちらも
/// `char::is_whitespace()` でトークン境界を判定しているため。ASCII の
/// 空白・タブだけをエスケープすると、値に U+00A0 のような非 ASCII 空白を
/// 含む候補を挿入した際に再レックスで 2 トークンに分裂してしまう。
///
/// 先頭の `~` はエスケープしない（チルダ展開を維持するため）。
pub(crate) fn escape_for_insert(value: &str) -> String {
    const SPECIAL: &[char] = &['\'', '"', '\\', '|', '&', ';', '<', '>', '(', ')', '$', '`'];

    let (head, rest) = if let Some(stripped) = value.strip_prefix('~') {
        ("~", stripped)
    } else {
        ("", value)
    };

    let mut out = String::with_capacity(head.len() + rest.len());
    out.push_str(head);
    for ch in rest.chars() {
        if ch.is_whitespace() || SPECIAL.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_for_insert_no_special_chars_unchanged() {
        assert_eq!(escape_for_insert("readme.txt"), "readme.txt");
    }

    #[test]
    fn escape_for_insert_space_is_escaped() {
        assert_eq!(escape_for_insert("foo bar.txt"), r"foo\ bar.txt");
    }

    #[test]
    fn escape_for_insert_tab_is_escaped() {
        assert_eq!(escape_for_insert("foo\tbar"), "foo\\\tbar");
    }

    #[test]
    fn escape_for_insert_all_special_chars() {
        let input = "a b'c\"d\\e|f&g;h<i>j(k)l$m`n";
        let expected = r#"a\ b\'c\"d\\e\|f\&g\;h\<i\>j\(k\)l\$m\`n"#;
        assert_eq!(escape_for_insert(input), expected);
    }

    #[test]
    fn escape_for_insert_leading_tilde_untouched() {
        assert_eq!(escape_for_insert("~/Documents"), "~/Documents");
    }

    #[test]
    fn escape_for_insert_leading_tilde_with_special_in_rest() {
        assert_eq!(escape_for_insert("~/foo bar"), r"~/foo\ bar");
    }

    #[test]
    fn escape_for_insert_tilde_not_at_start_is_escaped() {
        // 先頭以外の `~` は特別扱いされない（SPECIAL に含まれないのでそのまま）。
        assert_eq!(escape_for_insert("a~b"), "a~b");
    }

    #[test]
    fn escape_for_insert_empty_string() {
        assert_eq!(escape_for_insert(""), "");
    }

    #[test]
    fn escape_for_insert_ascii_space_round_trips_via_lenient_scanner() {
        use super::super::context::extract_context;

        let escaped = escape_for_insert("a b.txt");
        let line = format!("echo {escaped}");
        let ctx = extract_context(&line, line.len());

        assert_eq!(
            ctx.tokens.len(),
            2,
            "escaped ASCII-space candidate should re-lex as a single trailing token: {:?}",
            ctx.tokens
        );
        assert_eq!(ctx.tokens[1].value, "a b.txt");
    }

    #[test]
    fn escape_for_insert_unicode_nbsp_round_trips_via_lenient_scanner() {
        use super::super::context::extract_context;

        let value = "a\u{00A0}b.txt";
        let escaped = escape_for_insert(value);
        // NBSP は SPECIAL に含まれないが is_whitespace() では true になるため
        // バックスラッシュエスケープされているはず。
        assert_eq!(escaped, "a\\\u{00A0}b.txt");

        let line = format!("echo {escaped}");
        let ctx = extract_context(&line, line.len());

        assert_eq!(
            ctx.tokens.len(),
            2,
            "escaped NBSP candidate should re-lex as a single trailing token: {:?}",
            ctx.tokens
        );
        assert_eq!(
            ctx.tokens[1].value, value,
            "round-tripped token value should equal the original candidate value"
        );
    }
}
