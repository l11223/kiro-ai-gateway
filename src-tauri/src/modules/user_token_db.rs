//! User Token Database Module
//! 用户令牌数据库操作模块

use chrono::{FixedOffset, Timelike, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// 用户令牌
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserToken {
    pub id: String,
    pub token: String,
    pub username: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub expires_type: String,
    pub expires_at: Option<i64>,
    pub max_ips: i32,
    pub curfew_start: Option<String>,
    pub curfew_end: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_used_at: Option<i64>,
    pub total_requests: i64,
    pub total_tokens_used: i64,
}

/// 令牌 IP 绑定
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenIpBinding {
    pub id: String,
    pub token_id: String,
    pub ip_address: String,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
    pub request_count: i64,
    pub user_agent: Option<String>,
}

/// 令牌使用日志
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsageLog {
    pub id: String,
    pub token_id: String,
    pub ip_address: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub request_time: i64,
    pub status: i32,
}

// ============================================================================
// Database functions
// ============================================================================

fn get_db_path() -> Result<PathBuf, String> {
    Ok(crate::modules::account::get_data_dir()?.join("user_tokens.db"))
}

fn get_connection() -> Result<Connection, String> {
    let path = get_db_path()?;
    Connection::open(&path).map_err(|e| format!("Failed to open user_tokens.db: {}", e))
}

pub fn init_db() -> Result<(), String> {
    let conn = get_connection()?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS user_tokens (
            id TEXT PRIMARY KEY,
            token TEXT UNIQUE NOT NULL,
            username TEXT NOT NULL,
            description TEXT,
            enabled BOOLEAN DEFAULT 1,
            expires_type TEXT,
            expires_at INTEGER,
            max_ips INTEGER DEFAULT 0,
            curfew_start TEXT,
            curfew_end TEXT,
            created_at INTEGER,
            updated_at INTEGER,
            last_used_at INTEGER,
            total_requests INTEGER DEFAULT 0,
            total_tokens_used INTEGER DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS token_ip_bindings (
            id TEXT PRIMARY KEY,
            token_id TEXT NOT NULL,
            ip_address TEXT NOT NULL,
            first_seen_at INTEGER,
            last_seen_at INTEGER,
            request_count INTEGER DEFAULT 0,
            user_agent TEXT,
            UNIQUE(token_id, ip_address)
        );
        CREATE TABLE IF NOT EXISTS token_usage_logs (
            id TEXT PRIMARY KEY,
            token_id TEXT NOT NULL,
            ip_address TEXT,
            model TEXT,
            input_tokens INTEGER,
            output_tokens INTEGER,
            request_time INTEGER,
            status INTEGER
        );",
    )
    .map_err(|e| format!("Failed to init user_tokens.db: {}", e))
}

pub fn list_tokens() -> Result<Vec<UserToken>, String> {
    let conn = get_connection()?;
    let mut stmt = conn
        .prepare("SELECT id, token, username, description, enabled, expires_type, expires_at, max_ips, curfew_start, curfew_end, created_at, updated_at, last_used_at, total_requests, total_tokens_used FROM user_tokens ORDER BY created_at DESC")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(UserToken {
                id: row.get(0)?,
                token: row.get(1)?,
                username: row.get(2)?,
                description: row.get(3)?,
                enabled: row.get(4)?,
                expires_type: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                expires_at: row.get(6)?,
                max_ips: row.get(7)?,
                curfew_start: row.get(8)?,
                curfew_end: row.get(9)?,
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
                last_used_at: row.get(12)?,
                total_requests: row.get(13)?,
                total_tokens_used: row.get(14)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn create_token(
    username: String,
    expires_type: String,
    description: Option<String>,
    max_ips: i32,
    curfew_start: Option<String>,
    curfew_end: Option<String>,
    custom_expires_at: Option<i64>,
) -> Result<UserToken, String> {
    let conn = get_connection()?;
    let id = Uuid::new_v4().to_string();
    let token = format!("sk-{}", Uuid::new_v4());
    let now = Utc::now().timestamp();
    let expires_at = custom_expires_at.or_else(|| {
        match expires_type.as_str() {
            "day" => Some(now + 86400),
            "week" => Some(now + 604800),
            "month" => Some(now + 2592000),
            "never" => None,
            _ => None,
        }
    });

    conn.execute(
        "INSERT INTO user_tokens (id, token, username, description, enabled, expires_type, expires_at, max_ips, curfew_start, curfew_end, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
        params![id, token, username, description, expires_type, expires_at, max_ips, curfew_start, curfew_end, now],
    ).map_err(|e| e.to_string())?;

    Ok(UserToken {
        id,
        token,
        username,
        description,
        enabled: true,
        expires_type,
        expires_at,
        max_ips,
        curfew_start,
        curfew_end,
        created_at: now,
        updated_at: now,
        last_used_at: None,
        total_requests: 0,
        total_tokens_used: 0,
    })
}

pub fn update_token(
    id: &str,
    username: Option<String>,
    description: Option<String>,
    enabled: Option<bool>,
    max_ips: Option<i32>,
    curfew_start: Option<Option<String>>,
    curfew_end: Option<Option<String>>,
) -> Result<(), String> {
    let conn = get_connection()?;
    let now = Utc::now().timestamp();
    let mut sets = vec!["updated_at = ?1".to_string()];
    let mut idx = 2u32;

    // Build dynamic update - simplified approach
    if let Some(ref _u) = username { sets.push(format!("username = ?{}", idx)); idx += 1; }
    if let Some(ref _d) = description { sets.push(format!("description = ?{}", idx)); idx += 1; }
    if let Some(_e) = enabled { sets.push(format!("enabled = ?{}", idx)); idx += 1; }
    if let Some(_m) = max_ips { sets.push(format!("max_ips = ?{}", idx)); idx += 1; }
    if let Some(ref _cs) = curfew_start { sets.push(format!("curfew_start = ?{}", idx)); idx += 1; }
    if let Some(ref _ce) = curfew_end { sets.push(format!("curfew_end = ?{}", idx)); idx += 1; }

    let sql = format!("UPDATE user_tokens SET {} WHERE id = ?{}", sets.join(", "), idx);

    let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(now)];
    if let Some(u) = username { params_vec.push(Box::new(u)); }
    if let Some(d) = description { params_vec.push(Box::new(d)); }
    if let Some(e) = enabled { params_vec.push(Box::new(e)); }
    if let Some(m) = max_ips { params_vec.push(Box::new(m)); }
    if let Some(cs) = curfew_start { params_vec.push(Box::new(cs)); }
    if let Some(ce) = curfew_end { params_vec.push(Box::new(ce)); }
    params_vec.push(Box::new(id.to_string()));

    let params_refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
    conn.execute(&sql, params_refs.as_slice()).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn delete_token(id: &str) -> Result<(), String> {
    let conn = get_connection()?;
    conn.execute("DELETE FROM token_ip_bindings WHERE token_id = ?1", params![id])
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM token_usage_logs WHERE token_id = ?1", params![id])
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM user_tokens WHERE id = ?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn renew_token(id: &str, expires_type: &str) -> Result<(), String> {
    let conn = get_connection()?;
    let now = Utc::now().timestamp();
    let expires_at = match expires_type {
        "day" => Some(now + 86400),
        "week" => Some(now + 604800),
        "month" => Some(now + 2592000),
        "never" => None,
        _ => None,
    };
    conn.execute(
        "UPDATE user_tokens SET expires_type = ?1, expires_at = ?2, updated_at = ?3 WHERE id = ?4",
        params![expires_type, expires_at, now, id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn get_token_ips(token_id: &str) -> Result<Vec<TokenIpBinding>, String> {
    let conn = get_connection()?;
    let mut stmt = conn
        .prepare("SELECT id, token_id, ip_address, first_seen_at, last_seen_at, request_count, user_agent FROM token_ip_bindings WHERE token_id = ?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![token_id], |row| {
            Ok(TokenIpBinding {
                id: row.get(0)?,
                token_id: row.get(1)?,
                ip_address: row.get(2)?,
                first_seen_at: row.get(3)?,
                last_seen_at: row.get(4)?,
                request_count: row.get(5)?,
                user_agent: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn validate_token(token_str: &str, client_ip: &str) -> Result<(bool, Option<String>, Option<UserToken>), String> {
    let conn = get_connection()?;
    let token: Option<UserToken> = conn
        .query_row(
            "SELECT id, token, username, description, enabled, expires_type, expires_at, max_ips, curfew_start, curfew_end, created_at, updated_at, last_used_at, total_requests, total_tokens_used FROM user_tokens WHERE token = ?1",
            params![token_str],
            |row| {
                Ok(UserToken {
                    id: row.get(0)?,
                    token: row.get(1)?,
                    username: row.get(2)?,
                    description: row.get(3)?,
                    enabled: row.get(4)?,
                    expires_type: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    expires_at: row.get(6)?,
                    max_ips: row.get(7)?,
                    curfew_start: row.get(8)?,
                    curfew_end: row.get(9)?,
                    created_at: row.get(10)?,
                    updated_at: row.get(11)?,
                    last_used_at: row.get(12)?,
                    total_requests: row.get(13)?,
                    total_tokens_used: row.get(14)?,
                })
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;

    let token = match token {
        Some(t) => t,
        None => return Ok((false, Some("Token not found".to_string()), None)),
    };

    if !token.enabled {
        return Ok((false, Some("Token is disabled".to_string()), None));
    }

    // Check expiry
    if let Some(expires_at) = token.expires_at {
        if Utc::now().timestamp() > expires_at {
            return Ok((false, Some("Token has expired".to_string()), None));
        }
    }

    // Check curfew
    if let (Some(ref start), Some(ref end)) = (&token.curfew_start, &token.curfew_end) {
        let beijing = FixedOffset::east_opt(8 * 3600).unwrap();
        let now_beijing = Utc::now().with_timezone(&beijing);
        let current_minutes = now_beijing.hour() * 60 + now_beijing.minute();

        if let Some(true) = is_in_curfew(start, end, current_minutes) {
            return Ok((false, Some(format!("Token is in curfew period ({}-{})", start, end)), None));
        }
    }

    // Check IP limit
    if token.max_ips > 0 {
        let ip_count: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT ip_address) FROM token_ip_bindings WHERE token_id = ?1",
                params![token.id],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let ip_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM token_ip_bindings WHERE token_id = ?1 AND ip_address = ?2",
                params![token.id, client_ip],
                |row| row.get::<_, i64>(0).map(|c| c > 0),
            )
            .unwrap_or(false);

        if !check_ip_limit(token.max_ips, ip_count, ip_exists) {
            return Ok((false, Some(format!("IP limit reached ({}/{})", ip_count, token.max_ips)), None));
        }
    }

    Ok((true, None, Some(token)))
}
/// Pure curfew time check function (extracted for testability).
///
/// Given curfew start/end times as "HH:MM" strings and the current time in minutes since midnight,
/// returns `true` if the current time falls within the curfew period.
/// Supports cross-midnight curfew periods (e.g., "22:00" to "06:00").
pub fn is_in_curfew(curfew_start: &str, curfew_end: &str, current_minutes: u32) -> Option<bool> {
    let parse_time = |s: &str| -> Option<u32> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() == 2 {
            let h: u32 = parts[0].parse().ok()?;
            let m: u32 = parts[1].parse().ok()?;
            if h >= 24 || m >= 60 {
                return None;
            }
            Some(h * 60 + m)
        } else {
            None
        }
    };

    let start_min = parse_time(curfew_start)?;
    let end_min = parse_time(curfew_end)?;

    let in_curfew = if start_min <= end_min {
        // Same-day curfew: e.g., 08:00 - 18:00
        current_minutes >= start_min && current_minutes < end_min
    } else {
        // Cross-midnight curfew: e.g., 22:00 - 06:00
        current_minutes >= start_min || current_minutes < end_min
    };

    Some(in_curfew)
}



/// Pure IP limit check function (extracted for testability).
///
/// Given the max allowed IPs, the current count of bound IPs, and whether the
/// requesting IP is already bound, returns `true` if the request should be
/// ALLOWED, `false` if it should be REJECTED.
///
/// Rules:
/// - If `max_ips == 0`, no limit is enforced → always allowed.
/// - If the requesting IP is already bound → always allowed.
/// - If `bound_ip_count >= max_ips` and the IP is new → rejected.
/// - Otherwise → allowed (new IP can still be added).
pub fn check_ip_limit(max_ips: i32, bound_ip_count: i64, ip_already_bound: bool) -> bool {
    if max_ips <= 0 {
        return true; // No limit
    }
    if ip_already_bound {
        return true; // Already bound IP is always allowed
    }
    // New IP: only allowed if we haven't reached the limit
    bound_ip_count < max_ips as i64
}

pub fn get_token_by_value(token_str: &str) -> Result<Option<UserToken>, String> {
    let conn = get_connection()?;
    conn.query_row(
        "SELECT id, token, username, description, enabled, expires_type, expires_at, max_ips, curfew_start, curfew_end, created_at, updated_at, last_used_at, total_requests, total_tokens_used FROM user_tokens WHERE token = ?1",
        params![token_str],
        |row| {
            Ok(UserToken {
                id: row.get(0)?,
                token: row.get(1)?,
                username: row.get(2)?,
                description: row.get(3)?,
                enabled: row.get(4)?,
                expires_type: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                expires_at: row.get(6)?,
                max_ips: row.get(7)?,
                curfew_start: row.get(8)?,
                curfew_end: row.get(9)?,
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
                last_used_at: row.get(12)?,
                total_requests: row.get(13)?,
                total_tokens_used: row.get(14)?,
            })
        },
    )
    .optional()
    .map_err(|e| e.to_string())
}


// ============================================================================
// Property-Based Tests
// ============================================================================

#[cfg(test)]
mod prop_curfew_time {
    use super::*;
    use proptest::prelude::*;

    /// **Feature: kiro-ai-gateway, Property 14: User Token 宵禁时间判定正确性**
    /// **Validates: Requirements 6.12**

    /// Strategy to generate valid HH:MM time strings
    fn time_str_strategy() -> impl Strategy<Value = (String, u32)> {
        (0u32..24, 0u32..60).prop_map(|(h, m)| {
            (format!("{:02}:{:02}", h, m), h * 60 + m)
        })
    }

    /// Strategy to generate current time as minutes since midnight (0..1440)
    fn current_minutes_strategy() -> impl Strategy<Value = u32> {
        0u32..1440
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Property 14: For any curfew configuration (start/end, supporting cross-midnight)
        /// and current time, when the current time is within the curfew period,
        /// is_in_curfew SHALL return true; when outside the period, SHALL return false.
        #[test]
        fn prop_curfew_same_day(
            (start_str, start_min) in time_str_strategy(),
            (end_str, end_min) in time_str_strategy(),
            current_minutes in current_minutes_strategy(),
        ) {
            // Skip when start == end (degenerate case: no curfew period)
            prop_assume!(start_min != end_min);

            let result = is_in_curfew(&start_str, &end_str, current_minutes);
            prop_assert!(result.is_some(), "is_in_curfew should return Some for valid inputs");
            let in_curfew = result.unwrap();

            if start_min <= end_min {
                // Same-day curfew: e.g., 08:00 - 18:00
                let expected = current_minutes >= start_min && current_minutes < end_min;
                prop_assert_eq!(in_curfew, expected,
                    "Same-day curfew: start={}, end={}, current={}, expected={}, got={}",
                    start_str, end_str, current_minutes, expected, in_curfew);
            } else {
                // Cross-midnight curfew: e.g., 22:00 - 06:00
                let expected = current_minutes >= start_min || current_minutes < end_min;
                prop_assert_eq!(in_curfew, expected,
                    "Cross-midnight curfew: start={}, end={}, current={}, expected={}, got={}",
                    start_str, end_str, current_minutes, expected, in_curfew);
            }
        }

        /// Property 14 (boundary): Times exactly at curfew_start are IN curfew,
        /// times exactly at curfew_end are NOT in curfew (half-open interval [start, end)).
        #[test]
        fn prop_curfew_boundary(
            (start_str, start_min) in time_str_strategy(),
            (end_str, end_min) in time_str_strategy(),
        ) {
            prop_assume!(start_min != end_min);

            // At start: should be IN curfew
            let at_start = is_in_curfew(&start_str, &end_str, start_min);
            prop_assert!(at_start.is_some());
            prop_assert_eq!(at_start.unwrap(), true,
                "Time at curfew_start should be IN curfew: start={}, end={}",
                start_str, end_str);

            // At end: should be NOT in curfew
            let at_end = is_in_curfew(&start_str, &end_str, end_min);
            prop_assert!(at_end.is_some());
            prop_assert_eq!(at_end.unwrap(), false,
                "Time at curfew_end should be NOT in curfew: start={}, end={}",
                start_str, end_str);
        }

        /// Property 14 (cross-midnight): For cross-midnight curfew (start > end),
        /// times in [start, 24:00) and [00:00, end) are in curfew.
        #[test]
        fn prop_curfew_cross_midnight(
            start_hour in 12u32..24,
            start_minute in 0u32..60,
            end_hour in 0u32..12,
            end_minute in 0u32..60,
            current_minutes in current_minutes_strategy(),
        ) {
            let start_min = start_hour * 60 + start_minute;
            let end_min = end_hour * 60 + end_minute;
            prop_assume!(start_min > end_min); // Ensure cross-midnight

            let start_str = format!("{:02}:{:02}", start_hour, start_minute);
            let end_str = format!("{:02}:{:02}", end_hour, end_minute);

            let result = is_in_curfew(&start_str, &end_str, current_minutes);
            prop_assert!(result.is_some());
            let in_curfew = result.unwrap();

            let expected = current_minutes >= start_min || current_minutes < end_min;
            prop_assert_eq!(in_curfew, expected,
                "Cross-midnight: start={}({}), end={}({}), current={}, expected={}, got={}",
                start_str, start_min, end_str, end_min, current_minutes, expected, in_curfew);
        }

        /// Property 14 (invalid input): Invalid time strings should return None.
        #[test]
        fn prop_curfew_invalid_returns_none(
            current_minutes in current_minutes_strategy(),
        ) {
            // Invalid format
            prop_assert!(is_in_curfew("invalid", "08:00", current_minutes).is_none());
            prop_assert!(is_in_curfew("08:00", "invalid", current_minutes).is_none());
            prop_assert!(is_in_curfew("25:00", "08:00", current_minutes).is_none());
            prop_assert!(is_in_curfew("08:00", "08:60", current_minutes).is_none());
        }
    }
}


// ============================================================================
// Property-Based Tests: IP Limit
// ============================================================================

#[cfg(test)]
mod prop_ip_limit {
    use super::*;
    use proptest::prelude::*;

    /// **Feature: kiro-ai-gateway, Property 15: User Token IP 限制正确性**
    /// **Validates: Requirements 6.13**

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Property 15: For any User Token with max_ips > 0, when the number of
        /// bound IPs equals max_ips, requests from a new IP SHALL be rejected;
        /// requests from an already-bound IP SHALL be allowed.
        #[test]
        fn prop_ip_limit_at_capacity_new_ip_rejected(
            max_ips in 1i32..100,
        ) {
            let bound_ip_count = max_ips as i64; // exactly at capacity
            let ip_already_bound = false;         // new IP

            let allowed = check_ip_limit(max_ips, bound_ip_count, ip_already_bound);
            prop_assert!(!allowed,
                "New IP should be REJECTED when bound IPs ({}) == max_ips ({})",
                bound_ip_count, max_ips);
        }

        /// Property 15: Already-bound IP SHALL be allowed even when at capacity.
        #[test]
        fn prop_ip_limit_at_capacity_existing_ip_allowed(
            max_ips in 1i32..100,
        ) {
            let bound_ip_count = max_ips as i64; // exactly at capacity
            let ip_already_bound = true;          // existing IP

            let allowed = check_ip_limit(max_ips, bound_ip_count, ip_already_bound);
            prop_assert!(allowed,
                "Existing IP should be ALLOWED even when bound IPs ({}) == max_ips ({})",
                bound_ip_count, max_ips);
        }

        /// Property 15: When bound IPs exceed max_ips, new IP SHALL be rejected.
        #[test]
        fn prop_ip_limit_over_capacity_new_ip_rejected(
            max_ips in 1i32..50,
            extra in 1i64..50,
        ) {
            let bound_ip_count = max_ips as i64 + extra; // over capacity
            let ip_already_bound = false;

            let allowed = check_ip_limit(max_ips, bound_ip_count, ip_already_bound);
            prop_assert!(!allowed,
                "New IP should be REJECTED when bound IPs ({}) > max_ips ({})",
                bound_ip_count, max_ips);
        }

        /// Property 15: When bound IPs exceed max_ips, existing IP SHALL still be allowed.
        #[test]
        fn prop_ip_limit_over_capacity_existing_ip_allowed(
            max_ips in 1i32..50,
            extra in 1i64..50,
        ) {
            let bound_ip_count = max_ips as i64 + extra; // over capacity
            let ip_already_bound = true;

            let allowed = check_ip_limit(max_ips, bound_ip_count, ip_already_bound);
            prop_assert!(allowed,
                "Existing IP should be ALLOWED even when bound IPs ({}) > max_ips ({})",
                bound_ip_count, max_ips);
        }

        /// Property 15: When bound IPs are below max_ips, new IP SHALL be allowed.
        #[test]
        fn prop_ip_limit_under_capacity_new_ip_allowed(
            max_ips in 2i32..100,
            bound_ip_count_offset in 1i32..100,
        ) {
            let bound_ip_count = (max_ips - 1).min(bound_ip_count_offset) as i64;
            prop_assume!(bound_ip_count < max_ips as i64);
            let ip_already_bound = false;

            let allowed = check_ip_limit(max_ips, bound_ip_count, ip_already_bound);
            prop_assert!(allowed,
                "New IP should be ALLOWED when bound IPs ({}) < max_ips ({})",
                bound_ip_count, max_ips);
        }

        /// Property 15: When max_ips == 0 (no limit), any IP SHALL be allowed.
        #[test]
        fn prop_ip_limit_no_limit(
            bound_ip_count in 0i64..1000,
            ip_already_bound in proptest::bool::ANY,
        ) {
            let allowed = check_ip_limit(0, bound_ip_count, ip_already_bound);
            prop_assert!(allowed,
                "Any IP should be ALLOWED when max_ips == 0 (no limit), bound={}, existing={}",
                bound_ip_count, ip_already_bound);
        }
    }
}
