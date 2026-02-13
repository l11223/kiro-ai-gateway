//! Old database migration module.
//!
//! Reads refresh tokens from legacy Antigravity `state.vscdb` SQLite databases
//! (both old and new internal formats) and imports them as Gateway accounts.

use std::path::PathBuf;

use base64::{engine::general_purpose, Engine as _};
use tracing::{info, warn};

use crate::models::{Account, TokenData};
use crate::modules::{account, oauth};
use crate::utils::protobuf;

/// Result of a migration operation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MigrationResult {
    pub success: usize,
    pub failed: usize,
    pub errors: Vec<String>,
    pub accounts: Vec<Account>,
}

/// Get the default Antigravity database path for the current platform.
pub fn get_default_db_path() -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().ok_or("failed to get home directory")?;
        Ok(home.join("Library/Application Support/Antigravity/User/globalStorage/state.vscdb"))
    }

    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA")
            .map_err(|_| "failed to get APPDATA environment variable".to_string())?;
        Ok(PathBuf::from(appdata).join("Antigravity\\User\\globalStorage\\state.vscdb"))
    }

    #[cfg(target_os = "linux")]
    {
        let home = dirs::home_dir().ok_or("failed to get home directory")?;
        Ok(home.join(".config/Antigravity/User/globalStorage/state.vscdb"))
    }
}

/// Extract a refresh token from a legacy database file.
///
/// Supports two internal formats:
/// - New (>= 1.16.5): key `antigravityUnifiedStateSync.oauthToken`
/// - Old (< 1.16.5): key `jetskiStateSync.agentManagerInitState`
pub fn extract_refresh_token_from_db(db_path: &PathBuf) -> Result<String, String> {
    if !db_path.exists() {
        return Err(format!("database file not found: {:?}", db_path));
    }

    let conn = rusqlite::Connection::open(db_path)
        .map_err(|e| format!("failed to open database: {}", e))?;

    // --- Try new format first ---
    let new_format_data: Option<String> = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key = ?",
            ["antigravityUnifiedStateSync.oauthToken"],
            |row| row.get(0),
        )
        .ok();

    if let Some(outer_b64) = new_format_data {
        info!("Detected new-format database (antigravityUnifiedStateSync.oauthToken)");

        let outer_blob = general_purpose::STANDARD
            .decode(&outer_b64)
            .map_err(|e| format!("outer base64 decode failed: {}", e))?;

        // Outer(Field 1) → Inner1
        let inner1 = protobuf::find_field(&outer_blob, 1)
            .map_err(|e| format!("parse outer field 1 failed: {}", e))?
            .ok_or("outer field 1 not found")?;

        // Inner1(Field 2) → Inner2
        let inner2 = protobuf::find_field(&inner1, 2)
            .map_err(|e| format!("parse inner1 field 2 failed: {}", e))?
            .ok_or("inner1 field 2 not found")?;

        // Inner2(Field 1) → base64-encoded OAuthInfo
        let oauth_info_bytes = protobuf::find_field(&inner2, 1)
            .map_err(|e| format!("parse inner2 field 1 failed: {}", e))?
            .ok_or("inner2 field 1 not found")?;

        let oauth_info_b64 =
            String::from_utf8(oauth_info_bytes).map_err(|_| "oauth info b64 not utf-8")?;

        let oauth_info_blob = general_purpose::STANDARD
            .decode(&oauth_info_b64)
            .map_err(|e| format!("inner base64 decode failed: {}", e))?;

        // OAuthInfo(Field 3) → refresh_token
        let refresh_bytes = protobuf::find_field(&oauth_info_blob, 3)
            .map_err(|e| format!("parse oauth info field 3 failed: {}", e))?
            .ok_or("refresh token not found in oauth info")?;

        return String::from_utf8(refresh_bytes)
            .map_err(|_| "refresh token is not utf-8".to_string());
    }

    // --- Fallback to old format ---
    info!("Trying old-format database (jetskiStateSync.agentManagerInitState)");

    let current_data: String = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key = ?",
            ["jetskiStateSync.agentManagerInitState"],
            |row| row.get(0),
        )
        .map_err(|_| "login state data not found in either format".to_string())?;

    let blob = general_purpose::STANDARD
        .decode(&current_data)
        .map_err(|e| format!("base64 decode failed: {}", e))?;

    // Field 6 → OAuthTokenInfo
    let oauth_data = protobuf::find_field(&blob, 6)
        .map_err(|e| format!("protobuf parse failed: {}", e))?
        .ok_or("oauth data not found (field 6)")?;

    // Field 3 → refresh_token
    let refresh_bytes = protobuf::find_field(&oauth_data, 3)
        .map_err(|e| format!("oauth data parse failed: {}", e))?
        .ok_or("refresh token not found (field 3)")?;

    String::from_utf8(refresh_bytes).map_err(|_| "refresh token is not utf-8".to_string())
}

/// Import accounts from a legacy database file.
///
/// Extracts the refresh token, validates it by refreshing, fetches user info,
/// and creates a new Account.
pub async fn import_from_db(db_path: &PathBuf) -> Result<MigrationResult, String> {
    let refresh_token = extract_refresh_token_from_db(db_path)?;

    info!("Extracted refresh token from legacy database, importing...");

    match import_single_token_inner(&refresh_token).await {
        Ok(acc) => Ok(MigrationResult {
            success: 1,
            failed: 0,
            errors: Vec::new(),
            accounts: vec![acc],
        }),
        Err(e) => Ok(MigrationResult {
            success: 0,
            failed: 1,
            errors: vec![e],
            accounts: Vec::new(),
        }),
    }
}

/// Import from the default database path.
pub async fn import_from_default_db() -> Result<MigrationResult, String> {
    let db_path = get_default_db_path()?;
    import_from_db(&db_path).await
}

// ---------------------------------------------------------------------------
// Internal helper shared with account_service import functions
// ---------------------------------------------------------------------------

/// Core logic: validate a refresh token, get user info, persist account.
pub(crate) async fn import_single_token_inner(refresh_token: &str) -> Result<Account, String> {
    let trimmed = refresh_token.trim();
    if trimmed.is_empty() {
        return Err("refresh_token is empty".to_string());
    }

    // 1. Refresh to validate & get access token
    let token_resp = oauth::refresh_access_token(trimmed).await?;

    // 2. Get user info
    let (email, name) = oauth::get_user_info(&token_resp.access_token).await?;

    // 3. Build TokenData
    let token_data = TokenData::new(
        token_resp.access_token,
        trimmed.to_string(),
        token_resp.expires_in,
        Some(email.clone()),
        None, // project_id fetched on demand
        None, // session_id generated by token_manager
    );

    // 4. Persist
    let acc = account::add_account(email.clone(), name, token_data)?;
    info!("Imported account: {}", email);
    Ok(acc)
}

/// Scan the V1 data directory (`~/.antigravity-agent/`) and import all
/// accounts found in the legacy index files.
pub async fn import_from_v1() -> Result<MigrationResult, String> {
    let home = dirs::home_dir().ok_or("failed to get home directory")?;
    let v1_dir = home.join(".antigravity-agent");

    let mut result = MigrationResult {
        success: 0,
        failed: 0,
        errors: Vec::new(),
        accounts: Vec::new(),
    };

    let index_files = ["antigravity_accounts.json", "accounts.json"];
    let mut found_index = false;

    for index_filename in &index_files {
        let index_path = v1_dir.join(index_filename);
        if !index_path.exists() {
            continue;
        }
        found_index = true;
        info!("V1 data discovered: {:?}", index_path);

        let content = match std::fs::read_to_string(&index_path) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to read V1 index {}: {}", index_filename, e);
                continue;
            }
        };

        let v1_index: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                warn!("Failed to parse V1 index JSON: {}", e);
                continue;
            }
        };

        // Compatible with two formats: direct map or { "accounts": { ... } }
        let accounts_map = if let Some(map) = v1_index.as_object() {
            if let Some(accounts) = map.get("accounts").and_then(|v| v.as_object()) {
                accounts.clone()
            } else {
                map.clone()
            }
        } else {
            continue;
        };

        for (id, acc_info) in &accounts_map {
            if !acc_info.is_object() {
                continue;
            }

            let email_placeholder = acc_info
                .get("email")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown")
                .to_string();

            // Try to extract refresh token from backup/data file
            let refresh_token = extract_v1_refresh_token(&v1_dir, acc_info);

            match refresh_token {
                Some(rt) => match import_single_token_inner(&rt).await {
                    Ok(acc) => {
                        result.success += 1;
                        result.accounts.push(acc);
                    }
                    Err(e) => {
                        let msg = format!("import failed for {} ({}): {}", id, email_placeholder, e);
                        warn!("{}", msg);
                        result.failed += 1;
                        result.errors.push(msg);
                    }
                },
                None => {
                    let msg = format!(
                        "no refresh token found for {} ({})",
                        id, email_placeholder
                    );
                    warn!("{}", msg);
                    result.failed += 1;
                    result.errors.push(msg);
                }
            }
        }
    }

    if !found_index {
        return Err("V1 account data directory not found".to_string());
    }

    Ok(result)
}

/// Try to extract a refresh token from a V1 account entry's backup/data file.
fn extract_v1_refresh_token(
    v1_dir: &PathBuf,
    acc_info: &serde_json::Value,
) -> Option<String> {
    let backup_file = acc_info
        .get("backup_file")
        .and_then(|v| v.as_str())
        .or_else(|| acc_info.get("data_file").and_then(|v| v.as_str()))?;

    let mut path = PathBuf::from(backup_file);

    // Resolve relative paths
    if !path.exists() {
        if let Some(fname) = PathBuf::from(backup_file).file_name() {
            path = v1_dir.join(fname);
        }
    }
    if !path.exists() {
        if let Some(fname) = PathBuf::from(backup_file).file_name() {
            let try_backups = v1_dir.join("backups").join(fname);
            if try_backups.exists() {
                path = try_backups;
            } else {
                let try_accounts = v1_dir.join("accounts").join(fname);
                if try_accounts.exists() {
                    path = try_accounts;
                }
            }
        }
    }

    if !path.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    // Format 1: JSON with "token.refresh_token"
    if let Some(rt) = json
        .get("token")
        .and_then(|t| t.get("refresh_token"))
        .and_then(|v| v.as_str())
    {
        return Some(rt.to_string());
    }

    // Format 2: Protobuf blob in "jetskiStateSync.agentManagerInitState"
    if let Some(state_b64) = json
        .get("jetskiStateSync.agentManagerInitState")
        .and_then(|v| v.as_str())
    {
        if let Ok(blob) = general_purpose::STANDARD.decode(state_b64) {
            if let Ok(Some(oauth_data)) = protobuf::find_field(&blob, 6) {
                if let Ok(Some(refresh_bytes)) = protobuf::find_field(&oauth_data, 3) {
                    if let Ok(rt) = String::from_utf8(refresh_bytes) {
                        return Some(rt);
                    }
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_get_default_db_path_returns_path() {
        // Just verify it doesn't panic and returns a path
        let result = get_default_db_path();
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.to_string_lossy().contains("Antigravity"));
    }

    #[test]
    fn test_extract_refresh_token_missing_file() {
        let path = PathBuf::from("/nonexistent/path/state.vscdb");
        let result = extract_refresh_token_from_db(&path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_extract_v1_refresh_token_json_format() {
        let dir = std::env::temp_dir().join(format!(
            "kiro_migration_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        fs::create_dir_all(&dir).unwrap();

        // Create a backup file with JSON token format
        let backup_content = serde_json::json!({
            "token": {
                "access_token": "ya29.test",
                "refresh_token": "1//test_refresh_token",
                "expires_in": 3600
            }
        });
        let backup_path = dir.join("backup.json");
        fs::write(&backup_path, backup_content.to_string()).unwrap();

        let acc_info = serde_json::json!({
            "email": "test@example.com",
            "backup_file": backup_path.to_string_lossy().to_string()
        });

        let result = extract_v1_refresh_token(&dir, &acc_info);
        assert_eq!(result, Some("1//test_refresh_token".to_string()));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_extract_v1_refresh_token_missing_file() {
        let dir = PathBuf::from("/nonexistent");
        let acc_info = serde_json::json!({
            "email": "test@example.com",
            "backup_file": "/nonexistent/backup.json"
        });

        let result = extract_v1_refresh_token(&dir, &acc_info);
        assert_eq!(result, None);
    }
}
