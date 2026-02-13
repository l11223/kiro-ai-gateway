pub mod models;
pub mod modules;
pub mod commands;
pub mod utils;
pub mod proxy;

use tracing::info;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! Welcome to Kiro AI Gateway!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    let is_headless = args.iter().any(|arg| arg == "--headless");

    if is_headless {
        info!("Starting in HEADLESS mode...");
        let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
        rt.block_on(async {
            info!("Headless proxy service is running.");
            tokio::signal::ctrl_c().await.ok();
            info!("Headless mode shutting down");
        });
        return;
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            use tauri::Manager;
            let _ = app.get_webview_window("main").map(|window| {
                let _ = window.show();
                let _ = window.set_focus();
            });
        }))
        .setup(|_app| {
            info!("Kiro AI Gateway setup complete");
            Ok(())
        })
        .manage(commands::proxy::ProxyServiceState::new())
        .manage(commands::cloudflared::CloudflaredState::new())
        .invoke_handler(tauri::generate_handler![
            greet,
            // Proxy service commands
            commands::proxy::start_proxy_service,
            commands::proxy::stop_proxy_service,
            commands::proxy::get_proxy_status,
            commands::proxy::get_proxy_stats,
            commands::proxy::get_proxy_logs,
            commands::proxy::set_proxy_monitor_enabled,
            commands::proxy::clear_proxy_logs,
            commands::proxy::get_proxy_logs_paginated,
            commands::proxy::get_proxy_log_detail,
            commands::proxy::get_proxy_logs_count,
            commands::proxy::export_proxy_logs,
            commands::proxy::export_proxy_logs_json,
            commands::proxy::get_proxy_logs_count_filtered,
            commands::proxy::get_proxy_logs_filtered,
            commands::proxy::generate_api_key,
            commands::proxy::reload_proxy_accounts,
            commands::proxy::update_model_mapping,
            commands::proxy::check_proxy_health,
            commands::proxy::get_proxy_pool_config,
            commands::proxy::get_proxy_scheduling_config,
            commands::proxy::update_proxy_scheduling_config,
            commands::proxy::clear_proxy_session_bindings,
            commands::proxy::set_preferred_account,
            commands::proxy::get_preferred_account,
            commands::proxy::clear_proxy_rate_limit,
            commands::proxy::clear_all_proxy_rate_limits,
            // User Token commands
            commands::user_token::list_user_tokens,
            commands::user_token::create_user_token,
            commands::user_token::update_user_token,
            commands::user_token::delete_user_token,
            commands::user_token::renew_user_token,
            commands::user_token::get_token_ip_bindings,
            commands::user_token::get_user_token_summary,
            // Security commands
            commands::security::get_ip_access_logs,
            commands::security::get_ip_stats,
            commands::security::clear_ip_access_logs,
            commands::security::get_ip_blacklist,
            commands::security::add_ip_to_blacklist,
            commands::security::remove_ip_from_blacklist,
            commands::security::clear_ip_blacklist,
            commands::security::check_ip_in_blacklist,
            commands::security::get_ip_whitelist,
            commands::security::add_ip_to_whitelist,
            commands::security::remove_ip_from_whitelist,
            commands::security::clear_ip_whitelist,
            commands::security::check_ip_in_whitelist,
            commands::security::get_security_config,
            commands::security::update_security_config,
            commands::security::get_ip_token_stats,
            // Cloudflared commands
            commands::cloudflared::cloudflared_check,
            commands::cloudflared::cloudflared_install,
            commands::cloudflared::cloudflared_start,
            commands::cloudflared::cloudflared_stop,
            commands::cloudflared::cloudflared_get_status,
            // Proxy Pool commands
            commands::proxy_pool::bind_account_proxy,
            commands::proxy_pool::unbind_account_proxy,
            commands::proxy_pool::get_account_proxy_binding,
            commands::proxy_pool::get_all_account_bindings,
            // Autostart commands
            commands::autostart::toggle_auto_launch,
            commands::autostart::is_auto_launch_enabled,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Kiro AI Gateway");
}
