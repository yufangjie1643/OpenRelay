use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::{Json, Router};
use chrono::Utc;
use futures_util::StreamExt;
use openrelay::config::{
    load_config, save_config, AppConfig, ModelConfig, ProviderConfig, VirtualKeyConfig,
};
use openrelay::database::{Database, UsageLog};
use openrelay::server::{build_router, ServerState};
use serde_json::json;
use std::path::Path;
use std::time::Instant;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, Duration};
use tokio_stream::wrappers::ReceiverStream;
use tower::ServiceExt;

#[derive(Debug)]
struct CapturedRequest {
    path_and_query: String,
    authorization: Option<String>,
    google_key: Option<String>,
}

#[tokio::test]
async fn models_endpoint_returns_configured_models_for_master_key() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.providers.push(ProviderConfig {
        id: "p1".to_string(),
        name: "OpenAI".to_string(),
        provider_type: "openai".to_string(),
        base_url: Some("https://api.example.com/v1".to_string()),
        api_key: "provider-key".to_string(),
        user_agent: None,
        models: vec![ModelConfig {
            model_name: "local-gpt".to_string(),
            model_id: "gpt-4o-mini".to_string(),
        }],
    });
    save_config(dir.path(), &cfg).unwrap();

    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header("authorization", "Bearer openrelay-master")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["data"][0]["id"], "local-gpt");
}

#[tokio::test]
async fn models_endpoint_uses_startup_config_cache_until_api_changes_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.providers.push(ProviderConfig {
        id: "p1".to_string(),
        name: "OpenAI".to_string(),
        provider_type: "openai".to_string(),
        base_url: Some("https://api.example.com/v1".to_string()),
        api_key: "provider-key".to_string(),
        user_agent: None,
        models: vec![ModelConfig {
            model_name: "cached-gpt".to_string(),
            model_id: "gpt-4o-mini".to_string(),
        }],
    });
    save_config(dir.path(), &cfg).unwrap();

    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    save_config(dir.path(), &AppConfig::default()).unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .header("authorization", "Bearer openrelay-master")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["data"][0]["id"], "cached-gpt");
}

#[tokio::test]
async fn config_api_requires_admin_bearer_token() {
    let dir = tempfile::tempdir().unwrap();
    openrelay::config::ensure_files(dir.path()).unwrap();
    let app = build_router(ServerState::new(dir.path().to_path_buf()));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn config_api_accepts_safe_config_body_from_frontend_save() {
    let dir = tempfile::tempdir().unwrap();
    openrelay::config::ensure_files(dir.path()).unwrap();
    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let token = login_token(app.clone()).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/config")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(value["admin"]["password_hash"].is_null());
    value["providers"] = json!([{
        "id": "p1",
        "name": "OpenAI",
        "type": "openai",
        "base_url": "https://api.example.com/v1",
        "api_key": "provider-key",
        "models": [{"model_name": "local-gpt", "model_id": "gpt-4o-mini"}]
    }]);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/config")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let saved = load_config(dir.path()).unwrap();
    assert_eq!(saved.providers[0].models[0].model_name, "local-gpt");
    assert!(!saved.admin.password_hash.is_empty());
}

#[tokio::test]
async fn login_returns_jwt_for_default_admin_password() {
    let dir = tempfile::tempdir().unwrap();
    openrelay::config::ensure_files(dir.path()).unwrap();
    let app = build_router(ServerState::new(dir.path().to_path_buf()));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username": "admin", "password": "admin123"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["success"], true);
    assert!(value["token"].as_str().unwrap().contains('.'));
}

#[tokio::test]
async fn usage_api_reads_sqlite_usage_database() {
    let dir = tempfile::tempdir().unwrap();
    openrelay::config::ensure_files(dir.path()).unwrap();
    let db = Database::open(dir.path()).unwrap();
    db.record_usage(&UsageLog {
        timestamp: "2026-05-21T12:00:00Z".to_string(),
        request_id: "req-http".to_string(),
        model: "local-gpt".to_string(),
        key_name: "master".to_string(),
        input_tokens: 12,
        cached_tokens: 3,
        cached_write_tokens: 0,
        output_tokens: 4,
        cost: 0.01,
        status: 200,
        duration_ms: 9,
        stream: false,
        user_agent: "test".to_string(),
        error: String::new(),
        path: "/v1/chat/completions".to_string(),
    })
    .unwrap();

    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let token = login_token(app.clone()).await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/usage?page=1&pageSize=20")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["stats"]["totalRequests"], 1);
    assert_eq!(value["stats"]["totalCachedTokens"], 3);
    assert_eq!(value["entries"][0]["request_id"], "req-http");
}

#[tokio::test]
async fn gemini_native_endpoint_uses_openrelay_key_and_records_usage() {
    let dir = tempfile::tempdir().unwrap();
    let (upstream_url, mut captured, shutdown, server) = spawn_gemini_upstream().await;
    let mut cfg = AppConfig::default();
    cfg.providers.push(ProviderConfig {
        id: "gemini".to_string(),
        name: "Gemini".to_string(),
        provider_type: "gemini".to_string(),
        base_url: Some(format!("{upstream_url}/v1beta")),
        api_key: "provider-gemini-key".to_string(),
        user_agent: None,
        models: vec![ModelConfig {
            model_name: "gemini-local".to_string(),
            model_id: "gemini-1.5-pro".to_string(),
        }],
    });
    cfg.virtual_keys.push(VirtualKeyConfig {
        name: "gemini-user".to_string(),
        key: "client-key".to_string(),
        enabled: Some(true),
        allowed_models: Some(vec!["gemini-local".to_string()]),
        budget: None,
        rpm: None,
        expires_at: None,
    });
    save_config(dir.path(), &cfg).unwrap();

    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/gemini/v1beta/models/gemini-local:generateContent?key=client-key")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"contents": [{"parts": [{"text": "hello"}]}]}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let captured = captured.recv().await.unwrap();
    assert_eq!(
        captured.path_and_query,
        "/v1beta/models/gemini-1.5-pro:generateContent"
    );
    assert_eq!(captured.authorization, None);
    assert_eq!(captured.google_key.as_deref(), Some("provider-gemini-key"));

    let page = Database::open(dir.path())
        .unwrap()
        .usage_page(1, 20)
        .unwrap();
    assert_eq!(page.pagination.total, 1);
    assert_eq!(page.entries[0].model, "gemini-local");
    assert_eq!(page.entries[0].key_name, "gemini-user");
    assert_eq!(page.entries[0].input_tokens, 23);
    assert_eq!(page.entries[0].output_tokens, 11);

    let _ = shutdown.send(());
    server.await.unwrap();
}

#[tokio::test]
async fn gemini_native_endpoint_enforces_model_rpm_limit() {
    let dir = tempfile::tempdir().unwrap();
    let (upstream_url, mut captured, shutdown, server) = spawn_gemini_upstream().await;
    let mut cfg = AppConfig::default();
    cfg.limits = json!({"gemini-local": {"rpm": 1}});
    cfg.providers.push(ProviderConfig {
        id: "gemini".to_string(),
        name: "Gemini".to_string(),
        provider_type: "gemini".to_string(),
        base_url: Some(format!("{upstream_url}/v1beta")),
        api_key: "provider-gemini-key".to_string(),
        user_agent: None,
        models: vec![ModelConfig {
            model_name: "gemini-local".to_string(),
            model_id: "gemini-1.5-pro".to_string(),
        }],
    });
    save_config(dir.path(), &cfg).unwrap();
    Database::open(dir.path())
        .unwrap()
        .record_usage(&UsageLog {
            timestamp: Utc::now().to_rfc3339(),
            request_id: "recent".to_string(),
            model: "gemini-local".to_string(),
            key_name: "master".to_string(),
            input_tokens: 1,
            cached_tokens: 0,
            cached_write_tokens: 0,
            output_tokens: 1,
            cost: 0.0,
            status: 200,
            duration_ms: 1,
            stream: false,
            user_agent: "test".to_string(),
            error: String::new(),
            path: "/gemini/v1beta/models/gemini-local:generateContent".to_string(),
        })
        .unwrap();

    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/gemini/v1beta/models/gemini-local:generateContent")
                .header("authorization", "Bearer openrelay-master")
                .header("content-type", "application/json")
                .body(Body::from(json!({"contents": []}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(captured.try_recv().is_err());

    let _ = shutdown.send(());
    server.await.unwrap();
}

#[tokio::test]
async fn conversation_storage_requires_user_selected_directory() {
    let dir = tempfile::tempdir().unwrap();
    openrelay::config::ensure_files(dir.path()).unwrap();
    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let token = login_token(app.clone()).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/conversation-storage")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"enabled": true, "directory": ""}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn conversation_storage_save_returns_saved_setting() {
    let dir = tempfile::tempdir().unwrap();
    openrelay::config::ensure_files(dir.path()).unwrap();
    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let token = login_token(app.clone()).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/conversation-storage")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"enabled": true, "directory": "D:\\OpenRelay\\conversations"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["conversation_storage"]["enabled"], true);
    assert_eq!(
        value["conversation_storage"]["directory"],
        "D:\\OpenRelay\\conversations"
    );
}

#[tokio::test]
async fn proxy_request_is_saved_and_listed_when_conversation_storage_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let conversation_dir = tempfile::tempdir().unwrap();
    let (upstream_url, shutdown, server) = spawn_openai_json_upstream().await;
    let mut cfg = AppConfig::default();
    cfg.conversation_storage.enabled = true;
    cfg.conversation_storage.directory = conversation_dir.path().to_string_lossy().to_string();
    cfg.providers.push(ProviderConfig {
        id: "p1".to_string(),
        name: "OpenAI".to_string(),
        provider_type: "openai".to_string(),
        base_url: Some(format!("{upstream_url}/v1")),
        api_key: "provider-key".to_string(),
        user_agent: None,
        models: vec![ModelConfig {
            model_name: "local-gpt".to_string(),
            model_id: "gpt-4o-mini".to_string(),
        }],
    });
    save_config(dir.path(), &cfg).unwrap();
    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let token = login_token(app.clone()).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", "Bearer openrelay-master")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "local-gpt",
                        "messages": [{"role": "user", "content": "hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/conversations")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(list_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(list["enabled"], true);
    assert_eq!(list["entries"].as_array().unwrap().len(), 1);
    assert_eq!(list["entries"][0]["model"], "local-gpt");
    let filename = list["entries"][0]["filename"].as_str().unwrap();

    let detail_response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/conversations/{filename}"))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(detail_response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(detail_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let detail: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(detail["input"]["model"], "local-gpt");
    assert_eq!(detail["output"]["choices"][0]["message"]["content"], "ok");

    let _ = shutdown.send(());
    server.await.unwrap();
}

#[tokio::test]
async fn connections_api_lists_inflight_proxy_request() {
    let dir = tempfile::tempdir().unwrap();
    let (upstream_url, shutdown_upstream, upstream_server) = spawn_openai_slow_upstream().await;
    let mut cfg = AppConfig::default();
    cfg.providers.push(ProviderConfig {
        id: "p1".to_string(),
        name: "OpenAI".to_string(),
        provider_type: "openai".to_string(),
        base_url: Some(format!("{upstream_url}/v1")),
        api_key: "provider-key".to_string(),
        user_agent: None,
        models: vec![ModelConfig {
            model_name: "local-gpt".to_string(),
            model_id: "gpt-4o-mini".to_string(),
        }],
    });
    save_config(dir.path(), &cfg).unwrap();
    let (gateway_url, shutdown_gateway, gateway_server) = spawn_openrelay_server(dir.path()).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let login = client
        .post(format!("{gateway_url}/api/login"))
        .json(&json!({"username": "admin", "password": "admin123"}))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let token = login["token"].as_str().unwrap().to_string();

    let proxy_client = client.clone();
    let proxy_url = format!("{gateway_url}/v1/chat/completions");
    let proxy_task = tokio::spawn(async move {
        proxy_client
            .post(proxy_url)
            .bearer_auth("openrelay-master")
            .json(&json!({
                "model": "local-gpt",
                "messages": [{"role": "user", "content": "hello"}]
            }))
            .send()
            .await
            .unwrap()
    });
    sleep(Duration::from_millis(120)).await;

    let connections = client
        .get(format!("{gateway_url}/api/connections"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(connections["count"], 1);
    assert_eq!(connections["entries"][0]["model"], "local-gpt");
    assert_eq!(connections["entries"][0]["keyName"], "master");
    assert!(connections["entries"][0]["elapsedMs"].as_u64().unwrap() > 0);

    let response = proxy_task.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let _ = shutdown_gateway.send(());
    gateway_server.await.unwrap();
    let _ = shutdown_upstream.send(());
    upstream_server.await.unwrap();
}

#[tokio::test]
async fn openai_stream_proxy_forwards_first_chunk_before_upstream_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let (upstream_url, shutdown_upstream, upstream_server) =
        spawn_openai_streaming_upstream().await;
    let mut cfg = AppConfig::default();
    cfg.providers.push(ProviderConfig {
        id: "p1".to_string(),
        name: "OpenAI".to_string(),
        provider_type: "openai".to_string(),
        base_url: Some(format!("{upstream_url}/v1")),
        api_key: "provider-key".to_string(),
        user_agent: None,
        models: vec![ModelConfig {
            model_name: "local-gpt".to_string(),
            model_id: "gpt-4o-mini".to_string(),
        }],
    });
    save_config(dir.path(), &cfg).unwrap();
    let (gateway_url, shutdown_gateway, gateway_server) = spawn_openrelay_server(dir.path()).await;

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let started = Instant::now();
    let response = client
        .post(format!("{gateway_url}/v1/chat/completions"))
        .bearer_auth("openrelay-master")
        .json(&json!({
            "model": "local-gpt",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.bytes_stream();
    let first = stream.next().await.unwrap().unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "first stream chunk was buffered for {:?}",
        started.elapsed()
    );
    assert!(String::from_utf8_lossy(&first).contains("delta"));

    let _ = shutdown_gateway.send(());
    gateway_server.await.unwrap();
    let _ = shutdown_upstream.send(());
    upstream_server.await.unwrap();
}

async fn login_token(app: axum::Router) -> String {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username": "admin", "password": "admin123"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    value["token"].as_str().unwrap().to_string()
}

async fn spawn_openrelay_server(
    root: &Path,
) -> (String, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let app = build_router(ServerState::new(root.to_path_buf()));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });
    (format!("http://{addr}"), shutdown_tx, server)
}

async fn spawn_openai_json_upstream() -> (String, oneshot::Sender<()>, tokio::task::JoinHandle<()>)
{
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let app = Router::new().route("/*path", any(openai_json_response));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });
    (format!("http://{addr}"), shutdown_tx, server)
}

async fn openai_json_response() -> Json<serde_json::Value> {
    Json(json!({
        "choices": [{"message": {"content": "ok"}}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 2}
    }))
}

async fn spawn_openai_slow_upstream() -> (String, oneshot::Sender<()>, tokio::task::JoinHandle<()>)
{
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let app = Router::new().route("/*path", any(openai_slow_response));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });
    (format!("http://{addr}"), shutdown_tx, server)
}

async fn openai_slow_response() -> Json<serde_json::Value> {
    sleep(Duration::from_millis(700)).await;
    openai_json_response().await
}

async fn spawn_openai_streaming_upstream(
) -> (String, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let app = Router::new().route("/*path", any(openai_streaming_response));
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });
    (format!("http://{addr}"), shutdown_tx, server)
}

async fn openai_streaming_response() -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(4);
    tokio::spawn(async move {
        let _ = tx
            .send(Ok(Bytes::from_static(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
            )))
            .await;
        sleep(Duration::from_millis(700)).await;
        let _ = tx
            .send(Ok(Bytes::from_static(
                b"data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n",
            )))
            .await;
        let _ = tx.send(Ok(Bytes::from_static(b"data: [DONE]\n\n"))).await;
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap()
}

async fn spawn_gemini_upstream() -> (
    String,
    mpsc::UnboundedReceiver<CapturedRequest>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let app = Router::new()
        .route("/*path", any(capture_gemini_request))
        .with_state(tx);
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });
    (format!("http://{addr}"), rx, shutdown_tx, server)
}

async fn capture_gemini_request(
    State(tx): State<mpsc::UnboundedSender<CapturedRequest>>,
    uri: Uri,
    headers: HeaderMap,
    _body: axum::body::Bytes,
) -> impl IntoResponse {
    let _ = tx.send(CapturedRequest {
        path_and_query: uri
            .path_and_query()
            .map(|pq| pq.as_str().to_string())
            .unwrap_or_else(|| uri.path().to_string()),
        authorization: headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        google_key: headers
            .get("x-goog-api-key")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
    });
    Json(json!({
        "candidates": [{"content": {"parts": [{"text": "ok"}]}}],
        "usageMetadata": {
            "promptTokenCount": 23,
            "candidatesTokenCount": 11,
            "totalTokenCount": 34
        }
    }))
}
