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

struct AnthropicApiKeyGuard(Option<String>);

impl Drop for AnthropicApiKeyGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(key) => std::env::set_var("ANTHROPIC_API_KEY", key),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
    }
}

#[test]
#[serial]
fn new_succeeds_with_openai_key() {
    let _guard = ApiKeyGuard(std::env::var("OPENAI_API_KEY").ok());
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");

    assert!(JarvisAI::new(&AiConfig::default()).is_ok());
}

#[test]
#[serial]
fn new_fails_without_openai_key() {
    let _guard = ApiKeyGuard(std::env::var("OPENAI_API_KEY").ok());
    std::env::remove_var("OPENAI_API_KEY");

    let result = JarvisAI::new(&AiConfig::default());
    assert!(result.is_err());
}

#[test]
#[serial]
fn update_config_does_not_rebuild_client() {
    let _guard = ApiKeyGuard(std::env::var("OPENAI_API_KEY").ok());
    std::env::set_var("OPENAI_API_KEY", "test-openai-key");

    let mut ai = JarvisAI::new(&AiConfig::default()).unwrap();
    let backend_address = &ai.backend as *const _;
    let config = AiConfig {
        model: "another-model".to_string(),
        temperature: 0.2,
        ..AiConfig::default()
    };

    ai.update_config(&config);

    assert_eq!(backend_address, &ai.backend as *const _);
    assert_eq!(ai.model, "another-model");
    assert_eq!(ai.temperature, 0.2);
}

#[test]
#[serial]
fn new_selects_anthropic_backend_and_default_max_tokens() {
    let _guard = AnthropicApiKeyGuard(std::env::var("ANTHROPIC_API_KEY").ok());
    std::env::set_var("ANTHROPIC_API_KEY", "test-anthropic-key");
    let config = AiConfig {
        provider: "anthropic".to_string(),
        base_url: Some("http://127.0.0.1:1".to_string()),
        ..AiConfig::default()
    };

    let ai = JarvisAI::new(&config).unwrap();
    assert!(matches!(
        ai.backend,
        crate::ai::provider::AiBackend::Anthropic(_)
    ));
    assert_eq!(ai.max_tokens, 16_384);
}

#[test]
#[serial]
fn new_fails_without_anthropic_key() {
    let _guard = AnthropicApiKeyGuard(std::env::var("ANTHROPIC_API_KEY").ok());
    std::env::remove_var("ANTHROPIC_API_KEY");
    let config = AiConfig {
        provider: "anthropic".to_string(),
        ..AiConfig::default()
    };

    assert!(JarvisAI::new(&config).is_err());
}
