pub mod blob;
mod context;
pub mod history;
mod record;
pub(crate) mod sanitizer;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use rusqlite::Connection;
use std::path::PathBuf;

use blob::BlobStore;

pub use history::BlackBoxHistory;

/// コマンド履歴エントリ。AI コンテキストとして使用する。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HistoryEntry {
    pub id: i64,
    pub command: String,
    pub cwd: String,
    pub exit_code: i32,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub created_at: String,
}

/// コマンド実行履歴とその出力を永続化する Black Box。
/// SQLite でメタデータを管理し、BlobStore で stdout/stderr を保存する。
pub struct BlackBox {
    conn: Connection,
    blob_store: BlobStore,
    session_id: i64,
}

impl BlackBox {
    /// 指定されたディレクトリで BlackBox を初期化する。
    pub fn open_at(data_dir: PathBuf, session_id: i64) -> Result<Self> {
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("failed to create data directory: {}", data_dir.display()))?;

        let db_path = data_dir.join("history.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open database: {}", db_path.display()))?;

        Self::migrate(&conn)?;

        let blob_store = BlobStore::new(data_dir.join("blobs"))?;

        Ok(Self {
            conn,
            blob_store,
            session_id,
        })
    }

    /// セッション終了時に session_id を NULL に解放する。
    ///
    /// 終了済みセッションの履歴は次回起動時に上下矢印で辿れるようになる。
    /// 同時実行中の他セッションの履歴は session_id が残っているため分離が維持される。
    pub fn release_session(&self) {
        let _ = self.conn.execute(
            "UPDATE command_history SET session_id = NULL WHERE session_id = ?1",
            rusqlite::params![self.session_id],
        );
    }

    /// データディレクトリのパスを返す。
    ///
    /// `directories` クレートを使用してプラットフォームに応じたパスを決定する。
    /// - macOS: `~/Library/Application Support/jarvish/`
    /// - Linux: `~/.local/share/jarvish/`
    pub(crate) fn data_dir() -> PathBuf {
        ProjectDirs::from("", "", "jarvish")
            .map(|p| p.data_dir().to_path_buf())
            .unwrap_or_else(|| {
                eprintln!("jarvish: warning: failed to determine data directory, using fallback");
                std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(".jarvish")
            })
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
            used_alt_screen: false,
        }
    }

    #[test]
    fn open_creates_database() {
        let tmp = TempDir::new().unwrap();
        let bb = BlackBox::open_at(tmp.path().to_path_buf(), 1).unwrap();

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
        let bb = BlackBox::open_at(tmp.path().to_path_buf(), 1).unwrap();

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
        let bb = BlackBox::open_at(tmp.path().to_path_buf(), 1).unwrap();

        let stdout_content = "output line 1\noutput line 2\n";
        let stderr_content = "error: something went wrong\n";
        let result = make_result(stdout_content, stderr_content, 1);
        bb.record("failing-command", &result).unwrap();

        let (stdout_hash, stderr_hash): (Option<String>, Option<String>) = bb
            .conn
            .query_row(
                "SELECT stdout_hash, stderr_hash FROM command_history WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        let loaded_stdout = bb.blob_store.load(&stdout_hash.unwrap()).unwrap();
        assert_eq!(loaded_stdout, stdout_content);

        let loaded_stderr = bb.blob_store.load(&stderr_hash.unwrap()).unwrap();
        assert_eq!(loaded_stderr, stderr_content);
    }

    #[test]
    fn record_with_empty_output_stores_null_hashes() {
        let tmp = TempDir::new().unwrap();
        let bb = BlackBox::open_at(tmp.path().to_path_buf(), 1).unwrap();

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
    fn get_recent_context_returns_formatted_history() {
        let tmp = TempDir::new().unwrap();
        let bb = BlackBox::open_at(tmp.path().to_path_buf(), 1).unwrap();

        bb.record("echo hello", &make_result("hello\n", "", 0))
            .unwrap();
        bb.record("bad-cmd", &make_result("", "error: not found\n", 1))
            .unwrap();

        let ctx = bb.get_recent_context(5).unwrap();
        assert!(ctx.contains("echo hello"));
        assert!(ctx.contains("bad-cmd"));
        assert!(ctx.contains("error: not found"));
        assert!(ctx.contains("hello"));
    }

    #[test]
    fn get_recent_context_empty_when_no_history() {
        let tmp = TempDir::new().unwrap();
        let bb = BlackBox::open_at(tmp.path().to_path_buf(), 1).unwrap();

        let ctx = bb.get_recent_context(5).unwrap();
        assert!(ctx.is_empty());
    }

    #[test]
    fn multiple_records_increment_id() {
        let tmp = TempDir::new().unwrap();
        let bb = BlackBox::open_at(tmp.path().to_path_buf(), 1).unwrap();

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
