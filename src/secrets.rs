use crate::config::{save_config, AppConfig, ConfigError};
use bcrypt::verify;
use serde::Serialize;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const DEFAULT_ADMIN_PASSWORD: &str = "admin123";
const DEFAULT_MASTER_KEY: &str = "openrelay-master";
const PROTECTED_PREFIX: &str = "protected:dpapi:";

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("config error: {0}")]
    Config(#[from] ConfigError),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretProtectionResult {
    pub supported: bool,
    pub enabled: bool,
    pub protected_count: usize,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityFinding {
    pub severity: &'static str,
    pub code: String,
    pub path: String,
    pub message: String,
}

pub fn secrets_supported() -> bool {
    cfg!(windows)
}

pub fn is_protected_secret(value: &str) -> bool {
    value.starts_with(PROTECTED_PREFIX)
}

pub fn protection_marker_path(root: &Path) -> PathBuf {
    root.join(".secret-protection")
}

pub fn protection_enabled(root: &Path) -> bool {
    secrets_supported() && protection_marker_path(root).exists()
}

pub fn protect_config_secrets(
    root: &Path,
    cfg: &AppConfig,
) -> Result<SecretProtectionResult, SecretError> {
    if !secrets_supported() {
        return Ok(SecretProtectionResult {
            supported: false,
            enabled: false,
            protected_count: 0,
            message: "当前平台不支持 Windows DPAPI".to_string(),
        });
    }
    fs::write(protection_marker_path(root), "provider_api_keys=dpapi\n")?;
    save_config(root, cfg)?;
    Ok(SecretProtectionResult {
        supported: true,
        enabled: true,
        protected_count: count_protectable_provider_keys(cfg),
        message: "Provider API Key 已启用本机用户级 DPAPI 保护".to_string(),
    })
}

pub fn config_for_storage(root: &Path, cfg: &AppConfig) -> Result<AppConfig, io::Error> {
    if !protection_enabled(root) {
        return Ok(cfg.clone());
    }
    let mut stored = cfg.clone();
    for provider in &mut stored.providers {
        if should_protect_value(&provider.api_key) {
            provider.api_key = protect_secret(&provider.api_key)?;
        }
    }
    Ok(stored)
}

pub fn unprotect_config(cfg: &mut AppConfig) -> Result<(), io::Error> {
    for provider in &mut cfg.providers {
        if is_protected_secret(&provider.api_key) {
            provider.api_key = unprotect_secret(&provider.api_key)?;
        }
    }
    Ok(())
}

pub fn audit_security(
    root: &Path,
    cfg: &AppConfig,
    jwt_secret_present: bool,
) -> Result<Vec<SecurityFinding>, SecretError> {
    let mut findings = Vec::new();
    if verify(DEFAULT_ADMIN_PASSWORD, &cfg.admin.password_hash).unwrap_or(false) {
        findings.push(finding(
            "danger",
            "default_admin_password",
            "admin.password",
            "管理员仍在使用默认密码 admin123",
        ));
    }
    if cfg.general_settings.master_key.as_deref() == Some(DEFAULT_MASTER_KEY) {
        findings.push(finding(
            "danger",
            "default_master_key",
            "general_settings.master_key",
            "Master key 仍是默认值 openrelay-master",
        ));
    }
    if !jwt_secret_present {
        findings.push(finding(
            "warning",
            "missing_jwt_secret",
            "environment.JWT_SECRET",
            "未设置 JWT_SECRET，重启后使用内置默认签名密钥",
        ));
    }
    if secrets_supported() && !protection_enabled(root) {
        findings.push(finding(
            "warning",
            "secret_protection_disabled",
            ".secret-protection",
            "Provider API Key 尚未启用 Windows DPAPI 本机保护",
        ));
    }
    for (index, provider) in cfg.providers.iter().enumerate() {
        if provider.api_key.trim().is_empty()
            || is_env_reference(&provider.api_key)
            || is_protected_secret(&provider.api_key)
        {
            continue;
        }
        findings.push(finding(
            "warning",
            "plaintext_provider_key",
            format!("providers[{index}].api_key"),
            format!("{} 的 API Key 当前以可读文本保存", provider_name(provider)),
        ));
    }
    Ok(findings)
}

pub fn redact_secret(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.len() <= 8 {
        return "***".to_string();
    }
    format!("{}...{}", &trimmed[..4], &trimmed[trimmed.len() - 4..])
}

fn count_protectable_provider_keys(cfg: &AppConfig) -> usize {
    cfg.providers
        .iter()
        .filter(|provider| should_protect_value(&provider.api_key))
        .count()
}

fn should_protect_value(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && !is_env_reference(value) && !is_protected_secret(value)
}

fn is_env_reference(value: &str) -> bool {
    value.starts_with("os.environ/")
}

fn finding(
    severity: &'static str,
    code: impl Into<String>,
    path: impl Into<String>,
    message: impl Into<String>,
) -> SecurityFinding {
    SecurityFinding {
        severity,
        code: code.into(),
        path: path.into(),
        message: message.into(),
    }
}

fn provider_name(provider: &crate::config::ProviderConfig) -> String {
    if provider.name.trim().is_empty() {
        provider.id.clone()
    } else {
        provider.name.clone()
    }
}

#[cfg(windows)]
fn protect_secret(value: &str) -> Result<String, io::Error> {
    use std::ptr;
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let mut input = CRYPT_INTEGER_BLOB {
        cbData: value.as_bytes().len() as u32,
        pbData: value.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &mut input,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(io::Error::from_raw_os_error(
            unsafe { GetLastError() } as i32
        ));
    }
    let bytes = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) };
    let encoded = format!("{PROTECTED_PREFIX}{}", hex_encode(bytes));
    unsafe {
        LocalFree(output.pbData as _);
    }
    Ok(encoded)
}

#[cfg(not(windows))]
fn protect_secret(value: &str) -> Result<String, io::Error> {
    Ok(value.to_string())
}

#[cfg(windows)]
fn unprotect_secret(value: &str) -> Result<String, io::Error> {
    use std::ptr;
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let encoded = value
        .strip_prefix(PROTECTED_PREFIX)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid protected secret"))?;
    let mut bytes = hex_decode(encoded)?;
    let mut input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &mut input,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(io::Error::from_raw_os_error(
            unsafe { GetLastError() } as i32
        ));
    }
    let plaintext = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) };
    let text = String::from_utf8(plaintext.to_vec())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    unsafe {
        LocalFree(output.pbData as _);
    }
    Ok(text)
}

#[cfg(not(windows))]
fn unprotect_secret(value: &str) -> Result<String, io::Error> {
    Ok(value.to_string())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(value: &str) -> Result<Vec<u8>, io::Error> {
    if value.len() % 2 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid protected secret encoding",
        ));
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let raw = value.as_bytes();
    for pair in raw.chunks(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_value(byte: u8) -> Result<u8, io::Error> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid protected secret encoding",
        )),
    }
}
