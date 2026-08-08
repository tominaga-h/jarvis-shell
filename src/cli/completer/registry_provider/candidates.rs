//! Static and dynamic candidate construction.

use std::time::{Duration, Instant};

use crate::engine::expand::split_quoted;

use super::super::external::run_external_capped;
use super::super::provider::Candidate;
use super::super::registry::CompletionSpec;

/// 動的候補（`$(...)`）の value として許容する最大バイト数。
///
/// これを超える行は表示上・メモリ上の防御として切り詰める
/// （[`sanitize_dynamic_value`] 参照）。
pub(super) const MAX_DYNAMIC_VALUE_BYTES: usize = 512;

/// 動的候補（`$(...)`）1 spec あたりの最小実行タイムアウト。
///
/// [`static_candidates`] の集約予算が尽きかけていても、少なくともこの時間
/// だけは各動的 spec に与える。あまりに小さい残り予算で spawn しても
/// ほぼ確実に失敗するだけなので、フロアを設けて「1個も試さず全部スキップ」
/// を避けつつ、全体予算を大きく超えないバランスを取る。
const MIN_PER_SPEC_DYNAMIC_TIMEOUT_MS: u64 = 50;

/// `-a` の候補（静的または動的）を展開し、`partial` に前方一致するものを返す。
///
/// 動的候補（`$(...)`）を持つ spec が複数ある場合、[`RegistryProvider::provide`]
/// 1 回の呼び出し全体で 1 個の集約デッドラインを共有する。spec ごとに
/// フルタイムアウトを与えると N 個の hang しうる spec が UI スレッドを
/// N倍ブロックしてしまうため、`dynamic_timeout`（呼び出し全体の予算）を
/// 起点に「残り時間」を都度計算し、各 spec にはその残り時間（下限
/// [`MIN_PER_SPEC_DYNAMIC_TIMEOUT_MS`]）だけを与える。予算を使い切ったら
/// 残りの動的 spec は実行せず即座にスキップする（静的/フラグ処理は untimed
/// のまま — 十分に安価なため）。
pub(super) fn static_candidates(
    specs: &[&CompletionSpec],
    partial: &str,
    dynamic_timeout: Duration,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let deadline = Instant::now() + dynamic_timeout;
    let per_spec_floor = Duration::from_millis(MIN_PER_SPEC_DYNAMIC_TIMEOUT_MS);
    let mut budget_exhausted = false;

    for spec in specs {
        let Some(raw) = &spec.arguments else {
            continue;
        };
        if let Some(inner) = dynamic_source_command(raw) {
            if budget_exhausted {
                continue;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                budget_exhausted = true;
                continue;
            }
            let spec_timeout = remaining.max(per_spec_floor);
            candidates.extend(dynamic_candidates(
                inner,
                spec.description.as_deref(),
                partial,
                spec_timeout,
            ));
            continue;
        }
        for word in split_arguments(raw) {
            if word.starts_with(partial) {
                candidates.push(Candidate {
                    value: word,
                    description: spec.description.clone(),
                    append_whitespace: true,
                });
            }
        }
    }
    candidates
}

/// `raw`（`-a` の生文字列）が前後の空白を除いてちょうど `$(...)` の形なら、
/// 中身のコマンド文字列を返す。
///
/// 「先頭が `$(` かつ末尾が `)`」だけでなく、その `$(` に対応する閉じ括弧が
/// 本当に文字列の末尾であること（内側に別の `$(...)` が混在していても
/// 全体が 1 個の `$(...)` で包まれていること）を括弧の深さで確認する。
/// これにより `$(foo) $(bar)`（2 個の $(...) の並び）のような紛らわしい
/// 入力を「単一の動的ソース」と誤認しない。
///
/// 深さの走査はクォート状態を意識する: シングル/ダブルクォート内の
/// `)` は括弧として数えない。これにより `$(awk '{print ")"}')` のような
/// 「クォートされた `)` が中に含まれる」正当な単一動的ソースを、誤って
/// 「途中で深さ 0 に戻った」= 複数の $(...) の並びと誤認しない。バック
/// スラッシュエスケープされたクォート文字（`\'` `\"`）はクォート状態を
/// 変化させない（シェルの一般的なクォート規則に合わせる）。ダブルクォート
/// はシングルクォートを無効化し、その逆も同様（シェルのネスト不可規則）。
pub(super) fn dynamic_source_command(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let inner = trimmed.strip_prefix("$(")?.strip_suffix(')')?;

    // `inner` 自身の中で括弧の深さを追い、途中でゼロに戻る（= 先頭の `$(`
    // に対応する閉じ括弧が inner の末尾より手前にある）場合は「単一の
    // $(...) で全体を包んでいる」とは言えないため弾く
    // （例: "$(foo) $(bar)" → strip して得た inner は "foo) $(bar" で、
    // 最初の `)` で深さ 0 に戻ってしまう）。クォート内の括弧はカウント
    // 対象外。
    let mut depth = 0i32;
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if in_double || !in_single => {
                // ダブルクォート内、またはクォート外でのバックスラッシュは
                // 次の 1 文字をエスケープとして読み飛ばす（クォート判定を
                // 誤らせないため）。シングルクォート内ではバックスラッシュに
                // 特別な意味は無い（POSIX シェル規則）ので読み飛ばさない。
                chars.next();
            }
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '(' if !in_single && !in_double => depth += 1,
            ')' if !in_single && !in_double => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            _ => {}
        }
    }
    if depth != 0 || in_single || in_double {
        return None;
    }
    Some(inner)
}

/// 動的候補ソースを実行し、`value<TAB>description` 形式の stdout をパースする。
///
/// タイムアウト・非ゼロ終了・spawn 失敗はいずれも空の `Vec`（= この spec
/// からは 0 候補、グレースフルデグレード）。
fn dynamic_candidates(
    inner_command: &str,
    fallback_description: Option<&str>,
    partial: &str,
    timeout: Duration,
) -> Vec<Candidate> {
    let Some(stdout) = run_external_capped(
        std::path::Path::new("/bin/sh"),
        &["-c".to_string(), inner_command.to_string()],
        &[],
        timeout,
    ) else {
        return Vec::new();
    };

    let mut candidates = Vec::new();
    for line in stdout.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let (raw_value, raw_description) = match line.split_once('\t') {
            Some((value, desc)) => (value, Some(desc.to_string())),
            None => (line, fallback_description.map(str::to_string)),
        };

        // ユーザーが任意コマンドを登録できる動的候補ソースの stdout は
        // 信頼できない出力として扱い、reedline に渡す前に必ずサニタイズする
        // （sibling の zsh_bridge の ANSI 除去方針をミラーする — 本モジュールは
        // zsh_bridge の private ヘルパーを再利用できない配置のため、同等の
        // CSI/OSC 除去ロジックをここに複製する）。
        let Some(value) = sanitize_dynamic_value(raw_value) else {
            continue;
        };
        if value.is_empty() || !value.starts_with(partial) {
            continue;
        }
        let description = raw_description.and_then(|d| sanitize_dynamic_value(&d));

        candidates.push(Candidate {
            value,
            description,
            append_whitespace: true,
        });
    }
    candidates
}

/// 動的候補（`$(...)`）の 1 フィールド（value または description）を
/// サニタイズする。
///
/// 1. ANSI エスケープシーケンス（CSI: `ESC [ ... final byte`、OSC:
///    `ESC ] ... (BEL または ESC \)`）を除去する。
/// 2. 除去後になお C0 制御文字（U+0000〜U+001F）または DEL（U+007F）が
///    残っている場合は `None`（そのフィールドを含む候補を丸ごと破棄）。
///    タブ・改行等の生の制御バイトは 1 行プロトコル（`value<TAB>description`）
///    や reedline のメニュー描画を壊しうるため、安全側に倒して破棄する
///    （`zsh_bridge::zsh_escape_span` が「制御文字含みは `None`」とする方針
///    と同じ考え方 — ただしこちらはエスケープ経路を持たないため素直に破棄）。
/// 3. 512 バイトを超える値は切り詰める（暴走した動的ソースが巨大な行を
///    返してもメニュー描画やメモリを圧迫しないための防御）。
pub(super) fn sanitize_dynamic_value(input: &str) -> Option<String> {
    let stripped = strip_ansi_and_osc(input);
    // `char::is_control()` は C0 (U+0000..=U+001F) と DEL (U+007F) の両方を
    // 含む（`zsh_bridge::zsh_escape_span` のドキュメント参照）。
    if stripped.chars().any(char::is_control) {
        return None;
    }
    if stripped.len() <= MAX_DYNAMIC_VALUE_BYTES {
        return Some(stripped);
    }
    let mut truncated = stripped;
    while !truncated.is_char_boundary(MAX_DYNAMIC_VALUE_BYTES) {
        truncated.pop();
    }
    truncated.truncate(MAX_DYNAMIC_VALUE_BYTES);
    Some(truncated)
}

/// ANSI エスケープシーケンス（CSI・OSC）を取り除く。
///
/// [`super::zsh_bridge`] の `strip_ansi`（CSI のみ対応）と同じ考え方だが、
/// 動的候補は任意の外部コマンドの生 stdout であり `\x1b]...\x07`（OSC:
/// 例えばウィンドウタイトル設定等）が混じる可能性もゼロではないため、
/// OSC 終端（BEL `\x07` または ST `ESC \`）も追加で読み飛ばす。
/// `zsh_bridge::strip_ansi` は `fn`（非公開）のためモジュール外から再利用
/// できず、ここに複製する。
fn strip_ansi_and_osc(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next(); // consume '['
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                    continue;
                }
                Some(']') => {
                    chars.next(); // consume ']'
                                  // OSC は BEL (\x07) または ST (ESC \) で終端する。
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next(); // consume '\\'
                            break;
                        }
                    }
                    continue;
                }
                _ => {}
            }
        }
        out.push(ch);
    }
    out
}

/// `-a` の生文字列を単語列に分割する。`split_quoted` を優先し、失敗したら
/// 空白区切りにフォールバックする。
fn split_arguments(raw: &str) -> Vec<String> {
    match split_quoted(raw) {
        Ok(tokens) => tokens.into_iter().map(|t| t.value).collect(),
        Err(_) => raw.split_whitespace().map(str::to_string).collect(),
    }
}
