use crate::config::{
    load_config, save_config, AppConfig, ConfigError, ConversationStorage, VirtualKeyConfig,
};
use crate::database::{Database, DatabaseError, UsageLog};
use crate::migration::{migrate_legacy_data, MigrationError};
use crate::paths::{data_root_from_env, static_root_from_env, AppPaths};
use crate::proxy::{
    build_gemini_models_response, build_gemini_upstream_url, build_models_response,
    build_upstream_url, calc_cost, check_virtual_key, estimate_tokens, extract_usage_tokens,
    get_request_model, is_gemini_models_endpoint, is_models_endpoint, resolve_provider,
    rewrite_json_model, KeyCheck,
};
use axum::body::{to_bytes, Body, Bytes};
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post, put};
use axum::{Json, Router};
use bcrypt::verify;
use chrono::{Duration, Utc};
use futures_util::Stream;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll};
use std::time::Instant;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("Unauthorized")]
    Unauthorized,
    #[error("{0}")]
    Config(#[from] ConfigError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("HTTP client error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("Database error: {0}")]
    Database(#[from] DatabaseError),
    #[error("migration error: {0}")]
    Migration(#[from] MigrationError),
    #[error("JWT error: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),
    #[error("invalid header value: {0}")]
    HeaderValue(#[from] axum::http::header::InvalidHeaderValue),
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        (
            match self {
                ServerError::Unauthorized | ServerError::Jwt(_) => StatusCode::UNAUTHORIZED,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            },
            Json(json!({ "error": self.to_string() })),
        )
            .into_response()
    }
}

#[derive(Clone)]
pub struct ServerState {
    pub root: Arc<PathBuf>,
    pub static_root: Arc<PathBuf>,
    pub database: Database,
    config: Arc<RwLock<AppConfig>>,
    connections: Arc<RwLock<BTreeMap<String, ActiveConnection>>>,
    client: reqwest::Client,
    jwt_secret: Arc<String>,
}

#[derive(Clone)]
struct ActiveConnection {
    request_id: String,
    model: String,
    key_name: String,
    user_agent: String,
    stream: bool,
    started: Instant,
}

#[derive(Clone)]
struct ConversationTrace {
    directory: PathBuf,
    filename: String,
    timestamp: String,
    request_id: String,
    model: String,
    key_name: String,
    user_agent: String,
    input: Value,
}

impl ServerState {
    pub fn new(root: PathBuf) -> Self {
        Self::with_roots(root, static_root_from_env())
    }

    pub fn with_roots(root: PathBuf, static_root: PathBuf) -> Self {
        let database = Database::open(&root)
            .unwrap_or_else(|err| panic!("failed to open OpenRelay database: {err}"));
        let config = load_config(&root).unwrap_or_else(|_| AppConfig::default());
        Self {
            root: Arc::new(root),
            static_root: Arc::new(static_root),
            database,
            config: Arc::new(RwLock::new(config)),
            connections: Arc::new(RwLock::new(BTreeMap::new())),
            client: reqwest::Client::new(),
            jwt_secret: Arc::new(
                std::env::var("JWT_SECRET")
                    .unwrap_or_else(|_| "openrelay-webui-secret-change-me".to_string()),
            ),
        }
    }

    pub async fn current_config(&self) -> AppConfig {
        self.config.read().await.clone()
    }

    pub async fn replace_config(&self, config: AppConfig) {
        *self.config.write().await = config;
    }

    async fn insert_connection(&self, connection: ActiveConnection) {
        self.connections
            .write()
            .await
            .insert(connection.request_id.clone(), connection);
    }

    async fn remove_connection(&self, request_id: &str) -> bool {
        self.connections.write().await.remove(request_id).is_some()
    }

    async fn connection_entries(&self) -> Vec<Value> {
        self.connections
            .read()
            .await
            .values()
            .map(|connection| {
                json!({
                    "requestId": connection.request_id,
                    "model": connection.model,
                    "keyName": connection.key_name,
                    "userAgent": connection.user_agent,
                    "stream": connection.stream,
                    "elapsedMs": connection.started.elapsed().as_millis() as u64
                })
            })
            .collect()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    username: String,
    exp: usize,
}

#[derive(Debug, Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Debug, Deserialize)]
struct PasswordRequest {
    #[serde(rename = "oldPassword")]
    old_password: String,
    #[serde(rename = "newPassword")]
    new_password: String,
}

#[derive(Debug, Deserialize)]
struct TestProviderRequest {
    #[serde(rename = "type")]
    provider_type: String,
    #[serde(default)]
    base_url: String,
    api_key: String,
    #[serde(default)]
    user_agent: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsageQuery {
    #[serde(default)]
    page: Option<u64>,
    #[serde(default, rename = "pageSize")]
    page_size: Option<u64>,
    #[serde(default)]
    token: Option<String>,
}

pub fn build_router(state: ServerState) -> Router {
    Router::new()
        .route("/api/login", post(login))
        .route("/api/change-password", post(change_password))
        .route("/api/config", get(get_config).post(post_config))
        .route("/api/restart-proxy", post(restart_proxy))
        .route("/api/pricing", get(get_pricing).post(post_pricing))
        .route("/api/limits", get(get_limits).post(post_limits))
        .route(
            "/api/virtual-keys",
            get(get_virtual_keys).post(post_virtual_key),
        )
        .route(
            "/api/virtual-keys/:index",
            put(put_virtual_key).delete(delete_virtual_key),
        )
        .route("/api/test-provider", post(test_provider))
        .route("/api/detect-models", get(detect_models))
        .route("/api/usage", get(get_usage))
        .route("/api/usage/export", get(export_usage))
        .route("/api/usage/clear", post(clear_usage))
        .route("/api/usage/merge", post(merge_usage))
        .route("/api/conversations", get(get_conversations))
        .route(
            "/api/conversations/:filename",
            get(get_conversation).delete(delete_conversation),
        )
        .route("/api/conversation-storage", post(post_conversation_storage))
        .route(
            "/api/conversation-storage/open",
            post(open_conversation_storage),
        )
        .route("/api/connections", get(get_connections))
        .route("/api/connections/:id/abort", post(abort_connection))
        .route("/gemini/*path", any(proxy_handler))
        .route("/proxy/gemini/*path", any(proxy_handler))
        .route("/v1/*path", any(proxy_handler))
        .route("/proxy/*path", any(proxy_handler))
        .route("/chat/completions", any(proxy_handler))
        .route("/responses", any(proxy_handler))
        .route("/completions", any(proxy_handler))
        .route("/embeddings", any(proxy_handler))
        .route("/messages", any(proxy_handler))
        .route("/rerank", any(proxy_handler))
        .route("/t2a_v2", any(proxy_handler))
        .route("/image_generation", any(proxy_handler))
        .route("/video_generation", any(proxy_handler))
        .route("/music_generation", any(proxy_handler))
        .fallback(static_file)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_headers(Any)
                .allow_methods(Any),
        )
        .with_state(state)
}

pub async fn run_from_env() -> Result<(), ServerError> {
    serve_paths(AppPaths::from_env(), port_from_env()).await
}

pub fn root_from_env() -> PathBuf {
    data_root_from_env()
}

pub fn port_from_env() -> u16 {
    std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(18783)
}

pub async fn serve(root: PathBuf, port: u16) -> Result<(), ServerError> {
    migrate_legacy_data(&root, None)?;
    serve_initialized(root, static_root_from_env(), port).await
}

pub async fn serve_paths(paths: AppPaths, port: u16) -> Result<(), ServerError> {
    migrate_legacy_data(&paths.data_root, paths.legacy_root.as_deref())?;
    serve_initialized(paths.data_root, paths.static_root, port).await
}

async fn serve_initialized(
    root: PathBuf,
    static_root: PathBuf,
    port: u16,
) -> Result<(), ServerError> {
    let addr = format!("0.0.0.0:{port}");
    let listener = TcpListener::bind(&addr).await?;
    println!("OpenRelay Rust backend running at http://localhost:{port}");
    axum::serve(
        listener,
        build_router(ServerState::with_roots(root, static_root)),
    )
    .await?;
    Ok(())
}

pub async fn serve_with_shutdown<F>(
    root: PathBuf,
    port: u16,
    shutdown: F,
) -> Result<(), ServerError>
where
    F: Future<Output = ()> + Send + 'static,
{
    migrate_legacy_data(&root, None)?;
    serve_with_shutdown_and_static(root, static_root_from_env(), port, shutdown).await
}

pub async fn serve_with_shutdown_and_static<F>(
    root: PathBuf,
    static_root: PathBuf,
    port: u16,
    shutdown: F,
) -> Result<(), ServerError>
where
    F: Future<Output = ()> + Send + 'static,
{
    let addr = format!("0.0.0.0:{port}");
    let listener = TcpListener::bind(&addr).await?;
    println!("OpenRelay Rust backend running at http://localhost:{port}");
    axum::serve(
        listener,
        build_router(ServerState::with_roots(root, static_root)),
    )
    .with_graceful_shutdown(shutdown)
    .await?;
    Ok(())
}

async fn login(
    State(state): State<ServerState>,
    Json(payload): Json<LoginRequest>,
) -> Result<impl IntoResponse, ServerError> {
    let cfg = state.current_config().await;
    if payload.username != cfg.admin.username
        || !verify(payload.password, &cfg.admin.password_hash).unwrap_or(false)
    {
        return Ok((
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "Invalid credentials" })),
        ));
    }
    let claims = Claims {
        username: payload.username,
        exp: (Utc::now() + Duration::days(7)).timestamp() as usize,
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(state.jwt_secret.as_bytes()),
    )?;
    Ok((
        StatusCode::OK,
        Json(json!({ "success": true, "token": token })),
    ))
}

async fn change_password(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(payload): Json<PasswordRequest>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let mut cfg = state.current_config().await;
    if !verify(payload.old_password, &cfg.admin.password_hash).unwrap_or(false) {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Old password incorrect" })),
        )
            .into_response());
    }
    cfg.admin.password_hash =
        bcrypt::hash(payload.new_password, bcrypt::DEFAULT_COST).map_err(ConfigError::from)?;
    save_config(&state.root, &cfg)?;
    state.replace_config(cfg).await;
    Ok(Json(json!({ "success": true })).into_response())
}

async fn get_config(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let mut safe = serde_json::to_value(state.current_config().await)?;
    if let Some(admin) = safe.get_mut("admin") {
        *admin = json!({ "username": admin.get("username").cloned().unwrap_or(json!("admin")) });
    }
    if let Some(keys) = safe.get_mut("virtual_keys").and_then(Value::as_array_mut) {
        for key in keys {
            if let Some(raw) = key.get("key").and_then(Value::as_str) {
                key["key"] = json!(mask_key(raw));
            }
        }
    }
    Ok(Json(safe).into_response())
}

async fn post_config(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(mut incoming_value): Json<Value>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let current = state.current_config().await;
    if let Some(incoming) = incoming_value.as_object_mut() {
        incoming.insert("admin".to_string(), serde_json::to_value(&current.admin)?);
        incoming.insert(
            "virtual_keys".to_string(),
            serde_json::to_value(&current.virtual_keys)?,
        );
    }
    let mut incoming: AppConfig = serde_json::from_value(incoming_value)?;
    incoming.admin = current.admin;
    incoming.virtual_keys = current.virtual_keys;
    save_config(&state.root, &incoming)?;
    state.replace_config(incoming).await;
    Ok(Json(json!({ "success": true })).into_response())
}

async fn restart_proxy() -> Json<Value> {
    Json(json!({
        "success": true,
        "unified": true,
        "message": "配置已保存，OpenRelay Rust 后端已刷新内存配置。"
    }))
}

async fn get_pricing(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    Ok(Json(state.current_config().await.pricing).into_response())
}

async fn post_pricing(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(pricing): Json<Value>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let mut cfg = state.current_config().await;
    cfg.pricing = pricing;
    save_config(&state.root, &cfg)?;
    state.replace_config(cfg).await;
    Ok(Json(json!({ "success": true })).into_response())
}

async fn get_limits(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    Ok(Json(state.current_config().await.limits).into_response())
}

async fn post_limits(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(limits): Json<Value>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let mut cfg = state.current_config().await;
    cfg.limits = limits;
    save_config(&state.root, &cfg)?;
    state.replace_config(cfg).await;
    Ok(Json(json!({ "success": true })).into_response())
}

async fn get_virtual_keys(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let keys: Vec<Value> = state
        .current_config()
        .await
        .virtual_keys
        .into_iter()
        .map(|mut k| {
            k.key = mask_key(&k.key);
            serde_json::to_value(k).unwrap_or(json!({}))
        })
        .collect();
    Ok(Json(keys).into_response())
}

async fn post_virtual_key(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(mut key): Json<VirtualKeyConfig>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let mut cfg = state.current_config().await;
    key.key = format!("sk-vk-{}", Uuid::new_v4().simple());
    cfg.virtual_keys.push(key.clone());
    save_config(&state.root, &cfg)?;
    state.replace_config(cfg).await;
    Ok(Json(json!({ "success": true, "key": key })).into_response())
}

async fn put_virtual_key(
    AxumPath(index): AxumPath<usize>,
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(mut key): Json<VirtualKeyConfig>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let mut cfg = state.current_config().await;
    if index >= cfg.virtual_keys.len() {
        return Ok((StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" }))).into_response());
    }
    key.key = cfg.virtual_keys[index].key.clone();
    cfg.virtual_keys[index] = key;
    save_config(&state.root, &cfg)?;
    state.replace_config(cfg).await;
    Ok(Json(json!({ "success": true })).into_response())
}

async fn delete_virtual_key(
    AxumPath(index): AxumPath<usize>,
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let mut cfg = state.current_config().await;
    if index >= cfg.virtual_keys.len() {
        return Ok((StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" }))).into_response());
    }
    cfg.virtual_keys.remove(index);
    save_config(&state.root, &cfg)?;
    state.replace_config(cfg).await;
    Ok(Json(json!({ "success": true })).into_response())
}

async fn test_provider(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(payload): Json<TestProviderRequest>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let api_key = resolve_api_key(&payload.api_key);
    let base_url = provider_base_url(&payload.provider_type, &payload.base_url);
    let is_gemini = payload.provider_type == "gemini";
    let url = if is_gemini {
        build_gemini_upstream_url(&base_url, "/v1beta/models", "", "").map_err(|e| {
            ConfigError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                e.to_string(),
            ))
        })?
    } else {
        crate::proxy::build_upstream_url(&base_url, "/v1/models", "").map_err(|e| {
            ConfigError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                e.to_string(),
            ))
        })?
    };
    let mut request = state.client.get(url).header(
        header::USER_AGENT,
        payload
            .user_agent
            .unwrap_or_else(|| "OpenRelay-Gateway/1.0".to_string()),
    );
    request = if is_gemini {
        request.header("x-goog-api-key", api_key)
    } else {
        request.header(header::AUTHORIZATION, format!("Bearer {api_key}"))
    };
    let response = request.send().await;
    let Ok(response) = response else {
        return Ok(Json(json!({ "success": false, "error": "连接失败" })).into_response());
    };
    let status = response.status();
    let value = response.json::<Value>().await.unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        return Ok(
            Json(json!({ "success": false, "error": format!("HTTP {status}") })).into_response(),
        );
    }
    let models = if is_gemini {
        value
            .get("models")
            .and_then(Value::as_array)
            .map(|models| {
                models
                    .iter()
                    .map(|model| {
                        let name = model
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .strip_prefix("models/")
                            .unwrap_or_else(|| {
                                model
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                            });
                        json!({
                            "id": name,
                            "owned_by": "google",
                            "object": "model"
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        value
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    Ok(Json(json!({ "success": true, "models": models })).into_response())
}

async fn detect_models(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let cfg = state.current_config().await;
    let mut models = Vec::new();
    for provider in &cfg.providers {
        for model in &provider.models {
            models.push(json!({
                "provider": provider.name,
                "id": if model.model_id.is_empty() { &model.model_name } else { &model.model_id },
                "local_alias": model.model_name,
                "owned_by": provider.name
            }));
        }
    }
    Ok(Json(json!({ "models": models, "count": models.len(), "errors": [] })).into_response())
}

async fn get_usage(
    State(state): State<ServerState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<UsageQuery>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    Ok(Json(
        state
            .database
            .usage_page_async(query.page.unwrap_or(1), query.page_size.unwrap_or(20))
            .await?,
    )
    .into_response())
}

async fn export_usage(
    State(state): State<ServerState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<UsageQuery>,
) -> Result<Response, ServerError> {
    require_admin_or_query_token(&state, &headers, query.token.as_deref())?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"usage-export-rust.csv\"",
            ),
        ],
        state.database.export_usage_csv_async().await?,
    )
        .into_response())
}

async fn clear_usage(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ServerError> {
    require_admin(&state, &headers)?;
    state.database.clear_usage_async().await?;
    Ok(Json(json!({ "success": true })))
}
async fn merge_usage(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ServerError> {
    require_admin(&state, &headers)?;
    Ok(Json(
        json!({ "success": true, "changed": false, "database": true }),
    ))
}
async fn get_conversations(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let storage = state.current_config().await.conversation_storage;
    if !storage.enabled || storage.directory.trim().is_empty() {
        return Ok(Json(json!({ "enabled": false, "entries": [] })).into_response());
    }
    let directory = PathBuf::from(storage.directory.trim());
    let entries = tokio::task::spawn_blocking({
        let directory = directory.clone();
        move || list_conversation_entries(&directory)
    })
    .await
    .map_err(|err| DatabaseError::BlockingTask(err.to_string()))??;
    Ok(Json(json!({
        "enabled": true,
        "directory": directory.to_string_lossy(),
        "entries": entries
    }))
    .into_response())
}
async fn get_conversation(
    AxumPath(filename): AxumPath<String>,
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let Some(path) = conversation_file_path(&state.current_config().await, &filename) else {
        return Ok((StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" }))).into_response());
    };
    let value = tokio::task::spawn_blocking(move || read_conversation_file(&path))
        .await
        .map_err(|err| DatabaseError::BlockingTask(err.to_string()))??;
    Ok(Json(value).into_response())
}
async fn delete_conversation(
    AxumPath(filename): AxumPath<String>,
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let Some(path) = conversation_file_path(&state.current_config().await, &filename) else {
        return Ok((StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" }))).into_response());
    };
    let removed = tokio::task::spawn_blocking(move || match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(ServerError::Io(err)),
    })
    .await
    .map_err(|err| DatabaseError::BlockingTask(err.to_string()))??;
    if removed {
        Ok(Json(json!({ "success": true })).into_response())
    } else {
        Ok((StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" }))).into_response())
    }
}
async fn post_conversation_storage(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(storage): Json<ConversationStorage>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    if storage.enabled && storage.directory.trim().is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "启用对话记录前需要先配置保存目录" })),
        )
            .into_response());
    }
    let storage = ConversationStorage {
        enabled: storage.enabled,
        directory: storage.directory.trim().to_string(),
    };
    let mut cfg = state.current_config().await;
    cfg.conversation_storage = storage.clone();
    save_config(&state.root, &cfg)?;
    state.replace_config(cfg).await;
    Ok(Json(json!({ "success": true, "conversation_storage": storage })).into_response())
}
async fn open_conversation_storage() -> Json<Value> {
    Json(json!({ "success": false, "error": "Not implemented in Rust prototype" }))
}
async fn get_connections(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ServerError> {
    require_admin(&state, &headers)?;
    let entries = state.connection_entries().await;
    Ok(Json(json!({ "count": entries.len(), "entries": entries })))
}
async fn abort_connection(
    AxumPath(request_id): AxumPath<String>,
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    if state.remove_connection(&request_id).await {
        Ok(Json(json!({ "success": true })).into_response())
    } else {
        Ok((
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": "Not found" })),
        )
            .into_response())
    }
}

fn list_conversation_entries(directory: &Path) -> Result<Vec<Value>, ServerError> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(value) = read_conversation_file(&path) else {
            continue;
        };
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        entries.push(json!({
            "filename": filename,
            "timestamp": value.get("timestamp").cloned().unwrap_or(json!("")),
            "model": value.get("model").cloned().unwrap_or(json!("")),
            "key_name": value.get("key_name").cloned().unwrap_or(json!("master")),
            "request_id": value.get("request_id").cloned().unwrap_or(json!(""))
        }));
    }
    entries.sort_by(|left, right| {
        right
            .get("timestamp")
            .and_then(Value::as_str)
            .cmp(&left.get("timestamp").and_then(Value::as_str))
    });
    Ok(entries)
}

fn read_conversation_file(path: &Path) -> Result<Value, ServerError> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

fn conversation_file_path(cfg: &AppConfig, filename: &str) -> Option<PathBuf> {
    if !cfg.conversation_storage.enabled || cfg.conversation_storage.directory.trim().is_empty() {
        return None;
    }
    if !safe_conversation_filename(filename) {
        return None;
    }
    Some(PathBuf::from(cfg.conversation_storage.directory.trim()).join(filename))
}

fn safe_conversation_filename(filename: &str) -> bool {
    !filename.trim().is_empty()
        && !filename.contains('/')
        && !filename.contains('\\')
        && filename.ends_with(".json")
}

fn conversation_trace(
    cfg: &AppConfig,
    timestamp: &str,
    request_id: &str,
    model: &str,
    key_name: &str,
    user_agent: &str,
    input: &Value,
) -> Option<ConversationTrace> {
    if !cfg.conversation_storage.enabled || cfg.conversation_storage.directory.trim().is_empty() {
        return None;
    }
    Some(ConversationTrace {
        directory: PathBuf::from(cfg.conversation_storage.directory.trim()),
        filename: format!(
            "{}-{}.json",
            Utc::now()
                .format("%Y%m%dT%H%M%S%.3fZ")
                .to_string()
                .replace('.', ""),
            request_id
        ),
        timestamp: timestamp.to_string(),
        request_id: request_id.to_string(),
        model: model.to_string(),
        key_name: key_name.to_string(),
        user_agent: user_agent.to_string(),
        input: input.clone(),
    })
}

async fn save_conversation_record_async(
    trace: ConversationTrace,
    output: Option<Value>,
    output_raw: Option<String>,
) -> Result<(), ServerError> {
    tokio::task::spawn_blocking(move || write_conversation_record(trace, output, output_raw))
        .await
        .map_err(|err| DatabaseError::BlockingTask(err.to_string()))??;
    Ok(())
}

fn write_conversation_record(
    trace: ConversationTrace,
    output: Option<Value>,
    output_raw: Option<String>,
) -> Result<(), ServerError> {
    std::fs::create_dir_all(&trace.directory)?;
    let mut record = json!({
        "timestamp": trace.timestamp,
        "request_id": trace.request_id,
        "model": trace.model,
        "key_name": trace.key_name,
        "user_agent": trace.user_agent,
        "input": trace.input,
        "output": output.unwrap_or(Value::Null)
    });
    if let Some(raw) = output_raw {
        record["output_raw"] = json!(raw);
    }
    std::fs::write(
        trace.directory.join(trace.filename),
        serde_json::to_vec_pretty(&record)?,
    )?;
    Ok(())
}

struct ConnectionBodyStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    state: ServerState,
    request_id: String,
    conversation: Option<ConversationTrace>,
    output: Arc<StdMutex<Vec<u8>>>,
    finished: bool,
}

impl ConnectionBodyStream {
    fn new<S>(
        stream: S,
        state: ServerState,
        request_id: String,
        conversation: Option<ConversationTrace>,
    ) -> Self
    where
        S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    {
        Self {
            inner: Box::pin(stream),
            state,
            request_id,
            conversation,
            output: Arc::new(StdMutex::new(Vec::new())),
            finished: false,
        }
    }

    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let state = self.state.clone();
        let request_id = self.request_id.clone();
        let conversation = self.conversation.take();
        let output = self
            .output
            .lock()
            .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
            .unwrap_or_default();
        tokio::spawn(async move {
            state.remove_connection(&request_id).await;
            if let Some(trace) = conversation {
                let _ = save_conversation_record_async(trace, None, Some(output)).await;
            }
        });
    }
}

impl Stream for ConnectionBodyStream {
    type Item = Result<Bytes, reqwest::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                if let Ok(mut output) = self.output.lock() {
                    output.extend_from_slice(&bytes);
                }
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(None) => {
                self.finish();
                Poll::Ready(None)
            }
            other => other,
        }
    }
}

impl Drop for ConnectionBodyStream {
    fn drop(&mut self) {
        self.finish();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyProtocol {
    OpenAi,
    Gemini,
}

fn split_proxy_protocol(target: &str) -> (ProxyProtocol, &str) {
    if let Some(rest) = target.strip_prefix("/gemini") {
        let rest = if rest.is_empty() { "/" } else { rest };
        return (ProxyProtocol::Gemini, rest);
    }
    (ProxyProtocol::OpenAi, target)
}

fn client_key_for_protocol(
    protocol: ProxyProtocol,
    headers: &HeaderMap,
    query: Option<&str>,
) -> Option<String> {
    bearer_token(headers).or_else(|| match protocol {
        ProxyProtocol::OpenAi => None,
        ProxyProtocol::Gemini => headers
            .get("x-goog-api-key")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .or_else(|| query_key(query)),
    })
}

fn query_key(query: Option<&str>) -> Option<String> {
    query.and_then(|query| {
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(key, value)| key == "key" && !value.trim().is_empty())
            .map(|(_, value)| value.into_owned())
    })
}

async fn check_request_limits(
    state: &ServerState,
    cfg: &AppConfig,
    model: &str,
    key_name: &str,
    key_check: &KeyCheck,
) -> Result<Option<String>, ServerError> {
    let since = (Utc::now() - Duration::minutes(1)).to_rfc3339();

    if let Some(rpm) = key_check.virtual_key.as_ref().and_then(|key| key.rpm) {
        if state
            .database
            .request_count_since_async(since.clone(), Some(key_name.to_string()), None)
            .await?
            >= rpm as u64
        {
            return Ok(Some(format!("密钥 \"{key_name}\" 已达到 RPM 限额 {rpm}")));
        }
    }
    if let Some(budget) = key_check.virtual_key.as_ref().and_then(|key| key.budget) {
        if state
            .database
            .total_cost_async(Some(key_name.to_string()), None)
            .await?
            >= budget
        {
            return Ok(Some(format!("密钥 \"{key_name}\" 已达到预算限额 {budget}")));
        }
    }

    if let Some(rpm) = limit_u64(cfg.limits.get("global"), "rpm") {
        if state
            .database
            .request_count_since_async(since.clone(), None, None)
            .await?
            >= rpm
        {
            return Ok(Some(format!("全局 RPM 已达到限额 {rpm}")));
        }
    }
    if let Some(budget) = limit_f64(cfg.limits.get("global"), "budget") {
        if state.database.total_cost_async(None, None).await? >= budget {
            return Ok(Some(format!("全局预算已达到限额 {budget}")));
        }
    }
    if let Some(rpm) = limit_u64(cfg.limits.get(model), "rpm") {
        if state
            .database
            .request_count_since_async(since.clone(), None, Some(model.to_string()))
            .await?
            >= rpm
        {
            return Ok(Some(format!("模型 {model} 已达到 RPM 限额 {rpm}")));
        }
    }
    if let Some(budget) = limit_f64(cfg.limits.get(model), "budget") {
        if state
            .database
            .total_cost_async(None, Some(model.to_string()))
            .await?
            >= budget
        {
            return Ok(Some(format!("模型 {model} 已达到预算限额 {budget}")));
        }
    }
    Ok(None)
}

fn limit_u64(value: Option<&Value>, field: &str) -> Option<u64> {
    value
        .and_then(|value| value.get(field))
        .and_then(|value| value.as_u64().or_else(|| value.as_f64().map(|v| v as u64)))
        .filter(|value| *value > 0)
}

fn limit_f64(value: Option<&Value>, field: &str) -> Option<f64> {
    value
        .and_then(|value| value.get(field))
        .and_then(Value::as_f64)
        .filter(|value| *value > 0.0)
}

async fn proxy_handler(
    State(state): State<ServerState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    request: Request<Body>,
) -> Result<Response, ServerError> {
    let started = Instant::now();
    let cfg = state.current_config().await;
    let raw_path = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(uri.path());
    let target = raw_path.strip_prefix("/proxy").unwrap_or(raw_path);
    let (protocol, target) = split_proxy_protocol(target);
    let target_path = target.split('?').next().unwrap_or(target);
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let raw_body = to_bytes(request.into_body(), 200 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let json_body: Value = if content_type.contains("json") && !raw_body.is_empty() {
        serde_json::from_slice(&raw_body).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };
    let models_endpoint = match protocol {
        ProxyProtocol::OpenAi => is_models_endpoint(target_path),
        ProxyProtocol::Gemini => is_gemini_models_endpoint(target_path),
    };
    let request_model = get_request_model(&json_body, target_path);
    let auth_key = client_key_for_protocol(protocol, &headers, uri.query()).unwrap_or_default();
    let key_check = check_virtual_key(
        &cfg,
        &auth_key,
        if models_endpoint {
            None
        } else {
            request_model.as_deref()
        },
    );
    if !key_check.allowed {
        return Ok(limit_error(
            key_check.reason.unwrap_or_else(|| "无权访问".to_string()),
        ));
    }

    if models_endpoint {
        let allowed = key_check
            .virtual_key
            .as_ref()
            .and_then(|k| k.allowed_models.as_deref());
        return Ok(match protocol {
            ProxyProtocol::OpenAi => Json(build_models_response(&cfg, allowed)).into_response(),
            ProxyProtocol::Gemini => {
                Json(build_gemini_models_response(&cfg, allowed)).into_response()
            }
        });
    }

    let Some(model) = request_model else {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": "请求体缺少 model 字段，无法路由到兼容 API 服务商", "type": "invalid_request_error", "code": 400 } })),
        ).into_response());
    };
    let key_name = key_check
        .virtual_key
        .as_ref()
        .map(|key| {
            if key.name.trim().is_empty() {
                "virtual-key".to_string()
            } else {
                key.name.clone()
            }
        })
        .unwrap_or_else(|| "master".to_string());
    if let Some(message) = check_request_limits(&state, &cfg, &model, &key_name, &key_check).await?
    {
        return Ok(limit_error(message));
    }
    let request_id = Uuid::new_v4().to_string();
    let request_timestamp = Utc::now().to_rfc3339();
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let stream = json_body
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || (protocol == ProxyProtocol::Gemini && target_path.contains(":streamGenerateContent"));
    let Some(provider) = resolve_provider(&model, &cfg) else {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": format!("未知模型: {model}"), "type": "invalid_request_error", "code": 400 } })),
        ).into_response());
    };
    let upstream_url = match protocol {
        ProxyProtocol::OpenAi => build_upstream_url(&provider.base_url, target_path, &query),
        ProxyProtocol::Gemini => {
            build_gemini_upstream_url(&provider.base_url, target_path, &query, &provider.model_id)
        }
    }
    .map_err(|e| {
        ConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            e.to_string(),
        ))
    })?;
    let upstream_body = match protocol {
        ProxyProtocol::OpenAi if content_type.contains("json") => {
            rewrite_json_model(json_body.clone(), &provider.model_id)
        }
        _ => raw_body.to_vec(),
    };
    let mut req = state
        .client
        .request(method.clone(), upstream_url)
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::ACCEPT,
            headers
                .get(header::ACCEPT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/json"),
        );
    req = match protocol {
        ProxyProtocol::OpenAi => req.header(
            header::AUTHORIZATION,
            format!("Bearer {}", provider.api_key),
        ),
        ProxyProtocol::Gemini => req.header("x-goog-api-key", provider.api_key.clone()),
    };
    if let Some(ua) = provider.user_agent {
        req = req.header(header::USER_AGENT, ua);
    }
    for passthrough in [
        "openai-beta",
        "anthropic-version",
        "anthropic-beta",
        "idempotency-key",
    ] {
        if let Some(value) = headers.get(passthrough) {
            req = req.header(passthrough, value.clone());
        }
    }
    state
        .insert_connection(ActiveConnection {
            request_id: request_id.clone(),
            model: model.clone(),
            key_name: key_name.clone(),
            user_agent: user_agent.clone(),
            stream,
            started,
        })
        .await;
    let conversation = conversation_trace(
        &cfg,
        &request_timestamp,
        &request_id,
        &model,
        &key_name,
        &user_agent,
        &json_body,
    );
    let response = match req.body(upstream_body).send().await {
        Ok(response) => response,
        Err(err) => {
            state.remove_connection(&request_id).await;
            return Err(err.into());
        }
    };
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let response_content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("application/json"));
    let content_disposition = response.headers().get(header::CONTENT_DISPOSITION).cloned();
    let mut out = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, response_content_type.clone());
    if let Some(disposition) = content_disposition {
        out = out.header(header::CONTENT_DISPOSITION, disposition);
    }
    if stream {
        let database = state.database.clone();
        let pricing = cfg.pricing.clone();
        let usage_body = json_body.clone();
        let usage_model = model.clone();
        let usage_path = target_path.to_string();
        let usage_request_id = request_id.clone();
        let usage_timestamp = request_timestamp.clone();
        let duration_ms = started.elapsed().as_millis() as u64;
        let error = if status.is_success() {
            String::new()
        } else {
            format!("HTTP {status}")
        };
        tokio::spawn(async move {
            let estimated_input = estimate_tokens(&usage_body);
            let cost = calc_cost(&usage_model, estimated_input, 0, 0, 0, &pricing);
            let _ = database
                .record_usage_async(UsageLog {
                    timestamp: usage_timestamp,
                    request_id: usage_request_id,
                    model: usage_model,
                    key_name,
                    input_tokens: estimated_input,
                    cached_tokens: 0,
                    cached_write_tokens: 0,
                    output_tokens: 0,
                    cost,
                    status: status.as_u16(),
                    duration_ms,
                    stream,
                    user_agent,
                    error,
                    path: usage_path,
                })
                .await;
        });
        let stream = ConnectionBodyStream::new(
            response.bytes_stream(),
            state.clone(),
            request_id.clone(),
            conversation,
        );
        return Ok(out.body(Body::from_stream(stream)).unwrap());
    }

    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(err) => {
            state.remove_connection(&request_id).await;
            return Err(err.into());
        }
    };
    let estimated_input = estimate_tokens(&json_body);
    let mut usage = crate::proxy::UsageTokens {
        input_tokens: estimated_input,
        ..Default::default()
    };
    let mut response_json = None;
    if response_content_type
        .to_str()
        .unwrap_or("")
        .contains("json")
    {
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            usage = extract_usage_tokens(&value, estimated_input);
            response_json = Some(value);
        }
    }
    let cost = calc_cost(
        &model,
        usage.input_tokens,
        usage.output_tokens,
        usage.cached_tokens,
        usage.cached_write_tokens,
        &cfg.pricing,
    );
    let error = if status.is_success() {
        String::new()
    } else {
        String::from_utf8_lossy(&bytes)
            .chars()
            .take(300)
            .collect::<String>()
    };
    let _ = state
        .database
        .record_usage_async(UsageLog {
            timestamp: Utc::now().to_rfc3339(),
            request_id: request_id.clone(),
            model: model.clone(),
            key_name: key_name.clone(),
            input_tokens: usage.input_tokens,
            cached_tokens: usage.cached_tokens,
            cached_write_tokens: usage.cached_write_tokens,
            output_tokens: usage.output_tokens,
            cost,
            status: status.as_u16(),
            duration_ms: started.elapsed().as_millis() as u64,
            stream,
            user_agent,
            error,
            path: target_path.to_string(),
        })
        .await;
    if let Some(trace) = conversation {
        let output_raw = if response_json.is_none() {
            Some(String::from_utf8_lossy(&bytes).to_string())
        } else {
            None
        };
        let _ = save_conversation_record_async(trace, response_json, output_raw).await;
    }
    state.remove_connection(&request_id).await;
    Ok(out.body(Body::from(bytes)).unwrap())
}

async fn static_file(State(state): State<ServerState>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let relative = if path.is_empty() { "index.html" } else { path };
    let file = state
        .static_root
        .join("public")
        .join(safe_relative(relative));
    match tokio::fs::read(&file).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(file)
                .first_or_octet_stream()
                .to_string();
            ([(header::CONTENT_TYPE, mime)], bytes).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

fn safe_relative(path: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for component in Path::new(path).components() {
        if let Component::Normal(part) = component {
            out.push(part);
        }
    }
    out
}

fn require_admin(state: &ServerState, headers: &HeaderMap) -> Result<(), ServerError> {
    let Some(token) = bearer_token(headers) else {
        return Err(ServerError::Unauthorized);
    };
    require_admin_token(state, &token)
}

fn require_admin_or_query_token(
    state: &ServerState,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<(), ServerError> {
    if let Some(token) = query_token.filter(|token| !token.trim().is_empty()) {
        return require_admin_token(state, token);
    }
    require_admin(state, headers)
}

fn require_admin_token(state: &ServerState, token: &str) -> Result<(), ServerError> {
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(state.jwt_secret.as_bytes()),
        &Validation::default(),
    )?;
    Ok(())
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::to_string)
}

fn mask_key(key: &str) -> String {
    if key.len() <= 12 {
        return "****".to_string();
    }
    format!("{}****{}", &key[..8], &key[key.len() - 4..])
}

fn limit_error(message: String) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({ "error": { "message": message, "type": "limit_error", "code": 429 } })),
    )
        .into_response()
}

fn resolve_api_key(api_key: &str) -> String {
    api_key
        .strip_prefix("os.environ/")
        .and_then(|name| std::env::var(name).ok())
        .unwrap_or_else(|| api_key.to_string())
}

fn provider_base_url(provider_type: &str, base_url: &str) -> String {
    if !base_url.trim().is_empty() {
        return base_url.trim().to_string();
    }
    match provider_type {
        "anthropic" => "https://api.anthropic.com/v1".to_string(),
        "gemini" => "https://generativelanguage.googleapis.com/v1beta".to_string(),
        _ => "https://api.openai.com/v1".to_string(),
    }
}
