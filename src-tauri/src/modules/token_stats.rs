use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Aggregated token statistics for a time period
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStatsAggregated {
    pub period: String,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens: u64,
    pub request_count: u64,
}

/// Per-account token statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountTokenStats {
    pub account_email: String,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens: u64,
    pub request_count: u64,
}

/// Summary statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStatsSummary {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens: u64,
    pub total_requests: u64,
    pub unique_accounts: u64,
}

/// Per-model token statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTokenStats {
    pub model: String,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens: u64,
    pub request_count: u64,
}

/// Model trend data point (for stacked area chart)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTrendPoint {
    pub period: String,
    pub model_data: std::collections::HashMap<String, u64>,
}

/// Account trend data point (for stacked area chart)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountTrendPoint {
    pub period: String,
    pub account_data: std::collections::HashMap<String, u64>,
}

pub(crate) fn get_db_path() -> Result<PathBuf, String> {
    let data_dir = crate::modules::account::get_data_dir()?;
    Ok(data_dir.join("token_stats.db"))
}

fn connect_db() -> Result<Connection, String> {
    let db_path = get_db_path()?;
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "busy_timeout", 5000)
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| e.to_string())?;

    Ok(conn)
}

/// Initialize the token stats database
pub fn init_db() -> Result<(), String> {
    let conn = connect_db()?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS token_usage (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp INTEGER NOT NULL,
            account_email TEXT NOT NULL,
            model TEXT NOT NULL,
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL DEFAULT 0
        )",
        [],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_token_timestamp ON token_usage (timestamp DESC)",
        [],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_token_account ON token_usage (account_email)",
        [],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS token_stats_hourly (
            hour_bucket TEXT NOT NULL,
            account_email TEXT NOT NULL,
            total_input_tokens INTEGER NOT NULL DEFAULT 0,
            total_output_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL DEFAULT 0,
            request_count INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (hour_bucket, account_email)
        )",
        [],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// Record token usage from a request
pub fn record_usage(
    account_email: &str,
    model: &str,
    input_tokens: u32,
    output_tokens: u32,
) -> Result<(), String> {
    let conn = connect_db()?;
    let now = chrono::Utc::now();
    let timestamp = now.timestamp();
    let total_tokens = input_tokens + output_tokens;

    conn.execute(
        "INSERT INTO token_usage (timestamp, account_email, model, input_tokens, output_tokens, total_tokens)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![timestamp, account_email, model, input_tokens, output_tokens, total_tokens],
    )
    .map_err(|e| e.to_string())?;

    let hour_bucket = now.format("%Y-%m-%d %H:00").to_string();
    conn.execute(
        "INSERT INTO token_stats_hourly (hour_bucket, account_email, total_input_tokens, total_output_tokens, total_tokens, request_count)
         VALUES (?1, ?2, ?3, ?4, ?5, 1)
         ON CONFLICT(hour_bucket, account_email) DO UPDATE SET
            total_input_tokens = total_input_tokens + ?3,
            total_output_tokens = total_output_tokens + ?4,
            total_tokens = total_tokens + ?5,
            request_count = request_count + 1",
        params![hour_bucket, account_email, input_tokens, output_tokens, total_tokens],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// Get hourly aggregated stats for a time range
pub fn get_hourly_stats(hours: i64) -> Result<Vec<TokenStatsAggregated>, String> {
    let conn = connect_db()?;
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(hours);
    let cutoff_bucket = cutoff.format("%Y-%m-%d %H:00").to_string();

    let mut stmt = conn
        .prepare(
            "SELECT hour_bucket,
                SUM(total_input_tokens) as input,
                SUM(total_output_tokens) as output,
                SUM(total_tokens) as total,
                SUM(request_count) as count
             FROM token_stats_hourly
             WHERE hour_bucket >= ?1
             GROUP BY hour_bucket
             ORDER BY hour_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([cutoff_bucket], |row| {
            Ok(TokenStatsAggregated {
                period: row.get(0)?,
                total_input_tokens: row.get(1)?,
                total_output_tokens: row.get(2)?,
                total_tokens: row.get(3)?,
                request_count: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// Get daily aggregated stats for a time range
pub fn get_daily_stats(days: i64) -> Result<Vec<TokenStatsAggregated>, String> {
    let conn = connect_db()?;
    let cutoff = chrono::Utc::now() - chrono::Duration::days(days);
    let cutoff_bucket = cutoff.format("%Y-%m-%d").to_string();

    let mut stmt = conn
        .prepare(
            "SELECT substr(hour_bucket, 1, 10) as day_bucket,
                SUM(total_input_tokens) as input,
                SUM(total_output_tokens) as output,
                SUM(total_tokens) as total,
                SUM(request_count) as count
             FROM token_stats_hourly
             WHERE substr(hour_bucket, 1, 10) >= ?1
             GROUP BY day_bucket
             ORDER BY day_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([cutoff_bucket], |row| {
            Ok(TokenStatsAggregated {
                period: row.get(0)?,
                total_input_tokens: row.get(1)?,
                total_output_tokens: row.get(2)?,
                total_tokens: row.get(3)?,
                request_count: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// Get weekly aggregated stats
pub fn get_weekly_stats(weeks: i64) -> Result<Vec<TokenStatsAggregated>, String> {
    let conn = connect_db()?;
    let cutoff = chrono::Utc::now() - chrono::Duration::weeks(weeks);
    let cutoff_timestamp = cutoff.timestamp();

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-W%W', datetime(timestamp, 'unixepoch')) as week_bucket,
                SUM(input_tokens) as input,
                SUM(output_tokens) as output,
                SUM(total_tokens) as total,
                COUNT(*) as count
             FROM token_usage
             WHERE timestamp >= ?1
             GROUP BY week_bucket
             ORDER BY week_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([cutoff_timestamp], |row| {
            Ok(TokenStatsAggregated {
                period: row.get(0)?,
                total_input_tokens: row.get(1)?,
                total_output_tokens: row.get(2)?,
                total_tokens: row.get(3)?,
                request_count: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// Get per-account statistics for a time range
pub fn get_account_stats(hours: i64) -> Result<Vec<AccountTokenStats>, String> {
    let conn = connect_db()?;
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(hours);
    let cutoff_bucket = cutoff.format("%Y-%m-%d %H:00").to_string();

    let mut stmt = conn
        .prepare(
            "SELECT account_email,
                SUM(total_input_tokens) as input,
                SUM(total_output_tokens) as output,
                SUM(total_tokens) as total,
                SUM(request_count) as count
             FROM token_stats_hourly
             WHERE hour_bucket >= ?1
             GROUP BY account_email
             ORDER BY total DESC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([cutoff_bucket], |row| {
            Ok(AccountTokenStats {
                account_email: row.get(0)?,
                total_input_tokens: row.get(1)?,
                total_output_tokens: row.get(2)?,
                total_tokens: row.get(3)?,
                request_count: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// Get summary statistics for a time range
pub fn get_summary_stats(hours: i64) -> Result<TokenStatsSummary, String> {
    let conn = connect_db()?;
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(hours);
    let cutoff_bucket = cutoff.format("%Y-%m-%d %H:00").to_string();

    let (total_input, total_output, total, requests): (u64, u64, u64, u64) = conn
        .query_row(
            "SELECT COALESCE(SUM(total_input_tokens), 0),
                COALESCE(SUM(total_output_tokens), 0),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(request_count), 0)
             FROM token_stats_hourly
             WHERE hour_bucket >= ?1",
            [&cutoff_bucket],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|e| e.to_string())?;

    let unique_accounts: u64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT account_email) FROM token_stats_hourly WHERE hour_bucket >= ?1",
            [&cutoff_bucket],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;

    Ok(TokenStatsSummary {
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        total_tokens: total,
        total_requests: requests,
        unique_accounts,
    })
}

/// Get per-model statistics for a time range
pub fn get_model_stats(hours: i64) -> Result<Vec<ModelTokenStats>, String> {
    let conn = connect_db()?;
    let cutoff = chrono::Utc::now().timestamp() - (hours * 3600);

    let mut stmt = conn
        .prepare(
            "SELECT model,
                SUM(input_tokens) as input,
                SUM(output_tokens) as output,
                SUM(total_tokens) as total,
                COUNT(*) as count
             FROM token_usage
             WHERE timestamp >= ?1
             GROUP BY model
             ORDER BY total DESC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([cutoff], |row| {
            Ok(ModelTokenStats {
                model: row.get(0)?,
                total_input_tokens: row.get(1)?,
                total_output_tokens: row.get(2)?,
                total_tokens: row.get(3)?,
                request_count: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// Get model trend data aggregated by hour
pub fn get_model_trend_hourly(hours: i64) -> Result<Vec<ModelTrendPoint>, String> {
    let conn = connect_db()?;
    let cutoff = chrono::Utc::now().timestamp() - (hours * 3600);

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m-%d %H:00', datetime(timestamp, 'unixepoch')) as hour_bucket,
                model,
                SUM(total_tokens) as total
             FROM token_usage
             WHERE timestamp >= ?1
             GROUP BY hour_bucket, model
             ORDER BY hour_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let mut trend_map: std::collections::BTreeMap<String, std::collections::HashMap<String, u64>> =
        std::collections::BTreeMap::new();

    let rows = stmt
        .query_map([cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    for row in rows {
        let (period, model, total) = row.map_err(|e| e.to_string())?;
        trend_map.entry(period).or_default().insert(model, total);
    }

    Ok(trend_map
        .into_iter()
        .map(|(period, model_data)| ModelTrendPoint { period, model_data })
        .collect())
}

/// Get model trend data aggregated by day
pub fn get_model_trend_daily(days: i64) -> Result<Vec<ModelTrendPoint>, String> {
    let conn = connect_db()?;
    let cutoff = chrono::Utc::now().timestamp() - (days * 24 * 3600);

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m-%d', datetime(timestamp, 'unixepoch')) as day_bucket,
                model,
                SUM(total_tokens) as total
             FROM token_usage
             WHERE timestamp >= ?1
             GROUP BY day_bucket, model
             ORDER BY day_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let mut trend_map: std::collections::BTreeMap<String, std::collections::HashMap<String, u64>> =
        std::collections::BTreeMap::new();

    let rows = stmt
        .query_map([cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    for row in rows {
        let (period, model, total) = row.map_err(|e| e.to_string())?;
        trend_map.entry(period).or_default().insert(model, total);
    }

    Ok(trend_map
        .into_iter()
        .map(|(period, model_data)| ModelTrendPoint { period, model_data })
        .collect())
}

/// Get account trend data aggregated by hour
pub fn get_account_trend_hourly(hours: i64) -> Result<Vec<AccountTrendPoint>, String> {
    let conn = connect_db()?;
    let cutoff = chrono::Utc::now().timestamp() - (hours * 3600);

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m-%d %H:00', datetime(timestamp, 'unixepoch')) as hour_bucket,
                account_email,
                SUM(total_tokens) as total
             FROM token_usage
             WHERE timestamp >= ?1
             GROUP BY hour_bucket, account_email
             ORDER BY hour_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let mut trend_map: std::collections::BTreeMap<String, std::collections::HashMap<String, u64>> =
        std::collections::BTreeMap::new();

    let rows = stmt
        .query_map([cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    for row in rows {
        let (period, account, total) = row.map_err(|e| e.to_string())?;
        trend_map.entry(period).or_default().insert(account, total);
    }

    Ok(trend_map
        .into_iter()
        .map(|(period, account_data)| AccountTrendPoint {
            period,
            account_data,
        })
        .collect())
}

/// Get account trend data aggregated by day
pub fn get_account_trend_daily(days: i64) -> Result<Vec<AccountTrendPoint>, String> {
    let conn = connect_db()?;
    let cutoff = chrono::Utc::now().timestamp() - (days * 24 * 3600);

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m-%d', datetime(timestamp, 'unixepoch')) as day_bucket,
                account_email,
                SUM(total_tokens) as total
             FROM token_usage
             WHERE timestamp >= ?1
             GROUP BY day_bucket, account_email
             ORDER BY day_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let mut trend_map: std::collections::BTreeMap<String, std::collections::HashMap<String, u64>> =
        std::collections::BTreeMap::new();

    let rows = stmt
        .query_map([cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    for row in rows {
        let (period, account, total) = row.map_err(|e| e.to_string())?;
        trend_map.entry(period).or_default().insert(account, total);
    }

    Ok(trend_map
        .into_iter()
        .map(|(period, account_data)| AccountTrendPoint {
            period,
            account_data,
        })
        .collect())
}

// ── Internal helpers for testable DB operations ──

fn init_db_with_conn(conn: &Connection) -> Result<(), String> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS token_usage (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp INTEGER NOT NULL,
            account_email TEXT NOT NULL,
            model TEXT NOT NULL,
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL DEFAULT 0
        )",
        [],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_token_timestamp ON token_usage (timestamp DESC)",
        [],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_token_account ON token_usage (account_email)",
        [],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS token_stats_hourly (
            hour_bucket TEXT NOT NULL,
            account_email TEXT NOT NULL,
            total_input_tokens INTEGER NOT NULL DEFAULT 0,
            total_output_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL DEFAULT 0,
            request_count INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (hour_bucket, account_email)
        )",
        [],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

fn record_usage_with_conn(
    conn: &Connection,
    account_email: &str,
    model: &str,
    input_tokens: u32,
    output_tokens: u32,
    timestamp: i64,
    hour_bucket: &str,
) -> Result<(), String> {
    let total_tokens = input_tokens + output_tokens;

    conn.execute(
        "INSERT INTO token_usage (timestamp, account_email, model, input_tokens, output_tokens, total_tokens)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![timestamp, account_email, model, input_tokens, output_tokens, total_tokens],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "INSERT INTO token_stats_hourly (hour_bucket, account_email, total_input_tokens, total_output_tokens, total_tokens, request_count)
         VALUES (?1, ?2, ?3, ?4, ?5, 1)
         ON CONFLICT(hour_bucket, account_email) DO UPDATE SET
            total_input_tokens = total_input_tokens + ?3,
            total_output_tokens = total_output_tokens + ?4,
            total_tokens = total_tokens + ?5,
            request_count = request_count + 1",
        params![hour_bucket, account_email, input_tokens, output_tokens, total_tokens],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

fn get_summary_stats_with_conn(
    conn: &Connection,
    cutoff_bucket: &str,
) -> Result<TokenStatsSummary, String> {
    let (total_input, total_output, total, requests): (u64, u64, u64, u64) = conn
        .query_row(
            "SELECT COALESCE(SUM(total_input_tokens), 0),
                COALESCE(SUM(total_output_tokens), 0),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(request_count), 0)
             FROM token_stats_hourly
             WHERE hour_bucket >= ?1",
            [cutoff_bucket],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|e| e.to_string())?;

    let unique_accounts: u64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT account_email) FROM token_stats_hourly WHERE hour_bucket >= ?1",
            [cutoff_bucket],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;

    Ok(TokenStatsSummary {
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        total_tokens: total,
        total_requests: requests,
        unique_accounts,
    })
}

fn get_account_stats_with_conn(
    conn: &Connection,
    cutoff_bucket: &str,
) -> Result<Vec<AccountTokenStats>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT account_email,
                SUM(total_input_tokens) as input,
                SUM(total_output_tokens) as output,
                SUM(total_tokens) as total,
                SUM(request_count) as count
             FROM token_stats_hourly
             WHERE hour_bucket >= ?1
             GROUP BY account_email
             ORDER BY total DESC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([cutoff_bucket], |row| {
            Ok(AccountTokenStats {
                account_email: row.get(0)?,
                total_input_tokens: row.get(1)?,
                total_output_tokens: row.get(2)?,
                total_tokens: row.get(3)?,
                request_count: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

fn get_model_stats_with_conn(
    conn: &Connection,
    cutoff_timestamp: i64,
) -> Result<Vec<ModelTokenStats>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT model,
                SUM(input_tokens) as input,
                SUM(output_tokens) as output,
                SUM(total_tokens) as total,
                COUNT(*) as count
             FROM token_usage
             WHERE timestamp >= ?1
             GROUP BY model
             ORDER BY total DESC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([cutoff_timestamp], |row| {
            Ok(ModelTokenStats {
                model: row.get(0)?,
                total_input_tokens: row.get(1)?,
                total_output_tokens: row.get(2)?,
                total_tokens: row.get(3)?,
                request_count: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

fn get_hourly_stats_with_conn(
    conn: &Connection,
    cutoff_bucket: &str,
) -> Result<Vec<TokenStatsAggregated>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT hour_bucket,
                SUM(total_input_tokens) as input,
                SUM(total_output_tokens) as output,
                SUM(total_tokens) as total,
                SUM(request_count) as count
             FROM token_stats_hourly
             WHERE hour_bucket >= ?1
             GROUP BY hour_bucket
             ORDER BY hour_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([cutoff_bucket], |row| {
            Ok(TokenStatsAggregated {
                period: row.get(0)?,
                total_input_tokens: row.get(1)?,
                total_output_tokens: row.get(2)?,
                total_tokens: row.get(3)?,
                request_count: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db_with_conn(&conn).unwrap();
        conn
    }

    #[test]
    fn test_init_db_creates_tables() {
        let conn = setup_test_db();

        // Verify token_usage table exists
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM token_usage", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // Verify token_stats_hourly table exists
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM token_stats_hourly", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_record_usage_inserts_raw_and_hourly() {
        let conn = setup_test_db();
        let now = chrono::Utc::now();
        let bucket = now.format("%Y-%m-%d %H:00").to_string();

        record_usage_with_conn(&conn, "user@test.com", "gemini-pro", 100, 50, now.timestamp(), &bucket).unwrap();

        // Check raw table
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM token_usage", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Check hourly aggregation
        let (input, output, total, req_count): (u64, u64, u64, u64) = conn
            .query_row(
                "SELECT total_input_tokens, total_output_tokens, total_tokens, request_count FROM token_stats_hourly WHERE account_email = ?1",
                ["user@test.com"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(input, 100);
        assert_eq!(output, 50);
        assert_eq!(total, 150);
        assert_eq!(req_count, 1);
    }

    #[test]
    fn test_record_usage_aggregates_same_hour() {
        let conn = setup_test_db();
        let now = chrono::Utc::now();
        let bucket = now.format("%Y-%m-%d %H:00").to_string();

        record_usage_with_conn(&conn, "user@test.com", "gemini-pro", 100, 50, now.timestamp(), &bucket).unwrap();
        record_usage_with_conn(&conn, "user@test.com", "gemini-flash", 200, 100, now.timestamp(), &bucket).unwrap();

        // Raw table should have 2 rows
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM token_usage", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);

        // Hourly table should have 1 row (same account, same hour)
        let (input, output, total, req_count): (u64, u64, u64, u64) = conn
            .query_row(
                "SELECT total_input_tokens, total_output_tokens, total_tokens, request_count FROM token_stats_hourly WHERE account_email = ?1",
                ["user@test.com"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(input, 300);
        assert_eq!(output, 150);
        assert_eq!(total, 450);
        assert_eq!(req_count, 2);
    }

    #[test]
    fn test_summary_stats_empty_db() {
        let conn = setup_test_db();
        let summary = get_summary_stats_with_conn(&conn, "2000-01-01 00:00").unwrap();
        assert_eq!(summary.total_input_tokens, 0);
        assert_eq!(summary.total_output_tokens, 0);
        assert_eq!(summary.total_tokens, 0);
        assert_eq!(summary.total_requests, 0);
        assert_eq!(summary.unique_accounts, 0);
    }

    #[test]
    fn test_summary_stats_with_data() {
        let conn = setup_test_db();
        let now = chrono::Utc::now();
        let bucket = now.format("%Y-%m-%d %H:00").to_string();

        record_usage_with_conn(&conn, "alice@test.com", "gemini-pro", 100, 50, now.timestamp(), &bucket).unwrap();
        record_usage_with_conn(&conn, "bob@test.com", "gemini-flash", 200, 100, now.timestamp(), &bucket).unwrap();
        record_usage_with_conn(&conn, "alice@test.com", "gemini-pro", 50, 25, now.timestamp(), &bucket).unwrap();

        let summary = get_summary_stats_with_conn(&conn, "2000-01-01 00:00").unwrap();
        assert_eq!(summary.total_input_tokens, 350);
        assert_eq!(summary.total_output_tokens, 175);
        assert_eq!(summary.total_tokens, 525);
        assert_eq!(summary.total_requests, 3);
        assert_eq!(summary.unique_accounts, 2);
    }

    #[test]
    fn test_account_stats_ordering() {
        let conn = setup_test_db();
        let now = chrono::Utc::now();
        let bucket = now.format("%Y-%m-%d %H:00").to_string();

        // Bob uses more tokens
        record_usage_with_conn(&conn, "alice@test.com", "gemini-pro", 100, 50, now.timestamp(), &bucket).unwrap();
        record_usage_with_conn(&conn, "bob@test.com", "gemini-pro", 500, 300, now.timestamp(), &bucket).unwrap();

        let stats = get_account_stats_with_conn(&conn, "2000-01-01 00:00").unwrap();
        assert_eq!(stats.len(), 2);
        // Bob should be first (higher total)
        assert_eq!(stats[0].account_email, "bob@test.com");
        assert_eq!(stats[0].total_tokens, 800);
        assert_eq!(stats[1].account_email, "alice@test.com");
        assert_eq!(stats[1].total_tokens, 150);
    }

    #[test]
    fn test_model_stats_grouping() {
        let conn = setup_test_db();
        let now = chrono::Utc::now();
        let bucket = now.format("%Y-%m-%d %H:00").to_string();

        record_usage_with_conn(&conn, "alice@test.com", "gemini-pro", 100, 50, now.timestamp(), &bucket).unwrap();
        record_usage_with_conn(&conn, "bob@test.com", "gemini-pro", 200, 100, now.timestamp(), &bucket).unwrap();
        record_usage_with_conn(&conn, "alice@test.com", "gemini-flash", 50, 25, now.timestamp(), &bucket).unwrap();

        let stats = get_model_stats_with_conn(&conn, 0).unwrap();
        assert_eq!(stats.len(), 2);
        // gemini-pro should be first (higher total)
        assert_eq!(stats[0].model, "gemini-pro");
        assert_eq!(stats[0].total_tokens, 450);
        assert_eq!(stats[0].request_count, 2);
        assert_eq!(stats[1].model, "gemini-flash");
        assert_eq!(stats[1].total_tokens, 75);
        assert_eq!(stats[1].request_count, 1);
    }

    #[test]
    fn test_hourly_stats_bucketing() {
        let conn = setup_test_db();
        let now = chrono::Utc::now();
        let bucket = now.format("%Y-%m-%d %H:00").to_string();

        record_usage_with_conn(&conn, "alice@test.com", "gemini-pro", 100, 50, now.timestamp(), &bucket).unwrap();
        record_usage_with_conn(&conn, "bob@test.com", "gemini-flash", 200, 100, now.timestamp(), &bucket).unwrap();

        let stats = get_hourly_stats_with_conn(&conn, "2000-01-01 00:00").unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].period, bucket);
        assert_eq!(stats[0].total_input_tokens, 300);
        assert_eq!(stats[0].total_output_tokens, 150);
        assert_eq!(stats[0].total_tokens, 450);
        assert_eq!(stats[0].request_count, 2);
    }

    #[test]
    fn test_hourly_stats_multiple_buckets() {
        let conn = setup_test_db();
        let now = chrono::Utc::now();
        let bucket1 = "2025-01-15 10:00";
        let bucket2 = "2025-01-15 11:00";
        let ts = now.timestamp();

        record_usage_with_conn(&conn, "alice@test.com", "gemini-pro", 100, 50, ts, bucket1).unwrap();
        record_usage_with_conn(&conn, "alice@test.com", "gemini-pro", 200, 100, ts, bucket2).unwrap();

        let stats = get_hourly_stats_with_conn(&conn, "2025-01-15 09:00").unwrap();
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].period, "2025-01-15 10:00");
        assert_eq!(stats[0].total_tokens, 150);
        assert_eq!(stats[1].period, "2025-01-15 11:00");
        assert_eq!(stats[1].total_tokens, 300);
    }

    #[test]
    fn test_hourly_stats_cutoff_filter() {
        let conn = setup_test_db();
        let ts = chrono::Utc::now().timestamp();

        record_usage_with_conn(&conn, "alice@test.com", "gemini-pro", 100, 50, ts, "2025-01-15 08:00").unwrap();
        record_usage_with_conn(&conn, "alice@test.com", "gemini-pro", 200, 100, ts, "2025-01-15 12:00").unwrap();

        // Cutoff at 10:00 should only return the 12:00 bucket
        let stats = get_hourly_stats_with_conn(&conn, "2025-01-15 10:00").unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].period, "2025-01-15 12:00");
    }

    #[test]
    fn test_struct_serialization() {
        let stats = TokenStatsAggregated {
            period: "2025-01-15 10:00".to_string(),
            total_input_tokens: 100,
            total_output_tokens: 50,
            total_tokens: 150,
            request_count: 1,
        };
        let json = serde_json::to_string(&stats).unwrap();
        let deserialized: TokenStatsAggregated = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.period, "2025-01-15 10:00");
        assert_eq!(deserialized.total_tokens, 150);
    }

    #[test]
    fn test_account_token_stats_serialization() {
        let stats = AccountTokenStats {
            account_email: "user@test.com".to_string(),
            total_input_tokens: 1000,
            total_output_tokens: 500,
            total_tokens: 1500,
            request_count: 10,
        };
        let json = serde_json::to_string(&stats).unwrap();
        let deserialized: AccountTokenStats = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.account_email, "user@test.com");
        assert_eq!(deserialized.request_count, 10);
    }

    #[test]
    fn test_model_trend_point_serialization() {
        let mut model_data = std::collections::HashMap::new();
        model_data.insert("gemini-pro".to_string(), 1000u64);
        model_data.insert("gemini-flash".to_string(), 500u64);

        let point = ModelTrendPoint {
            period: "2025-01-15 10:00".to_string(),
            model_data,
        };
        let json = serde_json::to_string(&point).unwrap();
        let deserialized: ModelTrendPoint = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.model_data.get("gemini-pro"), Some(&1000));
    }

    #[test]
    fn test_summary_stats_serialization() {
        let summary = TokenStatsSummary {
            total_input_tokens: 5000,
            total_output_tokens: 2500,
            total_tokens: 7500,
            total_requests: 50,
            unique_accounts: 3,
        };
        let json = serde_json::to_string(&summary).unwrap();
        let deserialized: TokenStatsSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.total_tokens, 7500);
        assert_eq!(deserialized.unique_accounts, 3);
    }

    #[test]
    fn test_zero_token_usage() {
        let conn = setup_test_db();
        let now = chrono::Utc::now();
        let bucket = now.format("%Y-%m-%d %H:00").to_string();

        record_usage_with_conn(&conn, "user@test.com", "gemini-pro", 0, 0, now.timestamp(), &bucket).unwrap();

        let summary = get_summary_stats_with_conn(&conn, "2000-01-01 00:00").unwrap();
        assert_eq!(summary.total_tokens, 0);
        assert_eq!(summary.total_requests, 1);
    }

    #[test]
    fn test_multiple_accounts_same_model() {
        let conn = setup_test_db();
        let now = chrono::Utc::now();
        let bucket = now.format("%Y-%m-%d %H:00").to_string();

        for i in 0..5 {
            record_usage_with_conn(
                &conn,
                &format!("user{}@test.com", i),
                "gemini-pro",
                100,
                50,
                now.timestamp(),
                &bucket,
            )
            .unwrap();
        }

        let model_stats = get_model_stats_with_conn(&conn, 0).unwrap();
        assert_eq!(model_stats.len(), 1);
        assert_eq!(model_stats[0].request_count, 5);
        assert_eq!(model_stats[0].total_tokens, 750); // 5 * 150

        let account_stats = get_account_stats_with_conn(&conn, "2000-01-01 00:00").unwrap();
        assert_eq!(account_stats.len(), 5);
    }
}
