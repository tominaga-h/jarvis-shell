//! cd 履歴クエリヘルパ
//!
//! `command_history.cwd` カラムを LRU 順に重複排除して返す。
//! 既存スキーマを読み取るだけで、新規テーブル追加はしない。
//!
//! 主に `cdhist` / `cdj` ビルトインから利用される。

use std::path::Path;

use rusqlite::Connection;

/// 内部ハードキャップ（`limit = 0` 指定時の上限）。
///
/// SQLite から極端な件数を読み出さないよう、無制限指定時もこの数まで。
const HARD_CAP: usize = 10_000;

/// `command_history.cwd` から重複排除 LRU 順のディレクトリ一覧を返す。
///
/// - `db_path`: history.db へのパス。存在しない場合は空 Vec を返す（エラーにしない）
/// - `limit`: 最大件数。`0` を指定すると `HARD_CAP` まで（実質全件）
/// - `only_existing`: `true` の場合、`Path::is_dir() == false` のパスを除外
/// - `exclude_cwd`: 指定パス（canonicalize 比較）を結果から除外
///
/// 並び順は最近訪問順（`MAX(id) DESC`）。
pub fn recent_unique_dirs(
    db_path: &Path,
    limit: usize,
    only_existing: bool,
    exclude_cwd: Option<&Path>,
) -> Result<Vec<String>, String> {
    // DB が無ければ空 Vec を返す（初回起動などで自然に発生する）
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let conn = Connection::open(db_path)
        .map_err(|e| format!("failed to open cd history database: {e}"))?;

    // WAL モード（BlackBox 書き込みとの並行アクセス安全化）
    let _ = conn.execute_batch("PRAGMA journal_mode=WAL;");

    let effective_limit = if limit == 0 {
        HARD_CAP as i64
    } else {
        (limit.min(HARD_CAP)) as i64
    };

    let sql = "SELECT cwd, MAX(id) AS last_id \
               FROM command_history \
               WHERE cwd != '' \
               GROUP BY cwd \
               ORDER BY last_id DESC \
               LIMIT ?1";

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("failed to prepare cd history query: {e}"))?;

    let rows = stmt
        .query_map(rusqlite::params![effective_limit], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|e| format!("failed to query cd history: {e}"))?;

    let exclude_canonical = exclude_cwd.and_then(|p| p.canonicalize().ok());

    let mut out = Vec::new();
    for row in rows {
        let path = row.map_err(|e| format!("failed to read cd history row: {e}"))?;
        let path_buf = std::path::PathBuf::from(&path);

        if only_existing && !path_buf.is_dir() {
            continue;
        }

        if let Some(ref ex) = exclude_canonical {
            if let Ok(canon) = path_buf.canonicalize() {
                if &canon == ex {
                    continue;
                }
            }
        }

        out.push(path);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// テスト用に command_history テーブルを作成し、cwd 履歴を順番に挿入する。
    ///
    /// `entries` は (cwd, command) の配列で、配列の後ろほど新しい（id が大きい）。
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
    fn returns_dirs_in_lru_order_with_dedup() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let dir_c = TempDir::new().unwrap();
        let a = dir_a.path().to_str().unwrap();
        let b = dir_b.path().to_str().unwrap();
        let c = dir_c.path().to_str().unwrap();

        // 古→新の順: A, B, A, C, B → 最新訪問順は B, C, A
        let (_tmp, db) = setup_db(&[
            (a, "cmd1"),
            (b, "cmd2"),
            (a, "cmd3"),
            (c, "cmd4"),
            (b, "cmd5"),
        ]);

        let result = recent_unique_dirs(&db, 10, true, None).unwrap();
        assert_eq!(result, vec![b.to_string(), c.to_string(), a.to_string()]);
    }

    #[test]
    fn respects_limit() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let dir_c = TempDir::new().unwrap();
        let a = dir_a.path().to_str().unwrap();
        let b = dir_b.path().to_str().unwrap();
        let c = dir_c.path().to_str().unwrap();

        let (_tmp, db) = setup_db(&[(a, "c1"), (b, "c2"), (c, "c3")]);

        // 最新 2 件 → C, B
        let result = recent_unique_dirs(&db, 2, true, None).unwrap();
        assert_eq!(result, vec![c.to_string(), b.to_string()]);
    }

    #[test]
    fn limit_zero_returns_all_within_hard_cap() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let a = dir_a.path().to_str().unwrap();
        let b = dir_b.path().to_str().unwrap();

        let (_tmp, db) = setup_db(&[(a, "c1"), (b, "c2")]);

        let result = recent_unique_dirs(&db, 0, true, None).unwrap();
        assert_eq!(result.len(), 2);
        // 最新訪問順: B, A
        assert_eq!(result, vec![b.to_string(), a.to_string()]);
    }

    #[test]
    fn filters_out_nonexistent_paths_when_requested() {
        let dir_a = TempDir::new().unwrap();
        let a = dir_a.path().to_str().unwrap();
        let bogus = "/this/path/should/not/exist/cdhist-test";

        let (_tmp, db) = setup_db(&[(bogus, "c1"), (a, "c2")]);

        // only_existing = true → bogus は除外
        let result = recent_unique_dirs(&db, 10, true, None).unwrap();
        assert_eq!(result, vec![a.to_string()]);

        // only_existing = false → 両方含む
        let result_all = recent_unique_dirs(&db, 10, false, None).unwrap();
        assert_eq!(result_all, vec![a.to_string(), bogus.to_string()]);
    }

    #[test]
    fn excludes_specified_cwd() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let a = dir_a.path().to_str().unwrap();
        let b = dir_b.path().to_str().unwrap();

        let (_tmp, db) = setup_db(&[(a, "c1"), (b, "c2")]);

        // b を現在の cwd として除外 → a のみ
        let result = recent_unique_dirs(&db, 10, true, Some(dir_b.path())).unwrap();
        assert_eq!(result, vec![a.to_string()]);
    }

    #[test]
    fn empty_database_returns_empty_vec() {
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

        let result = recent_unique_dirs(&db_path, 10, true, None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn nonexistent_database_returns_empty_vec() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("history.db");
        // DB ファイルを作成しない

        let result = recent_unique_dirs(&db_path, 10, true, None).unwrap();
        assert!(result.is_empty());
    }
}
