//! cdhist: cd 履歴ディレクトリを出力するビルトイン
//!
//! `command_history.cwd` を LRU 順に重複排除して 1 行 1 件で stdout に出す。
//! 現在の cwd と存在しないディレクトリは除外する。
//!
//! `cdj` ビルトインから内部的にも利用される（候補ソース）。

use std::env;
use std::path::{Path, PathBuf};

use clap::Parser;

use crate::engine::CommandResult;
use crate::storage::{cd_history, BlackBox};

/// cdhist: cd 履歴ディレクトリを 1 行 1 件で出力する。
#[derive(Parser)]
#[command(name = "cdhist", about = "Print recently visited directories (LRU)")]
struct CdhistArgs {
    /// Maximum entries to show (0 = unlimited up to internal hard cap)
    #[arg(short = 'l', long, default_value = "200")]
    limit: usize,
}

/// 既定の data_dir 配下の history.db を使って実行する。
pub(crate) fn execute(args: &[&str]) -> CommandResult {
    let db_path = BlackBox::data_dir().join("history.db");
    execute_with_db_path(args, &db_path)
}

/// 任意の DB パスで実行する（テスト用）。
pub(crate) fn execute_with_db_path(args: &[&str], db_path: &Path) -> CommandResult {
    let parsed = match super::parse_args::<CdhistArgs>("cdhist", args) {
        Ok(a) => a,
        Err(result) => return result,
    };

    let current_cwd = env::current_dir().ok();
    let exclude: Option<&Path> = current_cwd.as_deref();

    let dirs = match cd_history::recent_unique_dirs(db_path, parsed.limit, true, exclude) {
        Ok(d) => d,
        Err(e) => {
            let msg = format!("jarvish: cdhist: {e}\n");
            eprint!("{msg}");
            return CommandResult::error(msg, 1);
        }
    };

    let mut output = String::new();
    for dir in &dirs {
        output.push_str(dir);
        output.push('\n');
    }
    print!("{output}");

    CommandResult::success(output)
}

/// `cdj` から候補一覧を取得する内部ヘルパ。
///
/// 直接 `recent_unique_dirs` を呼ぶ薄いラッパ（既定の DB パスを解決する）。
pub(crate) fn collect_candidates(limit: usize) -> Result<Vec<String>, String> {
    let db_path = BlackBox::data_dir().join("history.db");
    let current_cwd = env::current_dir().ok();
    let exclude: Option<&Path> = current_cwd.as_deref();
    cd_history::recent_unique_dirs(&db_path, limit, true, exclude)
}

/// `cdj` から候補一覧を取得する内部ヘルパ（DB パス注入版、テスト用）。
#[cfg(test)]
pub(crate) fn collect_candidates_with_db(
    db_path: &Path,
    limit: usize,
    cwd: Option<&Path>,
) -> Result<Vec<String>, String> {
    cd_history::recent_unique_dirs(db_path, limit, true, cwd)
}

/// 旧 cwd を取得して PathBuf に変換するヘルパ（cdj 共通利用想定だが未使用警告抑制のため pub(super)）。
#[allow(dead_code)]
pub(super) fn current_cwd() -> Option<PathBuf> {
    env::current_dir().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};
    use tempfile::TempDir;

    fn setup_db(entries: &[(&str, &str)]) -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("history.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS command_history (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                command     TEXT    NOT NULL,
                cwd         TEXT    NOT NULL,
                exit_code   INTEGER NOT NULL,
                stdout_hash TEXT,
                stderr_hash TEXT,
                created_at  TEXT    NOT NULL,
                session_id  INTEGER
            );",
        )
        .unwrap();
        for (cwd, cmd) in entries {
            conn.execute(
                "INSERT INTO command_history (command, cwd, exit_code, created_at) \
                 VALUES (?1, ?2, 0, datetime('now'))",
                params![cmd, cwd],
            )
            .unwrap();
        }
        (tmp, db_path)
    }

    #[test]
    fn cdhist_help_returns_success() {
        let result = execute(&["--help"]);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("cdhist"));
    }

    #[test]
    fn cdhist_clap_parses_limit() {
        let args = CdhistArgs::try_parse_from(["cdhist", "--limit", "42"]).unwrap();
        assert_eq!(args.limit, 42);

        let args = CdhistArgs::try_parse_from(["cdhist", "-l", "10"]).unwrap();
        assert_eq!(args.limit, 10);
    }

    #[test]
    fn cdhist_default_limit_is_200() {
        let args = CdhistArgs::try_parse_from(["cdhist"]).unwrap();
        assert_eq!(args.limit, 200);
    }

    #[test]
    fn cdhist_outputs_directories_in_lru_order() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let a = dir_a.path().to_str().unwrap();
        let b = dir_b.path().to_str().unwrap();

        // 古→新: A, B, A → LRU は A, B
        let (_tmp, db) = setup_db(&[(a, "c1"), (b, "c2"), (a, "c3")]);

        // 現在の cwd が結果に含まれないように、A/B 以外の場所からテスト DB を読む。
        // execute_with_db_path は env::current_dir() を除外対象にするが、
        // A/B はいずれも env::current_dir() ではないのでそのまま返るはず。
        let result = execute_with_db_path(&[], &db);
        assert_eq!(result.exit_code, 0);

        let lines: Vec<&str> = result.stdout.lines().collect();
        assert_eq!(lines, vec![a, b]);
    }

    #[test]
    fn cdhist_invalid_argument_returns_error() {
        // `--limit abc` は数値パースに失敗 → exit 2
        let result = execute(&["--limit", "abc"]);
        assert_eq!(result.exit_code, 2);
    }
}
