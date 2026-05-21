use openrelay::config::{
    ensure_files, load_config, save_config, AppConfig, ModelConfig, ProviderConfig,
};

#[test]
fn ensure_files_creates_default_config_and_yaml() {
    let dir = tempfile::tempdir().unwrap();
    ensure_files(dir.path()).unwrap();

    assert!(dir.path().join("config.json").exists());
    assert!(dir.path().join("openrelay-config.yaml").exists());

    let cfg = load_config(dir.path()).unwrap();
    assert_eq!(cfg.admin.username, "admin");
    assert_eq!(
        cfg.general_settings.master_key.as_deref(),
        Some("openrelay-master")
    );
    assert!(!cfg.conversation_storage.enabled);
    assert!(cfg.conversation_storage.directory.is_empty());
}

#[test]
fn save_config_writes_provider_model_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.providers.push(ProviderConfig {
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
    });

    save_config(dir.path(), &cfg).unwrap();
    let yaml = std::fs::read_to_string(dir.path().join("openrelay-config.yaml")).unwrap();
    assert!(yaml.contains("model_name: local-gpt"));
    assert!(yaml.contains("model: openai/gpt-4o-mini"));
}
