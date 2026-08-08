use std::sync::{Arc, RwLock};
use std::time::Duration;

use super::super::carapace::ExternalCompletionSettings;
use super::super::context::extract_context;
use super::super::provider::{Candidate, CompletionProvider};
use super::super::registry::{CompletionRegistry, CompletionSpec};
use super::candidates::{dynamic_source_command, sanitize_dynamic_value, MAX_DYNAMIC_VALUE_BYTES};
use super::core::RegistryProvider;
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

//
// `condition_is_active` によるフィルタは `provide()` の冒頭で
// `active_specs` を求める際に一度だけ適用され、フラグ候補
// （`flag_candidates`）・引数候補（`static_candidates`）の両方の
// 候補源に共通して効く。上の `seen_subcommand_from_*` テストは
// 非ダッシュ分岐（引数候補）でのゲートしか検証していないため、
// ここでは `-s v` のみを持つ spec が '-' 分岐でも同じ条件でゲート
// されることを直接証明する。

#[test]
fn seen_subcommand_from_gates_flag_branch_when_condition_unmet() {
    let spec = CompletionSpec {
        condition: Some("__fish_seen_subcommand_from start".to_string()),
        short: vec!["v".to_string()],
        ..Default::default()
    };
    let provider = provider_for("cmd", spec);

    // "start" をまだ見ていない状態で "cmd -<Tab>" — フラグ分岐でも
    // 条件が満たされていないため何も出さない。
    let ctx = extract_context("cmd -", "cmd -".len());
    assert!(
        provider.provide(&ctx).is_none(),
        "-n unmet should gate flag-branch candidates too, not just argument candidates"
    );
}

#[test]
fn seen_subcommand_from_gates_flag_branch_when_condition_met() {
    let spec = CompletionSpec {
        condition: Some("__fish_seen_subcommand_from start".to_string()),
        short: vec!["v".to_string()],
        ..Default::default()
    };
    let provider = provider_for("cmd", spec);

    // "start" を見た後の "cmd start -<Tab>" — フラグ分岐が有効になり
    // -v が候補に出る。
    let ctx = extract_context("cmd start -", "cmd start -".len());
    let candidates = provider
        .provide(&ctx)
        .expect("-n met should activate the flag-branch spec");
    let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(values, vec!["-v"]);
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
    // デデュープが '-' 分岐のマージ結果にも適用される。
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
    // 集約予算 1000ms（フロア 200ms 超なのでそのまま使われる）。
    // 直列にフルタイムアウトを与えると 2 * 1000ms = 2000ms 掛かるはずだが、
    // 集約予算なら 1000ms + 若干のオーバーヘッドで収まるはず。
    //
    // 予算 300ms / 上限 900ms だったが、これは 2 回分の
    // fork+exec+kill に 600ms しか余裕がなく、負荷の高いランナーでは
    // フレークしていた。予算を上げて「直列なら 2000ms・集約なら 1000ms」
    // という差を広げることで、判別力（集約かどうか）を落とさずに
    // オーバーヘッド耐性だけを増やす。
    let settings = Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
        &CompletionConfig {
            external_timeout_ms: 1000,
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
    // 2倍(2000ms)には遠く及ばない、1回分の予算(1000ms) + 十分なエポックのみを許容する。
    assert!(
        elapsed < Duration::from_millis(1600),
        "two hanging specs must share ONE aggregate budget, not stack sequentially, took {elapsed:?}"
    );
}

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
