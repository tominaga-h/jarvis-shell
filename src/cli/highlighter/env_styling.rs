pub(super) fn is_assignment(word: &str) -> bool {
    word.contains('=') && !word.starts_with('\'') && !word.starts_with('"')
}
