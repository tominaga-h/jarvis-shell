//! `complete` ビルトインで登録されたユーザー定義補完を提供するプロバイダ
//!
//! [`CompletionRegistry`] を `Arc<RwLock<_>>` で `Shell` と共有し、Tab 補完の
//! ホットパスで読み取る。プロバイダチェーン内の位置は `CommandProvider` の
//! 直後・`GitProvider` の直前（ユーザー登録がビルトインの git 補完や外部
//! 補完より優先される）。
//!
//! 対象コマンドの判定は `ctx.expanded_head`（alias 展開後）があればそれを、
//! なければ先頭トークンをそのまま使う（[`CompletionContext::head_command`]
//! に委譲）。
//!
//! - partial が `-` で始まる場合: 登録済み spec の `-s`/`-l` からフラグ候補を
//!   前方一致で列挙する（`-x` 形式・`--long` 形式の両方、description は
//!   spec の `-d`）。
//! - それ以外: 登録済み spec の `-a`（静的候補の生文字列、または動的候補
//!   `$(...)` — 下記参照）を展開し、前方一致するものを列挙する
//!   （description は同じ spec の `-d`。動的候補は出力側の
//!   `<TAB>description` を優先する — [`dynamic_candidates`] 参照）。
//! - `-n`（condition）が設定されている spec は、[`evaluate_condition`] が
//!   `true` を返した場合のみ候補源として使う。認識できない条件式
//!   （`__fish_use_subcommand` / `__fish_seen_subcommand_from` 以外）を
//!   持つ spec は常に非アクティブ（候補を一切出さない）—
//!   `complete`（一覧表示）には引き続き表示されるが Tab 補完には反映
//!   されない。この制限は README/README_JA に明記する。
//! - 一致件数が 0 件なら `None` を返し、後続プロバイダ（外部補完・パス補完）
//!   にフォールスルーする — このフェーズには `-f`（ファイル補完併用）相当の
//!   機能はなく、ユーザーは `-a` に静的候補を明示登録する必要がある
//!   （ファイル名まで動的に欲しい場合は spec を登録しない選択肢を取る）。
//!
//! # 動的候補（`-a "$(...)"`）
//!
//! `-a` の生文字列が、前後の空白を除いてちょうど `$(...)` の形（`$(` で
//! 始まり `)` で終わる）の場合、静的な単語リストとしてではなく**動的候補
//! ソース**として扱う。中身のコマンドを `/bin/sh -c <inner>` として
//! [`run_external_capped`] 経由で実行し、標準出力を `value<TAB>description`
//! 形式の行としてパースする（タブは最初の 1 個で分割、description は
//! 省略可、空行はスキップ、行末 `\r` は除去）。タイムアウト・非ゼロ終了・
//! spawn 失敗はいずれも「この spec からは 0 候補」として扱う（他の spec は
//! 引き続き有効。全体としてゼロなら `None` にフォールスルーする通常の
//! ルールがそのまま適用される）。
//!
//! **静的候補と動的候補の混在は非サポート**: 1 個の `-a` 文字列は「純粋な
//! 静的単語リスト」か「単一の `$(...)`」のいずれかであり、それ以外
//! （例: `-a 'foo $(bar)'`）は静的文字列としてそのまま扱われる
//! （`$(...)` 部分も含めて 1 語として split される可能性がある — ドキュメント
//! で明記）。

use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::engine::expand::split_quoted;

use super::carapace::ExternalCompletionSettings;
use super::context::{CompletionContext, LexToken};
use super::external::run_external_capped;
use super::provider::{Candidate, CompletionProvider};
use super::registry::{CompletionRegistry, CompletionSpec};

/// 動的候補（`$(...)`）の value として許容する最大バイト数。
///
/// これを超える行は表示上・メモリ上の防御として切り詰める
/// （[`sanitize_dynamic_value`] 参照）。
const MAX_DYNAMIC_VALUE_BYTES: usize = 512;

/// 動的候補（`-a "$(...)"`）実行タイムアウトの下限値。
///
/// `[completion] external_timeout_ms` は 0 や極端に小さい値を設定できて
/// しまうため、Tab 補完のホットパスで実質即タイムアウト（= 常に無効）に
/// なることを避けるための最小フロア。carapace/zsh ブリッジと違い、
/// ユーザー自身が任意のコマンドを登録する機能のため、あまり大きくは
/// 取らず「明らかに短すぎる設定を底上げする」程度に留める。
const MIN_DYNAMIC_TIMEOUT_MS: u64 = 200;

/// ユーザー定義補完（`complete` ビルトイン）プロバイダ。
pub(super) struct RegistryProvider {
    registry: Arc<RwLock<CompletionRegistry>>,
    /// 動的候補（`-a "$(...)"`）実行時のタイムアウト算出に使う共有設定。
    /// `Shell` / carapace / zsh ブリッジと同じ `Arc<RwLock<_>>` 配管
    /// パターン（`external_timeout_ms` の hot-reload にも追従する）。
    external_completion: Arc<RwLock<ExternalCompletionSettings>>,
}

impl RegistryProvider {
    pub(super) fn new(
        registry: Arc<RwLock<CompletionRegistry>>,
        external_completion: Arc<RwLock<ExternalCompletionSettings>>,
    ) -> Self {
        Self {
            registry,
            external_completion,
        }
    }

    /// 動的候補実行の実効タイムアウトを求める（共有設定の値と
    /// [`MIN_DYNAMIC_TIMEOUT_MS`] の大きい方）。設定の読み取りに失敗した
    /// 場合（poisoned lock）はフロア値のみを使う。
    fn dynamic_timeout(&self) -> Duration {
        let floor = Duration::from_millis(MIN_DYNAMIC_TIMEOUT_MS);
        match self.external_completion.read() {
            Ok(settings) => settings.timeout.max(floor),
            Err(_) => floor,
        }
    }
}

impl CompletionProvider for RegistryProvider {
    fn provide(&self, ctx: &CompletionContext) -> Option<Vec<Candidate>> {
        if ctx.is_first_token {
            return None;
        }

        let head = ctx.head_command()?;

        let registry = self.registry.read().ok()?;
        let specs = registry.specs_for(head);
        if specs.is_empty() {
            return None;
        }

        let active_specs: Vec<&CompletionSpec> = specs
            .iter()
            .filter(|spec| condition_is_active(spec, ctx))
            .collect();
        if active_specs.is_empty() {
            return None;
        }

        // '-' 分岐: フラグ候補の後ろに `-a`（静的/動的）候補も連結する（B2）。
        // fish は非ダッシュ分岐では引数のみを出すが、'-' 分岐だけの場合
        // 「フラグに前方一致しない `-a` 語（例: `--custom`）が到達不能」に
        // なる不具合があったため、フラグ候補優先でマージする。非ダッシュ
        // 分岐は従来通り引数のみ（fish parity）。
        let candidates = if ctx.partial.starts_with('-') {
            let mut candidates = flag_candidates(&active_specs, &ctx.partial);
            candidates.extend(static_candidates(
                &active_specs,
                &ctx.partial,
                self.dynamic_timeout(),
            ));
            candidates
        } else {
            static_candidates(&active_specs, &ctx.partial, self.dynamic_timeout())
        };

        let candidates = dedup_candidates(candidates);

        if candidates.is_empty() {
            None
        } else {
            Some(candidates)
        }
    }
}

/// spec の `-n`（condition）が、この `ctx` の下でアクティブかどうか。
/// `condition` が `None`（未設定）なら常にアクティブ。
fn condition_is_active(spec: &CompletionSpec, ctx: &CompletionContext) -> bool {
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

/// 候補列を `value` で重複排除する（B1）。
///
/// fish の `complete` は「同じコマンドに対する複数回の `complete` 呼び出し
/// が蓄積される」ドキュメント化された挙動であり、`-a` の値が spec 間で
/// 重なるケース（例: 2 回の `complete -c mycmd -a build` 呼び出し、または
/// フラグ/静的/動的の複数ソースにまたがる同一値）は珍しくない。素朴に
/// 全 spec の候補を連結すると同じ値がメニューに複数行として並んでしまう
/// ため、初出を優先して（順序を保ったまま）以降の同値候補を捨てる。
/// description は初出のものを保持する（要件通り）。
fn dedup_candidates(candidates: Vec<Candidate>) -> Vec<Candidate> {
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
fn flag_candidates(specs: &[&CompletionSpec], partial: &str) -> Vec<Candidate> {
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

/// 動的候補（`$(...)`）1 spec あたりの最小実行タイムアウト。
///
/// [`static_candidates`] の集約予算が尽きかけていても、少なくともこの時間
/// だけは各動的 spec に与える（B4）。あまりに小さい残り予算で spawn しても
/// ほぼ確実に失敗するだけなので、フロアを設けて「1個も試さず全部スキップ」
/// を避けつつ、全体予算を大きく超えないバランスを取る。
const MIN_PER_SPEC_DYNAMIC_TIMEOUT_MS: u64 = 50;

/// `-a` の候補（静的または動的）を展開し、`partial` に前方一致するものを返す。
///
/// 動的候補（`$(...)`）を持つ spec が複数ある場合、[`RegistryProvider::provide`]
/// 1 回の呼び出し全体で 1 個の集約デッドラインを共有する（B4）。spec ごとに
/// フルタイムアウトを与えると N 個の hang しうる spec が UI スレッドを
/// N倍ブロックしてしまうため、`dynamic_timeout`（呼び出し全体の予算）を
/// 起点に「残り時間」を都度計算し、各 spec にはその残り時間（下限
/// [`MIN_PER_SPEC_DYNAMIC_TIMEOUT_MS`]）だけを与える。予算を使い切ったら
/// 残りの動的 spec は実行せず即座にスキップする（静的/フラグ処理は untimed
/// のまま — 十分に安価なため）。
fn static_candidates(
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
/// 深さの走査はクォート状態を意識する（B5）: シングル/ダブルクォート内の
/// `)` は括弧として数えない。これにより `$(awk '{print ")"}')` のような
/// 「クォートされた `)` が中に含まれる」正当な単一動的ソースを、誤って
/// 「途中で深さ 0 に戻った」= 複数の $(...) の並びと誤認しない。バック
/// スラッシュエスケープされたクォート文字（`\'` `\"`）はクォート状態を
/// 変化させない（シェルの一般的なクォート規則に合わせる）。ダブルクォート
/// はシングルクォートを無効化し、その逆も同様（シェルのネスト不可規則）。
fn dynamic_source_command(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let inner = trimmed.strip_prefix("$(")?.strip_suffix(')')?;

    // `inner` 自身の中で括弧の深さを追い、途中でゼロに戻る（= 先頭の `$(`
    // に対応する閉じ括弧が inner の末尾より手前にある）場合は「単一の
    // $(...) で全体を包んでいる」とは言えないため弾く
    // （例: "$(foo) $(bar)" → strip して得た inner は "foo) $(bar" で、
    // 最初の `)` で深さ 0 に戻ってしまう）。クォート内の括弧はカウント
    // 対象外（B5）。
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

        // B3: ユーザーが任意コマンドを登録できる動的候補ソースの stdout は
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
/// サニタイズする（B3）。
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
fn sanitize_dynamic_value(input: &str) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::super::context::extract_context;
    use super::*;
    use crate::config::CompletionConfig;
    use serial_test::serial;

    fn registry_with(cmd: &str, spec: CompletionSpec) -> Arc<RwLock<CompletionRegistry>> {
        let mut registry = CompletionRegistry::new();
        registry.register(cmd, spec);
        Arc::new(RwLock::new(registry))
    }

    fn default_external_completion() -> Arc<RwLock<ExternalCompletionSettings>> {
        Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig::default(),
        )))
    }

    fn provider_for(cmd: &str, spec: CompletionSpec) -> RegistryProvider {
        RegistryProvider::new(registry_with(cmd, spec), default_external_completion())
    }

    // ── フラグ補完 ──

    #[test]
    fn flag_completion_filters_long_by_prefix() {
        let spec = CompletionSpec {
            long: vec!["verbose".to_string(), "version".to_string()],
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd --v", "mycmd --v".len());
        let candidates = provider.provide(&ctx).expect("should offer flag matches");

        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"--verbose"));
        assert!(values.contains(&"--version"));
    }

    #[test]
    fn flag_completion_bare_dash_offers_both_short_and_long() {
        let spec = CompletionSpec {
            short: vec!["v".to_string()],
            long: vec!["verbose".to_string()],
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd -", "mycmd -".len());
        let candidates = provider.provide(&ctx).expect("should offer flag matches");

        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"-v"));
        assert!(values.contains(&"--verbose"));
    }

    #[test]
    fn flag_completion_carries_description() {
        let spec = CompletionSpec {
            long: vec!["verbose".to_string()],
            description: Some("Verbose output".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd --verb", "mycmd --verb".len());
        let candidates = provider.provide(&ctx).expect("should offer flag matches");

        let verbose = candidates
            .iter()
            .find(|c| c.value == "--verbose")
            .expect("--verbose should be present");
        assert_eq!(verbose.description.as_deref(), Some("Verbose output"));
        assert!(verbose.append_whitespace);
    }

    // ── 静的引数補完 ──

    #[test]
    fn static_arguments_with_descriptions() {
        let spec = CompletionSpec {
            arguments: Some("build test release".to_string()),
            description: Some("subcommand".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd b", "mycmd b".len());
        let candidates = provider.provide(&ctx).expect("should offer static matches");

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].value, "build");
        assert_eq!(candidates[0].description.as_deref(), Some("subcommand"));
    }

    #[test]
    fn static_arguments_quoted_words_are_split_with_split_quoted() {
        let spec = CompletionSpec {
            arguments: Some("'hello world' foo".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd h", "mycmd h".len());
        let candidates = provider.provide(&ctx).expect("should offer static matches");

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].value, "hello world");
    }

    #[test]
    fn static_arguments_falls_back_to_whitespace_split_on_parse_error() {
        // 未閉シングルクォート: split_quoted はエラーになるので空白分割にフォールバック。
        let spec = CompletionSpec {
            arguments: Some("foo 'unterminated bar".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd f", "mycmd f".len());
        let candidates = provider.provide(&ctx).expect("should offer static matches");

        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"foo"));
    }

    // ── ゼロ一致 → None ──

    #[test]
    fn zero_matches_returns_none() {
        let spec = CompletionSpec {
            arguments: Some("build test".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd zzz_no_such_", "mycmd zzz_no_such_".len());
        assert!(provider.provide(&ctx).is_none());
    }

    #[test]
    fn zero_matching_flags_returns_none() {
        let spec = CompletionSpec {
            long: vec!["verbose".to_string()],
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd --zzz_no_such_", "mycmd --zzz_no_such_".len());
        assert!(provider.provide(&ctx).is_none());
    }

    // ── is_first_token → None ──

    #[test]
    fn is_first_token_returns_none() {
        let spec = CompletionSpec {
            arguments: Some("build".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd", "mycmd".len());
        assert!(ctx.is_first_token);
        assert!(provider.provide(&ctx).is_none());
    }

    // ── alias head 解決 ──

    #[test]
    fn alias_expanded_head_resolves_real_command_specs() {
        let spec = CompletionSpec {
            arguments: Some("checkout".to_string()),
            ..Default::default()
        };
        let provider = provider_for("git", spec);

        let mut ctx = extract_context("g c", "g c".len());
        ctx.expanded_head = Some(vec!["git".to_string(), "c".to_string()]);

        let candidates = provider
            .provide(&ctx)
            .expect("expanded_head should resolve to 'git' specs");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].value, "checkout");
    }

    // ── 未登録コマンド → None ──

    #[test]
    fn unknown_command_returns_none() {
        let spec = CompletionSpec {
            arguments: Some("build".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("othercmd b", "othercmd b".len());
        assert!(provider.provide(&ctx).is_none());
    }

    // ── 動的候補: dynamic_source_command 判定 ──

    #[test]
    fn dynamic_source_command_recognizes_wrapped_form() {
        assert_eq!(dynamic_source_command("$(echo hi)"), Some("echo hi"));
        assert_eq!(dynamic_source_command("  $(echo hi)  "), Some("echo hi"));
    }

    #[test]
    fn dynamic_source_command_rejects_static_and_mixed_forms() {
        assert_eq!(dynamic_source_command("foo bar"), None);
        assert_eq!(dynamic_source_command("$(foo) bar"), None);
        assert_eq!(dynamic_source_command("foo $(bar)"), None);
        assert_eq!(dynamic_source_command("$(foo) $(bar)"), None);
    }

    #[test]
    fn dynamic_source_command_supports_nested_parens() {
        assert_eq!(
            dynamic_source_command("$(echo $(echo nested))"),
            Some("echo $(echo nested)")
        );
    }

    // ── 動的候補: 実行系（サブプロセスを spawn するため #[serial]） ──
    //
    // /bin/sh 前提のテストのみ収録。CI 環境で /bin/sh が存在しない場合に
    // 備え、各テストの冒頭で存在チェックし、無ければ skip する。

    fn require_sh() -> bool {
        std::path::Path::new("/bin/sh").exists()
    }

    #[test]
    #[serial]
    fn dynamic_candidates_from_fixture_script_with_descriptions() {
        if !require_sh() {
            eprintln!("skipping: /bin/sh not found");
            return;
        }
        let spec = CompletionSpec {
            arguments: Some(
                "$(printf 'start\\tBegin the thing\\nstop\\tEnd the thing\\n')".to_string(),
            ),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd s", "mycmd s".len());
        let candidates = provider
            .provide(&ctx)
            .expect("dynamic candidates should be offered");

        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"start"));
        assert!(values.contains(&"stop"));

        let start = candidates
            .iter()
            .find(|c| c.value == "start")
            .expect("start present");
        assert_eq!(start.description.as_deref(), Some("Begin the thing"));
    }

    #[test]
    #[serial]
    fn dynamic_candidates_prefix_filtered() {
        if !require_sh() {
            eprintln!("skipping: /bin/sh not found");
            return;
        }
        let spec = CompletionSpec {
            arguments: Some("$(printf 'alpha\\nbeta\\n')".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd a", "mycmd a".len());
        let candidates = provider
            .provide(&ctx)
            .expect("dynamic candidates should be offered");

        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(values, vec!["alpha"]);
    }

    #[test]
    #[serial]
    fn dynamic_candidates_value_only_line_without_tab() {
        if !require_sh() {
            eprintln!("skipping: /bin/sh not found");
            return;
        }
        let spec = CompletionSpec {
            arguments: Some("$(printf 'noDescriptionHere\\n')".to_string()),
            description: Some("fallback description".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd n", "mycmd n".len());
        let candidates = provider
            .provide(&ctx)
            .expect("dynamic candidates should be offered");

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].value, "noDescriptionHere");
        // タブなし行は spec の -d をフォールバック description として使う。
        assert_eq!(
            candidates[0].description.as_deref(),
            Some("fallback description")
        );
    }

    #[test]
    #[serial]
    fn dynamic_candidates_hanging_fixture_falls_through_within_budget() {
        if !require_sh() {
            eprintln!("skipping: /bin/sh not found");
            return;
        }
        let spec = CompletionSpec {
            arguments: Some("$(sleep 5)".to_string()),
            ..Default::default()
        };
        // タイムアウトを明示的に短く設定した provider を直接組み立てる
        // （デフォルトの external_timeout_ms だと最大 400ms + フロア 200ms
        // だが、CI 環境差を考慮してテストでも明示する）。
        let settings = Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig {
                external_timeout_ms: 100,
                ..CompletionConfig::default()
            },
        )));
        let provider = RegistryProvider::new(registry_with("mycmd", spec), settings);

        let start = std::time::Instant::now();
        let ctx = extract_context("mycmd s", "mycmd s".len());
        let result = provider.provide(&ctx);
        let elapsed = start.elapsed();

        assert!(
            result.is_none(),
            "hanging dynamic source should yield zero candidates -> None fall-through"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "should return within the timeout budget, took {elapsed:?}"
        );
    }

    #[test]
    #[serial]
    fn dynamic_candidates_nonzero_exit_is_graceful() {
        if !require_sh() {
            eprintln!("skipping: /bin/sh not found");
            return;
        }
        let spec = CompletionSpec {
            arguments: Some("$(exit 3)".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd s", "mycmd s".len());
        assert!(
            provider.provide(&ctx).is_none(),
            "non-zero exit dynamic source should yield zero candidates -> None fall-through"
        );
    }

    #[test]
    #[serial]
    fn dynamic_candidates_other_static_specs_still_apply_when_dynamic_fails() {
        if !require_sh() {
            eprintln!("skipping: /bin/sh not found");
            return;
        }
        let mut registry = CompletionRegistry::new();
        registry.register(
            "mycmd",
            CompletionSpec {
                arguments: Some("$(exit 3)".to_string()),
                ..Default::default()
            },
        );
        registry.register(
            "mycmd",
            CompletionSpec {
                arguments: Some("static_word".to_string()),
                ..Default::default()
            },
        );
        let provider = RegistryProvider::new(
            Arc::new(RwLock::new(registry)),
            default_external_completion(),
        );

        let ctx = extract_context("mycmd s", "mycmd s".len());
        let candidates = provider
            .provide(&ctx)
            .expect("static spec should still offer candidates");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(values, vec!["static_word"]);
    }

    // ── -n 条件: __fish_use_subcommand ──

    #[test]
    fn use_subcommand_true_right_after_head() {
        let spec = CompletionSpec {
            condition: Some("__fish_use_subcommand".to_string()),
            arguments: Some("start stop".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd s", "mycmd s".len());
        let candidates = provider
            .provide(&ctx)
            .expect("should be active right after head");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"start"));
        assert!(values.contains(&"stop"));
    }

    #[test]
    fn use_subcommand_false_after_subcommand_present() {
        let spec = CompletionSpec {
            condition: Some("__fish_use_subcommand".to_string()),
            arguments: Some("start stop".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd start ", "mycmd start ".len());
        assert!(
            provider.provide(&ctx).is_none(),
            "should be inactive once a subcommand word is present"
        );
    }

    #[test]
    fn use_subcommand_true_with_flag_only_after_head() {
        // フラグ (-v) はサブコマンド判定から除外されるため、依然 true。
        let spec = CompletionSpec {
            condition: Some("__fish_use_subcommand".to_string()),
            arguments: Some("start stop".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd -v s", "mycmd -v s".len());
        let candidates = provider
            .provide(&ctx)
            .expect("flags before the subcommand position should not count as a subcommand");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"start"));
    }

    // ── -n 条件: __fish_seen_subcommand_from ──

    #[test]
    fn seen_subcommand_from_true_when_listed_word_present() {
        let spec = CompletionSpec {
            condition: Some("__fish_seen_subcommand_from start".to_string()),
            arguments: Some("main develop".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd start m", "mycmd start m".len());
        let candidates = provider
            .provide(&ctx)
            .expect("should be active once 'start' has been seen");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"main"));
    }

    #[test]
    fn seen_subcommand_from_false_when_listed_word_absent() {
        let spec = CompletionSpec {
            condition: Some("__fish_seen_subcommand_from start".to_string()),
            arguments: Some("main develop".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd stop m", "mycmd stop m".len());
        assert!(
            provider.provide(&ctx).is_none(),
            "should be inactive when none of the listed subcommands have been seen"
        );
    }

    // ── -n 条件: 未知の条件式は常に非アクティブ ──

    #[test]
    fn unknown_condition_spec_never_offers_candidates() {
        let spec = CompletionSpec {
            condition: Some("some_unsupported_condition".to_string()),
            arguments: Some("build".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd b", "mycmd b".len());
        assert!(
            provider.provide(&ctx).is_none(),
            "unsupported -n condition should keep the spec inactive for completion"
        );
    }

    #[test]
    fn unknown_condition_spec_still_listed_by_complete_registry() {
        // registry.rs レベルでは condition の値に関わらず spec がそのまま
        // 保持・列挙されることの確認（一覧表示は complete.rs 側の責務だが、
        // 「Tab 補完には出ないが登録データとしては残る」ことを本モジュール
        // の境界でも確認しておく）。
        let mut registry = CompletionRegistry::new();
        registry.register(
            "mycmd",
            CompletionSpec {
                condition: Some("some_unsupported_condition".to_string()),
                arguments: Some("build".to_string()),
                ..Default::default()
            },
        );
        let specs = registry.specs_for("mycmd");
        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0].condition.as_deref(),
            Some("some_unsupported_condition")
        );
    }

    // ── 統合: git-branch 風の2段階サブコマンド例（issue #89 3.3 worked example）──

    #[test]
    #[serial]
    fn two_spec_subcommand_example_completes_start_stop_then_dynamic_values() {
        if !require_sh() {
            eprintln!("skipping: /bin/sh not found");
            return;
        }
        let mut registry = CompletionRegistry::new();
        registry.register(
            "mycmd",
            CompletionSpec {
                condition: Some("__fish_use_subcommand".to_string()),
                arguments: Some("start stop".to_string()),
                ..Default::default()
            },
        );
        registry.register(
            "mycmd",
            CompletionSpec {
                condition: Some("__fish_seen_subcommand_from start".to_string()),
                arguments: Some("$(printf 'server\\ndb\\n')".to_string()),
                ..Default::default()
            },
        );
        let provider = RegistryProvider::new(
            Arc::new(RwLock::new(registry)),
            default_external_completion(),
        );

        // 位置1: サブコマンド候補 (start / stop)。
        let ctx = extract_context("mycmd ", "mycmd ".len());
        let candidates = provider.provide(&ctx).expect("subcommand position");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(values, vec!["start", "stop"]);

        // "start" の後: 動的候補 (server / db) のみが有効。
        let ctx2 = extract_context("mycmd start ", "mycmd start ".len());
        let candidates2 = provider
            .provide(&ctx2)
            .expect("dynamic position after start");
        let values2: Vec<&str> = candidates2.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(values2, vec!["server", "db"]);
    }

    // ── B1: 候補の重複排除 ──

    #[test]
    fn dedup_collapses_overlapping_specs_to_single_entry() {
        // ドキュメント化された「累積される複数回の complete 呼び出し」パターン:
        // 同じ値 "build" を持つ 2 個の spec が登録されている。
        let mut registry = CompletionRegistry::new();
        registry.register(
            "mycmd",
            CompletionSpec {
                arguments: Some("build".to_string()),
                description: Some("first".to_string()),
                ..Default::default()
            },
        );
        registry.register(
            "mycmd",
            CompletionSpec {
                arguments: Some("build".to_string()),
                description: Some("second".to_string()),
                ..Default::default()
            },
        );
        let provider = RegistryProvider::new(
            Arc::new(RwLock::new(registry)),
            default_external_completion(),
        );

        let ctx = extract_context("mycmd b", "mycmd b".len());
        let candidates = provider.provide(&ctx).expect("should offer matches");

        assert_eq!(
            candidates.len(),
            1,
            "overlapping specs must collapse into a single menu row: {candidates:?}"
        );
        assert_eq!(candidates[0].value, "build");
        // 初出（1個目の spec）の description を保持する。
        assert_eq!(candidates[0].description.as_deref(), Some("first"));
    }

    // ── B2: '-' 分岐でのフラグ+静的/動的候補のマージ ──

    #[test]
    fn dash_branch_also_offers_matching_static_argument_words() {
        // `-a` に "--custom" のような '-' 始まりの語がある場合、'-' 分岐でも
        // 到達可能でなければならない（flag_candidates だけでは拾えない）。
        let spec = CompletionSpec {
            long: vec!["verbose".to_string()],
            arguments: Some("--custom".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd --c", "mycmd --c".len());
        let candidates = provider
            .provide(&ctx)
            .expect("static argument word should be reachable from the '-' branch");

        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(
            values.contains(&"--custom"),
            "expected --custom reachable in '-' branch, got {values:?}"
        );
    }

    #[test]
    fn dash_branch_merge_is_still_deduplicated() {
        // フラグ候補と静的候補の両方が同じ値 "--verbose" を生成しうる場合でも
        // B1 のデデュープが '-' 分岐のマージ結果にも適用される。
        let spec = CompletionSpec {
            long: vec!["verbose".to_string()],
            arguments: Some("--verbose".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd --v", "mycmd --v".len());
        let candidates = provider.provide(&ctx).expect("should offer matches");

        let matching: Vec<&Candidate> = candidates
            .iter()
            .filter(|c| c.value == "--verbose")
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "duplicate --verbose from flag+static sources must collapse to one row: {candidates:?}"
        );
    }

    #[test]
    fn non_dash_branch_stays_arguments_only() {
        // 非ダッシュ分岐は fish parity のため引数候補のみ（フラグは出さない）。
        let spec = CompletionSpec {
            long: vec!["verbose".to_string()],
            arguments: Some("build".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd b", "mycmd b".len());
        let candidates = provider.provide(&ctx).expect("should offer matches");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(values, vec!["build"]);
    }

    // ── B3: 動的候補のサニタイズ ──

    #[test]
    fn sanitize_dynamic_value_strips_ansi_csi_sequence() {
        let input = "\u{1b}[31mred\u{1b}[0m";
        assert_eq!(sanitize_dynamic_value(input).as_deref(), Some("red"));
    }

    #[test]
    fn sanitize_dynamic_value_strips_osc_sequence() {
        // OSC 8 (ハイパーリンク) 形式: ESC ] 8 ;; URL BEL text ESC ] 8 ;; BEL
        let input = "\u{1b}]8;;http://example.com\u{7}linktext\u{1b}]8;;\u{7}";
        assert_eq!(sanitize_dynamic_value(input).as_deref(), Some("linktext"));
    }

    #[test]
    fn sanitize_dynamic_value_drops_candidate_with_residual_control_char() {
        // ANSI 除去では取り除けない裸の C0 制御文字（ESC 単体、CSI final byte
        // 無しなど）が残る場合は、そのフィールドを含む候補ごと破棄する。
        assert_eq!(sanitize_dynamic_value("bad\u{1}value"), None);
    }

    #[test]
    fn sanitize_dynamic_value_drops_candidate_with_del() {
        assert_eq!(sanitize_dynamic_value("bad\u{7f}value"), None);
    }

    #[test]
    fn sanitize_dynamic_value_truncates_absurdly_long_values() {
        let huge = "a".repeat(MAX_DYNAMIC_VALUE_BYTES + 100);
        let sanitized = sanitize_dynamic_value(&huge).expect("plain ascii should not be dropped");
        assert_eq!(sanitized.len(), MAX_DYNAMIC_VALUE_BYTES);
    }

    #[test]
    #[serial]
    fn dynamic_candidates_ansi_polluted_stdout_is_sanitized() {
        if !require_sh() {
            eprintln!("skipping: /bin/sh not found");
            return;
        }
        let spec = CompletionSpec {
            arguments: Some("$(printf '\\033[32mgreen\\033[0m\\tcolored desc\\n')".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd g", "mycmd g".len());
        let candidates = provider
            .provide(&ctx)
            .expect("dynamic candidates should be offered after sanitization");

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].value, "green");
        assert_eq!(candidates[0].description.as_deref(), Some("colored desc"));
    }

    // ── B4: 動的 spec 全体で 1 個の集約タイムアウト予算 ──

    #[test]
    #[serial]
    fn two_hanging_dynamic_specs_share_a_single_aggregate_budget() {
        if !require_sh() {
            eprintln!("skipping: /bin/sh not found");
            return;
        }
        let mut registry = CompletionRegistry::new();
        registry.register(
            "mycmd",
            CompletionSpec {
                arguments: Some("$(sleep 5)".to_string()),
                ..Default::default()
            },
        );
        registry.register(
            "mycmd",
            CompletionSpec {
                arguments: Some("$(sleep 5)".to_string()),
                ..Default::default()
            },
        );
        // 集約予算 300ms（フロア 200ms 超なのでそのまま使われる）。
        // 直列にフルタイムアウトを与えると 2 * 300ms = 600ms 掛かるはずだが、
        // 集約予算なら 300ms + 若干のオーバーヘッドで収まるはず。
        let settings = Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig {
                external_timeout_ms: 300,
                ..CompletionConfig::default()
            },
        )));
        let provider = RegistryProvider::new(Arc::new(RwLock::new(registry)), settings);

        let start = std::time::Instant::now();
        let ctx = extract_context("mycmd s", "mycmd s".len());
        let result = provider.provide(&ctx);
        let elapsed = start.elapsed();

        assert!(
            result.is_none(),
            "both hanging dynamic sources should yield zero candidates -> None fall-through"
        );
        // 2倍(600ms)には遠く及ばない、1回分の予算 + 十分なエポックのみを許容する。
        assert!(
            elapsed < Duration::from_millis(900),
            "two hanging specs must share ONE aggregate budget, not stack sequentially, took {elapsed:?}"
        );
    }

    // ── B5: dynamic_source_command のクォート対応括弧スキャン ──

    #[test]
    fn dynamic_source_command_accepts_quoted_paren_as_single_source() {
        // シングルクォート内の `)` は括弧としてカウントしない。
        let raw = r#"$(awk '{print ")"}')"#;
        assert_eq!(
            dynamic_source_command(raw),
            Some(r#"awk '{print ")"}'"#),
            "a quoted ')' inside a single $(...) must not be misdetected as unbalanced"
        );
    }

    #[test]
    fn dynamic_source_command_accepts_quoted_paren_in_double_quotes() {
        let raw = r#"$(echo "(")"#;
        assert_eq!(dynamic_source_command(raw), Some(r#"echo "(""#));
    }

    #[test]
    fn dynamic_source_command_still_rejects_two_real_dollar_parens() {
        assert_eq!(dynamic_source_command("$(a) $(b)"), None);
    }

    #[test]
    fn dynamic_source_command_still_rejects_unterminated_quote() {
        // 閉じられていないクォートで終わる場合は不正な形として弾く。
        assert_eq!(dynamic_source_command("$(echo 'unterminated)"), None);
    }

    // ── B6: リダイレクト対象語を条件評価から除外 ──

    #[test]
    fn use_subcommand_true_after_redirect_target() {
        // リダイレクト対象語 (start.log) はサブコマンドとしてカウントしない。
        let spec = CompletionSpec {
            condition: Some("__fish_use_subcommand".to_string()),
            arguments: Some("build test".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd > start.log ", "mycmd > start.log ".len());
        let candidates = provider
            .provide(&ctx)
            .expect("redirect target must not count as a subcommand word");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"build"));
    }

    #[test]
    fn seen_subcommand_from_not_satisfied_by_redirect_target() {
        // "start.log" という語がたまたま監視対象名と一致しても、リダイレクト
        // 対象語である限り __fish_seen_subcommand_from を満たしてはならない。
        let spec = CompletionSpec {
            condition: Some("__fish_seen_subcommand_from start.log".to_string()),
            arguments: Some("build".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd > start.log ", "mycmd > start.log ".len());
        assert!(
            provider.provide(&ctx).is_none(),
            "a redirect target must not satisfy __fish_seen_subcommand_from"
        );
    }

    #[test]
    fn seen_subcommand_from_still_satisfied_by_real_subcommand_after_redirect() {
        // リダイレクトの後ろに来た「本物の」サブコマンド単語は引き続き認識される。
        let spec = CompletionSpec {
            condition: Some("__fish_seen_subcommand_from start".to_string()),
            arguments: Some("server db".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd > out.log start s", "mycmd > out.log start s".len());
        let candidates = provider
            .provide(&ctx)
            .expect("a genuine subcommand after a redirection should still be seen");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"server"));
    }

    #[test]
    fn use_subcommand_true_with_append_redirect_target() {
        // `>>` (追記リダイレクト) の対象語も同様に除外する。
        let spec = CompletionSpec {
            condition: Some("__fish_use_subcommand".to_string()),
            arguments: Some("build".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd >> out.log ", "mycmd >> out.log ".len());
        let candidates = provider
            .provide(&ctx)
            .expect("append-redirect target must not count as a subcommand word");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"build"));
    }

    #[test]
    fn use_subcommand_true_with_input_redirect_target() {
        // `<` (入力リダイレクト) の対象語も同様に除外する。
        let spec = CompletionSpec {
            condition: Some("__fish_use_subcommand".to_string()),
            arguments: Some("build".to_string()),
            ..Default::default()
        };
        let provider = provider_for("mycmd", spec);

        let ctx = extract_context("mycmd < in.txt ", "mycmd < in.txt ".len());
        let candidates = provider
            .provide(&ctx)
            .expect("input-redirect target must not count as a subcommand word");
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"build"));
    }
}
