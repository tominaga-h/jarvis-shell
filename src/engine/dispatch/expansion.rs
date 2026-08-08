//! ディスパッチ前のトークン展開のオーケストレーション。

use crate::engine::expand;

pub(super) fn expand_tokens(
    tokens: Vec<expand::Token>,
    preserve_operators: bool,
) -> Result<Vec<String>, expand::ExpandError> {
    let mut expanded: Vec<String> = Vec::with_capacity(tokens.len());
    for tok in tokens {
        if preserve_operators
            && matches!(
                tok.value.as_str(),
                "|" | ">" | ">>" | "<" | "&&" | "||" | ";"
            )
        {
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
        expanded.extend(expanded_result?);
    }
    Ok(expanded)
}
