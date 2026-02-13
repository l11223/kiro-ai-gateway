use crate::models::config::{CloudflaredConfig, TunnelMode};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::RwLock;
use tracing::{debug, info};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const DETACHED_PROCESS: u32 = 0x00000008;
#[cfg(target_os = "windows")]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;

// ============================================================================
// CloudflaredStatus
// ============================================================================

/// Cloudflared tunnel status information
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CloudflaredStatus {
    pub installed: bool,
    pub version: Option<String>,
    pub running: bool,
    pub url: Option<String>,
    pub error: Option<String>,
}

impl Default for CloudflaredStatus {
    fn default() -> Self {
        Self {
            installed: false,
            version: None,
            running: false,
            url: None,
            error: None,
        }
    }
}

// ============================================================================
// CloudflaredManager
// ============================================================================

/// Manages the cloudflared tunnel process lifecycle: install, start, stop, status.
pub struct CloudflaredManager {
    process: Arc<RwLock<Option<Child>>>,
    status: Arc<RwLock<CloudflaredStatus>>,
    bin_path: PathBuf,
    shutdown_tx: RwLock<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl CloudflaredManager {
    /// Create a new manager. `data_dir` is the application data directory;
    /// the binary will be stored at `{data_dir}/bin/cloudflared[.exe]`.
    pub fn new(data_dir: &PathBuf) -> Self {
        let bin_name = if cfg!(target_os = "windows") {
            "cloudflared.exe"
        } else {
            "cloudflared"
        };
        let bin_path = data_dir.join("bin").join(bin_name);

        Self {
            process: Arc::new(RwLock::new(None)),
            status: Arc::new(RwLock::new(CloudflaredStatus::default())),
            bin_path,
            shutdown_tx: RwLock::new(None),
        }
    }

    /// Check whether the cloudflared binary is installed and return its version.
    pub async fn check_installed(&self) -> (bool, Option<String>) {
        if !self.bin_path.exists() {
            return (false, None);
        }

        let mut cmd = Command::new(&self.bin_path);
        cmd.arg("--version");
        #[cfg(target_os = "windows")]
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

        match cmd.output().await {
            Ok(output) => {
                if output.status.success() {
                    let version = String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .next()
                        .map(|s| s.trim().to_string());
                    (true, version)
                } else {
                    (false, None)
                }
            }
            Err(_) => (false, None),
        }
    }

    /// Return the current cached status.
    pub async fn get_status(&self) -> CloudflaredStatus {
        self.status.read().await.clone()
    }

    /// Mutate the cached status via a closure.
    async fn update_status(&self, f: impl FnOnce(&mut CloudflaredStatus)) {
        let mut status = self.status.write().await;
        f(&mut status);
    }

    /// Download and install the cloudflared binary for the current platform.
    pub async fn install(&self) -> Result<CloudflaredStatus, String> {
        let bin_dir = self
            .bin_path
            .parent()
            .ok_or_else(|| "Invalid bin path".to_string())?;
        if !bin_dir.exists() {
            std::fs::create_dir_all(bin_dir)
                .map_err(|e| format!("Failed to create bin directory: {}", e))?;
        }

        let download_url = get_download_url()?;
        info!("[cloudflared] Downloading from: {}", download_url);

        let response = reqwest::get(&download_url)
            .await
            .map_err(|e| format!("Download failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!(
                "Download failed with status: {}",
                response.status()
            ));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!("Failed to read response: {}", e))?;

        let is_archive = download_url.ends_with(".tgz");
        if is_archive {
            let archive_path = self.bin_path.with_extension("tgz");
            std::fs::write(&archive_path, &bytes)
                .map_err(|e| format!("Failed to write archive: {}", e))?;

            let status = Command::new("tar")
                .arg("-xzf")
                .arg(&archive_path)
                .arg("-C")
                .arg(bin_dir)
                .status()
                .await
                .map_err(|e| format!("Failed to extract archive: {}", e))?;

            if !status.success() {
                return Err("Failed to extract cloudflared archive".to_string());
            }

            let _ = std::fs::remove_file(&archive_path);
        } else {
            std::fs::write(&self.bin_path, &bytes)
                .map_err(|e| format!("Failed to write binary: {}", e))?;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.bin_path, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("Failed to set permissions: {}", e))?;
        }

        let (installed, version) = self.check_installed().await;
        self.update_status(|s| {
            s.installed = installed;
            s.version = version.clone();
        })
        .await;

        info!(
            "[cloudflared] Installed successfully, version: {:?}",
            version
        );
        Ok(self.get_status().await)
    }

    /// Start the cloudflared tunnel with the given configuration.
    ///
    /// In `Quick` mode a temporary trycloudflare.com URL is created.
    /// In `Auth` mode a named tunnel is run using the provided token.
    pub async fn start(&self, config: CloudflaredConfig) -> Result<CloudflaredStatus, String> {
        // Already running – return current status
        {
            let proc = self.process.read().await;
            if proc.is_some() {
                return Ok(self.get_status().await);
            }
        }

        // Cancel any previous process-monitor task
        if let Some(tx) = self.shutdown_tx.write().await.take() {
            let _ = tx.send(());
        }

        let (installed, version) = self.check_installed().await;
        if !installed {
            return Err("Cloudflared not installed".to_string());
        }

        let local_url = format!("http://localhost:{}", config.port);
        info!("[cloudflared] Starting tunnel to: {}", local_url);

        let mut cmd = Command::new(&self.bin_path);

        if let Some(bin_dir) = self.bin_path.parent() {
            cmd.current_dir(bin_dir);
            debug!("[cloudflared] Working directory: {:?}", bin_dir);
        }

        match config.mode {
            TunnelMode::Quick => {
                cmd.arg("tunnel").arg("--url").arg(&local_url);
                if config.use_http2 {
                    cmd.arg("--protocol").arg("http2");
                }
                info!("[cloudflared] Quick mode: tunnel --url {}", local_url);
            }
            TunnelMode::Auth => {
                if let Some(token) = &config.token {
                    cmd.arg("tunnel")
                        .arg("run")
                        .arg("--token")
                        .arg(token);
                    if config.use_http2 {
                        cmd.arg("--protocol").arg("http2");
                    }
                    info!("[cloudflared] Auth mode: tunnel run --token [HIDDEN]");
                } else {
                    return Err("Token required for auth mode".to_string());
                }
            }
        }

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        #[cfg(target_os = "windows")]
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn cloudflared: {}", e))?;

        // Spawn log readers that parse stdout/stderr for the tunnel URL
        let status_clone = self.status.clone();
        if let Some(stdout) = child.stdout.take() {
            spawn_log_reader(stdout, status_clone.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_reader(stderr, status_clone);
        }

        *self.process.write().await = Some(child);
        self.update_status(|s| {
            s.installed = installed;
            s.version = version;
            s.running = true;
            s.error = None;
        })
        .await;

        // Spawn a background task that monitors the child process
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        *self.shutdown_tx.write().await = Some(shutdown_tx);

        let process_ref = self.process.clone();
        let status_ref = self.status.clone();

        tokio::spawn(async move {
            tokio::select! {
                _ = shutdown_rx => {
                    debug!("[cloudflared] Process monitor shutdown");
                }
                _ = async {
                    loop {
                        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

                        let mut proc_lock = process_ref.write().await;
                        if let Some(ref mut child) = *proc_lock {
                            match child.try_wait() {
                                Ok(Some(exit_status)) => {
                                    info!("[cloudflared] Process exited: {:?}", exit_status);
                                    *proc_lock = None;
                                    drop(proc_lock);
                                    let mut s = status_ref.write().await;
                                    s.running = false;
                                    s.error = Some(format!(
                                        "Tunnel process exited (status: {:?})",
                                        exit_status
                                    ));
                                    break;
                                }
                                Ok(None) => { /* still running */ }
                                Err(e) => {
                                    info!("[cloudflared] Error checking process: {}", e);
                                    *proc_lock = None;
                                    drop(proc_lock);
                                    let mut s = status_ref.write().await;
                                    s.running = false;
                                    s.error =
                                        Some(format!("Error checking tunnel: {}", e));
                                    break;
                                }
                            }
                        } else {
                            drop(proc_lock);
                            let mut s = status_ref.write().await;
                            if s.running {
                                s.running = false;
                                s.error = Some("Tunnel process not found".to_string());
                            }
                            break;
                        }
                    }
                } => {}
            }
        });

        Ok(self.get_status().await)
    }

    /// Stop the running tunnel process.
    pub async fn stop(&self) -> Result<CloudflaredStatus, String> {
        // Signal the monitor task to stop
        if let Some(tx) = self.shutdown_tx.write().await.take() {
            let _ = tx.send(());
        }

        let mut proc_lock = self.process.write().await;
        if let Some(mut child) = proc_lock.take() {
            let _ = child.kill().await;
            info!("[cloudflared] Tunnel stopped");
        }

        self.update_status(|s| {
            s.running = false;
            s.url = None;
            s.error = None;
        })
        .await;

        Ok(self.get_status().await)
    }
}

// ============================================================================
// Helper functions
// ============================================================================

/// Return the download URL for the cloudflared binary matching the current OS/arch.
fn get_download_url() -> Result<String, String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let (os_str, arch_str, ext) = match (os, arch) {
        ("macos", "aarch64") => ("darwin", "arm64", ".tgz"),
        ("macos", "x86_64") => ("darwin", "amd64", ".tgz"),
        ("linux", "x86_64") => ("linux", "amd64", ""),
        ("linux", "aarch64") => ("linux", "arm64", ""),
        ("windows", "x86_64") => ("windows", "amd64", ".exe"),
        _ => return Err(format!("Unsupported platform: {}-{}", os, arch)),
    };

    Ok(format!(
        "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-{}-{}{}",
        os_str, arch_str, ext
    ))
}

/// Spawn a tokio task that reads lines from an async stream and looks for the tunnel URL.
fn spawn_log_reader<R>(stream: R, status_ref: Arc<RwLock<CloudflaredStatus>>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let reader = BufReader::new(stream);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            debug!("[cloudflared output] {}", line);
            if let Some(url) = extract_tunnel_url(&line) {
                info!("[cloudflared] Tunnel URL: {}", url);
                let mut s = status_ref.write().await;
                s.url = Some(url);
            }
        }
    });
}

/// Extract the public tunnel URL from a cloudflared log line.
///
/// Supports two patterns:
/// 1. Quick tunnel: `https://<random>.trycloudflare.com`
/// 2. Named tunnel: hostname from `Updated to new configuration` ingress JSON
fn extract_tunnel_url(line: &str) -> Option<String> {
    // Quick tunnel: look for trycloudflare.com URL
    if let Some(url) = line
        .split_whitespace()
        .find(|s| s.starts_with("https://") && s.contains(".trycloudflare.com"))
    {
        return Some(url.to_string());
    }

    // Named tunnel: parse hostname from ingress config log
    if line.contains("Updated to new configuration") && line.contains("ingress") {
        if let Some(start) = line.find("\\\"hostname\\\":\\\"") {
            let after_key = &line[start + 15..];
            if let Some(end) = after_key.find("\\\"") {
                let hostname = &after_key[..end];
                if !hostname.is_empty() {
                    return Some(format!("https://{}", hostname));
                }
            }
        }
    }

    None
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cloudflared_status_default() {
        let status = CloudflaredStatus::default();
        assert!(!status.installed);
        assert!(status.version.is_none());
        assert!(!status.running);
        assert!(status.url.is_none());
        assert!(status.error.is_none());
    }

    #[test]
    fn test_cloudflared_status_serialization_roundtrip() {
        let status = CloudflaredStatus {
            installed: true,
            version: Some("2024.1.0".to_string()),
            running: true,
            url: Some("https://test-abc.trycloudflare.com".to_string()),
            error: None,
        };
        let json = serde_json::to_string(&status).unwrap();
        let deserialized: CloudflaredStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, deserialized);
    }

    #[test]
    fn test_extract_tunnel_url_quick_mode() {
        let line = "2024-01-01T00:00:00Z INF +-------------------------------------------+\n";
        assert_eq!(extract_tunnel_url(line), None);

        let line =
            "2024-01-01T00:00:00Z INF |  https://random-name-here.trycloudflare.com  |";
        assert_eq!(
            extract_tunnel_url(line),
            Some("https://random-name-here.trycloudflare.com".to_string())
        );
    }

    #[test]
    fn test_extract_tunnel_url_named_tunnel() {
        let line = r#"Updated to new configuration config="{\"ingress\":[{\"hostname\":\"api.example.com\", \"service\":\"http://localhost:8045\"}]}"#;
        assert_eq!(
            extract_tunnel_url(line),
            Some("https://api.example.com".to_string())
        );
    }

    #[test]
    fn test_extract_tunnel_url_no_match() {
        assert_eq!(extract_tunnel_url("some random log line"), None);
        assert_eq!(extract_tunnel_url(""), None);
        assert_eq!(
            extract_tunnel_url("https://example.com is not a tunnel"),
            None
        );
    }

    #[test]
    fn test_extract_tunnel_url_named_empty_hostname() {
        let line = r#"Updated to new configuration config="{\"ingress\":[{\"hostname\":\"\", \"service\":\"http://localhost:8045\"}]}"#;
        assert_eq!(extract_tunnel_url(line), None);
    }

    #[test]
    fn test_get_download_url_returns_ok() {
        // Should succeed on any supported platform (linux/macos/windows x86_64/aarch64)
        let result = get_download_url();
        // We can't guarantee the platform in CI, but the function should not panic
        match result {
            Ok(url) => {
                assert!(url.starts_with("https://github.com/cloudflare/cloudflared/"));
                assert!(url.contains("cloudflared-"));
            }
            Err(e) => {
                // Unsupported platform is acceptable in test
                assert!(e.contains("Unsupported platform"));
            }
        }
    }

    #[test]
    fn test_manager_new_sets_bin_path() {
        let data_dir = PathBuf::from("/tmp/test-data");
        let manager = CloudflaredManager::new(&data_dir);

        if cfg!(target_os = "windows") {
            assert_eq!(
                manager.bin_path,
                PathBuf::from("/tmp/test-data/bin/cloudflared.exe")
            );
        } else {
            assert_eq!(
                manager.bin_path,
                PathBuf::from("/tmp/test-data/bin/cloudflared")
            );
        }
    }

    #[tokio::test]
    async fn test_manager_check_installed_missing_binary() {
        let data_dir = PathBuf::from("/tmp/nonexistent-cloudflared-test-dir");
        let manager = CloudflaredManager::new(&data_dir);
        let (installed, version) = manager.check_installed().await;
        assert!(!installed);
        assert!(version.is_none());
    }

    #[tokio::test]
    async fn test_manager_get_status_default() {
        let data_dir = PathBuf::from("/tmp/test-cf-status");
        let manager = CloudflaredManager::new(&data_dir);
        let status = manager.get_status().await;
        assert!(!status.installed);
        assert!(!status.running);
        assert!(status.url.is_none());
    }

    #[tokio::test]
    async fn test_manager_start_not_installed() {
        let data_dir = PathBuf::from("/tmp/nonexistent-cf-start-test");
        let manager = CloudflaredManager::new(&data_dir);
        let config = CloudflaredConfig::default();
        let result = manager.start(config).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not installed"));
    }

    #[tokio::test]
    async fn test_manager_stop_when_not_running() {
        let data_dir = PathBuf::from("/tmp/test-cf-stop");
        let manager = CloudflaredManager::new(&data_dir);
        let result = manager.stop().await;
        assert!(result.is_ok());
        let status = result.unwrap();
        assert!(!status.running);
        assert!(status.url.is_none());
    }

    #[tokio::test]
    async fn test_manager_start_auth_mode_no_token() {
        let data_dir = PathBuf::from("/tmp/nonexistent-cf-auth-test");
        let manager = CloudflaredManager::new(&data_dir);
        // Even though binary doesn't exist, auth mode without token should fail first
        // Actually, it checks installed first, so we get "not installed"
        let config = CloudflaredConfig {
            enabled: true,
            mode: TunnelMode::Auth,
            port: 8045,
            token: None,
            use_http2: true,
        };
        let result = manager.start(config).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_cloudflared_config_default_values() {
        let config = CloudflaredConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.mode, TunnelMode::Quick);
        assert_eq!(config.port, 8045);
        assert!(config.token.is_none());
        assert!(config.use_http2);
    }

    #[test]
    fn test_cloudflared_config_serde_roundtrip() {
        let config = CloudflaredConfig {
            enabled: true,
            mode: TunnelMode::Auth,
            port: 9090,
            token: Some("my-tunnel-token".to_string()),
            use_http2: false,
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: CloudflaredConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_tunnel_mode_default() {
        assert_eq!(TunnelMode::default(), TunnelMode::Quick);
    }

    #[test]
    fn test_tunnel_mode_serde() {
        let quick_json = serde_json::to_string(&TunnelMode::Quick).unwrap();
        assert_eq!(quick_json, "\"quick\"");
        let auth_json = serde_json::to_string(&TunnelMode::Auth).unwrap();
        assert_eq!(auth_json, "\"auth\"");

        let deserialized: TunnelMode = serde_json::from_str("\"quick\"").unwrap();
        assert_eq!(deserialized, TunnelMode::Quick);
        let deserialized: TunnelMode = serde_json::from_str("\"auth\"").unwrap();
        assert_eq!(deserialized, TunnelMode::Auth);
    }

    #[test]
    fn test_extract_tunnel_url_with_pipe_chars() {
        // cloudflared outputs the URL inside a box with pipe characters
        let line = "2024-01-01T00:00:00Z INF |  https://abc-def-ghi.trycloudflare.com  |";
        let url = extract_tunnel_url(line);
        assert_eq!(
            url,
            Some("https://abc-def-ghi.trycloudflare.com".to_string())
        );
    }

    #[tokio::test]
    async fn test_update_status() {
        let data_dir = PathBuf::from("/tmp/test-cf-update-status");
        let manager = CloudflaredManager::new(&data_dir);

        manager
            .update_status(|s| {
                s.installed = true;
                s.version = Some("1.0.0".to_string());
                s.running = true;
                s.url = Some("https://test.trycloudflare.com".to_string());
            })
            .await;

        let status = manager.get_status().await;
        assert!(status.installed);
        assert_eq!(status.version, Some("1.0.0".to_string()));
        assert!(status.running);
        assert_eq!(
            status.url,
            Some("https://test.trycloudflare.com".to_string())
        );
    }
}
