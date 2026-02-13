//! コマンド補完 — Tab キーで PATH コマンド名・ビルトイン・ファイルパスを補完
//!
//! - 先頭トークン: PATH 内の実行可能コマンド + ビルトイン (cd, cwd, exit)
//! - それ以降: カレントディレクトリ基準のファイル / ディレクトリ名

use std::collections::HashSet;
use std::env;
use std::fs;

use reedline::{Completer, Span, Suggestion};

/// Jarvish 用の補完エンジン
pub struct JarvishCompleter {
    /// PATH 内の実行可能コマンド名 + ビルトイン（ソート済み）
    commands: Vec<String>,
}

impl JarvishCompleter {
    /// PATH を走査し、ビルトインコマンドとマージして初期化する。
    pub fn new() -> Self {
        let mut set = Self::build_path_cache();

        // ビルトインコマンドを追加
        for b in &["cd", "cwd", "exit"] {
            set.insert((*b).to_string());
        }

        let mut commands: Vec<String> = set.into_iter().collect();
        commands.sort();

        Self { commands }
    }

    // ========== 補完ロジック ==========

    /// コマンド名補完（先頭トークン）
    fn complete_command(&self, partial: &str, span: Span) -> Vec<Suggestion> {
        self.commands
            .iter()
            .filter(|cmd| cmd.starts_with(partial))
            .map(|cmd| Suggestion {
                value: cmd.clone(),
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
    fn complete_path(&self, partial: &str, span: Span) -> Vec<Suggestion> {
        let (search_dir, prefix) = Self::split_path_prefix(partial);

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

                let is_dir = entry
                    .file_type()
                    .map(|ft| ft.is_dir())
                    .unwrap_or(false);

                // 補完値: ディレクトリ部分を保持し、ディレクトリには `/` を付与
                let value = if let Some(idx) = partial.rfind('/') {
                    let dir_part = &partial[..=idx];
                    if is_dir {
                        format!("{dir_part}{name}/")
                    } else {
                        format!("{dir_part}{name}")
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

    // ========== ヘルパー ==========

    /// PATH 環境変数を走査し、実行可能ファイル名を収集する。
    fn build_path_cache() -> HashSet<String> {
        let mut commands = HashSet::new();

        let path_var = match env::var("PATH") {
            Ok(p) => p,
            Err(_) => return commands,
        };

        for dir in env::split_paths(&path_var) {
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };

            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if let Ok(meta) = fs::metadata(entry.path()) {
                        if meta.is_file() {
                            commands.insert(name.to_string());
                        }
                    }
                }
            }
        }

        commands
    }

    /// 部分パス文字列を「検索ディレクトリ」と「ファイル名プレフィックス」に分割する。
    ///
    /// 例:
    /// - `"src/ma"` → (`"src/"`, `"ma"`)
    /// - `"file"` → (`"."`, `"file"`)
    /// - `"~/do"` → (`"$HOME/"`, `"do"`)
    fn split_path_prefix(partial: &str) -> (String, String) {
        if let Some(idx) = partial.rfind('/') {
            let dir_part = &partial[..=idx];
            let file_part = &partial[idx + 1..];

            // チルダ展開
            let expanded = if dir_part.starts_with("~/") {
                if let Some(home) = env::var_os("HOME") {
                    format!("{}{}", home.to_string_lossy(), &dir_part[1..])
                } else {
                    dir_part.to_string()
                }
            } else {
                dir_part.to_string()
            };

            (expanded, file_part.to_string())
        } else {
            (".".to_string(), partial.to_string())
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
            self.complete_path(partial, span)
        }
    }
}
