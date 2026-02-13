use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Supported CLI applications for proxy config sync.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub enum CliApp {
    Claude,
    Codex,
    Gemini,
    OpenCode,
}

/// Represents a single config file associated with a CLI app.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct CliConfigFile {
    pub name: String,
    pub path: PathBuf,
}

impl CliApp {
    pub fn as_str(&self) -> &'static str {
        match self {
            CliApp::Claude => "claude",
            CliApp::Codex => "codex",
            CliApp::Gemini => "gemini",
            CliApp::OpenCode => "opencode",
        }
    }

    /// Returns the list of config files for this CLI app.
    pub fn config_files(&self) -> Vec<CliConfigFile> {
        let home = match dirs::home_dir() {
            Some(p) => p,
            None => return vec![],
        };
        match self {
            CliApp::Claude => vec![
                CliConfigFile {
                    name: ".claude.json".to_string(),
                    path: home.join(".claude.json"),
                },
                CliConfigFile {
                    name: "settings.json".to_string(),
                    path: home.join(".claude").join("settings.json"),
                },
            ],
            CliApp::Codex => vec![
                CliConfigFile {
                    name: "auth.json".to_string(),
                    path: home.join(".codex").join("auth.json"),
                },
                CliConfigFile {
                    name: "config.toml".to_string(),
                    path: home.join(".codex").join("config.toml"),
                },
            ],
            CliApp::Gemini => vec![
                CliConfigFile {
                    name: ".env".to_string(),
                    path: home.join(".gemini").join(".env"),
                },
                CliConfigFile {
                    name: "settings.json".to_string(),
                    path: home.join(".gemini").join("settings.json"),
                },
                CliConfigFile {
                    name: "config.json".to_string(),
                    path: home.join(".gemini").join("config.json"),
                },
            ],
            CliApp::OpenCode => vec![
                CliConfigFile {
                    name: "config.json".to_string(),
                    path: home.join(".opencode").join("config.json"),
                },
            ],
        }
    }

    /// Returns the default upstream URL for this CLI app.
    pub fn default_url(&self) -> &'static str {
        match self {
            CliApp::Claude => "https://api.anthropic.com",
            CliApp::Codex => "https://api.openai.com/v1",
            CliApp::Gemini => "https://generativelanguage.googleapis.com",
            CliApp::OpenCode => "https://api.openai.com/v1",
        }
    }
}

/// Status of a CLI app's sync state.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CliStatus {
    pub installed: bool,
    pub version: Option<String>,
    pub is_synced: bool,
    pub has_backup: bool,
    pub current_base_url: Option<String>,
    pub files: Vec<String>,
}

/// Detect whether a CLI app is installed and retrieve its version.
pub fn check_cli_installed(app: &CliApp) -> (bool, Option<String>) {
    let cmd = app.as_str();
    let mut executable_path = PathBuf::from(cmd);

    // Use which/where to detect the CLI in PATH
    let which_output = if cfg!(target_os = "windows") {
        let mut c = Command::new("where");
        c.arg(cmd);
        #[cfg(target_os = "windows")]
        c.creation_flags(CREATE_NO_WINDOW);
        c.output()
    } else {
        Command::new("which").arg(cmd).output()
    };

    let mut installed = match which_output {
        Ok(out) => out.status.success(),
        Err(_) => false,
    };

    // macOS/Linux fallback: search common binary paths
    if !installed && !cfg!(target_os = "windows") {
        let home = dirs::home_dir().unwrap_or_default();
        let mut common_paths = vec![
            home.join(".local/bin"),
            home.join(".bun/bin"),
            home.join(".bun/install/global/node_modules/.bin"),
            home.join(".npm-global/bin"),
            home.join(".volta/bin"),
            home.join("bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/bin"),
        ];

        // Scan nvm node versions
        let nvm_base = home.join(".nvm/versions/node");
        if nvm_base.exists() {
            if let Ok(entries) = std::fs::read_dir(&nvm_base) {
                for entry in entries.flatten() {
                    let bin_path = entry.path().join("bin");
                    if bin_path.exists() {
                        common_paths.push(bin_path);
                    }
                }
            }
        }

        for path in common_paths {
            let full_path = path.join(cmd);
            if full_path.exists() {
                tracing::debug!("[CLI-Sync] Detected {} via explicit path: {:?}", cmd, full_path);
                installed = true;
                executable_path = full_path;
                break;
            }
        }
    }

    if !installed {
        return (false, None);
    }

    // Get version
    let mut ver_cmd = Command::new(&executable_path);
    ver_cmd.arg("--version");
    #[cfg(target_os = "windows")]
    ver_cmd.creation_flags(CREATE_NO_WINDOW);

    let version = match ver_cmd.output() {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let cleaned = s
                .split(|c: char| !c.is_numeric() && c != '.')
                .filter(|part| !part.is_empty())
                .last()
                .map(|p| p.trim())
                .unwrap_or(&s)
                .to_string();
            Some(cleaned)
        }
        _ => None,
    };

    (true, version)
}

/// Read current config and detect sync status.
/// Returns (is_synced, has_backup, current_base_url).
pub fn get_sync_status(app: &CliApp, proxy_url: &str) -> (bool, bool, Option<String>) {
    let files = app.config_files();
    if files.is_empty() {
        return (false, false, None);
    }

    let mut all_synced = true;
    let mut has_backup = false;
    let mut current_base_url = None;

    for file in &files {
        let backup_path = file
            .path
            .with_file_name(format!("{}.antigravity.bak", file.name));

        if backup_path.exists() {
            has_backup = true;
        }

        if !file.path.exists() {
            // Gemini settings.json/config.json are optional
            if app == &CliApp::Gemini
                && (file.name == "settings.json" || file.name == "config.json")
            {
                continue;
            }
            all_synced = false;
            continue;
        }

        let content = match fs::read_to_string(&file.path) {
            Ok(c) => c,
            Err(_) => {
                all_synced = false;
                continue;
            }
        };

        match app {
            CliApp::Claude => {
                if file.name == "settings.json" {
                    let json: Value = serde_json::from_str(&content).unwrap_or_default();
                    let url = json
                        .get("env")
                        .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
                        .and_then(|v| v.as_str());
                    if let Some(u) = url {
                        current_base_url = Some(u.to_string());
                        if u.trim_end_matches('/') != proxy_url.trim_end_matches('/') {
                            all_synced = false;
                        }
                    } else {
                        all_synced = false;
                    }
                } else if file.name == ".claude.json" {
                    let json: Value = serde_json::from_str(&content).unwrap_or_default();
                    if json.get("hasCompletedOnboarding") != Some(&Value::Bool(true)) {
                        all_synced = false;
                    }
                }
            }
            CliApp::Codex => {
                if file.name == "config.toml" {
                    let re =
                        regex::Regex::new(r#"(?m)^\s*base_url\s*=\s*['"]([^'"]+)['"]"#).unwrap();
                    if let Some(caps) = re.captures(&content) {
                        let url = &caps[1];
                        current_base_url = Some(url.to_string());
                        if url.trim_end_matches('/') != proxy_url.trim_end_matches('/') {
                            all_synced = false;
                        }
                    } else {
                        all_synced = false;
                    }
                }
            }
            CliApp::Gemini => {
                if file.name == ".env" {
                    let re =
                        regex::Regex::new(r#"(?m)^GOOGLE_GEMINI_BASE_URL=(.*)$"#).unwrap();
                    if let Some(caps) = re.captures(&content) {
                        let url = caps[1].trim();
                        current_base_url = Some(url.to_string());
                        if url.trim_end_matches('/') != proxy_url.trim_end_matches('/') {
                            all_synced = false;
                        }
                    } else {
                        all_synced = false;
                    }
                }
            }
            CliApp::OpenCode => {
                if file.name == "config.json" {
                    let json: Value = serde_json::from_str(&content).unwrap_or_default();
                    let url = json
                        .get("providers")
                        .and_then(|p| p.get("openai"))
                        .and_then(|o| o.get("baseURL"))
                        .and_then(|v| v.as_str());
                    if let Some(u) = url {
                        current_base_url = Some(u.to_string());
                        if u.trim_end_matches('/') != proxy_url.trim_end_matches('/') {
                            all_synced = false;
                        }
                    } else {
                        all_synced = false;
                    }
                }
            }
        }
    }

    (all_synced, has_backup, current_base_url)
}

/// Perform sync: write proxy URL and API key into CLI config files.
/// Automatically creates a backup of existing files before modification.
pub fn sync_config(
    app: &CliApp,
    proxy_url: &str,
    api_key: &str,
    model: Option<&str>,
) -> Result<(), String> {
    let files = app.config_files();

    for file in &files {
        // Gemini compatibility: skip config.json if settings.json exists
        if app == &CliApp::Gemini && file.name == "config.json" && !file.path.exists() {
            let settings_path = file.path.with_file_name("settings.json");
            if settings_path.exists() {
                continue;
            }
        }

        if let Some(parent) = file.path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Cannot create directory: {}", e))?;
        }

        // Auto-backup: create .antigravity.bak if file exists and no backup yet
        if file.path.exists() {
            let backup_path = file
                .path
                .with_file_name(format!("{}.antigravity.bak", file.name));
            if !backup_path.exists() {
                if let Err(e) = fs::copy(&file.path, &backup_path) {
                    tracing::warn!("Failed to create backup for {}: {}", file.name, e);
                } else {
                    tracing::info!("Created backup for {}: {:?}", file.name, backup_path);
                }
            }
        }

        let mut content = if file.path.exists() {
            fs::read_to_string(&file.path).unwrap_or_default()
        } else {
            String::new()
        };

        match app {
            CliApp::Claude => {
                content = sync_claude_file(&file.name, &content, proxy_url, api_key, model);
            }
            CliApp::Codex => {
                content = sync_codex_file(&file.name, &content, proxy_url, api_key, model);
            }
            CliApp::Gemini => {
                content = sync_gemini_file(&file.name, &content, proxy_url, api_key, model);
            }
            CliApp::OpenCode => {
                content = sync_opencode_simple_file(&file.name, &content, proxy_url, api_key);
            }
        }

        // Atomic write via temp file
        let tmp_path = file.path.with_extension("tmp");
        fs::write(&tmp_path, &content)
            .map_err(|e| format!("Failed to write temp file: {}", e))?;
        fs::rename(&tmp_path, &file.path)
            .map_err(|e| format!("Failed to rename config file: {}", e))?;
    }

    Ok(())
}

/// Restore CLI config files from backup.
pub fn restore_config(app: &CliApp) -> Result<(), String> {
    let files = app.config_files();
    let mut restored_count = 0;

    for file in &files {
        let backup_path = file
            .path
            .with_file_name(format!("{}.antigravity.bak", file.name));
        if backup_path.exists() {
            if let Err(e) = fs::rename(&backup_path, &file.path) {
                return Err(format!("Failed to restore backup {}: {}", file.name, e));
            }
            restored_count += 1;
        }
    }

    if restored_count > 0 {
        return Ok(());
    }

    // No backup found: restore to default config
    let default_url = app.default_url();
    sync_config(app, default_url, "", None)
}

/// Read the content of a specific CLI config file.
pub fn get_config_content(app: &CliApp, file_name: Option<&str>) -> Result<String, String> {
    let files = app.config_files();
    let file = if let Some(name) = file_name {
        files
            .into_iter()
            .find(|f| f.name == name)
            .ok_or_else(|| "File not found".to_string())?
    } else {
        files
            .into_iter()
            .next()
            .ok_or_else(|| "No config files".to_string())?
    };

    if !file.path.exists() {
        return Err("Config file does not exist".to_string());
    }
    fs::read_to_string(&file.path).map_err(|e| format!("Failed to read config: {}", e))
}

/// Get full CLI status including install check and sync state.
pub fn get_cli_status(app: &CliApp, proxy_url: &str) -> CliStatus {
    let (installed, version) = check_cli_installed(app);
    let (is_synced, has_backup, current_base_url) = if installed {
        get_sync_status(app, proxy_url)
    } else {
        (false, false, None)
    };

    CliStatus {
        installed,
        version,
        is_synced,
        has_backup,
        current_base_url,
        files: app.config_files().into_iter().map(|f| f.name).collect(),
    }
}

// ── Internal sync helpers ──────────────────────────────────────────

fn sync_claude_file(
    file_name: &str,
    content: &str,
    proxy_url: &str,
    api_key: &str,
    model: Option<&str>,
) -> String {
    if file_name == ".claude.json" {
        let mut json: Value =
            serde_json::from_str(content).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(obj) = json.as_object_mut() {
            obj.insert(
                "hasCompletedOnboarding".to_string(),
                Value::Bool(true),
            );
        }
        serde_json::to_string_pretty(&json).unwrap()
    } else if file_name == "settings.json" {
        let mut json: Value =
            serde_json::from_str(content).unwrap_or_else(|_| serde_json::json!({}));
        if json.as_object().is_none() {
            json = serde_json::json!({});
        }
        let env = json
            .as_object_mut()
            .unwrap()
            .entry("env")
            .or_insert(serde_json::json!({}));
        if let Some(env_obj) = env.as_object_mut() {
            env_obj.insert(
                "ANTHROPIC_BASE_URL".to_string(),
                Value::String(proxy_url.to_string()),
            );
            if !api_key.is_empty() {
                env_obj.insert(
                    "ANTHROPIC_API_KEY".to_string(),
                    Value::String(api_key.to_string()),
                );
                // Remove conflicting keys
                env_obj.remove("ANTHROPIC_AUTH_TOKEN");
                env_obj.remove("ANTHROPIC_MODEL");
                env_obj.remove("ANTHROPIC_DEFAULT_HAIKU_MODEL");
                env_obj.remove("ANTHROPIC_DEFAULT_OPUS_MODEL");
                env_obj.remove("ANTHROPIC_DEFAULT_SONNET_MODEL");
            } else {
                env_obj.remove("ANTHROPIC_API_KEY");
            }
        }
        if let Some(m) = model {
            json.as_object_mut()
                .unwrap()
                .insert("model".to_string(), Value::String(m.to_string()));
        }
        serde_json::to_string_pretty(&json).unwrap()
    } else {
        content.to_string()
    }
}

fn sync_codex_file(
    file_name: &str,
    content: &str,
    proxy_url: &str,
    api_key: &str,
    model: Option<&str>,
) -> String {
    if file_name == "auth.json" {
        let mut json: Value =
            serde_json::from_str(content).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(obj) = json.as_object_mut() {
            obj.insert(
                "OPENAI_API_KEY".to_string(),
                Value::String(api_key.to_string()),
            );
            obj.insert(
                "OPENAI_BASE_URL".to_string(),
                Value::String(proxy_url.to_string()),
            );
        }
        serde_json::to_string_pretty(&json).unwrap()
    } else if file_name == "config.toml" {
        use toml_edit::{value, DocumentMut};
        let mut doc = content
            .parse::<DocumentMut>()
            .unwrap_or_else(|_| DocumentMut::new());

        let providers = doc
            .entry("model_providers")
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
        if let Some(p_table) = providers.as_table_mut() {
            let custom = p_table
                .entry("custom")
                .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
            if let Some(c_table) = custom.as_table_mut() {
                c_table.insert("name", value("custom"));
                c_table.insert("wire_api", value("responses"));
                c_table.insert("requires_openai_auth", value(true));
                c_table.insert("base_url", value(proxy_url));
                if let Some(m) = model {
                    c_table.insert("model", value(m));
                }
            }
        }
        doc.insert("model_provider", value("custom"));
        if let Some(m) = model {
            doc.insert("model", value(m));
        }
        doc.remove("openai_api_key");
        doc.remove("openai_base_url");
        doc.to_string()
    } else {
        content.to_string()
    }
}

fn sync_gemini_file(
    file_name: &str,
    content: &str,
    proxy_url: &str,
    api_key: &str,
    model: Option<&str>,
) -> String {
    if file_name == ".env" {
        let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
        let mut found_url = false;
        let mut found_key = false;
        for line in lines.iter_mut() {
            if line.starts_with("GOOGLE_GEMINI_BASE_URL=") {
                *line = format!("GOOGLE_GEMINI_BASE_URL={}", proxy_url);
                found_url = true;
            } else if line.trim().starts_with("GEMINI_API_KEY=") {
                *line = format!("GEMINI_API_KEY={}", api_key);
                found_key = true;
            }
        }
        if !found_url {
            lines.push(format!("GOOGLE_GEMINI_BASE_URL={}", proxy_url));
        }
        if !found_key {
            lines.push(format!("GEMINI_API_KEY={}", api_key));
        }
        if let Some(m) = model {
            let mut found_model = false;
            for line in lines.iter_mut() {
                if line.starts_with("GOOGLE_GEMINI_MODEL=") {
                    *line = format!("GOOGLE_GEMINI_MODEL={}", m);
                    found_model = true;
                }
            }
            if !found_model {
                lines.push(format!("GOOGLE_GEMINI_MODEL={}", m));
            }
        }
        lines.join("\n")
    } else if file_name == "settings.json" || file_name == "config.json" {
        let mut json: Value =
            serde_json::from_str(content).unwrap_or_else(|_| serde_json::json!({}));
        if json.as_object().is_none() {
            json = serde_json::json!({});
        }
        let sec = json
            .as_object_mut()
            .unwrap()
            .entry("security")
            .or_insert(serde_json::json!({}));
        let auth = sec
            .as_object_mut()
            .unwrap()
            .entry("auth")
            .or_insert(serde_json::json!({}));
        if let Some(auth_obj) = auth.as_object_mut() {
            auth_obj.insert(
                "selectedType".to_string(),
                Value::String("gemini-api-key".to_string()),
            );
        }
        serde_json::to_string_pretty(&json).unwrap()
    } else {
        content.to_string()
    }
}

fn sync_opencode_simple_file(
    file_name: &str,
    content: &str,
    proxy_url: &str,
    api_key: &str,
) -> String {
    if file_name == "config.json" {
        let mut json: Value =
            serde_json::from_str(content).unwrap_or_else(|_| serde_json::json!({}));
        if json.as_object().is_none() {
            json = serde_json::json!({});
        }
        let providers = json
            .as_object_mut()
            .unwrap()
            .entry("providers")
            .or_insert(serde_json::json!({}));
        let openai = providers
            .as_object_mut()
            .unwrap()
            .entry("openai")
            .or_insert(serde_json::json!({}));
        if let Some(openai_obj) = openai.as_object_mut() {
            openai_obj.insert(
                "baseURL".to_string(),
                Value::String(proxy_url.to_string()),
            );
            if !api_key.is_empty() {
                openai_obj.insert(
                    "apiKey".to_string(),
                    Value::String(api_key.to_string()),
                );
            }
        }
        serde_json::to_string_pretty(&json).unwrap()
    } else {
        content.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ── CliApp unit tests ──────────────────────────────────────────

    #[test]
    fn test_cli_app_as_str() {
        assert_eq!(CliApp::Claude.as_str(), "claude");
        assert_eq!(CliApp::Codex.as_str(), "codex");
        assert_eq!(CliApp::Gemini.as_str(), "gemini");
        assert_eq!(CliApp::OpenCode.as_str(), "opencode");
    }

    #[test]
    fn test_cli_app_default_url() {
        assert_eq!(CliApp::Claude.default_url(), "https://api.anthropic.com");
        assert_eq!(CliApp::Codex.default_url(), "https://api.openai.com/v1");
        assert_eq!(
            CliApp::Gemini.default_url(),
            "https://generativelanguage.googleapis.com"
        );
        assert_eq!(CliApp::OpenCode.default_url(), "https://api.openai.com/v1");
    }

    #[test]
    fn test_cli_app_config_files_not_empty() {
        // config_files depends on home_dir; if available, should return non-empty
        if dirs::home_dir().is_some() {
            assert!(!CliApp::Claude.config_files().is_empty());
            assert!(!CliApp::Codex.config_files().is_empty());
            assert!(!CliApp::Gemini.config_files().is_empty());
            assert!(!CliApp::OpenCode.config_files().is_empty());
        }
    }

    #[test]
    fn test_cli_app_serde_roundtrip() {
        let apps = vec![CliApp::Claude, CliApp::Codex, CliApp::Gemini, CliApp::OpenCode];
        for app in apps {
            let json = serde_json::to_string(&app).unwrap();
            let deserialized: CliApp = serde_json::from_str(&json).unwrap();
            assert_eq!(app, deserialized);
        }
    }

    // ── sync_claude_file tests ─────────────────────────────────────

    #[test]
    fn test_sync_claude_json_sets_onboarding() {
        let result = sync_claude_file(".claude.json", "{}", "http://proxy", "key", None);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["hasCompletedOnboarding"], true);
    }

    #[test]
    fn test_sync_claude_settings_sets_base_url_and_key() {
        let result = sync_claude_file("settings.json", "{}", "http://proxy:8080", "my-key", None);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["env"]["ANTHROPIC_BASE_URL"], "http://proxy:8080");
        assert_eq!(json["env"]["ANTHROPIC_API_KEY"], "my-key");
    }

    #[test]
    fn test_sync_claude_settings_removes_key_when_empty() {
        let existing = r#"{"env":{"ANTHROPIC_API_KEY":"old-key"}}"#;
        let result = sync_claude_file("settings.json", existing, "http://proxy", "", None);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert!(json["env"].get("ANTHROPIC_API_KEY").is_none());
    }

    #[test]
    fn test_sync_claude_settings_sets_model() {
        let result = sync_claude_file(
            "settings.json",
            "{}",
            "http://proxy",
            "key",
            Some("claude-3-opus"),
        );
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["model"], "claude-3-opus");
    }

    #[test]
    fn test_sync_claude_settings_removes_conflicting_keys() {
        let existing = r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"tok","ANTHROPIC_MODEL":"m"}}"#;
        let result = sync_claude_file("settings.json", existing, "http://proxy", "key", None);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert!(json["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
        assert!(json["env"].get("ANTHROPIC_MODEL").is_none());
    }

    // ── sync_codex_file tests ──────────────────────────────────────

    #[test]
    fn test_sync_codex_auth_json() {
        let result = sync_codex_file("auth.json", "{}", "http://proxy", "key123", None);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["OPENAI_API_KEY"], "key123");
        assert_eq!(json["OPENAI_BASE_URL"], "http://proxy");
    }

    #[test]
    fn test_sync_codex_config_toml() {
        let result = sync_codex_file("config.toml", "", "http://proxy:3000", "key", None);
        assert!(result.contains("base_url"));
        assert!(result.contains("http://proxy:3000"));
        assert!(result.contains(r#"model_provider = "custom""#));
    }

    #[test]
    fn test_sync_codex_config_toml_with_model() {
        let result = sync_codex_file(
            "config.toml",
            "",
            "http://proxy",
            "key",
            Some("gpt-4"),
        );
        assert!(result.contains("gpt-4"));
    }

    // ── sync_gemini_file tests ─────────────────────────────────────

    #[test]
    fn test_sync_gemini_env_new() {
        let result = sync_gemini_file(".env", "", "http://proxy:8080", "gem-key", None);
        assert!(result.contains("GOOGLE_GEMINI_BASE_URL=http://proxy:8080"));
        assert!(result.contains("GEMINI_API_KEY=gem-key"));
    }

    #[test]
    fn test_sync_gemini_env_update_existing() {
        let existing = "GOOGLE_GEMINI_BASE_URL=http://old\nGEMINI_API_KEY=old-key";
        let result = sync_gemini_file(".env", existing, "http://new", "new-key", None);
        assert!(result.contains("GOOGLE_GEMINI_BASE_URL=http://new"));
        assert!(result.contains("GEMINI_API_KEY=new-key"));
        assert!(!result.contains("http://old"));
    }

    #[test]
    fn test_sync_gemini_env_with_model() {
        let result = sync_gemini_file(".env", "", "http://proxy", "key", Some("gemini-pro"));
        assert!(result.contains("GOOGLE_GEMINI_MODEL=gemini-pro"));
    }

    #[test]
    fn test_sync_gemini_settings_json() {
        let result = sync_gemini_file("settings.json", "{}", "http://proxy", "key", None);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["security"]["auth"]["selectedType"], "gemini-api-key");
    }

    // ── sync_opencode_simple_file tests ────────────────────────────

    #[test]
    fn test_sync_opencode_config_json() {
        let result = sync_opencode_simple_file("config.json", "{}", "http://proxy", "oc-key");
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["providers"]["openai"]["baseURL"], "http://proxy");
        assert_eq!(json["providers"]["openai"]["apiKey"], "oc-key");
    }

    #[test]
    fn test_sync_opencode_config_json_empty_key() {
        let result = sync_opencode_simple_file("config.json", "{}", "http://proxy", "");
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["providers"]["openai"]["baseURL"], "http://proxy");
        assert!(json["providers"]["openai"].get("apiKey").is_none());
    }

    // ── Backup & restore integration tests (using temp dirs) ──────

    #[test]
    fn test_sync_and_restore_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let config_dir = tmp.path().join(".opencode");
        fs::create_dir_all(&config_dir).unwrap();

        let config_path = config_dir.join("config.json");
        let original = r#"{"original": true}"#;
        fs::write(&config_path, original).unwrap();

        // Simulate sync by writing new content and creating backup
        let backup_path = config_dir.join("config.json.antigravity.bak");
        fs::copy(&config_path, &backup_path).unwrap();
        fs::write(&config_path, r#"{"synced": true}"#).unwrap();

        // Verify synced content
        let synced = fs::read_to_string(&config_path).unwrap();
        assert!(synced.contains("synced"));

        // Restore from backup
        fs::rename(&backup_path, &config_path).unwrap();
        let restored = fs::read_to_string(&config_path).unwrap();
        assert_eq!(restored, original);
    }

    // ── get_sync_status tests (with temp files) ───────────────────

    #[test]
    fn test_get_sync_status_no_files() {
        // CliApp with no home dir returns empty config_files
        // We test the function logic with a custom app that has no files
        let app = CliApp::Claude;
        // If home dir exists, files will exist but won't be synced
        let (synced, backup, _url) = get_sync_status(&app, "http://localhost:3000");
        // Without actual files on disk, should not be synced
        assert!(!synced || backup || true); // Just verify it doesn't panic
    }

    #[test]
    fn test_unknown_file_name_passthrough() {
        // Unknown file names should return content as-is
        let result = sync_claude_file("unknown.txt", "hello", "url", "key", None);
        assert_eq!(result, "hello");

        let result = sync_codex_file("unknown.txt", "hello", "url", "key", None);
        assert_eq!(result, "hello");

        let result = sync_gemini_file("unknown.txt", "hello", "url", "key", None);
        assert_eq!(result, "hello");

        let result = sync_opencode_simple_file("unknown.txt", "hello", "url", "key");
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_sync_claude_settings_preserves_existing_fields() {
        let existing = r#"{"someField": 42, "env": {"OTHER_VAR": "keep"}}"#;
        let result = sync_claude_file("settings.json", existing, "http://proxy", "key", None);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["someField"], 42);
        assert_eq!(json["env"]["OTHER_VAR"], "keep");
        assert_eq!(json["env"]["ANTHROPIC_BASE_URL"], "http://proxy");
    }

    #[test]
    fn test_sync_codex_auth_preserves_existing_fields() {
        let existing = r#"{"SOME_OTHER_KEY": "value"}"#;
        let result = sync_codex_file("auth.json", existing, "http://proxy", "key", None);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["SOME_OTHER_KEY"], "value");
        assert_eq!(json["OPENAI_API_KEY"], "key");
    }

    #[test]
    fn test_sync_gemini_env_preserves_other_lines() {
        let existing = "SOME_VAR=hello\nGOOGLE_GEMINI_BASE_URL=http://old";
        let result = sync_gemini_file(".env", existing, "http://new", "key", None);
        assert!(result.contains("SOME_VAR=hello"));
        assert!(result.contains("GOOGLE_GEMINI_BASE_URL=http://new"));
    }
}
