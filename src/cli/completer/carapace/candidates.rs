//! Conversion of parsed carapace values into completion candidates.

use super::parsing::CarapaceExport;
use crate::cli::completer::provider::Candidate;

pub(super) fn convert_export(export: CarapaceExport) -> Option<Vec<Candidate>> {
    if export.values.is_empty() {
        // エラー/空 → フォールスルー（plan.md の決定事項）。
        return None;
    }

    let mut candidates: Vec<Candidate> = export
        .values
        .into_iter()
        .filter(|v| !v.value.is_empty())
        .map(|v| {
            let append_whitespace = !should_suppress_whitespace(&v.value, &export.nospace);
            Candidate {
                value: v.value,
                description: if v.description.is_empty() {
                    None
                } else {
                    Some(v.description)
                },
                append_whitespace,
            }
        })
        .collect();

    candidates.sort_by(|a, b| a.value.cmp(&b.value));
    candidates.dedup_by(|a, b| a.value == b.value);

    if candidates.is_empty() {
        return None;
    }

    Some(candidates)
}

/// 確定後にスペースを追記しないべきかどうかを判定する。
///
/// - `nospace` が `"*"`（全候補で抑制、carapace の慣習的なワイルドカード値）
/// - `nospace` に `value` の最終文字が含まれる（carapace は `nospace` を
///   「この文字で終わる値の後ろにはスペースを入れない」という文字集合として
///   使う。実地検証: ディレクトリ補完では `nospace == "/"` かつ `value` が
///   `subdir/` のように既に `/` で終わる）
/// - `value` が `/` で終わる（ディレクトリ値。上記条件と重複しうるが、
///   `nospace` が空文字列のケースへの安全側フォールバックとして明示的に判定する）
pub(super) fn should_suppress_whitespace(value: &str, nospace: &str) -> bool {
    if nospace == "*" {
        return true;
    }
    if value.ends_with('/') {
        return true;
    }
    if let Some(last) = value.chars().last() {
        if nospace.contains(last) {
            return true;
        }
    }
    false
}
