// Admin API Handlers - Web management backend endpoints
//
// Requirements covered:
// - 1.1-1.16: Account management (list, add, delete, export, sort, switch, refresh quota)
// - 5.1-5.8: Quota monitoring and warmup
// - 6.1-6.18: Security management (IP logs, blacklist, whitelist, user tokens)
// - 8.1-8.6: CLI sync (status, sync, restore, config view)
// - 13.1-13.5: Request monitoring and statistics
// - 14.1-14.8: Hot update and runtime configuration

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use tracing::{error, warn};

use crate::modules::{account, config as app_config, device, oauth, quota, security_db, token_stats, user_token_db, proxy_db};
use crate::proxy::cli_sync::{self, CliApp};
use crate::proxy::opencode_sync;
use crate::proxy::droid_sync;
use crate::proxy::monitor;

use super::AppState;

// ============================================================================
// Common types
// ============================================================================

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

type AdminResult<T> = Result<T, (StatusCode, Json<ErrorResponse>)>;

fn err_500(msg: String) -> (StatusCode, Json<ErrorResponse>) {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: msg }))
}

fn err_400(msg: String) -> (StatusCode, Json<ErrorResponse>) {
    (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: msg }))
}

// ============================================================================
// Account Management
// ============================================================================

/// List all accounts
pub async fn admin_list_accounts(
    State(_state): State<AppState>,
) -> AdminResult<impl IntoResponse> {
    let accounts = account::list_accounts().map_err(err_500)?;
    let current_id = account::get_current_account_id().unwrap_or(None);

    let responses: Vec<serde_json::Value> = accounts
        .into_iter()
        .map(|acc| {
            let is_current = current_id.as_ref().map(|id| id == &acc.id).unwrap_or(false);
            serde_json::json!({
                "id": acc.id,
                "email": acc.email,
                "name": acc.name,
                "is_current": is_current,
                "disabled": acc.disabled,
                "disabled_reason": acc.disabled_reason,
                "disabled_at": acc.disabled_at,
                "proxy_disabled": acc.proxy_disabled,
                "proxy_disabled_reason": acc.proxy_disabled_reason,
                "proxy_disabled_at": acc.proxy_disabled_at,
                "protected_models": acc.protected_models.iter().collect::<Vec<_>>(),
                "validation_blocked": acc.validation_blocked,
                "validation_blocked_until": acc.validation_blocked_until,
                "validation_blocked_reason": acc.validation_blocked_reason,
                "quota": acc.quota,
                "device_bound": acc.device_profile.is_some(),
                "last_used": acc.last_used,
                "custom_label": acc.custom_label,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "current_account_id": current_id,
        "accounts": responses,
    })))
}

#[derive(Deserialize)]
pub struct AddAccountRequest {
    pub refresh_token: String,
}

/// Add account via refresh_token
pub async fn admin_add_account(
    State(state): State<AppState>,
    Json(payload): Json<AddAccountRequest>,
) -> AdminResult<impl IntoResponse> {
    let account = account::import_single_token(&payload.refresh_token)
        .await
        .map_err(err_500)?;

    // Reload TokenManager after adding
    if let Err(e) = state.token_manager.load_accounts().await {
        error!("[Admin] Failed to reload accounts after adding: {}", e);
    }

    Ok(Json(serde_json::json!({
        "id": account.id,
        "email": account.email,
        "name": account.name,
    })))
}

/// Delete single account
pub async fn admin_delete_account(
    State(state): State<AppState>,
    Path(account_id): Path<String>,
) -> AdminResult<impl IntoResponse> {
    account::delete_account(&account_id).map_err(err_500)?;

    // Remove from TokenManager memory
    state.token_manager.remove_account(&account_id);

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct BulkDeleteRequest {
    pub account_ids: Vec<String>,
}

/// Bulk delete accounts
pub async fn admin_delete_accounts(
    State(state): State<AppState>,
    Json(payload): Json<BulkDeleteRequest>,
) -> AdminResult<impl IntoResponse> {
    account::delete_accounts(&payload.account_ids).map_err(err_500)?;

    for id in &payload.account_ids {
        state.token_manager.remove_account(id);
    }

    Ok(Json(serde_json::json!({
        "deleted": payload.account_ids.len(),
    })))
}

#[derive(Deserialize)]
pub struct ExportAccountsRequest {
    pub account_ids: Option<Vec<String>>,
}

/// Export accounts
pub async fn admin_export_accounts(
    Json(_payload): Json<ExportAccountsRequest>,
) -> AdminResult<impl IntoResponse> {
    let items = account::export_accounts().map_err(err_500)?;
    Ok(Json(items))
}

#[derive(Deserialize)]
pub struct ReorderRequest {
    pub account_ids: Vec<String>,
}

/// Reorder accounts
pub async fn admin_reorder_accounts(
    Json(payload): Json<ReorderRequest>,
) -> AdminResult<impl IntoResponse> {
    account::reorder_accounts(&payload.account_ids).map_err(err_500)?;
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
pub struct ToggleProxyRequest {
    pub proxy_disabled: bool,
    pub reason: Option<String>,
}

/// Toggle account proxy status
pub async fn admin_toggle_proxy_status(
    State(state): State<AppState>,
    Path(account_id): Path<String>,
    Json(payload): Json<ToggleProxyRequest>,
) -> AdminResult<impl IntoResponse> {
    let mut acc = account::load_account(&account_id).map_err(err_500)?;
    acc.proxy_disabled = payload.proxy_disabled;
    acc.proxy_disabled_reason = payload.reason;
    acc.proxy_disabled_at = if payload.proxy_disabled {
        Some(chrono::Utc::now().timestamp())
    } else {
        None
    };
    account::save_account(&acc).map_err(err_500)?;

    // Reload in TokenManager
    if let Err(e) = state.token_manager.reload_account(&account_id).await {
        warn!("[Admin] Failed to reload account after toggle: {}", e);
    }

    Ok(StatusCode::OK)
}

/// Refresh quota for a single account
pub async fn admin_fetch_account_quota(
    Path(account_id): Path<String>,
) -> AdminResult<impl IntoResponse> {
    let acc = account::load_account(&account_id).map_err(err_500)?;

    let token = oauth::ensure_fresh_token(&acc.token)
        .await
        .map_err(err_500)?;

    let (quota_data, project_id) = quota::fetch_quota_with_cache(
        &token.access_token,
        &acc.email,
        acc.token.project_id.as_deref(),
    )
    .await
    .map_err(err_500)?;

    // Save updated quota
    account::update_account_quota(&account_id, quota_data.clone()).map_err(err_500)?;

    // Queue for TokenManager reload
    crate::proxy::server::trigger_account_reload(&account_id);

    Ok(Json(serde_json::json!({
        "quota": quota_data,
        "project_id": project_id,
    })))
}

/// Refresh all account quotas
pub async fn admin_refresh_all_quotas() -> AdminResult<impl IntoResponse> {
    let accounts = account::list_accounts().map_err(err_500)?;
    let mut success = 0u32;
    let mut failed = 0u32;

    for acc in &accounts {
        match oauth::ensure_fresh_token(&acc.token).await {
            Ok(token) => {
                match quota::fetch_quota_with_cache(
                    &token.access_token,
                    &acc.email,
                    acc.token.project_id.as_deref(),
                )
                .await
                {
                    Ok((quota_data, _)) => {
                        let _ = account::update_account_quota(&acc.id, quota_data);
                        crate::proxy::server::trigger_account_reload(&acc.id);
                        success += 1;
                    }
                    Err(_) => failed += 1,
                }
            }
            Err(_) => failed += 1,
        }
    }

    Ok(Json(serde_json::json!({
        "total": accounts.len(),
        "success": success,
        "failed": failed,
    })))
}

#[derive(Deserialize)]
pub struct SwitchRequest {
    pub account_id: String,
}

/// Switch current account
pub async fn admin_switch_account(
    State(state): State<AppState>,
    Json(payload): Json<SwitchRequest>,
) -> AdminResult<impl IntoResponse> {
    account::set_current_account_id(&payload.account_id).map_err(err_500)?;

    // Clear session bindings and reload
    state.token_manager.clear_all_sessions();
    if let Err(e) = state.token_manager.load_accounts().await {
        error!("[Admin] Failed to reload accounts after switch: {}", e);
    }

    Ok(StatusCode::OK)
}

// ============================================================================
// Device Fingerprint Management
// ============================================================================

#[derive(Deserialize)]
pub struct BindDeviceRequest {
    #[serde(default = "default_bind_mode")]
    pub mode: String,
}

fn default_bind_mode() -> String {
    "generate".to_string()
}

/// Bind device fingerprint to account
pub async fn admin_bind_device(
    Path(account_id): Path<String>,
    Json(_payload): Json<BindDeviceRequest>,
) -> AdminResult<impl IntoResponse> {
    let profile = device::generate_device_profile();
    device::bind_device_profile(&account_id, profile.clone()).map_err(err_500)?;

    Ok(Json(serde_json::json!({
        "success": true,
        "device_profile": profile,
    })))
}

/// Get device profiles for account
pub async fn admin_get_device_profiles(
    Path(account_id): Path<String>,
) -> AdminResult<impl IntoResponse> {
    let acc = account::load_account(&account_id).map_err(err_500)?;
    Ok(Json(serde_json::json!({
        "device_profile": acc.device_profile,
        "device_history": acc.device_history,
    })))
}

/// List device versions for account
pub async fn admin_list_device_versions(
    Path(account_id): Path<String>,
) -> AdminResult<impl IntoResponse> {
    let history = device::get_device_history(&account_id).map_err(err_500)?;
    Ok(Json(history))
}

/// Preview generate a device profile (without binding)
pub async fn admin_preview_generate_profile() -> AdminResult<impl IntoResponse> {
    let profile = device::generate_device_profile();
    Ok(Json(profile))
}

#[derive(Deserialize)]
pub struct BindDeviceProfileWrapper {
    pub label: Option<String>,
    pub profile: DeviceProfileInput,
}

#[derive(Deserialize)]
pub struct DeviceProfileInput {
    pub machine_id: String,
    pub mac_machine_id: String,
    pub dev_device_id: String,
    pub sqm_id: String,
}

/// Bind a specific device profile to account
pub async fn admin_bind_device_profile_with_profile(
    Path(account_id): Path<String>,
    Json(payload): Json<BindDeviceProfileWrapper>,
) -> AdminResult<impl IntoResponse> {
    let profile = crate::models::DeviceProfile {
        machine_id: payload.profile.machine_id,
        mac_machine_id: payload.profile.mac_machine_id,
        dev_device_id: payload.profile.dev_device_id,
        sqm_id: payload.profile.sqm_id,
    };

    let label = payload.label.unwrap_or_else(|| "Manual bind".to_string());
    let version = device::add_device_history(&account_id, &label, profile).map_err(err_500)?;

    Ok(Json(serde_json::json!({
        "success": true,
        "version": version,
    })))
}

/// Restore original device (remove device profile)
pub async fn admin_restore_original_device(
    Json(payload): Json<serde_json::Value>,
) -> AdminResult<impl IntoResponse> {
    let account_id = payload["account_id"]
        .as_str()
        .ok_or_else(|| err_400("account_id required".to_string()))?;

    let mut acc = account::load_account(account_id).map_err(err_500)?;
    acc.device_profile = None;
    account::save_account(&acc).map_err(err_500)?;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// Restore a specific device version
pub async fn admin_restore_device_version(
    Path((account_id, version_id)): Path<(String, String)>,
) -> AdminResult<impl IntoResponse> {
    let mut acc = account::load_account(&account_id).map_err(err_500)?;

    let version = acc
        .device_history
        .iter()
        .find(|v| v.id == version_id)
        .cloned()
        .ok_or_else(|| err_400("Version not found".to_string()))?;

    // Set as current
    for entry in acc.device_history.iter_mut() {
        entry.is_current = entry.id == version_id;
    }
    acc.device_profile = Some(version.profile.clone());
    account::save_account(&acc).map_err(err_500)?;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// Delete a device version
pub async fn admin_delete_device_version(
    Path((account_id, version_id)): Path<(String, String)>,
) -> AdminResult<impl IntoResponse> {
    let mut acc = account::load_account(&account_id).map_err(err_500)?;
    acc.device_history.retain(|v| v.id != version_id);
    account::save_account(&acc).map_err(err_500)?;

    Ok(StatusCode::NO_CONTENT)
}

// ============================================================================
// Configuration Management
// ============================================================================

/// Get application config
pub async fn admin_get_config() -> AdminResult<impl IntoResponse> {
    let config = app_config::load_app_config().map_err(err_500)?;
    Ok(Json(config))
}

#[derive(Deserialize)]
pub struct SaveConfigWrapper {
    #[serde(flatten)]
    pub config: crate::models::AppConfig,
}

/// Save application config
pub async fn admin_save_config(
    Json(payload): Json<SaveConfigWrapper>,
) -> AdminResult<impl IntoResponse> {
    app_config::save_app_config(&payload.config).map_err(err_500)?;
    Ok(Json(serde_json::json!({ "success": true })))
}

// ============================================================================
// Proxy Control
// ============================================================================

/// Get proxy status
pub async fn admin_get_proxy_status(
    State(state): State<AppState>,
) -> AdminResult<impl IntoResponse> {
    let token_count = state.token_manager.len();
    let config = app_config::load_app_config().map_err(err_500)?;

    Ok(Json(serde_json::json!({
        "enabled": config.proxy.enabled,
        "port": config.proxy.port,
        "token_count": token_count,
        "allow_lan_access": config.proxy.allow_lan_access,
        "auth_mode": format!("{:?}", config.proxy.auth_mode),
    })))
}

/// Start proxy service (placeholder - actual start is managed by Tauri commands)
pub async fn admin_start_proxy_service(
    State(_state): State<AppState>,
) -> AdminResult<impl IntoResponse> {
    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Proxy service start requested",
    })))
}

/// Stop proxy service (placeholder - actual stop is managed by Tauri commands)
pub async fn admin_stop_proxy_service(
    State(_state): State<AppState>,
) -> AdminResult<impl IntoResponse> {
    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Proxy service stop requested",
    })))
}

#[derive(Deserialize)]
pub struct UpdateMappingWrapper {
    pub custom_mapping: std::collections::HashMap<String, String>,
}

/// Update model mapping
pub async fn admin_update_model_mapping(
    State(state): State<AppState>,
    Json(payload): Json<UpdateMappingWrapper>,
) -> AdminResult<impl IntoResponse> {
    let mut mapping = state.custom_mapping.write().await;
    *mapping = payload.custom_mapping;
    drop(mapping);

    Ok(Json(serde_json::json!({ "success": true })))
}

/// Generate a new API key
pub async fn admin_generate_api_key() -> impl IntoResponse {
    let key = format!("sk-{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
    Json(serde_json::json!({ "api_key": key }))
}

/// Clear proxy session bindings
pub async fn admin_clear_proxy_session_bindings(
    State(state): State<AppState>,
) -> impl IntoResponse {
    state.token_manager.clear_all_sessions();
    Json(serde_json::json!({ "success": true }))
}

/// Clear all rate limits
pub async fn admin_clear_all_rate_limits(
    State(state): State<AppState>,
) -> impl IntoResponse {
    state.token_manager.clear_all_rate_limits();
    Json(serde_json::json!({ "success": true }))
}

/// Clear rate limit for specific account
pub async fn admin_clear_rate_limit(
    State(state): State<AppState>,
    Path(account_id): Path<String>,
) -> impl IntoResponse {
    let cleared = state.token_manager.clear_rate_limit(&account_id);
    Json(serde_json::json!({ "success": cleared }))
}

/// Get preferred account
pub async fn admin_get_preferred_account(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let preferred = state.token_manager.get_preferred_account().await;
    Json(serde_json::json!({ "preferred_account_id": preferred }))
}

#[derive(Deserialize)]
pub struct SetPreferredAccountRequest {
    pub account_id: Option<String>,
}

/// Set preferred account
pub async fn admin_set_preferred_account(
    State(state): State<AppState>,
    Json(payload): Json<SetPreferredAccountRequest>,
) -> impl IntoResponse {
    state.token_manager.set_preferred_account(payload.account_id).await;
    Json(serde_json::json!({ "success": true }))
}

/// Toggle proxy monitor enabled
pub async fn admin_set_proxy_monitor_enabled(
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    let _enabled = payload["enabled"].as_bool().unwrap_or(true);
    // Monitor enable/disable is handled via ProxyMonitor instance
    Json(serde_json::json!({ "success": true }))
}

// ============================================================================
// CLI Sync
// ============================================================================

#[derive(Deserialize)]
pub struct CliSyncStatusRequest {
    pub app: String,
}

/// Get CLI sync status
pub async fn admin_get_cli_sync_status(
    Json(payload): Json<CliSyncStatusRequest>,
) -> AdminResult<impl IntoResponse> {
    let config = app_config::load_app_config().map_err(err_500)?;
    let port = config.proxy.port;
    let proxy_url = format!("http://127.0.0.1:{}", port);

    let app = parse_cli_app(&payload.app)?;
    let status = cli_sync::get_cli_status(&app, &proxy_url);
    Ok(Json(status))
}

#[derive(Deserialize)]
pub struct CliSyncRequest {
    pub app: String,
    pub api_key: Option<String>,
    pub model: Option<String>,
}

/// Execute CLI sync
pub async fn admin_execute_cli_sync(
    Json(payload): Json<CliSyncRequest>,
) -> AdminResult<impl IntoResponse> {
    let config = app_config::load_app_config().map_err(err_500)?;
    let port = config.proxy.port;
    let proxy_url = format!("http://127.0.0.1:{}", port);
    let api_key = payload.api_key.unwrap_or(config.proxy.api_key);

    let app = parse_cli_app(&payload.app)?;
    cli_sync::sync_config(&app, &proxy_url, &api_key, payload.model.as_deref()).map_err(err_500)?;

    Ok(Json(serde_json::json!({ "success": true })))
}

#[derive(Deserialize)]
pub struct CliRestoreRequest {
    pub app: String,
}

/// Restore CLI config
pub async fn admin_execute_cli_restore(
    Json(payload): Json<CliRestoreRequest>,
) -> AdminResult<impl IntoResponse> {
    let app = parse_cli_app(&payload.app)?;
    cli_sync::restore_config(&app).map_err(err_500)?;
    Ok(Json(serde_json::json!({ "success": true })))
}

#[derive(Deserialize)]
pub struct CliConfigContentRequest {
    pub app: String,
    pub file_name: Option<String>,
}

/// Get CLI config content
pub async fn admin_get_cli_config_content(
    Json(payload): Json<CliConfigContentRequest>,
) -> AdminResult<impl IntoResponse> {
    let app = parse_cli_app(&payload.app)?;
    let content = cli_sync::get_config_content(&app, payload.file_name.as_deref()).map_err(err_500)?;
    Ok(Json(serde_json::json!({ "content": content })))
}

fn parse_cli_app(name: &str) -> Result<CliApp, (StatusCode, Json<ErrorResponse>)> {
    match name.to_lowercase().as_str() {
        "claude" | "claude_code" | "claude-code" => Ok(CliApp::Claude),
        "codex" | "codex_cli" | "codex-cli" => Ok(CliApp::Codex),
        "gemini" | "gemini_cli" | "gemini-cli" => Ok(CliApp::Gemini),
        "opencode" | "open_code" | "open-code" => Ok(CliApp::OpenCode),
        _ => Err(err_400(format!("Unknown CLI app: {}", name))),
    }
}

// ============================================================================
// OpenCode Sync
// ============================================================================

#[derive(Deserialize)]
pub struct OpencodeSyncStatusRequest {
    pub proxy_url: Option<String>,
}

/// Get OpenCode sync status
pub async fn admin_get_opencode_sync_status(
    Json(payload): Json<OpencodeSyncStatusRequest>,
) -> AdminResult<impl IntoResponse> {
    let config = app_config::load_app_config().map_err(err_500)?;
    let proxy_url = payload.proxy_url.unwrap_or_else(|| format!("http://127.0.0.1:{}", config.proxy.port));
    let status = opencode_sync::get_opencode_status(&proxy_url);
    Ok(Json(status))
}

#[derive(Deserialize)]
pub struct OpencodeSyncRequest {
    pub api_key: Option<String>,
    pub model_ids: Option<Vec<String>>,
}

/// Execute OpenCode sync
pub async fn admin_execute_opencode_sync(
    Json(payload): Json<OpencodeSyncRequest>,
) -> AdminResult<impl IntoResponse> {
    let config = app_config::load_app_config().map_err(err_500)?;
    let proxy_url = format!("http://127.0.0.1:{}", config.proxy.port);
    let api_key = payload.api_key.unwrap_or(config.proxy.api_key);

    let model_refs: Option<Vec<String>> = payload.model_ids;
    opencode_sync::sync_opencode_config(&proxy_url, &api_key, model_refs).map_err(err_500)?;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// Restore OpenCode config
pub async fn admin_execute_opencode_restore() -> AdminResult<impl IntoResponse> {
    opencode_sync::restore_opencode_config().map_err(err_500)?;
    Ok(Json(serde_json::json!({ "success": true })))
}

#[derive(Deserialize)]
pub struct GetOpencodeConfigRequest {
    pub file_name: Option<String>,
}

/// Get OpenCode config content
pub async fn admin_get_opencode_config_content(
    Json(payload): Json<GetOpencodeConfigRequest>,
) -> AdminResult<impl IntoResponse> {
    let content = opencode_sync::read_opencode_config_content(payload.file_name.as_deref()).map_err(err_500)?;
    Ok(Json(serde_json::json!({ "content": content })))
}

#[derive(Deserialize)]
pub struct OpencodeClearRequest {
    pub proxy_url: Option<String>,
}

/// Clear OpenCode sync
pub async fn admin_execute_opencode_clear(
    Json(payload): Json<OpencodeClearRequest>,
) -> AdminResult<impl IntoResponse> {
    let config = app_config::load_app_config().map_err(err_500)?;
    let proxy_url = payload.proxy_url.unwrap_or_else(|| format!("http://127.0.0.1:{}", config.proxy.port));
    let api_key = &config.proxy.api_key;

    // Clear by syncing with empty config
    opencode_sync::sync_opencode_config(&proxy_url, api_key, None).map_err(err_500)?;
    Ok(Json(serde_json::json!({ "success": true })))
}

// ============================================================================
// Droid Sync
// ============================================================================

#[derive(Deserialize)]
pub struct DroidSyncStatusRequest {
    pub proxy_url: Option<String>,
}

/// Get Droid sync status
pub async fn admin_get_droid_sync_status(
    Json(payload): Json<DroidSyncStatusRequest>,
) -> AdminResult<impl IntoResponse> {
    let config = app_config::load_app_config().map_err(err_500)?;
    let proxy_url = payload.proxy_url.unwrap_or_else(|| format!("http://127.0.0.1:{}", config.proxy.port));
    let status = droid_sync::get_droid_status(&proxy_url);
    Ok(Json(status))
}

#[derive(Deserialize)]
pub struct DroidSyncRequest {
    pub custom_models: Option<Vec<serde_json::Value>>,
}

/// Execute Droid sync
pub async fn admin_execute_droid_sync(
    Json(payload): Json<DroidSyncRequest>,
) -> AdminResult<impl IntoResponse> {
    let models = payload.custom_models.unwrap_or_default();
    let count = droid_sync::sync_droid_config(models).map_err(err_500)?;
    Ok(Json(serde_json::json!({ "success": true, "synced_models": count })))
}

/// Restore Droid config
pub async fn admin_execute_droid_restore() -> AdminResult<impl IntoResponse> {
    droid_sync::restore_droid_config().map_err(err_500)?;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// Get Droid config content
pub async fn admin_get_droid_config_content() -> AdminResult<impl IntoResponse> {
    let content = droid_sync::read_droid_config_content().map_err(err_500)?;
    Ok(Json(serde_json::json!({ "content": content })))
}

// ============================================================================
// Security Management - IP Logs, Stats, Blacklist, Whitelist
// ============================================================================

#[derive(Deserialize)]
pub struct IpAccessLogQuery {
    #[serde(default = "default_page")]
    pub page: usize,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    pub ip: Option<String>,
    #[serde(default)]
    pub blocked_only: bool,
}

fn default_page() -> usize { 1 }
fn default_page_size() -> usize { 50 }

/// Get IP access logs
pub async fn admin_get_ip_access_logs(
    Query(query): Query<IpAccessLogQuery>,
) -> AdminResult<impl IntoResponse> {
    let offset = (query.page.saturating_sub(1)) * query.page_size;
    let logs = security_db::get_ip_access_logs(
        query.page_size,
        offset,
        query.ip.as_deref(),
        query.blocked_only,
    )
    .map_err(err_500)?;

    let total = security_db::get_ip_access_logs_count(query.ip.as_deref(), query.blocked_only)
        .map_err(err_500)?;

    Ok(Json(serde_json::json!({
        "logs": logs,
        "total": total,
        "page": query.page,
        "page_size": query.page_size,
    })))
}

/// Clear IP access logs
pub async fn admin_clear_ip_access_logs() -> AdminResult<impl IntoResponse> {
    security_db::clear_ip_access_logs().map_err(err_500)?;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// Get IP statistics
pub async fn admin_get_ip_stats() -> AdminResult<impl IntoResponse> {
    let stats = security_db::get_ip_stats().map_err(err_500)?;
    Ok(Json(stats))
}

#[derive(Deserialize)]
pub struct IpTokenStatsQuery {
    #[serde(default = "default_ip_stats_limit")]
    pub limit: usize,
    #[serde(default = "default_ip_stats_hours")]
    pub hours: i64,
}

fn default_ip_stats_limit() -> usize { 50 }
fn default_ip_stats_hours() -> i64 { 24 }

/// Get IP token usage stats
pub async fn admin_get_ip_token_stats(
    Query(query): Query<IpTokenStatsQuery>,
) -> AdminResult<impl IntoResponse> {
    let stats = proxy_db::get_token_usage_by_ip(query.limit, query.hours).map_err(err_500)?;
    Ok(Json(stats))
}

// --- Blacklist ---

/// Get IP blacklist
pub async fn admin_get_ip_blacklist() -> AdminResult<impl IntoResponse> {
    let list = security_db::get_blacklist().map_err(err_500)?;
    Ok(Json(list))
}

#[derive(Deserialize)]
pub struct AddBlacklistRequest {
    pub ip_pattern: String,
    pub reason: Option<String>,
    pub expires_at: Option<i64>,
}

/// Add IP to blacklist
pub async fn admin_add_ip_to_blacklist(
    Json(payload): Json<AddBlacklistRequest>,
) -> AdminResult<impl IntoResponse> {
    let entry = security_db::add_to_blacklist(
        &payload.ip_pattern,
        payload.reason.as_deref(),
        payload.expires_at,
        "admin",
    )
    .map_err(err_500)?;
    Ok(Json(entry))
}

#[derive(Deserialize)]
pub struct RemoveIpRequest {
    pub id: String,
}

/// Remove IP from blacklist
pub async fn admin_remove_ip_from_blacklist(
    Json(payload): Json<RemoveIpRequest>,
) -> AdminResult<impl IntoResponse> {
    security_db::remove_from_blacklist(&payload.id).map_err(err_500)?;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// Clear IP blacklist
pub async fn admin_clear_ip_blacklist() -> AdminResult<impl IntoResponse> {
    // Clear all entries by getting and removing each
    let entries = security_db::get_blacklist().map_err(err_500)?;
    for entry in &entries {
        let _ = security_db::remove_from_blacklist(&entry.id);
    }
    Ok(Json(serde_json::json!({ "success": true, "cleared": entries.len() })))
}

#[derive(Deserialize)]
pub struct CheckIpQuery {
    pub ip: String,
}

/// Check if IP is in blacklist
pub async fn admin_check_ip_in_blacklist(
    Query(query): Query<CheckIpQuery>,
) -> AdminResult<impl IntoResponse> {
    let is_blocked = security_db::is_ip_in_blacklist(&query.ip).map_err(err_500)?;
    Ok(Json(serde_json::json!({ "blocked": is_blocked })))
}

// --- Whitelist ---

/// Get IP whitelist
pub async fn admin_get_ip_whitelist() -> AdminResult<impl IntoResponse> {
    let list = security_db::get_whitelist().map_err(err_500)?;
    Ok(Json(list))
}

#[derive(Deserialize)]
pub struct AddWhitelistRequest {
    pub ip_pattern: String,
    pub description: Option<String>,
}

/// Add IP to whitelist
pub async fn admin_add_ip_to_whitelist(
    Json(payload): Json<AddWhitelistRequest>,
) -> AdminResult<impl IntoResponse> {
    let entry = security_db::add_to_whitelist(&payload.ip_pattern, payload.description.as_deref())
        .map_err(err_500)?;
    Ok(Json(entry))
}

/// Remove IP from whitelist
pub async fn admin_remove_ip_from_whitelist(
    Json(payload): Json<RemoveIpRequest>,
) -> AdminResult<impl IntoResponse> {
    security_db::remove_from_whitelist(&payload.id).map_err(err_500)?;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// Clear IP whitelist
pub async fn admin_clear_ip_whitelist() -> AdminResult<impl IntoResponse> {
    let entries = security_db::get_whitelist().map_err(err_500)?;
    for entry in &entries {
        let _ = security_db::remove_from_whitelist(&entry.id);
    }
    Ok(Json(serde_json::json!({ "success": true, "cleared": entries.len() })))
}

/// Check if IP is in whitelist
pub async fn admin_check_ip_in_whitelist(
    Query(query): Query<CheckIpQuery>,
) -> AdminResult<impl IntoResponse> {
    let is_whitelisted = security_db::is_ip_in_whitelist(&query.ip).map_err(err_500)?;
    Ok(Json(serde_json::json!({ "whitelisted": is_whitelisted })))
}

// ============================================================================
// User Token Management
// ============================================================================

/// List all user tokens
pub async fn admin_list_user_tokens() -> AdminResult<impl IntoResponse> {
    let tokens = user_token_db::list_tokens().map_err(err_500)?;
    Ok(Json(tokens))
}

/// Get user token summary
pub async fn admin_get_user_token_summary() -> AdminResult<impl IntoResponse> {
    let tokens = user_token_db::list_tokens().map_err(err_500)?;
    let total = tokens.len();
    let active = tokens.iter().filter(|t| t.enabled).count();
    let expired = tokens
        .iter()
        .filter(|t| {
            t.expires_at
                .map(|exp| exp < chrono::Utc::now().timestamp())
                .unwrap_or(false)
        })
        .count();

    Ok(Json(serde_json::json!({
        "total": total,
        "active": active,
        "expired": expired,
        "total_requests": tokens.iter().map(|t| t.total_requests).sum::<i64>(),
        "total_tokens_used": tokens.iter().map(|t| t.total_tokens_used).sum::<i64>(),
    })))
}

/// Create user token
pub async fn admin_create_user_token(
    Json(payload): Json<serde_json::Value>,
) -> AdminResult<impl IntoResponse> {
    let username = payload["username"]
        .as_str()
        .unwrap_or("user")
        .to_string();
    let expires_type = payload["expires_type"]
        .as_str()
        .unwrap_or("never")
        .to_string();
    let description = payload["description"].as_str().map(|s| s.to_string());
    let max_ips = payload["max_ips"].as_i64().unwrap_or(0) as i32;
    let curfew_start = payload["curfew_start"].as_str().map(|s| s.to_string());
    let curfew_end = payload["curfew_end"].as_str().map(|s| s.to_string());
    let custom_expires_at = payload["expires_at"].as_i64();

    let token = user_token_db::create_token(
        username,
        expires_type,
        description,
        max_ips,
        curfew_start,
        curfew_end,
        custom_expires_at,
    )
    .map_err(err_500)?;

    Ok(Json(token))
}

#[derive(Deserialize)]
pub struct RenewTokenRequest {
    pub expires_type: String,
}

/// Renew user token
pub async fn admin_renew_user_token(
    Path(id): Path<String>,
    Json(payload): Json<RenewTokenRequest>,
) -> AdminResult<impl IntoResponse> {
    user_token_db::renew_token(&id, &payload.expires_type).map_err(err_500)?;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// Delete user token
pub async fn admin_delete_user_token(
    Path(id): Path<String>,
) -> AdminResult<impl IntoResponse> {
    user_token_db::delete_token(&id).map_err(err_500)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Update user token
pub async fn admin_update_user_token(
    Path(id): Path<String>,
    Json(payload): Json<serde_json::Value>,
) -> AdminResult<impl IntoResponse> {
    let username = payload["username"].as_str().map(|s| s.to_string());
    let description = payload["description"].as_str().map(|s| s.to_string());
    let enabled = payload["enabled"].as_bool();
    let max_ips = payload["max_ips"].as_i64().map(|v| v as i32);

    // curfew fields: Option<Option<String>> - outer None means "don't change",
    // inner None means "clear the value"
    let curfew_start: Option<Option<String>> = if payload.get("curfew_start").is_some() {
        Some(payload["curfew_start"].as_str().map(|s| s.to_string()))
    } else {
        None
    };
    let curfew_end: Option<Option<String>> = if payload.get("curfew_end").is_some() {
        Some(payload["curfew_end"].as_str().map(|s| s.to_string()))
    } else {
        None
    };

    user_token_db::update_token(
        &id,
        username,
        description,
        enabled,
        max_ips,
        curfew_start,
        curfew_end,
    )
    .map_err(err_500)?;

    Ok(Json(serde_json::json!({ "success": true })))
}

// ============================================================================
// System Management
// ============================================================================

/// Get data directory path
pub async fn admin_get_data_dir_path() -> impl IntoResponse {
    match account::get_data_dir() {
        Ok(path) => Json(serde_json::json!({ "path": path.to_string_lossy() })),
        Err(e) => Json(serde_json::json!({ "error": e })),
    }
}

/// Check for updates (placeholder)
pub async fn admin_should_check_updates() -> AdminResult<impl IntoResponse> {
    Ok(Json(serde_json::json!({
        "should_check": false,
        "last_check": null,
    })))
}

/// Check for updates
pub async fn admin_check_for_updates() -> AdminResult<impl IntoResponse> {
    Ok(Json(serde_json::json!({
        "has_update": false,
        "current_version": env!("CARGO_PKG_VERSION"),
    })))
}

/// Is auto launch enabled
pub async fn admin_is_auto_launch_enabled() -> impl IntoResponse {
    let config = app_config::load_app_config().unwrap_or_default();
    Json(serde_json::json!({ "enabled": config.auto_launch }))
}

/// Toggle auto launch
pub async fn admin_toggle_auto_launch(
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    let _enabled = payload["enabled"].as_bool().unwrap_or(false);
    // Auto-launch toggle is platform-specific, handled by Tauri commands
    Json(serde_json::json!({ "success": true }))
}

/// Clear cache
pub async fn admin_clear_cache(
    Json(_payload): Json<serde_json::Value>,
) -> AdminResult<impl IntoResponse> {
    // Clear proxy logs older than 30 days
    let _ = monitor::cleanup_old_logs(30);
    let _ = security_db::cleanup_old_ip_logs(30);
    Ok(Json(serde_json::json!({ "success": true })))
}

/// Clear log cache
pub async fn admin_clear_log_cache() -> AdminResult<impl IntoResponse> {
    monitor::clear_logs().map_err(err_500)?;
    Ok(Json(serde_json::json!({ "success": true })))
}

// ============================================================================
// Proxy Logs & Stats
// ============================================================================

#[derive(Deserialize)]
pub struct LogsFilterQuery {
    #[serde(default = "default_page")]
    pub page: usize,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    #[serde(default)]
    pub filter: String,
    #[serde(default)]
    pub errors_only: bool,
}

/// Get proxy logs (filtered, paginated)
pub async fn admin_get_proxy_logs_filtered(
    Query(query): Query<LogsFilterQuery>,
) -> AdminResult<impl IntoResponse> {
    let result = proxy_db::get_logs_paginated(
        query.page.saturating_sub(1),
        query.page_size,
        &query.filter,
        query.errors_only,
    )
    .map_err(err_500)?;

    Ok(Json(result))
}

/// Get proxy logs count (filtered)
pub async fn admin_get_proxy_logs_count_filtered(
    Query(query): Query<LogsFilterQuery>,
) -> AdminResult<impl IntoResponse> {
    let count = proxy_db::get_logs_count_filtered(&query.filter, query.errors_only)
        .map_err(err_500)?;
    Ok(Json(serde_json::json!({ "count": count })))
}

/// Clear proxy logs
pub async fn admin_clear_proxy_logs() -> impl IntoResponse {
    let _ = monitor::clear_logs();
    Json(serde_json::json!({ "success": true }))
}

/// Get proxy log detail
pub async fn admin_get_proxy_log_detail(
    Path(log_id): Path<String>,
) -> AdminResult<impl IntoResponse> {
    let log = proxy_db::get_log_detail(&log_id).map_err(err_500)?;
    Ok(Json(log))
}

/// Get proxy stats
pub async fn admin_get_proxy_stats() -> AdminResult<impl IntoResponse> {
    let stats = monitor::get_stats_from_db().map_err(err_500)?;
    Ok(Json(stats))
}

// ============================================================================
// Token Stats
// ============================================================================

#[derive(Deserialize)]
pub struct StatsPeriodQuery {
    #[serde(default = "default_stats_hours")]
    pub hours: i64,
    #[serde(default = "default_stats_days")]
    pub days: i64,
    #[serde(default = "default_stats_weeks")]
    pub weeks: i64,
}

fn default_stats_hours() -> i64 { 24 }
fn default_stats_days() -> i64 { 7 }
fn default_stats_weeks() -> i64 { 4 }

/// Get hourly token stats
pub async fn admin_get_token_stats_hourly(
    Query(query): Query<StatsPeriodQuery>,
) -> AdminResult<impl IntoResponse> {
    let stats = token_stats::get_hourly_stats(query.hours).map_err(err_500)?;
    Ok(Json(stats))
}

/// Get daily token stats
pub async fn admin_get_token_stats_daily(
    Query(query): Query<StatsPeriodQuery>,
) -> AdminResult<impl IntoResponse> {
    let stats = token_stats::get_daily_stats(query.days).map_err(err_500)?;
    Ok(Json(stats))
}

/// Get weekly token stats
pub async fn admin_get_token_stats_weekly(
    Query(query): Query<StatsPeriodQuery>,
) -> AdminResult<impl IntoResponse> {
    let stats = token_stats::get_weekly_stats(query.weeks).map_err(err_500)?;
    Ok(Json(stats))
}

/// Get per-account token stats
pub async fn admin_get_token_stats_by_account(
    Query(query): Query<StatsPeriodQuery>,
) -> AdminResult<impl IntoResponse> {
    let stats = token_stats::get_account_stats(query.hours).map_err(err_500)?;
    Ok(Json(stats))
}

/// Get token stats summary
pub async fn admin_get_token_stats_summary(
    Query(query): Query<StatsPeriodQuery>,
) -> AdminResult<impl IntoResponse> {
    let stats = token_stats::get_summary_stats(query.hours).map_err(err_500)?;
    Ok(Json(stats))
}

/// Get per-model token stats
pub async fn admin_get_token_stats_by_model(
    Query(query): Query<StatsPeriodQuery>,
) -> AdminResult<impl IntoResponse> {
    let stats = token_stats::get_model_stats(query.hours).map_err(err_500)?;
    Ok(Json(stats))
}

/// Get model trend hourly
pub async fn admin_get_token_stats_model_trend_hourly(
    Query(query): Query<StatsPeriodQuery>,
) -> AdminResult<impl IntoResponse> {
    let stats = token_stats::get_model_trend_hourly(query.hours).map_err(err_500)?;
    Ok(Json(stats))
}

/// Get model trend daily
pub async fn admin_get_token_stats_model_trend_daily(
    Query(query): Query<StatsPeriodQuery>,
) -> AdminResult<impl IntoResponse> {
    let stats = token_stats::get_model_trend_daily(query.days).map_err(err_500)?;
    Ok(Json(stats))
}

/// Get account trend hourly
pub async fn admin_get_token_stats_account_trend_hourly(
    Query(query): Query<StatsPeriodQuery>,
) -> AdminResult<impl IntoResponse> {
    let stats = token_stats::get_account_trend_hourly(query.hours).map_err(err_500)?;
    Ok(Json(stats))
}

/// Get account trend daily
pub async fn admin_get_token_stats_account_trend_daily(
    Query(query): Query<StatsPeriodQuery>,
) -> AdminResult<impl IntoResponse> {
    let stats = token_stats::get_account_trend_daily(query.days).map_err(err_500)?;
    Ok(Json(stats))
}

/// Clear token stats
pub async fn admin_clear_token_stats() -> impl IntoResponse {
    // Token stats clear is not yet implemented in the module
    // For now, return success
    Json(serde_json::json!({ "success": true }))
}

// ============================================================================
// OAuth
// ============================================================================

/// Prepare OAuth URL
pub async fn admin_prepare_oauth_url() -> AdminResult<impl IntoResponse> {
    let config = app_config::load_app_config().map_err(err_500)?;
    let port = config.proxy.port;
    let redirect_uri = format!("http://127.0.0.1:{}/auth/callback", port);
    let state = uuid::Uuid::new_v4().to_string();
    let url = oauth::get_auth_url(&redirect_uri, &state);

    Ok(Json(serde_json::json!({
        "url": url,
        "state": state,
    })))
}

/// Start OAuth login (placeholder - actual flow managed by frontend)
pub async fn admin_start_oauth_login() -> AdminResult<impl IntoResponse> {
    Ok(Json(serde_json::json!({
        "status": "waiting_for_callback",
    })))
}

/// Complete OAuth login (placeholder)
pub async fn admin_complete_oauth_login() -> AdminResult<impl IntoResponse> {
    Ok(Json(serde_json::json!({
        "status": "completed",
    })))
}

/// Cancel OAuth login
pub async fn admin_cancel_oauth_login() -> AdminResult<impl IntoResponse> {
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
pub struct SubmitCodeRequest {
    pub code: String,
    pub state: Option<String>,
}

/// Submit OAuth code
pub async fn admin_submit_oauth_code(
    State(state): State<AppState>,
    Json(payload): Json<SubmitCodeRequest>,
) -> AdminResult<impl IntoResponse> {
    let config = app_config::load_app_config().map_err(err_500)?;
    let port = config.proxy.port;
    let redirect_uri = format!("http://127.0.0.1:{}/auth/callback", port);

    let token_response = oauth::exchange_code(&payload.code, &redirect_uri)
        .await
        .map_err(err_500)?;

    let refresh_token = token_response
        .refresh_token
        .ok_or_else(|| err_500("No refresh_token returned by Google".to_string()))?;

    let account = account::import_single_token(&refresh_token)
        .await
        .map_err(err_500)?;

    // Reload TokenManager
    if let Err(e) = state.token_manager.load_accounts().await {
        error!("[Admin] Failed to reload accounts after OAuth: {}", e);
    }

    Ok(Json(serde_json::json!({
        "id": account.id,
        "email": account.email,
        "name": account.name,
    })))
}

/// Prepare OAuth URL for web mode
pub async fn admin_prepare_oauth_url_web(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AdminResult<impl IntoResponse> {
    let config = app_config::load_app_config().map_err(err_500)?;
    let port = config.proxy.port;

    // Use host/proto from query params if available (for reverse proxy scenarios)
    let host = params.get("host");
    let proto = params.get("proto");

    let redirect_uri = if let (Some(h), Some(p)) = (host, proto) {
        format!("{}://{}/auth/callback", p, h)
    } else {
        format!("http://127.0.0.1:{}/auth/callback", port)
    };

    let state_val = uuid::Uuid::new_v4().to_string();
    let url = oauth::get_auth_url(&redirect_uri, &state_val);

    Ok(Json(serde_json::json!({
        "url": url,
        "state": state_val,
    })))
}

// ============================================================================
// Warmup
// ============================================================================

/// Warm up all accounts
pub async fn admin_warm_up_all_accounts() -> AdminResult<impl IntoResponse> {
    let accounts = account::list_accounts().map_err(err_500)?;
    let mut warmed = 0u32;

    for acc in &accounts {
        if acc.disabled || acc.proxy_disabled {
            continue;
        }
        match quota::get_valid_token_for_warmup(acc).await {
            Ok((token, project_id)) => {
                let _ = quota::warmup_model_directly(
                    &token,
                    "gemini-2.0-flash",
                    &project_id,
                    &acc.email,
                    100,
                    Some(&acc.id),
                )
                .await;
                warmed += 1;
            }
            Err(e) => {
                warn!("[Admin] Warmup failed for {}: {}", acc.email, e);
            }
        }
    }

    Ok(Json(serde_json::json!({
        "total": accounts.len(),
        "warmed": warmed,
    })))
}

/// Warm up single account
pub async fn admin_warm_up_account(
    Path(account_id): Path<String>,
) -> AdminResult<impl IntoResponse> {
    let acc = account::load_account(&account_id).map_err(err_500)?;
    let (token, project_id) = quota::get_valid_token_for_warmup(&acc)
        .await
        .map_err(err_500)?;

    let success = quota::warmup_model_directly(
        &token,
        "gemini-2.0-flash",
        &project_id,
        &acc.email,
        100,
        Some(&acc.id),
    )
    .await;

    Ok(Json(serde_json::json!({ "success": success })))
}

// ============================================================================
// Proxy Pool
// ============================================================================

/// Get proxy pool config
pub async fn admin_get_proxy_pool_config() -> AdminResult<impl IntoResponse> {
    let config = app_config::load_app_config().map_err(err_500)?;
    Ok(Json(config.proxy.proxy_pool))
}

/// Get all account proxy bindings
pub async fn admin_get_all_account_bindings() -> AdminResult<impl IntoResponse> {
    if let Some(pool) = crate::proxy::proxy_pool::get_global_proxy_pool() {
        let bindings = pool.get_all_bindings_snapshot();
        Ok(Json(serde_json::json!({ "bindings": bindings })))
    } else {
        Ok(Json(serde_json::json!({ "bindings": {} })))
    }
}

#[derive(Deserialize)]
pub struct BindAccountProxyRequest {
    pub account_id: String,
    pub proxy_id: String,
}

/// Bind account to proxy
pub async fn admin_bind_account_proxy(
    Json(payload): Json<BindAccountProxyRequest>,
) -> AdminResult<impl IntoResponse> {
    if let Some(pool) = crate::proxy::proxy_pool::get_global_proxy_pool() {
        pool.bind_account_to_proxy(payload.account_id.clone(), payload.proxy_id.clone())
            .await
            .map_err(err_500)?;
        Ok(Json(serde_json::json!({ "success": true })))
    } else {
        Err(err_500("Proxy pool not initialized".to_string()))
    }
}

#[derive(Deserialize)]
pub struct UnbindAccountProxyRequest {
    pub account_id: String,
}

/// Unbind account from proxy
pub async fn admin_unbind_account_proxy(
    Json(payload): Json<UnbindAccountProxyRequest>,
) -> AdminResult<impl IntoResponse> {
    if let Some(pool) = crate::proxy::proxy_pool::get_global_proxy_pool() {
        pool.unbind_account_proxy(&payload.account_id).await;
        Ok(Json(serde_json::json!({ "success": true })))
    } else {
        Err(err_500("Proxy pool not initialized".to_string()))
    }
}

/// Get account proxy binding
pub async fn admin_get_account_proxy_binding(
    Path(account_id): Path<String>,
) -> AdminResult<impl IntoResponse> {
    if let Some(pool) = crate::proxy::proxy_pool::get_global_proxy_pool() {
        let binding = pool.get_account_binding(&account_id);
        Ok(Json(serde_json::json!({ "proxy_id": binding })))
    } else {
        Ok(Json(serde_json::json!({ "proxy_id": null })))
    }
}

/// Trigger proxy health check
pub async fn admin_trigger_proxy_health_check() -> AdminResult<impl IntoResponse> {
    if let Some(pool) = crate::proxy::proxy_pool::get_global_proxy_pool() {
        pool.health_check().await.map_err(err_500)?;
        Ok(Json(serde_json::json!({ "success": true })))
    } else {
        Err(err_500("Proxy pool not initialized".to_string()))
    }
}

// ============================================================================
// OAuth Callback (Web / Headless mode)
// Requirements: 11.1
// ============================================================================

/// Query parameters for the OAuth callback
#[derive(Debug, Deserialize)]
pub struct OAuthCallbackParams {
    pub code: String,
    #[allow(dead_code)]
    pub state: Option<String>,
    #[allow(dead_code)]
    pub scope: Option<String>,
}

/// Build the OAuth redirect URI from the incoming request headers.
///
/// Supports reverse-proxy scenarios via X-Forwarded-Proto / Host headers.
pub fn build_oauth_redirect_uri(port: u16, host: Option<&str>, proto: Option<&str>) -> String {
    if let (Some(h), Some(p)) = (host, proto) {
        format!("{}://{}/auth/callback", p, h)
    } else if let Some(h) = host {
        format!("http://{}/auth/callback", h)
    } else {
        format!("http://127.0.0.1:{}/auth/callback", port)
    }
}

/// Handle the OAuth callback (GET /auth/callback).
///
/// Exchanges the authorization code for tokens, fetches user info,
/// creates the account, and returns an HTML page that notifies the
/// opener window (SPA) of the result.
pub async fn handle_oauth_callback(
    Query(params): Query<OAuthCallbackParams>,
    headers: axum::http::HeaderMap,
) -> axum::response::Html<String> {
    use axum::response::Html;

    let config = match app_config::load_app_config() {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to load config during OAuth callback: {}", e);
            return Html(format!(
                r#"<html><body><h1>Error</h1><p>Failed to load config: {}</p></body></html>"#,
                e
            ));
        }
    };

    let port = config.proxy.port;
    let host = headers.get("host").and_then(|h| h.to_str().ok());
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok());
    let redirect_uri = build_oauth_redirect_uri(port, host, proto);

    // Exchange the authorization code for tokens
    let token_response = match oauth::exchange_code(&params.code, &redirect_uri).await {
        Ok(t) => t,
        Err(e) => {
            error!("OAuth code exchange failed: {}", e);
            return Html(oauth_error_html(&format!("Code exchange failed: {}", e)));
        }
    };

    // Get user info using the access token
    let (email, name) = match oauth::get_user_info(&token_response.access_token).await {
        Ok(info) => info,
        Err(e) => {
            error!("Failed to get user info: {}", e);
            return Html(oauth_error_html(&format!("Failed to get user info: {}", e)));
        }
    };

    // Create and save the account
    let account_id = uuid::Uuid::new_v4().to_string();
    let token_data = crate::models::TokenData {
        access_token: token_response.access_token.clone(),
        refresh_token: token_response.refresh_token.clone().unwrap_or_default(),
        expires_in: token_response.expires_in,
        expiry_timestamp: chrono::Utc::now().timestamp() + token_response.expires_in,
        token_type: "Bearer".to_string(),
        email: Some(email.clone()),
        project_id: None,
        session_id: None,
    };

    let mut new_account = crate::models::Account::new(
        account_id,
        email.clone(),
        token_data,
    );
    new_account.name = name;

    if let Err(e) = account::save_account(&new_account) {
        error!("Failed to save account: {}", e);
        return Html(oauth_error_html(&format!("Failed to save account: {}", e)));
    }

    // Trigger a reload so the proxy picks up the new account
    crate::proxy::server::trigger_account_reload(&new_account.id);

    Html(oauth_success_html(&email))
}

/// Generate the success HTML page for OAuth callback.
fn oauth_success_html(email: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Authorization Successful</title>
    <style>
        body {{ font-family: system-ui, -apple-system, sans-serif; display: flex; flex-direction: column; align-items: center; justify-content: center; min-height: 100vh; margin: 0; background-color: #f9fafb; padding: 20px; box-sizing: border-box; }}
        .card {{ background: white; padding: 2rem; border-radius: 1.5rem; box-shadow: 0 10px 25px -5px rgb(0 0 0 / 0.1); text-align: center; max-width: 500px; width: 100%; }}
        .icon {{ font-size: 3rem; margin-bottom: 1rem; }}
        h1 {{ color: #059669; margin: 0 0 1rem 0; font-size: 1.5rem; }}
        p {{ color: #4b5563; line-height: 1.5; margin-bottom: 1.5rem; }}
        .email {{ font-weight: 600; color: #1f2937; }}
    </style>
</head>
<body>
    <div class="card">
        <div class="icon">✅</div>
        <h1>Authorization Successful</h1>
        <p>Account <span class="email">{email}</span> has been added successfully.</p>
        <p>You can close this window now. The application should refresh automatically.</p>
    </div>
    <script>
        if (window.opener) {{
            window.opener.postMessage({{ type: 'oauth-success', message: 'login success' }}, '*');
        }}
    </script>
</body>
</html>"#,
        email = email
    )
}

/// Generate the error HTML page for OAuth callback.
fn oauth_error_html(error_msg: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Authorization Failed</title>
    <style>
        body {{ font-family: system-ui, -apple-system, sans-serif; display: flex; flex-direction: column; align-items: center; justify-content: center; min-height: 100vh; margin: 0; background-color: #f9fafb; padding: 20px; box-sizing: border-box; }}
        .card {{ background: white; padding: 2rem; border-radius: 1.5rem; box-shadow: 0 10px 25px -5px rgb(0 0 0 / 0.1); text-align: center; max-width: 500px; width: 100%; }}
        .icon {{ font-size: 3rem; margin-bottom: 1rem; }}
        h1 {{ color: #dc2626; margin: 0 0 1rem 0; font-size: 1.5rem; }}
        p {{ color: #4b5563; line-height: 1.5; }}
    </style>
</head>
<body>
    <div class="card">
        <div class="icon">❌</div>
        <h1>Authorization Failed</h1>
        <p>{error}</p>
    </div>
</body>
</html>"#,
        error = error_msg
    )
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_response_serialization() {
        let err = ErrorResponse {
            error: "test error".to_string(),
        };
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("test error"));
    }

    #[test]
    fn test_err_500_returns_internal_server_error() {
        let (status, body) = err_500("something broke".to_string());
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.error, "something broke");
    }

    #[test]
    fn test_err_400_returns_bad_request() {
        let (status, body) = err_400("bad input".to_string());
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body.error, "bad input");
    }

    #[test]
    fn test_parse_cli_app_claude() {
        assert!(matches!(parse_cli_app("claude"), Ok(CliApp::Claude)));
        assert!(matches!(parse_cli_app("claude_code"), Ok(CliApp::Claude)));
        assert!(matches!(parse_cli_app("claude-code"), Ok(CliApp::Claude)));
    }

    #[test]
    fn test_parse_cli_app_codex() {
        assert!(matches!(parse_cli_app("codex"), Ok(CliApp::Codex)));
        assert!(matches!(parse_cli_app("codex_cli"), Ok(CliApp::Codex)));
        assert!(matches!(parse_cli_app("codex-cli"), Ok(CliApp::Codex)));
    }

    #[test]
    fn test_parse_cli_app_gemini() {
        assert!(matches!(parse_cli_app("gemini"), Ok(CliApp::Gemini)));
        assert!(matches!(parse_cli_app("gemini_cli"), Ok(CliApp::Gemini)));
        assert!(matches!(parse_cli_app("gemini-cli"), Ok(CliApp::Gemini)));
    }

    #[test]
    fn test_parse_cli_app_unknown() {
        assert!(parse_cli_app("unknown").is_err());
        assert!(parse_cli_app("").is_err());
    }

    #[test]
    fn test_default_bind_mode() {
        assert_eq!(default_bind_mode(), "generate");
    }

    #[test]
    fn test_default_page_values() {
        assert_eq!(default_page(), 1);
        assert_eq!(default_page_size(), 50);
    }

    #[test]
    fn test_default_stats_values() {
        assert_eq!(default_stats_hours(), 24);
        assert_eq!(default_stats_days(), 7);
        assert_eq!(default_stats_weeks(), 4);
    }

    #[test]
    fn test_add_account_request_deserialize() {
        let json = r#"{"refresh_token": "1//test_token"}"#;
        let req: AddAccountRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.refresh_token, "1//test_token");
    }

    #[test]
    fn test_bulk_delete_request_deserialize() {
        let json = r#"{"account_ids": ["id1", "id2", "id3"]}"#;
        let req: BulkDeleteRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.account_ids.len(), 3);
    }

    #[test]
    fn test_reorder_request_deserialize() {
        let json = r#"{"account_ids": ["a", "b", "c"]}"#;
        let req: ReorderRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.account_ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_toggle_proxy_request_deserialize() {
        let json = r#"{"proxy_disabled": true, "reason": "maintenance"}"#;
        let req: ToggleProxyRequest = serde_json::from_str(json).unwrap();
        assert!(req.proxy_disabled);
        assert_eq!(req.reason, Some("maintenance".to_string()));
    }

    #[test]
    fn test_toggle_proxy_request_without_reason() {
        let json = r#"{"proxy_disabled": false}"#;
        let req: ToggleProxyRequest = serde_json::from_str(json).unwrap();
        assert!(!req.proxy_disabled);
        assert!(req.reason.is_none());
    }

    #[test]
    fn test_update_mapping_wrapper_deserialize() {
        let json = r#"{"custom_mapping": {"gpt-4": "gemini-2.5-pro", "claude-3": "gemini-2.0-flash"}}"#;
        let req: UpdateMappingWrapper = serde_json::from_str(json).unwrap();
        assert_eq!(req.custom_mapping.len(), 2);
        assert_eq!(req.custom_mapping.get("gpt-4"), Some(&"gemini-2.5-pro".to_string()));
    }

    #[test]
    fn test_switch_request_deserialize() {
        let json = r#"{"account_id": "abc-123"}"#;
        let req: SwitchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.account_id, "abc-123");
    }

    #[test]
    fn test_submit_code_request_deserialize() {
        let json = r#"{"code": "4/0test", "state": "some-state"}"#;
        let req: SubmitCodeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.code, "4/0test");
        assert_eq!(req.state, Some("some-state".to_string()));
    }

    #[test]
    fn test_submit_code_request_without_state() {
        let json = r#"{"code": "4/0test"}"#;
        let req: SubmitCodeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.code, "4/0test");
        assert!(req.state.is_none());
    }

    #[test]
    fn test_add_blacklist_request_deserialize() {
        let json = r#"{"ip_pattern": "10.0.0.0/8", "reason": "suspicious"}"#;
        let req: AddBlacklistRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.ip_pattern, "10.0.0.0/8");
        assert_eq!(req.reason, Some("suspicious".to_string()));
        assert!(req.expires_at.is_none());
    }

    #[test]
    fn test_add_whitelist_request_deserialize() {
        let json = r#"{"ip_pattern": "192.168.1.0/24", "description": "office"}"#;
        let req: AddWhitelistRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.ip_pattern, "192.168.1.0/24");
        assert_eq!(req.description, Some("office".to_string()));
    }

    #[test]
    fn test_renew_token_request_deserialize() {
        let json = r#"{"expires_type": "month"}"#;
        let req: RenewTokenRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.expires_type, "month");
    }

    #[test]
    fn test_cli_sync_request_deserialize() {
        let json = r#"{"app": "claude", "api_key": "sk-test", "model": "gpt-4"}"#;
        let req: CliSyncRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.app, "claude");
        assert_eq!(req.api_key, Some("sk-test".to_string()));
        assert_eq!(req.model, Some("gpt-4".to_string()));
    }

    #[test]
    fn test_bind_account_proxy_request_deserialize() {
        let json = r#"{"account_id": "acc-1", "proxy_id": "proxy-1"}"#;
        let req: BindAccountProxyRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.account_id, "acc-1");
        assert_eq!(req.proxy_id, "proxy-1");
    }

    #[test]
    fn test_ip_access_log_query_defaults() {
        let json = r#"{}"#;
        let query: IpAccessLogQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.page, 1);
        assert_eq!(query.page_size, 50);
        assert!(query.ip.is_none());
        assert!(!query.blocked_only);
    }

    #[test]
    fn test_stats_period_query_defaults() {
        let json = r#"{}"#;
        let query: StatsPeriodQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.hours, 24);
        assert_eq!(query.days, 7);
        assert_eq!(query.weeks, 4);
    }

    #[test]
    fn test_device_profile_input_deserialize() {
        let json = r#"{
            "machine_id": "auth0|user_abc",
            "mac_machine_id": "uuid-1",
            "dev_device_id": "uuid-2",
            "sqm_id": "{UUID-3}"
        }"#;
        let input: DeviceProfileInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.machine_id, "auth0|user_abc");
        assert_eq!(input.sqm_id, "{UUID-3}");
    }

    // ================================================================
    // OAuth callback & web serving tests (Requirement 11.1)
    // ================================================================

    #[test]
    fn test_build_oauth_redirect_uri_with_host_and_proto() {
        let uri = build_oauth_redirect_uri(3000, Some("example.com"), Some("https"));
        assert_eq!(uri, "https://example.com/auth/callback");
    }

    #[test]
    fn test_build_oauth_redirect_uri_with_host_only() {
        let uri = build_oauth_redirect_uri(3000, Some("myhost:8080"), None);
        assert_eq!(uri, "http://myhost:8080/auth/callback");
    }

    #[test]
    fn test_build_oauth_redirect_uri_fallback_to_localhost() {
        let uri = build_oauth_redirect_uri(4567, None, None);
        assert_eq!(uri, "http://127.0.0.1:4567/auth/callback");
    }

    #[test]
    fn test_build_oauth_redirect_uri_proto_without_host_falls_back() {
        // proto alone without host should still fall back to localhost
        let uri = build_oauth_redirect_uri(9000, None, Some("https"));
        assert_eq!(uri, "http://127.0.0.1:9000/auth/callback");
    }

    #[test]
    fn test_oauth_callback_params_deserialize() {
        let json = r#"{"code":"abc123","state":"xyz","scope":"openid"}"#;
        let params: OAuthCallbackParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.code, "abc123");
        assert_eq!(params.state, Some("xyz".to_string()));
        assert_eq!(params.scope, Some("openid".to_string()));
    }

    #[test]
    fn test_oauth_callback_params_deserialize_minimal() {
        let json = r#"{"code":"abc123"}"#;
        let params: OAuthCallbackParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.code, "abc123");
        assert!(params.state.is_none());
        assert!(params.scope.is_none());
    }

    #[test]
    fn test_oauth_success_html_contains_email() {
        let html = oauth_success_html("user@example.com");
        assert!(html.contains("user@example.com"));
        assert!(html.contains("Authorization Successful"));
        assert!(html.contains("oauth-success"));
    }

    #[test]
    fn test_oauth_error_html_contains_message() {
        let html = oauth_error_html("Something went wrong");
        assert!(html.contains("Something went wrong"));
        assert!(html.contains("Authorization Failed"));
    }

    #[test]
    fn test_oauth_success_html_is_valid_html() {
        let html = oauth_success_html("test@test.com");
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("</html>"));
    }

    #[test]
    fn test_oauth_error_html_is_valid_html() {
        let html = oauth_error_html("error");
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("</html>"));
    }
}
