// Proxy pool management commands
//
// Tauri commands for managing account-to-proxy bindings
// in the proxy pool system.

use crate::commands::proxy::ProxyServiceState;
use std::collections::HashMap;
use tauri::State;

// ============================================================================
// Tauri Commands
// ============================================================================

/// Bind an account to a specific proxy
#[tauri::command]
pub async fn bind_account_proxy(
    state: State<'_, ProxyServiceState>,
    account_id: String,
    proxy_id: String,
) -> Result<(), String> {
    let instance_lock = state.instance.read().await;
    if instance_lock.is_some() {
        let app_config = crate::modules::config::load_app_config()
            .map_err(|e| format!("Failed to load config: {}", e))?;
        let pool_config_arc =
            std::sync::Arc::new(tokio::sync::RwLock::new(app_config.proxy.proxy_pool));
        let manager = crate::proxy::proxy_pool::ProxyPoolManager::new(pool_config_arc);
        manager.bind_account_to_proxy(account_id, proxy_id).await
    } else {
        Err("Service not running".to_string())
    }
}

/// Unbind an account from its proxy
#[tauri::command]
pub async fn unbind_account_proxy(
    state: State<'_, ProxyServiceState>,
    account_id: String,
) -> Result<(), String> {
    let instance_lock = state.instance.read().await;
    if instance_lock.is_some() {
        let app_config = crate::modules::config::load_app_config()
            .map_err(|e| format!("Failed to load config: {}", e))?;
        let pool_config_arc =
            std::sync::Arc::new(tokio::sync::RwLock::new(app_config.proxy.proxy_pool));
        let manager = crate::proxy::proxy_pool::ProxyPoolManager::new(pool_config_arc);
        manager.unbind_account_proxy(&account_id).await;
        Ok(())
    } else {
        Err("Service not running".to_string())
    }
}

/// Get the proxy binding for a specific account
#[tauri::command]
pub async fn get_account_proxy_binding(
    state: State<'_, ProxyServiceState>,
    account_id: String,
) -> Result<Option<String>, String> {
    let instance_lock = state.instance.read().await;
    if instance_lock.is_some() {
        let app_config = crate::modules::config::load_app_config()
            .map_err(|e| format!("Failed to load config: {}", e))?;
        let pool_config_arc =
            std::sync::Arc::new(tokio::sync::RwLock::new(app_config.proxy.proxy_pool));
        let manager = crate::proxy::proxy_pool::ProxyPoolManager::new(pool_config_arc);
        Ok(manager.get_account_binding(&account_id))
    } else {
        Err("Service not running".to_string())
    }
}

/// Get all account proxy bindings
#[tauri::command]
pub async fn get_all_account_bindings(
    state: State<'_, ProxyServiceState>,
) -> Result<HashMap<String, String>, String> {
    let instance_lock = state.instance.read().await;
    if instance_lock.is_some() {
        let app_config = crate::modules::config::load_app_config()
            .map_err(|e| format!("Failed to load config: {}", e))?;
        let pool_config_arc =
            std::sync::Arc::new(tokio::sync::RwLock::new(app_config.proxy.proxy_pool));
        let manager = crate::proxy::proxy_pool::ProxyPoolManager::new(pool_config_arc);
        Ok(manager.get_all_bindings_snapshot())
    } else {
        Err("Service not running".to_string())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_bindings_hashmap() {
        let map: HashMap<String, String> = HashMap::new();
        let json = serde_json::to_string(&map).unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn test_bindings_hashmap_serialization() {
        let mut map = HashMap::new();
        map.insert("acc-1".to_string(), "proxy-a".to_string());
        map.insert("acc-2".to_string(), "proxy-b".to_string());
        let json = serde_json::to_string(&map).unwrap();
        let deserialized: HashMap<String, String> = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.len(), 2);
        assert_eq!(deserialized.get("acc-1"), Some(&"proxy-a".to_string()));
    }
}
