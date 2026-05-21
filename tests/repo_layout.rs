use std::path::Path;

#[test]
fn repository_keeps_rust_runtime_layout_only() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    assert!(root.join("Cargo.toml").exists());
    assert!(root.join("src").join("main.rs").exists());
    assert!(root.join("public").join("index.html").exists());
    assert!(root.join("public").join("login.html").exists());
    assert!(root.join("assets").join("openrelay.ico").exists());
    assert!(root.join("build.bat").exists());

    assert!(!root.join("web").join("server.js").exists());
    assert!(!root.join("web").join("package.json").exists());
    assert!(!root.join("rust-backend").join("Cargo.toml").exists());
    assert!(!root.join("start.bat").exists());
    assert!(!root.join("start-rust.bat").exists());
    assert!(!root.join("start-tray.vbs").exists());
}

#[test]
fn frontend_static_assets_do_not_depend_on_node_modules() {
    let index = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("public")
            .join("index.html"),
    )
    .unwrap();

    assert!(!index.contains("/vendor/js-yaml"));
    assert!(!index.contains("cdnjs.cloudflare.com"));
}

#[test]
fn frontend_shows_protocol_specific_proxy_endpoints_and_stable_model_picker() {
    let index = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("public")
            .join("index.html"),
    )
    .unwrap();

    assert!(index.contains("OpenAI 兼容接口"));
    assert!(index.contains("Gemini 原生接口"));
    assert!(index.contains("/gemini/v1beta/models/{model}:generateContent"));
    assert!(index.contains("/gemini/v1beta/models/{model}:streamGenerateContent"));
    assert!(index.contains("model-test-result"));
    assert!(index.contains("fetched-model-list"));
    assert!(index.contains("fetched-model-row"));
    assert!(index.contains("overflow-wrap:anywhere"));
}

#[test]
fn rust_proxy_hot_path_uses_streaming_cache_async_db_and_tokenizer() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let server = std::fs::read_to_string(root.join("src").join("server.rs")).unwrap();
    let database = std::fs::read_to_string(root.join("src").join("database.rs")).unwrap();
    let proxy = std::fs::read_to_string(root.join("src").join("proxy.rs")).unwrap();
    let cargo = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();

    assert!(server.contains("RwLock<AppConfig>"));
    assert!(server.contains("current_config().await"));
    assert!(server.contains("Body::from_stream"));
    assert!(server.contains("bytes_stream()"));
    assert!(server.contains("record_usage_async"));
    assert!(server.contains("request_count_since_async"));
    assert!(server.contains("total_cost_async"));
    assert!(database.contains("spawn_blocking"));
    assert!(cargo.contains("tiktoken-rs"));
    assert!(!proxy.contains("/ 4.0"));
}
