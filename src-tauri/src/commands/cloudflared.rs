// Cloudflared tunnel control commands
//
// Tauri commands for managing the Cloudflare Tunnel lifecycle:
// check installation, install, start, stop, and get status.

use crate::models::config::CloudflaredConfig;
use crate::modules::cloudflared::{CloudflaredManager, CloudflaredStatus};
use std::sync::Arc;
use tauri::State;
use tokio::sync::RwLock;

// ============================================================================
// State
// ============================================================================

/// Cloudflared service state managed by Tauri
#[derive(Clone)]
pub struct CloudflaredState {
    pub manager: Arc<RwLock<Option<CloudflaredManager>>>,
}

impl CloudflaredState {
    pub fn new() -> Self {
        Self {
            manager: Arc::new(RwLock::new(None)),
        }
    }

    /// Ensure the manager is initialized
    pub async fn ensure_manager(&self) -> Result<(), String> {
        let mut lock = self.manager.write().await;
        if lock.is_none() {
            let data_dir = crate::modules::account::get_data_dir()?;
            *lock = Some(CloudflaredManager::new(&data_dir));
        }
        Ok(())
    }
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Check if cloudflared is installed
#[tauri::command]
pub async fn cloudflared_check(
    state: State<'_, CloudflaredState>,
) -> Result<CloudflaredStatus, String> {
    state.ensure_manager().await?;
    let lock = state.manager.read().await;
    if let Some(manager) = lock.as_ref() {
        let (installed, version) = manager.check_installed().await;
        Ok(CloudflaredStatus {
            installed,
            version,
            running: false,
            url: None,
            error: None,
        })
    } else {
        Err("Manager not initialized".to_string())
    }
}

/// Install cloudflared
#[tauri::command]
pub async fn cloudflared_install(
    state: State<'_, CloudflaredState>,
) -> Result<CloudflaredStatus, String> {
    state.ensure_manager().await?;
    let lock = state.manager.read().await;
    if let Some(manager) = lock.as_ref() {
        manager.install().await
    } else {
        Err("Manager not initialized".to_string())
    }
}

/// Start cloudflared tunnel
#[tauri::command]
pub async fn cloudflared_start(
    state: State<'_, CloudflaredState>,
    config: CloudflaredConfig,
) -> Result<CloudflaredStatus, String> {
    state.ensure_manager().await?;
    let lock = state.manager.read().await;
    if let Some(manager) = lock.as_ref() {
        manager.start(config).await
    } else {
        Err("Manager not initialized".to_string())
    }
}

/// Stop cloudflared tunnel
#[tauri::command]
pub async fn cloudflared_stop(
    state: State<'_, CloudflaredState>,
) -> Result<CloudflaredStatus, String> {
    state.ensure_manager().await?;
    let lock = state.manager.read().await;
    if let Some(manager) = lock.as_ref() {
        manager.stop().await
    } else {
        Err("Manager not initialized".to_string())
    }
}

/// Get cloudflared status
#[tauri::command]
pub async fn cloudflared_get_status(
    state: State<'_, CloudflaredState>,
) -> Result<CloudflaredStatus, String> {
    state.ensure_manager().await?;
    let lock = state.manager.read().await;
    if let Some(manager) = lock.as_ref() {
        let (installed, version) = manager.check_installed().await;
        let mut status = manager.get_status().await;
        status.installed = installed;
        status.version = version;
        if !installed {
            status.running = false;
            status.url = None;
        }
        Ok(status)
    } else {
        Ok(CloudflaredStatus::default())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cloudflared_state_new() {
        let state = CloudflaredState::new();
        // Manager should be None initially
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let lock = state.manager.read().await;
            assert!(lock.is_none());
        });
    }

    #[test]
    fn test_cloudflared_status_default() {
        let status = CloudflaredStatus::default();
        assert!(!status.installed);
        assert!(status.version.is_none());
        assert!(!status.running);
        assert!(status.url.is_none());
        assert!(status.error.is_none());
    }
}
