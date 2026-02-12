pub mod blob;

use anyhow::{Context, Result};
use chrono::Utc;
use directories::ProjectDirs;
use rusqlite::Connection;
use std::path::PathBuf;

use crate::engine::CommandResult;
use blob::BlobStore;

/// コマンド実行履歴とその出力を永続化する Black Box。
/// SQLite でメタデータを管理し、BlobStore で stdout/stderr を保存する。
pub struct BlackBox {
    conn: Connection,
    blob_store: BlobStore,
}

impl BlackBox {
    /// データディレクトリを決定し、DB と BlobStore を初期化する。
    pub fn open() -> Result<Self> {
        let data_dir = Self::data_dir()?;
        Self::open_at(data_dir)
    }

    /// 指定されたディレクトリで BlackBox を初期化する（テスト用にも使用）。
    pub fn open_at(data_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&data_dir).with_context(|| {
            format!(
                "failed to create data directory: {}",
                data_dir.display()
            )
        })?;

        let db_path = data_dir.join("history.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open database: {}", db_path.display()))?;

        Self::migrate(&conn)?;

        let blob_store = BlobStore::new(data_dir.join("blobs"))?;

        Ok(Self { conn, blob_store })
    }

    /// コマンド実行結果を記録する。
    /// stdout/stderr が空でなければ Blob として保存し、メタデータを DB に INSERT する。
    pub fn record(&self, command: &str, result: &CommandResult) -> Result<()> {
        let stdout_hash = self.blob_store.store(&result.stdout)?;
        let stderr_hash = self.blob_store.store(&result.stderr)?;

        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let created_at = Utc::now().to_rfc3339();

        self.conn
            .execute(
                "INSERT INTO command_history (command, cwd, exit_code, stdout_hash, stderr_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    command,
                    cwd,
                    result.exit_code,
                    stdout_hash,
                    stderr_hash,
                    created_at,
                ],
            )
            .context("failed to insert command history")?;

        Ok(())
    }

    /// データディレクトリのパスを返す。
    /// `directories` クレートを使用してプラットフォームに応じたパスを決定する。
    fn data_dir() -> Result<PathBuf> {
        let proj_dirs = ProjectDirs::from("", "", "jarvish")
            .context("failed to determine data directory")?;
        Ok(proj_dirs.data_dir().to_path_buf())
    }

    /// DB スキーマのマイグレーションを実行する。
    fn migrate(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS command_history (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                command     TEXT    NOT NULL,
                cwd         TEXT    NOT NULL,
                exit_code   INTEGER NOT NULL,
                stdout_hash TEXT,
                stderr_hash TEXT,
                created_at  TEXT    NOT NULL
            );",
        )
        .context("failed to create command_history table")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CommandResult, LoopAction};
    use tempfile::TempDir;

    fn make_result(stdout: &str, stderr: &str, exit_code: i32) -> CommandResult {
        CommandResult {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit_code,
            action: LoopAction::Continue,
        }
    }

    #[test]
    fn open_creates_database() {
        let tmp = TempDir::new().unwrap();
        let bb = BlackBox::open_at(tmp.path().to_path_buf()).unwrap();

        // テーブルが存在することを確認
        let count: i32 = bb
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='command_history'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn record_stores_command_metadata() {
        let tmp = TempDir::new().unwrap();
        let bb = BlackBox::open_at(tmp.path().to_path_buf()).unwrap();

        let result = make_result("hello world\n", "", 0);
        bb.record("echo hello world", &result).unwrap();

        let (cmd, exit_code): (String, i32) = bb
            .conn
            .query_row(
                "SELECT command, exit_code FROM command_history WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(cmd, "echo hello world");
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn record_stores_and_retrieves_blobs() {
        let tmp = TempDir::new().unwrap();
        let bb = BlackBox::open_at(tmp.path().to_path_buf()).unwrap();

        let stdout_content = "output line 1\noutput line 2\n";
        let stderr_content = "error: something went wrong\n";
        let result = make_result(stdout_content, stderr_content, 1);
        bb.record("failing-command", &result).unwrap();

        // DB からハッシュを取得
        let (stdout_hash, stderr_hash): (Option<String>, Option<String>) = bb
            .conn
            .query_row(
                "SELECT stdout_hash, stderr_hash FROM command_history WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        // Blob からコンテンツを復元して検証
        let loaded_stdout = bb.blob_store.load(&stdout_hash.unwrap()).unwrap();
        assert_eq!(loaded_stdout, stdout_content);

        let loaded_stderr = bb.blob_store.load(&stderr_hash.unwrap()).unwrap();
        assert_eq!(loaded_stderr, stderr_content);
    }

    #[test]
    fn record_with_empty_output_stores_null_hashes() {
        let tmp = TempDir::new().unwrap();
        let bb = BlackBox::open_at(tmp.path().to_path_buf()).unwrap();

        let result = make_result("", "", 0);
        bb.record("cd /tmp", &result).unwrap();

        let (stdout_hash, stderr_hash): (Option<String>, Option<String>) = bb
            .conn
            .query_row(
                "SELECT stdout_hash, stderr_hash FROM command_history WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert!(stdout_hash.is_none());
        assert!(stderr_hash.is_none());
    }

    #[test]
    fn multiple_records_increment_id() {
        let tmp = TempDir::new().unwrap();
        let bb = BlackBox::open_at(tmp.path().to_path_buf()).unwrap();

        bb.record("cmd1", &make_result("out1", "", 0)).unwrap();
        bb.record("cmd2", &make_result("out2", "", 0)).unwrap();
        bb.record("cmd3", &make_result("out3", "", 0)).unwrap();

        let count: i32 = bb
            .conn
            .query_row("SELECT COUNT(*) FROM command_history", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }
}
