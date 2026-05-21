use bcrypt::{hash, DEFAULT_COST};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_ADMIN_PASSWORD: &str = "admin123";
const DEFAULT_MASTER_KEY: &str = "openrelay-master";
const DEFAULT_USER_AGENT: &str = "OpenRelay-Gateway/1.0";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("bcrypt error: {0}")]
    Bcrypt(#[from] bcrypt::BcryptError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminConfig {
    pub username: String,
    pub password_hash: String,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            username: "admin".to_string(),
            password_hash: hash(DEFAULT_ADMIN_PASSWORD, DEFAULT_COST).unwrap_or_else(|_| {
                "$2a$10$KmfU1Kfs3BFCZ69V3FgJ6uxrzRy1sZmZMwdSlM8wTZNp0zsJLUPZ.".to_string()
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelConfig {
    pub model_name: String,
    #[serde(default)]
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type", default = "default_provider_type")]
    pub provider_type: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub models: Vec<ModelConfig>,
}

fn default_provider_type() -> String {
    "openai".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VirtualKeyConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    #[serde(default)]
    pub budget: Option<f64>,
    #[serde(default)]
    pub rpm: Option<u32>,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralSettings {
    #[serde(default)]
    pub master_key: Option<String>,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            master_key: Some(DEFAULT_MASTER_KEY.to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConversationStorage {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub directory: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub admin: AdminConfig,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub pricing: Value,
    #[serde(default)]
    pub limits: Value,
    #[serde(default)]
    pub virtual_keys: Vec<VirtualKeyConfig>,
    #[serde(default)]
    pub router_settings: Value,
    #[serde(default)]
    pub litellm_settings: Value,
    #[serde(default)]
    pub general_settings: GeneralSettings,
    #[serde(default)]
    pub conversation_storage: ConversationStorage,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            admin: AdminConfig::default(),
            providers: Vec::new(),
            pricing: json!({}),
            limits: json!({}),
            virtual_keys: Vec::new(),
            router_settings: json!({ "timeout": 60 }),
            litellm_settings: json!({ "drop_params": true, "allowed_headers": ["*"] }),
            general_settings: GeneralSettings::default(),
            conversation_storage: ConversationStorage::default(),
        }
    }
}

pub fn config_path(root: &Path) -> PathBuf {
    root.join("config.json")
}

pub fn yaml_path(root: &Path) -> PathBuf {
    root.join("openrelay-config.yaml")
}

pub fn ensure_files(root: &Path) -> Result<(), ConfigError> {
    if !config_path(root).exists() {
        save_config(root, &AppConfig::default())?;
        return Ok(());
    }
    let mut cfg = load_config(root)?;
    let mut changed = false;
    if cfg.general_settings.master_key.is_none() {
        cfg.general_settings.master_key = Some(DEFAULT_MASTER_KEY.to_string());
        changed = true;
    }
    if changed || !yaml_path(root).exists() {
        save_config(root, &cfg)?;
    }
    Ok(())
}

pub fn load_config(root: &Path) -> Result<AppConfig, ConfigError> {
    let raw = fs::read_to_string(config_path(root))?;
    let mut cfg: AppConfig = serde_json::from_str(&raw)?;
    if cfg.general_settings.master_key.is_none() {
        cfg.general_settings.master_key = Some(DEFAULT_MASTER_KEY.to_string());
    }
    Ok(cfg)
}

pub fn save_config(root: &Path, cfg: &AppConfig) -> Result<(), ConfigError> {
    fs::write(config_path(root), serde_json::to_string_pretty(cfg)?)?;
    fs::write(
        yaml_path(root),
        serde_yaml::to_string(&flatten_for_yaml(cfg))?,
    )?;
    Ok(())
}

pub fn flatten_for_yaml(cfg: &AppConfig) -> Value {
    let model_list: Vec<Value> = cfg
        .providers
        .iter()
        .flat_map(|provider| {
            provider.models.iter().map(move |model| {
                let prefix = match provider.provider_type.as_str() {
                    "gemini" => "gemini",
                    "anthropic" => "anthropic",
                    _ => "openai",
                };
                let model_id = if model.model_id.is_empty() {
                    model.model_name.as_str()
                } else {
                    model.model_id.as_str()
                };
                let mut params = json!({
                    "model": format!("{prefix}/{model_id}"),
                    "api_key": provider.api_key,
                    "extra_headers": {
                        "User-Agent": provider.user_agent.as_deref().unwrap_or(DEFAULT_USER_AGENT)
                    }
                });
                if let Some(base_url) = &provider.base_url {
                    params["api_base"] = json!(base_url);
                }
                json!({
                    "model_name": model.model_name,
                    "litellm_params": params
                })
            })
        })
        .collect();

    json!({
        "router_settings": cfg.router_settings,
        "litellm_settings": cfg.litellm_settings,
        "general_settings": cfg.general_settings,
        "model_list": model_list
    })
}
