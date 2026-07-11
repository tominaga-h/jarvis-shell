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

use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::engine::expand::split_quoted;

use super::carapace::ExternalCompletionSettings;
use super::context::CompletionContext;
use super::external::run_external_capped;
use super::provider::{Candidate, CompletionProvider};
use super::registry::{CompletionRegistry, CompletionSpec};

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

        let candidates = if ctx.partial.starts_with('-') {
            flag_candidates(&active_specs, &ctx.partial)
        } else {
            static_candidates(&active_specs, &ctx.partial, self.dynamic_timeout())
        };

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

/// `ctx.command_words()` から、末尾の「今まさに入力中の partial」を除いた
/// 確定済みの単語列を返す。
///
/// `command_words()` は開いている partial トークンをそのまま最終要素として
/// 含む（`context.rs` 参照）ため、条件評価では「まだ確定していない入力
/// 途中の単語」をサブコマンド判定に混ぜないよう取り除く。判定は
/// `ctx.partial` の有無ではなく「最後の要素が ctx.partial と同じ値か」で
/// 行う — trailing space（partial が空文字列）の場合は command_words() に
/// 空文字列は含まれない（`context.rs::command_words` はトークン由来の値
/// のみを積むため）ため、この場合は何も取り除く必要がない。
fn confirmed_command_words(ctx: &CompletionContext) -> Vec<&str> {
    let mut words = ctx.command_words();
    if !ctx.partial.is_empty() && words.last() == Some(&ctx.partial.as_str()) {
        words.pop();
    }
    words
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

/// `-a` の候補（静的または動的）を展開し、`partial` に前方一致するものを返す。
fn static_candidates(
    specs: &[&CompletionSpec],
    partial: &str,
    dynamic_timeout: Duration,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for spec in specs {
        let Some(raw) = &spec.arguments else {
            continue;
        };
        if let Some(inner) = dynamic_source_command(raw) {
            candidates.extend(dynamic_candidates(
                inner,
                spec.description.as_deref(),
                partial,
                dynamic_timeout,
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
fn dynamic_source_command(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let inner = trimmed.strip_prefix("$(")?.strip_suffix(')')?;

    // `inner` 自身の中で括弧の深さを追い、途中でゼロに戻る（= 先頭の `$(`
    // に対応する閉じ括弧が inner の末尾より手前にある）場合は「単一の
    // $(...) で全体を包んでいる」とは言えないため弾く
    // （例: "$(foo) $(bar)" → strip して得た inner は "foo) $(bar" で、
    // 最初の `)` で深さ 0 に戻ってしまう）。
    let mut depth = 0i32;
    for ch in inner.chars() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
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
        let (value, description) = match line.split_once('\t') {
            Some((value, desc)) => (value, Some(desc.to_string())),
            None => (line, fallback_description.map(str::to_string)),
        };
        if value.is_empty() || !value.starts_with(partial) {
            continue;
        }
        candidates.push(Candidate {
            value: value.to_string(),
            description,
            append_whitespace: true,
        });
    }
    candidates
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
}
