use openrelay::config::{
    ensure_files, load_config, save_config, AppConfig, ModelConfig, ProviderConfig,
};
use openrelay::{
    backups::{
        create_config_backup, export_current_config, list_config_backups, restore_config_backup,
    },
    secrets::{audit_security, is_protected_secret, protect_config_secrets, secrets_supported},
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

#[test]
fn config_backups_create_redact_and_restore_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.providers.push(ProviderConfig {
        id: "p1".to_string(),
        name: "OpenAI".to_string(),
        provider_type: "openai".to_string(),
        base_url: Some("https://api.example.com/v1".to_string()),
        api_key: "provider-secret".to_string(),
        user_agent: None,
        models: vec![ModelConfig {
            model_name: "local-gpt".to_string(),
            model_id: "gpt-4o-mini".to_string(),
        }],
    });
    save_config(dir.path(), &cfg).unwrap();

    let entry = create_config_backup(dir.path(), &cfg, "manual").unwrap();
    assert_eq!(entry.reason, "manual");
    assert_eq!(entry.provider_count, 1);

    let backups = list_config_backups(dir.path()).unwrap();
    assert_eq!(backups.len(), 1);
    assert_eq!(backups[0].id, entry.id);

    let redacted = export_current_config(&cfg, true).unwrap();
    assert_eq!(redacted["providers"][0]["api_key"], "prov...cret");

    let mut changed = AppConfig::default();
    changed.providers.clear();
    save_config(dir.path(), &changed).unwrap();
    let restored = restore_config_backup(dir.path(), &entry.id).unwrap();
    assert_eq!(restored.providers[0].api_key, "provider-secret");
}

#[test]
fn security_audit_flags_default_settings_and_plaintext_provider_keys() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.providers.push(ProviderConfig {
        id: "p1".to_string(),
        name: "OpenAI".to_string(),
        provider_type: "openai".to_string(),
        base_url: Some("https://api.example.com/v1".to_string()),
        api_key: "provider-secret".to_string(),
        user_agent: None,
        models: Vec::new(),
    });

    let findings = audit_security(dir.path(), &cfg, false).unwrap();
    let codes: Vec<&str> = findings
        .iter()
        .map(|finding| finding.code.as_str())
        .collect();
    assert!(codes.contains(&"default_admin_password"));
    assert!(codes.contains(&"default_master_key"));
    assert!(codes.contains(&"missing_jwt_secret"));
    assert!(codes.contains(&"plaintext_provider_key"));
}

#[test]
fn protect_config_secrets_encrypts_keys_when_supported_or_reports_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.providers.push(ProviderConfig {
        id: "p1".to_string(),
        name: "OpenAI".to_string(),
        provider_type: "openai".to_string(),
        base_url: Some("https://api.example.com/v1".to_string()),
        api_key: "provider-secret".to_string(),
        user_agent: None,
        models: Vec::new(),
    });
    save_config(dir.path(), &cfg).unwrap();

    let result = protect_config_secrets(dir.path(), &cfg).unwrap();

    if secrets_supported() {
        assert_eq!(result.protected_count, 1);
        let raw = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(!raw.contains("provider-secret"));
        let stored: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(is_protected_secret(
            stored["providers"][0]["api_key"].as_str().unwrap()
        ));
        let loaded = load_config(dir.path()).unwrap();
        assert_eq!(loaded.providers[0].api_key, "provider-secret");
    } else {
        assert_eq!(result.protected_count, 0);
        assert!(!result.supported);
    }
}
