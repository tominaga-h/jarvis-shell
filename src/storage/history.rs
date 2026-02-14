//! BlackBoxHistory — reedline の History トレイトを command_history テーブル上に実装
//!
//! BlackBox と同じ SQLite データベース (history.db) を使用し、
//! コマンド履歴を一元管理する。独自の SQLite コネクションを保持し、
//! BlackBox とは別接続でアクセスする。

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use reedline::{
    CommandLineSearch, History, HistoryItem, HistoryItemId, HistorySessionId, ReedlineError,
    SearchDirection, SearchQuery,
};
use rusqlite::{types::Value, Connection};

/// reedline の History トレイトを BlackBox の command_history テーブル上に実装する。
///
/// 独自の SQLite コネクションを保持し、BlackBox とは別接続でアクセスする。
/// WAL モードを有効にし、BlackBox との並行アクセスを安全に行う。
pub struct BlackBoxHistory {
    conn: Connection,
}

impl BlackBoxHistory {
    /// history.db へのパスを受け取り、BlackBoxHistory を初期化する。
    ///
    /// - 親ディレクトリが存在しない場合は作成する
    /// - BlackBox と同じスキーマで command_history テーブルを初期化する（冪等）
    /// - WAL モードを有効化する
    pub fn open(db_path: PathBuf) -> std::result::Result<Self, String> {
        // 親ディレクトリが存在しない場合は作成
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create directory: {e}"))?;
        }

        let conn = Connection::open(&db_path)
            .map_err(|e| format!("failed to open history database: {e}"))?;

        // BlackBox と同じスキーマで初期化（冪等）
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
        .map_err(|e| format!("failed to create command_history table: {e}"))?;

        // WAL モードを有効化（BlackBox との並行アクセスを安全にする）
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| format!("failed to enable WAL mode: {e}"))?;

        Ok(Self { conn })
    }

    /// rusqlite エラーを reedline の ReedlineError に変換する。
    fn to_reedline_err(e: rusqlite::Error) -> ReedlineError {
        std::io::Error::other(e.to_string()).into()
    }

    /// DB の行を HistoryItem に変換する。
    ///
    /// SELECT id, command, cwd, exit_code, created_at の順序を前提とする。
    fn row_to_item(row: &rusqlite::Row) -> rusqlite::Result<HistoryItem> {
        let id: i64 = row.get(0)?;
        let command: String = row.get(1)?;
        let cwd: String = row.get(2)?;
        let exit_code: i32 = row.get(3)?;
        let created_at: String = row.get(4)?;

        let timestamp = DateTime::parse_from_rfc3339(&created_at)
            .ok()
            .map(|dt| dt.with_timezone(&Utc));

        Ok(HistoryItem {
            id: Some(HistoryItemId::new(id)),
            start_timestamp: timestamp,
            command_line: command,
            session_id: None,
            hostname: None,
            cwd: Some(cwd),
            duration: None,
            exit_status: Some(exit_code as i64),
            more_info: None,
        })
    }

    /// SearchQuery から SQL の WHERE 句、ORDER BY、パラメータを構築する。
    ///
    /// `select` 引数で SELECT 句を切り替える（"COUNT(*)" or カラム列挙）。
    fn build_sql(&self, query: &SearchQuery, select: &str) -> (String, Vec<Value>) {
        let mut conditions = Vec::new();
        let mut params: Vec<Value> = Vec::new();

        // command_line フィルター
        if let Some(ref cmd_search) = query.filter.command_line {
            match cmd_search {
                CommandLineSearch::Prefix(p) => {
                    conditions.push("command LIKE ?".to_string());
                    params.push(Value::Text(format!("{p}%")));
                }
                CommandLineSearch::Substring(s) => {
                    conditions.push("command LIKE ?".to_string());
                    params.push(Value::Text(format!("%{s}%")));
                }
                CommandLineSearch::Exact(e) => {
                    conditions.push("command = ?".to_string());
                    params.push(Value::Text(e.clone()));
                }
            }
        }

        // cwd_exact フィルター
        if let Some(ref cwd) = query.filter.cwd_exact {
            conditions.push("cwd = ?".to_string());
            params.push(Value::Text(cwd.clone()));
        }

        // cwd_prefix フィルター
        if let Some(ref cwd_prefix) = query.filter.cwd_prefix {
            conditions.push("cwd LIKE ?".to_string());
            params.push(Value::Text(format!("{cwd_prefix}%")));
        }

        // exit_successful フィルター
        if let Some(success) = query.filter.exit_successful {
            if success {
                conditions.push("exit_code = 0".to_string());
            } else {
                conditions.push("exit_code != 0".to_string());
            }
        }

        // start_id / end_id（排他的カーソルベースのページネーション）
        // reedline は start_id を「この id より前/後（方向依存）」の意味で使う。
        // Backward: start_id → id < start_id, end_id → id > end_id
        // Forward:  start_id → id > start_id, end_id → id < end_id
        if let Some(start_id) = query.start_id {
            match query.direction {
                SearchDirection::Backward => {
                    conditions.push("id < ?".to_string());
                }
                SearchDirection::Forward => {
                    conditions.push("id > ?".to_string());
                }
            }
            params.push(Value::Integer(start_id.0));
        }

        if let Some(end_id) = query.end_id {
            match query.direction {
                SearchDirection::Backward => {
                    conditions.push("id > ?".to_string());
                }
                SearchDirection::Forward => {
                    conditions.push("id < ?".to_string());
                }
            }
            params.push(Value::Integer(end_id.0));
        }

        // start_time / end_time
        if let Some(ref start_time) = query.start_time {
            conditions.push("created_at >= ?".to_string());
            params.push(Value::Text(start_time.to_rfc3339()));
        }

        if let Some(ref end_time) = query.end_time {
            conditions.push("created_at <= ?".to_string());
            params.push(Value::Text(end_time.to_rfc3339()));
        }

        // WHERE 句の組み立て
        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };

        // ORDER BY
        let order = match query.direction {
            SearchDirection::Forward => "ASC",
            SearchDirection::Backward => "DESC",
        };

        // LIMIT
        let limit_clause = query
            .limit
            .map(|l| format!(" LIMIT {l}"))
            .unwrap_or_default();

        let sql = format!(
            "SELECT {select} FROM command_history{where_clause} ORDER BY id {order}{limit_clause}"
        );

        (sql, params)
    }
}

impl History for BlackBoxHistory {
    fn save(&mut self, h: HistoryItem) -> Result<HistoryItem, ReedlineError> {
        // 空のコマンドは保存しない
        if h.command_line.trim().is_empty() {
            return Ok(h);
        }

        if let Some(id) = h.id {
            // 既存エントリの更新
            let cwd = h.cwd.as_deref().unwrap_or("");
            let exit_code = h.exit_status.unwrap_or(0) as i32;
            let created_at = h
                .start_timestamp
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| Utc::now().to_rfc3339());

            self.conn
                .execute(
                    "UPDATE command_history \
                     SET command = ?1, cwd = ?2, exit_code = ?3, created_at = ?4 \
                     WHERE id = ?5",
                    rusqlite::params![h.command_line, cwd, exit_code, created_at, id.0],
                )
                .map_err(Self::to_reedline_err)?;

            Ok(h)
        } else {
            // 新規エントリの挿入
            let cwd = h.cwd.clone().unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default()
            });
            let exit_code = h.exit_status.unwrap_or(0) as i32;
            let created_at = h
                .start_timestamp
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| Utc::now().to_rfc3339());

            self.conn
                .execute(
                    "INSERT INTO command_history (command, cwd, exit_code, created_at) \
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![h.command_line, cwd, exit_code, created_at],
                )
                .map_err(Self::to_reedline_err)?;

            let new_id = self.conn.last_insert_rowid();

            Ok(HistoryItem {
                id: Some(HistoryItemId::new(new_id)),
                start_timestamp: h.start_timestamp,
                command_line: h.command_line,
                session_id: None,
                hostname: None,
                cwd: Some(cwd),
                duration: h.duration,
                exit_status: h.exit_status,
                more_info: None,
            })
        }
    }

    fn load(&self, id: HistoryItemId) -> Result<HistoryItem, ReedlineError> {
        self.conn
            .query_row(
                "SELECT id, command, cwd, exit_code, created_at \
                 FROM command_history WHERE id = ?1",
                rusqlite::params![id.0],
                Self::row_to_item,
            )
            .map_err(Self::to_reedline_err)
    }

    fn count(&self, query: SearchQuery) -> Result<i64, ReedlineError> {
        let (sql, params) = self.build_sql(&query, "COUNT(*)");

        self.conn
            .query_row(&sql, rusqlite::params_from_iter(params.iter()), |row| {
                row.get(0)
            })
            .map_err(Self::to_reedline_err)
    }

    fn search(&self, query: SearchQuery) -> Result<Vec<HistoryItem>, ReedlineError> {
        // #region agent log
        {
            use std::io::Write;
            let dir_str = match query.direction {
                SearchDirection::Forward => "Forward",
                SearchDirection::Backward => "Backward",
            };
            let cmd_filter = match &query.filter.command_line {
                Some(CommandLineSearch::Prefix(p)) => format!("Prefix({})", p),
                Some(CommandLineSearch::Substring(s)) => format!("Substring({})", s),
                Some(CommandLineSearch::Exact(e)) => format!("Exact({})", e),
                None => "None".to_string(),
            };
            let log = format!(
                "{{\"sessionId\":\"866495\",\"hypothesisId\":\"A\",\"location\":\"history.rs:search\",\"message\":\"search called\",\"data\":{{\"direction\":\"{}\",\"start_id\":{},\"end_id\":{},\"limit\":{},\"cmd_filter\":\"{}\",\"cwd_exact\":{},\"exit_successful\":{}}},\"timestamp\":{}}}\n",
                dir_str,
                query.start_id.map(|id| id.0.to_string()).unwrap_or("null".to_string()),
                query.end_id.map(|id| id.0.to_string()).unwrap_or("null".to_string()),
                query.limit.map(|l| l.to_string()).unwrap_or("null".to_string()),
                cmd_filter,
                query.filter.cwd_exact.as_deref().map(|s| format!("\"{}\"", s)).unwrap_or("null".to_string()),
                query.filter.exit_successful.map(|b| b.to_string()).unwrap_or("null".to_string()),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()
            );
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/Users/mad-tmng/lab/rust/jarvis-shell/.cursor/debug-866495.log")
            {
                let _ = f.write_all(log.as_bytes());
            }
        }
        // #endregion

        let (sql, params) = self.build_sql(&query, "id, command, cwd, exit_code, created_at");

        // #region agent log
        {
            use std::io::Write;
            let log = format!(
                "{{\"sessionId\":\"866495\",\"hypothesisId\":\"A\",\"location\":\"history.rs:search_sql\",\"message\":\"generated SQL\",\"data\":{{\"sql\":\"{}\",\"param_count\":{}}},\"timestamp\":{}}}\n",
                sql.replace('"', "\\\""),
                params.len(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()
            );
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/Users/mad-tmng/lab/rust/jarvis-shell/.cursor/debug-866495.log")
            {
                let _ = f.write_all(log.as_bytes());
            }
        }
        // #endregion

        let mut stmt = self.conn.prepare(&sql).map_err(Self::to_reedline_err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), Self::row_to_item)
            .map_err(Self::to_reedline_err)?;

        let mut items = Vec::new();
        for row in rows {
            items.push(row.map_err(Self::to_reedline_err)?);
        }

        // #region agent log
        {
            use std::io::Write;
            let ids: Vec<String> = items
                .iter()
                .map(|i| i.id.map(|id| id.0.to_string()).unwrap_or("?".to_string()))
                .collect();
            let cmds: Vec<String> = items
                .iter()
                .take(5)
                .map(|i| i.command_line.clone())
                .collect();
            let log = format!(
                "{{\"sessionId\":\"866495\",\"hypothesisId\":\"A\",\"location\":\"history.rs:search_result\",\"message\":\"search results\",\"data\":{{\"count\":{},\"ids\":[{}],\"cmds\":[{}]}},\"timestamp\":{}}}\n",
                items.len(),
                ids.iter().take(10).map(|s| s.as_str()).collect::<Vec<_>>().join(","),
                cmds.iter().map(|s| format!("\"{}\"", s.replace('"', "\\\""))).collect::<Vec<_>>().join(","),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()
            );
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/Users/mad-tmng/lab/rust/jarvis-shell/.cursor/debug-866495.log")
            {
                let _ = f.write_all(log.as_bytes());
            }
        }
        // #endregion

        Ok(items)
    }

    fn update(
        &mut self,
        id: HistoryItemId,
        updater: &dyn Fn(HistoryItem) -> HistoryItem,
    ) -> Result<(), ReedlineError> {
        let item = self.load(id)?;
        let updated = updater(item);
        self.save(HistoryItem {
            id: Some(id),
            ..updated
        })?;
        Ok(())
    }

    fn clear(&mut self) -> Result<(), ReedlineError> {
        self.conn
            .execute("DELETE FROM command_history", [])
            .map_err(Self::to_reedline_err)?;
        Ok(())
    }

    fn delete(&mut self, h: HistoryItemId) -> Result<(), ReedlineError> {
        self.conn
            .execute(
                "DELETE FROM command_history WHERE id = ?1",
                rusqlite::params![h.0],
            )
            .map_err(Self::to_reedline_err)?;
        Ok(())
    }

    fn sync(&mut self) -> std::io::Result<()> {
        // SQLite は自動的にディスクに書き出すため no-op
        Ok(())
    }

    fn session(&self) -> Option<HistorySessionId> {
        // セッション管理は使用しない
        None
    }
}
