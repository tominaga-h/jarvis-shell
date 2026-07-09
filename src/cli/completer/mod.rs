//! コマンド補完 — Tab キーで PATH コマンド名・ビルトイン・ファイルパス・git ブランチを補完
//!
//! - 先頭トークン: PATH 内の実行可能コマンド + ビルトイン (cd, cwd, exit, ...)
//! - 先頭トークンがパスらしい場合 (`./` `../` `/` `~/`): ファイル / ディレクトリ補完
//! - `git <branch系サブコマンド>`: git ブランチ名補完
//! - それ以降: カレントディレクトリ基準のファイル / ディレクトリ名
//!
//! [`CompletionProvider`] トレイトで補完源をプラグイン化しており、
//! `complete()` は [`Command`](command::CommandProvider) →
//! [`Git`](git::GitProvider) → [`Path`](path::PathProvider) の順に
//! 各プロバイダを走査し、最初に `Some` を返したプロバイダの候補を採用する
//! （`None` = 対象外で次へ、`Some(vec![])` = 担当したが候補なしでそこで確定）。
//!
//! コマンド名補完は fish shell の設計思想に倣い、インメモリキャッシュを持たず、
//! Tab 押下時にリアルタイムで `$PATH` を走査する（キャッシュレス設計）。
//! `brew install` 等で新しいバイナリが追加された直後でも即座に補完候補に出現する。
//!
//! シェルエイリアス（`alias` ビルトイン）は先頭トークンではない位置でのみ
//! 展開する（[`apply_shell_alias`]）。展開結果は各プロバイダ走査前に
//! `ctx.expanded_head` へ格納され、`GitProvider` 等は `command_words()`
//! 経由でそれを透過的に参照する。`aliases` は `Shell` と `Arc` を共有して
//! おり、`alias` ビルトイン実行直後の次の Tab から即座に反映される。

mod command;
mod context;
// TODO(Phase2a Task 2a.2): CarapaceProvider がこのランナーを消費し始めたら
// allow(dead_code) を外す。
#[allow(dead_code)]
mod external;
mod git;
mod path;
mod provider;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use reedline::{Completer, Suggestion};

use crate::engine::expand::{operator_prefix_len, split_quoted};

use command::CommandProvider;
use context::{extract_context, CompletionContext};
use git::GitProvider;
use path::PathProvider;
use provider::{escape_for_insert, CompletionProvider};

/// ColumnarMenu は description を持つ候補が 1 件でもあると全幅 1 カラムに
/// 描画が変わってしまうため、候補数がこの件数を超えたら description を
/// 一律で除去する（大きな PATH スキャン結果を守るガード）。
const DESCRIPTION_LIMIT: usize = 30;

/// Jarvish 用の補完エンジン
///
/// `$PATH` の走査はキャッシュレスだが、Git エイリアスの解決結果は
/// CWD ごとにインメモリキャッシュする（`includeIf` 等のディレクトリ依存設定に対応）。
///
/// `git_branch_commands` は `Shell` と共有され、`source` コマンドで動的に更新される。
pub struct JarvishCompleter {
    providers: Vec<Box<dyn CompletionProvider>>,
    /// シェルエイリアス（Shell と共有。`alias` ビルトインによる更新が
    /// 次回の Tab に即座に反映される — reload 不要）
    aliases: Arc<RwLock<HashMap<String, String>>>,
}

impl JarvishCompleter {
    pub fn new(
        git_branch_commands: Arc<RwLock<Vec<String>>>,
        aliases: Arc<RwLock<HashMap<String, String>>>,
    ) -> Self {
        let providers: Vec<Box<dyn CompletionProvider>> = vec![
            Box::new(CommandProvider::new(Arc::clone(&aliases))),
            Box::new(GitProvider::new(git_branch_commands)),
            Box::new(PathProvider),
        ];
        Self { providers, aliases }
    }
}

impl Completer for JarvishCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let mut ctx = extract_context(line, pos);

        if !ctx.is_first_token {
            // 短命な read ロック: スナップショットを clone したら即座に drop する。
            let snapshot = self
                .aliases
                .read()
                .map(|guard| guard.clone())
                .unwrap_or_default();
            apply_shell_alias(&mut ctx, &snapshot);
        }

        let candidates = self
            .providers
            .iter()
            .find_map(|provider| provider.provide(&ctx))
            .unwrap_or_default();

        let strip_descriptions = should_strip_descriptions(candidates.len());

        candidates
            .into_iter()
            .map(|candidate| Suggestion {
                value: escape_for_insert(&candidate.value),
                description: if strip_descriptions {
                    None
                } else {
                    candidate.description
                },
                style: None,
                extra: None,
                span: ctx.span,
                append_whitespace: candidate.append_whitespace,
                match_indices: None,
            })
            .collect()
    }
}

/// 候補数が [`DESCRIPTION_LIMIT`] を超えるかどうかを判定する。
///
/// `complete()` から切り出したのは、ファイルシステムや `Completer` トレイトの
/// セットアップなしに境界値（`DESCRIPTION_LIMIT` ちょうど / +1）を単体テスト
/// するため。
fn should_strip_descriptions(candidate_count: usize) -> bool {
    candidate_count > DESCRIPTION_LIMIT
}

/// `ctx.tokens[0]` がシェルエイリアスなら、値を展開して
/// `ctx.expanded_head` に格納する。
///
/// `tokens[0]` が先頭コマンド位置でも（`is_first_token` は呼び出し元で
/// 弾いている）、それ以外の位置でも意味を持たないため、呼び出しは常に
/// `!ctx.is_first_token` の場合に限る。
///
/// エイリアス値は実行系の `split_quoted`（strict パーサ）でトークナイズする。
/// パースエラー（未閉クォート等）が出たら展開をスキップする（エイリアス値は
/// ユーザーが `alias` ビルトインで自由に設定できるため、不正な値でも
/// パニックせずフォールバックする）。
///
/// 展開結果に演算子トークン（`| && || ; > >> <`）が 1 つでも含まれる場合は
/// 展開しない。パイプ等を含むエイリアス値はセグメントの再切断
/// （cut_index の再計算）が必要になり、Phase 1.5 のスコープ外として
/// 意図的に見送る — この場合は今までどおりパス補完にフォールバックする。
fn apply_shell_alias(ctx: &mut CompletionContext, aliases: &HashMap<String, String>) {
    let Some(first) = ctx.tokens.first() else {
        return;
    };
    let Some(alias_value) = aliases.get(first.value.as_str()) else {
        return;
    };

    let Ok(expanded_tokens) = split_quoted(alias_value) else {
        return;
    };

    let has_operator = expanded_tokens
        .iter()
        .any(|t| operator_prefix_len(&t.value) == t.value.len());
    if has_operator {
        return;
    }

    let mut values: Vec<String> = expanded_tokens.into_iter().map(|t| t.value).collect();
    // tokens[0] より後ろの既存トークン（partial 含む）の値を続ける。
    // `command_words()` の非展開経路と挙動を揃えるため、リダイレクト等の
    // 演算子トークン（`> >> <`）はここでも除外する。
    values.extend(
        ctx.tokens[1..]
            .iter()
            .filter(|t| !t.is_operator)
            .map(|t| t.value.clone()),
    );

    ctx.expanded_head = Some(values);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;
    use std::fs;

    use crate::config::CompletionConfig;

    fn test_completer() -> JarvishCompleter {
        let commands = CompletionConfig::default().git_branch_commands;
        JarvishCompleter::new(
            Arc::new(RwLock::new(commands)),
            Arc::new(RwLock::new(HashMap::new())),
        )
    }

    /// alias マップの `Arc` を呼び出し元にも返す（共有 Arc の即時反映を
    /// テストするため、completer 構築後に呼び出し元からマップを書き換えられる）。
    fn test_completer_with_aliases(
        aliases: HashMap<String, String>,
    ) -> (JarvishCompleter, Arc<RwLock<HashMap<String, String>>>) {
        let commands = CompletionConfig::default().git_branch_commands;
        let aliases = Arc::new(RwLock::new(aliases));
        let completer =
            JarvishCompleter::new(Arc::new(RwLock::new(commands)), Arc::clone(&aliases));
        (completer, aliases)
    }

    fn create_test_tree() -> (tempfile::TempDir, String) {
        let tmpdir = tempfile::tempdir().expect("failed to create tempdir");
        let base = tmpdir.path();

        fs::create_dir(base.join("Documents")).unwrap();
        fs::create_dir(base.join("Desktop")).unwrap();
        fs::create_dir(base.join("Downloads")).unwrap();
        fs::create_dir(base.join(".hidden_dir")).unwrap();

        fs::write(base.join("readme.txt"), "").unwrap();
        fs::write(base.join(".dotfile"), "").unwrap();

        let path = base.to_str().unwrap().to_string();
        (tmpdir, path)
    }

    fn create_test_git_repo() -> tempfile::TempDir {
        use std::process::Command;

        let tmpdir = tempfile::tempdir().unwrap();
        let dir = tmpdir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["branch", "test-feature"])
            .current_dir(dir)
            .output()
            .unwrap();

        tmpdir
    }

    fn create_test_git_repo_with_aliases() -> tempfile::TempDir {
        use std::process::Command;

        let tmpdir = create_test_git_repo();
        let dir = tmpdir.path();

        Command::new("git")
            .args(["config", "alias.co", "checkout"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "alias.nb", "checkout -b"])
            .current_dir(dir)
            .output()
            .unwrap();

        tmpdir
    }

    // ── complete (Completer trait) 統合テスト（既存の回帰網。原文の意図・assertion を維持） ──

    #[test]
    fn complete_cd_dirs_only_via_trait() {
        let (_tmpdir, path) = create_test_tree();
        let mut completer = test_completer();
        let line = format!("cd {path}/");
        let pos = line.len();

        let suggestions = completer.complete(&line, pos);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(values.contains(&format!("{path}/Documents/").as_str()));
        assert!(!values.iter().any(|v| v.contains("readme.txt")));
    }

    #[test]
    fn complete_ls_shows_files_and_dirs() {
        let (_tmpdir, path) = create_test_tree();
        let mut completer = test_completer();
        let line = format!("ls {path}/");
        let pos = line.len();

        let suggestions = completer.complete(&line, pos);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(values.contains(&format!("{path}/Documents/").as_str()));
        assert!(values.contains(&format!("{path}/readme.txt").as_str()));
    }

    #[test]
    #[serial]
    fn complete_tilde_alone_expands_home() {
        let mut completer = test_completer();
        let line = "cd ~";
        let pos = line.len();

        let suggestions = completer.complete(line, pos);

        assert!(!suggestions.is_empty(), "cd ~ should produce suggestions");
        for s in &suggestions {
            assert!(
                s.value.starts_with("~/"),
                "suggestion '{}' should start with ~/",
                s.value
            );
        }
    }

    #[test]
    #[serial]
    fn complete_tilde_slash_expands_home() {
        let mut completer = test_completer();
        let line = "cd ~/";
        let pos = line.len();

        let suggestions = completer.complete(line, pos);

        assert!(!suggestions.is_empty(), "cd ~/ should produce suggestions");
        for s in &suggestions {
            assert!(
                s.value.starts_with("~/"),
                "suggestion '{}' should start with ~/",
                s.value
            );
        }
    }

    #[test]
    #[serial]
    fn complete_git_checkout_includes_branches() {
        let tmpdir = create_test_git_repo();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = "git checkout test-";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"test-feature"),
            "git checkout should suggest 'test-feature': {values:?}"
        );
    }

    #[test]
    fn complete_git_non_branch_subcommand_no_branches() {
        let mut completer = test_completer();
        let line = "git add zzz_no_such_";
        let pos = line.len();

        let suggestions = completer.complete(line, pos);
        assert!(
            suggestions.is_empty(),
            "git add should not suggest anything for nonexistent prefix: {suggestions:?}"
        );
    }

    #[test]
    #[serial]
    fn complete_git_alias_triggers_branch_completion() {
        let tmpdir = create_test_git_repo_with_aliases();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = "git co test-";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"test-feature"),
            "git co (alias for checkout) should suggest 'test-feature': {values:?}"
        );
    }

    #[test]
    #[serial]
    fn complete_git_multi_word_alias_triggers_branch_completion() {
        let tmpdir = create_test_git_repo_with_aliases();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = "git nb test-";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"test-feature"),
            "git nb (alias for checkout -b) should suggest 'test-feature': {values:?}"
        );
    }

    // ── 先頭トークンがパスのときファイル補完へ回る統合テスト (#321) ──

    #[test]
    fn complete_first_token_absolute_path_shows_entries() {
        let (_tmpdir, path) = create_test_tree();
        let mut completer = test_completer();
        let line = format!("{path}/");
        let pos = line.len();

        let suggestions = completer.complete(&line, pos);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        // dirs_only=false の証明: ディレクトリとファイルの両方が候補に出る
        assert!(
            values.contains(&format!("{path}/Documents/").as_str()),
            "should include Documents/ dir: {values:?}"
        );
        assert!(
            values.contains(&format!("{path}/readme.txt").as_str()),
            "should include readme.txt file: {values:?}"
        );
    }

    #[test]
    #[serial]
    fn complete_first_token_relative_path_shows_entries() {
        let (tmpdir, _path) = create_test_tree();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = "./";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(!suggestions.is_empty(), "./ should produce suggestions");
        assert!(
            values.iter().any(|v| v.contains("Documents/")),
            "./ should include Documents/: {values:?}"
        );
    }

    #[test]
    #[serial]
    fn complete_first_token_tilde_expands_home() {
        let mut completer = test_completer();
        let line = "~/";
        let pos = line.len();

        let suggestions = completer.complete(line, pos);

        assert!(!suggestions.is_empty(), "~/ should produce suggestions");
        for s in &suggestions {
            assert!(
                s.value.starts_with("~/"),
                "suggestion '{}' should start with ~/",
                s.value
            );
        }
    }

    #[test]
    #[serial]
    fn complete_first_token_plain_command_uses_path() {
        let (tmpdir, _path) = create_test_tree();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = "c";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        // コマンド補完に流れている証明: ビルトインが出る
        assert!(
            values.contains(&"cd"),
            "plain 'c' should suggest builtin 'cd': {values:?}"
        );
        assert!(
            values.contains(&"cwd"),
            "plain 'c' should suggest builtin 'cwd': {values:?}"
        );
        // ファイル補完へは流れていない: temp ディレクトリのファイルは出ない
        assert!(
            !values.iter().any(|v| v.contains("readme.txt")),
            "plain 'c' should not suggest files from CWD: {values:?}"
        );
    }

    #[test]
    #[serial]
    fn complete_first_token_parent_relative_path_shows_entries() {
        let (tmpdir, _path) = create_test_tree();
        // tree ルート直下の Documents へ cd し、`../` で親（tree ルート）を補完する
        let subdir = tmpdir.path().join("Documents");
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(&subdir).unwrap();

        let mut completer = test_completer();
        let line = "../";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(!suggestions.is_empty(), "../ should produce suggestions");
        assert!(
            values.iter().any(|v| v.contains("Desktop/")),
            "../ from Documents should include sibling Desktop/: {values:?}"
        );
    }

    #[test]
    fn complete_first_token_mid_slash_prefix_filters() {
        let (_tmpdir, path) = create_test_tree();
        let mut completer = test_completer();
        // 絶対パス + 中間スラッシュ + 末尾プレフィックス `Do` → 前方一致フィルタ
        let line = format!("{path}/Do");
        let pos = line.len();

        let suggestions = completer.complete(&line, pos);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&format!("{path}/Documents/").as_str()),
            "should include Documents/: {values:?}"
        );
        assert!(
            values.contains(&format!("{path}/Downloads/").as_str()),
            "should include Downloads/: {values:?}"
        );
        assert!(
            !values.iter().any(|v| v.contains("Desktop")),
            "'Do' prefix should exclude Desktop: {values:?}"
        );
        assert!(
            !values.iter().any(|v| v.contains("readme.txt")),
            "'Do' prefix should exclude readme.txt: {values:?}"
        );
    }

    // ── 新規テスト (Task 1.3) ──

    #[test]
    #[serial]
    fn complete_pipeline_git_checkout_includes_branches() {
        // 'ls | git checkout test-' でパイプ後のセグメントがブランチ補完される。
        let tmpdir = create_test_git_repo();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = "ls | git checkout test-";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"test-feature"),
            "pipeline segment should suggest 'test-feature': {values:?}"
        );
    }

    #[test]
    #[serial]
    fn complete_quoted_partial_completes_files() {
        // 'echo "fo' が一時ディレクトリ内の fo* ファイルを補完する。
        let tmpdir = tempfile::tempdir().unwrap();
        fs::write(tmpdir.path().join("foo.txt"), "").unwrap();
        fs::write(tmpdir.path().join("bar.txt"), "").unwrap();

        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = r#"echo "fo"#;
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"foo.txt"),
            "quoted partial 'fo' should suggest foo.txt: {values:?}"
        );
        assert!(
            !values.iter().any(|v| v.contains("bar")),
            "'fo' prefix should exclude bar.txt: {values:?}"
        );
    }

    #[test]
    #[serial]
    fn complete_file_with_space_is_escaped_on_insert() {
        // 'foo bar.txt' という名前のファイルはエスケープされた値で挿入される。
        let tmpdir = tempfile::tempdir().unwrap();
        fs::write(tmpdir.path().join("foo bar.txt"), "").unwrap();

        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = "cat foo";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&r"foo\ bar.txt"),
            "space in filename should be escaped: {values:?}"
        );
        assert!(
            !values.contains(&"foo bar.txt"),
            "unescaped raw value should not be inserted directly: {values:?}"
        );
    }

    #[test]
    fn complete_pipeline_cd_offers_dirs_only() {
        // 'ls | cd ' はディレクトリのみを候補に出す。
        let (_tmpdir, path) = create_test_tree();
        let mut completer = test_completer();
        let line = format!("ls | cd {path}/");
        let pos = line.len();

        let suggestions = completer.complete(&line, pos);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(values.contains(&format!("{path}/Documents/").as_str()));
        assert!(
            !values.iter().any(|v| v.contains("readme.txt")),
            "cd after pipe should only offer directories: {values:?}"
        );
    }

    #[test]
    fn complete_builtin_suggestions_carry_descriptions() {
        let mut completer = test_completer();
        let line = "cd";
        let pos = line.len();

        let suggestions = completer.complete(line, pos);

        let cd_suggestion = suggestions
            .iter()
            .find(|s| s.value == "cd")
            .expect("'cd' builtin should be suggested");
        assert!(
            cd_suggestion.description.is_some(),
            "builtin 'cd' suggestion should carry a description"
        );
    }

    #[test]
    #[serial]
    fn complete_large_candidate_set_strips_descriptions() {
        // DESCRIPTION_LIMIT を超える候補数になる場面では description が全除去される。
        let tmpdir = tempfile::tempdir().unwrap();
        for i in 0..(DESCRIPTION_LIMIT + 5) {
            fs::write(tmpdir.path().join(format!("file{i}.txt")), "").unwrap();
        }

        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut completer = test_completer();
        let line = "ls file";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        assert!(
            suggestions.len() > DESCRIPTION_LIMIT,
            "test setup should exceed DESCRIPTION_LIMIT: {}",
            suggestions.len()
        );
        assert!(
            suggestions.iter().all(|s| s.description.is_none()),
            "descriptions should be stripped when candidate count exceeds the limit"
        );
    }

    // ── 新規テスト (Task 1.5: alias 対応補完) ──

    #[test]
    #[serial]
    fn alias_single_word_head_triggers_branch_completion() {
        // alias g=git: 'g checkout test-' がブランチ補完される
        // （alias → git → git-alias 連鎖は GitProvider の既存解決を利用）。
        let tmpdir = create_test_git_repo();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), "git".to_string());
        let (mut completer, _aliases) = test_completer_with_aliases(aliases);

        let line = "g checkout test-";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"test-feature"),
            "'g checkout test-' (alias g=git) should suggest 'test-feature': {values:?}"
        );
    }

    #[test]
    #[serial]
    fn alias_head_in_pipeline_triggers_branch_completion() {
        // 'ls | g co test-' (alias g=git, git alias co=checkout):
        // シェルエイリアス→git→git-alias の二重連鎖。
        let tmpdir = create_test_git_repo_with_aliases();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut aliases = HashMap::new();
        aliases.insert("g".to_string(), "git".to_string());
        let (mut completer, _aliases) = test_completer_with_aliases(aliases);

        let line = "ls | g co test-";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"test-feature"),
            "'ls | g co test-' should suggest 'test-feature': {values:?}"
        );
    }

    #[test]
    #[serial]
    fn alias_multi_word_value_triggers_branch_completion() {
        // alias gco="git checkout": 'gco test-' がブランチ補完される。
        let tmpdir = create_test_git_repo();
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(tmpdir.path()).unwrap();

        let mut aliases = HashMap::new();
        aliases.insert("gco".to_string(), "git checkout".to_string());
        let (mut completer, _aliases) = test_completer_with_aliases(aliases);

        let line = "gco test-";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        env::set_current_dir(&original_dir).unwrap();

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"test-feature"),
            "'gco test-' (alias gco=\"git checkout\") should suggest 'test-feature': {values:?}"
        );
    }

    #[test]
    fn alias_with_operator_value_falls_back_to_path_completion() {
        // alias lg="ls | grep": 演算子入りのエイリアス値は展開せず、
        // 今までどおりパス補完にフォールバックする（クラッシュしないことも確認）。
        let (_tmpdir, path) = create_test_tree();

        let mut aliases = HashMap::new();
        aliases.insert("lg".to_string(), "ls | grep".to_string());
        let (mut completer, _aliases) = test_completer_with_aliases(aliases);

        let line = format!("lg {path}/");
        let pos = line.len();
        let suggestions = completer.complete(&line, pos);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&format!("{path}/readme.txt").as_str()),
            "operator-bearing alias should fall back to plain path completion: {values:?}"
        );
    }

    // これらのテストは PATH 上の実バイナリと衝突しない一意な名前
    // (`zzjarvishtestalias`) を使う。ありふれた接頭辞（"g" 等）は開発機の
    // PATH 上に DESCRIPTION_LIMIT を超える数のコマンドがヒットしうるため、
    // orchestrator の「候補過多で description を一律除去する」ガード
    // （既存仕様。`complete_large_candidate_set_strips_descriptions` 参照）
    // に巻き込まれて description アサーションが不安定になる。

    #[test]
    fn alias_name_offered_as_first_token_candidate() {
        // 先頭トークンの補完候補にエイリアス名が description=alias 値で出る。
        let mut aliases = HashMap::new();
        aliases.insert("zzjarvishtestalias".to_string(), "git".to_string());
        let (mut completer, _aliases) = test_completer_with_aliases(aliases);

        let line = "zzjarvishtestalias";
        let pos = line.len();
        let suggestions = completer.complete(line, pos);

        let alias_suggestion = suggestions
            .iter()
            .find(|s| s.value == "zzjarvishtestalias")
            .expect("alias should be offered as a first-token candidate");
        assert_eq!(
            alias_suggestion.description.as_deref(),
            Some("git"),
            "alias candidate description should be the alias value"
        );
    }

    #[test]
    fn alias_name_removed_from_map_disappears_from_candidates() {
        let mut aliases = HashMap::new();
        aliases.insert("zzjarvishtestalias".to_string(), "git".to_string());
        let (mut completer, aliases_arc) = test_completer_with_aliases(aliases);

        let line = "zzjarvishtestalias";
        let pos = line.len();
        let before = completer.complete(line, pos);
        assert!(
            before.iter().any(|s| s.value == "zzjarvishtestalias"),
            "alias should be offered before removal: {before:?}"
        );

        aliases_arc.write().unwrap().remove("zzjarvishtestalias");

        let after = completer.complete(line, pos);
        assert!(
            !after.iter().any(|s| s.value == "zzjarvishtestalias"),
            "alias should disappear once removed from the shared map: {after:?}"
        );
    }

    // ── DESCRIPTION_LIMIT 境界値テスト (should_strip_descriptions 単体) ──

    #[test]
    fn should_strip_descriptions_at_exact_limit_survives() {
        // ちょうど DESCRIPTION_LIMIT 件なら description は生存する。
        assert!(!should_strip_descriptions(DESCRIPTION_LIMIT));
    }

    #[test]
    fn should_strip_descriptions_one_over_limit_strips() {
        // DESCRIPTION_LIMIT を 1 件でも超えたら全除去される。
        assert!(should_strip_descriptions(DESCRIPTION_LIMIT + 1));
    }

    // ── apply_shell_alias 単体テスト（エッジケース） ──

    #[test]
    fn apply_shell_alias_operator_value_skips_expansion() {
        // alias 値に演算子 (`|`) が含まれる場合は展開しない。
        let mut ctx = extract_context("lg ", "lg ".len());
        let mut aliases = HashMap::new();
        aliases.insert("lg".to_string(), "ls | grep".to_string());

        apply_shell_alias(&mut ctx, &aliases);

        assert_eq!(
            ctx.expanded_head, None,
            "operator-bearing alias value should not be expanded"
        );
    }

    #[test]
    fn apply_shell_alias_unparseable_value_skips_safely() {
        // alias 値が split_quoted でパースエラーになる場合（未閉クォート）は
        // パニックせず安全にスキップする。
        let mut ctx = extract_context("bad ", "bad ".len());
        let mut aliases = HashMap::new();
        aliases.insert("bad".to_string(), "ls 'foo".to_string());

        apply_shell_alias(&mut ctx, &aliases);

        assert_eq!(
            ctx.expanded_head, None,
            "unparseable alias value should be skipped safely, not panic"
        );
    }

    #[test]
    fn apply_shell_alias_empty_value_is_safe() {
        // alias 値が空文字列でもクラッシュせず、安全な挙動（展開なし、
        // または空展開）になる。
        let mut ctx = extract_context("empty ", "empty ".len());
        let mut aliases = HashMap::new();
        aliases.insert("empty".to_string(), "".to_string());

        apply_shell_alias(&mut ctx, &aliases);

        // 実際の安全な挙動: split_quoted("") は空のトークン列を返すため
        // has_operator は false になり、expanded_head は Some(空 + 後続トークン) になる。
        assert_eq!(
            ctx.expanded_head,
            Some(Vec::new()),
            "empty alias value should expand to an empty head, not panic"
        );
    }

    #[test]
    fn apply_shell_alias_chained_alias_is_single_pass_only() {
        // a=b, b=git のとき 'a ' を展開すると expanded_head は "b" のまま
        // （"b" を再度 alias マップで引いて "git" まで再帰解決したりしない）。
        let mut ctx = extract_context("a ", "a ".len());
        let mut aliases = HashMap::new();
        aliases.insert("a".to_string(), "b".to_string());
        aliases.insert("b".to_string(), "git".to_string());

        apply_shell_alias(&mut ctx, &aliases);

        assert_eq!(
            ctx.expanded_head,
            Some(vec!["b".to_string()]),
            "alias expansion must be single-pass only, not recursively resolved to 'git'"
        );
    }

    #[test]
    fn alias_defined_between_complete_calls_is_picked_up_by_second() {
        // ヘッドライン UX: セッション中に `alias` ビルトインで定義した直後の
        // 次の Tab に即座に反映される（共有 Arc — reload 不要）。
        let (mut completer, aliases_arc) = test_completer_with_aliases(HashMap::new());

        let line = "zzjarvishtestalias";
        let pos = line.len();
        let first = completer.complete(line, pos);
        assert!(
            !first.iter().any(|s| s.value == "zzjarvishtestalias"),
            "alias should not be offered before it is defined: {first:?}"
        );

        aliases_arc
            .write()
            .unwrap()
            .insert("zzjarvishtestalias".to_string(), "git".to_string());

        let second = completer.complete(line, pos);
        let suggestion = second
            .iter()
            .find(|s| s.value == "zzjarvishtestalias")
            .expect("alias defined between complete() calls should be visible immediately");
        assert_eq!(
            suggestion.description.as_deref(),
            Some("git"),
            "newly defined alias should carry its value as description"
        );
    }
}
