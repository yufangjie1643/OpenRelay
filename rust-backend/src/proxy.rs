use crate::config::{AppConfig, ProviderConfig, VirtualKeyConfig};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use url::Url;

const MINIMAX_NATIVE_MODELS: &[&str] = &[
    "MiniMax-M2.7",
    "MiniMax-M2.7-highspeed",
    "MiniMax-M2.5",
    "MiniMax-M2.5-highspeed",
    "MiniMax-M2.1",
    "MiniMax-M2.1-highspeed",
    "MiniMax-M2",
    "M2-her",
    "speech-2.8-hd",
    "speech-2.8-turbo",
    "speech-2.6-hd",
    "speech-2.6-turbo",
    "speech-02-hd",
    "speech-02-turbo",
    "speech-01-hd",
    "speech-01-turbo",
    "MiniMax-Hailuo-2.3",
    "MiniMax-Hailuo-2.3-Fast",
    "MiniMax-Hailuo-02",
    "T2V-01",
    "I2V-01",
    "I2V-01-live",
    "I2V-01-Director",
    "T2V-01-Director",
    "I2V-01-Subject",
    "S2V-01",
    "image-01",
    "image-01-live",
    "music-2.6",
    "music-cover",
    "music-2.6-free",
    "music-cover-free",
];

#[derive(Debug, thiserror::Error)]
pub enum ProxyHelperError {
    #[error("invalid upstream base URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
}

#[derive(Debug, Clone)]
pub struct ResolvedProvider {
    pub name: String,
    pub provider_type: String,
    pub base_url: String,
    pub api_key: String,
    pub user_agent: Option<String>,
    pub model_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageTokens {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cached_write_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct KeyCheck {
    pub allowed: bool,
    pub is_master: bool,
    pub virtual_key: Option<VirtualKeyConfig>,
    pub reason: Option<String>,
}

pub fn build_upstream_url(
    base_url: &str,
    target_path: &str,
    query: &str,
) -> Result<String, ProxyHelperError> {
    let mut base = Url::parse(base_url.trim_end_matches('/'))?;
    let base_path = base.path().trim_end_matches('/').to_string();
    let mut target = target_path
        .split('?')
        .next()
        .unwrap_or(target_path)
        .to_string();
    if !target.starts_with('/') {
        target.insert(0, '/');
    }
    if is_versioned_base_path(&base_path) {
        target = target
            .strip_prefix("/v1")
            .filter(|s| !s.is_empty())
            .unwrap_or(target.as_str())
            .to_string();
        if !target.starts_with('/') {
            target.insert(0, '/');
        }
    }
    let joined = format!("{base_path}{target}");
    base.set_path(&joined.replace("//", "/"));
    if !query.is_empty() {
        base.set_query(Some(query.trim_start_matches('?')));
    }
    Ok(base.to_string())
}

fn is_versioned_base_path(path: &str) -> bool {
    path == "/v1" || path.ends_with("/v1")
}

pub fn get_request_model(body: &Value, target_path: &str) -> Option<String> {
    if let Some(model) = body.get("model").and_then(Value::as_str) {
        if !model.trim().is_empty() {
            return Some(model.to_string());
        }
    }
    let path = target_path.split('?').next().unwrap_or(target_path);
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    match parts.as_slice() {
        ["v1", "models", model] | ["models", model] => Some((*model).to_string()),
        _ => None,
    }
}

pub fn is_models_endpoint(target_path: &str) -> bool {
    let path = target_path.split('?').next().unwrap_or(target_path);
    matches!(path, "/v1/models" | "/models")
        || path.starts_with("/v1/models/")
        || path.starts_with("/models/")
}

pub fn resolve_provider(model: &str, cfg: &AppConfig) -> Option<ResolvedProvider> {
    for provider in &cfg.providers {
        if let Some(found) = provider.models.iter().find(|m| m.model_name == model) {
            return Some(to_resolved(
                provider,
                if found.model_id.is_empty() {
                    &found.model_name
                } else {
                    &found.model_id
                },
            ));
        }
    }
    if MINIMAX_NATIVE_MODELS.contains(&model) {
        if let Some(provider) = cfg.providers.iter().find(|provider| {
            let base = provider
                .base_url
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase();
            let name = provider.name.to_ascii_lowercase();
            base.contains("minimaxi.com") || name.contains("minimax")
        }) {
            return Some(to_resolved(provider, model));
        }
    }
    None
}

fn to_resolved(provider: &ProviderConfig, model_id: &str) -> ResolvedProvider {
    ResolvedProvider {
        name: provider.name.clone(),
        provider_type: provider.provider_type.clone(),
        base_url: provider.base_url.clone().unwrap_or_default(),
        api_key: provider.api_key.clone(),
        user_agent: provider.user_agent.clone(),
        model_id: model_id.to_string(),
    }
}

pub fn build_models_response(cfg: &AppConfig, allowed_models: Option<&[String]>) -> Value {
    let data: Vec<Value> = cfg
        .providers
        .iter()
        .flat_map(|provider| {
            provider.models.iter().filter_map(move |model| {
                if allowed_models
                    .map(|allowed| !allowed.iter().any(|m| m == &model.model_name))
                    .unwrap_or(false)
                {
                    return None;
                }
                Some(json!({
                    "id": model.model_name,
                    "object": "model",
                    "created": 0,
                    "owned_by": provider.name
                }))
            })
        })
        .collect();
    json!({ "object": "list", "data": data })
}

pub fn estimate_tokens(body: &Value) -> u64 {
    fn collect(value: &Value, out: &mut String) {
        match value {
            Value::String(s) => {
                out.push_str(s);
                out.push(' ');
            }
            Value::Array(items) => {
                for item in items {
                    collect(item, out);
                }
            }
            Value::Object(map) => {
                for value in map.values() {
                    collect(value, out);
                }
            }
            _ => {}
        }
    }
    let mut text = String::new();
    collect(body, &mut text);
    ((text.chars().count() as f64) / 4.0).ceil().max(1.0) as u64
}

pub fn extract_usage_tokens(data: &Value, fallback_input: u64) -> UsageTokens {
    let Some(usage) = data.get("usage") else {
        return UsageTokens {
            input_tokens: fallback_input,
            ..UsageTokens::default()
        };
    };
    let prompt = first_u64(&[
        usage.pointer("/prompt_tokens"),
        usage.pointer("/input_tokens"),
        usage.pointer("/total_tokens"),
    ])
    .unwrap_or(fallback_input);
    let completion = first_u64(&[
        usage.pointer("/completion_tokens"),
        usage.pointer("/output_tokens"),
        usage.pointer("/candidates_token_count"),
    ])
    .unwrap_or(0);
    let cached = first_u64(&[
        usage.pointer("/prompt_tokens_details/cached_tokens"),
        usage.pointer("/input_tokens_details/cached_tokens"),
        usage.pointer("/cached_tokens"),
        usage.pointer("/prompt_cache_hit_tokens"),
        usage.pointer("/cache_read_input_tokens"),
    ])
    .unwrap_or(0);
    let cache_write = first_u64(&[
        usage.pointer("/prompt_tokens_details/cache_write_tokens"),
        usage.pointer("/input_tokens_details/cache_write_tokens"),
        usage.pointer("/cache_creation_input_tokens"),
        usage.pointer("/prompt_cache_miss_tokens"),
    ])
    .unwrap_or(0);
    let input = if usage.get("prompt_cache_hit_tokens").is_some()
        || usage.get("prompt_cache_miss_tokens").is_some()
    {
        usage
            .get("prompt_cache_hit_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + usage
                .get("prompt_cache_miss_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0)
    } else {
        prompt
    };
    UsageTokens {
        input_tokens: input,
        output_tokens: completion,
        cached_tokens: cached,
        cached_write_tokens: cache_write,
    }
}

fn first_u64(values: &[Option<&Value>]) -> Option<u64> {
    values.iter().find_map(|v| v.and_then(Value::as_u64))
}

pub fn calc_cost(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    cached_write_tokens: u64,
    pricing: &Value,
) -> f64 {
    let p = pricing.get(model).unwrap_or(&Value::Null);
    let input_rate = p.get("input").and_then(Value::as_f64).unwrap_or(0.0);
    let cached_rate = p
        .get("cached_input")
        .and_then(Value::as_f64)
        .unwrap_or(input_rate);
    let cached_write_rate = p
        .get("cached_write")
        .and_then(Value::as_f64)
        .unwrap_or(input_rate);
    let output_rate = p.get("output").and_then(Value::as_f64).unwrap_or(0.0);
    let normal_input = input_tokens.saturating_sub(cached_tokens + cached_write_tokens);
    (normal_input as f64 / 1_000_000.0) * input_rate
        + (cached_tokens as f64 / 1_000_000.0) * cached_rate
        + (cached_write_tokens as f64 / 1_000_000.0) * cached_write_rate
        + (output_tokens as f64 / 1_000_000.0) * output_rate
}

pub fn check_virtual_key(cfg: &AppConfig, auth_key: &str, model: Option<&str>) -> KeyCheck {
    if auth_key.is_empty() {
        return denied("缺少 API Key");
    }
    if cfg.general_settings.master_key.as_deref() == Some(auth_key) {
        return KeyCheck {
            allowed: true,
            is_master: true,
            virtual_key: None,
            reason: None,
        };
    }
    let Some(key) = cfg
        .virtual_keys
        .iter()
        .find(|k| k.key == auth_key && k.enabled.unwrap_or(true))
    else {
        return denied("无效的 API Key");
    };
    if let Some(expires) = &key.expires_at {
        if let Ok(expires_at) = DateTime::parse_from_rfc3339(expires) {
            if expires_at.with_timezone(&Utc) < Utc::now() {
                return denied(format!("密钥 \"{}\" 已过期", key.name));
            }
        }
    }
    if let (Some(model), Some(allowed)) = (model, key.allowed_models.as_ref()) {
        if !allowed.iter().any(|m| m == model) {
            return denied(format!("密钥 \"{}\" 无权访问模型 {}", key.name, model));
        }
    }
    KeyCheck {
        allowed: true,
        is_master: false,
        virtual_key: Some(key.clone()),
        reason: None,
    }
}

fn denied(reason: impl Into<String>) -> KeyCheck {
    KeyCheck {
        allowed: false,
        is_master: false,
        virtual_key: None,
        reason: Some(reason.into()),
    }
}

pub fn rewrite_json_model(mut body: Value, upstream_model: &str) -> Vec<u8> {
    if body.get("model").is_some() {
        body["model"] = json!(upstream_model);
    }
    serde_json::to_vec(&body).unwrap_or_default()
}
