use chrono::{TimeZone, Utc};
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Debug, thiserror::Error)]
pub enum DatabaseError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database lock poisoned")]
    LockPoisoned,
    #[error("blocking database task failed: {0}")]
    BlockingTask(String),
}

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct UsageLog {
    pub timestamp: String,
    pub request_id: String,
    pub model: String,
    pub key_name: String,
    pub input_tokens: u64,
    pub cached_tokens: u64,
    pub cached_write_tokens: u64,
    pub output_tokens: u64,
    pub cost: f64,
    pub status: u16,
    pub duration_ms: u64,
    pub stream: bool,
    pub user_agent: String,
    pub error: String,
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct UsageBucket {
    pub requests: u64,
    pub input_tokens: u64,
    pub cached_tokens: u64,
    pub output_tokens: u64,
    pub cost: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    pub total_requests: u64,
    pub total_input_tokens: u64,
    pub total_cached_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost: f64,
    pub cache_hit_rate: f64,
    pub by_model: BTreeMap<String, UsageBucket>,
    pub by_key: BTreeMap<String, UsageBucket>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pagination {
    pub page: u64,
    pub page_size: u64,
    pub total: u64,
    pub total_pages: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsagePage {
    pub stats: UsageStats,
    pub entries: Vec<UsageLog>,
    pub pagination: Pagination,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ConversationEntry {
    pub filename: String,
    pub timestamp: String,
    pub model: String,
    pub key_name: String,
    pub request_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConversationPage {
    pub entries: Vec<ConversationEntry>,
    pub pagination: Pagination,
}

impl Database {
    pub fn open(root: &Path) -> Result<Self, DatabaseError> {
        std::fs::create_dir_all(root)?;
        let conn = Connection::open(root.join("openrelay.db"))?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.create_tables()?;
        Ok(db)
    }

    pub fn memory() -> Result<Self, DatabaseError> {
        let db = Self {
            conn: Arc::new(Mutex::new(Connection::open_in_memory()?)),
        };
        db.create_tables()?;
        Ok(db)
    }

    pub fn record_usage(&self, entry: &UsageLog) -> Result<(), DatabaseError> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO usage_logs (
                timestamp, request_id, model, key_name, input_tokens, cached_tokens,
                cached_write_tokens, output_tokens, cost, status, duration_ms, stream,
                user_agent, error, path
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                entry.timestamp,
                entry.request_id,
                entry.model,
                entry.key_name,
                entry.input_tokens as i64,
                entry.cached_tokens as i64,
                entry.cached_write_tokens as i64,
                entry.output_tokens as i64,
                entry.cost,
                entry.status as i64,
                entry.duration_ms as i64,
                if entry.stream { 1i64 } else { 0i64 },
                entry.user_agent,
                entry.error,
                entry.path
            ],
        )?;
        Ok(())
    }

    pub async fn record_usage_async(&self, entry: UsageLog) -> Result<(), DatabaseError> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.record_usage(&entry))
            .await
            .map_err(|err| DatabaseError::BlockingTask(err.to_string()))?
    }

    pub fn usage_page(&self, page: u64, page_size: u64) -> Result<UsagePage, DatabaseError> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        let offset = (page - 1) * page_size;
        let conn = self.lock_conn()?;
        let total: u64 = conn.query_row("SELECT COUNT(*) FROM usage_logs", [], |row| {
            row.get::<_, i64>(0)
        })? as u64;
        let total_pages = ((total + page_size - 1) / page_size).max(1);
        let stats = self.usage_stats_locked(&conn)?;
        let mut stmt = conn.prepare(
            "SELECT timestamp, request_id, model, key_name, input_tokens, cached_tokens,
                    cached_write_tokens, output_tokens, cost, status, duration_ms, stream,
                    user_agent, error, path
             FROM usage_logs
             ORDER BY id DESC
             LIMIT ?1 OFFSET ?2",
        )?;
        let entries = stmt
            .query_map(params![page_size as i64, offset as i64], row_to_usage_log)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(UsagePage {
            stats,
            entries,
            pagination: Pagination {
                page,
                page_size,
                total,
                total_pages,
            },
        })
    }

    pub async fn usage_page_async(
        &self,
        page: u64,
        page_size: u64,
    ) -> Result<UsagePage, DatabaseError> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.usage_page(page, page_size))
            .await
            .map_err(|err| DatabaseError::BlockingTask(err.to_string()))?
    }

    pub fn conversation_index_files(
        &self,
        directory: &Path,
    ) -> Result<BTreeMap<String, (u64, i64)>, DatabaseError> {
        let directory = directory_key(directory);
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT filename, file_size, modified_at
             FROM conversation_index
             WHERE directory = ?1",
        )?;
        let rows = stmt
            .query_map(params![directory], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (row.get::<_, i64>(1)?.max(0) as u64, row.get::<_, i64>(2)?),
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().collect())
    }

    pub fn conversation_usage_metadata(
        &self,
        request_id: &str,
        filename: &str,
    ) -> Result<Option<ConversationEntry>, DatabaseError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT timestamp, model, key_name
             FROM usage_logs
             WHERE request_id = ?1
             ORDER BY id DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![request_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(ConversationEntry {
            filename: filename.to_string(),
            timestamp: row.get(0)?,
            model: row.get(1)?,
            key_name: row.get(2)?,
            request_id: request_id.to_string(),
        }))
    }

    pub fn upsert_conversation_index(
        &self,
        directory: &Path,
        entry: &ConversationEntry,
        file_size: u64,
        modified_at: i64,
    ) -> Result<(), DatabaseError> {
        let directory = directory_key(directory);
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO conversation_index (
                directory, filename, timestamp, model, key_name, request_id,
                file_size, modified_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(directory, filename) DO UPDATE SET
                timestamp = excluded.timestamp,
                model = excluded.model,
                key_name = excluded.key_name,
                request_id = excluded.request_id,
                file_size = excluded.file_size,
                modified_at = excluded.modified_at",
            params![
                directory,
                entry.filename,
                entry.timestamp,
                entry.model,
                entry.key_name,
                entry.request_id,
                file_size as i64,
                modified_at,
            ],
        )?;
        Ok(())
    }

    pub fn remove_conversation_index(
        &self,
        directory: &Path,
        filename: &str,
    ) -> Result<(), DatabaseError> {
        let directory = directory_key(directory);
        self.lock_conn()?.execute(
            "DELETE FROM conversation_index WHERE directory = ?1 AND filename = ?2",
            params![directory, filename],
        )?;
        Ok(())
    }

    pub fn conversation_page(
        &self,
        directory: &Path,
        page: u64,
        page_size: u64,
    ) -> Result<ConversationPage, DatabaseError> {
        let requested_page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        let directory = directory_key(directory);
        let conn = self.lock_conn()?;
        let total: u64 = conn.query_row(
            "SELECT COUNT(*) FROM conversation_index WHERE directory = ?1",
            params![directory],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let total_pages = ((total + page_size - 1) / page_size).max(1);
        let page = requested_page.min(total_pages);
        let offset = (page - 1) * page_size;
        let mut stmt = conn.prepare(
            "SELECT filename, timestamp, model, key_name, request_id
             FROM conversation_index
             WHERE directory = ?1
             ORDER BY timestamp DESC, filename DESC
             LIMIT ?2 OFFSET ?3",
        )?;
        let entries = stmt
            .query_map(
                params![directory, page_size as i64, offset as i64],
                row_to_conversation_entry,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ConversationPage {
            entries,
            pagination: Pagination {
                page,
                page_size,
                total,
                total_pages,
            },
        })
    }

    pub fn user_agent_candidates(&self, limit: u64) -> Result<Vec<String>, DatabaseError> {
        let limit = limit.clamp(1, 100);
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT user_agent
             FROM usage_logs
             WHERE TRIM(user_agent) <> ''
             GROUP BY user_agent
             ORDER BY MAX(id) DESC
             LIMIT ?1",
        )?;
        let candidates = stmt
            .query_map(params![limit as i64], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(candidates)
    }

    pub async fn user_agent_candidates_async(
        &self,
        limit: u64,
    ) -> Result<Vec<String>, DatabaseError> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.user_agent_candidates(limit))
            .await
            .map_err(|err| DatabaseError::BlockingTask(err.to_string()))?
    }

    pub fn clear_usage(&self) -> Result<(), DatabaseError> {
        self.lock_conn()?.execute("DELETE FROM usage_logs", [])?;
        Ok(())
    }

    pub async fn clear_usage_async(&self) -> Result<(), DatabaseError> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.clear_usage())
            .await
            .map_err(|err| DatabaseError::BlockingTask(err.to_string()))?
    }

    pub fn request_count_since(
        &self,
        since: &str,
        key_name: Option<&str>,
        model: Option<&str>,
    ) -> Result<u64, DatabaseError> {
        let conn = self.lock_conn()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM usage_logs
             WHERE timestamp >= ?1
               AND (?2 IS NULL OR key_name = ?2)
               AND (?3 IS NULL OR model = ?3)",
            params![since, key_name, model],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    pub async fn request_count_since_async(
        &self,
        since: String,
        key_name: Option<String>,
        model: Option<String>,
    ) -> Result<u64, DatabaseError> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || {
            db.request_count_since(&since, key_name.as_deref(), model.as_deref())
        })
        .await
        .map_err(|err| DatabaseError::BlockingTask(err.to_string()))?
    }

    pub fn total_cost(
        &self,
        key_name: Option<&str>,
        model: Option<&str>,
    ) -> Result<f64, DatabaseError> {
        let conn = self.lock_conn()?;
        let cost: f64 = conn.query_row(
            "SELECT COALESCE(SUM(cost), 0)
             FROM usage_logs
             WHERE (?1 IS NULL OR key_name = ?1)
               AND (?2 IS NULL OR model = ?2)",
            params![key_name, model],
            |row| row.get(0),
        )?;
        Ok(cost)
    }

    pub async fn total_cost_async(
        &self,
        key_name: Option<String>,
        model: Option<String>,
    ) -> Result<f64, DatabaseError> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.total_cost(key_name.as_deref(), model.as_deref()))
            .await
            .map_err(|err| DatabaseError::BlockingTask(err.to_string()))?
    }

    pub fn export_usage_csv(&self) -> Result<String, DatabaseError> {
        let conn = self.lock_conn()?;
        let mut out = String::from(
            "timestamp,model,key_name,input_tokens,cached_tokens,cached_write_tokens,output_tokens,cost,status,duration_ms,stream,request_id,user_agent,error,path\n",
        );
        let mut stmt = conn.prepare(
            "SELECT timestamp, request_id, model, key_name, input_tokens, cached_tokens,
                    cached_write_tokens, output_tokens, cost, status, duration_ms, stream,
                    user_agent, error, path
             FROM usage_logs
             ORDER BY id DESC",
        )?;
        let entries = stmt
            .query_map([], row_to_usage_log)?
            .collect::<Result<Vec<_>, _>>()?;
        for entry in entries {
            out.push_str(&format!(
                "{},{},{},{},{},{},{},{:.8},{},{},{},{},{},{},{}\n",
                csv_cell(&entry.timestamp),
                csv_cell(&entry.model),
                csv_cell(&entry.key_name),
                entry.input_tokens,
                entry.cached_tokens,
                entry.cached_write_tokens,
                entry.output_tokens,
                entry.cost,
                entry.status,
                entry.duration_ms,
                if entry.stream { "true" } else { "false" },
                csv_cell(&entry.request_id),
                csv_cell(&entry.user_agent),
                csv_cell(&entry.error),
                csv_cell(&entry.path),
            ));
        }
        Ok(out)
    }

    pub async fn export_usage_csv_async(&self) -> Result<String, DatabaseError> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.export_usage_csv())
            .await
            .map_err(|err| DatabaseError::BlockingTask(err.to_string()))?
    }

    pub fn import_usage_jsonl(&self, path: &Path) -> Result<usize, DatabaseError> {
        let raw = std::fs::read_to_string(path)?;
        let mut imported = 0usize;
        for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            self.record_usage(&legacy_usage_log(&value))?;
            imported += 1;
        }
        Ok(imported)
    }

    fn create_tables(&self) -> Result<(), DatabaseError> {
        self.lock_conn()?.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS usage_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                request_id TEXT NOT NULL DEFAULT '',
                model TEXT NOT NULL DEFAULT '',
                key_name TEXT NOT NULL DEFAULT '',
                input_tokens INTEGER NOT NULL DEFAULT 0,
                cached_tokens INTEGER NOT NULL DEFAULT 0,
                cached_write_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                cost REAL NOT NULL DEFAULT 0,
                status INTEGER NOT NULL DEFAULT 0,
                duration_ms INTEGER NOT NULL DEFAULT 0,
                stream INTEGER NOT NULL DEFAULT 0,
                user_agent TEXT NOT NULL DEFAULT '',
                error TEXT NOT NULL DEFAULT '',
                path TEXT NOT NULL DEFAULT ''
             );
             CREATE INDEX IF NOT EXISTS idx_usage_logs_timestamp ON usage_logs(timestamp DESC);
             CREATE INDEX IF NOT EXISTS idx_usage_logs_model ON usage_logs(model);
             CREATE INDEX IF NOT EXISTS idx_usage_logs_key_name ON usage_logs(key_name);
             CREATE TABLE IF NOT EXISTS conversations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                filename TEXT NOT NULL UNIQUE,
                timestamp TEXT NOT NULL,
                model TEXT NOT NULL DEFAULT '',
                request_json TEXT NOT NULL DEFAULT '',
                response_json TEXT NOT NULL DEFAULT ''
             );
             CREATE TABLE IF NOT EXISTS conversation_index (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                directory TEXT NOT NULL,
                filename TEXT NOT NULL,
                timestamp TEXT NOT NULL DEFAULT '',
                model TEXT NOT NULL DEFAULT '',
                key_name TEXT NOT NULL DEFAULT 'master',
                request_id TEXT NOT NULL DEFAULT '',
                file_size INTEGER NOT NULL DEFAULT 0,
                modified_at INTEGER NOT NULL DEFAULT 0,
                UNIQUE(directory, filename)
             );
             CREATE INDEX IF NOT EXISTS idx_conversation_index_directory_timestamp
                ON conversation_index(directory, timestamp DESC, filename DESC);
             CREATE INDEX IF NOT EXISTS idx_conversation_index_request_id
                ON conversation_index(request_id);",
        )?;
        Ok(())
    }

    fn usage_stats_locked(&self, conn: &Connection) -> Result<UsageStats, DatabaseError> {
        let totals = conn.query_row(
            "SELECT
                COUNT(*),
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(cached_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(cost), 0)
             FROM usage_logs",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, i64>(2)? as u64,
                    row.get::<_, i64>(3)? as u64,
                    row.get::<_, f64>(4)?,
                ))
            },
        )?;
        let by_model = grouped_buckets(conn, "model")?;
        let by_key = grouped_buckets(conn, "key_name")?;
        let cache_hit_rate = if totals.1 == 0 {
            0.0
        } else {
            round2(totals.2 as f64 * 100.0 / totals.1 as f64)
        };
        Ok(UsageStats {
            total_requests: totals.0,
            total_input_tokens: totals.1,
            total_cached_tokens: totals.2,
            total_output_tokens: totals.3,
            total_cost: totals.4,
            cache_hit_rate,
            by_model,
            by_key,
        })
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, DatabaseError> {
        self.conn.lock().map_err(|_| DatabaseError::LockPoisoned)
    }
}

fn grouped_buckets(
    conn: &Connection,
    column: &str,
) -> Result<BTreeMap<String, UsageBucket>, DatabaseError> {
    let sql = format!(
        "SELECT {column}, COUNT(*), COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(cached_tokens), 0), COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(cost), 0)
         FROM usage_logs
         GROUP BY {column}
         ORDER BY COUNT(*) DESC, {column} ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let pairs = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                UsageBucket {
                    requests: row.get::<_, i64>(1)? as u64,
                    input_tokens: row.get::<_, i64>(2)? as u64,
                    cached_tokens: row.get::<_, i64>(3)? as u64,
                    output_tokens: row.get::<_, i64>(4)? as u64,
                    cost: row.get::<_, f64>(5)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(pairs.into_iter().collect())
}

fn row_to_usage_log(row: &rusqlite::Row<'_>) -> rusqlite::Result<UsageLog> {
    Ok(UsageLog {
        timestamp: row.get(0)?,
        request_id: row.get(1)?,
        model: row.get(2)?,
        key_name: row.get(3)?,
        input_tokens: row.get::<_, i64>(4)? as u64,
        cached_tokens: row.get::<_, i64>(5)? as u64,
        cached_write_tokens: row.get::<_, i64>(6)? as u64,
        output_tokens: row.get::<_, i64>(7)? as u64,
        cost: row.get(8)?,
        status: row.get::<_, i64>(9)? as u16,
        duration_ms: row.get::<_, i64>(10)? as u64,
        stream: row.get::<_, i64>(11)? != 0,
        user_agent: row.get(12)?,
        error: row.get(13)?,
        path: row.get(14)?,
    })
}

fn row_to_conversation_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationEntry> {
    Ok(ConversationEntry {
        filename: row.get(0)?,
        timestamp: row.get(1)?,
        model: row.get(2)?,
        key_name: row.get(3)?,
        request_id: row.get(4)?,
    })
}

fn directory_key(directory: &Path) -> String {
    directory.to_string_lossy().to_string()
}

fn legacy_usage_log(value: &Value) -> UsageLog {
    UsageLog {
        timestamp: legacy_timestamp(value),
        request_id: string_field(value, "request_id"),
        model: string_field(value, "model"),
        key_name: string_field(value, "key_name"),
        input_tokens: u64_field(value, "input_tokens"),
        cached_tokens: u64_field(value, "cached_tokens"),
        cached_write_tokens: u64_field(value, "cached_write_tokens"),
        output_tokens: u64_field(value, "output_tokens"),
        cost: f64_field(value, "cost"),
        status: u64_field(value, "status").min(u16::MAX as u64) as u16,
        duration_ms: u64_field(value, "duration_ms"),
        stream: value
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        user_agent: string_field(value, "user_agent"),
        error: string_field(value, "error"),
        path: string_field(value, "path"),
    }
}

fn legacy_timestamp(value: &Value) -> String {
    if let Some(timestamp) = value
        .get("timestamp")
        .and_then(Value::as_str)
        .filter(|timestamp| !timestamp.trim().is_empty())
    {
        return timestamp.to_string();
    }
    for field in ["timestamp", "last_timestamp", "first_timestamp"] {
        if let Some(millis) = value.get(field).and_then(Value::as_i64) {
            if let Some(datetime) = Utc.timestamp_millis_opt(millis).single() {
                return datetime.to_rfc3339();
            }
        }
    }
    Utc::now().to_rfc3339()
}

fn string_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn u64_field(value: &Value, field: &str) -> u64 {
    value.get(field).and_then(Value::as_u64).unwrap_or(0)
}

fn f64_field(value: &Value, field: &str) -> f64 {
    value.get(field).and_then(Value::as_f64).unwrap_or(0.0)
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn csv_cell(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}
