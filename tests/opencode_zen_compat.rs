use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::{routing::post, Router};
use jarvish::ai::{AiResponse, JarvisAI};
use jarvish::config::AiConfig;
use serde_json::Value;
use serial_test::serial;

#[derive(Clone, Default)]
struct MockState {
    request: Arc<Mutex<Option<CapturedRequest>>>,
}

#[derive(Clone)]
struct CapturedRequest {
    path: String,
    headers: HeaderMap,
    body: Value,
}

async fn completions(
    State(state): State<MockState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let body: Value = serde_json::from_slice(&body).unwrap();
    *state.request.lock().unwrap() = Some(CapturedRequest {
        path: uri.path().to_string(),
        headers,
        body,
    });
    (
        StatusCode::OK,
        [("content-type", "text/event-stream")],
        concat!(
            "data: {\"id\":\"chatcmpl_mock\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Zen mock response\"},\"finish_reason\":\"stop\",\"logprobs\":null}],\"created\":0,\"model\":\"mock\",\"object\":\"chat.completion.chunk\"}\n\n",
            "data: [DONE]\n\n"
        ),
    )
}

struct ApiKeyGuard(Option<String>);

impl Drop for ApiKeyGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(key) => std::env::set_var("OPENCODE_API_KEY", key),
            None => std::env::remove_var("OPENCODE_API_KEY"),
        }
    }
}

#[tokio::test]
#[serial]
async fn opencode_zen_uses_openai_compat_headers_and_streaming() {
    let _guard = ApiKeyGuard(std::env::var("OPENCODE_API_KEY").ok());
    std::env::set_var("OPENCODE_API_KEY", "test-opencode-key");

    let state = MockState::default();
    let captured = Arc::clone(&state.request);
    let app = Router::new()
        .route("/v1/chat/completions", post(completions))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let config = AiConfig {
        provider: "opencode-zen".into(),
        base_url: Some(format!("http://{address}/v1")),
        max_tokens: Some(8192),
        ..AiConfig::default()
    };
    let ai = JarvisAI::new(&config).unwrap();
    let result = ai.process_input("say hello", "").await.unwrap();

    server.abort();
    assert!(matches!(
        result.response,
        AiResponse::NaturalLanguage(text) if text == "Zen mock response"
    ));

    let request = captured.lock().unwrap().clone().unwrap();
    assert_eq!(request.path, "/v1/chat/completions");
    assert_eq!(
        request.headers.get("authorization").unwrap(),
        "Bearer test-opencode-key"
    );
    assert_eq!(
        request.headers.get("user-agent").unwrap().to_str().unwrap(),
        format!("jarvish/{}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(request.body["model"], "gpt-4o");
    assert_eq!(request.body["stream"], true);
    assert!(request.body["messages"].is_array());
}
