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
