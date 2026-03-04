//! Git ステータスの取得・フォーマット

use std::path::Path;

use super::super::color::{cyan, green, red, yellow};

/// 指定ディレクトリの Git ブランチ名を取得する。Git リポジトリ外の場合は None。
pub(super) fn current_git_branch_at(cwd: &Path) -> Option<String> {
    let repo = git2::Repository::discover(cwd).ok()?;
    let head = repo.head().ok()?;
    head.shorthand().map(|s| s.to_string())
}

/// 指定ディレクトリの Git リポジトリ内のファイルステータスを集計する。
///
/// 戻り値: `(added, modified, deleted)` のタプル。
/// - added: 新規ファイル数（ステージ済み + untracked）
/// - modified: 変更ファイル数（ステージ済み + ワーキングツリー）
/// - deleted: 削除ファイル数（ステージ済み + ワーキングツリー）
fn git_file_status_counts_at(cwd: &Path) -> Option<(usize, usize, usize)> {
    let repo = git2::Repository::discover(cwd).ok()?;
    let statuses = repo.statuses(None).ok()?;

    let mut added = 0usize;
    let mut modified = 0usize;
    let mut deleted = 0usize;

    for entry in statuses.iter() {
        let s = entry.status();
        if s.intersects(git2::Status::INDEX_NEW | git2::Status::WT_NEW) {
            added += 1;
        }
        if s.intersects(git2::Status::INDEX_MODIFIED | git2::Status::WT_MODIFIED) {
            modified += 1;
        }
        if s.intersects(git2::Status::INDEX_DELETED | git2::Status::WT_DELETED) {
            deleted += 1;
        }
    }

    Some((added, modified, deleted))
}

/// Git ステータスのカウントを色付き文字列にフォーマットする。
///
/// - `+N` (緑): 追加ファイル
/// - `~N` (黄): 変更ファイル
/// - `-N` (赤): 削除ファイル
///
/// 全て 0 の場合は空文字列を返す。
/// `nerd_font` が false の場合、NerdFont アイコンの代わりに ASCII 記号を使用する。
pub(super) fn format_git_status_at(cwd: &Path, nerd_font: bool) -> String {
    let (added, modified, deleted) = match git_file_status_counts_at(cwd) {
        Some(counts) => counts,
        None => return String::new(),
    };

    if added == 0 && modified == 0 && deleted == 0 {
        return String::new();
    }

    let (modified_prefix, added_prefix, deleted_prefix) = if nerd_font {
        ("\u{ea73} ", "\u{f067} ", "\u{f068} ") // ファイル、プラス、マイナスアイコン
    } else {
        ("~", "+", "-")
    };

    let mut parts = Vec::new();
    if modified > 0 {
        parts.push(yellow(&format!("{modified_prefix}{modified}")));
    }
    if added > 0 {
        parts.push(green(&format!("{added_prefix}{added}")));
    }
    if deleted > 0 {
        parts.push(red(&format!("{deleted_prefix}{deleted}")));
    }

    parts.join(" ")
}

/// ブランチ名をプロンプト表示用の色付きラベルにフォーマットする。
pub(super) fn format_branch_label(branch: &str, nerd_font: bool) -> String {
    if nerd_font {
        cyan(&format!("\u{e725} {branch}"))
    } else {
        cyan(branch)
    }
}
