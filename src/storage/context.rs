//! コンテキスト取得 — 直近の履歴を AI 用に整形

use anyhow::Result;
use tracing::debug;

use super::sanitizer;
use super::HistoryEntry;

impl super::BlackBox {
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
            let masked_command = if sanitizer::contains_secrets(&entry.command) {
                sanitizer::mask_secrets(&entry.command)
            } else {
                entry.command.clone()
            };
            context.push_str(&format!(
                "\n[#{}] {} (exit: {}, cwd: {})\n",
                entry.id, masked_command, entry.exit_code, entry.cwd
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
}
