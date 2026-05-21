use axum::body::Body;
use axum::http::{Request, StatusCode};
use openrelay::config::{save_config, AppConfig, ModelConfig, ProviderConfig};
use openrelay::server::{build_router, ServerState};
use serde_json::json;
use tower::ServiceExt;

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
