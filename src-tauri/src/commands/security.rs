// Security management commands
//
// Tauri commands for IP access logs, blacklist/whitelist CRUD,
// security config management, and IP statistics.

use crate::modules::security_db;
use serde::{Deserialize, Serialize};

// ============================================================================
// Request/Response types
// ============================================================================

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IpAccessLogQuery {
    pub page: usize,
    pub page_size: usize,
    pub search: Option<String>,
    pub blocked_only: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IpAccessLogResponse {
    pub logs: Vec<security_db::IpAccessLog>,
    pub total: usize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddBlacklistRequest {
    pub ip_pattern: String,
    pub reason: Option<String>,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddWhitelistRequest {
    pub ip_pattern: String,
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IpStatsResponse {
    pub total_requests: usize,
    pub unique_ips: usize,
    pub blocked_requests: usize,
    pub top_ips: Vec<security_db::IpRanking>,
}

// ============================================================================
// IP Access Log Commands
// ============================================================================

/// Get IP access logs (paginated)
#[tauri::command]
pub async fn get_ip_access_logs(query: IpAccessLogQuery) -> Result<IpAccessLogResponse, String> {
    let offset = (query.page.max(1) - 1) * query.page_size;
    let logs = security_db::get_ip_access_logs(
        query.page_size,
        offset,
        query.search.as_deref(),
        query.blocked_only,
    )?;
    let total = security_db::get_ip_access_logs_count(
        query.search.as_deref(),
        query.blocked_only,
    )? as usize;

    Ok(IpAccessLogResponse { logs, total })
}

/// Get IP statistics
#[tauri::command]
pub async fn get_ip_stats() -> Result<IpStatsResponse, String> {
    let stats = security_db::get_ip_stats()?;
    let top_ips = security_db::get_top_ips(10, 24)?;

    Ok(IpStatsResponse {
        total_requests: stats.total_requests as usize,
        unique_ips: stats.unique_ips as usize,
        blocked_requests: stats.blocked_count as usize,
        top_ips,
    })
}

/// Clear all IP access logs
#[tauri::command]
pub async fn clear_ip_access_logs() -> Result<(), String> {
    security_db::clear_ip_access_logs()
}

// ============================================================================
// IP Blacklist Commands
// ============================================================================

/// Get IP blacklist
#[tauri::command]
pub async fn get_ip_blacklist() -> Result<Vec<security_db::IpBlacklistEntry>, String> {
    security_db::get_blacklist()
}

/// Add IP to blacklist
#[tauri::command]
pub async fn add_ip_to_blacklist(request: AddBlacklistRequest) -> Result<(), String> {
    if !is_valid_ip_pattern(&request.ip_pattern) {
        return Err(
            "Invalid IP pattern. Use IP address or CIDR notation (e.g., 192.168.1.0/24)"
                .to_string(),
        );
    }
    security_db::add_to_blacklist(
        &request.ip_pattern,
        request.reason.as_deref(),
        request.expires_at,
        "manual",
    )?;
    Ok(())
}

/// Remove IP from blacklist
#[tauri::command]
pub async fn remove_ip_from_blacklist(ip_pattern: String) -> Result<(), String> {
    let entries = security_db::get_blacklist()?;
    let entry = entries.iter().find(|e| e.ip_pattern == ip_pattern);
    if let Some(entry) = entry {
        security_db::remove_from_blacklist(&entry.id)
    } else {
        Err(format!("IP pattern {} not found in blacklist", ip_pattern))
    }
}

/// Clear entire blacklist
#[tauri::command]
pub async fn clear_ip_blacklist() -> Result<(), String> {
    let entries = security_db::get_blacklist()?;
    for entry in entries {
        security_db::remove_from_blacklist(&entry.id)?;
    }
    Ok(())
}

/// Check if IP is in blacklist
#[tauri::command]
pub async fn check_ip_in_blacklist(ip: String) -> Result<bool, String> {
    security_db::is_ip_in_blacklist(&ip)
}

// ============================================================================
// IP Whitelist Commands
// ============================================================================

/// Get IP whitelist
#[tauri::command]
pub async fn get_ip_whitelist() -> Result<Vec<security_db::IpWhitelistEntry>, String> {
    security_db::get_whitelist()
}

/// Add IP to whitelist
#[tauri::command]
pub async fn add_ip_to_whitelist(request: AddWhitelistRequest) -> Result<(), String> {
    if !is_valid_ip_pattern(&request.ip_pattern) {
        return Err(
            "Invalid IP pattern. Use IP address or CIDR notation (e.g., 192.168.1.0/24)"
                .to_string(),
        );
    }
    security_db::add_to_whitelist(&request.ip_pattern, request.description.as_deref())?;
    Ok(())
}

/// Remove IP from whitelist
#[tauri::command]
pub async fn remove_ip_from_whitelist(ip_pattern: String) -> Result<(), String> {
    let entries = security_db::get_whitelist()?;
    let entry = entries.iter().find(|e| e.ip_pattern == ip_pattern);
    if let Some(entry) = entry {
        security_db::remove_from_whitelist(&entry.id)
    } else {
        Err(format!("IP pattern {} not found in whitelist", ip_pattern))
    }
}

/// Clear entire whitelist
#[tauri::command]
pub async fn clear_ip_whitelist() -> Result<(), String> {
    let entries = security_db::get_whitelist()?;
    for entry in entries {
        security_db::remove_from_whitelist(&entry.id)?;
    }
    Ok(())
}

/// Check if IP is in whitelist
#[tauri::command]
pub async fn check_ip_in_whitelist(ip: String) -> Result<bool, String> {
    security_db::is_ip_in_whitelist(&ip)
}

// ============================================================================
// Security Config Commands
// ============================================================================

/// Get security monitor config
#[tauri::command]
pub async fn get_security_config(
    app_state: tauri::State<'_, crate::commands::proxy::ProxyServiceState>,
) -> Result<crate::models::config::SecurityMonitorConfig, String> {
    let instance_lock = app_state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        return Ok(instance.config.security_monitor.clone());
    }
    let app_config = crate::modules::config::load_app_config()
        .map_err(|e| format!("Failed to load config: {}", e))?;
    Ok(app_config.proxy.security_monitor)
}

/// Update security monitor config
#[tauri::command]
pub async fn update_security_config(
    config: crate::models::config::SecurityMonitorConfig,
    app_state: tauri::State<'_, crate::commands::proxy::ProxyServiceState>,
) -> Result<(), String> {
    let mut app_config = crate::modules::config::load_app_config()
        .map_err(|e| format!("Failed to load config: {}", e))?;
    app_config.proxy.security_monitor = config.clone();
    crate::modules::config::save_app_config(&app_config)
        .map_err(|e| format!("Failed to save config: {}", e))?;

    {
        let mut instance_lock = app_state.instance.write().await;
        if let Some(instance) = instance_lock.as_mut() {
            instance.config.security_monitor = config;
            instance.axum_server.update_security(&instance.config).await;
        }
    }

    Ok(())
}

// ============================================================================
// Statistics Commands
// ============================================================================

/// Get IP token consumption stats
#[tauri::command]
pub async fn get_ip_token_stats(
    limit: Option<usize>,
    hours: Option<i64>,
) -> Result<Vec<crate::modules::proxy_db::IpTokenStats>, String> {
    crate::modules::proxy_db::get_token_usage_by_ip(limit.unwrap_or(100), hours.unwrap_or(720))
}

// ============================================================================
// Helper functions
// ============================================================================

/// Validate IP pattern format (supports single IP and CIDR)
fn is_valid_ip_pattern(pattern: &str) -> bool {
    if pattern.contains('/') {
        let parts: Vec<&str> = pattern.split('/').collect();
        if parts.len() != 2 {
            return false;
        }
        if !is_valid_ip(parts[0]) {
            return false;
        }
        if let Ok(mask) = parts[1].parse::<u8>() {
            return mask <= 32;
        }
        return false;
    }
    is_valid_ip(pattern)
}

/// Validate IPv4 address format
fn is_valid_ip(ip: &str) -> bool {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    parts.iter().all(|part| part.parse::<u8>().is_ok())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_ip_patterns() {
        assert!(is_valid_ip_pattern("192.168.1.1"));
        assert!(is_valid_ip_pattern("10.0.0.0/8"));
        assert!(is_valid_ip_pattern("172.16.0.0/16"));
        assert!(is_valid_ip_pattern("192.168.1.0/24"));
        assert!(is_valid_ip_pattern("8.8.8.8/32"));
    }

    #[test]
    fn test_invalid_ip_patterns() {
        assert!(!is_valid_ip_pattern("256.1.1.1"));
        assert!(!is_valid_ip_pattern("192.168.1"));
        assert!(!is_valid_ip_pattern("192.168.1.1/33"));
        assert!(!is_valid_ip_pattern("192.168.1.1/"));
        assert!(!is_valid_ip_pattern("invalid"));
    }

    #[test]
    fn test_ip_access_log_query_serialization() {
        let query = IpAccessLogQuery {
            page: 1,
            page_size: 20,
            search: Some("192.168".to_string()),
            blocked_only: true,
        };
        let json = serde_json::to_string(&query).unwrap();
        assert!(json.contains("blockedOnly"));
        assert!(json.contains("pageSize"));
    }

    #[test]
    fn test_add_blacklist_request_serialization() {
        let req = AddBlacklistRequest {
            ip_pattern: "10.0.0.0/8".to_string(),
            reason: Some("suspicious".to_string()),
            expires_at: Some(1700000000),
        };
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: AddBlacklistRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.ip_pattern, "10.0.0.0/8");
    }

    #[test]
    fn test_ip_stats_response_serialization() {
        let resp = IpStatsResponse {
            total_requests: 1000,
            unique_ips: 50,
            blocked_requests: 10,
            top_ips: vec![],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let deserialized: IpStatsResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.total_requests, 1000);
    }

    #[test]
    fn test_valid_ip_edge_cases() {
        assert!(is_valid_ip("0.0.0.0"));
        assert!(is_valid_ip("255.255.255.255"));
        assert!(!is_valid_ip(""));
        assert!(!is_valid_ip("1.2.3"));
        assert!(!is_valid_ip("1.2.3.4.5"));
    }

    #[test]
    fn test_cidr_edge_cases() {
        assert!(is_valid_ip_pattern("0.0.0.0/0"));
        assert!(is_valid_ip_pattern("255.255.255.255/32"));
        assert!(!is_valid_ip_pattern("192.168.1.0/"));
        assert!(!is_valid_ip_pattern("/24"));
        assert!(!is_valid_ip_pattern("192.168.1.0/abc"));
    }
}
