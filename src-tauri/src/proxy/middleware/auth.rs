// API Key 认证中间件
use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::proxy::security::{ProxySecurityConfig, UserTokenValidation, validate_user_token, identify_user_token};

// ============================================================================
// UserTokenIdentity - 用户令牌身份信息
// ============================================================================

/// 用户令牌身份信息 (传递给 Monitor 使用)
#[derive(Clone, Debug)]
pub struct UserTokenIdentity {
    pub token_id: String,
    #[allow(dead_code)]
    pub token: String,
    pub username: String,
}

// ============================================================================
// Auth Middleware
// ============================================================================

/// API Key 认证中间件 (代理接口使用，遵循 auth_mode)
pub async fn auth_middleware(
    state: State<Arc<RwLock<ProxySecurityConfig>>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    auth_middleware_internal(state, request, next, false).await
}

/// 管理接口认证中间件 (管理接口使用，强制严格鉴权)
pub async fn admin_auth_middleware(
    state: State<Arc<RwLock<ProxySecurityConfig>>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    auth_middleware_internal(state, request, next, true).await
}

/// 从请求中提取 API Key
fn extract_api_key(request: &Request) -> Option<String> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").or(Some(s)))
        .map(|s| s.to_string())
        .or_else(|| {
            request
                .headers()
                .get("x-api-key")
                .and_then(|h| h.to_str().ok())
                .map(|s| s.to_string())
        })
        .or_else(|| {
            request
                .headers()
                .get("x-goog-api-key")
                .and_then(|h| h.to_str().ok())
                .map(|s| s.to_string())
        })
}

/// 从请求中提取客户端 IP
fn extract_client_ip(request: &Request) -> String {
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
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

/// 内部认证逻辑
async fn auth_middleware_internal(
    State(security): State<Arc<RwLock<ProxySecurityConfig>>>,
    request: Request,
    next: Next,
    force_strict: bool,
) -> Result<Response, StatusCode> {
    let method = request.method().clone();
    let path = request.uri().path().to_string();

    let is_health_check = path == "/healthz" || path == "/api/health" || path == "/health";
    let is_internal_endpoint = path.starts_with("/internal/");

    if !path.contains("event_logging") && !is_health_check {
        tracing::info!("Request: {} {}", method, path);
    } else {
        tracing::trace!("Heartbeat/Health: {} {}", method, path);
    }

    // Allow CORS preflight
    if method == axum::http::Method::OPTIONS {
        return Ok(next.run(request).await);
    }

    let security = security.read().await.clone();

    if !force_strict {
        // AI 代理接口
        if !security.requires_auth(&path, false) {
            // auth_mode=Off 时，仍尝试识别 User Token 以记录使用情况
            if let Some(key) = extract_api_key(&request) {
                if let Some((token_id, token, username)) = identify_user_token(&key) {
                    let identity = UserTokenIdentity {
                        token_id,
                        token,
                        username,
                    };
                    let (mut parts, body) = request.into_parts();
                    parts.extensions.insert(identity);
                    let request = Request::from_parts(parts, body);
                    return Ok(next.run(request).await);
                }
            }
            return Ok(next.run(request).await);
        }

        // 内部端点豁免鉴权
        if is_internal_endpoint {
            tracing::debug!("Internal endpoint bypassed auth: {}", path);
            return Ok(next.run(request).await);
        }
    } else {
        // 管理接口
        if !security.requires_auth(&path, true) {
            return Ok(next.run(request).await);
        }
    }

    let api_key = extract_api_key(&request);

    if security.api_key.is_empty()
        && (security.admin_password.is_none()
            || security.admin_password.as_ref().unwrap().is_empty())
    {
        tracing::error!("Auth is required but both api_key and admin_password are empty");
        return Err(StatusCode::UNAUTHORIZED);
    }

    let authorized = if force_strict {
        // 管理接口：使用 validate_admin_key
        api_key
            .as_deref()
            .map(|k| security.validate_admin_key(k))
            .unwrap_or(false)
    } else {
        // AI 代理接口：使用 validate_api_key
        api_key
            .as_deref()
            .map(|k| security.validate_api_key(k))
            .unwrap_or(false)
    };

    if authorized {
        Ok(next.run(request).await)
    } else if !force_strict && api_key.is_some() {
        // API Key 不匹配，尝试验证 User Token
        let token_str = api_key.unwrap();
        let client_ip = extract_client_ip(&request);

        match validate_user_token(&token_str, &client_ip) {
            UserTokenValidation::Valid {
                token_id,
                token,
                username,
            } => {
                let identity = UserTokenIdentity {
                    token_id,
                    token,
                    username,
                };
                let (mut parts, body) = request.into_parts();
                parts.extensions.insert(identity);
                let request = Request::from_parts(parts, body);
                Ok(next.run(request).await)
            }
            UserTokenValidation::Rejected(reason) => {
                tracing::warn!("UserToken rejected: {}", reason);
                let body = serde_json::json!({
                    "error": {
                        "message": reason,
                        "type": "token_rejected",
                        "code": "token_rejected"
                    }
                });
                let response = axum::response::Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_string(&body).unwrap(),
                    ))
                    .unwrap();
                Ok(response)
            }
            UserTokenValidation::Error(e) => {
                tracing::error!("UserToken validation error: {}", e);
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
            UserTokenValidation::NotUserToken => Err(StatusCode::UNAUTHORIZED),
        }
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::config::{ProxyAuthMode, SecurityMonitorConfig};
    use proptest::prelude::*;

    #[test]
    fn test_auto_mode_resolves_off_for_local_only() {
        let s = ProxySecurityConfig {
            auth_mode: ProxyAuthMode::Auto,
            api_key: "sk-test".to_string(),
            admin_password: None,
            allow_lan_access: false,
            port: 8080,
            security_monitor: SecurityMonitorConfig::default(),
        };
        assert!(matches!(s.effective_auth_mode(), ProxyAuthMode::Off));
    }

    #[test]
    fn test_auto_mode_resolves_all_except_health_for_lan() {
        let s = ProxySecurityConfig {
            auth_mode: ProxyAuthMode::Auto,
            api_key: "sk-test".to_string(),
            admin_password: None,
            allow_lan_access: true,
            port: 8080,
            security_monitor: SecurityMonitorConfig::default(),
        };
        assert!(matches!(
            s.effective_auth_mode(),
            ProxyAuthMode::AllExceptHealth
        ));
    }

    #[test]
    fn test_strict_mode_stays_strict() {
        let s = ProxySecurityConfig {
            auth_mode: ProxyAuthMode::Strict,
            api_key: "sk-test".to_string(),
            admin_password: None,
            allow_lan_access: false,
            port: 8080,
            security_monitor: SecurityMonitorConfig::default(),
        };
        assert!(matches!(s.effective_auth_mode(), ProxyAuthMode::Strict));
    }

    #[test]
    fn test_off_mode_stays_off() {
        let s = ProxySecurityConfig {
            auth_mode: ProxyAuthMode::Off,
            api_key: "sk-test".to_string(),
            admin_password: None,
            allow_lan_access: true,
            port: 8080,
            security_monitor: SecurityMonitorConfig::default(),
        };
        assert!(matches!(s.effective_auth_mode(), ProxyAuthMode::Off));
    }

    #[test]
    fn test_from_proxy_config() {
        let proxy_config = crate::models::config::ProxyConfig::default();
        let sec = ProxySecurityConfig::from_proxy_config(&proxy_config);
        assert_eq!(sec.port, proxy_config.port);
        assert_eq!(sec.allow_lan_access, proxy_config.allow_lan_access);
    }

    // ---- Arbitrary strategies ----

    /// Generate a random auth_mode (only the 3 explicit modes; Auto is tested separately)
    fn arb_auth_mode() -> impl Strategy<Value = ProxyAuthMode> {
        prop_oneof![
            Just(ProxyAuthMode::Off),
            Just(ProxyAuthMode::Strict),
            Just(ProxyAuthMode::AllExceptHealth),
            Just(ProxyAuthMode::Auto),
        ]
    }

    /// Generate a random request path (mix of health endpoints and regular paths)
    fn arb_path() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("/healthz".to_string()),
            Just("/api/health".to_string()),
            Just("/health".to_string()),
            Just("/v1/chat/completions".to_string()),
            Just("/v1/messages".to_string()),
            Just("/v1/models".to_string()),
            Just("/v1/images/generations".to_string()),
            Just("/v1/audio/transcriptions".to_string()),
            Just("/api/accounts".to_string()),
            Just("/api/config".to_string()),
            "[a-z0-9/]{1,50}".prop_map(|s| format!("/{}", s)),
        ]
    }

    fn make_security_config(auth_mode: ProxyAuthMode, allow_lan: bool) -> ProxySecurityConfig {
        ProxySecurityConfig {
            auth_mode,
            api_key: "sk-test".to_string(),
            admin_password: None,
            allow_lan_access: allow_lan,
            port: 8080,
            security_monitor: SecurityMonitorConfig::default(),
        }
    }

    // **Feature: kiro-ai-gateway, Property 16: Auth Mode 鉴权一致性**
    // **Validates: Requirements 6.1, 6.2, 6.3**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// For any request path and auth_mode configuration:
        /// - strict: ALL routes SHALL require authentication
        /// - all_except_health: all routes EXCEPT /healthz SHALL require authentication
        /// - off: NO routes SHALL require authentication
        #[test]
        fn prop_auth_mode_consistency(
            auth_mode in arb_auth_mode(),
            path in arb_path(),
            allow_lan in proptest::bool::ANY,
        ) {
            let config = make_security_config(auth_mode.clone(), allow_lan);
            let effective = config.effective_auth_mode();
            let requires = config.requires_auth(&path, false);

            let is_health = path == "/healthz" || path == "/api/health" || path == "/health";

            match effective {
                ProxyAuthMode::Strict => {
                    // Strict: ALL routes require auth, including health
                    prop_assert!(
                        requires,
                        "strict mode: path {:?} should require auth but got false",
                        path
                    );
                }
                ProxyAuthMode::AllExceptHealth => {
                    if is_health {
                        prop_assert!(
                            !requires,
                            "all_except_health mode: health path {:?} should NOT require auth",
                            path
                        );
                    } else {
                        prop_assert!(
                            requires,
                            "all_except_health mode: non-health path {:?} should require auth",
                            path
                        );
                    }
                }
                ProxyAuthMode::Off => {
                    // Off: NO routes require auth
                    prop_assert!(
                        !requires,
                        "off mode: path {:?} should NOT require auth but got true",
                        path
                    );
                }
                ProxyAuthMode::Auto => {
                    // Auto resolves to Off (local) or AllExceptHealth (LAN)
                    // This case should not happen since effective_auth_mode() resolves Auto
                    prop_assert!(false, "effective_auth_mode should never return Auto");
                }
            }
        }
    }
}
