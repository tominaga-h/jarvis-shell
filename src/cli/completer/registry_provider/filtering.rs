//! Candidate filtering and flag candidate construction.

use std::collections::HashSet;

use super::super::provider::Candidate;
use super::super::registry::CompletionSpec;

/// 候補列を `value` で重複排除する。
///
/// fish の `complete` は「同じコマンドに対する複数回の `complete` 呼び出し
/// が蓄積される」ドキュメント化された挙動であり、`-a` の値が spec 間で
/// 重なるケース（例: 2 回の `complete -c mycmd -a build` 呼び出し、または
/// フラグ/静的/動的の複数ソースにまたがる同一値）は珍しくない。素朴に
/// 全 spec の候補を連結すると同じ値がメニューに複数行として並んでしまう
/// ため、初出を優先して（順序を保ったまま）以降の同値候補を捨てる。
/// description は初出のものを保持する（要件通り）。
pub(super) fn dedup_candidates(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut seen: HashSet<String> = HashSet::with_capacity(candidates.len());
    let mut deduped = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if seen.insert(candidate.value.clone()) {
            deduped.push(candidate);
        }
    }
    deduped
}

/// `-s`/`-l` からフラグ候補を組み立てる（`partial` に前方一致するもののみ）。
pub(super) fn flag_candidates(specs: &[&CompletionSpec], partial: &str) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for spec in specs {
        for s in &spec.short {
            let value = format!("-{s}");
            if value.starts_with(partial) {
                candidates.push(Candidate {
                    value,
                    description: spec.description.clone(),
                    append_whitespace: true,
                });
            }
        }
        for l in &spec.long {
            let value = format!("--{l}");
            if value.starts_with(partial) {
                candidates.push(Candidate {
                    value,
                    description: spec.description.clone(),
                    append_whitespace: true,
                });
            }
        }
    }
    candidates
}
