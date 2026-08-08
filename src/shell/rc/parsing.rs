/// rc スクリプト中の実行対象1行（コメント・空行を除去済み）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::shell) struct RcLine {
    /// ファイル内の行番号（1始まり、コメント・空行を含む元の行番号）
    pub(in crate::shell) lineno: usize,
    /// トリム済みの実行対象テキスト
    pub(in crate::shell) text: String,
}

/// rc スクリプトの内容を実行対象行のリストへパースする。
pub(in crate::shell) fn parse_rc_lines(content: &str) -> Vec<RcLine> {
    let mut lines = Vec::new();
    for (idx, raw) in content.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        lines.push(RcLine {
            lineno: idx + 1,
            text: trimmed.to_string(),
        });
    }
    lines
}
