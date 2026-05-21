use std::path::Path;

#[test]
fn repository_keeps_rust_runtime_layout_only() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    assert!(root.join("Cargo.toml").exists());
    assert!(root.join("src").join("main.rs").exists());
    assert!(root.join("public").join("index.html").exists());
    assert!(root.join("public").join("login.html").exists());
    assert!(root.join("assets").join("openrelay.ico").exists());

    assert!(!root.join("web").join("server.js").exists());
    assert!(!root.join("web").join("package.json").exists());
    assert!(!root.join("rust-backend").join("Cargo.toml").exists());
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
