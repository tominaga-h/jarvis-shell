use rusqlite::Connection;

use super::blob::BlobStore;

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
    pub(super) conn: Connection,
    pub(super) blob_store: BlobStore,
    pub(super) session_id: i64,
}
