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
}

/// 補完確定時に挿入する値をエスケープする。
///
/// 空白・タブおよび `' " \ | & ; < > ( ) ` (バックタイム) をバックスラッシュで
/// エスケープする。クォートで包む方式を採らないのは、クォートされたトークンが
/// 実行系でチルダ・環境変数展開をスキップしてしまうため（`quote.rs` / `expand.rs`
/// の展開はクォート外のトークンにのみ適用される）。
///
/// 先頭の `~` はエスケープしない（チルダ展開を維持するため）。
pub(crate) fn escape_for_insert(value: &str) -> String {
    const SPECIAL: &[char] = &[
        ' ', '\t', '\'', '"', '\\', '|', '&', ';', '<', '>', '(', ')', '$', '`',
    ];

    let (head, rest) = if let Some(stripped) = value.strip_prefix('~') {
        ("~", stripped)
    } else {
        ("", value)
    };

    let mut out = String::with_capacity(head.len() + rest.len());
    out.push_str(head);
    for ch in rest.chars() {
        if SPECIAL.contains(&ch) {
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
}
