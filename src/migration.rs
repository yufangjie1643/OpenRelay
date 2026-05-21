use crate::config::{config_path, ensure_files, ConfigError};
use crate::database::{Database, DatabaseError};
use crate::paths::same_path;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("config error: {0}")]
    Config(#[from] ConfigError),
    #[error("database error: {0}")]
    Database(#[from] DatabaseError),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub copied_config: bool,
    pub copied_database: bool,
    pub imported_usage_rows: usize,
}

pub fn migrate_legacy_data(
    data_root: &Path,
    legacy_root: Option<&Path>,
) -> Result<MigrationReport, MigrationError> {
    fs::create_dir_all(data_root)?;
    let mut report = MigrationReport::default();

    if let Some(legacy_root) = legacy_root {
        if !same_path(data_root, legacy_root) {
            report.copied_config = copy_config_if_needed(data_root, legacy_root)?;
            report.copied_database = copy_database_if_needed(data_root, legacy_root)?;
        }
    }

    ensure_files(data_root)?;

    if let Some(legacy_root) = legacy_root {
        let db = Database::open(data_root)?;
        report.imported_usage_rows = import_usage_jsonl_once(data_root, legacy_root, &db)?;
    }

    Ok(report)
}

fn copy_config_if_needed(data_root: &Path, legacy_root: &Path) -> Result<bool, MigrationError> {
    let target = config_path(data_root);
    let source = config_path(legacy_root);
    if target.exists() || !source.exists() {
        return Ok(false);
    }
    fs::copy(source, target)?;
    Ok(true)
}

fn copy_database_if_needed(data_root: &Path, legacy_root: &Path) -> Result<bool, MigrationError> {
    let target = data_root.join("openrelay.db");
    let source = legacy_root.join("openrelay.db");
    if target.exists() || !source.exists() {
        return Ok(false);
    }

    fs::copy(&source, &target)?;
    for suffix in ["-wal", "-shm"] {
        let source_sidecar = legacy_root.join(format!("openrelay.db{suffix}"));
        if source_sidecar.exists() {
            fs::copy(
                source_sidecar,
                data_root.join(format!("openrelay.db{suffix}")),
            )?;
        }
    }
    Ok(true)
}

fn import_usage_jsonl_once(
    data_root: &Path,
    legacy_root: &Path,
    db: &Database,
) -> Result<usize, MigrationError> {
    let usage_path = legacy_root.join("usage.jsonl");
    if !usage_path.exists() {
        return Ok(0);
    }
    let marker = migration_marker(data_root, "usage-jsonl.imported");
    if marker.exists() {
        return Ok(0);
    }

    let imported = db.import_usage_jsonl(&usage_path)?;
    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        marker,
        format!(
            "source={}\nimported_rows={imported}\n",
            usage_path.display()
        ),
    )?;
    Ok(imported)
}

fn migration_marker(data_root: &Path, name: &str) -> PathBuf {
    data_root.join(".migrations").join(name)
}
