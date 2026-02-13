// Tauri commands module
//
// Organizes all Tauri IPC commands into submodules:
// - proxy: Proxy service control (start/stop/status/logs/mapping)
// - user_token: User token CRUD
// - security: IP monitoring, blacklist/whitelist
// - cloudflared: Cloudflare Tunnel management
// - proxy_pool: Proxy pool account bindings
// - autostart: System auto-launch management

pub mod autostart;
pub mod cloudflared;
pub mod proxy;
pub mod proxy_pool;
pub mod security;
pub mod user_token;
