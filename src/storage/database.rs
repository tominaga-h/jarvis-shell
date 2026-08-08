use anyhow::{Context, Result};
use directories::ProjectDirs;
use rusqlite::Connection;
use std::path::PathBuf;

use super::blob::BlobStore;
use super::BlackBox;

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

    /// DB スキーマのマイグレーションを実行する。
    pub(super) fn migrate(conn: &Connection) -> Result<()> {
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
        .context("failed to create command_history table")?;

        // 既存 DB に session_id カラムがない場合に追加する
        let has_session_id = conn
            .prepare("SELECT session_id FROM command_history LIMIT 0")
            .is_ok();
        if !has_session_id {
            conn.execute_batch("ALTER TABLE command_history ADD COLUMN session_id INTEGER;")
                .context("failed to add session_id column")?;
        }

        Ok(())
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
