/// `latest` が `current` より新しいかどうかを semver 比較で判定する。
pub(super) fn is_newer_version(current: &str, latest: &str) -> bool {
    let current_parts: Vec<u32> = current.split('.').filter_map(|s| s.parse().ok()).collect();
    let latest_parts: Vec<u32> = latest.split('.').filter_map(|s| s.parse().ok()).collect();
    latest_parts > current_parts
}

/// `--version` 出力からバージョン番号を抽出する。
///
/// `"jarvish 1.8.0\n"` → `Some("1.8.0")`
pub(super) fn parse_version_from_output(output: &str) -> Option<String> {
    let trimmed = output.trim();
    // "jarvish X.Y.Z" or "X.Y.Z" のどちらも対応
    let version_str = trimmed.rsplit_once(' ').map(|(_, v)| v).unwrap_or(trimmed);
    let version = version_str.trim_start_matches('v');
    // 数字で始まるか確認（バージョン番号の妥当性チェック）
    if version.starts_with(|c: char| c.is_ascii_digit()) {
        Some(version.to_string())
    } else {
        None
    }
}
