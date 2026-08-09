use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::{routing::post, Router};
use jarvish::ai::{AiResponse, JarvisAI};
use jarvish::config::AiConfig;
use serde_json::Value;
use serial_test::serial;

#[derive(Clone, Default)]
struct MockState {
    requests: Arc<Mutex<Vec<Value>>>,
}

async fn messages(
    State(state): State<MockState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    assert_eq!(headers.get("x-api-key").unwrap(), "test-anthropic-key");
    assert_eq!(headers.get("anthropic-version").unwrap(), "2023-06-01");
    let request: Value = serde_json::from_slice(&body).unwrap();
    let mut requests = state.requests.lock().unwrap();
    requests.push(request);
    let fixture = if requests.len() == 1 {
        include_str!("fixtures/anthropic_sse/agent_loop_full.txt")
    } else {
        include_str!("fixtures/anthropic_sse/simple_text.txt")
    };
    (
        StatusCode::OK,
        [("content-type", "text/event-stream")],
        fixture,
    )
}

#[tokio::test]
#[serial]
async fn anthropic_mock_runs_agent_tool_loop() {
    let old_key = std::env::var("ANTHROPIC_API_KEY").ok();
    std::env::set_var("ANTHROPIC_API_KEY", "test-anthropic-key");

    let state = MockState::default();
    let requests = Arc::clone(&state.requests);
    let app = Router::new()
        .route("/v1/messages", post(messages))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let config = AiConfig {
        provider: "anthropic".into(),
        base_url: Some(format!("http://{address}")),
        max_tokens: Some(16_384),
        ..AiConfig::default()
    };
    let ai = JarvisAI::new(&config).unwrap();
    let result = ai.process_input("inspect the project", "").await.unwrap();

    server.abort();
    match old_key {
        Some(key) => std::env::set_var("ANTHROPIC_API_KEY", key),
        None => std::env::remove_var("ANTHROPIC_API_KEY"),
    }

    assert!(
        matches!(result.response, AiResponse::NaturalLanguage(text) if text == "Mock response")
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let second_messages = requests[1]["messages"].as_array().unwrap();
    assert!(second_messages.iter().any(|message| {
        message["role"] == "user"
            && message["content"]
                .as_array()
                .is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "tool_result"))
    }));
    assert_eq!(requests[0]["max_tokens"], 16_384);
    assert!(requests[0].get("temperature").is_none());
}
