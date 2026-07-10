//! zsh 補完ブリッジ — vendored `capture.zsh` を経由して zsh の compsys
//! （`_*` 補完関数群）の候補をワンショットで吸い出す Provider
//!
//! `assets/zsh/capture.zsh`（`Valodim/zsh-capture-completion`、MIT）を
//! `include_str!` でバイナリに埋め込み、Tab 押下ごとに `zsh --no-rcs -c
//! <script> -- <spans...>` を [`run_external_capped`] 経由で起動する。
//! スクリプト内部では `zpty` で "内側の" zsh をさらに1本起動し compinit
//! した上で completion widget を叩くため（`zsh.go` の呼び出し形と実地検証済み
//! プロトコル — このファイル冒頭のドキュメント参照）、ハング時は
//! プロセスグループ全体を kill する `run_external_capped` の group-kill が
//! 特に重要（内側の zpty 子が孤児化して残るのを防ぐ）。
//!
//! # 呼び出し形（`zsh.go` = carapace-bridge のリファレンス実装と一致）
//! ```text
//! zsh --no-rcs -c <embedded script> -- <word0> <word1> ... <partial>
//! ```
//! `partial` は最後の引数（空文字列もありうる）。`capture.zsh` は
//! `zpty -w z "$*"$'\t'` で argv をスペース結合して内側の zsh バッファに
//! 流し込むため、`partial` が空文字列でも末尾にスペースが付き、compsys が
//! 「新しい単語の補完」として扱う（`spans()` が既にこの規約で
//! 末尾に空文字列を追加する設計になっている — `context.rs` 参照）。
//!
//! # 出力パース
//! `capture.zsh` 自身が NUL センチネル行を吸収し、候補行だけを stdout に
//! 流す（このファイルではセンチネル処理は不要）。行区切りは PTY 由来の
//! `\r\n` で、末尾の空要素は捨てる。各行は:
//! 1. ANSI エスケープ除去
//! 2. `zsh.go` のバックスラッシュ unquote テーブルを適用
//! 3. 最初の `" -- "` で `value` / `description` に分割（区切りなしなら
//!    description は `None`）

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use super::carapace::{ExternalCompletionSettings, ExternalMode};
use super::context::CompletionContext;
use super::external::run_external_capped;
use super::provider::{Candidate, CompletionProvider};

/// zsh ブリッジ本体（`assets/zsh/capture.zsh` を vendor したもの）。
///
/// Task 2b.2 で spawn 行にブリッジ用 zshrc の source 呼び出しを追加する
/// 予定のため、このファイルでは一切改変しない。
const CAPTURE_SCRIPT: &str = include_str!("../../../assets/zsh/capture.zsh");

/// zsh ブリッジの有効化フラグ。
///
/// TODO(Task 2b.4): `[completion]` 設定でのプロバイダ順・有効化制御に
/// 置き換える。それまでは常に有効（zsh バイナリの有無のみで実質ゲート）。
const ENABLED: bool = true;

/// `zsh --no-rcs -c <script>` のハードタイムアウト予算。
///
/// ワンショットの内側 zsh 起動 + compinit は環境によって 100〜300ms
/// かかりうるため、carapace 用に短めに調整された
/// `ExternalCompletionSettings::timeout` をそのまま使うとタイムアウトで
/// 候補を取りこぼしやすい。Task 2b.4 で独立設定が入るまでの暫定措置として、
/// 共有設定の timeout と、compinit の重さを見込んだ下限値の大きい方を使う。
const MIN_TIMEOUT_MS: u64 = 800;

/// zsh 補完ブリッジ Provider。
///
/// `ExternalCompletionSettings` を [`super::carapace::CarapaceProvider`] と
/// 同じ `Arc<RwLock<_>>` で共有する（Task 2b.4 で有効化・優先順を専用設定に
/// 切り出すまでの暫定配管）。`mode == ExternalMode::None`（`[completion]
/// external = "none"`）のときは `CarapaceProvider` と同様に無効化する —
/// 現時点で zsh ブリッジ専用の有効/無効設定はまだない（Task 2b.4）ため、
/// carapace と共通の無効化スイッチに乗せておくのが「外部補完を丸ごと切る」
/// ユーザー意図と整合する。それ以外のモード（`Auto` / `Carapace`）では
/// zsh バイナリの有無のみでゲートする。timeout も同じ設定から間借りする。
pub(super) struct ZshBridgeProvider {
    settings: Arc<RwLock<ExternalCompletionSettings>>,
    /// テスト用に zsh の場所を差し替えられるようにするフック。
    /// 本番は `None` で `which::which("zsh")` を都度引く。
    zsh_override: Option<PathBuf>,
}

impl ZshBridgeProvider {
    pub(super) fn new(settings: Arc<RwLock<ExternalCompletionSettings>>) -> Self {
        Self {
            settings,
            zsh_override: None,
        }
    }

    #[cfg(test)]
    fn with_zsh_binary(settings: Arc<RwLock<ExternalCompletionSettings>>, zsh: PathBuf) -> Self {
        Self {
            settings,
            zsh_override: Some(zsh),
        }
    }

    fn resolve_zsh(&self) -> Option<PathBuf> {
        if let Some(path) = &self.zsh_override {
            return Some(path.clone());
        }
        which::which("zsh").ok()
    }
}

impl CompletionProvider for ZshBridgeProvider {
    fn provide(&self, ctx: &CompletionContext) -> Option<Vec<Candidate>> {
        if !ENABLED {
            return None;
        }
        if ctx.is_first_token {
            // コマンド名自体の補完は CommandProvider の担当。
            return None;
        }

        let timeout = {
            // 短命な read ロック: mode と timeout を取得したら即座に drop する
            // （`carapace.rs` / `mod.rs` の aliases スナップショットと同じ方針）。
            let settings = self.settings.read().ok()?;
            if settings.mode == ExternalMode::None {
                return None;
            }
            settings
                .timeout
                .max(std::time::Duration::from_millis(MIN_TIMEOUT_MS))
        };

        let zsh = self.resolve_zsh()?;

        let spans = ctx.spans();
        if spans.len() < 2 {
            // spans[0] (コマンド名) しかない = まだサブコマンド/引数の
            // 補完対象がない（carapace.rs と同じガード）。
            return None;
        }

        let mut args = vec![
            "--no-rcs".to_string(),
            "-c".to_string(),
            CAPTURE_SCRIPT.to_string(),
            "--".to_string(),
        ];
        args.extend(spans);

        let stdout = run_external_capped(&zsh, &args, &[], timeout)?;

        let candidates = parse_capture_output(&stdout);
        if candidates.is_empty() {
            return None;
        }
        Some(candidates)
    }
}

/// `capture.zsh` の stdout をパースして候補列に変換する。
///
/// PTY 由来の `\r\n` で分割し、末尾の空要素（トレイリング改行）は捨てる。
/// 各行は ANSI 除去 → バックスラッシュ unquote → 最初の `" -- "` で
/// value/description に分割、の順で処理する。
fn parse_capture_output(stdout: &str) -> Vec<Candidate> {
    let mut lines: Vec<&str> = stdout.split("\r\n").collect();
    // 末尾の空要素（トレイリング区切りの結果）を捨てる。
    if lines.last() == Some(&"") {
        lines.pop();
    }

    lines
        .into_iter()
        .filter(|line| !line.is_empty())
        .filter_map(parse_capture_line)
        .collect()
}

/// 1 行を [`Candidate`] へ変換する。空行や value が空の行は `None`。
fn parse_capture_line(line: &str) -> Option<Candidate> {
    let stripped = strip_ansi(line);
    let unquoted = unquote_backslashes(&stripped);

    let (value, description) = match unquoted.find(" -- ") {
        Some(idx) => {
            let (value, rest) = unquoted.split_at(idx);
            // rest は " -- ..." なので " -- " (4 バイト) を飛ばす。
            (value.to_string(), Some(rest[4..].to_string()))
        }
        None => (unquoted, None),
    };

    if value.is_empty() {
        return None;
    }

    let append_whitespace = !ends_with_no_space_rune(&value);

    Some(Candidate {
        value,
        description,
        append_whitespace,
    })
}

/// carapace 慣習の「この文字で終わる値の後ろにはスペースを入れない」文字集合
/// （`carapace-bridge` の `NoSpace([]rune("/=@:.,"))` と同じ）。
fn ends_with_no_space_rune(value: &str) -> bool {
    matches!(
        value.chars().last(),
        Some('/' | '=' | '@' | ':' | '.' | ',')
    )
}

/// ANSI エスケープシーケンス（CSI: `ESC [ ... <final byte>`）を取り除く。
///
/// compsys の候補行は色付け（`zstyle ':completion:*' list-colors` 等）で
/// ANSI コードが混じることがあるため、確定挿入前に必ず取り除く。
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
                          // パラメータ・中間バイトを読み飛ばし、final byte (0x40-0x7E) で終端。
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

/// `zsh.go` の unquoter テーブルと同じバックスラッシュエスケープを解除する。
///
/// 対象: `\\` `\&` `\<` `\>` `` \` `` `\'` `\"` `\{` `\}` `\$` `\#` `\|` `\?`
/// `\(` `\)` `\;` `\ ` `\[` `\]` `\*` `\~`
fn unquote_backslashes(input: &str) -> String {
    const ESCAPABLE: &[char] = &[
        '\\', '&', '<', '>', '`', '\'', '"', '{', '}', '$', '#', '|', '?', '(', ')', ';', ' ', '[',
        ']', '*', '~',
    ];

    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next) = chars.peek() {
                if ESCAPABLE.contains(&next) {
                    out.push(next);
                    chars.next();
                    continue;
                }
            }
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CompletionConfig;
    use serial_test::serial;
    use std::env;
    use std::process::Command;
    use std::time::Duration;

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

    // ── provider-contract テスト ──

    /// `mode == None` で明示的に無効化した設定
    /// （`JarvishCompleter` の他プロバイダの単体テストと同じ方針で、
    /// このファイル内でも「外部補完まるごと無効」を再現するために使う）。
    fn disabled_external_completion() -> Arc<RwLock<ExternalCompletionSettings>> {
        Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig {
                external: "none".to_string(),
                ..CompletionConfig::default()
            },
        )))
    }

    /// `mode == Auto` で有効化した設定。バイナリ検出（carapace）自体は
    /// `ZshBridgeProvider` の判定に使わないため、carapace の実機有無に
    /// 左右されずゲート（first-token / zsh 有無 / spans 長さ）だけを
    /// 単体テストできる。
    fn enabled_external_completion() -> Arc<RwLock<ExternalCompletionSettings>> {
        Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig {
                external: "auto".to_string(),
                ..CompletionConfig::default()
            },
        )))
    }

    #[test]
    fn provide_returns_none_when_mode_is_disabled() {
        // 外部補完が `[completion] external = "none"` で無効化されている場合、
        // zsh バイナリがあり非 first-token でも候補を返さない
        // （carapace と共通の無効化スイッチに乗る、という設計の直接検証）。
        let settings = disabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
        let ctx = super::super::context::extract_context("git chec", 8);
        assert!(!ctx.is_first_token);
        assert!(provider.provide(&ctx).is_none());
    }

    #[test]
    fn provide_returns_none_for_first_token() {
        let settings = enabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
        let ctx = super::super::context::extract_context("gi", 2);
        assert!(ctx.is_first_token);
        assert!(provider.provide(&ctx).is_none());
    }

    #[test]
    fn provide_returns_none_when_zsh_missing() {
        let settings = enabled_external_completion();
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
        let settings = enabled_external_completion();
        let provider = ZshBridgeProvider::with_zsh_binary(settings, PathBuf::from("/bin/zsh"));
        let ctx = super::super::context::extract_context("git", 3);
        // "git" は非空白なので first-token 扱いになりこちらのガードで弾かれる。
        assert!(provider.provide(&ctx).is_none());
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
                external: "auto".to_string(),
                external_timeout_ms: 3000,
                ..CompletionConfig::default()
            },
        )));
        let provider = ZshBridgeProvider::with_zsh_binary(settings, zsh);

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
        let settings = enabled_external_completion();
        let provider = ZshBridgeProvider::new(settings);
        let ctx = super::super::context::extract_context("gi", 2);
        assert!(provider.provide(&ctx).is_none());
    }

    #[test]
    fn timeout_budget_is_at_least_min_timeout() {
        // MIN_TIMEOUT_MS 未満の設定 timeout でも実効タイムアウトが
        // MIN_TIMEOUT_MS を下回らないことを保証する（compinit の重さ対策）。
        let settings = Arc::new(RwLock::new(ExternalCompletionSettings::resolve(
            &CompletionConfig {
                external: "none".to_string(),
                external_timeout_ms: 50,
                ..CompletionConfig::default()
            },
        )));
        let read = settings.read().unwrap();
        let effective = read.timeout.max(Duration::from_millis(MIN_TIMEOUT_MS));
        assert!(effective >= Duration::from_millis(MIN_TIMEOUT_MS));
    }
}
