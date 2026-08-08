use super::BlackBox;

impl BlackBox {
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
}
