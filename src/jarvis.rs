use crate::color::white;

/// Jarvis が発話するときに使う共通関数。
/// 先頭に 🤵 絵文字を付与し、白色テキストで表示する。
pub fn jarvis_talk(message: &str) {
    println!("🤵 {}", white(message));
}
