use crate::config::{AppConfig, ProviderConfig};
use crate::proxy::{build_gemini_upstream_url, build_upstream_url};
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderHealth {
    pub provider_id: String,
    pub name: String,
    pub status: &'static str,
    pub code: &'static str,
    pub message: String,
    pub latency_ms: Option<u64>,
    pub http_status: Option<u16>,
    pub model_count: Option<usize>,
}

pub async fn check_all_providers(client: &reqwest::Client, cfg: &AppConfig) -> Vec<ProviderHealth> {
    let mut checks = Vec::new();
    for provider in &cfg.providers {
        checks.push(check_provider(client, provider).await);
    }
    checks
}

pub async fn check_provider(client: &reqwest::Client, provider: &ProviderConfig) -> ProviderHealth {
    if provider.api_key.trim().is_empty() {
        return health(
            provider,
            "error",
            "missing_api_key",
            "缺少 API Key",
            None,
            None,
            None,
        );
    }
    let api_key = resolve_api_key(&provider.api_key);
    if provider.api_key.starts_with("os.environ/") && api_key == provider.api_key {
        return health(
            provider,
            "warning",
            "unresolved_api_key_env",
            "API Key 环境变量未设置",
            None,
            None,
            None,
        );
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
        return health(
            provider,
            "error",
            "invalid_base_url",
            "Base URL 无法拼接模型列表端点",
            None,
            None,
            None,
        );
    };

    let started = Instant::now();
    let mut request = client.get(url).timeout(Duration::from_secs(4)).header(
        header::USER_AGENT,
        provider
            .user_agent
            .clone()
            .unwrap_or_else(|| "OpenRelay-Gateway/1.0".to_string()),
    );
    request = if is_gemini {
        request.header("x-goog-api-key", api_key)
    } else {
        request.header(header::AUTHORIZATION, format!("Bearer {api_key}"))
    };

    match request.send().await {
        Ok(response) => {
            let latency_ms = started.elapsed().as_millis().max(1) as u64;
            let status = response.status();
            let http_status = Some(status.as_u16());
            let text = response.text().await.unwrap_or_default();
            if status.is_success() {
                let model_count = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|value| model_count_from_value(&value, is_gemini));
                return health(
                    provider,
                    "ok",
                    "available",
                    "服务商可用",
                    Some(latency_ms),
                    http_status,
                    model_count,
                );
            }
            let code = match status.as_u16() {
                401 | 403 => "auth_failed",
                402 => "quota_or_balance",
                429 => "rate_limited",
                _ => {
                    if text.to_ascii_lowercase().contains("quota")
                        || text.to_ascii_lowercase().contains("balance")
                    {
                        "quota_or_balance"
                    } else {
                        "upstream_error"
                    }
                }
            };
            health(
                provider,
                "error",
                code,
                format!("模型列表检查返回 HTTP {}", status.as_u16()),
                Some(latency_ms),
                http_status,
                None,
            )
        }
        Err(err) => health(
            provider,
            "error",
            if err.is_timeout() {
                "timeout"
            } else {
                "network_error"
            },
            format!("连接失败: {err}"),
            Some(started.elapsed().as_millis().max(1) as u64),
            None,
            None,
        ),
    }
}

fn health(
    provider: &ProviderConfig,
    status: &'static str,
    code: &'static str,
    message: impl Into<String>,
    latency_ms: Option<u64>,
    http_status: Option<u16>,
    model_count: Option<usize>,
) -> ProviderHealth {
    ProviderHealth {
        provider_id: provider.id.clone(),
        name: provider_display_name(provider),
        status,
        code,
        message: message.into(),
        latency_ms,
        http_status,
        model_count,
    }
}

fn model_count_from_value(value: &Value, is_gemini: bool) -> Option<usize> {
    if is_gemini {
        value.get("models").and_then(Value::as_array).map(Vec::len)
    } else {
        value.get("data").and_then(Value::as_array).map(Vec::len)
    }
}

fn provider_display_name(provider: &ProviderConfig) -> String {
    if provider.name.trim().is_empty() {
        provider.id.clone()
    } else {
        provider.name.clone()
    }
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

fn resolve_api_key(api_key: &str) -> String {
    api_key
        .strip_prefix("os.environ/")
        .and_then(|name| std::env::var(name).ok())
        .unwrap_or_else(|| api_key.to_string())
}
