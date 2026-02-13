// 上游客户端实现
// 基于 reqwest 封装，支持上游代理和代理池

use dashmap::DashMap;
use reqwest::{header, Client, Response, StatusCode};
use serde_json::Value;
use tokio::sync::RwLock;
use tokio::time::Duration;

use crate::models::config::UpstreamProxyConfig;

/// 默认 User-Agent
const DEFAULT_USER_AGENT: &str = "kiro-ai-gateway/1.0";

/// 端点降级尝试的记录信息
#[derive(Debug, Clone)]
pub struct FallbackAttemptLog {
    pub endpoint_url: String,
    pub status: Option<u16>,
    pub error: String,
}

/// 上游调用结果
pub struct UpstreamCallResult {
    pub response: Response,
    pub fallback_attempts: Vec<FallbackAttemptLog>,
}

/// 邮箱脱敏
pub fn mask_email(email: &str) -> String {
    if let Some(at_pos) = email.find('@') {
        let local = &email[..at_pos];
        let domain = &email[at_pos + 1..];
        let local_prefix: String = local.chars().take(3).collect();
        let domain_prefix: String = domain.chars().take(2).collect();
        format!("{}***@{}***", local_prefix, domain_prefix)
    } else {
        let prefix: String = email.chars().take(5).collect();
        format!("{}***", prefix)
    }
}

// Cloud Code v1internal endpoints (fallback order: Sandbox → Daily → Prod)
const V1_INTERNAL_BASE_URL_PROD: &str = "https://cloudcode-pa.googleapis.com/v1internal";
const V1_INTERNAL_BASE_URL_DAILY: &str = "https://daily-cloudcode-pa.googleapis.com/v1internal";
const V1_INTERNAL_BASE_URL_SANDBOX: &str =
    "https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal";

const V1_INTERNAL_BASE_URL_FALLBACKS: [&str; 3] = [
    V1_INTERNAL_BASE_URL_SANDBOX,
    V1_INTERNAL_BASE_URL_DAILY,
    V1_INTERNAL_BASE_URL_PROD,
];

/// 代理池代理配置（简化版，供 client 使用）
#[derive(Clone)]
pub struct PoolProxyConfig {
    pub entry_id: String,
    pub proxy: reqwest::Proxy,
}

/// 标准化代理 URL（确保有协议前缀）
pub fn normalize_proxy_url(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("socks5://")
        || trimmed.starts_with("socks5h://")
    {
        trimmed.to_string()
    } else {
        format!("http://{}", trimmed)
    }
}

pub struct UpstreamClient {
    default_client: Client,
    client_cache: DashMap<String, Client>,
    user_agent_override: RwLock<Option<String>>,
}

impl UpstreamClient {
    pub fn new(proxy_config: Option<UpstreamProxyConfig>) -> Self {
        let default_client =
            Self::build_client_internal(proxy_config).expect("Failed to create default HTTP client");

        Self {
            default_client,
            client_cache: DashMap::new(),
            user_agent_override: RwLock::new(None),
        }
    }

    /// Build client with optional upstream proxy
    fn build_client_internal(
        proxy_config: Option<UpstreamProxyConfig>,
    ) -> Result<Client, reqwest::Error> {
        let mut builder = Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .pool_max_idle_per_host(16)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .timeout(Duration::from_secs(600))
            .user_agent(DEFAULT_USER_AGENT);

        if let Some(config) = proxy_config {
            if config.enabled && !config.url.is_empty() {
                let url = normalize_proxy_url(&config.url);
                if let Ok(proxy) = reqwest::Proxy::all(&url) {
                    builder = builder.proxy(proxy);
                    tracing::info!("UpstreamClient enabled proxy: {}", url);
                }
            }
        }

        builder.build()
    }

    /// Build a client with a specific pool proxy
    fn build_client_with_proxy(
        proxy_config: PoolProxyConfig,
    ) -> Result<Client, reqwest::Error> {
        Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .pool_max_idle_per_host(16)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .timeout(Duration::from_secs(600))
            .user_agent(DEFAULT_USER_AGENT)
            .proxy(proxy_config.proxy)
            .build()
    }

    /// Set dynamic User-Agent override
    pub async fn set_user_agent_override(&self, ua: Option<String>) {
        let mut lock = self.user_agent_override.write().await;
        *lock = ua;
        tracing::debug!("UpstreamClient User-Agent override updated");
    }

    /// Get current User-Agent
    pub async fn get_user_agent(&self) -> String {
        let ua_override = self.user_agent_override.read().await;
        ua_override
            .as_ref()
            .cloned()
            .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string())
    }

    /// Get client for a specific account (uses default if no proxy pool binding)
    pub async fn get_client(&self, _account_id: Option<&str>) -> Client {
        // Proxy pool integration will be added when proxy_pool module is wired up.
        // For now, always return the default client.
        self.default_client.clone()
    }

    /// Get client with a specific pool proxy config, caching by entry_id
    pub async fn get_client_with_pool_proxy(&self, proxy_cfg: PoolProxyConfig) -> Client {
        if let Some(client) = self.client_cache.get(&proxy_cfg.entry_id) {
            return client.clone();
        }
        match Self::build_client_with_proxy(proxy_cfg.clone()) {
            Ok(client) => {
                self.client_cache
                    .insert(proxy_cfg.entry_id.clone(), client.clone());
                client
            }
            Err(e) => {
                tracing::error!(
                    "Failed to build client for proxy {}: {}, falling back to default",
                    proxy_cfg.entry_id,
                    e
                );
                self.default_client.clone()
            }
        }
    }

    /// Build v1internal URL
    fn build_url(base_url: &str, method: &str, query_string: Option<&str>) -> String {
        if let Some(qs) = query_string {
            format!("{}:{}?{}", base_url, method, qs)
        } else {
            format!("{}:{}", base_url, method)
        }
    }

    /// Determine if we should try next endpoint
    fn should_try_next_endpoint(status: StatusCode) -> bool {
        status == StatusCode::TOO_MANY_REQUESTS
            || status == StatusCode::REQUEST_TIMEOUT
            || status == StatusCode::NOT_FOUND
            || status.is_server_error()
    }

    /// Call v1internal API with multi-endpoint fallback
    pub async fn call_v1_internal(
        &self,
        method: &str,
        access_token: &str,
        body: Value,
        query_string: Option<&str>,
        account_id: Option<&str>,
    ) -> Result<UpstreamCallResult, String> {
        self.call_v1_internal_with_headers(
            method,
            access_token,
            body,
            query_string,
            std::collections::HashMap::new(),
            account_id,
        )
        .await
    }

    /// Call v1internal API with extra headers and multi-endpoint fallback
    pub async fn call_v1_internal_with_headers(
        &self,
        method: &str,
        access_token: &str,
        body: Value,
        query_string: Option<&str>,
        extra_headers: std::collections::HashMap<String, String>,
        account_id: Option<&str>,
    ) -> Result<UpstreamCallResult, String> {
        let client = self.get_client(account_id).await;

        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {}", access_token))
                .map_err(|e| e.to_string())?,
        );
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_str(&self.get_user_agent().await).unwrap_or_else(|e| {
                tracing::warn!("Invalid User-Agent header value: {}", e);
                header::HeaderValue::from_static(DEFAULT_USER_AGENT)
            }),
        );

        for (k, v) in extra_headers {
            if let Ok(hk) = header::HeaderName::from_bytes(k.as_bytes()) {
                if let Ok(hv) = header::HeaderValue::from_str(&v) {
                    headers.insert(hk, hv);
                }
            }
        }

        let mut last_err: Option<String> = None;
        let mut fallback_attempts: Vec<FallbackAttemptLog> = Vec::new();

        for (idx, base_url) in V1_INTERNAL_BASE_URL_FALLBACKS.iter().enumerate() {
            let url = Self::build_url(base_url, method, query_string);
            let has_next = idx + 1 < V1_INTERNAL_BASE_URL_FALLBACKS.len();

            let response = client
                .post(&url)
                .headers(headers.clone())
                .json(&body)
                .send()
                .await;

            match response {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        if idx > 0 {
                            tracing::info!(
                                "✓ Upstream fallback succeeded | Endpoint: {} | Status: {}",
                                base_url,
                                status
                            );
                        }
                        return Ok(UpstreamCallResult {
                            response: resp,
                            fallback_attempts,
                        });
                    }

                    if has_next && Self::should_try_next_endpoint(status) {
                        let err_msg = format!("Upstream {} returned {}", base_url, status);
                        tracing::warn!(
                            "Upstream endpoint returned {} at {} (method={}), trying next",
                            status,
                            base_url,
                            method
                        );
                        fallback_attempts.push(FallbackAttemptLog {
                            endpoint_url: url.clone(),
                            status: Some(status.as_u16()),
                            error: err_msg.clone(),
                        });
                        last_err = Some(err_msg);
                        continue;
                    }

                    return Ok(UpstreamCallResult {
                        response: resp,
                        fallback_attempts,
                    });
                }
                Err(e) => {
                    let msg = format!("HTTP request failed at {}: {}", base_url, e);
                    tracing::debug!("{}", msg);
                    fallback_attempts.push(FallbackAttemptLog {
                        endpoint_url: url.clone(),
                        status: None,
                        error: msg.clone(),
                    });
                    last_err = Some(msg);
                    if !has_next {
                        break;
                    }
                    continue;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| "All endpoints failed".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_url() {
        let base = "https://cloudcode-pa.googleapis.com/v1internal";
        assert_eq!(
            UpstreamClient::build_url(base, "generateContent", None),
            "https://cloudcode-pa.googleapis.com/v1internal:generateContent"
        );
        assert_eq!(
            UpstreamClient::build_url(base, "streamGenerateContent", Some("alt=sse")),
            "https://cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn test_mask_email() {
        assert_eq!(mask_email("userexample@gmail.com"), "use***@gm***");
        assert_eq!(mask_email("ab@x.com"), "ab***@x.***");
        assert_eq!(mask_email("noemail"), "noema***");
    }

    #[test]
    fn test_normalize_proxy_url() {
        assert_eq!(normalize_proxy_url("http://proxy:8080"), "http://proxy:8080");
        assert_eq!(normalize_proxy_url("socks5://proxy:1080"), "socks5://proxy:1080");
        assert_eq!(normalize_proxy_url("proxy:8080"), "http://proxy:8080");
        assert_eq!(normalize_proxy_url("  https://p  "), "https://p");
    }

    #[test]
    fn test_should_try_next_endpoint() {
        assert!(UpstreamClient::should_try_next_endpoint(StatusCode::TOO_MANY_REQUESTS));
        assert!(UpstreamClient::should_try_next_endpoint(StatusCode::REQUEST_TIMEOUT));
        assert!(UpstreamClient::should_try_next_endpoint(StatusCode::NOT_FOUND));
        assert!(UpstreamClient::should_try_next_endpoint(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(!UpstreamClient::should_try_next_endpoint(StatusCode::FORBIDDEN));
        assert!(!UpstreamClient::should_try_next_endpoint(StatusCode::OK));
    }

    #[tokio::test]
    async fn test_user_agent_override() {
        let client = UpstreamClient::new(None);
        assert_eq!(client.get_user_agent().await, DEFAULT_USER_AGENT);

        client.set_user_agent_override(Some("custom-agent/2.0".to_string())).await;
        assert_eq!(client.get_user_agent().await, "custom-agent/2.0");

        client.set_user_agent_override(None).await;
        assert_eq!(client.get_user_agent().await, DEFAULT_USER_AGENT);
    }
}
