//! AI 応答テキストの Markdown 判定
//!
//! ヒューリスティックにテキストが Markdown フォーマットかどうかを判定する。
//! Classifier と同様に、パターンマッチベースの軽量判定を行う。

/// テキストが Markdown フォーマットかどうかをヒューリスティックに判定する。
///
/// 複数の Markdown パターンをスコアリングし、
/// 合計スコアが閾値（2）以上であれば `true` を返す。
/// 単一パターン1回だけでは偽陽性を抑制するため `false` となる。
pub fn is_markdown(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }

    let mut score: u32 = 0;

    let mut has_heading = false;
    let mut code_block_count: u32 = 0;
    let mut unordered_list_count: u32 = 0;
    let mut ordered_list_count: u32 = 0;
    let mut has_table_separator = false;
    let mut has_horizontal_rule = false;

    for line in text.lines() {
        let trimmed = line.trim_start();

        if !has_heading && trimmed.starts_with('#') {
            let hash_end = trimmed.find(|c: char| c != '#').unwrap_or(0);
            if (1..=6).contains(&hash_end) && trimmed.as_bytes().get(hash_end) == Some(&b' ') {
                has_heading = true;
            }
        }

        if trimmed.starts_with("```") {
            code_block_count += 1;
        }

        if (trimmed.starts_with("- ") || trimmed.starts_with("* "))
            && trimmed.len() > 2
            && trimmed.as_bytes()[2] != b' '
        {
            unordered_list_count += 1;
        }

        if let Some(dot_pos) = trimmed.find(". ") {
            let prefix = &trimmed[..dot_pos];
            if !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit()) {
                ordered_list_count += 1;
            }
        }

        if !has_table_separator && trimmed.starts_with('|') && trimmed.contains("---") {
            has_table_separator = true;
        }

        let line_trimmed = line.trim();
        if !has_horizontal_rule && line_trimmed.len() >= 3 && line_trimmed.chars().all(|c| c == '-')
        {
            has_horizontal_rule = true;
        }
    }

    if has_heading {
        score += 2;
    }
    if code_block_count >= 2 {
        score += 2;
    }
    if unordered_list_count >= 2 {
        score += 1;
    }
    if ordered_list_count >= 2 {
        score += 1;
    }
    if has_table_separator {
        score += 2;
    }
    if has_horizontal_rule {
        score += 1;
    }

    if text.contains("**") && text.matches("**").count() >= 2 {
        score += 1;
    }

    if text.contains("](") && text.contains('[') {
        score += 1;
    }

    score >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_not_markdown() {
        assert!(!is_markdown("Hello, world!"));
        assert!(!is_markdown("This is just a plain text response."));
        assert!(!is_markdown("はい、了解しました。"));
    }

    #[test]
    fn empty_text_is_not_markdown() {
        assert!(!is_markdown(""));
        assert!(!is_markdown("   "));
    }

    #[test]
    fn headings_detected() {
        let text = "# Changelog\n\n## v1.0.3\n\n- Fixed a bug";
        assert!(is_markdown(text));
    }

    #[test]
    fn code_blocks_detected() {
        let text = "Here is an example:\n\n```rust\nfn main() {}\n```\n";
        assert!(is_markdown(text));
    }

    #[test]
    fn lists_with_headings_detected() {
        let text = "# Tasks\n\n- Task 1\n- Task 2\n- Task 3\n";
        assert!(is_markdown(text));
    }

    #[test]
    fn bold_with_lists_detected() {
        let text = "- **Important**: Do this\n- **Also**: Do that\n";
        assert!(is_markdown(text));
    }

    #[test]
    fn table_detected() {
        let text = "| Name | Value |\n| --- | --- |\n| foo | 42 |\n";
        assert!(is_markdown(text));
    }

    #[test]
    fn single_dash_list_not_markdown() {
        assert!(!is_markdown("- just one item"));
    }

    #[test]
    fn changelog_markdown() {
        let text = "\
# CHANGELOG

## v1.0.3 (2025-12-01)

### New Features
- Added AI pipe support
- Improved error handling

### Bug Fixes
- Fixed crash on empty input

---

## v1.0.2 (2025-11-15)

- Initial release
";
        assert!(is_markdown(text));
    }

    #[test]
    fn link_with_bold_detected() {
        let text = "See **this** for details: [docs](https://example.com)\n";
        assert!(is_markdown(text));
    }

    #[test]
    fn ordered_list_with_heading() {
        let text = "# Steps\n\n1. First step\n2. Second step\n3. Third step\n";
        assert!(is_markdown(text));
    }

    #[test]
    fn plain_numbered_lines_not_markdown() {
        assert!(!is_markdown("3. Only one numbered line here"));
    }
}
