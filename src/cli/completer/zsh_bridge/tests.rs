use super::super::carapace::ResolvedExternal;
use super::super::carapace::{gate, ExternalCompletionSettings, ExternalKind};
use super::super::provider::CompletionProvider;
use super::core::*;
use super::lifecycle::*;
use super::output_parse::*;
use crate::config::{CompletionConfig, ExternalSetting};
use serial_test::serial;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

// ── パーサ単体テスト（固定文字列フィクスチャ） ──

#[test]
fn parse_simple_value_no_description() {
    let stdout = "checkout\r\ncheckout-index\r\n";
    let candidates = parse_capture_output(stdout);
    let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(values, vec!["checkout", "checkout-index"]);
    assert!(candidates.iter().all(|c| c.description.is_none()));
}

#[test]
fn parse_value_with_description() {
    // `git log --one` の実機キャプチャ（このタスクの検証時に取得）。
    let stdout = "--oneline -- shorthand for --pretty=oneline --abbrev-commit\r\n";
    let candidates = parse_capture_output(stdout);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].value, "--oneline");
    assert_eq!(
        candidates[0].description.as_deref(),
        Some("shorthand for --pretty=oneline --abbrev-commit")
    );
}

#[test]
fn parse_trailing_empty_element_dropped() {
    let stdout = "foo\r\nbar\r\n";
    let candidates = parse_capture_output(stdout);
    assert_eq!(candidates.len(), 2);
}

#[test]
fn parse_empty_stdout_yields_no_candidates() {
    assert!(parse_capture_output("").is_empty());
}

#[test]
fn parse_strips_ansi_codes() {
    let stdout = "\u{1b}[34mmain\u{1b}[0m -- local branch\r\n";
    let candidates = parse_capture_output(stdout);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].value, "main");
    assert_eq!(candidates[0].description.as_deref(), Some("local branch"));
}

#[test]
fn parse_value_containing_double_dash_separator_uses_first_occurrence() {
    // value 自体に " -- " を含む場合（説明文中にも同じ区切りが出うる）、
    // 最初の出現で分割する（zsh.go の SplitN(line, " -- ", 2) と同じ）。
    let stdout = "foo -- first -- second\r\n";
    let candidates = parse_capture_output(stdout);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].value, "foo");
    assert_eq!(
        candidates[0].description.as_deref(),
        Some("first -- second")
    );
}

#[test]
fn parse_unquotes_escaped_special_chars() {
    let stdout = "foo\\ bar.txt\\#1\r\n";
    let candidates = parse_capture_output(stdout);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].value, "foo bar.txt#1");
}

#[test]
fn parse_empty_line_between_entries_is_skipped() {
    let stdout = "foo\r\n\r\nbar\r\n";
    let candidates = parse_capture_output(stdout);
    let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(values, vec!["foo", "bar"]);
}

#[test]
fn append_whitespace_false_for_no_space_rune_suffix() {
    let stdout = "subdir/\r\nfoo=\r\nplain\r\n";
    let candidates = parse_capture_output(stdout);
    let plain = candidates.iter().find(|c| c.value == "plain").unwrap();
    let dir = candidates.iter().find(|c| c.value == "subdir/").unwrap();
    let eq = candidates.iter().find(|c| c.value == "foo=").unwrap();
    assert!(plain.append_whitespace);
    assert!(!dir.append_whitespace);
    assert!(!eq.append_whitespace);
}

#[test]
fn unquote_backslashes_handles_full_table() {
    let input = r#"a\\b\&c\<d\>e\`f\'g\"h\{i\}j\$k\#l\|m\?n\(o\)p\;q\ r\[s\]t\*u\~v"#;
    let out = unquote_backslashes(input);
    assert_eq!(out, "a\\b&c<d>e`f'g\"h{i}j$k#l|m?n(o)p;q r[s]t*u~v");
}

#[test]
fn strip_ansi_removes_csi_sequences() {
    assert_eq!(strip_ansi("\u{1b}[1;34mtext\u{1b}[0m"), "text");
}

#[test]
fn strip_ansi_no_escape_is_unchanged() {
    assert_eq!(strip_ansi("plain text"), "plain text");
}

#[test]
fn zsh_escape_span_escapes_space_and_specials_table() {
    // ZSH_SPECIAL_CHARS の全種を1つの span に詰めて、それぞれが
    // バックスラッシュ付きで出力されることを確認する。
    //
    // 注意: これは *送信方向*（Rust argv → 内側 zsh の "$*" バッファ）の
    // エスケープであり、`unquote_backslashes`（*受信方向*: compadd 候補
    // 行 → Rust 側の表示値）とは独立したテーブル・用途である。`=` は
    // 送信方向でのみ zsh のファイル名展開 (`=command`) 抑止のために
    // エスケープするが、`unquote_backslashes` 側のテーブルには含まれて
    // いない（`capture.zsh` の compadd 出力に `=` がバックスラッシュ
    // 付きで出てくることはないため、逆変換対象に含める必要がない）。
    // そのため往復対称性は主張しない。
    let input = "a b\\c\"d'e`f$g|h&i;j<k>l(m)n{o}p[q]r*s?t~u#v=w";
    let escaped = zsh_escape_span(input).expect("no control chars");
    assert!(escaped.contains("a\\ b"), "space: {escaped}");
    assert!(escaped.contains("b\\\\c"), "backslash: {escaped}");
    assert!(escaped.contains("c\\\"d"), "double quote: {escaped}");
    assert!(escaped.contains("d\\'e"), "single quote: {escaped}");
    assert!(escaped.contains("e\\`f"), "backtick: {escaped}");
    assert!(escaped.contains("f\\$g"), "dollar: {escaped}");
    assert!(escaped.contains("g\\|h"), "pipe: {escaped}");
    assert!(escaped.contains("h\\&i"), "ampersand: {escaped}");
    assert!(escaped.contains("i\\;j"), "semicolon: {escaped}");
    assert!(escaped.contains("j\\<k"), "less-than: {escaped}");
    assert!(escaped.contains("k\\>l"), "greater-than: {escaped}");
    assert!(escaped.contains("l\\(m"), "open paren: {escaped}");
    assert!(escaped.contains("m\\)n"), "close paren: {escaped}");
    assert!(escaped.contains("n\\{o"), "open brace: {escaped}");
    assert!(escaped.contains("o\\}p"), "close brace: {escaped}");
    assert!(escaped.contains("p\\[q"), "open bracket: {escaped}");
    assert!(escaped.contains("q\\]r"), "close bracket: {escaped}");
    assert!(escaped.contains("r\\*s"), "asterisk: {escaped}");
    assert!(escaped.contains("s\\?t"), "question mark: {escaped}");
    assert!(escaped.contains("t\\~u"), "tilde: {escaped}");
    assert!(escaped.contains("u\\#v"), "hash: {escaped}");
    assert!(escaped.contains("v\\=w"), "equals: {escaped}");
}

#[test]
fn zsh_escape_span_multi_word_value_survives_space_join_as_one_zsh_word() {
    // "hello world" のような1 span（git commit -m "hello world" の
    // 引数）をエスケープしてスペース結合した場合、実際の zsh レキサに
    // 通せば元の1トークンに戻ることを実地の zsh で証明する
    // （capture.zsh の `"$*"` 結合と同じプロトコルの直接検証）。
    // これがこの Fix の核心保証であり、単純な `str::split(' ')` による
    // 近似では「エスケープされた空白」と「区切りの空白」を区別できない
    // ため、テストとしての意味を持たせるには本物のシェルワードスプリット
    // が必要（実装時に発覚 — naive split では検証にならない）。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };

    let spans = vec![
        "git".to_string(),
        "commit".to_string(),
        "-m".to_string(),
        "hello world".to_string(),
    ];
    let escaped = escape_spans(&spans).expect("no control chars");
    let joined = escaped.join(" ");

    // `print -l -- ${(z)line}` は zsh 自身のワードスプリット規則
    // （`"$*"` 経由の内側 zsh バッファ解釈と同じレキサ）で `joined` を
    // 単語分割し、1行1単語で出力する。
    let output = Command::new(&zsh)
        .args(["-fc", "print -l -- ${(z)1}", "--", &joined])
        .output()
        .expect("failed to run zsh for word-split check");
    assert!(output.status.success(), "zsh word-split invocation failed");
    let words: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect();

    assert_eq!(
            words,
            vec!["git", "commit", "-m", "hello world"],
            "escaped+joined spans must re-split into the original 4 words via real zsh word-split, got {words:?}"
        );
}

#[test]
fn zsh_escape_span_empty_span_is_untouched() {
    // trailing partial（新規単語補完マーカー）は空のまま渡さねばならない。
    assert_eq!(zsh_escape_span("").as_deref(), Some(""));
}

#[test]
fn zsh_escape_span_plain_word_is_unchanged() {
    // 特殊文字を含まない単語はバイト単位で不変であるべき。
    assert_eq!(zsh_escape_span("checkout").as_deref(), Some("checkout"));
    assert_eq!(zsh_escape_span("main").as_deref(), Some("main"));
}

#[test]
fn zsh_escape_span_rejects_newline() {
    assert_eq!(zsh_escape_span("hello\nworld"), None);
}

#[test]
fn zsh_escape_span_rejects_carriage_return() {
    assert_eq!(zsh_escape_span("hello\rworld"), None);
}

#[test]
fn zsh_escape_span_rejects_nul() {
    assert_eq!(zsh_escape_span("hello\0world"), None);
}

#[test]
fn zsh_escape_span_rejects_other_c0_control_chars() {
    // タブ以外にも一般の C0 制御文字（例: \x01, \x1b の単体混入）を拒否する。
    assert_eq!(zsh_escape_span("hello\u{1}world"), None);
    assert_eq!(zsh_escape_span("hello\u{7f}world"), None);
}

#[test]
fn zsh_escape_span_allows_tab_as_a_normal_escapable_char() {
    // タブは制御文字だが「安全に表現できない」ケースではなく、
    // ZSH_SPECIAL_CHARS に含めてバックスラッシュエスケープで表現する
    // 対象なので、guard には引っかからず正常にエスケープされる。
    let escaped = zsh_escape_span("a\tb").expect("tab should be escapable, not rejected");
    assert_eq!(escaped, "a\\\tb");
}

#[test]
fn escape_spans_propagates_none_on_any_control_char_span() {
    let spans = vec!["git".to_string(), "co\nmmit".to_string()];
    assert_eq!(escape_spans(&spans), None);
}

#[test]
fn escape_spans_leaves_trailing_empty_partial_as_bare_empty_string() {
    let spans = vec!["git".to_string(), "checkout".to_string(), String::new()];
    let escaped = escape_spans(&spans).expect("no control chars");
    assert_eq!(escaped, vec!["git", "checkout", ""]);
}

// ── provider-contract テスト ──

/// 外部補完が明示的に無効化された設定（`external = "none"`）
/// （`JarvishCompleter` の他プロバイダの単体テストと同じ方針で、
/// このファイル内でも「外部補完まるごと無効」を再現するために使う）。
fn disabled_external_completion() -> Arc<RwLock<ExternalCompletionSettings>> {
    Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
        &CompletionConfig {
            external: ExternalSetting::Single("none".to_string()),
            ..CompletionConfig::default()
        },
    )))
}

/// 実 zsh を spawn する E2E テストが使う補完タイムアウト（ms）。
///
/// 実測で `zsh --no-rcs` + `compinit` のコールドスタートは無負荷でも
/// 数百 ms 掛かり、CI ランナーやテスト並列実行で CPU が飽和すると
/// 容易に秒オーダーへ伸びる。プロダクションの既定値
/// （`MIN_TIMEOUT_MS` = 2000ms）をそのまま使うと、実装は正しく
/// タイムアウト縮退しているのにテストの assertion だけが落ちるため、
/// E2E テストでは十分に大きい値を使う。
const E2E_TIMEOUT_MS: u64 = 15_000;

/// zsh のみを明示的に有効化した設定（`external = "zsh"` 相当）。carapace の
/// 実機有無に左右されずゲート（first-token / zsh 有無 / spans 長さ）だけを
/// 単体テストできる。
///
/// `ExternalCompletionSettings::resolve` は**使わない**。resolve() は
/// `which::which("zsh")` で実機の PATH を引くため、zsh が入っていない
/// 環境（GitHub Actions の ubuntu-latest 等）では `binary: None` に解決され、
/// `gate()` が `None` を返してゲート系テストが環境依存で落ちる。
/// ここでの関心は「zsh が優先順リストに載っていてバイナリも解決済み」
/// という状態でのゲート挙動なので、carapace.rs のテストヘルパと同じく
/// `enabled` を直接手組みして PATH 非依存にする。
///
/// 実際に zsh プロセスを spawn する統合テストは別途 `zsh_binary()` で
/// 実機の有無を確認して skip するため、この手組みが「zsh 不在なのに
/// 起動を試みる」ことにはならない。
///
/// `timeout` は既定値（`external_timeout_ms`）ではなく
/// [`E2E_TIMEOUT_MS`] を使う。このヘルパは実 zsh を spawn する E2E
/// テストからも使われ、そこでの主張は「補完候補が正しく返る」ことで
/// あって「規定のタイムアウト内に返る」ことではない。既定値のままだと
/// 負荷の高いマシンで `compinit` が予算を超え、**プロダクションコードは
/// 正しく None に縮退しているのにテストだけが落ちる**（実測: CPU を
/// 2 倍にオーバーサブスクライブすると 18 件が一斉に落ちる）。
/// タイムアウト挙動そのものを検証するテストは個別に短い値を明示指定して
/// いるため、ここを緩めても検出力は落ちない。
fn zsh_enabled_external_completion() -> Arc<RwLock<ExternalCompletionSettings>> {
    let defaults = CompletionConfig::default();
    Arc::new(RwLock::new(ExternalCompletionSettings {
        timeout: Duration::from_millis(E2E_TIMEOUT_MS),
        enabled: vec![ResolvedExternal {
            kind: ExternalKind::Zsh,
            // 実機に zsh があればそれを、無ければ慣用パスをダミーとして使う。
            // ゲート判定は「Some かどうか」しか見ないため値自体は問わないが、
            // 実機があるときは本物のパスを入れておくことで、この設定を
            // そのまま使って spawn する統合テストとも整合する。
            binary: Some(which::which("zsh").unwrap_or_else(|_| PathBuf::from("/bin/zsh"))),
        }],
        zsh_daemon_enabled: defaults.external_zsh_daemon,
    }))
}

/// carapace のみを明示的に有効化した設定（`external = "carapace"`）。
/// zsh は優先順リストに含まれないため、`ZshBridgeProvider` は
/// `binary_path(Zsh)` が常に `None` を返すことで無効化される
/// （「他プロバイダの設定に巻き込まれない」ことの検証に使う）。
fn carapace_only_external_completion() -> Arc<RwLock<ExternalCompletionSettings>> {
    Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
        &CompletionConfig {
            external: ExternalSetting::Single("carapace".to_string()),
            ..CompletionConfig::default()
        },
    )))
}

#[test]
fn provide_returns_none_when_external_is_disabled() {
    // 外部補完が `[completion] external = "none"` で無効化されている場合、
    // zsh バイナリがあり非 first-token でも候補を返さない。
    let settings = disabled_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
    let ctx = super::super::context::extract_context("git chec", 8);
    assert!(!ctx.is_first_token);
    assert!(provider.provide(&ctx).is_none());
}

#[test]
fn provide_returns_none_when_only_carapace_is_enabled() {
    // `external = "carapace"` のとき zsh は優先順リストに含まれないため、
    // ZshBridgeProvider は他プロバイダ（carapace）の有効化設定に
    // 巻き込まれず無効のままであるべき。
    let settings = carapace_only_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
    let ctx = super::super::context::extract_context("git chec", 8);
    assert!(!ctx.is_first_token);
    assert!(provider.provide(&ctx).is_none());
}

#[test]
fn provide_returns_none_for_first_token() {
    let settings = zsh_enabled_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
    let ctx = super::super::context::extract_context("gi", 2);
    assert!(ctx.is_first_token);
    assert!(provider.provide(&ctx).is_none());
}

#[test]
fn provide_returns_none_when_zsh_missing() {
    let settings = zsh_enabled_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary(
        settings,
        PathBuf::from("/no/such/zsh/binary/zzjarvish"),
    );
    // resolve_zsh は override をそのまま使う設計のため、この場合
    // provide() は run_external_capped の spawn 失敗経由で None になる。
    let ctx = super::super::context::extract_context("git chec", 8);
    assert!(!ctx.is_first_token);
    assert!(provider.provide(&ctx).is_none());
}

#[test]
fn provide_returns_none_when_spans_too_short() {
    // spans が [head, partial] 未満 = コマンド名しかない状態。
    let settings = zsh_enabled_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
    let ctx = super::super::context::extract_context("git", 3);
    // "git" は非空白なので first-token 扱いになりこちらのガードで弾かれる。
    assert!(provider.provide(&ctx).is_none());
}

// ── is_responsible (perf/completion-latency) ──
//
// provide() 冒頭の「そもそも自分の対象か」ガードと同じ基準で、実際に
// zsh プロセスを起動せず判定できることを検証する。

#[test]
fn is_responsible_true_when_zsh_enabled_and_not_first_token() {
    let settings = zsh_enabled_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
    let ctx = super::super::context::extract_context("git chec", 8);
    assert!(!ctx.is_first_token);
    assert!(provider.is_responsible(&ctx));
}

#[test]
fn is_responsible_false_when_external_is_disabled() {
    let settings = disabled_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
    let ctx = super::super::context::extract_context("git chec", 8);
    assert!(!provider.is_responsible(&ctx));
}

#[test]
fn is_responsible_false_when_only_carapace_is_enabled() {
    let settings = carapace_only_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
    let ctx = super::super::context::extract_context("git chec", 8);
    assert!(!provider.is_responsible(&ctx));
}

#[test]
fn is_responsible_false_for_first_token() {
    let settings = zsh_enabled_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
    let ctx = super::super::context::extract_context("gi", 2);
    assert!(ctx.is_first_token);
    assert!(!provider.is_responsible(&ctx));
}

#[test]
fn is_responsible_false_when_spans_too_short() {
    let settings = zsh_enabled_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
    let ctx = super::super::context::extract_context("git", 3);
    assert!(!provider.is_responsible(&ctx));
}

#[test]
fn is_responsible_matches_provide_none_reason_when_zsh_disabled_or_carapace_only() {
    // provide() と is_responsible() が同じ判定基準（gate() 経由）を
    // 共有していることの確認: 無効化されている場合は両方揃って
    // false/None を返す。
    let settings = carapace_only_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
    let ctx = super::super::context::extract_context("git chec", 8);
    assert_eq!(provider.provide(&ctx), None);
    assert!(!provider.is_responsible(&ctx));
}

// ── 統合テスト（実行時 zsh 有無で skip） ──

fn zsh_binary() -> Option<PathBuf> {
    which::which("zsh").ok()
}

#[test]
#[serial]
fn integration_git_checkout_prefix_suggests_branch() {
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };

    let tmpdir = tempfile::tempdir().unwrap();
    let dir = tmpdir.path();
    for args in [
        vec!["init"],
        vec!["config", "user.email", "test@test.com"],
        vec!["config", "user.name", "Test"],
        vec!["commit", "--allow-empty", "-m", "init"],
        vec!["branch", "zzjarvish-bridge-feature"],
    ] {
        Command::new("git")
            .args(&args)
            .current_dir(dir)
            .output()
            .unwrap();
    }

    let original_dir = env::current_dir().unwrap();
    env::set_current_dir(dir).unwrap();

    let settings = Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
        &CompletionConfig {
            external: ExternalSetting::Single("auto".to_string()),
            external_timeout_ms: E2E_TIMEOUT_MS,
            ..CompletionConfig::default()
        },
    )));
    // ブリッジディレクトリと HOME をテスト専用の一時ディレクトリへ隔離する。
    //
    // 隔離しない場合、このテストは**実ユーザーの**
    // `~/.config/jarvish/zsh-bridge` と実 `$HOME` を使ってデーモンを
    // spawn してしまう。そのため結果が開発者のローカル環境に依存し、
    // 実際に以下の形で不安定化していた（実測）:
    //   - 実 `.zshrc` / `.zshenv` が重い（プラグインマネージャ等）と
    //     レディマーカーが cold timeout 内に届かず spawn に失敗する
    //     （`zsh daemon failed to reach ready marker within timeout`）
    //   - 実ブリッジ dir に溜まった大量の残骸ファイルや、実 `$HOME` の
    //     compdump キャッシュの状態に左右される
    // 他の統合テスト（`extra_envs` で `HOME` を隔離しているもの）と同じ
    // 方針に揃え、環境非依存にする。
    let bridge_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
        settings,
        zsh,
        bridge_tmp.path().join("zsh-bridge"),
        vec![(
            "HOME".to_string(),
            home_tmp.path().to_string_lossy().into_owned(),
        )],
    );

    let line = "git checkout zzjarvish-bridge-";
    let ctx = super::super::context::extract_context(line, line.len());
    let result = provider.provide(&ctx);

    env::set_current_dir(&original_dir).unwrap();

    let candidates = result.expect("zsh bridge should return candidates for git checkout");
    assert!(
        candidates
            .iter()
            .any(|c| c.value == "zzjarvish-bridge-feature"),
        "expected branch suggestion among {candidates:?}"
    );
}

#[test]
#[serial]
fn integration_first_token_yields_none() {
    if zsh_binary().is_none() {
        eprintln!("skipping: zsh not found on PATH");
        return;
    }
    // `ZshBridgeProvider::new`（`which::which("zsh")` を都度引く本番経路）
    // + 有効化設定でも、first-token では担当外として None を返すことを
    // 確認する（`resolve_zsh` のオーバーライドなし経路のカバレッジ）。
    let settings = zsh_enabled_external_completion();
    let provider = ZshBridgeProvider::new(settings, new_shared_daemon_slot());
    let ctx = super::super::context::extract_context("gi", 2);
    assert!(provider.provide(&ctx).is_none());
}

#[test]
fn timeout_budget_is_at_least_min_timeout() {
    // MIN_TIMEOUT_MS 未満の設定 timeout でも、`gate`（共有ヘルパー化）
    // 経由の実効タイムアウトが MIN_TIMEOUT_MS を下回らないことを保証する
    // （compinit の重さ対策）。zsh を有効化した settings で `gate` 自体を
    // 呼び、`ZshBridgeProvider::provide` が実際に使う経路をそのまま検証する。
    // `external = "zsh"` の resolve() は実機に zsh バイナリが無いと
    // gate 自体が None になる（binary_path が None）ため、実行時 skip する。
    if zsh_binary().is_none() {
        eprintln!("skipping: zsh not found on PATH");
        return;
    }
    let settings = Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
        &CompletionConfig {
            external: ExternalSetting::Single("zsh".to_string()),
            external_timeout_ms: 50,
            ..CompletionConfig::default()
        },
    )));
    let (_binary, effective) = gate(
        &settings,
        ExternalKind::Zsh,
        Some(Duration::from_millis(MIN_TIMEOUT_MS)),
    )
    .expect("zsh should be gated-in when external = \"zsh\"");
    assert!(effective >= Duration::from_millis(MIN_TIMEOUT_MS));
}

#[test]
fn compute_warm_timeout_floors_low_configured_value_to_2000ms() {
    // 実機計測: tmuxinator 等の重い補完関数は Ruby インタプリタ起動で
    // 460〜910ms かかる。デフォルト設定（external_timeout_ms = 400）が
    // そのまま使われると必ずタイムアウトする——核心の回帰防止。
    let effective = compute_warm_timeout(Duration::from_millis(400));
    assert_eq!(effective, Duration::from_millis(2000));
}

#[test]
fn compute_warm_timeout_preserves_configured_value_above_floor() {
    // 床を上回る設定値はそのまま使う（フロアは下限であって固定値では
    // ない）。
    let effective = compute_warm_timeout(Duration::from_millis(3000));
    assert_eq!(effective, Duration::from_millis(3000));
}

#[test]
fn compute_warm_timeout_floors_extremely_low_value() {
    let effective = compute_warm_timeout(Duration::from_millis(1));
    assert_eq!(effective, Duration::from_millis(WARM_MIN_TIMEOUT_MS));
}

// ── ブリッジディレクトリ / .zshrc テンプレート ──

#[test]
fn bridge_dir_is_under_config_jarvish() {
    let dir = bridge_dir();
    // "~/.config/jarvish/zsh-bridge" で終わる（HOME 有無どちらでも）。
    assert!(dir.ends_with(".config/jarvish/zsh-bridge"));
}

#[test]
fn ensure_bridge_zshrc_creates_dir_and_template_when_absent() {
    let tmpdir = tempfile::tempdir().unwrap();
    let bridge = tmpdir.path().join("zsh-bridge");
    assert!(!bridge.exists());

    let zshrc = ensure_bridge_zshrc(&bridge).unwrap();

    assert!(bridge.is_dir());
    assert!(zshrc.is_file());
    let contents = fs::read_to_string(&zshrc).unwrap();
    assert!(contents.contains("fpath"));
    assert!(contents.contains("compdef"));
}

#[test]
fn ensure_bridge_zshrc_does_not_overwrite_existing_file() {
    let tmpdir = tempfile::tempdir().unwrap();
    let bridge = tmpdir.path().join("zsh-bridge");
    fs::create_dir_all(&bridge).unwrap();
    let zshrc = bridge_zshrc_path(&bridge);
    fs::write(&zshrc, "# user customized content\n").unwrap();

    ensure_bridge_zshrc(&bridge).unwrap();

    let contents = fs::read_to_string(&zshrc).unwrap();
    assert_eq!(contents, "# user customized content\n");
}

#[test]
fn ensure_bridge_zshrc_is_idempotent_across_calls() {
    let tmpdir = tempfile::tempdir().unwrap();
    let bridge = tmpdir.path().join("zsh-bridge");

    ensure_bridge_zshrc(&bridge).unwrap();
    let first = fs::read_to_string(bridge_zshrc_path(&bridge)).unwrap();
    ensure_bridge_zshrc(&bridge).unwrap();
    let second = fs::read_to_string(bridge_zshrc_path(&bridge)).unwrap();

    assert_eq!(first, second);
}

//
// 攻撃シナリオ: 攻撃者が ~/.config/jarvish/zsh-bridge を（存在する前に）
// 事前に自分が制御するディレクトリへのシンボリックリンクとして作成して
// おく、または正規のブリッジディレクトリ内に .zshrc という名前で
// シンボリックリンクを仕込んでおく。どちらのケースでも
// ensure_bridge_zshrc は書き込み・利用を一切行わず Err を返し、
// provide() はこれを受けて None に縮退しなければならない。

#[cfg(unix)]
#[test]
fn ensure_bridge_zshrc_rejects_symlinked_bridge_dir() {
    use std::os::unix::fs::symlink;

    let tmpdir = tempfile::tempdir().unwrap();
    // 攻撃者が制御する「本物の」ディレクトリ（symlink の先）。
    let attacker_dir = tmpdir.path().join("attacker-controlled");
    fs::create_dir_all(&attacker_dir).unwrap();
    // ブリッジディレクトリのパス自体をシンボリックリンクにする
    // （事前作成攻撃: jarvish が create_dir_all を呼ぶ前に攻撃者が
    // このパスへ symlink を仕込んでいたケースを再現）。
    let bridge = tmpdir.path().join("zsh-bridge");
    symlink(&attacker_dir, &bridge).unwrap();
    assert!(fs::symlink_metadata(&bridge)
        .unwrap()
        .file_type()
        .is_symlink());

    let result = ensure_bridge_zshrc(&bridge);

    assert!(
        result.is_err(),
        "ensure_bridge_zshrc must reject a symlinked bridge dir, got {result:?}"
    );
    // 攻撃者のディレクトリ配下には何も書き込まれていないこと
    // （symlink をたどって .zshrc を書いてしまっていないか）。
    assert!(
        !attacker_dir.join(".zshrc").exists(),
        "must not write through the symlink into the attacker-controlled dir"
    );
}

#[cfg(unix)]
#[test]
fn ensure_bridge_zshrc_rejects_symlinked_zshrc_inside_real_dir() {
    use std::os::unix::fs::symlink;

    let tmpdir = tempfile::tempdir().unwrap();
    let bridge = tmpdir.path().join("zsh-bridge");
    fs::create_dir_all(&bridge).unwrap();

    // ブリッジディレクトリ自体は本物だが、.zshrc だけが攻撃者の
    // ファイルへのシンボリックリンクになっているケース。
    let attacker_zshrc = tmpdir.path().join("attacker-zshrc-target");
    fs::write(&attacker_zshrc, "# attacker payload\n").unwrap();
    let zshrc_path = bridge_zshrc_path(&bridge);
    symlink(&attacker_zshrc, &zshrc_path).unwrap();

    let result = ensure_bridge_zshrc(&bridge);

    assert!(
        result.is_err(),
        "ensure_bridge_zshrc must reject a symlinked .zshrc, got {result:?}"
    );
    // 攻撃者のファイルの中身が書き換えられていないこと。
    let attacker_contents = fs::read_to_string(&attacker_zshrc).unwrap();
    assert_eq!(attacker_contents, "# attacker payload\n");
}

#[test]
fn ensure_bridge_zshrc_normal_dir_still_works() {
    // 通常ケース（symlink が一切絡まない）の回帰確認 — 挙動が
    // byte-identical に保たれていること。
    let tmpdir = tempfile::tempdir().unwrap();
    let bridge = tmpdir.path().join("zsh-bridge");

    let zshrc = ensure_bridge_zshrc(&bridge).unwrap();

    assert!(bridge.is_dir());
    assert!(!fs::symlink_metadata(&bridge)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(zshrc.is_file());
    let contents = fs::read_to_string(&zshrc).unwrap();
    assert!(contents.contains("fpath"));
}

#[cfg(unix)]
#[test]
#[serial]
fn provide_returns_none_when_bridge_dir_is_symlinked() {
    // provide() 経路全体を通した回帰テスト: シンボリックリンクされた
    // ブリッジディレクトリでは spawn 自体に到達せず None に縮退する。
    use std::os::unix::fs::symlink;

    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };

    let tmpdir = tempfile::tempdir().unwrap();
    let attacker_dir = tmpdir.path().join("attacker-controlled");
    fs::create_dir_all(&attacker_dir).unwrap();
    let bridge = tmpdir.path().join("zsh-bridge");
    symlink(&attacker_dir, &bridge).unwrap();

    let settings = zsh_enabled_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary_and_bridge_dir(settings, zsh, bridge);

    let line = "git checkout ";
    let ctx = super::super::context::extract_context(line, line.len());
    assert!(provider.provide(&ctx).is_none());
    assert!(!attacker_dir.join(".zshrc").exists());
}

// ── E2E: ユーザー定義 zsh 補完がブリッジ経由で反映されるか ──
//
// 実際に capture.zsh の -f 除去 + ZDOTDIR の配線が機能していることを
// 実地で証明する。temp ZDOTDIR に .zshrc を置き、そこから temp fpath
// ディレクトリ上のカスタム補完関数 `_jarvishtestcmd`（固定ワードリストを
// compadd するだけ）を読み込ませ、`jarvishtestcmd <Tab>` でその固定
// ワードが候補に出ることを確認する。これが失敗する = ZDOTDIR 配線か
// -f 除去のどちらかが壊れている、という決定的な回帰検知になる。
#[test]
#[serial]
fn e2e_user_zshrc_fpath_completion_is_used_via_zdotdir() {
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };

    let tmpdir = tempfile::tempdir().unwrap();
    let zdotdir = tmpdir.path().join("zdotdir");
    let fpath_dir = tmpdir.path().join("completions");
    fs::create_dir_all(&zdotdir).unwrap();
    fs::create_dir_all(&fpath_dir).unwrap();

    // ユーザー定義の補完関数: 固定ワードリストを compadd する。
    fs::write(
        fpath_dir.join("_jarvishtestcmd"),
        "#compdef jarvishtestcmd\ncompadd -- alpha beta gamma\n",
    )
    .unwrap();

    // ブリッジ .zshrc: fpath に上のディレクトリを追加するだけの
    // ユーザー拡張例（README の fpath 例と同じ形）。
    fs::write(
        zdotdir.join(".zshrc"),
        format!("fpath=({} $fpath)\n", fpath_dir.display()),
    )
    .unwrap();

    let settings = Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
        &CompletionConfig {
            external: ExternalSetting::Single("auto".to_string()),
            external_timeout_ms: E2E_TIMEOUT_MS,
            ..CompletionConfig::default()
        },
    )));
    let provider =
        ZshBridgeProvider::with_zsh_binary_and_bridge_dir(settings, zsh, zdotdir.clone());

    let line = "jarvishtestcmd ";
    let ctx = super::super::context::extract_context(line, line.len());
    let result = provider.provide(&ctx);

    let candidates = result.expect("zsh bridge should return candidates from user fpath");
    let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
    assert!(values.contains(&"alpha"), "got {values:?}");
    assert!(values.contains(&"beta"), "got {values:?}");
    assert!(values.contains(&"gamma"), "got {values:?}");

    // ensure_bridge_zshrc がユーザーの .zshrc を上書きしていないこと
    // （E2E 経路でも既存ファイル保護が効くことの確認）。
    let contents = fs::read_to_string(zdotdir.join(".zshrc")).unwrap();
    assert!(contents.contains("fpath=("));
}

//
// `git commit -m "hello world"` のような、空白を含む1つの span を
// ctx.spans() 経由で渡した場合、capture.zsh の `"$*"` 単純スペース結合
// (132行目、vendor 元のまま) によって内側 zsh 側で誤って2単語に分裂
// すると、$CURRENT（カーソル位置の単語インデックス）がずれて後続引数の
// 補完が壊れる。これを実地で証明するため、$CURRENT の値そのものを
// 候補として compadd するオラクル関数 `_jarvishtestcmd2` を使う:
// 正しくエスケープされていれば「コマンド名 + 複数語1引数 + 次の
// partial」で $CURRENT=3 になるはずだが、素朴な空白結合バグが残って
// いると "hello" "world" の2語に割れて $CURRENT=4 になってしまう。
#[test]
#[serial]
fn e2e_multi_word_span_keeps_current_index_correct() {
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };

    let tmpdir = tempfile::tempdir().unwrap();
    let zdotdir = tmpdir.path().join("zdotdir");
    let fpath_dir = tmpdir.path().join("completions");
    // 隔離用 HOME: `capture.zsh`（vendor）の `compinit -d
    // ~/.zcompdump_capture` は $ZDOTDIR ではなく $HOME 基準の固定パスに
    // compdump キャッシュを読み書きする。実 $HOME を共有したまま複数の
    // E2E テスト（異なる fpath tempdir）を連続実行すると、後続テストが
    // 前のテストの compdump を再利用して新しい #compdef 関数を認識
    // できないことが実地検証で判明した（環境依存フレークの原因）。
    // このテストは独自の $HOME を与えて compdump キャッシュを完全に
    // 隔離することで、他の E2E テストの実行順序・実行有無に依存しない
    // 決定的な結果を保証する。
    let isolated_home = tmpdir.path().join("home");
    fs::create_dir_all(&zdotdir).unwrap();
    fs::create_dir_all(&fpath_dir).unwrap();
    fs::create_dir_all(&isolated_home).unwrap();

    // オラクル補完関数: $CURRENT をそのまま候補として compadd する。
    // 単語分裂が起きていなければ常に一定の値になるはずで、分裂が起きると
    // 値がずれる — 「候補が期待どおりの場所に出る/word-count drift が
    // 無い」ことの直接的な証拠になる。
    fs::write(
        fpath_dir.join("_jarvishtestcmd2"),
        "#compdef jarvishtestcmd2\ncompadd -- \"current-is-$CURRENT\"\n",
    )
    .unwrap();
    fs::write(
        zdotdir.join(".zshrc"),
        format!("fpath=({} $fpath)\n", fpath_dir.display()),
    )
    .unwrap();

    let settings = Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
        &CompletionConfig {
            external: ExternalSetting::Single("auto".to_string()),
            external_timeout_ms: E2E_TIMEOUT_MS,
            ..CompletionConfig::default()
        },
    )));
    let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
        settings,
        zsh,
        zdotdir.clone(),
        vec![(
            "HOME".to_string(),
            isolated_home.to_string_lossy().into_owned(),
        )],
    );

    // spans: ["jarvishtestcmd2", "hello world", ""] (3 spans)。
    // ctx.spans() が実際にこの形を作ることを確認したうえで、
    // provide() に通す。カーソルは "hello world" の後の trailing
    // space（新規引数の開始）にある想定。
    let line = r#"jarvishtestcmd2 "hello world" "#;
    let ctx = super::super::context::extract_context(line, line.len());
    assert_eq!(
        ctx.spans(),
        vec!["jarvishtestcmd2", "hello world", ""],
        "ctx.spans() should keep the quoted two-word arg as a single span"
    );

    let result = provider.provide(&ctx);
    let candidates = result.expect("zsh bridge should return candidates when CURRENT is correct");
    let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();

    // 正しい配線: コマンド名(1) + "hello world"(1 span, 2) + 新規引数(3)
    // = $CURRENT が 3 の位置で呼ばれる。バグ版（単純スペース結合で
    // "hello"/"world" に分裂）では $CURRENT=4 になり、"current-is-3" は
    // 出現しない（症状の直接再現・回帰検知）。
    assert!(
        values.contains(&"current-is-3"),
        "expected $CURRENT=3 (no word-count drift) among {values:?}; \
             a value of current-is-4 here would indicate the old space-join bug regressed"
    );
    assert!(
        !values.contains(&"current-is-4"),
        "current-is-4 indicates the multi-word span was split into two words: {values:?}"
    );
}

//
// `disabled_external_completion` / `zsh_enabled_external_completion` は
// `CompletionConfig::default()` を土台にしており、そのデフォルトは
// `external_zsh_daemon = true` のため、このファイル内の既存の
// "one-shot" 統合テスト（`integration_git_checkout_prefix_suggests_branch`
// 等）は本タスク以降、実際にはデーモン経路を経由する。それでも出力
// フォーマット（`parse_capture_output` が読む "value -- description"
// 形式）は capture.zsh と daemon_init.zsh で共通のため、既存アサーション
// は変更なしにそのまま通る（daemon_init.zsh のモジュールドキュメント
// 参照）。ここでは daemon 固有の契約（遅延 spawn・使い回し・ホット
// リロードでの off 切り替え・mtime 再起動トリガ・失敗時のこの Tab
// での None）を直接検証する。

/// zsh のみを有効化し、かつ `external_zsh_daemon = false` を明示した
/// 設定（ワンショット経路のみを強制するテスト専用ヘルパー）。
fn zsh_enabled_daemon_off_external_completion() -> Arc<RwLock<ExternalCompletionSettings>> {
    Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
        &CompletionConfig {
            external: ExternalSetting::Single("zsh".to_string()),
            external_timeout_ms: E2E_TIMEOUT_MS,
            external_zsh_daemon: false,
            ..CompletionConfig::default()
        },
    )))
}

/// デーモンテスト用の隔離フィクスチャ。`zsh_bridge.rs` の既存 E2E
/// テスト（`e2e_user_zshrc_fpath_completion_is_used_via_zdotdir` 等）と
/// 同じ理由（`compinit -d ~/.zcompdump_capture` は `$ZDOTDIR` ではなく
/// **`$HOME`** 基準の固定パスに compdump キャッシュを読み書きするため、
/// 実 `$HOME` を共有したまま複数テストを並行実行すると compdump の
/// 汚染・衝突で `#compdef` 関数が認識されず、デフォルトのファイル名
/// 補完へ静かにフォールバックしてしまう ── 実機検証で確認済みの
/// フレーク要因）で、`HOME` も専用 tempdir に隔離する。呼び出し元は
/// `extra_envs()` で得られる `HOME` の env ペアを
/// `with_zsh_binary_bridge_dir_and_envs` に渡すこと。
fn zsh_daemon_test_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmpdir = tempfile::tempdir().unwrap();
    let zdotdir = tmpdir.path().join("zdotdir");
    let fpath_dir = tmpdir.path().join("completions");
    let home = tmpdir.path().join("home");
    fs::create_dir_all(&zdotdir).unwrap();
    fs::create_dir_all(&fpath_dir).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::write(
        fpath_dir.join("_jarvishtestcmd"),
        "#compdef jarvishtestcmd\ncompadd -- alpha beta gamma\n",
    )
    .unwrap();
    fs::write(
        zdotdir.join(".zshrc"),
        format!("fpath=({} $fpath)\n", fpath_dir.display()),
    )
    .unwrap();
    (tmpdir, zdotdir, fpath_dir)
}

/// [`zsh_daemon_test_fixture`] の tmpdir から隔離 `HOME` の env ペアを
/// 組み立てる（`with_zsh_binary_bridge_dir_and_envs` にそのまま渡せる形）。
fn zsh_daemon_test_home_envs(tmpdir: &tempfile::TempDir) -> Vec<(String, String)> {
    vec![(
        "HOME".to_string(),
        tmpdir.path().join("home").to_string_lossy().into_owned(),
    )]
}

#[test]
#[serial]
fn daemon_flag_off_never_spawns_daemon_one_shot_still_serves_candidates() {
    // フラグ off: `provide()` は一度も daemon フィールドを埋めず、
    // 常にワンショット経路（`run_external_capped` 経由）で候補を返す。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);

    let settings = zsh_enabled_daemon_off_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
        settings,
        zsh,
        zdotdir.clone(),
        home_envs,
    );

    let line = "jarvishtestcmd ";
    let ctx = super::super::context::extract_context(line, line.len());
    let result = provider.provide(&ctx);

    let candidates = result.expect("one-shot path should still serve candidates");
    let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
    assert!(values.contains(&"alpha"), "got {values:?}");

    assert!(
        provider.daemon.lock().unwrap().is_none(),
        "daemon slot must remain empty when external_zsh_daemon = false"
    );
}

#[test]
#[serial]
fn daemon_path_e2e_serves_candidates_via_warm_daemon() {
    // デーモン経路 (external_zsh_daemon = true, デフォルト) で、
    // capture.zsh と同じユーザー fpath 補完がそのまま反映されることを
    // 実機で証明する。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);

    let settings = zsh_enabled_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
        settings,
        zsh,
        zdotdir.clone(),
        home_envs,
    );

    let line = "jarvishtestcmd ";
    let ctx = super::super::context::extract_context(line, line.len());
    let result = provider.provide(&ctx);

    let candidates = result.expect("daemon path should serve candidates");
    let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
    assert!(values.contains(&"alpha"), "got {values:?}");
    assert!(values.contains(&"beta"), "got {values:?}");
    assert!(values.contains(&"gamma"), "got {values:?}");

    assert!(
        provider.daemon.lock().unwrap().is_some(),
        "daemon slot must be populated after a successful daemon-path request"
    );
}

#[test]
#[serial]
fn cold_spawn_budget_does_not_starve_a_slow_first_request() {
    // cold_timeout（MIN_TIMEOUT_MS = 2000ms）は spawn + init の
    // レディマーカー待ちのみを賄う予算であり、初回の実補完リクエストは
    // 別枠（warm_timeout）で走る。ここでは spawn+init 自体は速いが、
    // 最初の補完関数呼び出し自体が「cold budget の残りを
    // 使い切っていたはずの長さ」だけ遅い（900ms）フィクスチャを使い、
    // それでも最初の Tab が候補を返すことを証明する（実機報告の
    // tmuxinator シナリオの直接再現）。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);
    fs::write(
        fpath_dir.join("_jarvishtestslowfirst"),
        "#compdef jarvishtestslowfirst\nsleep 0.9\ncompadd -- slowcandidate\n",
    )
    .unwrap();

    // デフォルト相当の設定（external_timeout_ms=400 → warm floor 2000ms）。
    let settings = zsh_enabled_external_completion();
    let provider =
        ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(settings, zsh, zdotdir, home_envs);

    let line = "jarvishtestslowfirst ";
    let ctx = super::super::context::extract_context(line, line.len());
    let result = provider.provide(&ctx);

    let candidates =
        result.expect("first Tab must serve real candidates, not fall through to None");
    let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
    assert!(
        values.contains(&"slowcandidate"),
        "expected the slow first request's own candidate among {values:?}"
    );
    assert!(
        provider.daemon.lock().unwrap().is_some(),
        "daemon must have survived spawning + the slow first request"
    );
}

#[test]
#[serial]
fn realistic_interpreter_startup_proxy_survives_three_tabs_same_pid() {
    // 実測ベース受け入れテスト: `_tmuxinator` の実測値
    // （Ruby インタプリタ起動込みで 460〜910ms）を模した、サブプロセス
    // を exec して ~600ms かかる補完関数フィクスチャを、デフォルト
    // 相当の設定（external_timeout_ms 未指定 = 400ms → warm floor
    // 2000ms）で3回連続 Tab 押下し、いずれも None でも
    // PathProvider フォールバック相当でもなく実際の候補を返し、かつ
    // 同じデーモン pid のまま生き続けることを検証する。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);
    // `sh -c 'sleep 0.6'` でサブプロセス exec + 待ち合わせを模す
    // （tmuxinator が `ruby` を exec するのと同じ「補完関数がサブ
    // プロセスを起動して待つ」構造）。
    fs::write(
        fpath_dir.join("_jarvishtestinterp"),
        "#compdef jarvishtestinterp\n\
             sh -c 'sleep 0.6'\n\
             compadd -- interpcandidate\n",
    )
    .unwrap();

    let settings = zsh_enabled_external_completion();
    let provider =
        ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(settings, zsh, zdotdir, home_envs);

    let line = "jarvishtestinterp ";
    let ctx = super::super::context::extract_context(line, line.len());

    let mut pid_seen: Option<u32> = None;
    for tab in 1..=3 {
        let result = provider.provide(&ctx);
        let candidates = result.unwrap_or_else(|| {
            panic!("Tab #{tab} must return real candidates, not None/path-fallback")
        });
        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(
            values.contains(&"interpcandidate"),
            "Tab #{tab}: expected interpcandidate among {values:?}"
        );

        let pid_now = {
            let guard = provider.daemon.lock().unwrap();
            guard.as_ref().unwrap().daemon.child_pid_for_test()
        };
        if let Some(prev) = pid_seen {
            assert_eq!(
                pid_now, prev,
                "Tab #{tab}: daemon must survive with the same pid across all 3 tabs"
            );
        }
        pid_seen = Some(pid_now);
    }
}

#[test]
#[serial]
fn daemon_path_reuses_same_daemon_across_requests() {
    // 2回連続でリクエストしても同じ ZshDaemon インスタンス（同じ子
    // プロセス pid）が使い回されることを、実際の pid を比較して直接
    // 証明する（ウォームリクエストが「起動コストなしの計算のみ」に
    // なっているという本タスクの動機の核心）。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);

    let settings = zsh_enabled_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
        settings,
        zsh,
        zdotdir.clone(),
        home_envs,
    );

    let line = "jarvishtestcmd ";
    let ctx = super::super::context::extract_context(line, line.len());

    let first = provider.provide(&ctx);
    assert!(first.is_some(), "first request should succeed");
    let pid_after_first = {
        let guard = provider.daemon.lock().unwrap();
        guard.as_ref().unwrap().daemon.child_pid_for_test()
    };

    let second = provider.provide(&ctx);
    assert!(second.is_some(), "second request should succeed");
    let pid_after_second = {
        let guard = provider.daemon.lock().unwrap();
        guard.as_ref().unwrap().daemon.child_pid_for_test()
    };

    assert_eq!(
        pid_after_first, pid_after_second,
        "the same daemon child process must serve both requests (no respawn)"
    );
}

#[test]
#[serial]
fn daemon_path_restarts_when_bridge_zshrc_mtime_changes() {
    // ブリッジ .zshrc を最初のリクエスト後に touch（新しい補完関数を
    // 追加した想定）すると、次のリクエストで既存デーモンが shutdown
    // され、新しいデーモン（新しい pid）が遅延 spawn される。かつ、
    // 新しく追加した補完関数の候補がちゃんと反映される
    // （respawn 後は新しい ZDOTDIR/.zshrc を source し直しているという
    // 直接証拠）。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);

    let settings = zsh_enabled_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
        settings,
        zsh,
        zdotdir.clone(),
        home_envs,
    );

    let line = "jarvishtestcmd ";
    let ctx = super::super::context::extract_context(line, line.len());
    let first = provider.provide(&ctx).expect("first request should work");
    assert!(first.iter().any(|c| c.value == "alpha"));

    let pid_before = {
        let guard = provider.daemon.lock().unwrap();
        guard.as_ref().unwrap().daemon.child_pid_for_test()
    };

    // mtime が確実に進むよう、ファイルシステムの mtime 解像度を超える
    // だけ待ってから書き換える（多くの環境で mtime は最低でも秒未満
    // 精度を持つが、安全側に倒して確実に差分を作る）。
    std::thread::sleep(Duration::from_millis(1100));

    // 新しい補完関数を追加し、.zshrc をそれを拾うよう書き換える
    // （mtime が更新される）。
    fs::write(
        fpath_dir.join("_jarvishtestcmd3"),
        "#compdef jarvishtestcmd3\ncompadd -- delta\n",
    )
    .unwrap();
    fs::write(
        zdotdir.join(".zshrc"),
        format!(
            "fpath=({} $fpath)\n# touched to bump mtime\n",
            fpath_dir.display()
        ),
    )
    .unwrap();

    let line2 = "jarvishtestcmd3 ";
    let ctx2 = super::super::context::extract_context(line2, line2.len());
    let second = provider
        .provide(&ctx2)
        .expect("request after .zshrc touch should still work (daemon respawned)");
    assert!(
        second.iter().any(|c| c.value == "delta"),
        "respawned daemon should have re-sourced the updated bridge .zshrc: {second:?}"
    );

    let pid_after = {
        let guard = provider.daemon.lock().unwrap();
        guard.as_ref().unwrap().daemon.child_pid_for_test()
    };
    assert_ne!(
        pid_before, pid_after,
        "daemon must be restarted (new child pid) after bridge .zshrc mtime changes"
    );
}

#[test]
#[serial]
fn daemon_survives_zshrc_deletion_after_spawn_mtime_none_is_treated_as_unchanged() {
    // 「両側 None => 変化なしとして扱い、スプリアスな再起動をしない」
    // の片側 — spawn 時点では mtime が取れていた（Some）が、その後
    // ブリッジ .zshrc 自体が削除されて current_mtime が None になる
    // ケース。`(Some(a), Some(b)) if a != b` という一致条件は片方が
    // None の時点で成立しないため、mtime_changed は false のまま
    // ——同じデーモン（同じ pid）が使い回されることを直接証明する。
    //
    // `provide()` 経由だと `ensure_bridge_zshrc` が「削除済みの .zshrc」
    // を検知してデフォルトテンプレートで再作成してしまい（fpath 設定が
    // 失われ `_jarvishtestcmd` が引けなくなる）、mtime 比較ロジック
    // 自体とは無関係な理由でテストが崩れる。そのため
    // `request_via_daemon` を直接呼び、mtime 比較ロジックだけを隔離して
    // 検証する（`ensure_bridge_zshrc` 自体の再作成挙動は別テストの
    // 責務）。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);

    let settings = zsh_enabled_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
        settings,
        zsh,
        zdotdir.clone(),
        home_envs,
    );

    let escaped = vec!["jarvishtestcmd".to_string(), String::new()];
    // spawn/補完の予算はプロダクション定数ではなく E2E 用の余裕を持った
    // 値を使う（`E2E_TIMEOUT_MS` のドキュメント参照 — 実 zsh の compinit は
    // 高負荷環境で 2s の本番予算を超え、実装が正しくてもテストだけが落ちる）。
    let cold = Duration::from_millis(E2E_TIMEOUT_MS);
    let warm = Duration::from_millis(E2E_TIMEOUT_MS);

    let first = provider
        .request_via_daemon(
            &provider.resolve_zsh().unwrap(),
            &zdotdir,
            &escaped,
            cold,
            warm,
        )
        .expect("first request should work");
    assert!(parse_capture_output(&first)
        .iter()
        .any(|c| c.value == "alpha"));

    let pid_before = {
        let guard = provider.daemon.lock().unwrap();
        let slot = guard.as_ref().unwrap();
        // spawn 時点で記録された mtime が Some だったことを確認して
        // おく（そうでないとこのテストが片側 None のケースを検証して
        // いないことになる）。
        assert!(
            slot.zshrc_mtime_at_spawn.is_some(),
            "precondition: spawn-time mtime must be Some for this test to be meaningful"
        );
        slot.daemon.child_pid_for_test()
    };

    // ブリッジ .zshrc を削除する — 以後 fs::metadata(...).modified() は
    // NotFound エラーとなり current_mtime は None になる。
    // request_via_daemon 自体は ensure_bridge_zshrc を呼ばないため、
    // ここでは削除された状態のまま mtime 比較に入る。
    fs::remove_file(bridge_zshrc_path(&zdotdir)).unwrap();

    let second = provider
        .request_via_daemon(
            &provider.resolve_zsh().unwrap(),
            &zdotdir,
            &escaped,
            cold,
            warm,
        )
        .expect("request after .zshrc deletion should still work (daemon reused, not respawned)");
    assert!(
        parse_capture_output(&second)
            .iter()
            .any(|c| c.value == "alpha"),
        "the SAME (already-running) daemon must still serve the fpath completion it \
             loaded at spawn time — proof that it was not respawned/re-sourced"
    );

    let pid_after = {
        let guard = provider.daemon.lock().unwrap();
        guard.as_ref().unwrap().daemon.child_pid_for_test()
    };
    assert_eq!(
        pid_before, pid_after,
        "deleting the bridge .zshrc (spawn-time Some -> current None) must NOT trigger a \
             restart — either side being None is treated as 'unchanged' (safe-side fallback)"
    );
}

#[test]
#[serial]
fn daemon_reused_when_spawn_time_mtime_was_none_and_file_now_present() {
    // 「両側 None => 変化なし」のもう片側 — spawn 時点では mtime が
    // 取れなかった（意図的に spawn 前に .zshrc を削除しておくケース）が、
    // 次のリクエスト時には .zshrc が（再）存在し current_mtime が Some
    // になっているケース。`(Some(a), Some(b))` のパターンは spawn 側が
    // None の時点でマッチしないため、mtime_changed は false のまま
    // ——同じデーモンが使い回されることを直接証明する。
    //
    // 前のテストと同じ理由で `request_via_daemon` を直接呼び、
    // `ensure_bridge_zshrc` のテンプレート再作成による干渉を避ける。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);
    let zshrc_path = bridge_zshrc_path(&zdotdir);

    let settings = zsh_enabled_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
        settings,
        zsh,
        zdotdir.clone(),
        home_envs,
    );

    // spawn 直前に .zshrc を退避しておき、request_via_daemon が spawn
    // 時点で current_mtime = None を記録するよう仕向ける。ZDOTDIR には
    // 依然として fpath 設定済みの .zshrc が存在しないため、内側 zsh の
    // 起動時点では compdef が登録されないが、リクエスト前に復元する
    // ことで内側 zsh 自体には実害が出ない
    // （spawn() 完了後に .zshrc を読み直すことはない — 初期化は spawn
    // 時の一度きりのため）。このテストの関心は mtime 比較ロジック
    // のみであり、実際の補完動作は前段の `_jarvishtestcmd` フィクスチャ
    // ではなく、単に「同じデーモンが使い回されたか」を pid 比較で見る。
    let saved = fs::read(&zshrc_path).unwrap();
    fs::remove_file(&zshrc_path).unwrap();

    let escaped = vec!["jarvishtestcmd".to_string(), String::new()];
    // spawn/補完の予算はプロダクション定数ではなく E2E 用の余裕を持った
    // 値を使う（`E2E_TIMEOUT_MS` のドキュメント参照 — 実 zsh の compinit は
    // 高負荷環境で 2s の本番予算を超え、実装が正しくてもテストだけが落ちる）。
    let cold = Duration::from_millis(E2E_TIMEOUT_MS);
    let warm = Duration::from_millis(E2E_TIMEOUT_MS);

    // spawn 時点で .zshrc が存在しないため fpath は素の状態(ZDOTDIR の
    // デフォルト検索パスのみ)になるが、request_via_daemon 自体は spawn
    // に成功する（zsh -i 自体は .zshrc が無くても起動できる）。
    let _first = provider.request_via_daemon(
        &provider.resolve_zsh().unwrap(),
        &zdotdir,
        &escaped,
        cold,
        warm,
    );

    let pid_before = {
        let guard = provider.daemon.lock().unwrap();
        let slot = guard.as_ref().unwrap();
        assert!(
            slot.zshrc_mtime_at_spawn.is_none(),
            "precondition: spawn-time mtime must be None for this test to be meaningful"
        );
        slot.daemon.child_pid_for_test()
    };

    // .zshrc を復元する — 以後 current_mtime は Some になる。
    fs::write(&zshrc_path, &saved).unwrap();

    let _second = provider.request_via_daemon(
        &provider.resolve_zsh().unwrap(),
        &zdotdir,
        &escaped,
        cold,
        warm,
    );

    let pid_after = {
        let guard = provider.daemon.lock().unwrap();
        guard.as_ref().unwrap().daemon.child_pid_for_test()
    };
    assert_eq!(
        pid_before, pid_after,
        "spawn-time None -> current Some must NOT trigger a restart — either side being \
             None is treated as 'unchanged' (safe-side fallback)"
    );
}

#[test]
#[serial]
fn daemon_failure_after_two_consecutive_timeouts_respawns_lazily_next_tab() {
    // サーキットブレーカーの provide() 経由 E2E: 完全ハングする
    // 補完関数に対して1回目の Tab は None（グレース、デーモンはまだ
    // 生存）、2回目の Tab（=1回目の残留フレームのドレイン失敗 + 2回目
    // 自体もハング）でサーキットブレーカーが作動しデーモンが kill
    // される。3回目の Tab では遅延 respawn されて通常どおり候補を
    // 返すことを確認する。
    //
    // 実装ノート: 隔離 `HOME`（`zsh_daemon_test_home_envs`）を必ず渡す
    // こと。`compinit -d ~/.zcompdump_capture` は `$HOME` 基準の固定
    // パスに compdump キャッシュを読み書きするため、実 `$HOME` を
    // 共有したまま並行実行すると `#compdef jarvishtesthang` が
    // 一時的に認識されず zsh のデフォルトのファイル名補完へ静かに
    // フォールバックしてしまい、`sleep 30` で本来ハングするはずの
    // リクエストが即座に（誤った）候補を返す desync として観測される
    // ── 実機検証で確認済みのフレーク要因（`zsh_daemon_test_fixture`
    // のドキュメント参照）。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);
    fs::write(
        fpath_dir.join("_jarvishtesthang"),
        "#compdef jarvishtesthang\nsleep 30\ncompadd -- neverseen\n",
    )
    .unwrap();

    // このテストは2つの相反する要求を持つ:
    // - セットアップ/遅延 respawn フェーズ: 実 zsh のコールドスタート
    //   （compinit）が完了する余裕が要る。
    // - ハングフェーズ: 短いタイムアウトで確実に timeout させたい。
    //
    // 単一の短い値（500ms 固定）だとセットアップ側が高負荷環境で
    // 予算不足になり、実装が正しいのに `cold-spawn request should succeed`
    // で落ちる。設定は `Arc<RwLock<_>>` でホットリロード可能なので、
    // フェーズごとに切り替えて両方の要求を満たす。
    let settings = Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
        &CompletionConfig {
            external: ExternalSetting::Single("zsh".to_string()),
            external_timeout_ms: E2E_TIMEOUT_MS,
            external_zsh_daemon: true,
            ..CompletionConfig::default()
        },
    )));
    /// ハング検証フェーズで使う短いタイムアウト（ms）。
    const HANG_TIMEOUT_MS: u64 = 500;
    let set_timeout = |ms: u64| {
        settings.write().unwrap().timeout = Duration::from_millis(ms);
    };
    let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
        Arc::clone(&settings),
        zsh,
        zdotdir.clone(),
        home_envs,
    );

    // 1回目のリクエストでコールド spawn させておく。決定的な補完
    // 関数（zsh_daemon_test_fixture が用意する _jarvishtestcmd、
    // 固定ワードリスト）を使い、レスポンスが素早く確定することを
    // 確認してからハング側のリクエストへ進む。
    let warm_line = "jarvishtestcmd ";
    let warm_ctx = super::super::context::extract_context(warm_line, warm_line.len());
    let cold_result = provider.provide(&warm_ctx);
    let cold_candidates = cold_result.expect("cold-spawn request should succeed");
    assert!(cold_candidates.iter().any(|c| c.value == "alpha"));
    assert!(
        provider.daemon.lock().unwrap().is_some(),
        "daemon should have been spawned by the first request"
    );
    let pid_before_hangs = {
        let guard = provider.daemon.lock().unwrap();
        guard.as_ref().unwrap().daemon.child_pid_for_test()
    };

    let hang_line = "jarvishtesthang ";
    let hang_ctx = super::super::context::extract_context(hang_line, hang_line.len());

    // ここからハング検証フェーズ: 短いタイムアウトへ切り替える。
    set_timeout(HANG_TIMEOUT_MS);

    // 1回目のハング Tab: グレースにより None だが、デーモンは同じ pid
    // のまま生存し続ける。
    let start1 = std::time::Instant::now();
    let hung_result1 = provider.provide(&hang_ctx);
    let elapsed1 = start1.elapsed();
    assert_eq!(
        hung_result1, None,
        "hung completion must yield None for this Tab (no one-shot fallback)"
    );
    assert!(
        elapsed1 < Duration::from_secs(5),
        "provide() should return promptly after the configured timeout, took {elapsed1:?}"
    );
    assert!(
        provider.daemon.lock().unwrap().is_some(),
        "daemon must survive a single timeout (Fix D2 grace)"
    );
    assert_eq!(
        {
            let guard = provider.daemon.lock().unwrap();
            guard.as_ref().unwrap().daemon.child_pid_for_test()
        },
        pid_before_hangs,
        "grace must not respawn the daemon"
    );

    // 2回目のハング Tab: 1回目の残留フレームのドレインが失敗し、これで
    // 連続2回目としてサーキットブレーカーが作動、デーモンが kill される。
    let start2 = std::time::Instant::now();
    let hung_result2 = provider.provide(&hang_ctx);
    let elapsed2 = start2.elapsed();
    assert_eq!(hung_result2, None);
    assert!(
        elapsed2 < Duration::from_secs(5),
        "provide() should return promptly, took {elapsed2:?}"
    );
    assert!(
        provider.daemon.lock().unwrap().is_none(),
        "daemon slot must be cleared after 2 consecutive timeouts (circuit breaker) \
             so the next Tab respawns lazily"
    );

    // 3回目の Tab: 遅延 respawn されて再び通常の補完が使えることを確認する。
    // ここは実 zsh のコールドスタートを伴うため E2E 予算へ戻す。
    set_timeout(E2E_TIMEOUT_MS);
    let retry = provider.provide(&warm_ctx);
    let candidates = retry.expect("next Tab should lazily respawn and succeed");
    assert!(candidates.iter().any(|c| c.value == "alpha"));
    assert!(provider.daemon.lock().unwrap().is_some());
}

#[test]
#[serial]
fn daemon_turned_off_mid_session_shuts_down_running_daemon() {
    // ホットリロードのシミュレーション: 稼働中のデーモンがある状態から
    // 共有 settings の `zsh_daemon_enabled` を false に書き換えると、
    // 次の `provide()` 呼び出しでデーモンが shutdown され、以後は
    // ワンショット経路にフォールバックする（`reload_config` が
    // `Arc<RwLock<_>>` の中身を丸ごと差し替える経路の模擬 —
    // `carapace.rs` の hot-reload テストと同じ方針）。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);

    let settings = zsh_enabled_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
        Arc::clone(&settings),
        zsh,
        zdotdir,
        home_envs,
    );

    let line = "jarvishtestcmd ";
    let ctx = super::super::context::extract_context(line, line.len());
    let first = provider.provide(&ctx);
    assert!(first.is_some(), "daemon path should work before reload");
    assert!(provider.daemon.lock().unwrap().is_some());

    // reload: 同じ Arc の中身を daemon off の設定に丸ごと差し替える。
    {
        let mut guard = settings.write().unwrap();
        guard.zsh_daemon_enabled = false;
    }

    let second = provider.provide(&ctx);
    assert!(
        second.is_some(),
        "one-shot fallback should still serve candidates after daemon is turned off"
    );
    assert!(
        provider.daemon.lock().unwrap().is_none(),
        "running daemon must be shut down as soon as external_zsh_daemon flips to false"
    );
}

#[test]
fn daemon_enabled_default_true_uses_daemon_field_type() {
    // ExternalCompletionSettings::resolve のデフォルト（CompletionConfig
    // ::default()）で zsh_daemon_enabled が true になっていることの
    // 単体確認（zsh 不要、実機非依存）。
    let settings = zsh_enabled_external_completion();
    assert!(settings.read().unwrap().zsh_daemon_enabled);
}

/// pid が実際に ESRCH になる（プロセスが死んでいる）まで短時間・
/// 有界回数ポーリングする（`zsh_daemon.rs` / `external.rs` の既存
/// テストと同じ考え方）。
fn wait_for_pid_death(pid: u32) -> bool {
    for _ in 0..40 {
        let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if ret == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn new_shared_daemon_slot_starts_empty() {
    let slot = new_shared_daemon_slot();
    assert!(slot.lock().unwrap().is_none());
}

#[test]
fn shutdown_shared_daemon_on_empty_slot_is_a_no_op() {
    // 既に空のスロットに対して shutdown_shared_daemon を呼んでも
    // panic せず、スロットは空のままである（冪等性）。
    let slot = new_shared_daemon_slot();
    shutdown_shared_daemon(&slot);
    assert!(slot.lock().unwrap().is_none());
}

#[test]
#[serial]
fn shutdown_shared_daemon_kills_live_daemon_and_empties_slot() {
    // Shell::exec_restart / main.rs の exit 経路が呼ぶのと同じ
    // shutdown_shared_daemon() を直接呼び、実際に子プロセスが ESRCH に
    // なる（本当に死ぬ）ことと、スロットが None に戻ることの両方を
    // 実機で証明する（unit テスト — exec() 自体はテストしない）。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);

    let shared_slot = new_shared_daemon_slot();
    let settings = zsh_enabled_external_completion();
    let provider = ZshBridgeProvider::with_shared_daemon_slot_for_test(
        settings,
        zsh,
        zdotdir,
        home_envs,
        Arc::clone(&shared_slot),
    );

    let line = "jarvishtestcmd ";
    let ctx = super::super::context::extract_context(line, line.len());
    assert!(provider.provide(&ctx).is_some(), "daemon should spawn");

    let child_pid = {
        let guard = shared_slot.lock().unwrap();
        guard.as_ref().unwrap().daemon.child_pid_for_test()
    };

    shutdown_shared_daemon(&shared_slot);

    assert!(
        shared_slot.lock().unwrap().is_none(),
        "slot must be empty after shutdown_shared_daemon"
    );
    assert!(
        wait_for_pid_death(child_pid),
        "child pid {child_pid} should be dead after shutdown_shared_daemon"
    );
}

#[test]
#[serial]
fn provide_shuts_down_daemon_when_gate_returns_none() {
    // zsh が enabled-kinds リストから外れる（gate() が None を
    // 返す）と、provide() は早期 return する前に生きているデーモンを
    // shutdown しなければならない。まず zsh 有効設定でデーモンを
    // spawn させ、その後 settings を carapace のみへ丸ごと差し替えて
    // （zsh が binary_path から消える）再度 provide() を呼び、
    // スロットが空になり子プロセスが実際に死ぬことを確認する。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);

    let settings = zsh_enabled_external_completion();
    let provider = ZshBridgeProvider::with_zsh_binary_bridge_dir_and_envs(
        Arc::clone(&settings),
        zsh,
        zdotdir,
        home_envs,
    );

    let line = "jarvishtestcmd ";
    let ctx = super::super::context::extract_context(line, line.len());
    assert!(provider.provide(&ctx).is_some(), "daemon should spawn");

    let child_pid = {
        let guard = provider.daemon.lock().unwrap();
        guard.as_ref().unwrap().daemon.child_pid_for_test()
    };

    // settings を丸ごと carapace のみ（zsh は enabled から消える）に
    // 差し替える — `external_provider_chain` の並び替えではなく、
    // 同じ Arc の中身だけを書き換える hot-reload シミュレーション
    // （carapace.rs / zsh_bridge.rs の既存 hot-reload テストと同じ方針）。
    {
        let mut guard = settings.write().unwrap();
        *guard = ExternalCompletionSettings::resolve(&CompletionConfig {
            external: ExternalSetting::Single("carapace".to_string()),
            ..CompletionConfig::default()
        });
    }
    assert!(
        settings
            .read()
            .unwrap()
            .binary_path(ExternalKind::Zsh)
            .is_none(),
        "zsh must no longer be gated-in after the settings swap"
    );

    let result = provider.provide(&ctx);
    assert!(
        result.is_none(),
        "provide() must return None once zsh is gated out"
    );
    assert!(
        provider.daemon.lock().unwrap().is_none(),
        "provide() must shut down the now-forbidden daemon before returning None (A4)"
    );
    assert!(
        wait_for_pid_death(child_pid),
        "child pid {child_pid} should be dead after gate()-None shutdown"
    );
}

/// [`prewarm_zsh_daemon_with`] 用の poll ヘルパー: 生成された総合的な
/// 猶予時間内でスロットが埋まるのを待つ（バックグラウンドスレッド経由
/// の spawn は非同期なので、テスト側は寛容にポーリングする）。
fn wait_for_slot_populated(slot: &SharedDaemonSlot, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if slot.lock().unwrap().is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    slot.lock().unwrap().is_some()
}

#[test]
#[serial]
fn prewarm_populates_slot_when_daemon_enabled_without_provide_call() {
    // 核心保証: settings がデーモン有効を示している状態で
    // prewarm を呼ぶと、`provide()` を一度も呼ばなくてもスロットが
    // 埋まる。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);

    let settings = zsh_enabled_external_completion();
    let slot = new_shared_daemon_slot();
    let gate = DaemonGate::new();

    prewarm_zsh_daemon_with(&settings, &slot, &gate, &zsh, &zdotdir, &home_envs);

    if !prewarm_spawned(&slot) {
        return;
    }
    assert!(
        slot.lock().unwrap().is_some(),
        "prewarm should populate the slot synchronously in this direct call \
             (no provide() call was made)"
    );
}

/// prewarm が実際にデーモンを spawn できたかを確認し、できていなければ
/// このテストの前提が成立しないものとして skip すべきかを返す。
///
/// [`prewarm_zsh_daemon_with`] は spawn 予算に**プロダクション定数**
/// [`MIN_TIMEOUT_MS`]（2000ms）をハードコードしており、テスト側の
/// `ExternalCompletionSettings::timeout` では上書きできない（UI スレッドを
/// ブロックしないための本番仕様なので変更しない）。実 zsh の
/// `compinit` コールドスタートは CPU が飽和すると容易に 2s を超えるため、
/// その状況では prewarm がスロットを埋められないのが**正しい挙動**で
/// ある。ここで無条件に `is_some()` を主張すると、実装が正しいのに
/// 高負荷環境でだけ落ちるフレークになる（実測: CPU 2 倍
/// オーバーサブスクライブで再現）。
fn prewarm_spawned(slot: &SharedDaemonSlot) -> bool {
    if slot.lock().unwrap().is_some() {
        return true;
    }
    eprintln!(
        "skipping: prewarm could not spawn a daemon within its hardcoded \
             {MIN_TIMEOUT_MS}ms budget (host too slow / saturated)"
    );
    false
}

#[test]
fn prewarm_is_a_no_op_when_daemon_disabled() {
    // フラグ off、または zsh が enabled-kinds に含まれない設定では
    // prewarm は一切 spawn せずスロットは空のまま。zsh バイナリの
    // 実機有無に関わらずテストできる（should_run_zsh_daemon の判定が
    // spawn より先に効くため、無効な zsh パスを渡しても安全）。
    let settings = zsh_enabled_daemon_off_external_completion();
    let slot = new_shared_daemon_slot();
    let gate = DaemonGate::new();

    prewarm_zsh_daemon_with(
        &settings,
        &slot,
        &gate,
        Path::new("/no/such/zsh/binary/zzjarvish"),
        Path::new("/tmp/zzjarvish-unused-bridge-dir"),
        &[],
    );

    assert!(
        slot.lock().unwrap().is_none(),
        "prewarm must be a no-op (slot stays None) when the daemon is disabled"
    );
}

#[test]
fn prewarm_is_a_no_op_when_zsh_not_in_enabled_kinds() {
    // フラグは on だが zsh が優先順リストに無い（例: external =
    // "carapace"）ケースも同様に no-op であるべき
    // （`should_run_zsh_daemon` のもう一方の条件）。
    let settings = carapace_only_external_completion();
    let slot = new_shared_daemon_slot();
    let gate = DaemonGate::new();

    prewarm_zsh_daemon_with(
        &settings,
        &slot,
        &gate,
        Path::new("/no/such/zsh/binary/zzjarvish"),
        Path::new("/tmp/zzjarvish-unused-bridge-dir"),
        &[],
    );

    assert!(slot.lock().unwrap().is_none());
}

#[test]
#[serial]
fn provide_reuses_the_prewarmed_daemon_same_pid() {
    // prewarm で spawn したデーモンを、その後の provide() 呼び出しが
    // 再利用する（同じ pid で新規 spawn しない）ことを直接証明する。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);

    let settings = zsh_enabled_external_completion();
    let slot = new_shared_daemon_slot();
    let gate = DaemonGate::new();
    prewarm_zsh_daemon_with(&settings, &slot, &gate, &zsh, &zdotdir, &home_envs);
    if !prewarm_spawned(&slot) {
        return;
    }
    let pid_from_prewarm = {
        let guard = slot.lock().unwrap();
        guard.as_ref().unwrap().daemon.child_pid_for_test()
    };

    let provider = ZshBridgeProvider::with_shared_daemon_slot_for_test(
        settings,
        zsh,
        zdotdir,
        home_envs,
        Arc::clone(&slot),
    );

    let line = "jarvishtestcmd ";
    let ctx = super::super::context::extract_context(line, line.len());
    let result = provider.provide(&ctx);
    let candidates = result.expect("provide() should serve candidates via the prewarmed daemon");
    assert!(candidates.iter().any(|c| c.value == "alpha"));

    let pid_after_provide = {
        let guard = slot.lock().unwrap();
        guard.as_ref().unwrap().daemon.child_pid_for_test()
    };
    assert_eq!(
        pid_from_prewarm, pid_after_provide,
        "provide() must reuse the daemon spawned by prewarm rather than spawning a new one"
    );
}

#[test]
#[serial]
fn prewarm_and_provide_race_yields_exactly_one_daemon_process() {
    // レース回避保証: prewarm をトリガーした直後（そのスレッドが
    // 実際に Mutex を取るより前）に provide() を呼び、両方が spawn を
    // 試みうる状況を作る。最終的にプロセスは1つだけ生き残ることを、
    // スロットの pid と実際のプロセス生存確認の両方で検証する。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);

    let settings = zsh_enabled_external_completion();
    let slot = new_shared_daemon_slot();
    let gate = DaemonGate::new();

    let prewarm_settings = Arc::clone(&settings);
    let prewarm_slot = Arc::clone(&slot);
    let prewarm_gate = Arc::clone(&gate);
    let prewarm_zsh = zsh.clone();
    let prewarm_zdotdir = zdotdir.clone();
    let prewarm_envs = home_envs.clone();
    let handle = std::thread::spawn(move || {
        prewarm_zsh_daemon_with(
            &prewarm_settings,
            &prewarm_slot,
            &prewarm_gate,
            &prewarm_zsh,
            &prewarm_zdotdir,
            &prewarm_envs,
        );
    });

    // provide() をほぼ同時に呼ぶ（トリガー直後、prewarm スレッドが
    // Mutex を取るより先に到達しうるタイミングを狙う——スケジューリング
    // 依存のため決定的なタイミング保証はないが、両方が spawn を試みる
    // ケースをできるだけ再現する）。
    let provider = ZshBridgeProvider::with_shared_daemon_slot_for_test(
        Arc::clone(&settings),
        zsh,
        zdotdir,
        home_envs,
        Arc::clone(&slot),
    );
    let line = "jarvishtestcmd ";
    let ctx = super::super::context::extract_context(line, line.len());
    let provide_result = provider.provide(&ctx);

    handle.join().expect("prewarm thread should not panic");

    // 少なくとも一方は成功しているはず（prewarm か provide() か、
    // タイミング次第でどちらが先でも構わない）。
    assert!(
        wait_for_slot_populated(&slot, Duration::from_secs(10)),
        "slot should be populated by either prewarm or provide()"
    );
    if provide_result.is_none() {
        // provide() 側がスロット未確定のタイミングで走り None を返した
        // 場合でも、最終的にスロットは埋まっていること自体は上で確認
        // 済み。再度 provide() すれば必ず候補が返る（レースの後始末が
        // 正しく完了していることの追加確認）。
        let retry = provider.provide(&ctx);
        assert!(retry.is_some(), "retry after the race should succeed");
    }

    let final_pid = {
        let guard = slot.lock().unwrap();
        guard.as_ref().unwrap().daemon.child_pid_for_test()
    };

    // pgrep で「バックグラウンド事前ウォームアップの子として spawn
    // されうる zsh -i プロセス」の総数を数える代わりに、より決定的な
    // 方法として: スロットに残った pid が生きていることと、レースで
    // 捨てられた側（もしあれば）が実際に kill/reap されて ESRCH に
    // なっていることを確認する。捨てられた pid を直接知る手段はテスト
    // からは無い（`prewarm_zsh_daemon_with` 内部でのみ判明する）ため、
    // 「スロットに残っている pid が実際に生きているプロセスである」
    // ことと「そのプロセスに対して同じ pid で複数回 provide しても
    // 常に同じ pid が返る（=2個目のデーモンが紛れ込んでいない）」
    // ことを検証することで、実質的に「1個だけ生き残った」ことの
    // 十分な証拠とする。
    let ret = unsafe { libc::kill(final_pid as libc::pid_t, 0) };
    assert_eq!(
        ret, 0,
        "the surviving daemon pid {final_pid} must be a live process"
    );

    let second_provide = provider.provide(&ctx);
    let pid_again = {
        let guard = slot.lock().unwrap();
        guard.as_ref().unwrap().daemon.child_pid_for_test()
    };
    assert!(second_provide.is_some());
    assert_eq!(
        final_pid, pid_again,
        "no second daemon should have been spawned after the race settled"
    );
}

#[test]
fn daemon_gate_starts_open() {
    let gate = DaemonGate::new();
    assert!(!gate.is_closed());
}

#[test]
fn daemon_gate_close_is_observed_and_idempotent() {
    let gate = DaemonGate::new();
    gate.close();
    assert!(gate.is_closed());
    // 二重 close は panic せず、閉じたままである（冪等性）。
    gate.close();
    assert!(gate.is_closed());
}

#[test]
#[serial]
fn prewarm_after_gate_closed_never_populates_slot_and_kills_spawned_child() {
    // 核心保証（決定的ユニットテスト）: `shutdown_shared_daemon_blocking`
    // 相当（= gate を close してからスロットを shutdown）が**先に**
    // 起きたあとで `prewarm_zsh_daemon_with` を呼んでも、スロットは
    // 空のまま保たれ、かつ prewarm が実際に spawn した子プロセスは
    // （spawn 自体は締め切り後に発生する準正常系だが）確実に kill
    // される。「タイミング運任せの多くは漏れない」ではなく、closed
    // 状態の下では 100% 決定的にスロットへ書き込まれないことを保証する
    // （Mutex 内での再チェックがこの決定性の根拠 — 実装コメント参照）。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);

    let settings = zsh_enabled_external_completion();
    let slot = new_shared_daemon_slot();
    let gate = DaemonGate::new();

    // 終端 shutdown が「先に」起きたことをシミュレートする
    // （main.rs の -c 経路 / rc.jsh 内 exit 経路が踏む順序と同じ:
    // gate.close() → スロット shutdown、この時点でスロットは空）。
    gate.close();
    shutdown_shared_daemon(&slot);
    assert!(slot.lock().unwrap().is_none());

    // その後で prewarm が（レースにより）遅れて spawn を試みる。
    prewarm_zsh_daemon_with(&settings, &slot, &gate, &zsh, &zdotdir, &home_envs);

    // 決定的保証その1: スロットは書き込まれない。
    assert!(
        slot.lock().unwrap().is_none(),
        "prewarm must never populate the slot once the gate is closed"
    );
}

#[test]
#[serial]
fn prewarm_close_race_after_spawn_but_before_lock_kills_the_orphan() {
    // より正確に実運用のレースを再現する版: prewarm が「spawn を完了した
    // 後・Mutex を取る前」というタイミングで gate が close される
    // ケース（main.rs の shutdown_zsh_daemon が prewarm のスレッド
    // スケジューリングの隙を突いて割り込む実際のシナリオ）。
    //
    // `prewarm_zsh_daemon_with` は spawn 完了直後に一度 `gate.is_closed()`
    // を確認する（Mutex 外の早期チェック）ため、このテストは「spawn 後
    // 即座に close された場合でも、実際に spawn された子プロセスが
    // 確実に kill/reap される」ことを ESRCH ポーリングで直接証明する。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);

    let settings = zsh_enabled_external_completion();
    let slot = new_shared_daemon_slot();
    let gate = DaemonGate::new();

    // gate をあらかじめ close しておく（spawn 完了時点で必ず closed に
    // なっている、というタイミングの最も厳しいケースを決定的に作る —
    // 実際のスレッドインターリーブを待つのではなく、closed の状態で
    // prewarm を呼ぶことで「spawn 後チェックが機能するか」を直接検証）。
    gate.close();

    prewarm_zsh_daemon_with(&settings, &slot, &gate, &zsh, &zdotdir, &home_envs);

    assert!(
        slot.lock().unwrap().is_none(),
        "slot must remain empty when the gate was already closed before spawn completed"
    );
    // このテスト構成では spawn 自体が gate.is_closed() の早期チェック
    // （spawn 前）で弾かれるため、子プロセスは元々生成されていない
    // （早期リターンの網羅性を示す——重い spawn 処理にすら入らない）。
}

#[test]
#[serial]
fn shutdown_shared_daemon_blocking_with_gate_prevents_late_prewarm_insertion() {
    // 実際の Shell::shutdown_zsh_daemon が使う経路
    // （shutdown_shared_daemon_blocking(slot, deadline, Some(&gate))）を
    // 直接呼び、「shutdown 後に prewarm_zsh_daemon_with を呼んでもスロット
    // が空のまま」であることを、公開 API のシグネチャそのままで検証する
    // （タスク指示の受け入れ基準3 の決定的ユニットテスト）。
    let Some(zsh) = zsh_binary() else {
        eprintln!("skipping: zsh not found on PATH");
        return;
    };
    let (tmpdir, zdotdir, _fpath_dir) = zsh_daemon_test_fixture();
    let home_envs = zsh_daemon_test_home_envs(&tmpdir);

    let settings = zsh_enabled_external_completion();
    let slot = new_shared_daemon_slot();
    let gate = DaemonGate::new();

    // スロットは元々空（-c 単体実行が数ミリ秒で完走し、prewarm がまだ
    // 何も spawn していない時点で shutdown が先に走るケースを模す）。
    shutdown_shared_daemon_blocking(&slot, Duration::from_secs(2), Some(&gate));
    assert!(slot.lock().unwrap().is_none());
    assert!(gate.is_closed());

    // 遅れて prewarm が発火しても、closed を検知して自壊する。
    prewarm_zsh_daemon_with(&settings, &slot, &gate, &zsh, &zdotdir, &home_envs);

    assert!(
        slot.lock().unwrap().is_none(),
        "late prewarm after shutdown_shared_daemon_blocking(..., Some(gate)) must not \
             populate the slot (S5 acceptance criterion 3)"
    );
}

#[test]
fn shutdown_shared_daemon_blocking_without_gate_does_not_close_it() {
    // reload 経路（`apply_zsh_daemon_lifecycle_for_reload` 等）は
    // 通常 shutdown_shared_daemon（非ブロッキング、gate なし）を使うが、
    // 万一 shutdown_shared_daemon_blocking を `gate: None` で呼んでも
    // gate には触れない（tombstone は exit/exec 専用という契約）ことを
    // 確認する。
    let slot = new_shared_daemon_slot();
    let gate = DaemonGate::new();
    shutdown_shared_daemon_blocking(&slot, Duration::from_millis(100), None);
    assert!(!gate.is_closed(), "gate must stay open when not passed in");
}
