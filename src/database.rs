use rusqlite::{params, Connection};
use serde::Serialize;
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

    pub fn clear_usage(&self) -> Result<(), DatabaseError> {
        self.lock_conn()?.execute("DELETE FROM usage_logs", [])?;
        Ok(())
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
             );",
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
