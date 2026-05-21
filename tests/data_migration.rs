use openrelay::config::{load_config, save_config, AppConfig, GeneralSettings};
use openrelay::database::Database;
use openrelay::migration::migrate_legacy_data;
use openrelay::paths::default_data_root_for_home;
use serde_json::json;
use std::path::{Path, PathBuf};

#[test]
fn default_data_root_lives_under_user_home() {
    assert_eq!(
        default_data_root_for_home(Path::new(r"C:\Users\openrelay")),
        PathBuf::from(r"C:\Users\openrelay").join(".openrelay")
    );
}

#[test]
fn migration_imports_legacy_config_usage_and_database_without_conversations() {
    let legacy = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();

    let mut cfg = AppConfig::default();
    cfg.general_settings = GeneralSettings {
        master_key: Some("legacy-master".to_string()),
    };
    save_config(legacy.path(), &cfg).unwrap();
    std::fs::write(
        legacy.path().join("usage.jsonl"),
        format!(
            "{}\n",
            json!({
                "timestamp": 1778949353707i64,
                "request_id": "archive:legacy",
                "model": "legacy-model",
                "key_name": "old-key",
                "input_tokens": 11,
                "cached_tokens": 2,
                "cached_write_tokens": 3,
                "output_tokens": 17,
                "cost": 0.25,
                "duration_ms": 99,
                "status": 200,
                "stream": true,
                "user_agent": "old-node",
                "error": "",
                "path": "/v1/chat/completions"
            })
        ),
    )
    .unwrap();
    std::fs::create_dir_all(legacy.path().join("conversations")).unwrap();
    std::fs::write(
        legacy
            .path()
            .join("conversations")
            .join("keep-user-managed.json"),
        "{}",
    )
    .unwrap();

    migrate_legacy_data(data.path(), Some(legacy.path())).unwrap();

    let migrated_cfg = load_config(data.path()).unwrap();
    assert_eq!(
        migrated_cfg.general_settings.master_key.as_deref(),
        Some("legacy-master")
    );
    assert!(data.path().join("openrelay-config.yaml").exists());
    assert!(data.path().join("openrelay.db").exists());
    assert!(!data.path().join("conversations").exists());

    let db = Database::open(data.path()).unwrap();
    let page = db.usage_page(1, 20).unwrap();
    assert_eq!(page.pagination.total, 1);
    assert_eq!(page.entries[0].request_id, "archive:legacy");
    assert_eq!(page.entries[0].model, "legacy-model");
    assert_eq!(page.entries[0].input_tokens, 11);
}

#[test]
fn migration_keeps_existing_data_config() {
    let legacy = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();

    let mut legacy_cfg = AppConfig::default();
    legacy_cfg.general_settings.master_key = Some("legacy-master".to_string());
    save_config(legacy.path(), &legacy_cfg).unwrap();

    let mut data_cfg = AppConfig::default();
    data_cfg.general_settings.master_key = Some("current-master".to_string());
    save_config(data.path(), &data_cfg).unwrap();

    migrate_legacy_data(data.path(), Some(legacy.path())).unwrap();

    let cfg = load_config(data.path()).unwrap();
    assert_eq!(
        cfg.general_settings.master_key.as_deref(),
        Some("current-master")
    );
}
