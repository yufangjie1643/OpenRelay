use openrelay::config::{AppConfig, ModelConfig, ProviderConfig, VirtualKeyConfig};
use openrelay::proxy::{
    build_gemini_upstream_url, build_models_response, build_upstream_url, calc_cost,
    estimate_tokens, extract_usage_tokens, get_request_model, resolve_provider,
};
use serde_json::json;

fn sample_config() -> AppConfig {
    AppConfig {
        providers: vec![
            ProviderConfig {
                id: "p1".to_string(),
                name: "OpenAI".to_string(),
                provider_type: "openai".to_string(),
                base_url: Some("https://api.example.com/v1".to_string()),
                api_key: "provider-key".to_string(),
                user_agent: Some("OpenRelay-Gateway/1.0".to_string()),
                models: vec![ModelConfig {
                    model_name: "local-gpt".to_string(),
                    model_id: "gpt-4o-mini".to_string(),
                }],
            },
            ProviderConfig {
                id: "p2".to_string(),
                name: "MiniMax".to_string(),
                provider_type: "openai".to_string(),
                base_url: Some("https://api.minimaxi.com/v1".to_string()),
                api_key: "minimax-key".to_string(),
                user_agent: None,
                models: vec![],
            },
        ],
        virtual_keys: vec![VirtualKeyConfig {
            name: "dev".to_string(),
            key: "vk-dev".to_string(),
            enabled: Some(true),
            allowed_models: Some(vec!["local-gpt".to_string()]),
            budget: None,
            rpm: None,
            expires_at: None,
        }],
        ..AppConfig::default()
    }
}

#[test]
fn upstream_url_preserves_versioned_base_and_query() {
    assert_eq!(
        build_upstream_url(
            "https://api.example.com/v1",
            "/v1/chat/completions",
            "?trace=1"
        )
        .unwrap(),
        "https://api.example.com/v1/chat/completions?trace=1"
    );
    assert_eq!(
        build_upstream_url("https://api.example.com/openai/v1/", "/responses", "").unwrap(),
        "https://api.example.com/openai/v1/responses"
    );
}

#[test]
fn request_model_reads_body_and_model_path() {
    assert_eq!(
        get_request_model(&json!({"model": "local-gpt"}), "/v1/chat/completions"),
        Some("local-gpt".to_string())
    );
    assert_eq!(
        get_request_model(&json!({}), "/v1/models/local-gpt"),
        Some("local-gpt".to_string())
    );
    assert_eq!(get_request_model(&json!({}), "/v1/models"), None);
}

#[test]
fn gemini_model_is_read_from_native_model_path() {
    assert_eq!(
        get_request_model(
            &json!({"contents": [{"parts": [{"text": "hello"}]}]}),
            "/v1beta/models/gemini-local:generateContent"
        ),
        Some("gemini-local".to_string())
    );
    assert_eq!(
        get_request_model(
            &json!({}),
            "/v1beta/models/gemini-local:streamGenerateContent"
        ),
        Some("gemini-local".to_string())
    );
}

#[test]
fn gemini_upstream_url_rewrites_model_and_removes_client_key() {
    let url = build_gemini_upstream_url(
        "https://generativelanguage.googleapis.com/v1beta",
        "/v1beta/models/gemini-local:streamGenerateContent",
        "?key=client-key&alt=sse",
        "gemini-1.5-pro",
    )
    .unwrap();

    assert_eq!(
        url,
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-1.5-pro:streamGenerateContent?alt=sse"
    );
}

#[test]
fn resolves_configured_and_minimax_native_models() {
    let cfg = sample_config();
    let local = resolve_provider("local-gpt", &cfg).unwrap();
    assert_eq!(local.model_id, "gpt-4o-mini");
    assert_eq!(local.api_key, "provider-key");

    let minimax = resolve_provider("image-01", &cfg).unwrap();
    assert_eq!(minimax.model_id, "image-01");
    assert_eq!(minimax.api_key, "minimax-key");
}

#[test]
fn models_response_filters_by_virtual_key() {
    let cfg = sample_config();
    let response = build_models_response(&cfg, Some(&["local-gpt".to_string()]));
    assert_eq!(response["object"], "list");
    assert_eq!(response["data"][0]["id"], "local-gpt");
}

#[test]
fn extracts_openai_and_responses_usage() {
    let openai = extract_usage_tokens(
        &json!({"usage": {"prompt_tokens": 100, "completion_tokens": 7, "prompt_tokens_details": {"cached_tokens": 80, "cache_write_tokens": 5}}}),
        12,
    );
    assert_eq!(openai.input_tokens, 100);
    assert_eq!(openai.output_tokens, 7);
    assert_eq!(openai.cached_tokens, 80);
    assert_eq!(openai.cached_write_tokens, 5);

    let responses = extract_usage_tokens(
        &json!({"usage": {"input_tokens": 13, "output_tokens": 8, "input_tokens_details": {"cached_tokens": 3}}}),
        5,
    );
    assert_eq!(responses.input_tokens, 13);
    assert_eq!(responses.output_tokens, 8);
    assert_eq!(responses.cached_tokens, 3);
}

#[test]
fn extracts_deepseek_cache_hit_tokens_from_details() {
    let deepseek = extract_usage_tokens(
        &json!({
            "usage": {
                "prompt_tokens": 3_539_090,
                "completion_tokens": 19_312,
                "total_tokens": 3_558_402,
                "prompt_tokens_details": {
                    "cached_tokens": 0,
                    "prompt_cache_hit_tokens": 3_500_000,
                    "prompt_cache_miss_tokens": 39_090
                }
            }
        }),
        1,
    );

    assert_eq!(deepseek.input_tokens, 3_539_090);
    assert_eq!(deepseek.output_tokens, 19_312);
    assert_eq!(deepseek.cached_tokens, 3_500_000);
    assert_eq!(deepseek.cached_write_tokens, 39_090);

    let root_fields = extract_usage_tokens(
        &json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 8,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "prompt_cache_hit_tokens": 60,
                "prompt_cache_miss_tokens": 40
            }
        }),
        1,
    );

    assert_eq!(root_fields.input_tokens, 100);
    assert_eq!(root_fields.output_tokens, 8);
    assert_eq!(root_fields.cached_tokens, 60);
    assert_eq!(root_fields.cached_write_tokens, 40);
}

#[test]
fn extracts_gemini_usage_metadata() {
    let gemini = extract_usage_tokens(
        &json!({"usageMetadata": {"promptTokenCount": 23, "candidatesTokenCount": 11, "totalTokenCount": 34}}),
        5,
    );

    assert_eq!(gemini.input_tokens, 23);
    assert_eq!(gemini.output_tokens, 11);
    assert_eq!(gemini.cached_tokens, 0);
}

#[test]
fn estimates_cjk_prompt_tokens_with_model_tokenizer() {
    let tokens = estimate_tokens(&json!({
        "model": "gpt-4o",
        "messages": [{
            "role": "user",
            "content": "请帮我总结下面这段中文内容，并保持专业语气，同时列出三个关键结论。这里还包含一小段 Rust 代码：fn main() { println!(\"你好\"); }"
        }]
    }));

    assert!(
        tokens >= 40,
        "expected tokenizer-style estimate for CJK/code prompt, got {tokens}"
    );
}

#[test]
fn cost_uses_cache_read_and_write_rates() {
    let pricing = json!({
        "local-gpt": {"input": 10.0, "cached_input": 2.0, "cached_write": 4.0, "output": 20.0}
    });
    let cost = calc_cost("local-gpt", 100, 5, 30, 10, &pricing);
    assert!((cost - 0.0008).abs() < 1e-12);
}
