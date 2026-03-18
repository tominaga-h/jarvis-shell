//! ファイル / ディレクトリパス補完

use std::fs;

use reedline::{Span, Suggestion};

use crate::engine::expand;

impl super::JarvishCompleter {
    /// ファイル / ディレクトリパス補完（第 2 トークン以降）
    ///
    /// `dirs_only` が true の場合はディレクトリのみを候補に含める（`cd` 用）。
    pub(super) fn complete_path(
        &self,
        partial: &str,
        span: Span,
        dirs_only: bool,
    ) -> Vec<Suggestion> {
        let (search_dir, prefix, original_dir) = Self::split_path_prefix(partial);

        let entries = match fs::read_dir(&search_dir) {
            Ok(e) => e,
            Err(_) => return vec![],
        };

        let mut suggestions: Vec<Suggestion> = entries
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

    /// 部分パス文字列を「検索ディレクトリ」「ファイル名プレフィックス」「オリジナル dir 部分」に分割する。
    ///
    /// `expand::expand_token` でチルダ (`~`) と環境変数 (`$HOME` 等) を展開した上で
    /// ディレクトリ読み取り用パスを返す。補完候補の表示値にはオリジナル（展開前）の
    /// ディレクトリ部分を使う。
    ///
    /// 戻り値: `(search_dir, file_prefix, original_dir)`
    pub(super) fn split_path_prefix(partial: &str) -> (String, String, String) {
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
}

#[cfg(test)]
mod tests {
    use reedline::Span;
    use serial_test::serial;
    use std::env;
    use std::fs;

    use std::sync::{Arc, RwLock};

    use crate::cli::completer::JarvishCompleter;
    use crate::config::CompletionConfig;

    fn test_completer() -> JarvishCompleter {
        let commands = CompletionConfig::default().git_branch_commands;
        JarvishCompleter::new(Arc::new(RwLock::new(commands)))
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
        assert!(values.contains(&format!("{path}/Documents/").as_str()));
        assert!(values.contains(&format!("{path}/Desktop/").as_str()));
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

    #[test]
    fn complete_nonexistent_dir_returns_empty() {
        let completer = test_completer();
        let partial = "/nonexistent_dir_12345/";
        let span = Span::new(3, 3 + partial.len());

        let suggestions = completer.complete_path(partial, span, false);
        assert!(suggestions.is_empty());
    }
}
