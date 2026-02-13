// Proxy service control commands
//
// Tauri commands for managing the proxy service lifecycle,
// monitoring, logs, model mapping, scheduling, and rate limits.

use crate::models::config::{ProxyConfig, ProxyPoolConfig};
use crate::proxy::monitor::{ProxyRequestLog, ProxyStats};
use crate::proxy::token_manager::TokenManager;
use crate::proxy::server::AxumServer;
use crate::proxy::security::ProxySecurityConfig;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::State;
use tokio::sync::RwLock;

// ============================================================================
// State types
// ============================================================================

/// Proxy service status returned to the frontend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyStatus {
    pub running: bool,
    pub port: u16,
    pub base_url: String,
    pub active_accounts: usize,
}

/// Global proxy service state managed by Tauri
#[derive(Clone)]
pub struct ProxyServiceState {
    pub instance: Arc<RwLock<Option<ProxyServiceInstance>>>,
    pub monitor: Arc<RwLock<Option<Arc<crate::proxy::monitor::ProxyMonitor>>>>,
    pub admin_server: Arc<RwLock<Option<AdminServerInstance>>>,
    pub starting: Arc<AtomicBool>,
}

/// Admin server instance (always-on management API)
pub struct AdminServerInstance {
    pub axum_server: AxumServer,
    #[allow(dead_code)]
    pub server_handle: tokio::task::JoinHandle<()>,
}

/// Running proxy service instance
pub struct ProxyServiceInstance {
    pub config: ProxyConfig,
    pub token_manager: Arc<TokenManager>,
    pub axum_server: AxumServer,
    #[allow(dead_code)]
    pub server_handle: tokio::task::JoinHandle<()>,
}

impl ProxyServiceState {
    pub fn new() -> Self {
        Self {
            instance: Arc::new(RwLock::new(None)),
            monitor: Arc::new(RwLock::new(None)),
            admin_server: Arc::new(RwLock::new(None)),
            starting: Arc::new(AtomicBool::new(false)),
        }
    }
}

// ============================================================================
// Starting guard (RAII)
// ============================================================================

struct StartingGuard(Arc<AtomicBool>);
impl Drop for StartingGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

// ============================================================================
// Internal start logic (shared by Tauri command and headless mode)
// ============================================================================

/// Internal proxy service start logic, decoupled from Tauri State
pub async fn internal_start_proxy_service(
    config: ProxyConfig,
    state: &ProxyServiceState,
) -> Result<ProxyStatus, String> {
    // Check if already running
    {
        let instance_lock = state.instance.read().await;
        if instance_lock.is_some() {
            return Err("服务已在运行中".to_string());
        }
    }

    // Prevent concurrent starts
    if state
        .starting
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("服务正在启动中，请稍候...".to_string());
    }
    let _starting_guard = StartingGuard(state.starting.clone());

    // Ensure admin server is running
    ensure_admin_server(config.clone(), state).await?;

    // Reuse admin server's TokenManager (single instance)
    let token_manager = {
        let admin_lock = state.admin_server.read().await;
        admin_lock
            .as_ref()
            .unwrap()
            .axum_server
            .token_manager
            .clone()
    };

    // Sync config to TokenManager
    token_manager.start_auto_cleanup().await;

    // Load circuit breaker config
    let app_config = crate::modules::config::load_app_config()
        .unwrap_or_else(|_| crate::models::config::AppConfig::new());
    token_manager
        .update_circuit_breaker_config(app_config.circuit_breaker)
        .await;

    // Restore preferred account mode
    if let Some(ref account_id) = config.preferred_account_id {
        token_manager
            .set_preferred_account(Some(account_id.clone()))
            .await;
        tracing::info!("Fixed account mode restored: {}", account_id);
    }

    // Load accounts
    let active_accounts = token_manager.load_accounts().await.unwrap_or(0);

    if active_accounts == 0 {
        tracing::warn!("没有可用账号，反代逻辑将暂停，请通过管理界面添加。");
        return Ok(ProxyStatus {
            running: false,
            port: config.port,
            base_url: format!("http://127.0.0.1:{}", config.port),
            active_accounts: 0,
        });
    }

    let mut instance_lock = state.instance.write().await;
    let admin_lock = state.admin_server.read().await;
    let axum_server = admin_lock.as_ref().unwrap().axum_server.clone();

    let instance = ProxyServiceInstance {
        config: config.clone(),
        token_manager: token_manager.clone(),
        axum_server: axum_server.clone(),
        server_handle: tokio::spawn(async {}),
    };

    axum_server.set_running(true).await;
    *instance_lock = Some(instance);

    Ok(ProxyStatus {
        running: true,
        port: config.port,
        base_url: format!("http://127.0.0.1:{}", config.port),
        active_accounts,
    })
}

/// Ensure the admin server is running (starts it if not)
pub async fn ensure_admin_server(
    config: ProxyConfig,
    state: &ProxyServiceState,
) -> Result<(), String> {
    let mut admin_lock = state.admin_server.write().await;
    if admin_lock.is_some() {
        return Ok(());
    }

    // Ensure monitor exists
    {
        let mut monitor_lock = state.monitor.write().await;
        if monitor_lock.is_none() {
            *monitor_lock = Some(Arc::new(crate::proxy::monitor::ProxyMonitor::new(1000)));
        }
        if let Some(monitor) = monitor_lock.as_ref() {
            monitor.set_enabled(config.enable_logging);
        }
    }

    let app_data_dir = crate::modules::account::get_data_dir()?;
    let token_manager = Arc::new(TokenManager::new(app_data_dir));
    let _ = token_manager.load_accounts().await;

    let security_config = ProxySecurityConfig::from_proxy_config(&config);

    let (axum_server, server_handle) = AxumServer::start(
        config.get_bind_address().to_string(),
        config.port,
        token_manager,
        config.custom_mapping.clone(),
        config.upstream_proxy.clone(),
        config.user_agent_override.clone(),
        security_config,
    )
    .await
    .map_err(|e| format!("启动管理服务器失败: {}", e))?;

    *admin_lock = Some(AdminServerInstance {
        axum_server,
        server_handle,
    });

    // Initialize global configs
    crate::proxy::update_thinking_budget_config(config.thinking_budget.clone());
    crate::proxy::update_global_system_prompt_config(config.global_system_prompt.clone());
    crate::proxy::update_image_thinking_mode(config.image_thinking_mode.clone());

    Ok(())
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Start the proxy service
#[tauri::command]
pub async fn start_proxy_service(
    config: ProxyConfig,
    state: State<'_, ProxyServiceState>,
) -> Result<ProxyStatus, String> {
    internal_start_proxy_service(config, &state).await
}

/// Stop the proxy service
#[tauri::command]
pub async fn stop_proxy_service(state: State<'_, ProxyServiceState>) -> Result<(), String> {
    let mut instance_lock = state.instance.write().await;
    if instance_lock.is_none() {
        return Err("服务未运行".to_string());
    }

    if let Some(instance) = instance_lock.take() {
        instance
            .token_manager
            .graceful_shutdown(std::time::Duration::from_secs(2))
            .await;
        instance.axum_server.set_running(false).await;
    }

    Ok(())
}

/// Get proxy service status
#[tauri::command]
pub async fn get_proxy_status(state: State<'_, ProxyServiceState>) -> Result<ProxyStatus, String> {
    if state.starting.load(Ordering::SeqCst) {
        return Ok(ProxyStatus {
            running: false,
            port: 0,
            base_url: "starting".to_string(),
            active_accounts: 0,
        });
    }

    match state.instance.try_read() {
        Ok(instance_lock) => match instance_lock.as_ref() {
            Some(instance) => Ok(ProxyStatus {
                running: true,
                port: instance.config.port,
                base_url: format!("http://127.0.0.1:{}", instance.config.port),
                active_accounts: instance.token_manager.len(),
            }),
            None => Ok(ProxyStatus {
                running: false,
                port: 0,
                base_url: String::new(),
                active_accounts: 0,
            }),
        },
        Err(_) => Ok(ProxyStatus {
            running: false,
            port: 0,
            base_url: "busy".to_string(),
            active_accounts: 0,
        }),
    }
}

/// Get proxy statistics
#[tauri::command]
pub async fn get_proxy_stats(state: State<'_, ProxyServiceState>) -> Result<ProxyStats, String> {
    let monitor_lock = state.monitor.read().await;
    if let Some(monitor) = monitor_lock.as_ref() {
        Ok(monitor.get_stats().await)
    } else {
        Ok(ProxyStats::default())
    }
}

/// Get proxy request logs (from memory)
#[tauri::command]
pub async fn get_proxy_logs(
    state: State<'_, ProxyServiceState>,
    limit: Option<usize>,
) -> Result<Vec<ProxyRequestLog>, String> {
    let monitor_lock = state.monitor.read().await;
    if let Some(monitor) = monitor_lock.as_ref() {
        Ok(monitor.get_recent_logs(limit.unwrap_or(100)).await)
    } else {
        Ok(Vec::new())
    }
}

/// Set proxy monitor enabled state
#[tauri::command]
pub async fn set_proxy_monitor_enabled(
    state: State<'_, ProxyServiceState>,
    enabled: bool,
) -> Result<(), String> {
    let monitor_lock = state.monitor.read().await;
    if let Some(monitor) = monitor_lock.as_ref() {
        monitor.set_enabled(enabled);
    }
    Ok(())
}

/// Clear proxy request logs
#[tauri::command]
pub async fn clear_proxy_logs(state: State<'_, ProxyServiceState>) -> Result<(), String> {
    let monitor_lock = state.monitor.read().await;
    if let Some(monitor) = monitor_lock.as_ref() {
        monitor.clear().await;
    }
    Ok(())
}

/// Get paginated proxy logs from database
#[tauri::command]
pub async fn get_proxy_logs_paginated(
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<Vec<ProxyRequestLog>, String> {
    crate::modules::proxy_db::get_logs_summary(limit.unwrap_or(20), offset.unwrap_or(0))
}

/// Get single log detail
#[tauri::command]
pub async fn get_proxy_log_detail(log_id: String) -> Result<ProxyRequestLog, String> {
    crate::modules::proxy_db::get_log_detail(&log_id)
}

/// Get total log count
#[tauri::command]
pub async fn get_proxy_logs_count() -> Result<u64, String> {
    crate::proxy::monitor::get_logs_count()
}

/// Export all logs to file
#[tauri::command]
pub async fn export_proxy_logs(file_path: String) -> Result<usize, String> {
    let logs = crate::modules::proxy_db::get_all_logs_for_export()?;
    let count = logs.len();
    let json = serde_json::to_string_pretty(&logs)
        .map_err(|e| format!("Failed to serialize logs: {}", e))?;
    std::fs::write(&file_path, json).map_err(|e| format!("Failed to write file: {}", e))?;
    Ok(count)
}

/// Export logs JSON to file
#[tauri::command]
pub async fn export_proxy_logs_json(file_path: String, json_data: String) -> Result<usize, String> {
    let logs: Vec<serde_json::Value> =
        serde_json::from_str(&json_data).map_err(|e| format!("Failed to parse JSON: {}", e))?;
    let count = logs.len();
    let pretty_json =
        serde_json::to_string_pretty(&logs).map_err(|e| format!("Failed to serialize: {}", e))?;
    std::fs::write(&file_path, pretty_json).map_err(|e| format!("Failed to write file: {}", e))?;
    Ok(count)
}

/// Get filtered log count
#[tauri::command]
pub async fn get_proxy_logs_count_filtered(
    filter: String,
    errors_only: bool,
) -> Result<u64, String> {
    crate::modules::proxy_db::get_logs_count_filtered(&filter, errors_only)
}

/// Get filtered paginated logs
#[tauri::command]
pub async fn get_proxy_logs_filtered(
    filter: String,
    errors_only: bool,
    limit: usize,
    offset: usize,
) -> Result<Vec<ProxyRequestLog>, String> {
    crate::modules::proxy_db::get_logs_filtered(&filter, errors_only, limit, offset)
}

/// Generate a new API key
#[tauri::command]
pub fn generate_api_key() -> String {
    format!("sk-{}", uuid::Uuid::new_v4().simple())
}

/// Reload proxy accounts (when accounts are added/deleted)
#[tauri::command]
pub async fn reload_proxy_accounts(state: State<'_, ProxyServiceState>) -> Result<usize, String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        instance.token_manager.clear_all_sessions();
        instance
            .token_manager
            .load_accounts()
            .await
            .map_err(|e| format!("重新加载账号失败: {}", e))
    } else {
        Err("服务未运行".to_string())
    }
}

/// Hot-update model mapping
#[tauri::command]
pub async fn update_model_mapping(
    config: ProxyConfig,
    state: State<'_, ProxyServiceState>,
) -> Result<(), String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        instance.axum_server.update_mapping(&config).await;
    }

    let mut app_config = crate::modules::config::load_app_config()?;
    app_config.proxy.custom_mapping = config.custom_mapping;
    crate::modules::config::save_app_config(&app_config)?;

    Ok(())
}

/// Trigger proxy health check
#[tauri::command]
pub async fn check_proxy_health(
    state: State<'_, ProxyServiceState>,
) -> Result<ProxyPoolConfig, String> {
    let instance_lock = state.instance.read().await;
    if instance_lock.is_some() {
        let app_config = crate::modules::config::load_app_config()
            .map_err(|e| format!("Failed to load config: {}", e))?;
        let pool_config = app_config.proxy.proxy_pool.clone();
        let pool_config_arc = Arc::new(RwLock::new(pool_config));
        let manager = crate::proxy::proxy_pool::ProxyPoolManager::new(pool_config_arc.clone());
        manager.health_check().await?;
        let updated = pool_config_arc.read().await.clone();
        Ok(updated)
    } else {
        Err("服务未运行".to_string())
    }
}

/// Get current proxy pool config
#[tauri::command]
pub async fn get_proxy_pool_config(
    state: State<'_, ProxyServiceState>,
) -> Result<ProxyPoolConfig, String> {
    let instance_lock = state.instance.read().await;
    if instance_lock.is_some() {
        let app_config = crate::modules::config::load_app_config()
            .map_err(|e| format!("Failed to load config: {}", e))?;
        Ok(app_config.proxy.proxy_pool)
    } else {
        Err("服务未运行".to_string())
    }
}

/// Get scheduling config
#[tauri::command]
pub async fn get_proxy_scheduling_config(
    state: State<'_, ProxyServiceState>,
) -> Result<crate::models::config::StickySessionConfig, String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        Ok(instance.token_manager.get_sticky_config().await)
    } else {
        Ok(crate::models::config::StickySessionConfig::default())
    }
}

/// Update scheduling config
#[tauri::command]
pub async fn update_proxy_scheduling_config(
    state: State<'_, ProxyServiceState>,
    config: crate::models::config::StickySessionConfig,
) -> Result<(), String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        instance.token_manager.update_sticky_config(config).await;
        Ok(())
    } else {
        Err("服务未运行，无法更新实时配置".to_string())
    }
}

/// Clear all session bindings
#[tauri::command]
pub async fn clear_proxy_session_bindings(
    state: State<'_, ProxyServiceState>,
) -> Result<(), String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        instance.token_manager.clear_all_sessions();
        Ok(())
    } else {
        Err("服务未运行".to_string())
    }
}

/// Set preferred account (fixed account mode)
#[tauri::command]
pub async fn set_preferred_account(
    state: State<'_, ProxyServiceState>,
    account_id: Option<String>,
) -> Result<(), String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        let cleaned_id = account_id.filter(|s| !s.trim().is_empty());
        instance
            .token_manager
            .set_preferred_account(cleaned_id.clone())
            .await;

        // Persist to config
        let mut app_config = crate::modules::config::load_app_config()
            .map_err(|e| format!("加载配置失败: {}", e))?;
        app_config.proxy.preferred_account_id = cleaned_id;
        crate::modules::config::save_app_config(&app_config)
            .map_err(|e| format!("保存配置失败: {}", e))?;

        Ok(())
    } else {
        Err("服务未运行".to_string())
    }
}

/// Get preferred account ID
#[tauri::command]
pub async fn get_preferred_account(
    state: State<'_, ProxyServiceState>,
) -> Result<Option<String>, String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        Ok(instance.token_manager.get_preferred_account().await)
    } else {
        Ok(None)
    }
}

/// Clear rate limit for a specific account
#[tauri::command]
pub async fn clear_proxy_rate_limit(
    state: State<'_, ProxyServiceState>,
    account_id: String,
) -> Result<bool, String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        Ok(instance.token_manager.clear_rate_limit(&account_id))
    } else {
        Err("服务未运行".to_string())
    }
}

/// Clear all rate limits
#[tauri::command]
pub async fn clear_all_proxy_rate_limits(
    state: State<'_, ProxyServiceState>,
) -> Result<(), String> {
    let instance_lock = state.instance.read().await;
    if let Some(instance) = instance_lock.as_ref() {
        instance.token_manager.clear_all_rate_limits();
        Ok(())
    } else {
        Err("服务未运行".to_string())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proxy_status_serialization() {
        let status = ProxyStatus {
            running: true,
            port: 8045,
            base_url: "http://127.0.0.1:8045".to_string(),
            active_accounts: 5,
        };
        let json = serde_json::to_string(&status).unwrap();
        let deserialized: ProxyStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.running, true);
        assert_eq!(deserialized.port, 8045);
        assert_eq!(deserialized.active_accounts, 5);
    }

    #[test]
    fn test_proxy_service_state_new() {
        let state = ProxyServiceState::new();
        assert!(!state.starting.load(Ordering::SeqCst));
    }

    #[test]
    fn test_generate_api_key_format() {
        let key = generate_api_key();
        assert!(key.starts_with("sk-"));
        assert!(key.len() > 3);
    }

    #[test]
    fn test_generate_api_key_unique() {
        let key1 = generate_api_key();
        let key2 = generate_api_key();
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_starting_guard_resets_flag() {
        let flag = Arc::new(AtomicBool::new(true));
        {
            let _guard = StartingGuard(flag.clone());
            assert!(flag.load(Ordering::SeqCst));
        }
        // After guard is dropped, flag should be false
        assert!(!flag.load(Ordering::SeqCst));
    }
}
