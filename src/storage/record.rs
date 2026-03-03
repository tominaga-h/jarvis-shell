//! コマンド実行結果の記録 + DB マイグレーション

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::Connection;
use tracing::debug;

use crate::engine::CommandResult;

use super::sanitizer;

impl super::BlackBox {
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

        let masked_stdout = if sanitizer::contains_secrets(&result.stdout) {
            sanitizer::mask_secrets(&result.stdout)
        } else {
            result.stdout.clone()
        };
        let masked_stderr = if sanitizer::contains_secrets(&result.stderr) {
            sanitizer::mask_secrets(&result.stderr)
        } else {
            result.stderr.clone()
        };

        let stdout_hash = if result.used_alt_screen {
            Ok(None)
        } else {
            self.blob_store.store(&masked_stdout)
        }?;
        let stderr_hash = self.blob_store.store(&masked_stderr)?;

        let rows_updated = self
            .conn
            .execute(
                "UPDATE command_history \
                 SET exit_code = ?1, stdout_hash = ?2, stderr_hash = ?3 \
                 WHERE id = (SELECT MAX(id) FROM command_history WHERE command = ?4)",
                rusqlite::params![result.exit_code, stdout_hash, stderr_hash, command,],
            )
            .context("failed to update command history")?;

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
                created_at  TEXT    NOT NULL
            );",
        )
        .context("failed to create command_history table")?;

        Ok(())
    }
}
