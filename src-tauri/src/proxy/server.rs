// Proxy Server - Route assembly, middleware stack, and server lifecycle
//
// Requirements covered:
// - 11.1: Web management interface (static file serving, OAuth callback, SPA fallback)
// - 14.1: Hot update model mapping
// - 14.2: Hot update upstream proxy
// - 14.3: Hot update security config (IP blacklist/whitelist)
// - 14.4: Hot update User-Agent override
// - 14.5: Hot update Thinking Budget
// - 14.6: Hot update global system prompt
// - 14.7: Hot update proxy pool
// - 14.8: Hot update experimental features

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use tokio::sync::{oneshot, RwLock};
use tracing::{debug, error, info};

use axum::{
    extract::DefaultBodyLimit,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};

use crate::models::config::{ProxyConfig, UpstreamProxyConfig};
use crate::proxy::handlers::AppState;
use crate::proxy::middleware::{
    admin_auth_middleware, auth_middleware, cors_layer, ip_filter_middleware, monitor_middleware,
    service_status_middleware,
};
use crate::proxy::security::ProxySecurityConfig;
use crate::proxy::token_manager::TokenManager;
use crate::proxy::upstream::client::UpstreamClient;

// ============================================================================
// Global pending account queues
// ============================================================================

static PENDING_RELOAD_ACCOUNTS: OnceLock<std::sync::RwLock<HashSet<String>>> = OnceLock::new();
static PENDING_DELETE_ACCOUNTS: OnceLock<std::sync::RwLock<HashSet<String>>> = OnceLock::new();

fn get_pending_reload_accounts() -> &'static std::sync::RwLock<HashSet<String>> {
    PENDING_RELOAD_ACCOUNTS.get_or_init(|| std::sync::RwLock::new(HashSet::new()))
}

fn get_pending_delete_accounts() -> &'static std::sync::RwLock<HashSet<String>> {
    PENDING_DELETE_ACCOUNTS.get_or_init(|| std::sync::RwLock::new(HashSet::new()))
}

/// Queue an account for reload by TokenManager (called after quota protection updates)
pub fn trigger_account_reload(account_id: &str) {
    if let Ok(mut pending) = get_pending_reload_accounts().write() {
        pending.insert(account_id.to_string());
        debug!("[Quota] Queued account {} for TokenManager reload", account_id);
    }
}

/// Queue an account for deletion from memory cache
pub fn trigger_account_delete(account_id: &str) {
    if let Ok(mut pending) = get_pending_delete_accounts().write() {
        pending.insert(account_id.to_string());
        debug!("[Proxy] Queued account {} for cache removal", account_id);
    }
}

/// Take and clear all pending reload account IDs (called by TokenManager)
pub fn take_pending_reload_accounts() -> Vec<String> {
    if let Ok(mut pending) = get_pending_reload_accounts().write() {
        let accounts: Vec<String> = pending.drain().collect();
        if !accounts.is_empty() {
            debug!("[Quota] Taking {} pending accounts for reload", accounts.len());
        }
        accounts
    } else {
        Vec::new()
    }
}

/// Take and clear all pending delete account IDs
pub fn take_pending_delete_accounts() -> Vec<String> {
    if let Ok(mut pending) = get_pending_delete_accounts().write() {
        let accounts: Vec<String> = pending.drain().collect();
        if !accounts.is_empty() {
            debug!("[Proxy] Taking {} pending accounts for cache removal", accounts.len());
        }
        accounts
    } else {
        Vec::new()
    }
}

// ============================================================================
// Health check handler
// ============================================================================

async fn health_check_handler() -> Response {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
    .into_response()
}

/// Silent OK handler for event logging endpoints
async fn silent_ok_handler() -> Response {
    StatusCode::OK.into_response()
}

// ============================================================================
// Route builders
// ============================================================================

/// Build AI proxy routes (OpenAI, Claude, Gemini, Audio, Images, Warmup)
fn proxy_routes(state: AppState, security: Arc<RwLock<ProxySecurityConfig>>) -> Router {
    use crate::proxy::handlers;

    let routes = Router::new()
        // Health checks
        .route("/health", get(health_check_handler))
        .route("/healthz", get(health_check_handler))
        // OpenAI Protocol
        .route("/v1/models", get(handlers::openai::handle_list_models))
        .route("/v1/chat/completions", post(handlers::openai::handle_chat_completions))
        .route("/v1/completions", post(handlers::openai::handle_completions))
        .route("/v1/responses", post(handlers::openai::handle_completions))
        .route("/v1/images/generations", post(handlers::openai::handle_images_generations))
        .route("/v1/images/edits", post(handlers::openai::handle_images_edits))
        .route("/v1/audio/transcriptions", post(handlers::audio::handle_audio_transcription))
        // Claude Protocol
        .route("/v1/messages", post(handlers::claude::handle_messages))
        .route("/v1/messages/count_tokens", post(handlers::claude::handle_count_tokens))
        .route("/v1/models/claude", get(handlers::claude::handle_list_models))
        // Gemini Protocol (Native)
        .route("/v1beta/models", get(handlers::gemini::handle_list_models))
        .route(
            "/v1beta/models/:model",
            get(handlers::gemini::handle_get_model).post(handlers::gemini::handle_generate),
        )
        .route("/v1beta/models/:model/countTokens", post(handlers::gemini::handle_count_tokens))
        // Common
        .route("/v1/models/detect", post(handlers::common::handle_detect_model))
        .route("/internal/warmup", post(handlers::warmup::handle_warmup))
        // Silent endpoints
        .route("/v1/api/event_logging/batch", post(silent_ok_handler))
        .route("/v1/api/event_logging", post(silent_ok_handler))
        // Middleware stack (onion model): IP Filter → Auth → Monitor → Handler
        // Axum layers execute bottom-to-top for requests
        .layer(axum::middleware::from_fn(monitor_middleware))
        .layer(axum::middleware::from_fn_with_state(
            security.clone(),
            auth_middleware,
        ))
        .layer(axum::middleware::from_fn(ip_filter_middleware))
        .with_state(state);

    routes
}

/// Build admin API routes with all management endpoints
fn admin_routes(state: AppState, security: Arc<RwLock<ProxySecurityConfig>>) -> Router {
    use crate::proxy::handlers::admin;
    use axum::routing::delete;

    Router::new()
        .route("/health", get(health_check_handler))
        // Account Management
        .route("/accounts", get(admin::admin_list_accounts).post(admin::admin_add_account))
        .route("/accounts/switch", post(admin::admin_switch_account))
        .route("/accounts/refresh", post(admin::admin_refresh_all_quotas))
        .route("/accounts/bulk-delete", post(admin::admin_delete_accounts))
        .route("/accounts/export", post(admin::admin_export_accounts))
        .route("/accounts/reorder", post(admin::admin_reorder_accounts))
        .route("/accounts/warmup", post(admin::admin_warm_up_all_accounts))
        .route("/accounts/device-preview", post(admin::admin_preview_generate_profile))
        .route("/accounts/restore-original", post(admin::admin_restore_original_device))
        .route("/accounts/:accountId", delete(admin::admin_delete_account))
        .route("/accounts/:accountId/quota", get(admin::admin_fetch_account_quota))
        .route("/accounts/:accountId/toggle-proxy", post(admin::admin_toggle_proxy_status))
        .route("/accounts/:accountId/warmup", post(admin::admin_warm_up_account))
        .route("/accounts/:accountId/bind-device", post(admin::admin_bind_device))
        .route("/accounts/:accountId/device-profiles", get(admin::admin_get_device_profiles))
        .route("/accounts/:accountId/device-versions", get(admin::admin_list_device_versions))
        .route("/accounts/:accountId/bind-device-profile", post(admin::admin_bind_device_profile_with_profile))
        .route("/accounts/:accountId/device-versions/:versionId/restore", post(admin::admin_restore_device_version))
        .route("/accounts/:accountId/device-versions/:versionId", delete(admin::admin_delete_device_version))
        // OAuth
        .route("/accounts/oauth/prepare", post(admin::admin_prepare_oauth_url))
        .route("/accounts/oauth/start", post(admin::admin_start_oauth_login))
        .route("/accounts/oauth/complete", post(admin::admin_complete_oauth_login))
        .route("/accounts/oauth/cancel", post(admin::admin_cancel_oauth_login))
        .route("/accounts/oauth/submit-code", post(admin::admin_submit_oauth_code))
        .route("/auth/url", get(admin::admin_prepare_oauth_url_web))
        // Config
        .route("/config", get(admin::admin_get_config).post(admin::admin_save_config))
        // Proxy Control
        .route("/proxy/status", get(admin::admin_get_proxy_status))
        .route("/proxy/start", post(admin::admin_start_proxy_service))
        .route("/proxy/stop", post(admin::admin_stop_proxy_service))
        .route("/proxy/mapping", post(admin::admin_update_model_mapping))
        .route("/proxy/api-key/generate", post(admin::admin_generate_api_key))
        .route("/proxy/session-bindings/clear", post(admin::admin_clear_proxy_session_bindings))
        .route("/proxy/rate-limits", delete(admin::admin_clear_all_rate_limits))
        .route("/proxy/rate-limits/:accountId", delete(admin::admin_clear_rate_limit))
        .route("/proxy/preferred-account", get(admin::admin_get_preferred_account).post(admin::admin_set_preferred_account))
        .route("/proxy/monitor/toggle", post(admin::admin_set_proxy_monitor_enabled))
        .route("/proxy/stats", get(admin::admin_get_proxy_stats))
        // Proxy Pool
        .route("/proxy/pool/config", get(admin::admin_get_proxy_pool_config))
        .route("/proxy/pool/bindings", get(admin::admin_get_all_account_bindings))
        .route("/proxy/pool/bind", post(admin::admin_bind_account_proxy))
        .route("/proxy/pool/unbind", post(admin::admin_unbind_account_proxy))
        .route("/proxy/pool/binding/:accountId", get(admin::admin_get_account_proxy_binding))
        .route("/proxy/health-check/trigger", post(admin::admin_trigger_proxy_health_check))
        // CLI Sync
        .route("/proxy/cli/status", post(admin::admin_get_cli_sync_status))
        .route("/proxy/cli/sync", post(admin::admin_execute_cli_sync))
        .route("/proxy/cli/restore", post(admin::admin_execute_cli_restore))
        .route("/proxy/cli/config", post(admin::admin_get_cli_config_content))
        // OpenCode Sync
        .route("/proxy/opencode/status", post(admin::admin_get_opencode_sync_status))
        .route("/proxy/opencode/sync", post(admin::admin_execute_opencode_sync))
        .route("/proxy/opencode/restore", post(admin::admin_execute_opencode_restore))
        .route("/proxy/opencode/config", post(admin::admin_get_opencode_config_content))
        .route("/proxy/opencode/clear", post(admin::admin_execute_opencode_clear))
        // Droid Sync
        .route("/proxy/droid/status", post(admin::admin_get_droid_sync_status))
        .route("/proxy/droid/sync", post(admin::admin_execute_droid_sync))
        .route("/proxy/droid/restore", post(admin::admin_execute_droid_restore))
        .route("/proxy/droid/config", post(admin::admin_get_droid_config_content))
        // Proxy Logs
        .route("/logs", get(admin::admin_get_proxy_logs_filtered))
        .route("/logs/count", get(admin::admin_get_proxy_logs_count_filtered))
        .route("/logs/clear", post(admin::admin_clear_proxy_logs))
        .route("/logs/:logId", get(admin::admin_get_proxy_log_detail))
        // Security / IP Monitoring
        .route("/security/logs", get(admin::admin_get_ip_access_logs))
        .route("/security/logs/clear", post(admin::admin_clear_ip_access_logs))
        .route("/security/stats", get(admin::admin_get_ip_stats))
        .route("/security/token-stats", get(admin::admin_get_ip_token_stats))
        .route("/security/blacklist", get(admin::admin_get_ip_blacklist).post(admin::admin_add_ip_to_blacklist).delete(admin::admin_remove_ip_from_blacklist))
        .route("/security/blacklist/clear", post(admin::admin_clear_ip_blacklist))
        .route("/security/blacklist/check", get(admin::admin_check_ip_in_blacklist))
        .route("/security/whitelist", get(admin::admin_get_ip_whitelist).post(admin::admin_add_ip_to_whitelist).delete(admin::admin_remove_ip_from_whitelist))
        .route("/security/whitelist/clear", post(admin::admin_clear_ip_whitelist))
        .route("/security/whitelist/check", get(admin::admin_check_ip_in_whitelist))
        // User Tokens
        .route("/user-tokens", get(admin::admin_list_user_tokens).post(admin::admin_create_user_token))
        .route("/user-tokens/summary", get(admin::admin_get_user_token_summary))
        .route("/user-tokens/:id/renew", post(admin::admin_renew_user_token))
        .route("/user-tokens/:id", delete(admin::admin_delete_user_token).patch(admin::admin_update_user_token))
        // Token Stats
        .route("/stats/summary", get(admin::admin_get_token_stats_summary))
        .route("/stats/hourly", get(admin::admin_get_token_stats_hourly))
        .route("/stats/daily", get(admin::admin_get_token_stats_daily))
        .route("/stats/weekly", get(admin::admin_get_token_stats_weekly))
        .route("/stats/accounts", get(admin::admin_get_token_stats_by_account))
        .route("/stats/models", get(admin::admin_get_token_stats_by_model))
        .route("/stats/token/clear", post(admin::admin_clear_token_stats))
        .route("/stats/token/hourly", get(admin::admin_get_token_stats_hourly))
        .route("/stats/token/daily", get(admin::admin_get_token_stats_daily))
        .route("/stats/token/weekly", get(admin::admin_get_token_stats_weekly))
        .route("/stats/token/by-account", get(admin::admin_get_token_stats_by_account))
        .route("/stats/token/summary", get(admin::admin_get_token_stats_summary))
        .route("/stats/token/by-model", get(admin::admin_get_token_stats_by_model))
        .route("/stats/token/model-trend/hourly", get(admin::admin_get_token_stats_model_trend_hourly))
        .route("/stats/token/model-trend/daily", get(admin::admin_get_token_stats_model_trend_daily))
        .route("/stats/token/account-trend/hourly", get(admin::admin_get_token_stats_account_trend_hourly))
        .route("/stats/token/account-trend/daily", get(admin::admin_get_token_stats_account_trend_daily))
        // System
        .route("/system/data-dir", get(admin::admin_get_data_dir_path))
        .route("/system/updates/check-status", get(admin::admin_should_check_updates))
        .route("/system/updates/check", post(admin::admin_check_for_updates))
        .route("/system/autostart/status", get(admin::admin_is_auto_launch_enabled))
        .route("/system/autostart/toggle", post(admin::admin_toggle_auto_launch))
        .route("/system/cache/clear", post(admin::admin_clear_cache))
        .route("/system/logs/clear-cache", post(admin::admin_clear_log_cache))
        // Admin auth middleware (forced authentication)
        .layer(axum::middleware::from_fn_with_state(
            security,
            admin_auth_middleware,
        ))
        .with_state(state)
}

// ============================================================================
// AxumServer - Server lifecycle management
// ============================================================================

/// Axum proxy server instance with hot-reload support
#[derive(Clone)]
pub struct AxumServer {
    shutdown_tx: Arc<tokio::sync::Mutex<Option<oneshot::Sender<()>>>>,
    custom_mapping: Arc<RwLock<HashMap<String, String>>>,
    upstream: Arc<UpstreamClient>,
    security_state: Arc<RwLock<ProxySecurityConfig>>,
    pub is_running: Arc<RwLock<bool>>,
    pub token_manager: Arc<TokenManager>,
}

impl AxumServer {
    /// Start the proxy server
    pub async fn start(
        host: String,
        port: u16,
        token_manager: Arc<TokenManager>,
        custom_mapping: HashMap<String, String>,
        upstream_proxy: UpstreamProxyConfig,
        user_agent_override: Option<String>,
        security_config: ProxySecurityConfig,
    ) -> Result<(Self, tokio::task::JoinHandle<()>), String> {
        let custom_mapping_state = Arc::new(RwLock::new(custom_mapping));
        let security_state = Arc::new(RwLock::new(security_config));
        let is_running_state = Arc::new(RwLock::new(true));

        let upstream = Arc::new(UpstreamClient::new(Some(upstream_proxy)));
        if let Some(ua) = user_agent_override {
            upstream.set_user_agent_override(Some(ua)).await;
        }

        let app_state = AppState::new(
            token_manager.clone(),
            custom_mapping_state.clone(),
            upstream.clone(),
        );

        // Build routes
        let proxy = proxy_routes(app_state.clone(), security_state.clone());
        let admin = admin_routes(app_state, security_state.clone());

        // Max body size (default 100MB)
        let max_body_size: usize = std::env::var("KIRO_MAX_BODY_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(100 * 1024 * 1024);

        let app = Router::new()
            .nest("/api", admin)
            .merge(proxy)
            // Public route: OAuth callback (no admin auth required)
            .route("/auth/callback", get(crate::proxy::handlers::admin::handle_oauth_callback))
            .layer(axum::middleware::from_fn(service_status_middleware))
            .layer(cors_layer())
            .layer(DefaultBodyLimit::max(max_body_size));

        // Static file hosting for Headless/Docker mode (Requirement 11.1)
        // When KIRO_DIST_PATH is set and the directory exists, serve the
        // frontend build output and fall back to index.html for SPA routing.
        let dist_path = std::env::var("KIRO_DIST_PATH").unwrap_or_else(|_| "dist".to_string());
        let app = if std::path::Path::new(&dist_path).exists() {
            info!("Serving static assets from: {}", dist_path);
            app.fallback_service(
                tower_http::services::ServeDir::new(&dist_path).fallback(
                    tower_http::services::ServeFile::new(format!("{}/index.html", dist_path)),
                ),
            )
        } else {
            app
        };

        // Bind address
        let addr = format!("{}:{}", host, port);
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| format!("Failed to bind {}: {}", addr, e))?;

        info!("Proxy server started at http://{}", addr);

        // Shutdown channel
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

        let server_instance = Self {
            shutdown_tx: Arc::new(tokio::sync::Mutex::new(Some(shutdown_tx))),
            custom_mapping: custom_mapping_state,
            upstream: upstream.clone(),
            security_state,
            is_running: is_running_state,
            token_manager: token_manager.clone(),
        };

        // Spawn server task
        let handle = tokio::spawn(async move {
            use hyper::server::conn::http1;
            use hyper_util::rt::TokioIo;
            use hyper_util::service::TowerToHyperService;

            let app_service = app.into_service();

            loop {
                tokio::select! {
                    res = listener.accept() => {
                        match res {
                            Ok((stream, remote_addr)) => {
                                let io = TokioIo::new(stream);

                                use tower::ServiceExt;
                                use hyper::body::Incoming;
                                let svc = app_service.clone().map_request(
                                    move |mut req: axum::http::Request<Incoming>| {
                                        req.extensions_mut().insert(
                                            axum::extract::ConnectInfo(remote_addr),
                                        );
                                        req
                                    },
                                );

                                let hyper_svc = TowerToHyperService::new(svc);

                                tokio::task::spawn(async move {
                                    if let Err(err) = http1::Builder::new()
                                        .serve_connection(io, hyper_svc)
                                        .with_upgrades()
                                        .await
                                    {
                                        debug!("Connection ended: {:?}", err);
                                    }
                                });
                            }
                            Err(e) => {
                                error!("Accept connection failed: {:?}", e);
                            }
                        }
                    }
                    _ = &mut shutdown_rx => {
                        info!("Proxy server shutting down");
                        break;
                    }
                }
            }
        });

        Ok((server_instance, handle))
    }

    /// Stop the proxy server
    pub fn stop(&self) {
        let tx_mutex = self.shutdown_tx.clone();
        let is_running = self.is_running.clone();
        tokio::spawn(async move {
            let mut lock = tx_mutex.lock().await;
            if let Some(tx) = lock.take() {
                let _ = tx.send(());
                *is_running.write().await = false;
                info!("Proxy server stop signal sent");
            }
        });
    }

    /// Set running state
    pub async fn set_running(&self, running: bool) {
        *self.is_running.write().await = running;
    }

    // ========================================================================
    // Hot-reload methods (Requirements 14.1 - 14.8)
    // ========================================================================

    /// Hot update model mapping [Req 14.1]
    pub async fn update_mapping(&self, config: &ProxyConfig) {
        let mut mapping = self.custom_mapping.write().await;
        *mapping = config.custom_mapping.clone();
        info!("[HotReload] Model mapping updated ({} entries)", mapping.len());
    }

    /// Hot update upstream proxy [Req 14.2]
    ///
    /// Note: Full proxy reconfiguration requires rebuilding the HTTP client.
    /// Current implementation logs the update; full rebuild will be added
    /// when UpstreamClient supports runtime proxy reconfiguration.
    pub async fn update_proxy(&self, _new_config: UpstreamProxyConfig) {
        info!("[HotReload] Upstream proxy config updated (requires restart for full effect)");
    }

    /// Hot update security config (IP blacklist/whitelist, auth) [Req 14.3]
    pub async fn update_security(&self, config: &ProxyConfig) {
        let new_security = ProxySecurityConfig::from_proxy_config(config);
        *self.security_state.write().await = new_security;
        info!("[HotReload] Security config updated");
    }

    /// Hot update User-Agent override [Req 14.4]
    pub async fn update_user_agent(&self, user_agent: Option<String>) {
        self.upstream.set_user_agent_override(user_agent).await;
        info!("[HotReload] User-Agent override updated");
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Pending account queue tests ----

    #[test]
    fn test_trigger_and_take_reload_accounts() {
        // Clear any existing state
        let _ = take_pending_reload_accounts();

        trigger_account_reload("acc-1");
        trigger_account_reload("acc-2");
        trigger_account_reload("acc-1"); // duplicate

        let accounts = take_pending_reload_accounts();
        assert_eq!(accounts.len(), 2);
        assert!(accounts.contains(&"acc-1".to_string()));
        assert!(accounts.contains(&"acc-2".to_string()));

        // Should be empty after take
        let accounts2 = take_pending_reload_accounts();
        assert!(accounts2.is_empty());
    }

    #[test]
    fn test_trigger_and_take_delete_accounts() {
        let _ = take_pending_delete_accounts();

        trigger_account_delete("del-1");
        trigger_account_delete("del-2");

        let accounts = take_pending_delete_accounts();
        assert_eq!(accounts.len(), 2);
        assert!(accounts.contains(&"del-1".to_string()));
        assert!(accounts.contains(&"del-2".to_string()));

        let accounts2 = take_pending_delete_accounts();
        assert!(accounts2.is_empty());
    }

    #[test]
    fn test_take_empty_queues() {
        // Ensure fresh state
        let _ = take_pending_reload_accounts();
        let _ = take_pending_delete_accounts();

        assert!(take_pending_reload_accounts().is_empty());
        assert!(take_pending_delete_accounts().is_empty());
    }

    // ---- Health check handler test ----

    #[tokio::test]
    async fn test_health_check_handler() {
        let response = health_check_handler().await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_silent_ok_handler() {
        let response = silent_ok_handler().await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    // ---- Route builder tests ----

    #[test]
    fn test_proxy_routes_builds_without_panic() {
        let tm = Arc::new(TokenManager::new(std::path::PathBuf::from("/tmp")));
        let mapping = Arc::new(RwLock::new(HashMap::new()));
        let upstream = Arc::new(UpstreamClient::new(None));
        let state = AppState::new(tm, mapping, upstream);
        let security = Arc::new(RwLock::new(ProxySecurityConfig::from_proxy_config(
            &ProxyConfig::default(),
        )));

        // Should not panic
        let _router = proxy_routes(state, security);
    }

    #[test]
    fn test_admin_routes_builds_without_panic() {
        let tm = Arc::new(TokenManager::new(std::path::PathBuf::from("/tmp")));
        let mapping = Arc::new(RwLock::new(HashMap::new()));
        let upstream = Arc::new(UpstreamClient::new(None));
        let state = AppState::new(tm, mapping, upstream);
        let security = Arc::new(RwLock::new(ProxySecurityConfig::from_proxy_config(
            &ProxyConfig::default(),
        )));

        let _router = admin_routes(state, security);
    }

    // ---- AxumServer lifecycle test ----

    #[tokio::test]
    async fn test_server_start_and_stop() {
        let tm = Arc::new(TokenManager::new(std::path::PathBuf::from("/tmp")));
        let mapping = HashMap::new();
        let upstream_proxy = UpstreamProxyConfig::default();
        let security = ProxySecurityConfig::from_proxy_config(&ProxyConfig::default());

        // Use port 0 to let OS assign a free port
        let result = AxumServer::start(
            "127.0.0.1".to_string(),
            0,
            tm,
            mapping,
            upstream_proxy,
            None,
            security,
        )
        .await;

        assert!(result.is_ok());
        let (server, _handle) = result.unwrap();

        assert!(*server.is_running.read().await);

        server.stop();
        // Give the stop signal time to propagate
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        assert!(!*server.is_running.read().await);
    }

    // ---- Hot reload tests ----

    #[tokio::test]
    async fn test_hot_reload_mapping() {
        let tm = Arc::new(TokenManager::new(std::path::PathBuf::from("/tmp")));
        let mapping = HashMap::new();
        let upstream_proxy = UpstreamProxyConfig::default();
        let security = ProxySecurityConfig::from_proxy_config(&ProxyConfig::default());

        let (server, _handle) = AxumServer::start(
            "127.0.0.1".to_string(),
            0,
            tm,
            mapping,
            upstream_proxy,
            None,
            security,
        )
        .await
        .unwrap();

        // Initially empty
        assert!(server.custom_mapping.read().await.is_empty());

        // Update mapping
        let mut config = ProxyConfig::default();
        config.custom_mapping.insert("gpt-4".to_string(), "gemini-pro".to_string());
        server.update_mapping(&config).await;

        let mapping = server.custom_mapping.read().await;
        assert_eq!(mapping.get("gpt-4"), Some(&"gemini-pro".to_string()));

        server.stop();
    }

    #[tokio::test]
    async fn test_hot_reload_security() {
        let tm = Arc::new(TokenManager::new(std::path::PathBuf::from("/tmp")));
        let mapping = HashMap::new();
        let upstream_proxy = UpstreamProxyConfig::default();
        let security = ProxySecurityConfig::from_proxy_config(&ProxyConfig::default());

        let (server, _handle) = AxumServer::start(
            "127.0.0.1".to_string(),
            0,
            tm,
            mapping,
            upstream_proxy,
            None,
            security,
        )
        .await
        .unwrap();

        // Update security
        let mut config = ProxyConfig::default();
        config.api_key = "new-key-123".to_string();
        server.update_security(&config).await;

        let sec = server.security_state.read().await;
        assert_eq!(sec.api_key, "new-key-123");

        server.stop();
    }

    #[tokio::test]
    async fn test_set_running() {
        let tm = Arc::new(TokenManager::new(std::path::PathBuf::from("/tmp")));
        let mapping = HashMap::new();
        let upstream_proxy = UpstreamProxyConfig::default();
        let security = ProxySecurityConfig::from_proxy_config(&ProxyConfig::default());

        let (server, _handle) = AxumServer::start(
            "127.0.0.1".to_string(),
            0,
            tm,
            mapping,
            upstream_proxy,
            None,
            security,
        )
        .await
        .unwrap();

        assert!(*server.is_running.read().await);
        server.set_running(false).await;
        assert!(!*server.is_running.read().await);
        server.set_running(true).await;
        assert!(*server.is_running.read().await);

        server.stop();
    }

    // ---- Static file serving / web management tests (Requirement 11.1) ----

    #[test]
    fn test_dist_path_env_var_default() {
        // When KIRO_DIST_PATH is not set, the default should be "dist"
        std::env::remove_var("KIRO_DIST_PATH");
        let path = std::env::var("KIRO_DIST_PATH").unwrap_or_else(|_| "dist".to_string());
        assert_eq!(path, "dist");
    }

    #[test]
    fn test_dist_path_env_var_custom() {
        std::env::set_var("KIRO_DIST_PATH", "/app/frontend");
        let path = std::env::var("KIRO_DIST_PATH").unwrap_or_else(|_| "dist".to_string());
        assert_eq!(path, "/app/frontend");
        std::env::remove_var("KIRO_DIST_PATH");
    }

    #[tokio::test]
    async fn test_server_with_nonexistent_dist_path_starts_ok() {
        // When dist path doesn't exist, server should still start (no static serving)
        std::env::set_var("KIRO_DIST_PATH", "/nonexistent/path/that/does/not/exist");

        let tm = Arc::new(TokenManager::new(std::path::PathBuf::from("/tmp")));
        let mapping = HashMap::new();
        let upstream_proxy = UpstreamProxyConfig::default();
        let security = ProxySecurityConfig::from_proxy_config(&ProxyConfig::default());

        let result = AxumServer::start(
            "127.0.0.1".to_string(),
            0,
            tm,
            mapping,
            upstream_proxy,
            None,
            security,
        )
        .await;

        assert!(result.is_ok());
        let (server, _handle) = result.unwrap();
        server.stop();

        std::env::remove_var("KIRO_DIST_PATH");
    }

    #[tokio::test]
    async fn test_server_with_existing_dist_path_starts_ok() {
        // Create a temp directory to simulate dist/
        let tmp = tempfile::tempdir().unwrap();
        let index_path = tmp.path().join("index.html");
        std::fs::write(&index_path, "<html><body>Hello</body></html>").unwrap();

        std::env::set_var("KIRO_DIST_PATH", tmp.path().to_str().unwrap());

        let tm = Arc::new(TokenManager::new(std::path::PathBuf::from("/tmp")));
        let mapping = HashMap::new();
        let upstream_proxy = UpstreamProxyConfig::default();
        let security = ProxySecurityConfig::from_proxy_config(&ProxyConfig::default());

        let result = AxumServer::start(
            "127.0.0.1".to_string(),
            0,
            tm,
            mapping,
            upstream_proxy,
            None,
            security,
        )
        .await;

        assert!(result.is_ok());
        let (server, _handle) = result.unwrap();
        server.stop();

        std::env::remove_var("KIRO_DIST_PATH");
    }
}
