//! コマンド補完 — Tab キーで PATH コマンド名・ビルトイン・ファイルパスを補完
//!
//! - 先頭トークン: PATH 内の実行可能コマンド + ビルトイン (cd, cwd, exit)
//! - それ以降: カレントディレクトリ基準のファイル / ディレクトリ名
//!
//! `InputClassifier` の PATH キャッシュを共有参照し、
//! `export PATH=...` 等による動的変更を即座に反映する。

use std::env;
use std::fs;
use std::sync::Arc;

use reedline::{Completer, Span, Suggestion};

use crate::engine::classifier::InputClassifier;

/// ビルトインコマンド名（補完候補に常に含める）
const BUILTIN_COMMANDS: &[&str] = &["cd", "cwd", "exit", "export", "unset", "help", "history"];

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

                let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);

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
