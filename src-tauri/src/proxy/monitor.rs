//! Proxy Monitor - Core monitoring data structures and persistence
//!
//! Provides ProxyRequestLog recording, database persistence to proxy.db,
//! statistics tracking (total/success/error), and automatic cleanup (30 days).

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::RwLock;

// ============================================================================
// Data Structures
// ============================================================================

/// 代理请求日志
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyRequestLog {
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

/// 代理统计数据
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyStats {
    pub total_requests: u64,
    pub success_count: u64,
    pub error_count: u64,
}

// ============================================================================
// Database Layer
// ============================================================================

/// 获取 proxy.db 路径
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

/// 初始化 proxy.db 数据库
pub fn init_db() -> Result<(), String> {
    let conn = connect_db()?;
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

    Ok(())
}

/// 保存请求日志到数据库
pub fn save_log(log: &ProxyRequestLog) -> Result<(), String> {
    let conn = connect_db()?;
    conn.execute(
        "INSERT INTO proxy_logs (id, timestamp, method, url, status, duration, model, mapped_model,
         account_email, client_ip, error, request_body, response_body, input_tokens, output_tokens,
         protocol, username)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            log.id,
            log.timestamp,
            log.method,
            log.url,
            log.status,
            log.duration,
            log.model,
            log.mapped_model,
            log.account_email,
            log.client_ip,
            log.error,
            log.request_body,
            log.response_body,
            log.input_tokens,
            log.output_tokens,
            log.protocol,
            log.username,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 从数据库获取统计数据
pub fn get_stats_from_db() -> Result<ProxyStats, String> {
    let conn = connect_db()?;
    let (total_requests, success_count, error_count): (u64, u64, u64) = conn
        .query_row(
            "SELECT
                COUNT(*) as total,
                COALESCE(SUM(CASE WHEN status >= 200 AND status < 400 THEN 1 ELSE 0 END), 0) as success,
                COALESCE(SUM(CASE WHEN status < 200 OR status >= 400 THEN 1 ELSE 0 END), 0) as error
             FROM proxy_logs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|e| e.to_string())?;

    Ok(ProxyStats {
        total_requests,
        success_count,
        error_count,
    })
}

/// 清理超过指定天数的旧日志
pub fn cleanup_old_logs(days: i64) -> Result<usize, String> {
    let conn = connect_db()?;
    let cutoff_timestamp = chrono::Utc::now().timestamp() - (days * 24 * 3600);
    let deleted = conn
        .execute(
            "DELETE FROM proxy_logs WHERE timestamp < ?1",
            [cutoff_timestamp],
        )
        .map_err(|e| e.to_string())?;
    if deleted > 0 {
        conn.execute("VACUUM", []).map_err(|e| e.to_string())?;
    }
    Ok(deleted)
}

/// 清空所有日志
pub fn clear_logs() -> Result<(), String> {
    let conn = connect_db()?;
    conn.execute("DELETE FROM proxy_logs", [])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 获取日志总数
pub fn get_logs_count() -> Result<u64, String> {
    let conn = connect_db()?;
    let count: u64 = conn
        .query_row("SELECT COUNT(*) FROM proxy_logs", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    Ok(count)
}

// ============================================================================
// Internal DB helpers (for testable operations with injected connection)
// ============================================================================

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
    Ok(())
}

fn save_log_with_conn(conn: &Connection, log: &ProxyRequestLog) -> Result<(), String> {
    conn.execute(
        "INSERT INTO proxy_logs (id, timestamp, method, url, status, duration, model, mapped_model,
         account_email, client_ip, error, request_body, response_body, input_tokens, output_tokens,
         protocol, username)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            log.id,
            log.timestamp,
            log.method,
            log.url,
            log.status,
            log.duration,
            log.model,
            log.mapped_model,
            log.account_email,
            log.client_ip,
            log.error,
            log.request_body,
            log.response_body,
            log.input_tokens,
            log.output_tokens,
            log.protocol,
            log.username,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn get_stats_with_conn(conn: &Connection) -> Result<ProxyStats, String> {
    let (total_requests, success_count, error_count): (u64, u64, u64) = conn
        .query_row(
            "SELECT
                COUNT(*) as total,
                COALESCE(SUM(CASE WHEN status >= 200 AND status < 400 THEN 1 ELSE 0 END), 0) as success,
                COALESCE(SUM(CASE WHEN status < 200 OR status >= 400 THEN 1 ELSE 0 END), 0) as error
             FROM proxy_logs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|e| e.to_string())?;

    Ok(ProxyStats {
        total_requests,
        success_count,
        error_count,
    })
}

fn cleanup_old_logs_with_conn(conn: &Connection, days: i64) -> Result<usize, String> {
    let cutoff_timestamp = chrono::Utc::now().timestamp() - (days * 24 * 3600);
    let deleted = conn
        .execute(
            "DELETE FROM proxy_logs WHERE timestamp < ?1",
            [cutoff_timestamp],
        )
        .map_err(|e| e.to_string())?;
    Ok(deleted)
}

// ============================================================================
// ProxyMonitor - Core monitor component
// ============================================================================

/// 代理监控器
///
/// 管理请求日志的内存缓存和数据库持久化，提供统计数据和自动清理。
pub struct ProxyMonitor {
    logs: RwLock<VecDeque<ProxyRequestLog>>,
    stats: RwLock<ProxyStats>,
    max_logs: usize,
    enabled: AtomicBool,
}

impl ProxyMonitor {
    /// 创建新的 ProxyMonitor 实例
    ///
    /// 初始化数据库并自动清理超过 30 天的旧日志。
    pub fn new(max_logs: usize) -> Self {
        if let Err(e) = init_db() {
            tracing::error!("Failed to initialize proxy DB: {}", e);
        }

        // Auto cleanup old logs (>30 days)
        tokio::spawn(async {
            match cleanup_old_logs(30) {
                Ok(deleted) => {
                    if deleted > 0 {
                        tracing::info!("Auto cleanup: removed {} old proxy logs (>30 days)", deleted);
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to cleanup old proxy logs: {}", e);
                }
            }
        });

        Self {
            logs: RwLock::new(VecDeque::with_capacity(max_logs)),
            stats: RwLock::new(ProxyStats::default()),
            max_logs,
            enabled: AtomicBool::new(false),
        }
    }

    /// 设置监控启用状态
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// 获取监控启用状态
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// 记录一条请求日志
    ///
    /// 更新内存统计、内存日志缓存，并异步持久化到数据库。
    pub async fn log_request(&self, log: ProxyRequestLog) {
        if !self.is_enabled() {
            return;
        }

        // Update in-memory stats
        {
            let mut stats = self.stats.write().await;
            stats.total_requests += 1;
            if log.status >= 200 && log.status < 400 {
                stats.success_count += 1;
            } else {
                stats.error_count += 1;
            }
        }

        // Add to in-memory log buffer
        {
            let mut logs = self.logs.write().await;
            if logs.len() >= self.max_logs {
                logs.pop_back();
            }
            logs.push_front(log.clone());
        }

        // Persist to DB asynchronously
        let log_to_save = log;
        tokio::spawn(async move {
            if let Err(e) = save_log(&log_to_save) {
                tracing::error!("Failed to save proxy log to DB: {}", e);
            }
        });
    }

    /// 获取统计数据（优先从数据库获取）
    pub async fn get_stats(&self) -> ProxyStats {
        let db_result =
            tokio::task::spawn_blocking(get_stats_from_db).await;

        match db_result {
            Ok(Ok(stats)) => stats,
            Ok(Err(e)) => {
                tracing::error!("Failed to get stats from DB: {}", e);
                self.stats.read().await.clone()
            }
            Err(e) => {
                tracing::error!("Spawn blocking failed for get_stats: {}", e);
                self.stats.read().await.clone()
            }
        }
    }

    /// 获取最近的日志（从内存缓存）
    pub async fn get_recent_logs(&self, limit: usize) -> Vec<ProxyRequestLog> {
        let logs = self.logs.read().await;
        logs.iter().take(limit).cloned().collect()
    }

    /// 清空所有日志和统计
    pub async fn clear(&self) {
        {
            let mut logs = self.logs.write().await;
            logs.clear();
        }
        {
            let mut stats = self.stats.write().await;
            *stats = ProxyStats::default();
        }

        let _ = tokio::task::spawn_blocking(|| {
            if let Err(e) = clear_logs() {
                tracing::error!("Failed to clear proxy logs in DB: {}", e);
            }
        })
        .await;
    }
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

    fn make_log(id: &str, status: u16) -> ProxyRequestLog {
        ProxyRequestLog {
            id: id.to_string(),
            timestamp: chrono::Utc::now().timestamp(),
            method: "POST".to_string(),
            url: "/v1/chat/completions".to_string(),
            status,
            duration: 150,
            model: Some("gpt-4".to_string()),
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

    // ── ProxyRequestLog serialization ──

    #[test]
    fn test_proxy_request_log_serialization() {
        let log = make_log("log-1", 200);
        let json = serde_json::to_string(&log).unwrap();
        let deserialized: ProxyRequestLog = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "log-1");
        assert_eq!(deserialized.status, 200);
        assert_eq!(deserialized.model, Some("gpt-4".to_string()));
        assert_eq!(deserialized.input_tokens, Some(100));
    }

    #[test]
    fn test_proxy_request_log_optional_fields() {
        let log = ProxyRequestLog {
            id: "log-2".to_string(),
            timestamp: 1000,
            method: "GET".to_string(),
            url: "/v1/models".to_string(),
            status: 200,
            duration: 10,
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
        let json = serde_json::to_string(&log).unwrap();
        let deserialized: ProxyRequestLog = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "log-2");
        assert!(deserialized.model.is_none());
    }

    // ── ProxyStats ──

    #[test]
    fn test_proxy_stats_default() {
        let stats = ProxyStats::default();
        assert_eq!(stats.total_requests, 0);
        assert_eq!(stats.success_count, 0);
        assert_eq!(stats.error_count, 0);
    }

    #[test]
    fn test_proxy_stats_serialization() {
        let stats = ProxyStats {
            total_requests: 100,
            success_count: 90,
            error_count: 10,
        };
        let json = serde_json::to_string(&stats).unwrap();
        let deserialized: ProxyStats = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.total_requests, 100);
        assert_eq!(deserialized.success_count, 90);
        assert_eq!(deserialized.error_count, 10);
    }

    // ── Database operations ──

    #[test]
    fn test_init_db_creates_table() {
        let conn = setup_test_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM proxy_logs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_save_and_query_log() {
        let conn = setup_test_db();
        let log = make_log("save-1", 200);
        save_log_with_conn(&conn, &log).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM proxy_logs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let (id, status, model): (String, u16, Option<String>) = conn
            .query_row(
                "SELECT id, status, model FROM proxy_logs WHERE id = ?1",
                ["save-1"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(id, "save-1");
        assert_eq!(status, 200);
        assert_eq!(model, Some("gpt-4".to_string()));
    }

    #[test]
    fn test_save_log_all_fields() {
        let conn = setup_test_db();
        let log = make_log("full-1", 200);
        save_log_with_conn(&conn, &log).unwrap();

        let (mapped_model, email, ip, protocol, input, output): (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<u32>,
            Option<u32>,
        ) = conn
            .query_row(
                "SELECT mapped_model, account_email, client_ip, protocol, input_tokens, output_tokens
                 FROM proxy_logs WHERE id = ?1",
                ["full-1"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .unwrap();
        assert_eq!(mapped_model, Some("gemini-2.5-flash".to_string()));
        assert_eq!(email, Some("test@example.com".to_string()));
        assert_eq!(ip, Some("127.0.0.1".to_string()));
        assert_eq!(protocol, Some("openai".to_string()));
        assert_eq!(input, Some(100));
        assert_eq!(output, Some(200));
    }

    #[test]
    fn test_save_multiple_logs() {
        let conn = setup_test_db();
        for i in 0..5 {
            let log = make_log(&format!("multi-{}", i), if i % 2 == 0 { 200 } else { 500 });
            save_log_with_conn(&conn, &log).unwrap();
        }

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM proxy_logs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 5);
    }

    // ── Stats from DB ──

    #[test]
    fn test_stats_empty_db() {
        let conn = setup_test_db();
        let stats = get_stats_with_conn(&conn).unwrap();
        assert_eq!(stats.total_requests, 0);
        assert_eq!(stats.success_count, 0);
        assert_eq!(stats.error_count, 0);
    }

    #[test]
    fn test_stats_with_mixed_statuses() {
        let conn = setup_test_db();

        // 3 success (200, 201, 301) + 2 errors (400, 500)
        let statuses = [200u16, 201, 301, 400, 500];
        for (i, &status) in statuses.iter().enumerate() {
            let log = make_log(&format!("stat-{}", i), status);
            save_log_with_conn(&conn, &log).unwrap();
        }

        let stats = get_stats_with_conn(&conn).unwrap();
        assert_eq!(stats.total_requests, 5);
        assert_eq!(stats.success_count, 3); // 200, 201, 301
        assert_eq!(stats.error_count, 2); // 400, 500
    }

    #[test]
    fn test_stats_all_success() {
        let conn = setup_test_db();
        for i in 0..3 {
            save_log_with_conn(&conn, &make_log(&format!("ok-{}", i), 200)).unwrap();
        }
        let stats = get_stats_with_conn(&conn).unwrap();
        assert_eq!(stats.total_requests, 3);
        assert_eq!(stats.success_count, 3);
        assert_eq!(stats.error_count, 0);
    }

    #[test]
    fn test_stats_all_errors() {
        let conn = setup_test_db();
        for i in 0..3 {
            save_log_with_conn(&conn, &make_log(&format!("err-{}", i), 500)).unwrap();
        }
        let stats = get_stats_with_conn(&conn).unwrap();
        assert_eq!(stats.total_requests, 3);
        assert_eq!(stats.success_count, 0);
        assert_eq!(stats.error_count, 3);
    }

    // ── Cleanup ──

    #[test]
    fn test_cleanup_old_logs() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();

        // Insert an old log (40 days ago)
        let mut old_log = make_log("old-1", 200);
        old_log.timestamp = now - (40 * 24 * 3600);
        save_log_with_conn(&conn, &old_log).unwrap();

        // Insert a recent log
        let recent_log = make_log("recent-1", 200);
        save_log_with_conn(&conn, &recent_log).unwrap();

        let deleted = cleanup_old_logs_with_conn(&conn, 30).unwrap();
        assert_eq!(deleted, 1);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM proxy_logs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Verify the remaining log is the recent one
        let remaining_id: String = conn
            .query_row("SELECT id FROM proxy_logs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining_id, "recent-1");
    }

    #[test]
    fn test_cleanup_no_old_logs() {
        let conn = setup_test_db();
        save_log_with_conn(&conn, &make_log("new-1", 200)).unwrap();

        let deleted = cleanup_old_logs_with_conn(&conn, 30).unwrap();
        assert_eq!(deleted, 0);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM proxy_logs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_cleanup_all_old_logs() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();

        for i in 0..3 {
            let mut log = make_log(&format!("ancient-{}", i), 200);
            log.timestamp = now - (60 * 24 * 3600); // 60 days ago
            save_log_with_conn(&conn, &log).unwrap();
        }

        let deleted = cleanup_old_logs_with_conn(&conn, 30).unwrap();
        assert_eq!(deleted, 3);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM proxy_logs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    // ── Edge cases ──

    #[test]
    fn test_save_log_with_null_optional_fields() {
        let conn = setup_test_db();
        let log = ProxyRequestLog {
            id: "null-fields".to_string(),
            timestamp: 1000,
            method: "GET".to_string(),
            url: "/healthz".to_string(),
            status: 200,
            duration: 1,
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
        save_log_with_conn(&conn, &log).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM proxy_logs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_duplicate_id_fails() {
        let conn = setup_test_db();
        let log = make_log("dup-1", 200);
        save_log_with_conn(&conn, &log).unwrap();

        let result = save_log_with_conn(&conn, &log);
        assert!(result.is_err());
    }

    #[test]
    fn test_boundary_status_codes() {
        let conn = setup_test_db();

        // 199 = error, 200 = success, 399 = success, 400 = error
        save_log_with_conn(&conn, &make_log("s-199", 199)).unwrap();
        save_log_with_conn(&conn, &make_log("s-200", 200)).unwrap();
        save_log_with_conn(&conn, &make_log("s-399", 399)).unwrap();
        save_log_with_conn(&conn, &make_log("s-400", 400)).unwrap();

        let stats = get_stats_with_conn(&conn).unwrap();
        assert_eq!(stats.total_requests, 4);
        assert_eq!(stats.success_count, 2); // 200, 399
        assert_eq!(stats.error_count, 2); // 199, 400
    }
}
