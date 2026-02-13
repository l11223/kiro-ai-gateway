// 代理安全配置模块
//
// Requirements covered:
// - 6.1: API Key 验证 (strict mode)
// - 6.4: Admin API 强制鉴权 (admin_password 或 api_key)

use crate::models::config::{ProxyAuthMode, ProxyConfig, SecurityMonitorConfig};

// ============================================================================
// ProxySecurityConfig - 运行时安全配置
// ============================================================================

#[derive(Debug, Clone)]
pub struct ProxySecurityConfig {
    pub auth_mode: ProxyAuthMode,
    pub api_key: String,
    pub admin_password: Option<String>,
    pub allow_lan_access: bool,
    pub port: u16,
    pub security_monitor: SecurityMonitorConfig,
}

impl ProxySecurityConfig {
    pub fn from_proxy_config(config: &ProxyConfig) -> Self {
        Self {
            auth_mode: config.auth_mode.clone(),
            api_key: config.api_key.clone(),
            admin_password: config.admin_password.clone(),
            allow_lan_access: config.allow_lan_access,
            port: config.port,
            security_monitor: config.security_monitor.clone(),
        }
    }

    /// 根据 auth_mode 和 allow_lan_access 计算实际鉴权模式
    pub fn effective_auth_mode(&self) -> ProxyAuthMode {
        match self.auth_mode {
            ProxyAuthMode::Auto => {
                if self.allow_lan_access {
                    ProxyAuthMode::AllExceptHealth
                } else {
                    ProxyAuthMode::Off
                }
            }
            ref other => other.clone(),
        }
    }

    /// 验证 API Key 是否匹配
    pub fn validate_api_key(&self, key: &str) -> bool {
        !self.api_key.is_empty() && key == self.api_key
    }

    /// 验证管理密码（优先 admin_password，回退 api_key）
    pub fn validate_admin_key(&self, key: &str) -> bool {
        match &self.admin_password {
            Some(pwd) if !pwd.is_empty() => key == pwd,
            _ => self.validate_api_key(key),
        }
    }

    /// 检查是否需要鉴权（基于 effective_auth_mode 和路径）
    pub fn requires_auth(&self, path: &str, is_admin: bool) -> bool {
        let effective = self.effective_auth_mode();
        let is_health = path == "/healthz" || path == "/api/health" || path == "/health";

        if matches!(effective, ProxyAuthMode::Off) {
            return false;
        }

        if is_admin {
            // 管理接口：健康检查放行，其余需要鉴权
            !is_health
        } else {
            // 代理接口
            match effective {
                ProxyAuthMode::AllExceptHealth => !is_health,
                ProxyAuthMode::Strict => true,
                _ => false,
            }
        }
    }
}

// ============================================================================
// User Token 验证集成
// ============================================================================

/// User Token 验证结果
#[derive(Debug, Clone)]
pub enum UserTokenValidation {
    /// Token 有效
    Valid {
        token_id: String,
        token: String,
        username: String,
    },
    /// Token 无效（附带拒绝原因）
    Rejected(String),
    /// 验证出错
    Error(String),
    /// 不是 User Token（未找到匹配的 token）
    NotUserToken,
}

/// 验证 User Token
/// 集成 user_token_db 的 validate_token 和 get_token_by_value
pub fn validate_user_token(token_str: &str, client_ip: &str) -> UserTokenValidation {
    match crate::modules::user_token_db::validate_token(token_str, client_ip) {
        Ok((true, _, Some(user_token))) => UserTokenValidation::Valid {
            token_id: user_token.id,
            token: user_token.token,
            username: user_token.username,
        },
        Ok((true, _, None)) => {
            // Token 验证通过但未返回 token 信息，尝试再次获取
            match crate::modules::user_token_db::get_token_by_value(token_str) {
                Ok(Some(user_token)) => UserTokenValidation::Valid {
                    token_id: user_token.id,
                    token: user_token.token,
                    username: user_token.username,
                },
                Ok(None) => UserTokenValidation::NotUserToken,
                Err(e) => UserTokenValidation::Error(e),
            }
        }
        Ok((false, reason, _)) => {
            let reason_str = reason.unwrap_or_else(|| "Access denied".to_string());
            UserTokenValidation::Rejected(reason_str)
        }
        Err(e) => UserTokenValidation::Error(e),
    }
}

/// 尝试识别 User Token（不做完整验证，仅查找是否存在）
/// 用于 auth_mode=Off 时记录使用情况
pub fn identify_user_token(token_str: &str) -> Option<(String, String, String)> {
    match crate::modules::user_token_db::get_token_by_value(token_str) {
        Ok(Some(t)) => Some((t.id, t.token, t.username)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(auth_mode: ProxyAuthMode, allow_lan: bool) -> ProxySecurityConfig {
        ProxySecurityConfig {
            auth_mode,
            api_key: "sk-test-key".to_string(),
            admin_password: None,
            allow_lan_access: allow_lan,
            port: 8080,
            security_monitor: SecurityMonitorConfig::default(),
        }
    }

    // ---- effective_auth_mode tests ----

    #[test]
    fn test_auto_mode_resolves_off_for_local_only() {
        let s = make_config(ProxyAuthMode::Auto, false);
        assert!(matches!(s.effective_auth_mode(), ProxyAuthMode::Off));
    }

    #[test]
    fn test_auto_mode_resolves_all_except_health_for_lan() {
        let s = make_config(ProxyAuthMode::Auto, true);
        assert!(matches!(
            s.effective_auth_mode(),
            ProxyAuthMode::AllExceptHealth
        ));
    }

    #[test]
    fn test_strict_mode_stays_strict() {
        let s = make_config(ProxyAuthMode::Strict, false);
        assert!(matches!(s.effective_auth_mode(), ProxyAuthMode::Strict));
    }

    #[test]
    fn test_off_mode_stays_off() {
        let s = make_config(ProxyAuthMode::Off, true);
        assert!(matches!(s.effective_auth_mode(), ProxyAuthMode::Off));
    }

    // ---- from_proxy_config test ----

    #[test]
    fn test_from_proxy_config() {
        let proxy_config = ProxyConfig::default();
        let sec = ProxySecurityConfig::from_proxy_config(&proxy_config);
        assert_eq!(sec.port, proxy_config.port);
        assert_eq!(sec.allow_lan_access, proxy_config.allow_lan_access);
        assert_eq!(sec.api_key, proxy_config.api_key);
    }

    // ---- validate_api_key tests ----

    #[test]
    fn test_validate_api_key_correct() {
        let s = make_config(ProxyAuthMode::Strict, false);
        assert!(s.validate_api_key("sk-test-key"));
    }

    #[test]
    fn test_validate_api_key_wrong() {
        let s = make_config(ProxyAuthMode::Strict, false);
        assert!(!s.validate_api_key("wrong-key"));
    }

    #[test]
    fn test_validate_api_key_empty_config() {
        let mut s = make_config(ProxyAuthMode::Strict, false);
        s.api_key = String::new();
        assert!(!s.validate_api_key("any-key"));
    }

    // ---- validate_admin_key tests ----

    #[test]
    fn test_validate_admin_key_with_password() {
        let mut s = make_config(ProxyAuthMode::Strict, false);
        s.admin_password = Some("admin-secret".to_string());
        assert!(s.validate_admin_key("admin-secret"));
        assert!(!s.validate_admin_key("sk-test-key")); // api_key should NOT work
    }

    #[test]
    fn test_validate_admin_key_fallback_to_api_key() {
        let s = make_config(ProxyAuthMode::Strict, false);
        assert!(s.validate_admin_key("sk-test-key"));
    }

    #[test]
    fn test_validate_admin_key_empty_password_fallback() {
        let mut s = make_config(ProxyAuthMode::Strict, false);
        s.admin_password = Some(String::new());
        assert!(s.validate_admin_key("sk-test-key"));
    }

    // ---- requires_auth tests ----

    #[test]
    fn test_requires_auth_off_mode() {
        let s = make_config(ProxyAuthMode::Off, false);
        assert!(!s.requires_auth("/v1/chat/completions", false));
        assert!(!s.requires_auth("/api/accounts", true));
    }

    #[test]
    fn test_requires_auth_strict_mode() {
        let s = make_config(ProxyAuthMode::Strict, false);
        assert!(s.requires_auth("/v1/chat/completions", false));
        assert!(s.requires_auth("/healthz", false));
    }

    #[test]
    fn test_requires_auth_all_except_health() {
        let s = make_config(ProxyAuthMode::AllExceptHealth, false);
        assert!(s.requires_auth("/v1/chat/completions", false));
        assert!(!s.requires_auth("/healthz", false));
        assert!(!s.requires_auth("/api/health", false));
    }

    #[test]
    fn test_requires_auth_admin_health_exempt() {
        let s = make_config(ProxyAuthMode::Strict, true);
        assert!(!s.requires_auth("/healthz", true));
        assert!(s.requires_auth("/api/accounts", true));
    }

    #[test]
    fn test_requires_auth_auto_lan() {
        let s = make_config(ProxyAuthMode::Auto, true);
        // Auto + LAN → AllExceptHealth
        assert!(s.requires_auth("/v1/chat/completions", false));
        assert!(!s.requires_auth("/healthz", false));
    }

    #[test]
    fn test_requires_auth_auto_local() {
        let s = make_config(ProxyAuthMode::Auto, false);
        // Auto + local → Off
        assert!(!s.requires_auth("/v1/chat/completions", false));
    }
}
