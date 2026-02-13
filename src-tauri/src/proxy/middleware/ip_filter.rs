// IP 黑白名单过滤中间件
use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::models::config::SecurityMonitorConfig;

// ============================================================================
// CIDR 匹配
// ============================================================================

/// 解析 IPv4 地址为 u32
fn parse_ipv4(ip: &str) -> Option<u32> {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut result: u32 = 0;
    for part in &parts {
        let octet: u8 = part.parse().ok()?;
        result = (result << 8) | (octet as u32);
    }
    Some(result)
}

/// 检查 IP 是否匹配 CIDR 网段或精确匹配
pub fn cidr_match(ip: &str, pattern: &str) -> bool {
    // 精确匹配
    if ip == pattern {
        return true;
    }

    // CIDR 匹配
    if let Some(slash_pos) = pattern.find('/') {
        let network_str = &pattern[..slash_pos];
        let prefix_str = &pattern[slash_pos + 1..];

        let prefix_len: u32 = match prefix_str.parse() {
            Ok(p) if p <= 32 => p,
            _ => return false,
        };

        let ip_val = match parse_ipv4(ip) {
            Some(v) => v,
            None => return false,
        };
        let network_val = match parse_ipv4(network_str) {
            Some(v) => v,
            None => return false,
        };

        if prefix_len == 0 {
            return true; // /0 matches everything
        }

        let mask = !0u32 << (32 - prefix_len);
        return (ip_val & mask) == (network_val & mask);
    }

    false
}

/// 检查 IP 是否在列表中（支持精确匹配和 CIDR）
pub fn is_ip_in_list(ip: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| cidr_match(ip, pattern))
}

// ============================================================================
// IP Filter Middleware
// ============================================================================

/// 从请求中提取客户端 IP
pub fn extract_client_ip(request: &Request) -> Option<String> {
    request
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or(s).trim().to_string())
        .or_else(|| {
            request
                .headers()
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
        .or_else(|| {
            request
                .extensions()
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .map(|info| info.0.ip().to_string())
        })
}

/// 创建被封禁的响应
#[allow(dead_code)]
fn create_blocked_response(ip: &str, message: &str) -> Response {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": "ip_blocked",
            "code": "ip_blocked",
            "ip": ip,
        }
    });

    (
        StatusCode::FORBIDDEN,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&body).unwrap_or_else(|_| message.to_string()),
    )
        .into_response()
}

/// IP 过滤逻辑（纯函数，便于测试）
/// 返回 Ok(()) 表示放行，Err(message) 表示拒绝
pub fn check_ip_access(
    ip: &str,
    config: &SecurityMonitorConfig,
    blacklist_patterns: &[String],
    whitelist_patterns: &[String],
) -> Result<(), String> {
    // 1. 白名单模式启用时，只允许白名单 IP
    if config.whitelist.enabled {
        if is_ip_in_list(ip, whitelist_patterns) {
            return Ok(());
        }
        return Err("Access denied. Your IP is not in the whitelist.".to_string());
    }

    // 2. 白名单优先模式：在白名单中则跳过黑名单检查
    if config.whitelist.whitelist_priority && is_ip_in_list(ip, whitelist_patterns) {
        return Ok(());
    }

    // 3. 检查黑名单
    if config.blacklist.enabled && is_ip_in_list(ip, blacklist_patterns) {
        let msg = if config.blacklist.block_message.is_empty() {
            "Access denied".to_string()
        } else {
            config.blacklist.block_message.clone()
        };
        return Err(msg);
    }

    Ok(())
}

/// IP 黑白名单过滤中间件
///
/// 注意：此中间件的完整版本需要与 security_db 集成来动态查询黑白名单。
/// 当前版本提供核心过滤逻辑，security_db 集成将在 Task 11.1 中完成。
pub async fn ip_filter_middleware(request: Request, next: Next) -> Response {
    let client_ip = extract_client_ip(&request);

    if client_ip.is_none() {
        tracing::warn!("[IP Filter] Unable to extract client IP from request");
    }

    // 完整的 IP 过滤需要 AppState 中的 security_db 集成
    // 核心逻辑已在 check_ip_access() 中实现
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::config::{IpBlacklistConfig, IpWhitelistConfig, SecurityMonitorConfig};

    // ========================================================================
    // CIDR 匹配测试
    // ========================================================================

    #[test]
    fn test_cidr_exact_match() {
        assert!(cidr_match("192.168.1.1", "192.168.1.1"));
        assert!(!cidr_match("192.168.1.2", "192.168.1.1"));
    }

    #[test]
    fn test_cidr_subnet_match() {
        // /24 subnet
        assert!(cidr_match("192.168.1.1", "192.168.1.0/24"));
        assert!(cidr_match("192.168.1.255", "192.168.1.0/24"));
        assert!(!cidr_match("192.168.2.1", "192.168.1.0/24"));

        // /16 subnet
        assert!(cidr_match("10.0.1.1", "10.0.0.0/16"));
        assert!(cidr_match("10.0.255.255", "10.0.0.0/16"));
        assert!(!cidr_match("10.1.0.1", "10.0.0.0/16"));

        // /8 subnet
        assert!(cidr_match("10.1.2.3", "10.0.0.0/8"));
        assert!(!cidr_match("11.0.0.1", "10.0.0.0/8"));
    }

    #[test]
    fn test_cidr_edge_cases() {
        // /32 = exact match
        assert!(cidr_match("192.168.1.1", "192.168.1.1/32"));
        assert!(!cidr_match("192.168.1.2", "192.168.1.1/32"));

        // /0 = match everything
        assert!(cidr_match("1.2.3.4", "0.0.0.0/0"));
        assert!(cidr_match("255.255.255.255", "0.0.0.0/0"));
    }

    #[test]
    fn test_cidr_invalid_inputs() {
        assert!(!cidr_match("invalid", "192.168.1.0/24"));
        assert!(!cidr_match("192.168.1.1", "invalid/24"));
        assert!(!cidr_match("192.168.1.1", "192.168.1.0/33"));
        assert!(!cidr_match("192.168.1.1", "192.168.1.0/abc"));
    }

    #[test]
    fn test_parse_ipv4() {
        assert_eq!(parse_ipv4("0.0.0.0"), Some(0));
        assert_eq!(parse_ipv4("255.255.255.255"), Some(u32::MAX));
        assert_eq!(parse_ipv4("192.168.1.1"), Some(0xC0A80101));
        assert_eq!(parse_ipv4("10.0.0.1"), Some(0x0A000001));
        assert_eq!(parse_ipv4("invalid"), None);
        assert_eq!(parse_ipv4("256.0.0.1"), None);
        assert_eq!(parse_ipv4("1.2.3"), None);
    }

    // ========================================================================
    // IP 过滤逻辑测试
    // ========================================================================

    fn make_config(
        blacklist_enabled: bool,
        whitelist_enabled: bool,
        whitelist_priority: bool,
    ) -> SecurityMonitorConfig {
        SecurityMonitorConfig {
            blacklist: IpBlacklistConfig {
                enabled: blacklist_enabled,
                block_message: "Blocked".to_string(),
            },
            whitelist: IpWhitelistConfig {
                enabled: whitelist_enabled,
                whitelist_priority,
            },
        }
    }

    #[test]
    fn test_blacklist_blocks_ip() {
        let config = make_config(true, false, false);
        let blacklist = vec!["192.168.1.100".to_string()];
        let whitelist = vec![];

        assert!(check_ip_access("192.168.1.100", &config, &blacklist, &whitelist).is_err());
        assert!(check_ip_access("192.168.1.101", &config, &blacklist, &whitelist).is_ok());
    }

    #[test]
    fn test_blacklist_cidr_blocks() {
        let config = make_config(true, false, false);
        let blacklist = vec!["10.0.0.0/8".to_string()];
        let whitelist = vec![];

        assert!(check_ip_access("10.1.2.3", &config, &blacklist, &whitelist).is_err());
        assert!(check_ip_access("192.168.1.1", &config, &blacklist, &whitelist).is_ok());
    }

    #[test]
    fn test_whitelist_only_allows_listed() {
        let config = make_config(false, true, false);
        let blacklist = vec![];
        let whitelist = vec!["192.168.1.0/24".to_string()];

        assert!(check_ip_access("192.168.1.50", &config, &blacklist, &whitelist).is_ok());
        assert!(check_ip_access("10.0.0.1", &config, &blacklist, &whitelist).is_err());
    }

    #[test]
    fn test_whitelist_priority_overrides_blacklist() {
        // IP is in both blacklist and whitelist, whitelist_priority = true
        let config = make_config(true, false, true);
        let blacklist = vec!["192.168.1.100".to_string()];
        let whitelist = vec!["192.168.1.100".to_string()];

        // Should be allowed because whitelist_priority is true
        assert!(check_ip_access("192.168.1.100", &config, &blacklist, &whitelist).is_ok());
    }

    #[test]
    fn test_whitelist_priority_false_blacklist_wins() {
        let config = make_config(true, false, false);
        let blacklist = vec!["192.168.1.100".to_string()];
        let whitelist = vec!["192.168.1.100".to_string()];

        // whitelist_priority is false, blacklist should win
        assert!(check_ip_access("192.168.1.100", &config, &blacklist, &whitelist).is_err());
    }

    #[test]
    fn test_both_disabled_allows_all() {
        let config = make_config(false, false, false);
        let blacklist = vec!["192.168.1.100".to_string()];
        let whitelist = vec![];

        assert!(check_ip_access("192.168.1.100", &config, &blacklist, &whitelist).is_ok());
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;
    use crate::models::config::{IpBlacklistConfig, IpWhitelistConfig, SecurityMonitorConfig};
    use proptest::prelude::*;

    /// Generate a valid IPv4 address string from 4 octets
    fn ipv4_string(a: u8, b: u8, c: u8, d: u8) -> String {
        format!("{}.{}.{}.{}", a, b, c, d)
    }

    /// Convert 4 octets to a u32 value
    fn octets_to_u32(a: u8, b: u8, c: u8, d: u8) -> u32 {
        ((a as u32) << 24) | ((b as u32) << 16) | ((c as u32) << 8) | (d as u32)
    }

    // **Feature: kiro-ai-gateway, Property 12: IP 黑名单 CIDR 匹配正确性**
    // **Validates: Requirements 6.5**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// For any IPv4 address and CIDR network, when the IP belongs to the
        /// network, cidr_match SHALL return true; when it doesn't belong,
        /// cidr_match SHALL return false.
        #[test]
        fn prop_cidr_match_correctness(
            ip_a in 0u8..=255u8,
            ip_b in 0u8..=255u8,
            ip_c in 0u8..=255u8,
            ip_d in 0u8..=255u8,
            net_a in 0u8..=255u8,
            net_b in 0u8..=255u8,
            net_c in 0u8..=255u8,
            net_d in 0u8..=255u8,
            prefix_len in 0u32..=32u32,
        ) {
            let ip_str = ipv4_string(ip_a, ip_b, ip_c, ip_d);
            let cidr_str = format!("{}/{}", ipv4_string(net_a, net_b, net_c, net_d), prefix_len);

            let ip_val = octets_to_u32(ip_a, ip_b, ip_c, ip_d);
            let net_val = octets_to_u32(net_a, net_b, net_c, net_d);

            // Compute expected membership mathematically
            let expected = if prefix_len == 0 {
                true // /0 matches everything
            } else {
                let mask = !0u32 << (32 - prefix_len);
                (ip_val & mask) == (net_val & mask)
            };

            let result = cidr_match(&ip_str, &cidr_str);
            prop_assert_eq!(
                result, expected,
                "cidr_match({}, {}) returned {} but expected {}",
                ip_str, cidr_str, result, expected
            );
        }
    }

    // **Feature: kiro-ai-gateway, Property 13: 白名单优先规则**
    // **Validates: Requirements 6.7**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// For any IP address, when whitelist_priority mode is enabled and the
        /// IP is in both the blacklist and whitelist, access SHALL be allowed.
        #[test]
        fn prop_whitelist_priority_allows_dual_listed_ip(
            ip_a in 0u8..=255u8,
            ip_b in 0u8..=255u8,
            ip_c in 0u8..=255u8,
            ip_d in 0u8..=255u8,
        ) {
            let ip_str = ipv4_string(ip_a, ip_b, ip_c, ip_d);

            // Config: blacklist enabled, whitelist NOT in exclusive mode,
            // whitelist_priority = true
            let config = SecurityMonitorConfig {
                blacklist: IpBlacklistConfig {
                    enabled: true,
                    block_message: "Blocked".to_string(),
                },
                whitelist: IpWhitelistConfig {
                    enabled: false,
                    whitelist_priority: true,
                },
            };

            // IP is in both blacklist and whitelist
            let blacklist = vec![ip_str.clone()];
            let whitelist = vec![ip_str.clone()];

            let result = check_ip_access(&ip_str, &config, &blacklist, &whitelist);
            prop_assert!(
                result.is_ok(),
                "IP {} should be allowed when whitelist_priority is enabled and IP is in both lists, but got: {:?}",
                ip_str, result
            );
        }
    }
}

