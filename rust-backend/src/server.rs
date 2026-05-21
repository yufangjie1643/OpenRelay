use crate::config::{
    ensure_files, load_config, save_config, AppConfig, ConfigError, ConversationStorage,
    VirtualKeyConfig,
};
use crate::proxy::{
    build_models_response, build_upstream_url, check_virtual_key, estimate_tokens,
    extract_usage_tokens, get_request_model, is_models_endpoint, resolve_provider,
    rewrite_json_model,
};
use axum::body::{to_bytes, Body};
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post, put};
use axum::{Json, Router};
use bcrypt::verify;
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio::net::TcpListener;
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
    client: reqwest::Client,
    jwt_secret: Arc<String>,
}

impl ServerState {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root: Arc::new(root),
            client: reqwest::Client::new(),
            jwt_secret: Arc::new(
                std::env::var("JWT_SECRET")
                    .unwrap_or_else(|_| "openrelay-webui-secret-change-me".to_string()),
            ),
        }
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
    serve(root_from_env(), port_from_env()).await
}

pub fn root_from_env() -> PathBuf {
    std::env::var("OPENRELAY_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap_or(Path::new("."))
                .to_path_buf()
        })
}

pub fn port_from_env() -> u16 {
    std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(18783)
}

pub async fn serve(root: PathBuf, port: u16) -> Result<(), ServerError> {
    ensure_files(&root)?;
    let addr = format!("0.0.0.0:{port}");
    let listener = TcpListener::bind(&addr).await?;
    println!("OpenRelay Rust backend running at http://localhost:{port}");
    axum::serve(listener, build_router(ServerState::new(root))).await?;
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
    ensure_files(&root)?;
    let addr = format!("0.0.0.0:{port}");
    let listener = TcpListener::bind(&addr).await?;
    println!("OpenRelay Rust backend running at http://localhost:{port}");
    axum::serve(listener, build_router(ServerState::new(root)))
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

async fn login(
    State(state): State<ServerState>,
    Json(payload): Json<LoginRequest>,
) -> Result<impl IntoResponse, ServerError> {
    let cfg = load_config(&state.root)?;
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
    let mut cfg = load_config(&state.root)?;
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
    Ok(Json(json!({ "success": true })).into_response())
}

async fn get_config(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let mut safe = serde_json::to_value(load_config(&state.root)?)?;
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
    Json(mut incoming): Json<AppConfig>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let current = load_config(&state.root)?;
    incoming.admin = current.admin;
    incoming.virtual_keys = current.virtual_keys;
    save_config(&state.root, &incoming)?;
    Ok(Json(json!({ "success": true })).into_response())
}

async fn restart_proxy() -> Json<Value> {
    Json(json!({
        "success": true,
        "unified": true,
        "message": "配置已保存，OpenRelay Rust 后端会在下一次请求时读取最新配置。"
    }))
}

async fn get_pricing(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    Ok(Json(load_config(&state.root)?.pricing).into_response())
}

async fn post_pricing(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(pricing): Json<Value>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let mut cfg = load_config(&state.root)?;
    cfg.pricing = pricing;
    save_config(&state.root, &cfg)?;
    Ok(Json(json!({ "success": true })).into_response())
}

async fn get_limits(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    Ok(Json(load_config(&state.root)?.limits).into_response())
}

async fn post_limits(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(limits): Json<Value>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let mut cfg = load_config(&state.root)?;
    cfg.limits = limits;
    save_config(&state.root, &cfg)?;
    Ok(Json(json!({ "success": true })).into_response())
}

async fn get_virtual_keys(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let keys: Vec<Value> = load_config(&state.root)?
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
    let mut cfg = load_config(&state.root)?;
    key.key = format!("sk-vk-{}", Uuid::new_v4().simple());
    cfg.virtual_keys.push(key.clone());
    save_config(&state.root, &cfg)?;
    Ok(Json(json!({ "success": true, "key": key })).into_response())
}

async fn put_virtual_key(
    AxumPath(index): AxumPath<usize>,
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(mut key): Json<VirtualKeyConfig>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let mut cfg = load_config(&state.root)?;
    if index >= cfg.virtual_keys.len() {
        return Ok((StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" }))).into_response());
    }
    key.key = cfg.virtual_keys[index].key.clone();
    cfg.virtual_keys[index] = key;
    save_config(&state.root, &cfg)?;
    Ok(Json(json!({ "success": true })).into_response())
}

async fn delete_virtual_key(
    AxumPath(index): AxumPath<usize>,
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let mut cfg = load_config(&state.root)?;
    if index >= cfg.virtual_keys.len() {
        return Ok((StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" }))).into_response());
    }
    cfg.virtual_keys.remove(index);
    save_config(&state.root, &cfg)?;
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
    let url = crate::proxy::build_upstream_url(&base_url, "/v1/models", "").map_err(|e| {
        ConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            e.to_string(),
        ))
    })?;
    let response = state
        .client
        .get(url)
        .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
        .header(
            header::USER_AGENT,
            payload
                .user_agent
                .unwrap_or_else(|| "OpenRelay-Gateway/1.0".to_string()),
        )
        .send()
        .await;
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
    let models = value
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(Json(json!({ "success": true, "models": models })).into_response())
}

async fn detect_models(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let cfg = load_config(&state.root)?;
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
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    Ok(Json(json!({
        "stats": { "totalRequests": 0, "totalInputTokens": 0, "totalOutputTokens": 0, "totalCost": 0, "byModel": {}, "byKey": {} },
        "logs": [],
        "pagination": { "page": 1, "pageSize": 20, "total": 0, "totalPages": 1 }
    })).into_response())
}

async fn export_usage(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"usage-export-rust.csv\"",
            ),
        ],
        "timestamp,model,key_name,input_tokens,cached_tokens,output_tokens,cost,status\n",
    )
        .into_response())
}

async fn clear_usage() -> Json<Value> {
    Json(json!({ "success": true }))
}
async fn merge_usage() -> Json<Value> {
    Json(json!({ "success": true, "changed": false }))
}
async fn get_conversations() -> Json<Value> {
    Json(json!({ "enabled": false, "entries": [] }))
}
async fn get_conversation() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" }))).into_response()
}
async fn delete_conversation() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" }))).into_response()
}
async fn post_conversation_storage(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(storage): Json<ConversationStorage>,
) -> Result<Response, ServerError> {
    require_admin(&state, &headers)?;
    let mut cfg = load_config(&state.root)?;
    cfg.conversation_storage = storage;
    save_config(&state.root, &cfg)?;
    Ok(Json(json!({ "success": true })).into_response())
}
async fn open_conversation_storage() -> Json<Value> {
    Json(json!({ "success": false, "error": "Not implemented in Rust prototype" }))
}
async fn get_connections() -> Json<Value> {
    Json(json!({ "entries": [] }))
}
async fn abort_connection() -> Json<Value> {
    Json(
        json!({ "success": false, "error": "Connection tracking is not implemented in Rust prototype" }),
    )
}

async fn proxy_handler(
    State(state): State<ServerState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    request: Request<Body>,
) -> Result<Response, ServerError> {
    let cfg = load_config(&state.root)?;
    let raw_path = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(uri.path());
    let target = raw_path.strip_prefix("/proxy").unwrap_or(raw_path);
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
    let models_endpoint = is_models_endpoint(target_path);
    let request_model = get_request_model(&json_body, target_path);
    let auth_key = bearer_token(&headers).unwrap_or_default();
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
        return Ok(Json(build_models_response(&cfg, allowed)).into_response());
    }

    let Some(model) = request_model else {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": "请求体缺少 model 字段，无法路由到兼容 API 服务商", "type": "invalid_request_error", "code": 400 } })),
        ).into_response());
    };
    let Some(provider) = resolve_provider(&model, &cfg) else {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": format!("未知模型: {model}"), "type": "invalid_request_error", "code": 400 } })),
        ).into_response());
    };
    let upstream_url =
        build_upstream_url(&provider.base_url, target_path, &query).map_err(|e| {
            ConfigError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                e.to_string(),
            ))
        })?;
    let upstream_body = if content_type.contains("json") {
        rewrite_json_model(json_body.clone(), &provider.model_id)
    } else {
        raw_body.to_vec()
    };
    let mut req = state
        .client
        .request(method.clone(), upstream_url)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", provider.api_key),
        )
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::ACCEPT,
            headers
                .get(header::ACCEPT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/json"),
        );
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
    let response = req.body(upstream_body).send().await?;
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let response_content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("application/json"));
    let content_disposition = response.headers().get(header::CONTENT_DISPOSITION).cloned();
    let bytes = response.bytes().await?;
    let _estimated_input = estimate_tokens(&json_body);
    if response_content_type
        .to_str()
        .unwrap_or("")
        .contains("json")
    {
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            let _usage = extract_usage_tokens(&value, _estimated_input);
        }
    }
    let mut out = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, response_content_type);
    if let Some(disposition) = content_disposition {
        out = out.header(header::CONTENT_DISPOSITION, disposition);
    }
    Ok(out.body(Body::from(bytes)).unwrap())
}

async fn static_file(State(state): State<ServerState>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let relative = if path.is_empty() { "index.html" } else { path };
    let file = if let Some(vendor_path) = relative.strip_prefix("vendor/js-yaml/") {
        state
            .root
            .join("web")
            .join("node_modules")
            .join("js-yaml")
            .join("dist")
            .join(safe_relative(vendor_path))
    } else {
        state
            .root
            .join("web")
            .join("public")
            .join(safe_relative(relative))
    };
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
    decode::<Claims>(
        &token,
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
