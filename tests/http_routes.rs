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
async fn config_api_includes_user_agent_candidates_from_usage_database() {
    let dir = tempfile::tempdir().unwrap();
    openrelay::config::ensure_files(dir.path()).unwrap();
    let db = Database::open(dir.path()).unwrap();
    let mut usage = UsageLog {
        timestamp: "2026-05-22T12:00:00Z".to_string(),
        request_id: "req-ua".to_string(),
        model: "local-gpt".to_string(),
        key_name: "master".to_string(),
        input_tokens: 1,
        cached_tokens: 0,
        cached_write_tokens: 0,
        output_tokens: 1,
        cost: 0.0,
        status: 200,
        duration_ms: 1,
        stream: false,
        user_agent: "custom-tool/9.9".to_string(),
        error: String::new(),
        path: "/v1/chat/completions".to_string(),
    };
    db.record_usage(&usage).unwrap();
    usage.request_id = "req-ua-2".to_string();
    usage.user_agent = "cursor-agent/1.0.0".to_string();
    db.record_usage(&usage).unwrap();

    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let token = login_token(app.clone()).await;
    let response = app
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
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value["header_candidates"]["user_agents"][0],
        "cursor-agent/1.0.0"
    );
    assert!(value["header_candidates"]["user_agents"]
        .as_array()
        .unwrap()
        .iter()
        .any(|candidate| candidate == "custom-tool/9.9"));
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
async fn provider_presets_api_returns_wizard_seed_data() {
    let dir = tempfile::tempdir().unwrap();
    openrelay::config::ensure_files(dir.path()).unwrap();
    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let token = login_token(app.clone()).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/provider-presets")
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
    let presets = value["presets"].as_array().unwrap();
    assert!(presets.iter().any(|preset| preset["id"] == "openrouter"));
    assert!(presets.iter().any(|preset| preset["id"] == "bailian"));
    let siliconflow_cn = presets
        .iter()
        .find(|preset| preset["id"] == "siliconflow-cn")
        .unwrap();
    assert_eq!(siliconflow_cn["base_url"], "https://api.siliconflow.cn/v1");
    assert_eq!(
        siliconflow_cn["api_key_placeholder"],
        "os.environ/SILICONFLOW_CN_API_KEY"
    );
    assert!(siliconflow_cn["models"].as_array().unwrap().len() >= 3);
}

#[tokio::test]
async fn provider_health_api_reports_success_latency_and_model_count() {
    let dir = tempfile::tempdir().unwrap();
    let (upstream_url, shutdown, server) = spawn_models_upstream(StatusCode::OK).await;
    let mut cfg = AppConfig::default();
    cfg.providers.push(ProviderConfig {
        id: "p1".to_string(),
        name: "OpenAI".to_string(),
        provider_type: "openai".to_string(),
        base_url: Some(format!("{upstream_url}/v1")),
        api_key: "provider-key".to_string(),
        user_agent: None,
        models: Vec::new(),
    });
    save_config(dir.path(), &cfg).unwrap();
    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let token = login_token(app.clone()).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/providers/health")
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
    assert_eq!(value["providers"][0]["status"], "ok");
    assert_eq!(value["providers"][0]["code"], "available");
    assert_eq!(value["providers"][0]["httpStatus"], 200);
    assert_eq!(value["providers"][0]["modelCount"], 2);
    assert!(value["providers"][0]["latencyMs"].as_u64().unwrap() > 0);

    let _ = shutdown.send(());
    server.await.unwrap();
}

#[tokio::test]
async fn provider_health_api_classifies_auth_and_rate_limit_failures() {
    let dir = tempfile::tempdir().unwrap();
    let (auth_url, auth_shutdown, auth_server) =
        spawn_models_upstream(StatusCode::UNAUTHORIZED).await;
    let (rate_url, rate_shutdown, rate_server) =
        spawn_models_upstream(StatusCode::TOO_MANY_REQUESTS).await;
    let mut cfg = AppConfig::default();
    cfg.providers.push(ProviderConfig {
        id: "auth".to_string(),
        name: "Auth Provider".to_string(),
        provider_type: "openai".to_string(),
        base_url: Some(format!("{auth_url}/v1")),
        api_key: "provider-key".to_string(),
        user_agent: None,
        models: Vec::new(),
    });
    cfg.providers.push(ProviderConfig {
        id: "rate".to_string(),
        name: "Rate Provider".to_string(),
        provider_type: "openai".to_string(),
        base_url: Some(format!("{rate_url}/v1")),
        api_key: "provider-key".to_string(),
        user_agent: None,
        models: Vec::new(),
    });
    save_config(dir.path(), &cfg).unwrap();
    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let token = login_token(app.clone()).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/providers/health")
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
    assert_eq!(value["providers"][0]["code"], "auth_failed");
    assert_eq!(value["providers"][1]["code"], "rate_limited");

    let _ = auth_shutdown.send(());
    auth_server.await.unwrap();
    let _ = rate_shutdown.send(());
    rate_server.await.unwrap();
}

#[tokio::test]
async fn config_validation_reports_save_blockers_and_warnings() {
    let dir = tempfile::tempdir().unwrap();
    openrelay::config::ensure_files(dir.path()).unwrap();
    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let token = login_token(app.clone()).await;

    let payload = json!({
        "admin": {"username": "admin"},
        "providers": [
            {
                "id": "p1",
                "name": "Broken",
                "type": "openai",
                "base_url": "localhost:8000",
                "api_key": "",
                "models": [
                    {"model_name": "dup-model", "model_id": "upstream-a"}
                ]
            },
            {
                "id": "p2",
                "name": "Duplicate",
                "type": "openai",
                "base_url": "https://api.example.com/v1",
                "api_key": "provider-key",
                "models": [
                    {"model_name": "dup-model", "model_id": "upstream-b"}
                ]
            }
        ],
        "pricing": {
            "dup-model": {"input": 0, "cached_input": 0, "cached_write": 0, "output": 0}
        }
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/config/validate")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["ok"], false);
    let codes: Vec<&str> = value["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| issue["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"missing_api_key"));
    assert!(codes.contains(&"invalid_base_url"));
    assert!(codes.contains(&"duplicate_model_alias"));
    assert!(codes.contains(&"missing_pricing"));
}

#[tokio::test]
async fn config_validation_checks_provider_reachability_when_requested() {
    std::env::remove_var("OPENRELAY_E2E_MISSING_KEY");
    let dir = tempfile::tempdir().unwrap();
    openrelay::config::ensure_files(dir.path()).unwrap();
    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let token = login_token(app.clone()).await;

    let payload = json!({
        "admin": {"username": "admin"},
        "providers": [
            {
                "id": "p1",
                "name": "Env Provider",
                "type": "openai",
                "base_url": "http://127.0.0.1:9/v1",
                "api_key": "os.environ/OPENRELAY_E2E_MISSING_KEY",
                "models": [
                    {"model_name": "env-model", "model_id": "upstream-env"}
                ]
            },
            {
                "id": "p2",
                "name": "Closed Port",
                "type": "openai",
                "base_url": "http://127.0.0.1:9/v1",
                "api_key": "provider-key",
                "models": [
                    {"model_name": "closed-model", "model_id": "upstream-closed"}
                ]
            }
        ],
        "pricing": {
            "env-model": {"input": 0.1, "cached_input": 0.01, "cached_write": 0.02, "output": 0.2},
            "closed-model": {"input": 0.1, "cached_input": 0.01, "cached_write": 0.02, "output": 0.2}
        }
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/config/validate?reachability=true")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["ok"], true);
    let codes: Vec<&str> = value["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| issue["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"unresolved_api_key_env"));
    assert!(codes.contains(&"provider_unreachable"));
}

#[tokio::test]
async fn config_save_creates_automatic_backup_before_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.providers.push(ProviderConfig {
        id: "old".to_string(),
        name: "Old Provider".to_string(),
        provider_type: "openai".to_string(),
        base_url: Some("https://old.example.com/v1".to_string()),
        api_key: "old-provider-key".to_string(),
        user_agent: None,
        models: vec![ModelConfig {
            model_name: "old-model".to_string(),
            model_id: "old-upstream".to_string(),
        }],
    });
    save_config(dir.path(), &cfg).unwrap();

    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let token = login_token(app.clone()).await;
    let new_config = json!({
        "providers": [{
            "id": "new",
            "name": "New Provider",
            "type": "openai",
            "base_url": "https://new.example.com/v1",
            "api_key": "new-provider-key",
            "models": [{"model_name": "new-model", "model_id": "new-upstream"}]
        }],
        "pricing": {},
        "limits": {},
        "router_settings": {"timeout": 60},
        "litellm_settings": {"drop_params": true, "allowed_headers": ["*"]},
        "general_settings": {"master_key": "openrelay-master"},
        "conversation_storage": {"enabled": false, "directory": ""}
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/config")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(new_config.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/config/backups")
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
    assert_eq!(value["backups"][0]["providerCount"], 1);
    assert_eq!(load_config(dir.path()).unwrap().providers[0].id, "new");
}

#[tokio::test]
async fn security_api_audits_and_protects_provider_keys() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.providers.push(ProviderConfig {
        id: "p1".to_string(),
        name: "OpenAI".to_string(),
        provider_type: "openai".to_string(),
        base_url: Some("https://api.example.com/v1".to_string()),
        api_key: "provider-secret".to_string(),
        user_agent: None,
        models: Vec::new(),
    });
    save_config(dir.path(), &cfg).unwrap();
    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let token = login_token(app.clone()).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/security/audit")
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
    let codes: Vec<&str> = value["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"plaintext_provider_key"));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/security/protect-secrets")
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
    if cfg!(windows) {
        assert_eq!(value["supported"], true);
        assert_eq!(value["protectedCount"], 1);
        assert!(!std::fs::read_to_string(dir.path().join("config.json"))
            .unwrap()
            .contains("provider-secret"));
    } else {
        assert_eq!(value["supported"], false);
    }
}

#[tokio::test]
async fn app_status_exposes_version_paths_and_startup_state() {
    let dir = tempfile::tempdir().unwrap();
    openrelay::config::ensure_files(dir.path()).unwrap();
    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let token = login_token(app.clone()).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/app/status")
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
    assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
    assert!(value["dataRoot"].as_str().unwrap().contains("Temp"));
    assert!(value["startup"]["supported"].is_boolean());
}

#[tokio::test]
async fn usage_analytics_api_returns_trends_rankings_anomalies_and_budget_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.limits = json!({
        "global": {"budget": 3.0},
        "local-gpt": {"budget": 2.0}
    });
    cfg.virtual_keys.push(VirtualKeyConfig {
        name: "team-a".to_string(),
        key: "sk-team-a".to_string(),
        enabled: Some(true),
        allowed_models: None,
        budget: Some(2.0),
        rpm: None,
        expires_at: None,
    });
    save_config(dir.path(), &cfg).unwrap();
    let db = Database::open(dir.path()).unwrap();
    for i in 0..12 {
        db.record_usage(&UsageLog {
            timestamp: "2026-05-21T10:15:00Z".to_string(),
            request_id: format!("req-burst-{i}"),
            model: "local-gpt".to_string(),
            key_name: "team-a".to_string(),
            input_tokens: 100,
            cached_tokens: 20,
            cached_write_tokens: 0,
            output_tokens: 40,
            cost: 0.20,
            status: 200,
            duration_ms: 50,
            stream: false,
            user_agent: "test".to_string(),
            error: String::new(),
            path: "/v1/chat/completions".to_string(),
        })
        .unwrap();
    }

    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let token = login_token(app.clone()).await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/usage/analytics?period=day")
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
    assert_eq!(value["analytics"]["period"], "day");
    assert_eq!(value["analytics"]["trends"][0]["period"], "2026-05-21");
    assert_eq!(
        value["analytics"]["topModelsByCost"][0]["name"],
        "local-gpt"
    );
    assert_eq!(value["analytics"]["topKeysByRequests"][0]["name"], "team-a");
    assert_eq!(value["analytics"]["highFrequency"][0]["requests"], 12);
    assert_eq!(value["budgetWarnings"][0]["level"], "warning");
    assert!(value["budgetWarnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning["kind"] == "global"));
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
async fn conversations_api_paginates_without_returning_all_entries() {
    let dir = tempfile::tempdir().unwrap();
    let conversation_dir = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.conversation_storage.enabled = true;
    cfg.conversation_storage.directory = conversation_dir.path().to_string_lossy().to_string();
    save_config(dir.path(), &cfg).unwrap();

    for index in 0..25 {
        let request_id = format!("req-page-{index:02}");
        let timestamp = format!("2026-05-22T22:{index:02}:00Z");
        let filename = format!("20260522T2200{index:02}000Z-{request_id}.json");
        std::fs::write(
            conversation_dir.path().join(filename),
            json!({
                "timestamp": timestamp,
                "request_id": request_id,
                "model": "deepseek-v4-pro",
                "key_name": "read",
                "input": {"messages": []},
                "output": {"choices": []}
            })
            .to_string(),
        )
        .unwrap();
    }

    let app = build_router(ServerState::new(dir.path().to_path_buf()));
    let token = login_token(app.clone()).await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/conversations?page=2&pageSize=10")
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

    assert_eq!(value["enabled"], true);
    assert_eq!(value["entries"].as_array().unwrap().len(), 10);
    assert_eq!(value["pagination"]["page"], 2);
    assert_eq!(value["pagination"]["pageSize"], 10);
    assert_eq!(value["pagination"]["total"], 25);
    assert_eq!(value["pagination"]["totalPages"], 3);
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
        started.elapsed() < Duration::from_millis(900),
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

async fn spawn_models_upstream(
    status: StatusCode,
) -> (String, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let app = Router::new().route(
        "/*path",
        any(move || async move {
            (
                status,
                Json(json!({
                    "data": [
                        {"id": "gpt-a", "object": "model", "owned_by": "test"},
                        {"id": "gpt-b", "object": "model", "owned_by": "test"}
                    ],
                    "error": {"message": "test failure"}
                })),
            )
                .into_response()
        }),
    );
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
        sleep(Duration::from_millis(1200)).await;
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
