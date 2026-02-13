//! Proxy Log Database - Advanced query capabilities for proxy.db
//!
//! Provides pagination, filtering, detail view, debug log management,
//! and IP-based token usage statistics on top of the proxy.db initialized
//! by `proxy::monitor`.
//!
//! Requirements: 12.6, 13.3

use crate::proxy::monitor::ProxyRequestLog;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ============================================================================
// Data Structures
// ============================================================================

/// Paginated query result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginatedLogs {
    pub logs: Vec<ProxyRequestLog>,
    pub total: u64,
    pub page: usize,
    pub page_size: usize,
}

/// IP-based token usage statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpTokenStats {
    pub client_ip: String,
    pub total_tokens: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub request_count: i64,
    pub username: Option<String>,
}

/// Debug log entry for full request/response chain
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugLogEntry {
    pub id: String,
    pub timestamp: i64,
    pub method: String,
    pub url: String,
    pub status: u16,
    pub duration: u64,
    pub model: Option<String>,
    pub mapped_model: Option<String>,
    pub account_email: Option<String>,
    pub client_ip: Option<String>,
    pub error: Option<String>,
    pub request_body: Option<String>,
    pub response_body: Option<String>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub protocol: Option<String>,
    pub username: Option<String>,
}

// ============================================================================
// Database Connection
// ============================================================================

/// Get proxy.db path (same DB as proxy::monitor)
pub fn get_proxy_db_path() -> Result<PathBuf, String> {
    let data_dir = crate::modules::account::get_data_dir()?;
    Ok(data_dir.join("proxy.db"))
}

fn connect_db() -> Result<Connection, String> {
    let db_path = get_proxy_db_path()?;
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "busy_timeout", 5000)
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| e.to_string())?;
    Ok(conn)
}

// ============================================================================
// Initialization
// ============================================================================

/// Initialize proxy.db - ensures table and indexes exist.
/// Safe to call multiple times (uses IF NOT EXISTS).
pub fn init_db() -> Result<(), String> {
    let conn = connect_db()?;
    init_db_with_conn(&conn)
}

fn init_db_with_conn(conn: &Connection) -> Result<(), String> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS proxy_logs (
            id TEXT PRIMARY KEY,
            timestamp INTEGER NOT NULL,
            method TEXT,
            url TEXT,
            status INTEGER,
            duration INTEGER,
            model TEXT,
            mapped_model TEXT,
            account_email TEXT,
            client_ip TEXT,
            error TEXT,
            request_body TEXT,
            response_body TEXT,
            input_tokens INTEGER,
            output_tokens INTEGER,
            protocol TEXT,
            username TEXT
        )",
        [],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_proxy_logs_timestamp ON proxy_logs (timestamp DESC)",
        [],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_proxy_logs_status ON proxy_logs (status)",
        [],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_proxy_logs_client_ip ON proxy_logs (client_ip)",
        [],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_proxy_logs_model ON proxy_logs (model)",
        [],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

// ============================================================================
// Log CRUD - Pagination & Filtering
// ============================================================================

/// Get logs summary (without request_body/response_body) with pagination.
pub fn get_logs_summary(limit: usize, offset: usize) -> Result<Vec<ProxyRequestLog>, String> {
    let conn = connect_db()?;
    get_logs_summary_with_conn(&conn, limit, offset)
}

fn get_logs_summary_with_conn(
    conn: &Connection,
    limit: usize,
    offset: usize,
) -> Result<Vec<ProxyRequestLog>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, timestamp, method, url, status, duration, model, error,
                    input_tokens, output_tokens, account_email, mapped_model,
                    protocol, client_ip, username
             FROM proxy_logs
             ORDER BY timestamp DESC
             LIMIT ?1 OFFSET ?2",
        )
        .map_err(|e| e.to_string())?;

    let logs_iter = stmt
        .query_map(params![limit, offset], |row| {
            Ok(ProxyRequestLog {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                method: row.get(2)?,
                url: row.get(3)?,
                status: row.get(4)?,
                duration: row.get(5)?,
                model: row.get(6)?,
                error: row.get(7)?,
                request_body: None,
                response_body: None,
                input_tokens: row.get(8)?,
                output_tokens: row.get(9)?,
                account_email: row.get(10)?,
                mapped_model: row.get(11)?,
                protocol: row.get(12)?,
                client_ip: row.get(13)?,
                username: row.get(14)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut logs = Vec::new();
    for log in logs_iter {
        logs.push(log.map_err(|e| e.to_string())?);
    }
    Ok(logs)
}

/// Get filtered logs count.
/// `filter`: text to match in url, method, model, status, account_email, client_ip.
/// `errors_only`: if true, only count error logs (status < 200 or >= 400).
pub fn get_logs_count_filtered(filter: &str, errors_only: bool) -> Result<u64, String> {
    let conn = connect_db()?;
    get_logs_count_filtered_with_conn(&conn, filter, errors_only)
}

fn get_logs_count_filtered_with_conn(
    conn: &Connection,
    filter: &str,
    errors_only: bool,
) -> Result<u64, String> {
    let filter_pattern = format!("%{}%", filter);

    let count: u64 = if errors_only && !filter.is_empty() {
        conn.query_row(
            "SELECT COUNT(*) FROM proxy_logs
             WHERE (status < 200 OR status >= 400)
             AND (url LIKE ?1 OR method LIKE ?1 OR model LIKE ?1
                  OR CAST(status AS TEXT) LIKE ?1 OR account_email LIKE ?1 OR client_ip LIKE ?1)",
            params![filter_pattern],
            |row| row.get(0),
        )
    } else if errors_only {
        conn.query_row(
            "SELECT COUNT(*) FROM proxy_logs WHERE (status < 200 OR status >= 400)",
            [],
            |row| row.get(0),
        )
    } else if filter.is_empty() {
        conn.query_row("SELECT COUNT(*) FROM proxy_logs", [], |row| row.get(0))
    } else {
        conn.query_row(
            "SELECT COUNT(*) FROM proxy_logs
             WHERE (url LIKE ?1 OR method LIKE ?1 OR model LIKE ?1
                    OR CAST(status AS TEXT) LIKE ?1 OR account_email LIKE ?1 OR client_ip LIKE ?1)",
            params![filter_pattern],
            |row| row.get(0),
        )
    }
    .map_err(|e| e.to_string())?;

    Ok(count)
}

/// Get filtered logs with pagination.
pub fn get_logs_filtered(
    filter: &str,
    errors_only: bool,
    limit: usize,
    offset: usize,
) -> Result<Vec<ProxyRequestLog>, String> {
    let conn = connect_db()?;
    get_logs_filtered_with_conn(&conn, filter, errors_only, limit, offset)
}

fn get_logs_filtered_with_conn(
    conn: &Connection,
    filter: &str,
    errors_only: bool,
    limit: usize,
    offset: usize,
) -> Result<Vec<ProxyRequestLog>, String> {
    let filter_pattern = format!("%{}%", filter);

    let base_select = "SELECT id, timestamp, method, url, status, duration, model, error,
                input_tokens, output_tokens, account_email, mapped_model,
                protocol, client_ip, username
         FROM proxy_logs";

    let (sql, use_filter) = if errors_only && !filter.is_empty() {
        (
            format!(
                "{} WHERE (status < 200 OR status >= 400)
                 AND (url LIKE ?3 OR method LIKE ?3 OR model LIKE ?3
                      OR CAST(status AS TEXT) LIKE ?3 OR account_email LIKE ?3 OR client_ip LIKE ?3)
                 ORDER BY timestamp DESC LIMIT ?1 OFFSET ?2",
                base_select
            ),
            true,
        )
    } else if errors_only {
        (
            format!(
                "{} WHERE (status < 200 OR status >= 400)
                 ORDER BY timestamp DESC LIMIT ?1 OFFSET ?2",
                base_select
            ),
            false,
        )
    } else if filter.is_empty() {
        (
            format!(
                "{} ORDER BY timestamp DESC LIMIT ?1 OFFSET ?2",
                base_select
            ),
            false,
        )
    } else {
        (
            format!(
                "{} WHERE (url LIKE ?3 OR method LIKE ?3 OR model LIKE ?3
                      OR CAST(status AS TEXT) LIKE ?3 OR account_email LIKE ?3 OR client_ip LIKE ?3)
                 ORDER BY timestamp DESC LIMIT ?1 OFFSET ?2",
                base_select
            ),
            true,
        )
    };

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;

    let map_row = |row: &rusqlite::Row| -> rusqlite::Result<ProxyRequestLog> {
        Ok(ProxyRequestLog {
            id: row.get(0)?,
            timestamp: row.get(1)?,
            method: row.get(2)?,
            url: row.get(3)?,
            status: row.get(4)?,
            duration: row.get(5)?,
            model: row.get(6)?,
            error: row.get(7)?,
            request_body: None,
            response_body: None,
            input_tokens: row.get(8)?,
            output_tokens: row.get(9)?,
            account_email: row.get(10)?,
            mapped_model: row.get(11)?,
            protocol: row.get(12)?,
            client_ip: row.get(13)?,
            username: row.get(14)?,
        })
    };

    let logs_iter = if use_filter {
        stmt.query_map(params![limit, offset, filter_pattern], map_row)
    } else {
        stmt.query_map(params![limit, offset], map_row)
    }
    .map_err(|e| e.to_string())?;

    let mut logs = Vec::new();
    for log in logs_iter {
        logs.push(log.map_err(|e| e.to_string())?);
    }
    Ok(logs)
}

/// Get paginated logs with filter support.
pub fn get_logs_paginated(
    page: usize,
    page_size: usize,
    filter: &str,
    errors_only: bool,
) -> Result<PaginatedLogs, String> {
    let conn = connect_db()?;
    get_logs_paginated_with_conn(&conn, page, page_size, filter, errors_only)
}

fn get_logs_paginated_with_conn(
    conn: &Connection,
    page: usize,
    page_size: usize,
    filter: &str,
    errors_only: bool,
) -> Result<PaginatedLogs, String> {
    let offset = page * page_size;
    let total = get_logs_count_filtered_with_conn(conn, filter, errors_only)?;
    let logs = get_logs_filtered_with_conn(conn, filter, errors_only, page_size, offset)?;

    Ok(PaginatedLogs {
        logs,
        total,
        page,
        page_size,
    })
}

// ============================================================================
// Log Detail
// ============================================================================

/// Get single log detail with full request_body and response_body.
pub fn get_log_detail(log_id: &str) -> Result<ProxyRequestLog, String> {
    let conn = connect_db()?;
    get_log_detail_with_conn(&conn, log_id)
}

fn get_log_detail_with_conn(conn: &Connection, log_id: &str) -> Result<ProxyRequestLog, String> {
    conn.query_row(
        "SELECT id, timestamp, method, url, status, duration, model, error,
                request_body, response_body, input_tokens, output_tokens,
                account_email, mapped_model, protocol, client_ip, username
         FROM proxy_logs WHERE id = ?1",
        params![log_id],
        |row| {
            Ok(ProxyRequestLog {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                method: row.get(2)?,
                url: row.get(3)?,
                status: row.get(4)?,
                duration: row.get(5)?,
                model: row.get(6)?,
                error: row.get(7)?,
                request_body: row.get(8)?,
                response_body: row.get(9)?,
                input_tokens: row.get(10)?,
                output_tokens: row.get(11)?,
                account_email: row.get(12)?,
                mapped_model: row.get(13)?,
                protocol: row.get(14)?,
                client_ip: row.get(15)?,
                username: row.get(16)?,
            })
        },
    )
    .map_err(|e| e.to_string())
}

// ============================================================================
// Debug Logs - Full chain save/query
// ============================================================================

/// Save a debug log entry (full request/response chain).
pub fn save_debug_log(entry: &DebugLogEntry) -> Result<(), String> {
    let conn = connect_db()?;
    save_debug_log_with_conn(&conn, entry)
}

fn save_debug_log_with_conn(conn: &Connection, entry: &DebugLogEntry) -> Result<(), String> {
    // Debug logs are stored in the same proxy_logs table with full bodies.
    // The entry is a ProxyRequestLog with request_body and response_body populated.
    conn.execute(
        "INSERT OR REPLACE INTO proxy_logs (id, timestamp, method, url, status, duration, model,
         mapped_model, account_email, client_ip, error, request_body, response_body,
         input_tokens, output_tokens, protocol, username)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            entry.id,
            entry.timestamp,
            entry.method,
            entry.url,
            entry.status,
            entry.duration,
            entry.model,
            entry.mapped_model,
            entry.account_email,
            entry.client_ip,
            entry.error,
            entry.request_body,
            entry.response_body,
            entry.input_tokens,
            entry.output_tokens,
            entry.protocol,
            entry.username,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Get all logs with full details for export.
pub fn get_all_logs_for_export() -> Result<Vec<ProxyRequestLog>, String> {
    let conn = connect_db()?;
    get_all_logs_for_export_with_conn(&conn)
}

fn get_all_logs_for_export_with_conn(conn: &Connection) -> Result<Vec<ProxyRequestLog>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, timestamp, method, url, status, duration, model, error,
                    request_body, response_body, input_tokens, output_tokens,
                    account_email, mapped_model, protocol, client_ip, username
             FROM proxy_logs ORDER BY timestamp DESC",
        )
        .map_err(|e| e.to_string())?;

    let logs_iter = stmt
        .query_map([], |row| {
            Ok(ProxyRequestLog {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                method: row.get(2)?,
                url: row.get(3)?,
                status: row.get(4)?,
                duration: row.get(5)?,
                model: row.get(6)?,
                error: row.get(7)?,
                request_body: row.get(8)?,
                response_body: row.get(9)?,
                input_tokens: row.get(10)?,
                output_tokens: row.get(11)?,
                account_email: row.get(12)?,
                mapped_model: row.get(13)?,
                protocol: row.get(14)?,
                client_ip: row.get(15)?,
                username: row.get(16)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut logs = Vec::new();
    for log in logs_iter {
        logs.push(log.map_err(|e| e.to_string())?);
    }
    Ok(logs)
}

// ============================================================================
// IP Token Usage Statistics
// ============================================================================

/// Get token usage grouped by IP address.
pub fn get_token_usage_by_ip(limit: usize, hours: i64) -> Result<Vec<IpTokenStats>, String> {
    let conn = connect_db()?;
    get_token_usage_by_ip_with_conn(&conn, limit, hours)
}

fn get_token_usage_by_ip_with_conn(
    conn: &Connection,
    limit: usize,
    hours: i64,
) -> Result<Vec<IpTokenStats>, String> {
    let since = chrono::Utc::now().timestamp() - (hours * 3600);

    let mut stmt = conn
        .prepare(
            "SELECT
                client_ip,
                COALESCE(SUM(input_tokens), 0) + COALESCE(SUM(output_tokens), 0) as total,
                COALESCE(SUM(input_tokens), 0) as input,
                COALESCE(SUM(output_tokens), 0) as output,
                COUNT(*) as cnt
             FROM proxy_logs
             WHERE timestamp >= ?1 AND client_ip IS NOT NULL AND client_ip != ''
             GROUP BY client_ip
             ORDER BY total DESC
             LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map(params![since, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut stats = Vec::new();
    for row in rows {
        let (client_ip, total_tokens, input_tokens, output_tokens, request_count) =
            row.map_err(|e| e.to_string())?;

        stats.push(IpTokenStats {
            client_ip,
            total_tokens,
            input_tokens,
            output_tokens,
            request_count,
            username: None, // Caller can enrich from user_token_db if needed
        });
    }
    Ok(stats)
}

/// Limit maximum log count (keep newest N records).
pub fn limit_max_logs(max_count: usize) -> Result<usize, String> {
    let conn = connect_db()?;
    limit_max_logs_with_conn(&conn, max_count)
}

fn limit_max_logs_with_conn(conn: &Connection, max_count: usize) -> Result<usize, String> {
    let deleted = conn
        .execute(
            "DELETE FROM proxy_logs WHERE id NOT IN (
                SELECT id FROM proxy_logs ORDER BY timestamp DESC LIMIT ?1
            )",
            params![max_count],
        )
        .map_err(|e| e.to_string())?;
    Ok(deleted)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db_with_conn(&conn).unwrap();
        conn
    }

    fn make_log(id: &str, status: u16, model: Option<&str>) -> ProxyRequestLog {
        ProxyRequestLog {
            id: id.to_string(),
            timestamp: chrono::Utc::now().timestamp(),
            method: "POST".to_string(),
            url: "/v1/chat/completions".to_string(),
            status,
            duration: 150,
            model: model.map(|s| s.to_string()),
            mapped_model: Some("gemini-2.5-flash".to_string()),
            account_email: Some("test@example.com".to_string()),
            client_ip: Some("127.0.0.1".to_string()),
            error: if status >= 400 {
                Some("error".to_string())
            } else {
                None
            },
            request_body: Some(r#"{"model":"gpt-4"}"#.to_string()),
            response_body: Some(r#"{"choices":[]}"#.to_string()),
            input_tokens: Some(100),
            output_tokens: Some(200),
            protocol: Some("openai".to_string()),
            username: None,
        }
    }

    fn insert_log(conn: &Connection, log: &ProxyRequestLog) {
        conn.execute(
            "INSERT INTO proxy_logs (id, timestamp, method, url, status, duration, model,
             mapped_model, account_email, client_ip, error, request_body, response_body,
             input_tokens, output_tokens, protocol, username)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                log.id, log.timestamp, log.method, log.url, log.status, log.duration,
                log.model, log.mapped_model, log.account_email, log.client_ip,
                log.error, log.request_body, log.response_body, log.input_tokens,
                log.output_tokens, log.protocol, log.username,
            ],
        )
        .unwrap();
    }

    // ── Init ──

    #[test]
    fn test_init_db_creates_table_and_indexes() {
        let conn = setup_test_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM proxy_logs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_init_db_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init_db_with_conn(&conn).unwrap();
        init_db_with_conn(&conn).unwrap(); // second call should not fail
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM proxy_logs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    // ── Pagination ──

    #[test]
    fn test_get_logs_summary_empty() {
        let conn = setup_test_db();
        let logs = get_logs_summary_with_conn(&conn, 10, 0).unwrap();
        assert!(logs.is_empty());
    }

    #[test]
    fn test_get_logs_summary_pagination() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();
        for i in 0..5 {
            let mut log = make_log(&format!("page-{}", i), 200, Some("gpt-4"));
            log.timestamp = now + i as i64; // ensure ordering
            insert_log(&conn, &log);
        }

        // Page 0, size 2
        let page0 = get_logs_summary_with_conn(&conn, 2, 0).unwrap();
        assert_eq!(page0.len(), 2);
        assert_eq!(page0[0].id, "page-4"); // newest first

        // Page 1, size 2
        let page1 = get_logs_summary_with_conn(&conn, 2, 2).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].id, "page-2");

        // Page 2, size 2 (only 1 remaining)
        let page2 = get_logs_summary_with_conn(&conn, 2, 4).unwrap();
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].id, "page-0");
    }

    #[test]
    fn test_get_logs_summary_excludes_bodies() {
        let conn = setup_test_db();
        insert_log(&conn, &make_log("body-1", 200, Some("gpt-4")));

        let logs = get_logs_summary_with_conn(&conn, 10, 0).unwrap();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].request_body.is_none());
        assert!(logs[0].response_body.is_none());
    }

    // ── Filtered Count ──

    #[test]
    fn test_count_filtered_empty() {
        let conn = setup_test_db();
        let count = get_logs_count_filtered_with_conn(&conn, "", false).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_count_filtered_errors_only() {
        let conn = setup_test_db();
        insert_log(&conn, &make_log("ok-1", 200, Some("gpt-4")));
        insert_log(&conn, &make_log("err-1", 500, Some("gpt-4")));
        insert_log(&conn, &make_log("err-2", 429, Some("gpt-4")));

        let count = get_logs_count_filtered_with_conn(&conn, "", true).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_count_filtered_by_text() {
        let conn = setup_test_db();
        let mut log1 = make_log("f-1", 200, Some("gpt-4"));
        log1.client_ip = Some("192.168.1.1".to_string());
        insert_log(&conn, &log1);

        let mut log2 = make_log("f-2", 200, Some("claude-3"));
        log2.client_ip = Some("10.0.0.1".to_string());
        insert_log(&conn, &log2);

        // Filter by model
        let count = get_logs_count_filtered_with_conn(&conn, "claude", false).unwrap();
        assert_eq!(count, 1);

        // Filter by IP
        let count = get_logs_count_filtered_with_conn(&conn, "192.168", false).unwrap();
        assert_eq!(count, 1);

        // No match
        let count = get_logs_count_filtered_with_conn(&conn, "nonexistent", false).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_count_filtered_errors_with_text() {
        let conn = setup_test_db();
        insert_log(&conn, &make_log("ef-1", 500, Some("gpt-4")));
        insert_log(&conn, &make_log("ef-2", 429, Some("claude-3")));
        insert_log(&conn, &make_log("ef-3", 200, Some("gpt-4")));

        let count = get_logs_count_filtered_with_conn(&conn, "gpt", true).unwrap();
        assert_eq!(count, 1); // only ef-1 matches (error + gpt)
    }

    // ── Filtered Logs ──

    #[test]
    fn test_get_logs_filtered_no_filter() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();
        for i in 0..3 {
            let mut log = make_log(&format!("nf-{}", i), 200, Some("gpt-4"));
            log.timestamp = now + i as i64;
            insert_log(&conn, &log);
        }

        let logs = get_logs_filtered_with_conn(&conn, "", false, 10, 0).unwrap();
        assert_eq!(logs.len(), 3);
    }

    #[test]
    fn test_get_logs_filtered_by_model() {
        let conn = setup_test_db();
        insert_log(&conn, &make_log("m-1", 200, Some("gpt-4")));
        insert_log(&conn, &make_log("m-2", 200, Some("claude-3")));
        insert_log(&conn, &make_log("m-3", 200, Some("gpt-4o")));

        let logs = get_logs_filtered_with_conn(&conn, "gpt", false, 10, 0).unwrap();
        assert_eq!(logs.len(), 2);
    }

    #[test]
    fn test_get_logs_filtered_errors_only() {
        let conn = setup_test_db();
        insert_log(&conn, &make_log("eo-1", 200, Some("gpt-4")));
        insert_log(&conn, &make_log("eo-2", 500, Some("gpt-4")));
        insert_log(&conn, &make_log("eo-3", 429, Some("gpt-4")));

        let logs = get_logs_filtered_with_conn(&conn, "", true, 10, 0).unwrap();
        assert_eq!(logs.len(), 2);
    }

    // ── Paginated ──

    #[test]
    fn test_get_logs_paginated() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();
        for i in 0..7 {
            let mut log = make_log(&format!("pg-{}", i), 200, Some("gpt-4"));
            log.timestamp = now + i as i64;
            insert_log(&conn, &log);
        }

        let result = get_logs_paginated_with_conn(&conn, 0, 3, "", false).unwrap();
        assert_eq!(result.total, 7);
        assert_eq!(result.page, 0);
        assert_eq!(result.page_size, 3);
        assert_eq!(result.logs.len(), 3);
        assert_eq!(result.logs[0].id, "pg-6");

        let result = get_logs_paginated_with_conn(&conn, 2, 3, "", false).unwrap();
        assert_eq!(result.logs.len(), 1); // 7 - 6 = 1 remaining
        assert_eq!(result.logs[0].id, "pg-0");
    }

    #[test]
    fn test_get_logs_paginated_with_filter() {
        let conn = setup_test_db();
        insert_log(&conn, &make_log("pf-1", 200, Some("gpt-4")));
        insert_log(&conn, &make_log("pf-2", 500, Some("claude-3")));
        insert_log(&conn, &make_log("pf-3", 200, Some("gpt-4o")));

        let result = get_logs_paginated_with_conn(&conn, 0, 10, "gpt", false).unwrap();
        assert_eq!(result.total, 2);
        assert_eq!(result.logs.len(), 2);
    }

    // ── Log Detail ──

    #[test]
    fn test_get_log_detail() {
        let conn = setup_test_db();
        insert_log(&conn, &make_log("detail-1", 200, Some("gpt-4")));

        let detail = get_log_detail_with_conn(&conn, "detail-1").unwrap();
        assert_eq!(detail.id, "detail-1");
        assert_eq!(detail.status, 200);
        assert!(detail.request_body.is_some()); // detail includes bodies
        assert!(detail.response_body.is_some());
        assert_eq!(detail.model, Some("gpt-4".to_string()));
    }

    #[test]
    fn test_get_log_detail_not_found() {
        let conn = setup_test_db();
        let result = get_log_detail_with_conn(&conn, "nonexistent");
        assert!(result.is_err());
    }

    // ── Debug Logs ──

    #[test]
    fn test_save_debug_log() {
        let conn = setup_test_db();
        let entry = DebugLogEntry {
            id: "debug-1".to_string(),
            timestamp: chrono::Utc::now().timestamp(),
            method: "POST".to_string(),
            url: "/v1/chat/completions".to_string(),
            status: 200,
            duration: 500,
            model: Some("gpt-4".to_string()),
            mapped_model: Some("gemini-2.5-pro".to_string()),
            account_email: Some("debug@example.com".to_string()),
            client_ip: Some("10.0.0.1".to_string()),
            error: None,
            request_body: Some(r#"{"messages":[{"role":"user","content":"hello"}]}"#.to_string()),
            response_body: Some(r#"{"choices":[{"message":{"content":"hi"}}]}"#.to_string()),
            input_tokens: Some(10),
            output_tokens: Some(5),
            protocol: Some("openai".to_string()),
            username: Some("admin".to_string()),
        };

        save_debug_log_with_conn(&conn, &entry).unwrap();

        let detail = get_log_detail_with_conn(&conn, "debug-1").unwrap();
        assert_eq!(detail.id, "debug-1");
        assert!(detail.request_body.is_some());
        assert!(detail.response_body.is_some());
        assert_eq!(detail.username, Some("admin".to_string()));
    }

    #[test]
    fn test_save_debug_log_upsert() {
        let conn = setup_test_db();
        let entry = DebugLogEntry {
            id: "upsert-1".to_string(),
            timestamp: 1000,
            method: "POST".to_string(),
            url: "/v1/messages".to_string(),
            status: 200,
            duration: 100,
            model: Some("claude-3".to_string()),
            mapped_model: None,
            account_email: None,
            client_ip: None,
            error: None,
            request_body: Some("original".to_string()),
            response_body: None,
            input_tokens: None,
            output_tokens: None,
            protocol: Some("claude".to_string()),
            username: None,
        };
        save_debug_log_with_conn(&conn, &entry).unwrap();

        // Update with new body
        let mut updated = entry.clone();
        updated.request_body = Some("updated".to_string());
        updated.status = 500;
        save_debug_log_with_conn(&conn, &updated).unwrap();

        let detail = get_log_detail_with_conn(&conn, "upsert-1").unwrap();
        assert_eq!(detail.request_body, Some("updated".to_string()));
        assert_eq!(detail.status, 500);

        // Should still be only 1 record
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM proxy_logs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    // ── Export ──

    #[test]
    fn test_get_all_logs_for_export() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();
        for i in 0..3 {
            let mut log = make_log(&format!("exp-{}", i), 200, Some("gpt-4"));
            log.timestamp = now + i as i64;
            insert_log(&conn, &log);
        }

        let logs = get_all_logs_for_export_with_conn(&conn).unwrap();
        assert_eq!(logs.len(), 3);
        // Export includes bodies
        assert!(logs[0].request_body.is_some());
        assert!(logs[0].response_body.is_some());
        // Ordered by timestamp DESC
        assert_eq!(logs[0].id, "exp-2");
    }

    #[test]
    fn test_get_all_logs_for_export_empty() {
        let conn = setup_test_db();
        let logs = get_all_logs_for_export_with_conn(&conn).unwrap();
        assert!(logs.is_empty());
    }

    // ── IP Token Stats ──

    #[test]
    fn test_get_token_usage_by_ip() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();

        // Insert logs from different IPs
        let mut log1 = make_log("ip-1", 200, Some("gpt-4"));
        log1.timestamp = now;
        log1.client_ip = Some("192.168.1.1".to_string());
        log1.input_tokens = Some(100);
        log1.output_tokens = Some(200);
        insert_log(&conn, &log1);

        let mut log2 = make_log("ip-2", 200, Some("gpt-4"));
        log2.timestamp = now;
        log2.client_ip = Some("192.168.1.1".to_string());
        log2.input_tokens = Some(50);
        log2.output_tokens = Some(100);
        insert_log(&conn, &log2);

        let mut log3 = make_log("ip-3", 200, Some("gpt-4"));
        log3.timestamp = now;
        log3.client_ip = Some("10.0.0.1".to_string());
        log3.input_tokens = Some(30);
        log3.output_tokens = Some(40);
        insert_log(&conn, &log3);

        let stats = get_token_usage_by_ip_with_conn(&conn, 10, 24).unwrap();
        assert_eq!(stats.len(), 2);
        // 192.168.1.1 has more tokens, should be first
        assert_eq!(stats[0].client_ip, "192.168.1.1");
        assert_eq!(stats[0].total_tokens, 450); // 100+200+50+100
        assert_eq!(stats[0].input_tokens, 150);
        assert_eq!(stats[0].output_tokens, 300);
        assert_eq!(stats[0].request_count, 2);

        assert_eq!(stats[1].client_ip, "10.0.0.1");
        assert_eq!(stats[1].total_tokens, 70);
        assert_eq!(stats[1].request_count, 1);
    }

    #[test]
    fn test_get_token_usage_by_ip_excludes_old() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();

        // Old log (48 hours ago)
        let mut old_log = make_log("old-ip", 200, Some("gpt-4"));
        old_log.timestamp = now - (48 * 3600);
        old_log.client_ip = Some("1.2.3.4".to_string());
        insert_log(&conn, &old_log);

        // Recent log
        let mut recent_log = make_log("new-ip", 200, Some("gpt-4"));
        recent_log.timestamp = now;
        recent_log.client_ip = Some("5.6.7.8".to_string());
        insert_log(&conn, &recent_log);

        let stats = get_token_usage_by_ip_with_conn(&conn, 10, 24).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].client_ip, "5.6.7.8");
    }

    #[test]
    fn test_get_token_usage_by_ip_excludes_null_ip() {
        let conn = setup_test_db();
        let mut log = make_log("null-ip", 200, Some("gpt-4"));
        log.client_ip = None;
        insert_log(&conn, &log);

        let stats = get_token_usage_by_ip_with_conn(&conn, 10, 24).unwrap();
        assert!(stats.is_empty());
    }

    // ── Limit Max Logs ──

    #[test]
    fn test_limit_max_logs() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();
        for i in 0..10 {
            let mut log = make_log(&format!("lim-{}", i), 200, Some("gpt-4"));
            log.timestamp = now + i as i64;
            insert_log(&conn, &log);
        }

        let deleted = limit_max_logs_with_conn(&conn, 5).unwrap();
        assert_eq!(deleted, 5);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM proxy_logs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 5);

        // Verify newest 5 remain
        let logs = get_logs_summary_with_conn(&conn, 10, 0).unwrap();
        assert_eq!(logs[0].id, "lim-9");
        assert_eq!(logs[4].id, "lim-5");
    }

    #[test]
    fn test_limit_max_logs_no_delete_needed() {
        let conn = setup_test_db();
        for i in 0..3 {
            insert_log(&conn, &make_log(&format!("nd-{}", i), 200, Some("gpt-4")));
        }

        let deleted = limit_max_logs_with_conn(&conn, 10).unwrap();
        assert_eq!(deleted, 0);
    }

    // ── Serialization ──

    #[test]
    fn test_paginated_logs_serialization() {
        let result = PaginatedLogs {
            logs: vec![],
            total: 42,
            page: 2,
            page_size: 10,
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: PaginatedLogs = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.total, 42);
        assert_eq!(deserialized.page, 2);
        assert_eq!(deserialized.page_size, 10);
    }

    #[test]
    fn test_ip_token_stats_serialization() {
        let stats = IpTokenStats {
            client_ip: "127.0.0.1".to_string(),
            total_tokens: 1000,
            input_tokens: 400,
            output_tokens: 600,
            request_count: 5,
            username: Some("admin".to_string()),
        };
        let json = serde_json::to_string(&stats).unwrap();
        let deserialized: IpTokenStats = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.client_ip, "127.0.0.1");
        assert_eq!(deserialized.total_tokens, 1000);
        assert_eq!(deserialized.username, Some("admin".to_string()));
    }

    #[test]
    fn test_debug_log_entry_serialization() {
        let entry = DebugLogEntry {
            id: "ser-1".to_string(),
            timestamp: 1000,
            method: "POST".to_string(),
            url: "/test".to_string(),
            status: 200,
            duration: 50,
            model: None,
            mapped_model: None,
            account_email: None,
            client_ip: None,
            error: None,
            request_body: None,
            response_body: None,
            input_tokens: None,
            output_tokens: None,
            protocol: None,
            username: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: DebugLogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "ser-1");
    }
}
