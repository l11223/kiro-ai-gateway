// Autostart management commands
//
// Tauri commands for managing system auto-launch on startup.

use tauri_plugin_autostart::ManagerExt;

// ============================================================================
// Tauri Commands
// ============================================================================

/// Toggle auto-launch on system startup
#[tauri::command]
pub async fn toggle_auto_launch(app: tauri::AppHandle, enable: bool) -> Result<(), String> {
    let manager = app.autolaunch();

    if enable {
        manager
            .enable()
            .map_err(|e| format!("启用自动启动失败: {}", e))?;
        tracing::info!("已启用开机自动启动");
    } else {
        match manager.disable() {
            Ok(_) => {
                tracing::info!("已禁用开机自动启动");
            }
            Err(e) => {
                let err_msg = e.to_string();
                // On Windows, if the registry key doesn't exist, disable() returns
                // "系统找不到指定的文件" (os error 2). Treat as success since the
                // goal (disabled) is already achieved.
                if err_msg.contains("os error 2") || err_msg.contains("找不到指定的文件") {
                    tracing::info!("开机自启项已不存在，视为禁用成功");
                } else {
                    return Err(format!("禁用自动启动失败: {}", e));
                }
            }
        }
    }

    Ok(())
}

/// Check if auto-launch is enabled
#[tauri::command]
pub async fn is_auto_launch_enabled(app: tauri::AppHandle) -> Result<bool, String> {
    let manager = app.autolaunch();
    manager.is_enabled().map_err(|e| e.to_string())
}
