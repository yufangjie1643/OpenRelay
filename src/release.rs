use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    pub version: &'static str,
    pub data_root: String,
    pub static_root: String,
    pub startup: StartupStatus,
    pub secrets_supported: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupStatus {
    pub supported: bool,
    pub enabled: bool,
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StartupRequest {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current_version: &'static str,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub release_url: Option<String>,
    pub changelog: Option<String>,
    pub error: Option<String>,
}

pub fn app_status(data_root: &Path, static_root: &Path) -> AppStatus {
    AppStatus {
        version: env!("CARGO_PKG_VERSION"),
        data_root: data_root.to_string_lossy().to_string(),
        static_root: static_root.to_string_lossy().to_string(),
        startup: startup_status(),
        secrets_supported: crate::secrets::secrets_supported(),
    }
}

pub fn startup_status() -> StartupStatus {
    let path = startup_script_path();
    StartupStatus {
        supported: cfg!(windows) && path.is_some(),
        enabled: path.as_ref().is_some_and(|path| path.exists()),
        path: path.map(|path| path.to_string_lossy().to_string()),
    }
}

pub fn set_startup_enabled(enabled: bool) -> io::Result<StartupStatus> {
    let Some(path) = startup_script_path() else {
        return Ok(startup_status());
    };
    if enabled {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let exe = std::env::current_exe()?;
        fs::write(
            &path,
            format!(
                "@echo off\r\nstart \"OpenRelay\" \"{}\"\r\n",
                exe.to_string_lossy()
            ),
        )?;
    } else if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(startup_status())
}

pub async fn check_update(client: &reqwest::Client) -> UpdateInfo {
    let current = env!("CARGO_PKG_VERSION");
    let response = client
        .get("https://api.github.com/repos/yufangjie1643/OpenRelay/releases/latest")
        .header(reqwest::header::USER_AGENT, "OpenRelay-Gateway/1.0")
        .send()
        .await;
    let Ok(response) = response else {
        return UpdateInfo {
            current_version: current,
            latest_version: None,
            update_available: false,
            release_url: None,
            changelog: None,
            error: Some("无法连接 GitHub Releases".to_string()),
        };
    };
    if !response.status().is_success() {
        return UpdateInfo {
            current_version: current,
            latest_version: None,
            update_available: false,
            release_url: None,
            changelog: None,
            error: Some(format!("GitHub Releases 返回 HTTP {}", response.status())),
        };
    }
    let value = response.json::<Value>().await.unwrap_or_default();
    let latest = value
        .get("tag_name")
        .or_else(|| value.get("name"))
        .and_then(Value::as_str)
        .map(|tag| tag.trim_start_matches('v').to_string());
    let release_url = value
        .get("html_url")
        .and_then(Value::as_str)
        .map(str::to_string);
    let changelog = value
        .get("body")
        .and_then(Value::as_str)
        .map(|body| body.chars().take(4000).collect());
    let update_available = latest
        .as_deref()
        .map(|latest| version_newer(latest, current))
        .unwrap_or(false);
    UpdateInfo {
        current_version: current,
        latest_version: latest,
        update_available,
        release_url,
        changelog,
        error: None,
    }
}

fn startup_script_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup")
            .join("OpenRelay.cmd"),
    )
}

fn version_newer(latest: &str, current: &str) -> bool {
    let latest = version_parts(latest);
    let current = version_parts(current);
    latest > current
}

fn version_parts(value: &str) -> Vec<u64> {
    value
        .trim_start_matches('v')
        .split(|ch| ch == '.' || ch == '-' || ch == '+')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}
