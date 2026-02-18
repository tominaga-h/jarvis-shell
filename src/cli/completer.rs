//! コマンド補完 — Tab キーで PATH コマンド名・ビルトイン・ファイルパスを補完
//!
//! - 先頭トークン: PATH 内の実行可能コマンド + ビルトイン (cd, cwd, exit)
//! - それ以降: カレントディレクトリ基準のファイル / ディレクトリ名
//!
//! `InputClassifier` の PATH キャッシュを共有参照し、
//! `export PATH=...` 等による動的変更を即座に反映する。

use std::fs;
use std::sync::Arc;

use reedline::{Completer, Span, Suggestion};

use crate::engine::classifier::InputClassifier;
use crate::engine::expand;

/// ビルトインコマンド名（補完候補に常に含める）
const BUILTIN_COMMANDS: &[&str] = &["cd", "cwd", "exit", "export", "unset", "help", "history"];

/// ブランチ名補完を提供する git サブコマンド
const GIT_BRANCH_SUBCOMMANDS: &[&str] = &[
    "checkout",
    "switch",
    "merge",
    "rebase",
    "branch",
    "diff",
    "log",
    "cherry-pick",
    "reset",
];

/// Jarvish 用の補完エンジン
///
/// `InputClassifier` の PATH キャッシュを `Arc` で共有し、
/// PATH 変更時のリロードが自動的に補完候補にも反映される。
pub struct JarvishCompleter {
    /// PATH キャッシュの共有参照元
    classifier: Arc<InputClassifier>,
}

impl JarvishCompleter {
    /// `InputClassifier` の PATH キャッシュを共有して初期化する。
    pub fn new(classifier: Arc<InputClassifier>) -> Self {
        Self { classifier }
    }

    // ========== 補完ロジック ==========

    /// コマンド名補完（先頭トークン）
    ///
    /// `InputClassifier` の PATH キャッシュ + ビルトインコマンドから候補を生成する。
    fn complete_command(&self, partial: &str, span: Span) -> Vec<Suggestion> {
        let path_commands = self.classifier.path_commands();

        // PATH コマンド + ビルトインからマッチするものを収集
        let mut matches: Vec<&str> = path_commands
            .iter()
            .map(|s| s.as_str())
            .chain(BUILTIN_COMMANDS.iter().copied())
            .filter(|cmd| cmd.starts_with(partial))
            .collect();

        // ソート & 重複除去
        matches.sort_unstable();
        matches.dedup();

        matches
            .into_iter()
            .map(|cmd| Suggestion {
                value: cmd.to_string(),
                description: None,
                style: None,
                extra: None,
                span,
                append_whitespace: true,
                match_indices: None,
            })
            .collect()
    }

    /// ファイル / ディレクトリパス補完（第 2 トークン以降）
    ///
    /// `dirs_only` が true の場合はディレクトリのみを候補に含める（`cd` 用）。
    fn complete_path(&self, partial: &str, span: Span, dirs_only: bool) -> Vec<Suggestion> {
        let (search_dir, prefix, original_dir) = Self::split_path_prefix(partial);

        let entries = match fs::read_dir(&search_dir) {
            Ok(e) => e,
            Err(_) => return vec![],
        };

        let mut suggestions: Vec<Suggestion> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();

                // プレフィックスが一致しなければスキップ
                if !name.starts_with(&prefix) {
                    return None;
                }
                // ドットファイルは入力が `.` で始まるときのみ表示
                if name.starts_with('.') && !prefix.starts_with('.') {
                    return None;
                }

                let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);

                // cd ではディレクトリのみ補完
                if dirs_only && !is_dir {
                    return None;
                }

                // 補完値: オリジナルのディレクトリ部分を保持し、ディレクトリには `/` を付与
                let value = if !original_dir.is_empty() {
                    if is_dir {
                        format!("{original_dir}{name}/")
                    } else {
                        format!("{original_dir}{name}")
                    }
                } else if is_dir {
                    format!("{name}/")
                } else {
                    name
                };

                Some(Suggestion {
                    value,
                    description: None,
                    style: None,
                    extra: None,
                    span,
                    append_whitespace: !is_dir,
                    match_indices: None,
                })
            })
            .collect();

        suggestions.sort_by(|a, b| a.value.cmp(&b.value));
        suggestions
    }

    /// Git ブランチ名補完
    ///
    /// `git branch --format=%(refname:short)` を実行してローカルブランチ一覧を取得し、
    /// `partial` に前方一致するものを候補として返す。
    /// git リポジトリ外など実行失敗時は空ベクタを返す。
    fn complete_git_branch(&self, partial: &str, span: Span) -> Vec<Suggestion> {
        let output = match std::process::Command::new("git")
            .args(["branch", "--format=%(refname:short)"])
            .stderr(std::process::Stdio::null())
            .output()
        {
            Ok(o) if o.status.success() => o,
            _ => return vec![],
        };

        let stdout = String::from_utf8_lossy(&output.stdout);

        let mut branches: Vec<&str> = stdout.lines().filter(|b| b.starts_with(partial)).collect();

        branches.sort_unstable();
        branches.dedup();

        branches
            .into_iter()
            .map(|branch| Suggestion {
                value: branch.to_string(),
                description: None,
                style: None,
                extra: None,
                span,
                append_whitespace: true,
                match_indices: None,
            })
            .collect()
    }

    // ========== ヘルパー ==========

    /// 部分パス文字列を「検索ディレクトリ」「ファイル名プレフィックス」「オリジナル dir 部分」に分割する。
    ///
    /// `expand::expand_token` でチルダ (`~`) と環境変数 (`$HOME` 等) を展開した上で
    /// ディレクトリ読み取り用パスを返す。補完候補の表示値にはオリジナル（展開前）の
    /// ディレクトリ部分を使う。
    ///
    /// 戻り値: `(search_dir, file_prefix, original_dir)`
    ///
    /// 例:
    /// - `"src/ma"`    → (`"src/"`,    `"ma"`,  `"src/"`)
    /// - `"file"`      → (`"."`,       `"file"`, `""`)
    /// - `"~/do"`      → (`"$HOME/"`,  `"do"`,  `"~/"`)
    /// - `"~"`         → (`"$HOME/"`,  `""`,    `"~/"`)
    /// - `"$HOME/do"`  → (`"$HOME/"`,  `"do"`,  `"$HOME/"`)
    fn split_path_prefix(partial: &str) -> (String, String, String) {
        // `~` 単体はホームディレクトリそのものを指すため `~/` として扱う
        let effective = if partial == "~" { "~/" } else { partial };

        // チルダ・環境変数を展開
        let expanded = expand::expand_token(effective);

        if let Some(idx) = expanded.rfind('/') {
            let search_dir = expanded[..=idx].to_string();
            let file_part = expanded[idx + 1..].to_string();

            // オリジナル（展開前）のディレクトリ部分を計算
            // 補完候補の value に使い、ユーザーの入力形式を保持する
            let original_dir = if let Some(orig_idx) = partial.rfind('/') {
                partial[..=orig_idx].to_string()
            } else {
                // `~` のみなど、展開前にはスラッシュがないケース → `~/` を使用
                format!("{}/", partial)
            };

            (search_dir, file_part, original_dir)
        } else {
            // 展開後もスラッシュがない → カレントディレクトリで検索
            (".".to_string(), partial.to_string(), String::new())
        }
    }

    /// カーソルより前の文字列から、補完対象トークンの開始位置を返す。
    fn token_start(line: &str, pos: usize) -> usize {
        let before = &line[..pos];
        before.rfind(' ').map(|i| i + 1).unwrap_or(0)
    }

    /// カーソルが先頭トークン上にあるかを判定する。
    fn is_first_token(line: &str, pos: usize) -> bool {
        !line[..pos].contains(' ')
    }
}

impl Completer for JarvishCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let start = Self::token_start(line, pos);
        let partial = &line[start..pos];
        let span = Span::new(start, pos);

        if Self::is_first_token(line, pos) {
            self.complete_command(partial, span)
        } else {
            let tokens: Vec<&str> = line[..pos].split_whitespace().collect();
            let first_token = tokens.first().copied().unwrap_or("");

            if first_token == "git"
                && tokens.len() >= 2
                && GIT_BRANCH_SUBCOMMANDS.contains(&tokens[1])
            {
                self.complete_git_branch(partial, span)
            } else {
                let dirs_only = first_token == "cd";
                self.complete_path(partial, span, dirs_only)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;
    use std::fs;

    /// テスト用の completer を作成するヘルパー
    fn test_completer() -> JarvishCompleter {
        JarvishCompleter::new(Arc::new(InputClassifier::new()))
    }

    /// テスト用ディレクトリ構造を作成するヘルパー
    /// 返り値: (tmpdir, 作成したディレクトリのパス文字列)
    fn create_test_tree() -> (tempfile::TempDir, String) {
        let tmpdir = tempfile::tempdir().expect("failed to create tempdir");
        let base = tmpdir.path();

        // ディレクトリを作成
        fs::create_dir(base.join("Documents")).unwrap();
        fs::create_dir(base.join("Desktop")).unwrap();
        fs::create_dir(base.join("Downloads")).unwrap();
        fs::create_dir(base.join(".hidden_dir")).unwrap();

        // ファイルを作成
        fs::write(base.join("readme.txt"), "").unwrap();
        fs::write(base.join(".dotfile"), "").unwrap();

        let path = base.to_str().unwrap().to_string();
        (tmpdir, path)
    }

    // ── split_path_prefix テスト ──

    #[test]
    fn split_relative_path() {
        let (search_dir, prefix, original_dir) = JarvishCompleter::split_path_prefix("src/ma");
        assert_eq!(search_dir, "src/");
        assert_eq!(prefix, "ma");
        assert_eq!(original_dir, "src/");
    }

    #[test]
    fn split_bare_filename() {
        let (search_dir, prefix, original_dir) = JarvishCompleter::split_path_prefix("file");
        assert_eq!(search_dir, ".");
        assert_eq!(prefix, "file");
        assert_eq!(original_dir, "");
    }

    #[test]
    #[serial]
    fn split_tilde_with_slash() {
        let home = env::var("HOME").unwrap();
        let (search_dir, prefix, original_dir) = JarvishCompleter::split_path_prefix("~/Do");
        assert_eq!(search_dir, format!("{home}/"));
        assert_eq!(prefix, "Do");
        assert_eq!(original_dir, "~/");
    }

    #[test]
    #[serial]
    fn split_tilde_alone() {
        let home = env::var("HOME").unwrap();
        let (search_dir, prefix, original_dir) = JarvishCompleter::split_path_prefix("~");
        // `~` → `~/` として扱われ、HOME の中身を一覧する形になる
        assert_eq!(search_dir, format!("{home}/"));
        assert_eq!(prefix, "");
        assert_eq!(original_dir, "~/");
    }

    #[test]
    #[serial]
    fn split_tilde_trailing_slash() {
        let home = env::var("HOME").unwrap();
        let (search_dir, prefix, original_dir) = JarvishCompleter::split_path_prefix("~/");
        assert_eq!(search_dir, format!("{home}/"));
        assert_eq!(prefix, "");
        assert_eq!(original_dir, "~/");
    }

    #[test]
    fn split_absolute_path() {
        let (search_dir, prefix, original_dir) = JarvishCompleter::split_path_prefix("/tmp/te");
        assert_eq!(search_dir, "/tmp/");
        assert_eq!(prefix, "te");
        assert_eq!(original_dir, "/tmp/");
    }

    // ── complete_path テスト ──

    #[test]
    fn complete_path_absolute_with_trailing_slash() {
        let (_tmpdir, path) = create_test_tree();
        let completer = test_completer();
        let partial = format!("{path}/");
        let span = Span::new(3, 3 + partial.len());

        let suggestions = completer.complete_path(&partial, span, false);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(values.contains(&format!("{path}/Documents/").as_str()));
        assert!(values.contains(&format!("{path}/Desktop/").as_str()));
        assert!(values.contains(&format!("{path}/readme.txt").as_str()));
        // ドットファイルはプレフィックスが `.` で始まらない限り含まれない
        assert!(!values.iter().any(|v| v.contains(".hidden_dir")));
        assert!(!values.iter().any(|v| v.contains(".dotfile")));
    }

    #[test]
    fn complete_path_absolute_with_prefix() {
        let (_tmpdir, path) = create_test_tree();
        let completer = test_completer();
        let partial = format!("{path}/Do");
        let span = Span::new(3, 3 + partial.len());

        let suggestions = completer.complete_path(&partial, span, false);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        // "Do" にマッチするのは Documents と Downloads（Desktop は "De" で始まる）
        assert!(values.contains(&format!("{path}/Documents/").as_str()));
        assert!(values.contains(&format!("{path}/Downloads/").as_str()));
        assert!(!values.iter().any(|v| v.contains("Desktop")));
        assert!(!values.iter().any(|v| v.contains("readme")));
    }

    #[test]
    fn complete_path_dirs_only() {
        let (_tmpdir, path) = create_test_tree();
        let completer = test_completer();
        let partial = format!("{path}/");
        let span = Span::new(3, 3 + partial.len());

        let suggestions = completer.complete_path(&partial, span, true);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        // ディレクトリは含まれる
        assert!(values.contains(&format!("{path}/Documents/").as_str()));
        assert!(values.contains(&format!("{path}/Desktop/").as_str()));
        // ファイルは含まれない
        assert!(!values.iter().any(|v| v.contains("readme.txt")));
    }

    #[test]
    fn complete_path_dot_prefix_shows_hidden() {
        let (_tmpdir, path) = create_test_tree();
        let completer = test_completer();
        let partial = format!("{path}/.");
        let span = Span::new(3, 3 + partial.len());

        let suggestions = completer.complete_path(&partial, span, false);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(values.contains(&format!("{path}/.hidden_dir/").as_str()));
        assert!(values.contains(&format!("{path}/.dotfile").as_str()));
    }

    // ── complete (Completer trait) テスト ──

    #[test]
    fn complete_cd_dirs_only_via_trait() {
        let (_tmpdir, path) = create_test_tree();
        let mut completer = test_completer();
        let line = format!("cd {path}/");
        let pos = line.len();

        let suggestions = completer.complete(&line, pos);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        // ディレクトリは含まれる
        assert!(values.contains(&format!("{path}/Documents/").as_str()));
        // ファイルは含まれない
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
        // ディレクトリもファイルも含まれる
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

        // ~ が展開されて HOME ディレクトリの中身が補完候補になる
        assert!(!suggestions.is_empty(), "cd ~ should produce suggestions");
        // 全ての候補が ~/ で始まること
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
    fn complete_nonexistent_dir_returns_empty() {
        let completer = test_completer();
        let partial = "/nonexistent_dir_12345/";
        let span = Span::new(3, 3 + partial.len());

        let suggestions = completer.complete_path(partial, span, false);
        assert!(suggestions.is_empty());
    }

    // ── ヘルパーメソッドテスト ──

    #[test]
    fn token_start_no_space() {
        assert_eq!(JarvishCompleter::token_start("ls", 2), 0);
    }

    #[test]
    fn token_start_after_command() {
        assert_eq!(JarvishCompleter::token_start("cd /tmp", 7), 3);
    }

    #[test]
    fn is_first_token_true() {
        assert!(JarvishCompleter::is_first_token("ls", 2));
    }

    #[test]
    fn is_first_token_false() {
        assert!(!JarvishCompleter::is_first_token("cd /tmp", 7));
    }

    // ── Git ブランチ補完テスト ──

    #[test]
    fn complete_git_branch_returns_candidates() {
        let completer = test_completer();
        let span = Span::new(0, 0);

        let suggestions = completer.complete_git_branch("", span);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"main"),
            "main branch should be in suggestions: {values:?}"
        );
    }

    #[test]
    fn complete_git_branch_filters_by_prefix() {
        let completer = test_completer();
        let span = Span::new(0, 4);

        let suggestions = completer.complete_git_branch("main", span);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(values.contains(&"main"));
        for v in &values {
            assert!(v.starts_with("main"), "'{v}' should start with 'main'");
        }
    }

    #[test]
    fn complete_git_branch_nonexistent_prefix_returns_empty() {
        let completer = test_completer();
        let span = Span::new(0, 0);

        let suggestions = completer.complete_git_branch("zzz_no_such_branch_", span);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn complete_git_checkout_includes_branches() {
        let mut completer = test_completer();
        let line = "git checkout m";
        let pos = line.len();

        let suggestions = completer.complete(line, pos);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            values.contains(&"main"),
            "git checkout should suggest 'main': {values:?}"
        );
    }

    #[test]
    fn complete_git_non_branch_subcommand_no_branches() {
        let mut completer = test_completer();
        let line = "git add m";
        let pos = line.len();

        let suggestions = completer.complete(line, pos);

        let values: Vec<&str> = suggestions.iter().map(|s| s.value.as_str()).collect();
        assert!(
            !values.contains(&"main"),
            "git add should not suggest branches: {values:?}"
        );
    }
}
