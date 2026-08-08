use super::*;
use crate::config::AiConfig;
use serial_test::serial;

/// `OPENAI_API_KEY` を退避し、`Drop` で必ず復元する RAII ガード。
///
/// 手書きの復元コードだと assertion が落ちた瞬間にアンワインドで
/// 復元が飛ばされ、`OPENAI_API_KEY` が未設定のままテストバイナリの
/// 残り全体に漏れる（`engine/builtins` の `EnvGuard` と同じ方針で
/// パニック安全にする）。
struct ApiKeyGuard(Option<String>);

impl Drop for ApiKeyGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(key) => std::env::set_var("OPENAI_API_KEY", key),
            None => std::env::remove_var("OPENAI_API_KEY"),
        }
    }
}

#[test]
#[serial]
fn new_fails_without_api_key() {
    let _guard = ApiKeyGuard(std::env::var("OPENAI_API_KEY").ok());
    std::env::remove_var("OPENAI_API_KEY");

    let result = JarvisAI::new(&AiConfig::default());
    assert!(result.is_err());
}
