use std::fs;

use crate::models::AppConfig;
use super::account::get_data_dir;

const CONFIG_FILE: &str = "config.json";

/// Load application configuration from disk
pub fn load_app_config() -> Result<AppConfig, String> {
    let data_dir = get_data_dir()?;
    let config_path = data_dir.join(CONFIG_FILE);

    if !config_path.exists() {
        let config = AppConfig::new();
        // Persist initial config to prevent new API Key on every refresh
        let _ = save_app_config(&config);
        return Ok(config);
    }

    let content = fs::read_to_string(&config_path)
        .map_err(|e| format!("failed_to_read_config_file: {}", e))?;

    // Parse via Value first to support future migration logic
    let v: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("failed_to_parse_config_file: {}", e))?;

    let config: AppConfig = serde_json::from_value(v)
        .map_err(|e| format!("failed_to_convert_config: {}", e))?;

    Ok(config)
}

/// Save application configuration to disk
pub fn save_app_config(config: &AppConfig) -> Result<(), String> {
    let data_dir = get_data_dir()?;
    let config_path = data_dir.join(CONFIG_FILE);

    let content = serde_json::to_string_pretty(config)
        .map_err(|e| format!("failed_to_serialize_config: {}", e))?;

    fs::write(&config_path, content).map_err(|e| format!("failed_to_save_config: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TestDataDir {
        path: PathBuf,
    }

    impl TestDataDir {
        fn new() -> Self {
            let temp_path = std::env::temp_dir().join(format!(
                "kiro_config_test_{}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                uuid::Uuid::new_v4().simple()
            ));
            fs::create_dir_all(&temp_path).expect("Failed to create temp dir");
            Self { path: temp_path }
        }
    }

    impl Drop for TestDataDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn test_config_roundtrip() {
        let dir = TestDataDir::new();
        let config_path = dir.path.join(CONFIG_FILE);

        let config = AppConfig::new();
        let content = serde_json::to_string_pretty(&config).unwrap();
        fs::write(&config_path, &content).unwrap();

        let loaded_content = fs::read_to_string(&config_path).unwrap();
        let loaded: AppConfig = serde_json::from_str(&loaded_content).unwrap();

        assert_eq!(config.language, loaded.language);
        assert_eq!(config.theme, loaded.theme);
        assert_eq!(config.auto_refresh, loaded.auto_refresh);
        assert_eq!(config.refresh_interval, loaded.refresh_interval);
        assert_eq!(config.proxy.port, loaded.proxy.port);
    }

    #[test]
    fn test_config_missing_fields_use_defaults() {
        let dir = TestDataDir::new();
        let config_path = dir.path.join(CONFIG_FILE);

        // Minimal JSON with only required fields
        let minimal = r#"{
            "language": "en",
            "theme": "dark",
            "auto_refresh": false,
            "refresh_interval": 30
        }"#;
        fs::write(&config_path, minimal).unwrap();

        let loaded_content = fs::read_to_string(&config_path).unwrap();
        let loaded: AppConfig = serde_json::from_str(&loaded_content).unwrap();

        assert_eq!(loaded.language, "en");
        assert_eq!(loaded.theme, "dark");
        assert!(!loaded.auto_refresh);
        assert_eq!(loaded.refresh_interval, 30);
        // Defaults should be applied
        assert_eq!(loaded.proxy.port, 8045);
        assert!(!loaded.proxy.enabled);
        assert!(!loaded.auto_launch);
    }
}
