use crate::config::{load_config, save_config, AppConfig, ConfigError};
use crate::secrets::redact_secret;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("config error: {0}")]
    Config(#[from] ConfigError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("backup not found")]
    NotFound,
    #[error("invalid backup id")]
    InvalidId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackupEntry {
    pub id: String,
    pub created_at: String,
    pub reason: String,
    pub provider_count: usize,
    pub model_count: usize,
    pub virtual_key_count: usize,
    pub size_bytes: u64,
}

pub fn backups_dir(root: &Path) -> PathBuf {
    root.join("backups")
}

pub fn create_config_backup(
    root: &Path,
    cfg: &AppConfig,
    reason: &str,
) -> Result<BackupEntry, BackupError> {
    fs::create_dir_all(backups_dir(root))?;
    let created_at = Utc::now().to_rfc3339();
    let id = format!(
        "{}-{}",
        Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
        Uuid::new_v4().simple()
    );
    let payload = serde_json::to_vec_pretty(cfg)?;
    let path = backup_path(root, &id)?;
    fs::write(&path, payload)?;
    let size_bytes = fs::metadata(&path)?.len();
    Ok(entry_from_config(
        id,
        created_at,
        reason.trim().if_empty("manual").to_string(),
        cfg,
        size_bytes,
    ))
}

pub fn list_config_backups(root: &Path) -> Result<Vec<BackupEntry>, BackupError> {
    let dir = backups_dir(root);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        if !valid_backup_id(id) {
            continue;
        }
        let cfg: AppConfig = serde_json::from_str(&fs::read_to_string(&path)?)?;
        let created_at = backup_created_at(id);
        let size_bytes = entry.metadata()?.len();
        entries.push(entry_from_config(
            id.to_string(),
            created_at,
            "snapshot".to_string(),
            &cfg,
            size_bytes,
        ));
    }
    entries.sort_by(|left, right| right.id.cmp(&left.id));
    Ok(entries)
}

pub fn restore_config_backup(root: &Path, id: &str) -> Result<AppConfig, BackupError> {
    let path = backup_path(root, id)?;
    if !path.exists() {
        return Err(BackupError::NotFound);
    }
    let cfg: AppConfig = serde_json::from_str(&fs::read_to_string(path)?)?;
    save_config(root, &cfg)?;
    load_config(root).map_err(BackupError::from)
}

pub fn export_current_config(cfg: &AppConfig, redacted: bool) -> Result<Value, BackupError> {
    let mut value = serde_json::to_value(cfg)?;
    if redacted {
        redact_config_value(&mut value);
    }
    Ok(value)
}

pub fn load_backup_config(root: &Path, id: &str) -> Result<AppConfig, BackupError> {
    let path = backup_path(root, id)?;
    if !path.exists() {
        return Err(BackupError::NotFound);
    }
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

fn backup_path(root: &Path, id: &str) -> Result<PathBuf, BackupError> {
    if !valid_backup_id(id) {
        return Err(BackupError::InvalidId);
    }
    Ok(backups_dir(root).join(format!("{id}.json")))
}

fn valid_backup_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | 'T' | 'Z'))
}

fn backup_created_at(id: &str) -> String {
    id.split_once('-')
        .map(|(timestamp, _)| timestamp.to_string())
        .unwrap_or_else(|| id.to_string())
}

fn entry_from_config(
    id: String,
    created_at: String,
    reason: String,
    cfg: &AppConfig,
    size_bytes: u64,
) -> BackupEntry {
    BackupEntry {
        id,
        created_at,
        reason,
        provider_count: cfg.providers.len(),
        model_count: cfg
            .providers
            .iter()
            .map(|provider| provider.models.len())
            .sum(),
        virtual_key_count: cfg.virtual_keys.len(),
        size_bytes,
    }
}

fn redact_config_value(value: &mut Value) {
    if let Some(providers) = value.get_mut("providers").and_then(Value::as_array_mut) {
        for provider in providers {
            if let Some(raw) = provider.get("api_key").and_then(Value::as_str) {
                provider["api_key"] = Value::String(redact_secret(raw));
            }
        }
    }
    if let Some(keys) = value.get_mut("virtual_keys").and_then(Value::as_array_mut) {
        for key in keys {
            if let Some(raw) = key.get("key").and_then(Value::as_str) {
                key["key"] = Value::String(redact_secret(raw));
            }
        }
    }
    if let Some(master_key) = value
        .get("general_settings")
        .and_then(|settings| settings.get("master_key"))
        .and_then(Value::as_str)
        .map(redact_secret)
    {
        value["general_settings"]["master_key"] = Value::String(master_key);
    }
}

trait IfEmpty {
    fn if_empty<'a>(&'a self, fallback: &'a str) -> &'a str;
}

impl IfEmpty for str {
    fn if_empty<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.is_empty() {
            fallback
        } else {
            self
        }
    }
}
