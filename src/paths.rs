use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub data_root: PathBuf,
    pub static_root: PathBuf,
    pub legacy_root: Option<PathBuf>,
}

impl AppPaths {
    pub fn from_env() -> Self {
        let data_root = data_root_from_env();
        let static_root = static_root_from_env();
        let legacy_root = legacy_root_from_env(&data_root, &static_root);
        Self {
            data_root,
            static_root,
            legacy_root,
        }
    }
}

pub fn default_data_root_for_home(home: &Path) -> PathBuf {
    home.join(".openrelay")
}

pub fn data_root_from_env() -> PathBuf {
    env_path("OPENRELAY_DATA_DIR")
        .or_else(|| env_path("OPENRELAY_ROOT"))
        .unwrap_or_else(|| {
            home_dir()
                .map(|home| default_data_root_for_home(&home))
                .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        })
}

pub fn static_root_from_env() -> PathBuf {
    if let Some(path) = env_path("OPENRELAY_STATIC_ROOT") {
        return path;
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir.join("public").join("index.html").exists() {
                return dir.to_path_buf();
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn legacy_root_from_env(data_root: &Path, static_root: &Path) -> Option<PathBuf> {
    if let Some(path) = env_path("OPENRELAY_LEGACY_ROOT") {
        return Some(path);
    }
    let mut candidates = Vec::new();
    if let Ok(cwd) = env::current_dir() {
        candidates.push(cwd);
    }
    candidates.push(static_root.to_path_buf());
    candidates
        .into_iter()
        .find(|candidate| !same_path(candidate, data_root) && looks_like_legacy_root(candidate))
}

pub fn looks_like_legacy_root(root: &Path) -> bool {
    root.join("config.json").exists()
        || root.join("usage.jsonl").exists()
        || root.join("openrelay.db").exists()
        || root.join("litellm-config.yaml").exists()
        || root.join("conversations").exists()
}

pub fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
}
