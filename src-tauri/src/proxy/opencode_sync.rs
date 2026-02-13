use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const OPENCODE_DIR: &str = ".config/opencode";
const OPENCODE_CONFIG_FILE: &str = "opencode.json";
const ANTIGRAVITY_ACCOUNTS_FILE: &str = "antigravity-accounts.json";
const BACKUP_SUFFIX: &str = ".antigravity-manager.bak";
const OLD_BACKUP_SUFFIX: &str = ".antigravity.bak";
const ANTIGRAVITY_PROVIDER_ID: &str = "antigravity-manager";

/// Variant type for model thinking configurations.
#[derive(Debug, Clone, Copy)]
enum VariantType {
    ClaudeThinking,
    Gemini3Pro,
    Gemini3Flash,
    Gemini25Thinking,
}

/// Model definition with metadata and variants.
#[derive(Debug, Clone)]
struct ModelDef {
    id: &'static str,
    name: &'static str,
    context_limit: u32,
    output_limit: u32,
    input_modalities: &'static [&'static str],
    output_modalities: &'static [&'static str],
    reasoning: bool,
    variant_type: Option<VariantType>,
}

/// Build the complete model catalog for the antigravity-manager provider.
fn build_model_catalog() -> Vec<ModelDef> {
    vec![
        ModelDef { id: "claude-sonnet-4-5", name: "Claude Sonnet 4.5", context_limit: 200_000, output_limit: 64_000, input_modalities: &["text", "image", "pdf"], output_modalities: &["text"], reasoning: false, variant_type: None },
        ModelDef { id: "claude-sonnet-4-5-thinking", name: "Claude Sonnet 4.5 Thinking", context_limit: 200_000, output_limit: 64_000, input_modalities: &["text", "image", "pdf"], output_modalities: &["text"], reasoning: true, variant_type: Some(VariantType::ClaudeThinking) },
        ModelDef { id: "claude-opus-4-5-thinking", name: "Claude Opus 4.5 Thinking", context_limit: 200_000, output_limit: 64_000, input_modalities: &["text", "image", "pdf"], output_modalities: &["text"], reasoning: true, variant_type: Some(VariantType::ClaudeThinking) },
        ModelDef { id: "gemini-3-pro-high", name: "Gemini 3 Pro High", context_limit: 1_048_576, output_limit: 65_535, input_modalities: &["text", "image", "pdf"], output_modalities: &["text", "image"], reasoning: true, variant_type: Some(VariantType::Gemini3Pro) },
        ModelDef { id: "gemini-3-pro-low", name: "Gemini 3 Pro Low", context_limit: 1_048_576, output_limit: 65_535, input_modalities: &["text", "image", "pdf"], output_modalities: &["text", "image"], reasoning: true, variant_type: Some(VariantType::Gemini3Pro) },
        ModelDef { id: "gemini-3-flash", name: "Gemini 3 Flash", context_limit: 1_048_576, output_limit: 65_536, input_modalities: &["text", "image", "pdf"], output_modalities: &["text"], reasoning: true, variant_type: Some(VariantType::Gemini3Flash) },
        ModelDef { id: "gemini-3-pro-image", name: "Gemini 3 Pro Image", context_limit: 1_048_576, output_limit: 65_535, input_modalities: &["text", "image", "pdf"], output_modalities: &["text", "image"], reasoning: false, variant_type: None },
        ModelDef { id: "gemini-2.5-flash", name: "Gemini 2.5 Flash", context_limit: 1_048_576, output_limit: 65_536, input_modalities: &["text", "image", "pdf"], output_modalities: &["text"], reasoning: false, variant_type: None },
        ModelDef { id: "gemini-2.5-flash-lite", name: "Gemini 2.5 Flash Lite", context_limit: 1_048_576, output_limit: 65_536, input_modalities: &["text", "image", "pdf"], output_modalities: &["text"], reasoning: false, variant_type: None },
        ModelDef { id: "gemini-2.5-flash-thinking", name: "Gemini 2.5 Flash Thinking", context_limit: 1_048_576, output_limit: 65_536, input_modalities: &["text", "image", "pdf"], output_modalities: &["text"], reasoning: true, variant_type: Some(VariantType::Gemini25Thinking) },
        ModelDef { id: "gemini-2.5-pro", name: "Gemini 2.5 Pro", context_limit: 1_048_576, output_limit: 65_536, input_modalities: &["text", "image", "pdf"], output_modalities: &["text"], reasoning: true, variant_type: None },
    ]
}

/// Normalize OpenCode base URL to ensure it ends with `/v1`.
pub fn normalize_opencode_base_url(input: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        trimmed.to_string()
    } else {
        format!("{}/v1", trimmed)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OpencodeStatus {
    pub installed: bool,
    pub version: Option<String>,
    pub is_synced: bool,
    pub has_backup: bool,
    pub current_base_url: Option<String>,
    pub files: Vec<String>,
}

fn get_opencode_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(OPENCODE_DIR))
}

fn get_config_paths() -> Option<(PathBuf, PathBuf)> {
    get_opencode_dir().map(|dir| (dir.join(OPENCODE_CONFIG_FILE), dir.join(ANTIGRAVITY_ACCOUNTS_FILE)))
}

fn extract_version(raw: &str) -> String {
    let trimmed = raw.trim();
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    for part in parts {
        if let Some(slash_idx) = part.find('/') {
            let after_slash = &part[slash_idx + 1..];
            if is_valid_version(after_slash) {
                return after_slash.to_string();
            }
        }
        if is_valid_version(part) {
            return part.to_string();
        }
    }
    let version_chars: String = trimmed
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if !version_chars.is_empty() && version_chars.contains('.') {
        return version_chars;
    }
    "unknown".to_string()
}

fn is_valid_version(s: &str) -> bool {
    s.chars().next().map_or(false, |c| c.is_ascii_digit())
        && s.contains('.')
        && s.chars().all(|c| c.is_ascii_digit() || c == '.')
}

fn find_in_path(executable: &str) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let extensions = ["exe", "cmd", "bat"];
        if let Ok(path_var) = env::var("PATH") {
            for dir in path_var.split(';') {
                for ext in &extensions {
                    let full_path = PathBuf::from(dir).join(format!("{}.{}", executable, ext));
                    if full_path.exists() {
                        return Some(full_path);
                    }
                }
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(path_var) = env::var("PATH") {
            for dir in path_var.split(':') {
                let full_path = PathBuf::from(dir).join(executable);
                if full_path.exists() {
                    return Some(full_path);
                }
            }
        }
    }
    None
}

fn resolve_opencode_path() -> Option<PathBuf> {
    if let Some(path) = find_in_path("opencode") {
        return Some(path);
    }

    #[cfg(not(target_os = "windows"))]
    {
        let home = dirs::home_dir()?;
        let candidates = [
            home.join(".local/bin/opencode"),
            home.join(".npm-global/bin/opencode"),
            home.join(".volta/bin/opencode"),
            home.join("bin/opencode"),
            PathBuf::from("/opt/homebrew/bin/opencode"),
            PathBuf::from("/usr/local/bin/opencode"),
            PathBuf::from("/usr/bin/opencode"),
        ];
        for path in &candidates {
            if path.exists() {
                return Some(path.clone());
            }
        }
        // Scan nvm
        let nvm_dir = home.join(".nvm/versions/node");
        if nvm_dir.exists() {
            if let Ok(entries) = fs::read_dir(&nvm_dir) {
                for entry in entries.flatten() {
                    let opencode = entry.path().join("bin/opencode");
                    if opencode.exists() {
                        return Some(opencode);
                    }
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(app_data) = env::var("APPDATA") {
            for name in &["opencode.cmd", "opencode.exe"] {
                let p = PathBuf::from(&app_data).join("npm").join(name);
                if p.exists() {
                    return Some(p);
                }
            }
        }
        if let Ok(local) = env::var("LOCALAPPDATA") {
            for name in &["opencode.cmd", "opencode.exe"] {
                let p = PathBuf::from(&local).join("pnpm").join(name);
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }

    None
}

/// Check if OpenCode CLI is installed and get its version.
pub fn check_opencode_installed() -> (bool, Option<String>) {
    let opencode_path = match resolve_opencode_path() {
        Some(path) => path,
        None => return (false, None),
    };

    let mut cmd = Command::new(&opencode_path);
    cmd.arg("--version");
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    match cmd.output() {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let raw = if stdout.trim().is_empty() {
                stderr.to_string()
            } else {
                stdout.to_string()
            };
            (true, Some(extract_version(&raw)))
        }
        _ => (false, None),
    }
}

/// Get sync status for OpenCode.
/// Returns (is_synced, has_backup, current_base_url).
pub fn get_sync_status(proxy_url: &str) -> (bool, bool, Option<String>) {
    let Some((config_path, _)) = get_config_paths() else {
        return (false, false, None);
    };

    let backup_path = config_path.with_file_name(format!("{}{}", OPENCODE_CONFIG_FILE, BACKUP_SUFFIX));
    let old_backup_path = config_path.with_file_name(format!("{}{}", OPENCODE_CONFIG_FILE, OLD_BACKUP_SUFFIX));
    let has_backup = backup_path.exists() || old_backup_path.exists();

    if !config_path.exists() {
        return (false, has_backup, None);
    }

    let content = match fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return (false, has_backup, None),
    };

    let json: Value = serde_json::from_str(&content).unwrap_or_default();
    let normalized_proxy = normalize_opencode_base_url(proxy_url);

    let mut is_synced = true;
    let mut current_base_url = None;

    let ag_opts = json
        .get("provider")
        .and_then(|p| p.get(ANTIGRAVITY_PROVIDER_ID))
        .and_then(|prov| prov.get("options"));
    let ag_url = ag_opts.and_then(|o| o.get("baseURL")).and_then(|v| v.as_str());
    let ag_key = ag_opts.and_then(|o| o.get("apiKey")).and_then(|v| v.as_str());

    if let (Some(url), Some(_key)) = (ag_url, ag_key) {
        current_base_url = Some(url.to_string());
        if normalize_opencode_base_url(url) != normalized_proxy {
            is_synced = false;
        }
    } else {
        is_synced = false;
    }

    (is_synced, has_backup, current_base_url)
}

fn create_backup(path: &PathBuf) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let backup_path = path.with_file_name(format!(
        "{}{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        BACKUP_SUFFIX
    ));
    if backup_path.exists() {
        return Ok(());
    }
    fs::copy(path, &backup_path).map_err(|e| format!("Failed to create backup: {}", e))?;
    Ok(())
}

fn restore_backup_to_target(backup_path: &PathBuf, target_path: &PathBuf, label: &str) -> Result<(), String> {
    if target_path.exists() {
        fs::remove_file(target_path).map_err(|e| format!("Failed to remove existing {}: {}", label, e))?;
    }
    fs::rename(backup_path, target_path).map_err(|e| format!("Failed to restore {}: {}", label, e))
}

// ── Config manipulation helpers ────────────────────────────────────

fn ensure_object(value: &mut Value, key: &str) {
    let needs_reset = match value.get(key) {
        None => true,
        Some(v) if !v.is_object() => true,
        _ => false,
    };
    if needs_reset {
        value[key] = serde_json::json!({});
    }
}

fn ensure_provider_object(provider: &mut serde_json::Map<String, Value>, name: &str) {
    let needs_reset = match provider.get(name) {
        None => true,
        Some(v) if !v.is_object() => true,
        _ => false,
    };
    if needs_reset {
        provider.insert(name.to_string(), serde_json::json!({}));
    }
}

fn merge_provider_options(provider: &mut Value, base_url: &str, api_key: &str) {
    if provider.get("options").is_none() {
        provider["options"] = serde_json::json!({});
    }
    if let Some(options) = provider.get_mut("options").and_then(|o| o.as_object_mut()) {
        options.insert("baseURL".to_string(), Value::String(base_url.to_string()));
        options.insert("apiKey".to_string(), Value::String(api_key.to_string()));
    }
}

fn ensure_provider_string_field(provider: &mut Value, key: &str, value: &str) {
    if let Some(obj) = provider.as_object_mut() {
        obj.insert(key.to_string(), Value::String(value.to_string()));
    }
}

// ── Model catalog & variant builders ───────────────────────────────

fn build_claude_thinking_variant(budget: u32) -> Value {
    serde_json::json!({
        "thinkingConfig": { "thinkingBudget": budget },
        "thinking": { "type": "enabled", "budget_tokens": budget }
    })
}

fn build_gemini3_variant(level: &str) -> Value {
    serde_json::json!({ "thinkingLevel": level })
}

fn build_gemini25_thinking_variant(budget: u32) -> Value {
    serde_json::json!({
        "thinkingConfig": { "thinkingBudget": budget },
        "thinking": { "type": "enabled", "budget_tokens": budget }
    })
}

fn build_variants_object(variant_type: Option<VariantType>) -> Option<Value> {
    match variant_type {
        Some(VariantType::ClaudeThinking) => {
            let mut v = serde_json::Map::new();
            v.insert("low".into(), build_claude_thinking_variant(8192));
            v.insert("medium".into(), build_claude_thinking_variant(16384));
            v.insert("high".into(), build_claude_thinking_variant(24576));
            v.insert("max".into(), build_claude_thinking_variant(32768));
            Some(Value::Object(v))
        }
        Some(VariantType::Gemini3Pro) => {
            let mut v = serde_json::Map::new();
            v.insert("low".into(), build_gemini3_variant("low"));
            v.insert("high".into(), build_gemini3_variant("high"));
            Some(Value::Object(v))
        }
        Some(VariantType::Gemini3Flash) => {
            let mut v = serde_json::Map::new();
            v.insert("minimal".into(), build_gemini3_variant("minimal"));
            v.insert("low".into(), build_gemini3_variant("low"));
            v.insert("medium".into(), build_gemini3_variant("medium"));
            v.insert("high".into(), build_gemini3_variant("high"));
            Some(Value::Object(v))
        }
        Some(VariantType::Gemini25Thinking) => {
            let mut v = serde_json::Map::new();
            v.insert("low".into(), build_gemini25_thinking_variant(8192));
            v.insert("medium".into(), build_gemini25_thinking_variant(12288));
            v.insert("high".into(), build_gemini25_thinking_variant(16384));
            v.insert("max".into(), build_gemini25_thinking_variant(24576));
            Some(Value::Object(v))
        }
        None => None,
    }
}

fn build_model_json(model_def: &ModelDef) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("name".into(), Value::String(model_def.name.to_string()));
    obj.insert("limit".into(), serde_json::json!({ "context": model_def.context_limit, "output": model_def.output_limit }));
    obj.insert("modalities".into(), serde_json::json!({ "input": model_def.input_modalities, "output": model_def.output_modalities }));
    if model_def.reasoning {
        obj.insert("reasoning".into(), Value::Bool(true));
    }
    if let Some(variants) = build_variants_object(model_def.variant_type) {
        obj.insert("variants".into(), variants);
    }
    Value::Object(obj)
}

/// Merge catalog models into provider.models without deleting user models.
fn merge_catalog_models(provider: &mut Value, model_ids: Option<&[&str]>) {
    if provider.get("models").is_none() {
        provider["models"] = serde_json::json!({});
    }
    let catalog = build_model_catalog();
    let catalog_map: HashMap<&str, &ModelDef> = catalog.iter().map(|m| (m.id, m)).collect();

    if let Some(models) = provider.get_mut("models").and_then(|m| m.as_object_mut()) {
        let ids_to_sync: Vec<&str> = match model_ids {
            Some(ids) => ids.to_vec(),
            None => catalog_map.keys().copied().collect(),
        };
        for model_id in ids_to_sync {
            if let Some(model_def) = catalog_map.get(model_id) {
                let catalog_model = build_model_json(model_def);
                if let Some(existing) = models.get(model_id) {
                    if let Some(existing_obj) = existing.as_object() {
                        let mut merged = existing_obj.clone();
                        if let Some(catalog_obj) = catalog_model.as_object() {
                            for (key, value) in catalog_obj.iter() {
                                merged.insert(key.clone(), value.clone());
                            }
                        }
                        models.insert(model_id.to_string(), Value::Object(merged));
                    } else {
                        models.insert(model_id.to_string(), catalog_model);
                    }
                } else {
                    models.insert(model_id.to_string(), catalog_model);
                }
            }
        }
    }
}

/// Pure function: Apply sync logic to config JSON.
/// Returns the modified config Value.
pub fn apply_sync_to_config(
    mut config: Value,
    proxy_url: &str,
    api_key: &str,
    models_to_sync: Option<&[&str]>,
) -> Value {
    if !config.is_object() {
        config = serde_json::json!({});
    }
    if config.get("$schema").is_none() {
        config["$schema"] = Value::String("https://opencode.ai/config.json".to_string());
    }
    let normalized_url = normalize_opencode_base_url(proxy_url);
    ensure_object(&mut config, "provider");

    if let Some(provider) = config.get_mut("provider").and_then(|p| p.as_object_mut()) {
        ensure_provider_object(provider, ANTIGRAVITY_PROVIDER_ID);
        if let Some(ag_provider) = provider.get_mut(ANTIGRAVITY_PROVIDER_ID) {
            ensure_provider_string_field(ag_provider, "npm", "@ai-sdk/anthropic");
            ensure_provider_string_field(ag_provider, "name", "Antigravity Manager");
            merge_provider_options(ag_provider, &normalized_url, api_key);
            merge_catalog_models(ag_provider, models_to_sync);
        }
    }
    config
}

/// Pure function: Apply clear logic to config JSON.
pub fn apply_clear_to_config(
    mut config: Value,
    proxy_url: Option<&str>,
    clear_legacy: bool,
) -> Value {
    if let Some(provider) = config.get_mut("provider").and_then(|p| p.as_object_mut()) {
        provider.remove(ANTIGRAVITY_PROVIDER_ID);

        if clear_legacy {
            if let Some(proxy) = proxy_url {
                if let Some(anthropic) = provider.get_mut("anthropic") {
                    cleanup_legacy_provider(anthropic, proxy);
                }
                if let Some(google) = provider.get_mut("google") {
                    cleanup_legacy_provider(google, proxy);
                }
            }
        }

        if provider.is_empty() {
            if let Some(config_obj) = config.as_object_mut() {
                config_obj.remove("provider");
            }
        }
    }
    config
}

/// Sync OpenCode config to disk.
pub fn sync_opencode_config(
    proxy_url: &str,
    api_key: &str,
    models_to_sync: Option<Vec<String>>,
) -> Result<(), String> {
    let Some((config_path, _accounts_path)) = get_config_paths() else {
        return Err("Failed to get OpenCode config directory".to_string());
    };

    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    create_backup(&config_path)?;

    let mut config: Value = if config_path.exists() {
        fs::read_to_string(&config_path)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let model_refs: Option<Vec<&str>> = models_to_sync
        .as_ref()
        .map(|models| models.iter().map(|m| m.as_str()).collect());
    config = apply_sync_to_config(config, proxy_url, api_key, model_refs.as_deref());

    let tmp_path = config_path.with_extension("tmp");
    fs::write(&tmp_path, serde_json::to_string_pretty(&config).unwrap())
        .map_err(|e| format!("Failed to write temp file: {}", e))?;
    fs::rename(&tmp_path, &config_path)
        .map_err(|e| format!("Failed to rename config file: {}", e))?;

    Ok(())
}

/// Restore OpenCode config from backup.
pub fn restore_opencode_config() -> Result<(), String> {
    let Some((config_path, accounts_path)) = get_config_paths() else {
        return Err("Failed to get OpenCode config directory".to_string());
    };

    let mut restored = false;

    let config_backup_new = config_path.with_file_name(format!("{}{}", OPENCODE_CONFIG_FILE, BACKUP_SUFFIX));
    let config_backup_old = config_path.with_file_name(format!("{}{}", OPENCODE_CONFIG_FILE, OLD_BACKUP_SUFFIX));

    if config_backup_new.exists() {
        restore_backup_to_target(&config_backup_new, &config_path, "config")?;
        restored = true;
    } else if config_backup_old.exists() {
        restore_backup_to_target(&config_backup_old, &config_path, "config")?;
        restored = true;
    }

    let accounts_backup_new = accounts_path.with_file_name(format!("{}{}", ANTIGRAVITY_ACCOUNTS_FILE, BACKUP_SUFFIX));
    let accounts_backup_old = accounts_path.with_file_name(format!("{}{}", ANTIGRAVITY_ACCOUNTS_FILE, OLD_BACKUP_SUFFIX));

    if accounts_backup_new.exists() {
        restore_backup_to_target(&accounts_backup_new, &accounts_path, "accounts")?;
        restored = true;
    } else if accounts_backup_old.exists() {
        restore_backup_to_target(&accounts_backup_old, &accounts_path, "accounts")?;
        restored = true;
    }

    if restored {
        Ok(())
    } else {
        Err("No backup files found".to_string())
    }
}

/// Read OpenCode config file content.
pub fn read_opencode_config_content(file_name: Option<&str>) -> Result<String, String> {
    let Some((opencode_path, ag_accounts_path)) = get_config_paths() else {
        return Err("Failed to get OpenCode config directory".to_string());
    };

    let target_path = match file_name {
        Some(name) if name == ANTIGRAVITY_ACCOUNTS_FILE => ag_accounts_path,
        Some(name) if name == OPENCODE_CONFIG_FILE => opencode_path,
        Some(name) => return Err(format!("Invalid file name: {}", name)),
        None => opencode_path,
    };

    if !target_path.exists() {
        return Err(format!("Config file does not exist: {:?}", target_path));
    }
    fs::read_to_string(&target_path).map_err(|e| format!("Failed to read config: {}", e))
}

/// Get full OpenCode status.
pub fn get_opencode_status(proxy_url: &str) -> OpencodeStatus {
    let (installed, version) = check_opencode_installed();
    let (is_synced, has_backup, current_base_url) = if installed {
        get_sync_status(proxy_url)
    } else {
        (false, false, None)
    };

    OpencodeStatus {
        installed,
        version,
        is_synced,
        has_backup,
        current_base_url,
        files: vec![
            OPENCODE_CONFIG_FILE.to_string(),
            ANTIGRAVITY_ACCOUNTS_FILE.to_string(),
        ],
    }
}

// ── Legacy cleanup ─────────────────────────────────────────────────

/// List of Antigravity model IDs that may have been added to legacy providers.
const ANTIGRAVITY_MODEL_IDS: &[&str] = &[
    "claude-sonnet-4-5",
    "claude-sonnet-4-5-thinking",
    "claude-opus-4-5-thinking",
    "gemini-3-pro-high",
    "gemini-3-pro-low",
    "gemini-3-flash",
    "gemini-3-pro-image",
    "gemini-2.5-flash",
    "gemini-2.5-flash-lite",
    "gemini-2.5-flash-thinking",
    "gemini-2.5-pro",
];

/// Check if a base URL matches the proxy URL (supports both with and without /v1).
fn base_url_matches(config_url: &str, proxy_url: &str) -> bool {
    normalize_opencode_base_url(config_url) == normalize_opencode_base_url(proxy_url)
}

/// Cleanup legacy provider entries that were configured by old versions.
fn cleanup_legacy_provider(provider: &mut Value, proxy_url: &str) {
    if let Some(provider_obj) = provider.as_object_mut() {
        let remove_models_key = if let Some(models) = provider_obj.get_mut("models").and_then(|m| m.as_object_mut()) {
            for model_id in ANTIGRAVITY_MODEL_IDS {
                models.remove(*model_id);
            }
            models.is_empty()
        } else {
            false
        };
        if remove_models_key {
            provider_obj.remove("models");
        }

        let remove_options_key = if let Some(options) = provider_obj.get_mut("options").and_then(|o| o.as_object_mut()) {
            let should_cleanup = options
                .get("baseURL")
                .and_then(|v| v.as_str())
                .map(|base_url| base_url_matches(base_url, proxy_url))
                .unwrap_or(false);
            if should_cleanup {
                options.remove("baseURL");
                options.remove("apiKey");
                options.is_empty()
            } else {
                false
            }
        } else {
            false
        };
        if remove_options_key {
            provider_obj.remove("options");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_version_opencode_format() {
        assert_eq!(extract_version("opencode/1.2.3"), "1.2.3");
    }

    #[test]
    fn test_extract_version_codex_cli_format() {
        assert_eq!(extract_version("codex-cli 0.86.0\n"), "0.86.0");
    }

    #[test]
    fn test_extract_version_simple() {
        assert_eq!(extract_version("v2.0.1"), "2.0.1");
    }

    #[test]
    fn test_extract_version_unknown() {
        assert_eq!(extract_version("some random text"), "unknown");
    }

    #[test]
    fn test_normalize_opencode_base_url_without_v1() {
        assert_eq!(normalize_opencode_base_url("http://localhost:3000"), "http://localhost:3000/v1");
        assert_eq!(normalize_opencode_base_url("http://localhost:3000/"), "http://localhost:3000/v1");
    }

    #[test]
    fn test_normalize_opencode_base_url_with_v1() {
        assert_eq!(normalize_opencode_base_url("http://localhost:3000/v1"), "http://localhost:3000/v1");
        assert_eq!(normalize_opencode_base_url("http://localhost:3000/v1/"), "http://localhost:3000/v1");
    }

    #[test]
    fn test_normalize_opencode_base_url_with_whitespace() {
        assert_eq!(normalize_opencode_base_url("  http://localhost:3000  "), "http://localhost:3000/v1");
    }

    #[test]
    fn test_normalize_no_double_v1() {
        assert_eq!(normalize_opencode_base_url("http://localhost:3000/v1"), "http://localhost:3000/v1");
    }

    #[test]
    fn test_sync_preserves_existing_providers() {
        let config = serde_json::json!({
            "provider": {
                "google": { "options": { "apiKey": "google-key" } },
                "anthropic": { "options": { "apiKey": "anthropic-key" } }
            }
        });
        let result = apply_sync_to_config(config, "http://localhost:3000", "test-key", None);
        let provider = result.get("provider").unwrap();
        assert!(provider.get("google").is_some());
        assert!(provider.get("anthropic").is_some());
        assert_eq!(provider["google"]["options"]["apiKey"], "google-key");
    }

    #[test]
    fn test_sync_creates_antigravity_provider() {
        let config = serde_json::json!({});
        let result = apply_sync_to_config(config, "http://localhost:3000", "test-key", None);
        let ag = &result["provider"][ANTIGRAVITY_PROVIDER_ID];
        assert_eq!(ag["npm"], "@ai-sdk/anthropic");
        assert_eq!(ag["name"], "Antigravity Manager");
        assert_eq!(ag["options"]["baseURL"], "http://localhost:3000/v1");
        assert_eq!(ag["options"]["apiKey"], "test-key");
    }

    #[test]
    fn test_sync_creates_models() {
        let config = serde_json::json!({});
        let result = apply_sync_to_config(config, "http://localhost:3000", "key", None);
        let models = result["provider"][ANTIGRAVITY_PROVIDER_ID]["models"].as_object().unwrap();
        assert!(models.contains_key("claude-sonnet-4-5"));
        assert!(models.contains_key("gemini-3-pro-high"));
        assert!(models.contains_key("gemini-2.5-pro"));
    }

    #[test]
    fn test_sync_with_filtered_models() {
        let config = serde_json::json!({});
        let models = &["claude-sonnet-4-5", "gemini-3-pro-high"];
        let result = apply_sync_to_config(config, "http://localhost:3000", "key", Some(models));
        let m = result["provider"][ANTIGRAVITY_PROVIDER_ID]["models"].as_object().unwrap();
        assert!(m.contains_key("claude-sonnet-4-5"));
        assert!(m.contains_key("gemini-3-pro-high"));
        assert!(!m.contains_key("gemini-2.5-pro"));
    }

    #[test]
    fn test_clear_removes_antigravity_provider() {
        let config = serde_json::json!({
            "provider": {
                "antigravity-manager": { "options": { "baseURL": "http://localhost:3000/v1" } },
                "google": { "options": { "apiKey": "key" } }
            }
        });
        let result = apply_clear_to_config(config, None, false);
        let provider = result.get("provider").unwrap();
        assert!(provider.get(ANTIGRAVITY_PROVIDER_ID).is_none());
        assert!(provider.get("google").is_some());
    }

    #[test]
    fn test_clear_removes_empty_provider() {
        let config = serde_json::json!({
            "provider": {
                "antigravity-manager": { "options": { "baseURL": "http://localhost:3000/v1" } }
            }
        });
        let result = apply_clear_to_config(config, None, false);
        assert!(result.get("provider").is_none());
    }

    #[test]
    fn test_clear_legacy_removes_antigravity_models() {
        let config = serde_json::json!({
            "provider": {
                "anthropic": {
                    "options": { "baseURL": "http://localhost:3000/v1", "apiKey": "key" },
                    "models": { "claude-sonnet-4-5": { "name": "Claude" }, "claude-3": { "name": "Claude 3" } }
                }
            }
        });
        let result = apply_clear_to_config(config, Some("http://localhost:3000"), true);
        let models = result["provider"]["anthropic"]["models"].as_object().unwrap();
        assert!(!models.contains_key("claude-sonnet-4-5"));
        assert!(models.contains_key("claude-3"));
    }

    #[test]
    fn test_clear_legacy_preserves_options_when_url_different() {
        let config = serde_json::json!({
            "provider": {
                "anthropic": { "options": { "baseURL": "http://other.com/v1", "apiKey": "key" } }
            }
        });
        let result = apply_clear_to_config(config, Some("http://localhost:3000"), true);
        assert_eq!(result["provider"]["anthropic"]["options"]["baseURL"], "http://other.com/v1");
    }

    #[test]
    fn test_base_url_matches_with_v1() {
        assert!(base_url_matches("http://localhost:3000/v1", "http://localhost:3000"));
        assert!(base_url_matches("http://localhost:3000", "http://localhost:3000/v1"));
    }

    #[test]
    fn test_base_url_matches_different_urls() {
        assert!(!base_url_matches("http://localhost:3000", "http://other:3000"));
    }

    #[test]
    fn test_model_catalog_has_expected_entries() {
        let catalog = build_model_catalog();
        assert!(catalog.len() >= 11);
        assert!(catalog.iter().any(|m| m.id == "claude-sonnet-4-5"));
        assert!(catalog.iter().any(|m| m.id == "gemini-2.5-pro"));
    }

    #[test]
    fn test_build_model_json_structure() {
        let catalog = build_model_catalog();
        let model = catalog.iter().find(|m| m.id == "claude-sonnet-4-5").unwrap();
        let json = build_model_json(model);
        assert_eq!(json["name"], "Claude Sonnet 4.5");
        assert!(json.get("limit").is_some());
        assert!(json.get("modalities").is_some());
        // Non-reasoning model should not have reasoning field
        assert!(json.get("reasoning").is_none());
    }

    #[test]
    fn test_build_model_json_with_reasoning() {
        let catalog = build_model_catalog();
        let model = catalog.iter().find(|m| m.id == "claude-sonnet-4-5-thinking").unwrap();
        let json = build_model_json(model);
        assert_eq!(json["reasoning"], true);
        assert!(json.get("variants").is_some());
    }

    #[test]
    fn test_sync_merges_user_models() {
        let config = serde_json::json!({
            "provider": {
                "antigravity-manager": {
                    "models": {
                        "claude-sonnet-4-5": { "userField": "keep-me" }
                    }
                }
            }
        });
        let result = apply_sync_to_config(config, "http://localhost:3000", "key", Some(&["claude-sonnet-4-5"]));
        let model = &result["provider"][ANTIGRAVITY_PROVIDER_ID]["models"]["claude-sonnet-4-5"];
        // User field should be preserved
        assert_eq!(model["userField"], "keep-me");
        // Catalog fields should be merged
        assert_eq!(model["name"], "Claude Sonnet 4.5");
    }
}
