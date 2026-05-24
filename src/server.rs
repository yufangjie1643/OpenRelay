use crate::backups::{
    create_config_backup, export_current_config, list_config_backups, load_backup_config,
    restore_config_backup, BackupError,
};
use crate::config::{
    load_config, save_config, AppConfig, ConfigError, ConversationStorage, VirtualKeyConfig,
};
use crate::database::{
    ConversationEntry, Database, DatabaseError, UsageBucket, UsageLog, UsageStats,
};
use crate::health::{check_all_providers, check_provider};
use crate::migration::{migrate_legacy_data, MigrationError};
use crate::paths::{data_root_from_env, static_root_from_env, AppPaths};
use crate::proxy::{
    build_gemini_models_response, build_gemini_upstream_url, build_models_response,
    build_upstream_url, calc_cost, check_virtual_key, estimate_tokens, extract_usage_tokens,
    get_request_model, is_gemini_models_endpoint, is_models_endpoint, resolve_provider,
    rewrite_json_model, KeyCheck,
};
use crate::release::{app_status, check_update, set_startup_enabled, StartupRequest};
use crate::secrets::{audit_security, protect_config_secrets, SecretError};
use axum::body::{to_bytes, Body, Bytes};
use axum::extract::{Path as AxumPath, Query, State};
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
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll};
use std::time::{Duration as StdDuration, Instant};
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
    #[error("backup error: {0}")]
    Backup(#[from] BackupError),
    #[error("secret error: {0}")]
    Secret(#[from] SecretError),
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

#[derive(Debug, Deserialize)]
struct UsageAnalyticsQuery {
    #[serde(default)]
    period: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ValidateConfigQuery {
    #[serde(default)]
    reachability: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ExportConfigQuery {
    #[serde(default)]
    redacted: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ImportConfigRequest {
    config: Value,
}

#[derive(Debug, Clone, Serialize)]
struct PresetPricing {
    input: f64,
    cached_input: f64,
    cached_write: f64,
    output: f64,
}

#[derive(Debug, Clone, Serialize)]
struct PresetModel {
    id: &'static str,
    name: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pricing: Option<PresetPricing>,
}

#[derive(Debug, Clone, Serialize)]
struct ProviderPreset {
    id: &'static str,
    name: &'static str,
    #[serde(rename = "type")]
    provider_type: &'static str,
    base_url: &'static str,
    api_key_placeholder: &'static str,
    user_agent: &'static str,
    models: Vec<PresetModel>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigIssue {
    severity: &'static str,
    code: &'static str,
    path: String,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BudgetWarning {
    kind: &'static str,
    name: String,
    used: f64,
    budget: f64,
    percent: f64,
    level: &'static str,
}

pub fn build_router(state: ServerState) -> Router {
    Router::new()
        .route("/api/login", post(login))
        .route("/api/change-password", post(change_password))
        .route("/api/app/status", get(get_app_status))
        .route("/api/app/update-check", get(get_app_update_check))
        .route("/api/app/startup", post(post_app_startup))
        .route("/api/security/audit", get(get_security_audit))
        .route("/api/security/protect-secrets", post(post_protect_secrets))
        .route("/api/config", get(get_config).post(post_config))
        .route(
            "/api/config/backups",
            get(get_config_backups).post(post_config_backup),
        )
        .route("/api/config/backups/:id/export", get(export_config_backup))
        .route(
            "/api/config/backups/:id/restore",
            post(restore_config_backup_route),
        )
        .route("/api/config/import", post(import_config))
        .route("/api/config/validate", post(validate_config))
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
        .route("/api/provider-presets", get(get_provider_presets))
        .route(
            "/api/providers/health",
            get(get_provider_health).post(post_provider_health),
        )
        .route("/api/detect-models", get(detect_models))
        .route("/api/usage/analytics", get(get_usage_analytics))
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

async fn get_app_status(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    Ok(Json(app_status(&state.root, &state.static_root)).into_response())
}

async fn get_app_update_check(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    Ok(Json(check_update(&state.client).await).into_response())
}

async fn post_app_startup(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(payload): Json<StartupRequest>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let startup = set_startup_enabled(payload.enabled)?;
    Ok(Json(json!({ "success": true, "startup": startup })).into_response())
}

async fn get_security_audit(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let cfg = state.current_config().await;
    let findings = audit_security(&state.root, &cfg, std::env::var_os("JWT_SECRET").is_some())?;
    let ok = !findings.iter().any(|finding| finding.severity == "danger");
    Ok(Json(json!({ "ok": ok, "findings": findings })).into_response())
}

async fn post_protect_secrets(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let cfg = state.current_config().await;
    let result = protect_config_secrets(&state.root, &cfg)?;
    state.replace_config(load_config(&state.root)?).await;
    Ok(Json(json!({
        "success": result.supported,
        "supported": result.supported,
        "enabled": result.enabled,
        "protectedCount": result.protected_count,
        "message": result.message
    }))
    .into_response())
}

async fn get_config_backups(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    Ok(Json(json!({ "backups": list_config_backups(&state.root)? })).into_response())
}

async fn post_config_backup(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let cfg = state.current_config().await;
    let backup = create_config_backup(&state.root, &cfg, "manual")?;
    Ok(Json(json!({ "success": true, "backup": backup })).into_response())
}

async fn export_config_backup(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ExportConfigQuery>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let cfg = if id == "current" {
        state.current_config().await
    } else {
        load_backup_config(&state.root, &id)?
    };
    let value = export_current_config(&cfg, query.redacted.unwrap_or(true))?;
    Ok(Json(json!({ "config": value })).into_response())
}

async fn restore_config_backup_route(
    State(state): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let current = state.current_config().await;
    create_config_backup(&state.root, &current, "before-restore")?;
    let restored = restore_config_backup(&state.root, &id)?;
    state.replace_config(restored).await;
    Ok(Json(json!({ "success": true })).into_response())
}

async fn import_config(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(payload): Json<ImportConfigRequest>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let imported: AppConfig = serde_json::from_value(payload.config)?;
    let current = state.current_config().await;
    create_config_backup(&state.root, &current, "before-import")?;
    save_config(&state.root, &imported)?;
    state.replace_config(imported).await;
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
    if let Some(root) = safe.as_object_mut() {
        root.insert(
            "header_candidates".to_string(),
            json!({
                "user_agents": state.database.user_agent_candidates_async(30).await?
            }),
        );
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
        incoming.remove("header_candidates");
    }
    create_config_backup(&state.root, &current, "before-config-save")?;
    let mut incoming: AppConfig = serde_json::from_value(incoming_value)?;
    incoming.admin = current.admin;
    incoming.virtual_keys = current.virtual_keys;
    save_config(&state.root, &incoming)?;
    state.replace_config(incoming).await;
    Ok(Json(json!({ "success": true })).into_response())
}

async fn validate_config(
    State(state): State<ServerState>,
    Query(query): Query<ValidateConfigQuery>,
    headers: HeaderMap,
    Json(mut value): Json<Value>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let current = state.current_config().await;
    if let Some(incoming) = value.as_object_mut() {
        incoming.insert("admin".to_string(), serde_json::to_value(&current.admin)?);
        incoming.insert(
            "virtual_keys".to_string(),
            serde_json::to_value(&current.virtual_keys)?,
        );
        incoming.remove("header_candidates");
    }
    let cfg: AppConfig = serde_json::from_value(value)?;
    let mut issues = validate_config_issues(&cfg);
    if query.reachability.unwrap_or(false) {
        issues.extend(validate_provider_reachability(&state, &cfg).await);
    }
    let ok = !issues.iter().any(|issue| issue.severity == "error");
    Ok(Json(json!({ "ok": ok, "issues": issues })).into_response())
}

async fn get_provider_presets(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    Ok(Json(json!({ "presets": provider_presets() })).into_response())
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

async fn get_provider_health(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let cfg = state.current_config().await;
    let providers = check_all_providers(&state.client, &cfg).await;
    Ok(Json(json!({ "providers": providers })).into_response())
}

async fn post_provider_health(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(provider): Json<crate::config::ProviderConfig>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let provider = check_provider(&state.client, &provider).await;
    Ok(Json(json!({ "provider": provider })).into_response())
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

async fn get_usage_analytics(
    State(state): State<ServerState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<UsageAnalyticsQuery>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let period = query.period.unwrap_or_else(|| "day".to_string());
    let analytics = state.database.usage_analytics_async(period, 90, 10).await?;
    let cfg = state.current_config().await;
    let page = state.database.usage_page_async(1, 1).await?;
    let budget_warnings = build_budget_warnings(&cfg, &page.stats);
    Ok(Json(json!({
        "analytics": analytics,
        "budgetWarnings": budget_warnings
    }))
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
    axum::extract::Query(query): axum::extract::Query<UsageQuery>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let storage = state.current_config().await.conversation_storage;
    if !storage.enabled || storage.directory.trim().is_empty() {
        return Ok(Json(json!({ "enabled": false, "entries": [] })).into_response());
    }
    let directory = PathBuf::from(storage.directory.trim());
    let page = tokio::task::spawn_blocking({
        let database = state.database.clone();
        let directory = directory.clone();
        move || {
            sync_conversation_index(&database, &directory)?;
            database
                .conversation_page(
                    &directory,
                    query.page.unwrap_or(1),
                    query.page_size.unwrap_or(20),
                )
                .map_err(ServerError::Database)
        }
    })
    .await
    .map_err(|err| DatabaseError::BlockingTask(err.to_string()))??;
    Ok(Json(json!({
        "enabled": true,
        "directory": directory.to_string_lossy(),
        "entries": page.entries,
        "pagination": page.pagination
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
    let database = state.database.clone();
    let directory = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let removed = tokio::task::spawn_blocking(move || match std::fs::remove_file(&path) {
        Ok(()) => {
            database.remove_conversation_index(&directory, &filename)?;
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            database.remove_conversation_index(&directory, &filename)?;
            Ok(false)
        }
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

fn sync_conversation_index(database: &Database, directory: &Path) -> Result<(), ServerError> {
    let indexed = database.conversation_index_files(directory)?;
    if !directory.exists() {
        for filename in indexed.keys() {
            database.remove_conversation_index(directory, filename)?;
        }
        return Ok(());
    }
    let mut seen = BTreeSet::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let filename = filename.to_string();
        seen.insert(filename.clone());
        let metadata = entry.metadata()?;
        let file_size = metadata.len();
        let modified_at = file_modified_at(&metadata);
        if indexed
            .get(&filename)
            .is_some_and(|(size, modified)| *size == file_size && *modified == modified_at)
        {
            continue;
        }
        let fallback_timestamp = unix_milliseconds_to_rfc3339(modified_at);
        let index_entry =
            conversation_index_entry_from_file(database, &path, &filename, &fallback_timestamp)?;
        database.upsert_conversation_index(directory, &index_entry, file_size, modified_at)?;
    }
    for filename in indexed.keys() {
        if !seen.contains(filename) {
            database.remove_conversation_index(directory, filename)?;
        }
    }
    Ok(())
}

fn conversation_index_entry_from_file(
    database: &Database,
    path: &Path,
    filename: &str,
    fallback_timestamp: &str,
) -> Result<ConversationEntry, ServerError> {
    let request_id = conversation_request_id_from_filename(filename).unwrap_or_default();
    if !request_id.is_empty() {
        if let Some(entry) = database.conversation_usage_metadata(&request_id, filename)? {
            return Ok(entry);
        }
    }
    if let Ok(value) = read_conversation_file(path) {
        return Ok(conversation_index_entry_from_value(
            filename,
            &value,
            &request_id,
            fallback_timestamp,
        ));
    }
    Ok(ConversationEntry {
        filename: filename.to_string(),
        timestamp: fallback_timestamp.to_string(),
        model: String::new(),
        key_name: "master".to_string(),
        request_id,
    })
}

fn conversation_index_entry_from_value(
    filename: &str,
    value: &Value,
    fallback_request_id: &str,
    fallback_timestamp: &str,
) -> ConversationEntry {
    ConversationEntry {
        filename: filename.to_string(),
        timestamp: string_field(value, "timestamp")
            .unwrap_or_else(|| fallback_timestamp.to_string()),
        model: string_field(value, "model").unwrap_or_default(),
        key_name: string_field(value, "key_name").unwrap_or_else(|| "master".to_string()),
        request_id: string_field(value, "request_id")
            .unwrap_or_else(|| fallback_request_id.to_string()),
    }
}

fn conversation_request_id_from_filename(filename: &str) -> Option<String> {
    let stem = filename.strip_suffix(".json")?;
    let (_, request_id) = stem.split_once('-')?;
    if request_id.trim().is_empty() {
        None
    } else {
        Some(request_id.to_string())
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn file_modified_at(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn unix_milliseconds_to_rfc3339(milliseconds: i64) -> String {
    chrono::DateTime::<Utc>::from_timestamp_millis(milliseconds)
        .unwrap_or_else(Utc::now)
        .to_rfc3339()
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
    database: Database,
    trace: ConversationTrace,
    output: Option<Value>,
    output_raw: Option<String>,
) -> Result<(), ServerError> {
    tokio::task::spawn_blocking(move || {
        write_conversation_record(database, trace, output, output_raw)
    })
    .await
    .map_err(|err| DatabaseError::BlockingTask(err.to_string()))??;
    Ok(())
}

fn write_conversation_record(
    database: Database,
    trace: ConversationTrace,
    output: Option<Value>,
    output_raw: Option<String>,
) -> Result<(), ServerError> {
    std::fs::create_dir_all(&trace.directory)?;
    let index_entry = ConversationEntry {
        filename: trace.filename.clone(),
        timestamp: trace.timestamp.clone(),
        model: trace.model.clone(),
        key_name: trace.key_name.clone(),
        request_id: trace.request_id.clone(),
    };
    let path = trace.directory.join(&trace.filename);
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
    std::fs::write(&path, serde_json::to_vec_pretty(&record)?)?;
    let metadata = std::fs::metadata(&path)?;
    database.upsert_conversation_index(
        &trace.directory,
        &index_entry,
        metadata.len(),
        file_modified_at(&metadata),
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
                let _ = save_conversation_record_async(
                    state.database.clone(),
                    trace,
                    None,
                    Some(output),
                )
                .await;
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
        let _ = save_conversation_record_async(
            state.database.clone(),
            trace,
            response_json,
            output_raw,
        )
        .await;
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

fn provider_presets() -> Vec<ProviderPreset> {
    vec![
        ProviderPreset {
            id: "openai",
            name: "OpenAI",
            provider_type: "openai",
            base_url: "https://api.openai.com/v1",
            api_key_placeholder: "os.environ/OPENAI_API_KEY",
            user_agent: "OpenRelay-Gateway/1.0",
            models: vec![
                preset_model("gpt-4o-mini", 0.15, 0.075, 0.0, 0.60),
                preset_model("gpt-4o", 2.50, 1.25, 0.0, 10.00),
                preset_model("gpt-4.1-mini", 0.40, 0.10, 0.0, 1.60),
            ],
        },
        ProviderPreset {
            id: "gemini",
            name: "Google Gemini",
            provider_type: "gemini",
            base_url: "https://generativelanguage.googleapis.com/v1beta",
            api_key_placeholder: "os.environ/GEMINI_API_KEY",
            user_agent: "OpenRelay-Gateway/1.0",
            models: vec![
                preset_model("gemini-2.5-flash", 0.30, 0.075, 0.0, 2.50),
                preset_model("gemini-2.5-pro", 1.25, 0.31, 0.0, 10.00),
                preset_model("gemini-1.5-flash", 0.075, 0.01875, 0.0, 0.30),
            ],
        },
        ProviderPreset {
            id: "anthropic",
            name: "Anthropic",
            provider_type: "anthropic",
            base_url: "https://api.anthropic.com/v1",
            api_key_placeholder: "os.environ/ANTHROPIC_API_KEY",
            user_agent: "OpenRelay-Gateway/1.0",
            models: vec![
                preset_model("claude-sonnet-4-5", 3.0, 0.30, 3.75, 15.0),
                preset_model("claude-haiku-4-5", 1.0, 0.10, 1.25, 5.0),
                preset_model("claude-3-5-sonnet-20241022", 3.0, 0.30, 3.75, 15.0),
            ],
        },
        ProviderPreset {
            id: "deepseek",
            name: "DeepSeek",
            provider_type: "openai-custom",
            base_url: "https://api.deepseek.com/v1",
            api_key_placeholder: "os.environ/DEEPSEEK_API_KEY",
            user_agent: "OpenRelay-Gateway/1.0",
            models: vec![
                preset_model("deepseek-chat", 0.14, 0.028, 0.0, 0.28),
                preset_model("deepseek-reasoner", 0.14, 0.028, 0.0, 0.28),
                preset_model("deepseek-v4-pro", 1.74, 0.145, 0.0, 3.48),
            ],
        },
        ProviderPreset {
            id: "openrouter",
            name: "OpenRouter",
            provider_type: "openai-custom",
            base_url: "https://openrouter.ai/api/v1",
            api_key_placeholder: "os.environ/OPENROUTER_API_KEY",
            user_agent: "OpenRelay-Gateway/1.0",
            models: vec![
                preset_model("anthropic/claude-sonnet-4.6", 3.0, 0.30, 3.75, 15.0),
                preset_model("deepseek/deepseek-v3.2", 0.28, 0.28, 0.28, 0.40),
                preset_model("google/gemini-3-flash-preview", 0.30, 0.075, 0.0, 2.50),
            ],
        },
        ProviderPreset {
            id: "siliconflow",
            name: "SiliconFlow",
            provider_type: "openai-custom",
            base_url: "https://api.siliconflow.com/v1",
            api_key_placeholder: "os.environ/SILICONFLOW_API_KEY",
            user_agent: "OpenRelay-Gateway/1.0",
            models: vec![
                preset_model("Qwen/Qwen3-30B-A3B-Thinking-2507", 0.09, 0.09, 0.09, 0.30),
                preset_model("Qwen/Qwen2.5-Coder-32B-Instruct", 0.18, 0.18, 0.18, 0.18),
                preset_model("deepseek-ai/DeepSeek-V3.2", 0.50, 0.50, 0.50, 2.00),
            ],
        },
        ProviderPreset {
            id: "siliconflow-cn",
            name: "SiliconFlow (China)",
            provider_type: "openai-custom",
            base_url: "https://api.siliconflow.cn/v1",
            api_key_placeholder: "os.environ/SILICONFLOW_CN_API_KEY",
            user_agent: "OpenRelay-Gateway/1.0",
            models: vec![
                preset_model("Qwen/Qwen3.5-397B-A17B", 0.29, 0.29, 0.29, 1.74),
                preset_model("Qwen/Qwen3.5-35B-A3B", 0.23, 0.23, 0.23, 1.86),
                preset_model("siliconflow/deepseek-v3.2", 0.14, 0.14, 0.14, 0.28),
                preset_model("siliconflow/deepseek-r1-0528", 0.70, 0.70, 0.70, 2.50),
            ],
        },
        ProviderPreset {
            id: "bailian",
            name: "Alibaba Cloud Bailian",
            provider_type: "openai-custom",
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
            api_key_placeholder: "os.environ/DASHSCOPE_API_KEY",
            user_agent: "OpenRelay-Gateway/1.0",
            models: vec![
                preset_model("qwen-plus", 0.11, 0.11, 0.11, 0.30),
                preset_model("qwen-turbo", 0.05, 0.05, 0.05, 0.20),
                preset_model("qwen-max", 1.20, 1.20, 1.20, 6.00),
                preset_model("qwq-plus", 0.40, 0.40, 0.40, 1.60),
            ],
        },
    ]
}

fn preset_model(
    id: &'static str,
    input: f64,
    cached_input: f64,
    cached_write: f64,
    output: f64,
) -> PresetModel {
    PresetModel {
        id,
        name: id,
        pricing: Some(PresetPricing {
            input,
            cached_input,
            cached_write,
            output,
        }),
    }
}

fn validate_config_issues(cfg: &AppConfig) -> Vec<ConfigIssue> {
    let mut issues = Vec::new();
    let mut model_owners: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for (provider_index, provider) in cfg.providers.iter().enumerate() {
        let provider_path = format!("providers[{provider_index}]");
        if provider.name.trim().is_empty() {
            issues.push(config_issue(
                "error",
                "missing_provider_name",
                &provider_path,
                "服务商名称不能为空",
            ));
        }
        if provider.api_key.trim().is_empty() {
            issues.push(config_issue(
                "error",
                "missing_api_key",
                &format!("{provider_path}.api_key"),
                format!("{} 缺少 API Key", provider_display_name(provider)),
            ));
        }
        if let Some(base_url) = provider
            .base_url
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            match url::Url::parse(base_url.trim()) {
                Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => {}
                _ => issues.push(config_issue(
                    "error",
                    "invalid_base_url",
                    &format!("{provider_path}.base_url"),
                    format!(
                        "{} 的 Base URL 必须以 http:// 或 https:// 开头",
                        provider_display_name(provider)
                    ),
                )),
            }
        }
        if provider.models.is_empty() {
            issues.push(config_issue(
                "warning",
                "missing_models",
                &format!("{provider_path}.models"),
                format!("{} 还没有模型映射", provider_display_name(provider)),
            ));
        }
        for (model_index, model) in provider.models.iter().enumerate() {
            let alias = model.model_name.trim();
            if alias.is_empty() {
                issues.push(config_issue(
                    "error",
                    "missing_model_alias",
                    &format!("{provider_path}.models[{model_index}].model_name"),
                    format!("{} 存在空模型别名", provider_display_name(provider)),
                ));
                continue;
            }
            model_owners
                .entry(alias.to_string())
                .or_default()
                .push(provider_display_name(provider));
            if !pricing_present(&cfg.pricing, alias) {
                issues.push(config_issue(
                    "warning",
                    "missing_pricing",
                    &format!("pricing.{alias}"),
                    format!("{alias} 缺少有效输入/输出定价，成本统计会低估"),
                ));
            }
        }
    }

    for (model, owners) in model_owners {
        if owners.len() > 1 {
            issues.push(config_issue(
                "error",
                "duplicate_model_alias",
                &format!("providers.models.{model}"),
                format!("模型别名 {model} 在多个服务商中重复: {}", owners.join(", ")),
            ));
        }
    }

    issues
}

fn config_issue(
    severity: &'static str,
    code: &'static str,
    path: &str,
    message: impl Into<String>,
) -> ConfigIssue {
    ConfigIssue {
        severity,
        code,
        path: path.to_string(),
        message: message.into(),
    }
}

fn provider_display_name(provider: &crate::config::ProviderConfig) -> String {
    if provider.name.trim().is_empty() {
        provider.id.clone()
    } else {
        provider.name.clone()
    }
}

fn pricing_present(pricing: &Value, model: &str) -> bool {
    let Some(entry) = pricing.get(model) else {
        return false;
    };
    let input = entry.get("input").and_then(Value::as_f64).unwrap_or(0.0);
    let output = entry.get("output").and_then(Value::as_f64).unwrap_or(0.0);
    let cached = entry
        .get("cached_input")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let cached_write = entry
        .get("cached_write")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    input > 0.0 || output > 0.0 || cached > 0.0 || cached_write > 0.0
}

async fn validate_provider_reachability(state: &ServerState, cfg: &AppConfig) -> Vec<ConfigIssue> {
    let mut issues = Vec::new();

    for (provider_index, provider) in cfg.providers.iter().enumerate() {
        let provider_path = format!("providers[{provider_index}]");
        if provider.api_key.trim().is_empty() {
            continue;
        }
        if let Some(base_url) = provider
            .base_url
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            if url::Url::parse(base_url.trim()).is_err() {
                continue;
            }
        }

        let resolved_key = resolve_api_key(&provider.api_key);
        if provider.api_key.starts_with("os.environ/") && resolved_key == provider.api_key {
            issues.push(config_issue(
                "warning",
                "unresolved_api_key_env",
                &format!("{provider_path}.api_key"),
                format!(
                    "{} 使用的环境变量暂未设置，已跳过可达性检查",
                    provider_display_name(provider)
                ),
            ));
            continue;
        }

        let base_url = provider_base_url(
            &provider.provider_type,
            provider.base_url.as_deref().unwrap_or_default(),
        );
        let is_gemini = provider.provider_type == "gemini";
        let url = if is_gemini {
            build_gemini_upstream_url(&base_url, "/v1beta/models", "", "")
        } else {
            build_upstream_url(&base_url, "/v1/models", "")
        };
        let Ok(url) = url else {
            continue;
        };

        let mut request = state
            .client
            .get(url)
            .timeout(StdDuration::from_secs(3))
            .header(
                header::USER_AGENT,
                provider
                    .user_agent
                    .clone()
                    .unwrap_or_else(|| "OpenRelay-Gateway/1.0".to_string()),
            );
        request = if is_gemini {
            request.header("x-goog-api-key", resolved_key)
        } else {
            request.header(header::AUTHORIZATION, format!("Bearer {resolved_key}"))
        };

        match request.send().await {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => issues.push(config_issue(
                "warning",
                "provider_unreachable",
                &format!("{provider_path}.base_url"),
                format!(
                    "{} 可达性检查返回 HTTP {}",
                    provider_display_name(provider),
                    response.status()
                ),
            )),
            Err(err) => issues.push(config_issue(
                "warning",
                "provider_unreachable",
                &format!("{provider_path}.base_url"),
                format!("{} 暂时不可达: {}", provider_display_name(provider), err),
            )),
        }
    }

    issues
}

fn build_budget_warnings(cfg: &AppConfig, stats: &UsageStats) -> Vec<BudgetWarning> {
    let mut warnings = Vec::new();
    if let Some(budget) = cfg
        .limits
        .get("global")
        .and_then(|global| global.get("budget"))
        .and_then(Value::as_f64)
    {
        if let Some(warning) = budget_warning("global", "全局预算", stats.total_cost, budget) {
            warnings.push(warning);
        }
    }
    for (model, bucket) in &stats.by_model {
        let Some(budget) = cfg
            .limits
            .get(model)
            .and_then(|limits| limits.get("budget"))
            .and_then(Value::as_f64)
        else {
            continue;
        };
        if let Some(warning) = bucket_budget_warning("model", model, bucket, budget) {
            warnings.push(warning);
        }
    }
    for key in &cfg.virtual_keys {
        let Some(budget) = key.budget else {
            continue;
        };
        let Some(bucket) = stats.by_key.get(&key.name) else {
            continue;
        };
        if let Some(warning) = bucket_budget_warning("key", &key.name, bucket, budget) {
            warnings.push(warning);
        }
    }
    warnings
}

fn bucket_budget_warning(
    kind: &'static str,
    name: &str,
    bucket: &UsageBucket,
    budget: f64,
) -> Option<BudgetWarning> {
    budget_warning(kind, name, bucket.cost, budget)
}

fn budget_warning(
    kind: &'static str,
    name: impl Into<String>,
    used: f64,
    budget: f64,
) -> Option<BudgetWarning> {
    if budget <= 0.0 {
        return None;
    }
    let percent = round2(used * 100.0 / budget);
    if percent < 80.0 {
        return None;
    }
    Some(BudgetWarning {
        kind,
        name: name.into(),
        used,
        budget,
        percent,
        level: if percent >= 100.0 {
            "danger"
        } else {
            "warning"
        },
    })
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
