//! ファイル / ディレクトリパス補完

use std::fs;

use crate::engine::expand;

use super::context::CompletionContext;
use super::provider::{Candidate, CompletionProvider};

/// ファイル / ディレクトリパス補完プロバイダ（終端フォールバック）。
///
/// 常に `Some` を返す（担当外という概念がない、最後の砦のプロバイダ）。
/// `ctx.head_command() == Some("cd")` の場合はディレクトリのみを候補に含める。
/// 先頭トークンがパスらしく見える場合（`looks_like_path`）も、通常のファイル
/// 補完（`dirs_only = false`）としてここで処理する（#321 の挙動を維持）。
pub(super) struct PathProvider;

impl CompletionProvider for PathProvider {
    fn provide(&self, ctx: &CompletionContext) -> Option<Vec<Candidate>> {
        let dirs_only = if ctx.is_first_token {
            // 先頭トークンがパスらしくない場合は CommandProvider の担当だが、
            // 万一ここに到達しても（フォールバック）ディレクトリ限定はしない。
            false
        } else {
            ctx.head_command() == Some("cd")
        };

        Some(complete_path(&ctx.partial, dirs_only))
    }
}

/// パス補完候補を計算する。
///
/// `dirs_only` が true の場合はディレクトリのみを候補に含める（`cd` 用）。
fn complete_path(partial: &str, dirs_only: bool) -> Vec<Candidate> {
    let (search_dir, prefix, original_dir) = split_path_prefix(partial);

    let entries = match fs::read_dir(&search_dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    let mut candidates: Vec<Candidate> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();

            if !name.starts_with(&prefix) {
                return None;
            }
            // ドットファイルは入力が `.` で始まるときのみ表示
            if name.starts_with('.') && !prefix.starts_with('.') {
                return None;
            }

            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);

            if dirs_only && !is_dir {
                return None;
            }

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

            Some(Candidate {
                value,
                description: None,
                append_whitespace: !is_dir,
            })
        })
        .collect();

    candidates.sort_by(|a, b| a.value.cmp(&b.value));
    candidates
}

/// 部分パス文字列を「検索ディレクトリ」「ファイル名プレフィックス」「オリジナル dir 部分」に分割する。
///
/// `expand::expand_token` でチルダ (`~`) と環境変数 (`$HOME` 等) を展開した上で
/// ディレクトリ読み取り用パスを返す。補完候補の表示値にはオリジナル（展開前）の
/// ディレクトリ部分を使う。
///
/// 戻り値: `(search_dir, file_prefix, original_dir)`
fn split_path_prefix(partial: &str) -> (String, String, String) {
    // `~` 単体はホームディレクトリそのものを指すため `~/` として扱う
    let effective = if partial == "~" { "~/" } else { partial };

    // チルダ・環境変数を展開
    let expanded = expand::expand_token(effective);

    if let Some(idx) = expanded.rfind('/') {
        let search_dir = expanded[..=idx].to_string();
        let file_part = expanded[idx + 1..].to_string();

        // オリジナル（展開前）のディレクトリ部分を計算
        let original_dir = if let Some(orig_idx) = partial.rfind('/') {
            partial[..=orig_idx].to_string()
        } else {
            format!("{}/", partial)
        };

        (search_dir, file_part, original_dir)
    } else {
        (".".to_string(), partial.to_string(), String::new())
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;
    use std::env;
    use std::fs;

    use super::*;

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

    // ── split_path_prefix テスト ──

    #[test]
    fn split_relative_path() {
        let (search_dir, prefix, original_dir) = split_path_prefix("src/ma");
        assert_eq!(search_dir, "src/");
        assert_eq!(prefix, "ma");
        assert_eq!(original_dir, "src/");
    }

    #[test]
    fn split_bare_filename() {
        let (search_dir, prefix, original_dir) = split_path_prefix("file");
        assert_eq!(search_dir, ".");
        assert_eq!(prefix, "file");
        assert_eq!(original_dir, "");
    }

    #[test]
    #[serial]
    fn split_tilde_with_slash() {
        let home = env::var("HOME").unwrap();
        let (search_dir, prefix, original_dir) = split_path_prefix("~/Do");
        assert_eq!(search_dir, format!("{home}/"));
        assert_eq!(prefix, "Do");
        assert_eq!(original_dir, "~/");
    }

    #[test]
    #[serial]
    fn split_tilde_alone() {
        let home = env::var("HOME").unwrap();
        let (search_dir, prefix, original_dir) = split_path_prefix("~");
        assert_eq!(search_dir, format!("{home}/"));
        assert_eq!(prefix, "");
        assert_eq!(original_dir, "~/");
    }

    #[test]
    #[serial]
    fn split_tilde_trailing_slash() {
        let home = env::var("HOME").unwrap();
        let (search_dir, prefix, original_dir) = split_path_prefix("~/");
        assert_eq!(search_dir, format!("{home}/"));
        assert_eq!(prefix, "");
        assert_eq!(original_dir, "~/");
    }

    #[test]
    fn split_absolute_path() {
        let (search_dir, prefix, original_dir) = split_path_prefix("/tmp/te");
        assert_eq!(search_dir, "/tmp/");
        assert_eq!(prefix, "te");
        assert_eq!(original_dir, "/tmp/");
    }

    // ── complete_path テスト ──

    #[test]
    fn complete_path_absolute_with_trailing_slash() {
        let (_tmpdir, path) = create_test_tree();
        let partial = format!("{path}/");

        let candidates = complete_path(&partial, false);

        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&format!("{path}/Documents/").as_str()));
        assert!(values.contains(&format!("{path}/Desktop/").as_str()));
        assert!(values.contains(&format!("{path}/readme.txt").as_str()));
        assert!(!values.iter().any(|v| v.contains(".hidden_dir")));
        assert!(!values.iter().any(|v| v.contains(".dotfile")));
    }

    #[test]
    fn complete_path_absolute_with_prefix() {
        let (_tmpdir, path) = create_test_tree();
        let partial = format!("{path}/Do");

        let candidates = complete_path(&partial, false);

        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&format!("{path}/Documents/").as_str()));
        assert!(values.contains(&format!("{path}/Downloads/").as_str()));
        assert!(!values.iter().any(|v| v.contains("Desktop")));
        assert!(!values.iter().any(|v| v.contains("readme")));
    }

    #[test]
    fn complete_path_dirs_only() {
        let (_tmpdir, path) = create_test_tree();
        let partial = format!("{path}/");

        let candidates = complete_path(&partial, true);

        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&format!("{path}/Documents/").as_str()));
        assert!(values.contains(&format!("{path}/Desktop/").as_str()));
        assert!(!values.iter().any(|v| v.contains("readme.txt")));
    }

    #[test]
    fn complete_path_dot_prefix_shows_hidden() {
        let (_tmpdir, path) = create_test_tree();
        let partial = format!("{path}/.");

        let candidates = complete_path(&partial, false);

        let values: Vec<&str> = candidates.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&format!("{path}/.hidden_dir/").as_str()));
        assert!(values.contains(&format!("{path}/.dotfile").as_str()));
    }

    #[test]
    fn complete_nonexistent_dir_returns_empty() {
        let partial = "/nonexistent_dir_12345/";

        let candidates = complete_path(partial, false);
        assert!(candidates.is_empty());
    }
}
