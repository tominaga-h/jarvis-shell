use super::binary::{
    gate, is_known_external_value, ExternalCompletionSettings, ExternalKind, ResolvedExternal,
};
use super::candidates::should_suppress_whitespace;
use super::core::CarapaceProvider;
use super::parsing::CarapaceExport;
use crate::cli::completer::context::extract_context;
use crate::cli::completer::format_external_binaries_display;
use crate::cli::completer::format_external_summary;
use crate::cli::completer::provider::{Candidate, CompletionProvider};
use crate::config::CompletionConfig;
use serial_test::serial;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

// ── JSON 固定文字列パーステスト（実機キャプチャ, carapace-bin 1.7.3） ──

const SAMPLE_SINGLE_VALUE: &str = r#"{"version":"v1.13.0","messages":[],"noprefix":"","nospace":"","usage":"","values":[{"value":"main","display":"main","description":"Merge branch 'develop' for release v1.13.3","style":"blue","tag":"local branches"}]}"#;

// `carapace git export git chec` で実機キャプチャした複数値フィクスチャ。
const SAMPLE_MULTI_VALUE: &str = r#"{"version":"v1.13.0","messages":[],"noprefix":"","nospace":"","usage":"","values":[{"value":"check-attr","display":"check-attr","description":"Display gitattributes information","style":"dim green","tag":"low-level helper commands"},{"value":"check-ignore","display":"check-ignore","description":"Debug gitignore / exclude files","style":"dim green","tag":"low-level helper commands"},{"value":"check-mailmap","display":"check-mailmap","description":"Show canonical names and email addresses of contacts","style":"dim green","tag":"low-level helper commands"},{"value":"check-ref-format","display":"check-ref-format","description":"Ensures that a reference name is well formed","style":"dim green","tag":"low-level helper commands"},{"value":"checkout","display":"checkout","description":"Switch branches or restore working tree files","style":"blue","tag":"main commands"},{"value":"checkout-index","display":"checkout-index","description":"Copy files from the index to the working tree","style":"dim yellow","tag":"low-level manipulator commands"}]}"#;

// `carapace git export git log --one` で実機キャプチャ（nospace = "."）。
const SAMPLE_FLAG_NOSPACE_DOT: &str = r#"{"version":"v1.13.0","messages":[],"noprefix":"","nospace":".","usage":"","values":[{"value":"--oneline","display":"--oneline","description":"This is a shorthand for \"--pretty=oneline --abbrev-commit\" used together","tag":"longhand flags"}]}"#;

// `carapace ls export ls` で実機キャプチャ（ディレクトリ値は末尾 '/' 済み、nospace = "/"）。
const SAMPLE_DIR_NOSPACE_SLASH: &str = r#"{"version":"v1.13.0","messages":[],"noprefix":"","nospace":"/","usage":"","values":[{"value":"afile.txt","display":"afile.txt","tag":"files"},{"value":"subdir/","display":"subdir/","style":"blue bold","tag":"files"}]}"#;

#[test]
fn parse_single_value_sample_maps_to_candidate() {
    let export: CarapaceExport = serde_json::from_str(SAMPLE_SINGLE_VALUE).unwrap();
    assert_eq!(export.values.len(), 1);
    assert_eq!(export.values[0].value, "main");
    assert_eq!(
        export.values[0].description,
        "Merge branch 'develop' for release v1.13.3"
    );
    assert_eq!(export.nospace, "");
}

#[test]
fn parse_multi_value_sample_all_present() {
    let export: CarapaceExport = serde_json::from_str(SAMPLE_MULTI_VALUE).unwrap();
    assert_eq!(export.values.len(), 6);
    let values: Vec<&str> = export.values.iter().map(|v| v.value.as_str()).collect();
    assert!(values.contains(&"checkout"));
    assert!(values.contains(&"check-attr"));
}

#[test]
fn empty_description_maps_to_none() {
    let json = r#"{"values":[{"value":"foo","description":""}]}"#;
    let export: CarapaceExport = serde_json::from_str(json).unwrap();
    assert_eq!(export.values[0].description, "");
    // Provider の変換ロジックと同じ判定（description.is_empty() -> None）。
    assert!(export.values[0].description.is_empty());
}

#[test]
fn missing_optional_fields_default_via_serde_default() {
    // messages/usage/style/tag 等が欠けていてもパースが失敗しない。
    let json = r#"{"values":[{"value":"x"}]}"#;
    let export: CarapaceExport = serde_json::from_str(json).unwrap();
    assert_eq!(export.values.len(), 1);
    assert_eq!(export.values[0].value, "x");
    assert_eq!(export.values[0].description, "");
}

#[test]
fn completely_empty_object_parses_to_empty_values() {
    let export: CarapaceExport = serde_json::from_str("{}").unwrap();
    assert!(export.values.is_empty());
    assert_eq!(export.nospace, "");
}

// ── nospace / append_whitespace マッピング ──

#[test]
fn nospace_wildcard_star_suppresses_all() {
    assert!(should_suppress_whitespace("anything", "*"));
}

#[test]
fn nospace_containing_last_char_suppresses() {
    // value の最終文字 ('.') が nospace 文字集合に含まれる場合は抑制される。
    assert!(should_suppress_whitespace("example.", "."));
}

#[test]
fn nospace_not_containing_last_char_does_not_suppress() {
    // 実地検証: SAMPLE_FLAG_NOSPACE_DOT の nospace は "." で value は "--oneline"
    // （末尾は 'e'）なので、このケースでは抑制されない。
    let export: CarapaceExport = serde_json::from_str(SAMPLE_FLAG_NOSPACE_DOT).unwrap();
    let v = &export.values[0];
    assert!(!should_suppress_whitespace(&v.value, &export.nospace));
}

#[test]
fn nospace_empty_string_does_not_suppress_plain_value() {
    assert!(!should_suppress_whitespace("main", ""));
}

#[test]
fn value_ending_in_slash_suppresses_even_with_empty_nospace() {
    assert!(should_suppress_whitespace("subdir/", ""));
}

#[test]
fn dir_sample_slash_suffixed_value_suppresses_whitespace() {
    let export: CarapaceExport = serde_json::from_str(SAMPLE_DIR_NOSPACE_SLASH).unwrap();
    let file = export
        .values
        .iter()
        .find(|v| v.value == "afile.txt")
        .unwrap();
    let dir = export.values.iter().find(|v| v.value == "subdir/").unwrap();
    assert!(!should_suppress_whitespace(&file.value, &export.nospace));
    assert!(should_suppress_whitespace(&dir.value, &export.nospace));
}

// ── dedup ──

#[test]
fn dedup_removes_duplicate_values_across_tags() {
    let json = r#"{"values":[
            {"value":"foo","description":"from tag A"},
            {"value":"foo","description":"from tag B"},
            {"value":"bar","description":""}
        ]}"#;
    let export: CarapaceExport = serde_json::from_str(json).unwrap();
    let mut candidates: Vec<Candidate> = export
        .values
        .into_iter()
        .map(|v| Candidate {
            value: v.value,
            description: if v.description.is_empty() {
                None
            } else {
                Some(v.description)
            },
            append_whitespace: true,
        })
        .collect();
    candidates.sort_by(|a, b| a.value.cmp(&b.value));
    candidates.dedup_by(|a, b| a.value == b.value);

    assert_eq!(candidates.len(), 2, "duplicate 'foo' should be removed");
    let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(values, vec!["bar", "foo"]);
}

// ── provider-contract テスト ──

const CARAPACE_TIMEOUT: Duration = Duration::from_millis(400);

fn settings_with_binary(binary: Option<PathBuf>) -> Arc<RwLock<ExternalCompletionSettings>> {
    let enabled = binary
        .map(|b| {
            vec![ResolvedExternal {
                kind: ExternalKind::Carapace,
                binary: Some(b),
            }]
        })
        .unwrap_or_default();
    Arc::new(RwLock::new(ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: CARAPACE_TIMEOUT,
        enabled,
    }))
}

fn settings_with_binary_and_timeout(
    binary: PathBuf,
    timeout: Duration,
) -> Arc<RwLock<ExternalCompletionSettings>> {
    Arc::new(RwLock::new(ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout,
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Carapace,
            binary: Some(binary),
        }],
    }))
}

fn settings_disabled_with_dangling_binary(
    binary: PathBuf,
    timeout: Duration,
) -> Arc<RwLock<ExternalCompletionSettings>> {
    // enabled が空 = carapace は無効化されている、という状態を意図的に
    // 手組みする（通常の resolve() 経路では到達しないが、provide() 側の
    // 「無効なら enabled に存在しないので None」というガード自体を
    // 検証するために使う）。binary 引数は「誤って spawn されたら
    // 大きな声で失敗する」ためのダミーパスとして受け取るが、この
    // ヘルパー自体は enabled に含めないため使用しない。
    let _ = binary;
    Arc::new(RwLock::new(ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout,
        enabled: Vec::new(),
    }))
}

#[test]
fn provide_returns_none_when_binary_absent() {
    let provider = CarapaceProvider::new(settings_with_binary(None));
    let ctx = extract_context("git checkout ma", "git checkout ma".len());
    assert_eq!(provider.provide(&ctx), None);
}

#[test]
fn provide_disabled_returns_none_without_spawning_even_with_binary_set() {
    // carapace が enabled リストに含まれていない（= 無効化されている）場合、
    // provide() は binary_path() が None を返すことで即座に return する
    // べきで、バイナリを spawn してはならない。存在しないダミーパスを
    // 渡すことで、万一 spawn されれば大きな声で失敗する（Command::spawn
    // が Err を返し、run_external_capped 経由で None にはなるが、この
    // テストの主眼は「enabled に無い時点で早期 return し、そもそも
    // run_external_capped にすら到達しない」ことの確認）。
    let provider = CarapaceProvider::new(settings_disabled_with_dangling_binary(
        PathBuf::from("/no/such/carapace/binary/would-fail-loudly"),
        CARAPACE_TIMEOUT,
    ));
    let ctx = extract_context("git checkout ma", "git checkout ma".len());
    assert_eq!(provider.provide(&ctx), None);
}

#[test]
fn provide_returns_none_when_only_zsh_is_enabled() {
    // enabled に zsh のみが含まれ carapace が含まれない場合、
    // CarapaceProvider::binary_path(Carapace) は None を返し provide() は
    // 早期 return する（他プロバイダの設定に巻き込まれてはならない）。
    let settings = Arc::new(RwLock::new(ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: CARAPACE_TIMEOUT,
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Zsh,
            binary: Some(PathBuf::from("/bin/zsh")),
        }],
    }));
    let provider = CarapaceProvider::new(settings);
    let ctx = extract_context("git checkout ma", "git checkout ma".len());
    assert_eq!(provider.provide(&ctx), None);
}

#[test]
fn provide_returns_none_on_timeout_and_returns_quickly() {
    // タイムアウト経路の統合的な検証: 実際に遅いスクリプトを spawn させ、
    // 短い timeout で provide() が None を返しつつ、timeout を大幅に
    // 超えず速やかに戻ることを確認する（run_external_capped 自体の
    // タイムアウト・kill ロジックは external.rs 側で検証済みのため、
    // ここでは CarapaceProvider::provide() がその結果を正しく素通し
    // することのみを見る）。
    let tmpdir = tempfile::tempdir().unwrap();
    let script_path = tmpdir.path().join("slow-fake-carapace.sh");
    std::fs::write(&script_path, "#!/bin/sh\nsleep 2\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).unwrap();
    }

    let short_timeout = Duration::from_millis(100);
    let provider =
        CarapaceProvider::new(settings_with_binary_and_timeout(script_path, short_timeout));
    let ctx = extract_context("git checkout ma", "git checkout ma".len());

    let start = std::time::Instant::now();
    let result = provider.provide(&ctx);
    let elapsed = start.elapsed();

    assert_eq!(result, None, "slow external binary should time out to None");
    assert!(
        elapsed < Duration::from_secs(1),
        "provide() should return well under 1s on timeout, took {elapsed:?}"
    );
}

#[test]
fn provide_returns_none_for_first_token_even_with_binary_present() {
    // バイナリが存在する体で構築するが、実際の実行は起きない
    // (is_first_token で早期 return するはず)。存在しないダミーパスでも
    // 先頭トークン判定の方が先に効くことを確認する。
    let provider = CarapaceProvider::new(settings_with_binary(Some(PathBuf::from(
        "/no/such/carapace/binary",
    ))));
    let ctx = extract_context("gi", "gi".len());
    assert!(ctx.is_first_token);
    assert_eq!(provider.provide(&ctx), None);
}

#[test]
fn provide_returns_none_for_cd_even_when_binary_would_emit_files() {
    // 防御的ガードの証明: たとえ carapace（または将来の
    // ブリッジ実装）が cd 用の spec を持ち、ファイル + ディレクトリ混在の
    // JSON を返す状況になっても、CarapaceProvider は cd を一切担当せず
    // 即座に None を返す（PathProvider の dirs_only フィルタに完全に
    // 委譲する）。これを実証するため、実行されれば file + dir 混在の
    // 合成 JSON フィクスチャを吐く偽 carapace スクリプトを用意し、
    // ガードがスクリプト起動より先に効いて None を返すことを確認する
    // （スクリプトが実際に実行されていれば stdout パース経由で
    // 候補が返り、このテストは失敗するはず）。
    let tmpdir = tempfile::tempdir().unwrap();
    let script_path = tmpdir.path().join("fake-carapace-cd.sh");
    let fixture_json = r#"{"version":"v1.13.0","messages":[],"noprefix":"","nospace":"/","usage":"","values":[{"value":"readme.txt","display":"readme.txt","tag":"files"},{"value":"subdir/","display":"subdir/","tag":"files"}]}"#;
    std::fs::write(
        &script_path,
        format!("#!/bin/sh\ncat <<'EOF'\n{fixture_json}\nEOF\n"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).unwrap();
    }

    let provider = CarapaceProvider::new(settings_with_binary(Some(script_path)));
    let ctx = extract_context("cd sub", "cd sub".len());
    assert_eq!(ctx.head_command(), Some("cd"));

    assert_eq!(
        provider.provide(&ctx),
        None,
        "CarapaceProvider must defer cd entirely to PathProvider's dirs_only filter, \
             even when the (would-be) carapace output mixes files and dirs"
    );
}

#[test]
fn provide_returns_none_when_spans_too_short() {
    // spans().len() < 2 は対象外（防御的ガード）。通常の extract_context
    // 経路では !is_first_token のとき spans は必ず 2 要素以上になるため
    // (command_words() が非空 + partial)、このガードへは通常到達しない。
    // ここでは境界を直接検証するため、CompletionContext を手組みして
    // spans() が 1 要素になる状況を人工的に作る。
    let provider = CarapaceProvider::new(settings_with_binary(Some(PathBuf::from(
        "/no/such/carapace/binary",
    ))));

    let mut ctx = extract_context("git", "git".len());
    assert!(ctx.is_first_token);
    // is_first_token を強制的に false にしても spans() は変わらず1要素のまま。
    ctx.is_first_token = false;
    assert_eq!(ctx.spans(), vec!["git"]);

    assert_eq!(provider.provide(&ctx), None);
}

// ── is_responsible (perf/completion-latency) ──
//
// provide() の冒頭ガードと同じ条件を、実際に外部プロセスを起動せず
// 判定できることを検証する。

/// carapace は spec を持たないコマンドで正常終了しつつ空を返すため、
/// 「責任者だが失敗した」と申告してはならない（常に false）。
///
/// これを true にしていた実装では、carapace が spec を持たない
/// コマンド（tmuxinator 等）で `dispatch_providers` がチェーンを
/// 打ち切り、後段の zsh ブリッジが呼ばれずに候補ゼロ（実機の
/// 「NO RECORDS FOUND」）になっていた。その回帰テスト。
#[test]
fn is_responsible_is_always_false_so_the_chain_can_reach_the_zsh_bridge() {
    let provider = CarapaceProvider::new(settings_with_binary(Some(PathBuf::from(
        "/no/such/carapace/binary",
    ))));

    // 「carapace が対象になりうる」典型的なケースでも false を返す。
    for line in ["git checkout ma", "tmuxinator ", "docker run "] {
        let ctx = extract_context(line, line.len());
        assert!(
            !provider.is_responsible(&ctx),
            "carapace must never claim final responsibility ({line:?}) — \
                 the zsh bridge sits behind it and can still answer"
        );
    }
}

#[test]
fn is_responsible_false_when_binary_absent() {
    let provider = CarapaceProvider::new(settings_with_binary(None));
    let ctx = extract_context("git checkout ma", "git checkout ma".len());
    assert!(!provider.is_responsible(&ctx));
}

#[test]
fn is_responsible_false_for_first_token() {
    let provider = CarapaceProvider::new(settings_with_binary(Some(PathBuf::from(
        "/no/such/carapace/binary",
    ))));
    let ctx = extract_context("gi", "gi".len());
    assert!(ctx.is_first_token);
    assert!(!provider.is_responsible(&ctx));
}

#[test]
fn is_responsible_false_for_cd() {
    // cd は常に PathProvider の dirs_only 判定に譲る（provide() と
    // 同じガード）。carapace は cd について「対象外」を宣言するため、
    // cd の通常のディレクトリ補完は影響を受けない。
    let provider = CarapaceProvider::new(settings_with_binary(Some(PathBuf::from(
        "/no/such/carapace/binary",
    ))));
    let ctx = extract_context("cd sub", "cd sub".len());
    assert_eq!(ctx.head_command(), Some("cd"));
    assert!(!provider.is_responsible(&ctx));
}

#[test]
fn is_responsible_false_when_spans_too_short() {
    let provider = CarapaceProvider::new(settings_with_binary(Some(PathBuf::from(
        "/no/such/carapace/binary",
    ))));
    let mut ctx = extract_context("git", "git".len());
    ctx.is_first_token = false;
    assert_eq!(ctx.spans(), vec!["git"]);
    assert!(!provider.is_responsible(&ctx));
}

#[test]
fn is_responsible_matches_provide_none_reason_for_disabled_kind() {
    // carapace が enabled リストに含まれていない場合、provide() も
    // is_responsible() も揃って false/None を返す（判定基準の単一の
    // 情報源であることの確認）。
    let settings = Arc::new(RwLock::new(ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: CARAPACE_TIMEOUT,
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Zsh,
            binary: Some(PathBuf::from("/bin/zsh")),
        }],
    }));
    let provider = CarapaceProvider::new(settings);
    let ctx = extract_context("git checkout ma", "git checkout ma".len());
    assert_eq!(provider.provide(&ctx), None);
    assert!(!provider.is_responsible(&ctx));
}

// ── ExternalCompletionSettings::resolve ──

use crate::config::ExternalSetting;

fn config_with_external(external: ExternalSetting) -> CompletionConfig {
    CompletionConfig {
        external,
        ..CompletionConfig::default()
    }
}

#[test]
fn resolve_auto_string_with_carapace_installed_detects_binary() {
    let Ok(_) = which::which("carapace") else {
        eprintln!("skipping: carapace not installed");
        return;
    };
    let config = config_with_external(ExternalSetting::Single("auto".to_string()));
    let settings = ExternalCompletionSettings::resolve(&config);
    assert!(settings.binary_path(ExternalKind::Carapace).is_some());
}

#[test]
fn resolve_none_string_never_detects_any_binary_even_when_installed() {
    let config = config_with_external(ExternalSetting::Single("none".to_string()));
    let settings = ExternalCompletionSettings::resolve(&config);
    assert!(settings.enabled.is_empty());
    assert!(settings.binary_path(ExternalKind::Carapace).is_none());
    assert!(settings.binary_path(ExternalKind::Zsh).is_none());
}

#[test]
fn resolve_unknown_string_falls_back_to_auto_order() {
    let config = config_with_external(ExternalSetting::Single("bogus".to_string()));
    let settings = ExternalCompletionSettings::resolve(&config);
    // auto と同じ解決になるはず: 有効化されたプロバイダは carapace → zsh の
    // 優先順のうち実機に存在するものだけ。
    let auto_settings = ExternalCompletionSettings::resolve(&config_with_external(
        ExternalSetting::Single("auto".to_string()),
    ));
    let kinds: Vec<ExternalKind> = settings.enabled.iter().map(|e| e.kind).collect();
    let auto_kinds: Vec<ExternalKind> = auto_settings.enabled.iter().map(|e| e.kind).collect();
    assert_eq!(kinds, auto_kinds);
}

#[test]
fn resolve_carapace_string_only_targets_carapace() {
    let config = config_with_external(ExternalSetting::Single("carapace".to_string()));
    let settings = ExternalCompletionSettings::resolve(&config);
    assert_eq!(settings.enabled.len(), 1);
    assert_eq!(settings.enabled[0].kind, ExternalKind::Carapace);
}

#[test]
fn resolve_zsh_string_only_targets_zsh() {
    let config = config_with_external(ExternalSetting::Single("zsh".to_string()));
    let settings = ExternalCompletionSettings::resolve(&config);
    assert_eq!(settings.enabled.len(), 1);
    assert_eq!(settings.enabled[0].kind, ExternalKind::Zsh);
}

#[test]
fn resolve_array_form_preserves_explicit_order() {
    let config = config_with_external(ExternalSetting::List(vec![
        "zsh".to_string(),
        "carapace".to_string(),
    ]));
    let settings = ExternalCompletionSettings::resolve(&config);
    let kinds: Vec<ExternalKind> = settings.enabled.iter().map(|e| e.kind).collect();
    assert_eq!(kinds, vec![ExternalKind::Zsh, ExternalKind::Carapace]);
}

#[test]
fn resolve_array_form_single_entry_only_enables_that_kind() {
    let config = config_with_external(ExternalSetting::List(vec!["zsh".to_string()]));
    let settings = ExternalCompletionSettings::resolve(&config);
    let kinds: Vec<ExternalKind> = settings.enabled.iter().map(|e| e.kind).collect();
    assert_eq!(kinds, vec![ExternalKind::Zsh]);
}

#[test]
fn resolve_array_form_invalid_entry_is_skipped_others_kept() {
    // 不正な要素 ("bogus") は警告のうえスキップされ、有効な要素だけが残る。
    let config = config_with_external(ExternalSetting::List(vec![
        "zsh".to_string(),
        "bogus".to_string(),
        "carapace".to_string(),
    ]));
    let settings = ExternalCompletionSettings::resolve(&config);
    let kinds: Vec<ExternalKind> = settings.enabled.iter().map(|e| e.kind).collect();
    assert_eq!(kinds, vec![ExternalKind::Zsh, ExternalKind::Carapace]);
}

#[test]
fn resolve_array_form_all_invalid_entries_yields_empty() {
    let config = config_with_external(ExternalSetting::List(vec![
        "auto".to_string(),
        "none".to_string(),
    ]));
    let settings = ExternalCompletionSettings::resolve(&config);
    assert!(
        settings.enabled.is_empty(),
        "array form only accepts \"carapace\"/\"zsh\" entries; \
             \"auto\"/\"none\" inside an array should be skipped entirely"
    );
}

// ── ExternalKind::as_str / Display ──

#[test]
fn external_kind_as_str_matches_config_toml_values() {
    assert_eq!(ExternalKind::Carapace.as_str(), "carapace");
    assert_eq!(ExternalKind::Zsh.as_str(), "zsh");
}

#[test]
fn external_kind_display_matches_as_str() {
    assert_eq!(ExternalKind::Carapace.to_string(), "carapace");
    assert_eq!(ExternalKind::Zsh.to_string(), "zsh");
}

#[test]
fn external_kind_all_order_is_carapace_then_zsh() {
    // "auto" 解決順の前提（carapace の方が起動コストが低いため先）。
    assert_eq!(
        ExternalKind::ALL,
        [ExternalKind::Carapace, ExternalKind::Zsh]
    );
}

// ── binary_path ──

#[test]
fn binary_path_returns_none_for_kind_not_in_enabled_list() {
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Carapace,
            binary: Some(PathBuf::from("/usr/local/bin/carapace")),
        }],
    };
    assert!(settings.binary_path(ExternalKind::Zsh).is_none());
}

#[test]
fn binary_path_returns_none_when_entry_present_but_binary_not_found() {
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Carapace,
            binary: None,
        }],
    };
    assert!(settings.binary_path(ExternalKind::Carapace).is_none());
}

#[test]
fn binary_path_returns_path_when_enabled_and_detected() {
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Zsh,
            binary: Some(PathBuf::from("/bin/zsh")),
        }],
    };
    assert_eq!(
        settings.binary_path(ExternalKind::Zsh),
        Some(&PathBuf::from("/bin/zsh"))
    );
}

#[test]
fn should_run_zsh_daemon_true_when_flag_on_and_zsh_enabled() {
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Zsh,
            binary: Some(PathBuf::from("/bin/zsh")),
        }],
    };
    assert!(settings.should_run_zsh_daemon());
}

#[test]
fn should_run_zsh_daemon_false_when_flag_off_even_if_zsh_enabled() {
    // external_zsh_daemon = false のとき、zsh 自体は enabled-kinds
    // リストに残っていても稼働禁止でなければならない。
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: false,
        timeout: Duration::from_millis(400),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Zsh,
            binary: Some(PathBuf::from("/bin/zsh")),
        }],
    };
    assert!(!settings.should_run_zsh_daemon());
}

#[test]
fn should_run_zsh_daemon_false_when_zsh_not_in_enabled_kinds() {
    // フラグは on のままでも、zsh が優先順リストから外れていれば
    // （例: external = "carapace"）稼働禁止。
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Carapace,
            binary: Some(PathBuf::from("/usr/local/bin/carapace")),
        }],
    };
    assert!(!settings.should_run_zsh_daemon());
}

#[test]
fn should_run_zsh_daemon_false_when_enabled_list_is_empty() {
    // external = "none" 相当。
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: Vec::new(),
    };
    assert!(!settings.should_run_zsh_daemon());
}

#[test]
fn should_run_zsh_daemon_true_even_if_zsh_binary_not_detected() {
    // zsh が enabled-kinds に存在する限り、バイナリ未検出でも
    // should_run_zsh_daemon 自体は true を返す（バイナリ有無の判定は
    // gate()/binary_path() の責務であり、should_run_zsh_daemon は
    // 「zsh が優先順に候補として載っているか」だけを見る）。
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Zsh,
            binary: None,
        }],
    };
    assert!(settings.should_run_zsh_daemon());
}

// ── gate（carapace / zsh ブリッジ共通の read-lock/有効化/timeout ゲート）──
//
// 以前は同じ手順が CarapaceProvider::provide と
// ZshBridgeProvider::provide にコピペされ、MIN_TIMEOUT_MS フロアの
// 有無で drift していた。ここでは共有ヘルパー自体の契約
// （無効化 kind -> None、フロアは Some のときのみ適用）を直接検証する。

#[test]
fn gate_returns_none_when_kind_disabled() {
    // enabled リストが空（= 全プロバイダ無効化）なら、どの kind を
    // 指定しても None。
    let settings = Arc::new(RwLock::new(ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: Vec::new(),
    }));
    assert_eq!(gate(&settings, ExternalKind::Carapace, None), None);
    assert_eq!(gate(&settings, ExternalKind::Zsh, None), None);
}

#[test]
fn gate_returns_none_when_kind_not_in_enabled_list() {
    // enabled に別の kind (carapace) だけがある状態で zsh を問い合わせると None。
    let settings = Arc::new(RwLock::new(ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Carapace,
            binary: Some(PathBuf::from("/usr/local/bin/carapace")),
        }],
    }));
    assert_eq!(gate(&settings, ExternalKind::Zsh, None), None);
}

#[test]
fn gate_returns_none_when_binary_not_detected() {
    // エントリはあるが binary が None（明示指定したのに未検出のケース）。
    let settings = Arc::new(RwLock::new(ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Carapace,
            binary: None,
        }],
    }));
    assert_eq!(gate(&settings, ExternalKind::Carapace, None), None);
}

#[test]
fn gate_without_floor_uses_configured_timeout_verbatim() {
    // min_timeout = None のとき、設定 timeout がどれだけ短くてもそのまま使う
    // （carapace の実際の呼び出し方: フロアなし）。
    let settings = Arc::new(RwLock::new(ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(50),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Carapace,
            binary: Some(PathBuf::from("/usr/local/bin/carapace")),
        }],
    }));
    let (binary, timeout) = gate(&settings, ExternalKind::Carapace, None).unwrap();
    assert_eq!(binary, PathBuf::from("/usr/local/bin/carapace"));
    assert_eq!(
        timeout,
        Duration::from_millis(50),
        "without a floor, the configured timeout must be used verbatim even if very short"
    );
}

#[test]
fn gate_with_floor_raises_timeout_below_floor() {
    // min_timeout = Some(floor) かつ設定 timeout がそれ未満のとき、
    // floor まで引き上げられる（zsh ブリッジの実際の呼び出し方）。
    let settings = Arc::new(RwLock::new(ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(50),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Zsh,
            binary: Some(PathBuf::from("/bin/zsh")),
        }],
    }));
    let floor = Duration::from_millis(2000);
    let (binary, timeout) = gate(&settings, ExternalKind::Zsh, Some(floor)).unwrap();
    assert_eq!(binary, PathBuf::from("/bin/zsh"));
    assert_eq!(
        timeout, floor,
        "configured timeout below the floor must be raised to the floor"
    );
}

#[test]
fn gate_with_floor_does_not_lower_timeout_above_floor() {
    // 設定 timeout が floor を上回るときは floor に切り下げない（max の
    // 意味論をそのまま検証する）。
    let settings = Arc::new(RwLock::new(ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(5000),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Zsh,
            binary: Some(PathBuf::from("/bin/zsh")),
        }],
    }));
    let floor = Duration::from_millis(2000);
    let (_binary, timeout) = gate(&settings, ExternalKind::Zsh, Some(floor)).unwrap();
    assert_eq!(
        timeout,
        Duration::from_millis(5000),
        "configured timeout above the floor must not be lowered to the floor"
    );
}

// ── format_external_summary（`source` サマリーの external: 行）──
//
// `Shell` は構築せず、`ExternalCompletionSettings` を直接組み立てて
// 純粋関数のみを検証する。

#[test]
fn format_external_summary_known_single_value_shows_resolved_kind_only() {
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Carapace,
            binary: Some(PathBuf::from("/usr/local/bin/carapace")),
        }],
    };
    assert_eq!(format_external_summary("carapace", &settings), "carapace");
}

#[test]
fn format_external_summary_known_auto_value_shows_resolved_order_without_fallback_marker() {
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: vec![
            ResolvedExternal {
                kind: ExternalKind::Carapace,
                binary: Some(PathBuf::from("/usr/local/bin/carapace")),
            },
            ResolvedExternal {
                kind: ExternalKind::Zsh,
                binary: Some(PathBuf::from("/bin/zsh")),
            },
        ],
    };
    let out = format_external_summary("auto", &settings);
    assert_eq!(out, "carapace, zsh");
    assert!(!out.contains("未対応"));
}

#[test]
fn format_external_summary_none_value_shows_none() {
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: Vec::new(),
    };
    assert_eq!(format_external_summary("none", &settings), "none");
}

#[test]
fn format_external_summary_known_array_value_shows_resolved_order() {
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: vec![
            ResolvedExternal {
                kind: ExternalKind::Zsh,
                binary: Some(PathBuf::from("/bin/zsh")),
            },
            ResolvedExternal {
                kind: ExternalKind::Carapace,
                binary: Some(PathBuf::from("/usr/local/bin/carapace")),
            },
        ],
    };
    let raw = ExternalSetting::List(vec!["zsh".to_string(), "carapace".to_string()]);
    let out = format_external_summary(&raw.to_string(), &settings);
    assert_eq!(out, "zsh, carapace");
    assert!(!out.contains("未対応"));
}

#[test]
fn format_external_summary_unknown_value_shows_fallback_marker_with_raw_value() {
    // resolve() は未知の値を auto として解決する。ここでは carapace のみ
    // 検出された想定の settings を手組みして検証する。
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Carapace,
            binary: Some(PathBuf::from("/usr/local/bin/carapace")),
        }],
    };
    let out = format_external_summary("bogus", &settings);
    assert!(
        out.contains("carapace"),
        "fallback summary should mention the resolved order: {out:?}"
    );
    assert!(
        out.contains("bogus"),
        "fallback summary should mention the raw unknown value: {out:?}"
    );
    assert!(
        out.contains("未対応"),
        "fallback summary should carry a visible fallback marker: {out:?}"
    );
    assert!(
        out.contains("auto"),
        "fallback summary should mention that auto was used: {out:?}"
    );
}

#[test]
fn format_external_summary_unknown_value_end_to_end_via_resolve() {
    // resolve() が実際に fallback した結果を format_external_summary に
    // 渡す統合的な確認（raw と settings の食い違いを実際の呼び出し経路で検証）。
    let config = config_with_external(ExternalSetting::Single("typo-value".to_string()));
    let settings = ExternalCompletionSettings::resolve(&config);
    let out = format_external_summary(&config.external.to_string(), &settings);
    assert!(out.contains("typo-value"));
    assert!(out.contains("未対応"));
}

// ── format_external_binaries_display（`source` サマリーのバイナリパス一覧行）──
//
// `Shell::reload_config` にインラインで組み立てられていた
// ロジックを切り出した純粋関数。`Shell` を構築せずに
// `ExternalCompletionSettings` だけで検証する。

#[test]
fn format_external_binaries_display_empty_when_no_providers_enabled() {
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: Vec::new(),
    };
    assert_eq!(format_external_binaries_display(&settings), "");
}

#[test]
fn format_external_binaries_display_shows_binary_path_for_detected_entry() {
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Carapace,
            binary: Some(PathBuf::from("/usr/local/bin/carapace")),
        }],
    };
    let out = format_external_binaries_display(&settings);
    assert_eq!(out, "    carapace: /usr/local/bin/carapace\n");
}

#[test]
fn format_external_binaries_display_shows_not_found_for_missing_binary() {
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Zsh,
            binary: None,
        }],
    };
    let out = format_external_binaries_display(&settings);
    assert_eq!(out, "    zsh: not found\n");
}

#[test]
fn format_external_binaries_display_lists_multiple_entries_in_order() {
    let settings = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        enabled: vec![
            ResolvedExternal {
                kind: ExternalKind::Carapace,
                binary: Some(PathBuf::from("/usr/local/bin/carapace")),
            },
            ResolvedExternal {
                kind: ExternalKind::Zsh,
                binary: None,
            },
        ],
    };
    let out = format_external_binaries_display(&settings);
    assert_eq!(
        out,
        "    carapace: /usr/local/bin/carapace\n    zsh: not found\n"
    );
}

#[test]
#[serial]
fn resolve_carapace_string_missing_binary_disables_without_panic() {
    // PATH に無いことを保証するため、空の PATH で解決する。
    let original_path = std::env::var("PATH").ok();
    // SAFETY: テスト単体プロセス内で一時的に環境変数を書き換える。
    // 他のテストと並行実行されると PATH 汚染で誤検知しうるため #[serial] を付与。
    unsafe {
        std::env::set_var("PATH", "");
    }

    let config = config_with_external(ExternalSetting::Single("carapace".to_string()));
    let settings = ExternalCompletionSettings::resolve(&config);

    unsafe {
        match original_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
    }

    // エントリ自体は残り（「明示指定したのに無効」であることが可視化される）、
    // バイナリだけが未検出になる。
    assert_eq!(settings.enabled.len(), 1);
    assert_eq!(settings.enabled[0].kind, ExternalKind::Carapace);
    assert!(settings.enabled[0].binary.is_none());
    assert!(settings.binary_path(ExternalKind::Carapace).is_none());
}

#[test]
fn resolve_timeout_converts_millis_to_duration() {
    let config = CompletionConfig {
        external: ExternalSetting::Single("none".to_string()),
        external_timeout_ms: 1234,
        ..CompletionConfig::default()
    };
    let settings = ExternalCompletionSettings::resolve(&config);
    assert_eq!(settings.timeout, Duration::from_millis(1234));
}

// ── is_known_external_value ──

#[test]
fn is_known_external_value_recognizes_scalar_keywords() {
    assert!(is_known_external_value("auto"));
    assert!(is_known_external_value("carapace"));
    assert!(is_known_external_value("zsh"));
    assert!(is_known_external_value("none"));
}

#[test]
fn is_known_external_value_recognizes_valid_array_display() {
    let raw = ExternalSetting::List(vec!["zsh".to_string(), "carapace".to_string()]);
    assert!(is_known_external_value(&raw.to_string()));
}

#[test]
fn is_known_external_value_rejects_unknown_scalar() {
    assert!(!is_known_external_value("bogus"));
}

#[test]
fn is_known_external_value_rejects_array_with_invalid_entry() {
    let raw = ExternalSetting::List(vec!["zsh".to_string(), "bogus".to_string()]);
    assert!(!is_known_external_value(&raw.to_string()));
}

#[test]
fn is_known_external_value_rejects_empty_array() {
    assert!(!is_known_external_value("[]"));
}

// ── hot-reload 伝播（`Shell::reload_config` の書き込み経路を模擬） ──
//
// 完全な `Shell` は構築せず、`Shell::new` / `reload_config` が行うのと
// 同じ `Arc<RwLock<ExternalCompletionSettings>>` の生成・書き換えのみを
// 直接シミュレートする（git_branch_commands の hot-reload テストと同じ方針）。

#[test]
fn reload_write_path_updates_shared_settings_timeout_and_enabled() {
    let initial = ExternalCompletionSettings::resolve(&config_with_external(
        ExternalSetting::Single("none".to_string()),
    ));
    let initial = ExternalCompletionSettings {
        zsh_daemon_enabled: true,
        timeout: Duration::from_millis(400),
        ..initial
    };
    let shared = Arc::new(RwLock::new(initial));

    // `Shell::reload_config` と同じ書き込み経路: 新しい config から再解決し、
    // 書き込みロックで丸ごと置き換える。
    let reloaded_config = CompletionConfig {
        external: ExternalSetting::Single("none".to_string()),
        external_timeout_ms: 900,
        ..CompletionConfig::default()
    };
    let resolved = ExternalCompletionSettings::resolve(&reloaded_config);
    {
        let mut guard = shared.write().unwrap();
        *guard = resolved;
    }

    let after = shared.read().unwrap();
    assert!(after.enabled.is_empty());
    assert_eq!(after.timeout, Duration::from_millis(900));
}

#[test]
fn reload_write_path_installing_carapace_mid_session_enables_it() {
    // 「セッション中に carapace をインストールしてから source する」ケースの
    // 模擬: 最初は external = "none" 相当（binary なし）で開始し、reload 後に
    // "auto"（実機に carapace があれば検出される）へ切り替える。
    let Ok(expected_binary) = which::which("carapace") else {
        eprintln!("skipping: carapace not installed");
        return;
    };

    let initial = ExternalCompletionSettings::resolve(&config_with_external(
        ExternalSetting::Single("none".to_string()),
    ));
    let shared = Arc::new(RwLock::new(initial));
    assert!(
        shared
            .read()
            .unwrap()
            .binary_path(ExternalKind::Carapace)
            .is_none(),
        "external = \"none\" should never resolve a binary"
    );

    let resolved = ExternalCompletionSettings::resolve(&config_with_external(
        ExternalSetting::Single("auto".to_string()),
    ));
    {
        let mut guard = shared.write().unwrap();
        *guard = resolved;
    }

    let after = shared.read().unwrap();
    assert_eq!(
        after.binary_path(ExternalKind::Carapace),
        Some(&expected_binary),
        "reload with external = \"auto\" should re-detect the now-installed carapace binary"
    );
}

#[test]
#[serial]
fn reload_write_path_provider_sees_reload_via_same_shared_arc() {
    // reload_write_path_installing_carapace_mid_session_enables_it は
    // 「共有 settings が更新されること」までを検証する。このテストは
    // さらに一歩進め、その*同じ* Arc から構築した CarapaceProvider が
    // provide() 呼び出しごとに settings を読み直していることを証明する
    // （construction-time キャッシュではなく per-call read であることの
    // 直接証拠）。CarapaceProvider::new 呼び出しは reload の**前**に
    // 一度だけ行い、reload 後に同じ provider インスタンスへ provide()
    // することで、Provider 構築後の設定変更が反映されることを示す。
    let Ok(_) = which::which("carapace") else {
        eprintln!("skipping: carapace not installed");
        return;
    };

    let tmpdir = create_test_git_repo();
    let original_dir = env::current_dir().unwrap();
    env::set_current_dir(tmpdir.path()).unwrap();

    // reload 前: external = "none" 相当（binary なし）で settings を構築し、
    // provider は reload の前に一度だけ、この Arc から構築する。
    let initial = ExternalCompletionSettings::resolve(&config_with_external(
        ExternalSetting::Single("none".to_string()),
    ));
    let shared = Arc::new(RwLock::new(initial));
    let provider = CarapaceProvider::new(Arc::clone(&shared));

    let ctx = extract_context("git checkout test-", "git checkout test-".len());
    let before_reload = provider.provide(&ctx);
    assert_eq!(
        before_reload, None,
        "before reload (external = \"none\"), provider should not produce candidates"
    );

    // reload: `Shell::reload_config` と同じ書き込み経路で、同じ Arc の
    // 中身を "auto"（carapace 検出込み）へ丸ごと置き換える。
    //
    // このテストは reload 後に**実際に carapace を spawn する**ため、
    // `integration_settings()` と同じ理由でタイムアウトを 5s に広げる:
    // 本番既定の 400ms は、`cargo test --all-targets` で host が飽和した
    // 状態では carapace のコールドスタートに足りず、`provide()` が正しく
    // `None` に縮退した結果テストだけが落ちる（負荷起因のフレークであり
    // 製品・テストロジックのバグではない）。ここでの検証対象は
    // 「同じ Arc 経由で reload 後の設定が読み直されること」であって
    // 応答速度ではないので、緩めても検出力は落ちない。
    let mut resolved = ExternalCompletionSettings::resolve(&config_with_external(
        ExternalSetting::Single("auto".to_string()),
    ));
    resolved.timeout = Duration::from_secs(5);
    {
        let mut guard = shared.write().unwrap();
        *guard = resolved;
    }

    // provider インスタンス自体は再構築していない。同じインスタンスへの
    // 呼び出しが reload 後の設定を拾えていれば、construction-time
    // キャッシュではなく per-call read である証拠になる。
    let after_reload = provider.provide(&ctx);

    env::set_current_dir(&original_dir).unwrap();

    let candidates =
        after_reload.expect("provider should produce candidates after mid-session reload");
    let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
    assert!(
        values.contains(&"test-feature"),
        "reloaded provider should suggest 'test-feature' via the same shared Arc: {values:?}"
    );
}

// ── 統合テスト（実行時 skip: which carapace が失敗する環境では skip） ──

/// `[completion] external = "auto"` 相当の実 detect で settings を構築する。
/// carapace を実 spawn する統合テスト専用の設定。
///
/// 本番既定は 400ms タイムアウトだが、`cargo test --all-targets`（1000+
/// テストの並列実行、host が飽和して 1 回の run が 130s 超になる）では
/// carapace のコールドスタート子プロセスが 400ms を超えて `provide()` が
/// `None` を返し、統合テストが稀に fail する（純粋な負荷起因のフレークで
/// あり、製品・テストロジックのバグではない）。統合テストでは carapace が
/// 候補を返せることの検証が本質であり、応答速度そのものは対象外なので、
/// 負荷に強い余裕あるタイムアウト（5s）を明示して起動時間のばらつきを
/// 吸収する。
fn integration_settings() -> Arc<RwLock<ExternalCompletionSettings>> {
    let mut settings = ExternalCompletionSettings::resolve(&CompletionConfig::default());
    settings.timeout = Duration::from_secs(5);
    Arc::new(RwLock::new(settings))
}

fn create_test_git_repo() -> tempfile::TempDir {
    let tmpdir = tempfile::tempdir().unwrap();
    crate::cli::completer::test_git::init_repo_with_test_feature_branch(tmpdir.path());
    tmpdir
}

#[test]
#[serial]
fn integration_git_checkout_branch_prefix_includes_branch() {
    let Ok(_) = which::which("carapace") else {
        eprintln!("skipping: carapace not installed");
        return;
    };

    let tmpdir = create_test_git_repo();
    let original_dir = env::current_dir().unwrap();
    env::set_current_dir(tmpdir.path()).unwrap();

    let provider = CarapaceProvider::new(integration_settings());
    let ctx = extract_context("git checkout test-", "git checkout test-".len());
    let result = provider.provide(&ctx);

    env::set_current_dir(&original_dir).unwrap();

    let candidates = result.expect("carapace should produce candidates for git checkout");
    let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
    assert!(
        values.contains(&"test-feature"),
        "carapace git checkout completion should include 'test-feature': {values:?}"
    );
}

#[test]
#[serial]
fn integration_git_log_oneline_flag_completion() {
    let Ok(_) = which::which("carapace") else {
        eprintln!("skipping: carapace not installed");
        return;
    };

    let provider = CarapaceProvider::new(integration_settings());
    let ctx = extract_context("git log --one", "git log --one".len());
    let result = provider.provide(&ctx);

    let candidates = result.expect("carapace should produce candidates for git log --one");
    let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
    assert!(
        values.iter().any(|v| v.starts_with("--one")),
        "carapace git log completion should offer a --one* flag: {values:?}"
    );
}
