pub mod blob;
pub mod history;

use anyhow::{Context, Result};
use chrono::Utc;
use directories::ProjectDirs;
use rusqlite::Connection;
use std::path::PathBuf;
use tracing::debug;

use crate::engine::CommandResult;
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
}

impl BlackBox {
    /// データディレクトリを決定し、DB と BlobStore を初期化する。
    pub fn open() -> Result<Self> {
        let data_dir = Self::data_dir()?;
        Self::open_at(data_dir)
    }

    /// 指定されたディレクトリで BlackBox を初期化する（テスト用にも使用）。
    pub fn open_at(data_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("failed to create data directory: {}", data_dir.display()))?;

        let db_path = data_dir.join("history.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open database: {}", db_path.display()))?;

        Self::migrate(&conn)?;

        let blob_store = BlobStore::new(data_dir.join("blobs"))?;

        Ok(Self { conn, blob_store })
    }

    /// コマンド実行結果を記録する。
    /// stdout/stderr が空でなければ Blob として保存し、メタデータを DB に UPDATE する。
    /// Alternate Screen を使用した TUI コマンドの場合、stdout blob はスキップする。
    ///
    /// reedline の History::save() が先に INSERT しているため、
    /// 最新の該当行を UPDATE する。該当行が見つからない場合は INSERT にフォールバックする。
    pub fn record(&self, command: &str, result: &CommandResult) -> Result<()> {
        debug!(
            command = %command,
            exit_code = result.exit_code,
            stdout_len = result.stdout.len(),
            stderr_len = result.stderr.len(),
            used_alt_screen = result.used_alt_screen,
            "Recording command result to BlackBox"
        );

        // Alternate Screen 使用時は stdout を保存しない
        // （TUI の画面制御シーケンスは AI コンテキストとして無価値）
        let stdout_hash = if result.used_alt_screen {
            Ok(None)
        } else {
            self.blob_store.store(&result.stdout)
        }?;
        let stderr_hash = self.blob_store.store(&result.stderr)?;

        // reedline の save() が先に INSERT した最新行を UPDATE する
        let rows_updated = self
            .conn
            .execute(
                "UPDATE command_history \
                 SET exit_code = ?1, stdout_hash = ?2, stderr_hash = ?3 \
                 WHERE id = (SELECT MAX(id) FROM command_history WHERE command = ?4)",
                rusqlite::params![result.exit_code, stdout_hash, stderr_hash, command,],
            )
            .context("failed to update command history")?;

        // save() が呼ばれていない場合（初回起動時やテスト時）は INSERT にフォールバック
        if rows_updated == 0 {
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
        }

        Ok(())
    }

    /// 直近 N 件のコマンド履歴を取得し、AI に渡すコンテキスト文字列を生成する。
    /// stdout/stderr は末尾 50 行に切り詰める。
    pub fn get_recent_context(&self, limit: usize) -> Result<String> {
        let entries = self.get_recent_entries(limit)?;
        debug!(
            requested = limit,
            retrieved = entries.len(),
            "get_recent_context()"
        );
        if entries.is_empty() {
            return Ok(String::new());
        }

        let mut context = String::from("=== Recent Command History ===\n");
        for entry in &entries {
            context.push_str(&format!(
                "\n[#{}] {} (exit: {}, cwd: {})\n",
                entry.id, entry.command, entry.exit_code, entry.cwd
            ));
            if let Some(ref stdout) = entry.stdout {
                let truncated = Self::truncate_lines(stdout, 50);
                if !truncated.is_empty() {
                    context.push_str(&format!("stdout:\n{truncated}\n"));
                }
            }
            if let Some(ref stderr) = entry.stderr {
                let truncated = Self::truncate_lines(stderr, 50);
                if !truncated.is_empty() {
                    context.push_str(&format!("stderr:\n{truncated}\n"));
                }
            }
        }
        Ok(context)
    }

    /// 直近 N 件のコマンド履歴エントリを取得する（新しい順）。
    fn get_recent_entries(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, command, cwd, exit_code, stdout_hash, stderr_hash, created_at
             FROM command_history
             ORDER BY id DESC
             LIMIT ?1",
        )?;

        let rows = stmt.query_map(rusqlite::params![limit as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i32>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;

        let mut entries = Vec::new();
        for row in rows {
            let (id, command, cwd, exit_code, stdout_hash, stderr_hash, created_at) = row?;

            let stdout = stdout_hash
                .as_deref()
                .map(|h| self.blob_store.load(h))
                .transpose()
                .unwrap_or(None);
            let stderr = stderr_hash
                .as_deref()
                .map(|h| self.blob_store.load(h))
                .transpose()
                .unwrap_or(None);

            entries.push(HistoryEntry {
                id,
                command,
                cwd,
                exit_code,
                stdout,
                stderr,
                created_at,
            });
        }

        Ok(entries)
    }

    /// テキストを末尾 N 行に切り詰める。
    fn truncate_lines(text: &str, max_lines: usize) -> String {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() <= max_lines {
            text.to_string()
        } else {
            let skip = lines.len() - max_lines;
            format!(
                "... ({} lines omitted) ...\n{}",
                skip,
                lines[skip..].join("\n")
            )
        }
    }

    /// データディレクトリのパスを返す。
    /// `directories` クレートを使用してプラットフォームに応じたパスを決定する。
    /// BlackBoxHistory からも使用するため `pub(crate)` にしている。
    pub(crate) fn data_dir() -> Result<PathBuf> {
        let proj_dirs =
            ProjectDirs::from("", "", "jarvish").context("failed to determine data directory")?;
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
            used_alt_screen: false,
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
    fn get_recent_context_returns_formatted_history() {
        let tmp = TempDir::new().unwrap();
        let bb = BlackBox::open_at(tmp.path().to_path_buf()).unwrap();

        bb.record("echo hello", &make_result("hello\n", "", 0))
            .unwrap();
        bb.record("bad-cmd", &make_result("", "error: not found\n", 1))
            .unwrap();

        let ctx = bb.get_recent_context(5).unwrap();
        // 直近の履歴が含まれていることを確認
        assert!(ctx.contains("echo hello"));
        assert!(ctx.contains("bad-cmd"));
        assert!(ctx.contains("error: not found"));
        assert!(ctx.contains("hello"));
    }

    #[test]
    fn get_recent_context_empty_when_no_history() {
        let tmp = TempDir::new().unwrap();
        let bb = BlackBox::open_at(tmp.path().to_path_buf()).unwrap();

        let ctx = bb.get_recent_context(5).unwrap();
        assert!(ctx.is_empty());
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
