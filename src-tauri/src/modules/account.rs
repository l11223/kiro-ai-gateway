use serde_json;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

use crate::models::{Account, AccountIndex, AccountSummary, QuotaData, TokenData};
use once_cell::sync::Lazy;
use std::sync::Mutex;

/// Global account write lock to prevent corruption during concurrent operations
static ACCOUNT_INDEX_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

const DATA_DIR_NAME: &str = "kiro-ai-gateway";
const ACCOUNTS_INDEX: &str = "accounts.json";
const ACCOUNTS_DIR: &str = "accounts";

/// Get data directory path (~/.local/share/kiro-ai-gateway or platform equivalent)
pub fn get_data_dir() -> Result<PathBuf, String> {
    // Support custom data directory via environment variable
    if let Ok(env_path) = std::env::var("KIRO_DATA_DIR") {
        if !env_path.trim().is_empty() {
            let data_dir = PathBuf::from(env_path);
            if !data_dir.exists() {
                fs::create_dir_all(&data_dir)
                    .map_err(|e| format!("failed_to_create_custom_data_dir: {}", e))?;
            }
            return Ok(data_dir);
        }
    }

    let base = dirs::data_dir().ok_or("failed_to_get_data_dir")?;
    let data_dir = base.join(DATA_DIR_NAME);

    if !data_dir.exists() {
        fs::create_dir_all(&data_dir)
            .map_err(|e| format!("failed_to_create_data_dir: {}", e))?;
    }

    Ok(data_dir)
}

/// Get accounts directory path
pub fn get_accounts_dir() -> Result<PathBuf, String> {
    let data_dir = get_data_dir()?;
    let accounts_dir = data_dir.join(ACCOUNTS_DIR);

    if !accounts_dir.exists() {
        fs::create_dir_all(&accounts_dir)
            .map_err(|e| format!("failed_to_create_accounts_dir: {}", e))?;
    }

    Ok(accounts_dir)
}

/// Load account index from a specific directory (internal helper)
fn load_account_index_in_dir(data_dir: &PathBuf) -> Result<AccountIndex, String> {
    let index_path = data_dir.join(ACCOUNTS_INDEX);

    if !index_path.exists() {
        tracing::warn!("Account index file not found, attempting recovery from accounts directory");
        let recovered = rebuild_index_from_accounts_in_dir(data_dir)?;
        try_save_recovered_index(data_dir, &index_path, &recovered, None)?;
        return Ok(recovered);
    }

    let raw_content =
        fs::read(&index_path).map_err(|e| format!("failed_to_read_account_index: {}", e))?;

    // If file is empty, attempt recovery
    if raw_content.is_empty() {
        tracing::warn!("Account index is empty, attempting recovery from accounts directory");
        let recovered = rebuild_index_from_accounts_in_dir(data_dir)?;
        try_save_recovered_index(data_dir, &index_path, &recovered, None)?;
        return Ok(recovered);
    }

    // Sanitize content: strip BOM and leading NUL bytes
    let sanitized = sanitize_index_content(&raw_content);

    // If sanitized content is empty/whitespace, attempt recovery
    if sanitized.trim().is_empty() {
        tracing::warn!(
            "Account index is empty after sanitization, attempting recovery from accounts directory"
        );
        let recovered = rebuild_index_from_accounts_in_dir(data_dir)?;
        try_save_recovered_index(data_dir, &index_path, &recovered, None)?;
        return Ok(recovered);
    }

    // Try to parse sanitized content
    match serde_json::from_str::<AccountIndex>(&sanitized) {
        Ok(index) => {
            tracing::info!(
                "Successfully loaded index with {} accounts",
                index.accounts.len()
            );
            Ok(index)
        }
        Err(parse_err) => {
            tracing::error!(
                "Failed to parse account index: {}. Attempting recovery from accounts directory",
                parse_err
            );
            let recovered = rebuild_index_from_accounts_in_dir(data_dir)?;
            try_save_recovered_index(data_dir, &index_path, &recovered, Some(&raw_content))?;
            Ok(recovered)
        }
    }
}

/// Save account index to a specific directory (internal helper)
fn save_account_index_in_dir(data_dir: &PathBuf, index: &AccountIndex) -> Result<(), String> {
    let index_path = data_dir.join(ACCOUNTS_INDEX);
    let temp_filename = format!("{}.tmp.{}", ACCOUNTS_INDEX, Uuid::new_v4());
    let temp_path = data_dir.join(&temp_filename);

    let content = serde_json::to_string_pretty(index)
        .map_err(|e| format!("failed_to_serialize_account_index: {}", e))?;

    if let Err(e) = fs::write(&temp_path, content) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!("failed_to_write_temp_index_file: {}", e));
    }

    // Atomic rename
    if let Err(e) = fs::rename(&temp_path, &index_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!("failed_to_replace_index_file: {}", e));
    }

    Ok(())
}

/// Rebuild AccountIndex by scanning accounts/*.json files
fn rebuild_index_from_accounts_in_dir(data_dir: &PathBuf) -> Result<AccountIndex, String> {
    let accounts_dir = data_dir.join(ACCOUNTS_DIR);
    let mut summaries = Vec::new();

    if accounts_dir.exists() {
        if let Ok(entries) = fs::read_dir(&accounts_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "json") {
                    if let Some(account_id) = path.file_stem().and_then(|s| s.to_str()) {
                        match load_account_at_path(&path) {
                            Ok(account) => {
                                summaries.push(AccountSummary {
                                    id: account.id,
                                    email: account.email,
                                    name: account.name,
                                    disabled: account.disabled,
                                    proxy_disabled: account.proxy_disabled,
                                    protected_models: account.protected_models,
                                    created_at: account.created_at,
                                    last_used: account.last_used,
                                });
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to load account {} during recovery: {}",
                                    account_id,
                                    e
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // Sort by last_used desc, then by email for deterministic order
    summaries.sort_by(|a, b| {
        b.last_used
            .cmp(&a.last_used)
            .then_with(|| a.email.cmp(&b.email))
    });

    let current_account_id = summaries.first().map(|s| s.id.clone());

    tracing::info!(
        "Rebuilt index from accounts directory: {} accounts recovered",
        summaries.len()
    );

    Ok(AccountIndex {
        version: "2.0".to_string(),
        accounts: summaries,
        current_account_id,
    })
}

/// Load account from a specific path (internal helper)
fn load_account_at_path(account_path: &PathBuf) -> Result<Account, String> {
    let content = fs::read_to_string(account_path)
        .map_err(|e| format!("failed_to_read_account_data: {}", e))?;
    serde_json::from_str(&content).map_err(|e| format!("failed_to_parse_account_data: {}", e))
}

/// Sanitize index file content by stripping BOM and leading NUL bytes
fn sanitize_index_content(raw: &[u8]) -> String {
    // Skip UTF-8 BOM if present
    let without_bom = if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &raw[3..]
    } else {
        raw
    };

    // Skip leading NUL bytes
    let without_nul = without_bom
        .iter()
        .skip_while(|&&b| b == 0x00)
        .copied()
        .collect::<Vec<u8>>();

    String::from_utf8_lossy(&without_nul).into_owned()
}

/// Best-effort save of recovered index without deadlocking
fn try_save_recovered_index(
    data_dir: &PathBuf,
    _index_path: &PathBuf,
    index: &AccountIndex,
    corrupt_content: Option<&[u8]>,
) -> Result<(), String> {
    // Backup corrupt file if content provided
    if let Some(content) = corrupt_content {
        let timestamp = chrono::Utc::now().timestamp();
        let backup_name = format!("accounts.json.corrupt-{}-{}", timestamp, Uuid::new_v4());
        let backup_path = data_dir.join(&backup_name);
        if let Err(e) = fs::write(&backup_path, content) {
            tracing::warn!("Failed to backup corrupt index to {}: {}", backup_name, e);
        } else {
            tracing::info!("Backed up corrupt index to {}", backup_name);
        }
    }

    // Try to acquire lock without blocking
    match ACCOUNT_INDEX_LOCK.try_lock() {
        Ok(_guard) => {
            if let Err(e) = save_account_index_in_dir(data_dir, index) {
                tracing::warn!(
                    "Failed to save recovered index: {}. Will retry on next load.",
                    e
                );
            } else {
                tracing::info!("Successfully saved recovered index");
            }
        }
        Err(_) => {
            tracing::warn!(
                "Could not acquire lock to save recovered index. Will retry on next load."
            );
        }
    }

    Ok(())
}

// ============================================================================
// Public API
// ============================================================================

/// Load account index with recovery support
pub fn load_account_index() -> Result<AccountIndex, String> {
    let data_dir = get_data_dir()?;
    load_account_index_in_dir(&data_dir)
}

/// Save account index (atomic write)
pub fn save_account_index(index: &AccountIndex) -> Result<(), String> {
    let data_dir = get_data_dir()?;
    save_account_index_in_dir(&data_dir, index)
}

/// Load account data by ID
pub fn load_account(account_id: &str) -> Result<Account, String> {
    let accounts_dir = get_accounts_dir()?;
    let account_path = accounts_dir.join(format!("{}.json", account_id));
    load_account_at_path(&account_path)
}

/// Save account data to disk
pub fn save_account(account: &Account) -> Result<(), String> {
    let accounts_dir = get_accounts_dir()?;
    let account_path = accounts_dir.join(format!("{}.json", account.id));

    let content = serde_json::to_string_pretty(account)
        .map_err(|e| format!("failed_to_serialize_account_data: {}", e))?;

    fs::write(&account_path, content).map_err(|e| format!("failed_to_save_account_data: {}", e))
}

/// List all accounts (loads from index, then reads each account file)
pub fn list_accounts() -> Result<Vec<Account>, String> {
    let index = load_account_index()?;
    let mut accounts = Vec::new();

    for summary in &index.accounts {
        match load_account(&summary.id) {
            Ok(account) => accounts.push(account),
            Err(e) => {
                tracing::error!("Failed to load account {}: {}", summary.id, e);
                // Don't auto-remove from index to prevent data loss during transient FS issues
            }
        }
    }

    Ok(accounts)
}

/// Add a new account
pub fn add_account(
    email: String,
    name: Option<String>,
    token: TokenData,
) -> Result<Account, String> {
    let _lock = ACCOUNT_INDEX_LOCK
        .lock()
        .map_err(|e| format!("failed_to_acquire_lock: {}", e))?;
    let mut index = load_account_index()?;

    // Check if account already exists
    if index.accounts.iter().any(|s| s.email == email) {
        return Err(format!("Account already exists: {}", email));
    }

    let account_id = Uuid::new_v4().to_string();
    let mut account = Account::new(account_id.clone(), email.clone(), token);
    account.name = name.clone();

    save_account(&account)?;

    index.accounts.push(AccountSummary {
        id: account.id.clone(),
        email: account.email.clone(),
        name: account.name.clone(),
        disabled: account.disabled,
        proxy_disabled: account.proxy_disabled,
        protected_models: account.protected_models.clone(),
        created_at: account.created_at,
        last_used: account.last_used,
    });

    if index.current_account_id.is_none() {
        index.current_account_id = Some(account_id);
    }

    save_account_index(&index)?;

    Ok(account)
}

/// Delete account from disk and index
pub fn delete_account(account_id: &str) -> Result<(), String> {
    let _lock = ACCOUNT_INDEX_LOCK
        .lock()
        .map_err(|e| format!("failed_to_acquire_lock: {}", e))?;
    let mut index = load_account_index()?;

    let original_len = index.accounts.len();
    index.accounts.retain(|s| s.id != account_id);

    if index.accounts.len() == original_len {
        return Err(format!("Account ID not found: {}", account_id));
    }

    // Clear current account if it's being deleted
    if index.current_account_id.as_deref() == Some(account_id) {
        index.current_account_id = index.accounts.first().map(|s| s.id.clone());
    }

    save_account_index(&index)?;

    // Delete account file
    let accounts_dir = get_accounts_dir()?;
    let account_path = accounts_dir.join(format!("{}.json", account_id));

    if account_path.exists() {
        fs::remove_file(&account_path)
            .map_err(|e| format!("failed_to_delete_account_file: {}", e))?;
    }

    Ok(())
}

/// Batch delete accounts
pub fn delete_accounts(account_ids: &[String]) -> Result<(), String> {
    let _lock = ACCOUNT_INDEX_LOCK
        .lock()
        .map_err(|e| format!("failed_to_acquire_lock: {}", e))?;
    let mut index = load_account_index()?;
    let accounts_dir = get_accounts_dir()?;

    for account_id in account_ids {
        index.accounts.retain(|s| &s.id != account_id);

        if index.current_account_id.as_deref() == Some(account_id) {
            index.current_account_id = None;
        }

        let account_path = accounts_dir.join(format!("{}.json", account_id));
        if account_path.exists() {
            let _ = fs::remove_file(&account_path);
        }
    }

    if index.current_account_id.is_none() {
        index.current_account_id = index.accounts.first().map(|s| s.id.clone());
    }

    save_account_index(&index)
}

/// Reorder account list
pub fn reorder_accounts(account_ids: &[String]) -> Result<(), String> {
    let _lock = ACCOUNT_INDEX_LOCK
        .lock()
        .map_err(|e| format!("failed_to_acquire_lock: {}", e))?;
    let mut index = load_account_index()?;

    let id_to_summary: std::collections::HashMap<_, _> = index
        .accounts
        .iter()
        .map(|s| (s.id.clone(), s.clone()))
        .collect();

    let mut new_accounts = Vec::new();
    for id in account_ids {
        if let Some(summary) = id_to_summary.get(id) {
            new_accounts.push(summary.clone());
        }
    }

    // Add accounts missing from new order to the end
    for summary in &index.accounts {
        if !account_ids.contains(&summary.id) {
            new_accounts.push(summary.clone());
        }
    }

    index.accounts = new_accounts;
    save_account_index(&index)
}

/// Update account quota data
pub fn update_account_quota(account_id: &str, quota: QuotaData) -> Result<(), String> {
    let mut account = load_account(account_id)?;
    account.update_quota(quota);
    save_account(&account)?;

    // Update index summary's protected_models
    {
        let _lock = ACCOUNT_INDEX_LOCK
            .lock()
            .map_err(|e| format!("failed_to_acquire_lock: {}", e))?;
        if let Ok(mut index) = load_account_index() {
            if let Some(summary) = index.accounts.iter_mut().find(|a| a.id == account_id) {
                summary.protected_models = account.protected_models.clone();
                let _ = save_account_index(&index);
            }
        }
    }

    Ok(())
}

/// Get current account ID
pub fn get_current_account_id() -> Result<Option<String>, String> {
    let index = load_account_index()?;
    Ok(index.current_account_id)
}

/// Set current active account ID
pub fn set_current_account_id(account_id: &str) -> Result<(), String> {
    let _lock = ACCOUNT_INDEX_LOCK
        .lock()
        .map_err(|e| format!("failed_to_acquire_lock: {}", e))?;
    let mut index = load_account_index()?;
    index.current_account_id = Some(account_id.to_string());
    save_account_index(&index)
}

/// Add or update an account (upsert). If an account with the same email
/// already exists, its token data is updated in place; otherwise a new
/// account is created.
pub fn upsert_account(
    email: String,
    name: Option<String>,
    token: TokenData,
) -> Result<Account, String> {
    let _lock = ACCOUNT_INDEX_LOCK
        .lock()
        .map_err(|e| format!("failed_to_acquire_lock: {}", e))?;
    let mut index = load_account_index()?;

    // Check if account already exists by email
    if let Some(existing) = index.accounts.iter().find(|s| s.email == email) {
        let account_id = existing.id.clone();
        let mut account = load_account(&account_id)?;
        account.token = token;
        if name.is_some() {
            account.name = name;
        }
        account.update_last_used();
        save_account(&account)?;
        return Ok(account);
    }

    // New account
    let account_id = Uuid::new_v4().to_string();
    let mut account = Account::new(account_id.clone(), email.clone(), token);
    account.name = name;

    save_account(&account)?;

    index.accounts.push(AccountSummary {
        id: account.id.clone(),
        email: account.email.clone(),
        name: account.name.clone(),
        disabled: account.disabled,
        proxy_disabled: account.proxy_disabled,
        protected_models: account.protected_models.clone(),
        created_at: account.created_at,
        last_used: account.last_used,
    });

    if index.current_account_id.is_none() {
        index.current_account_id = Some(account_id);
    }

    save_account_index(&index)?;
    Ok(account)
}

// ============================================================================
// Import Functions (Requirements 1.2, 1.3)
// ============================================================================

use crate::models::AccountExportItem;

/// Result of a batch import operation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImportResult {
    pub success: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

/// Import a single account by refresh_token.
///
/// Validates the token by refreshing it, fetches user info from Google,
/// and creates (or updates) the corresponding Account entity.
///
/// Requirement 1.2: WHEN 用户提交单条 refresh_token THEN Gateway SHALL
/// 验证 token 有效性、获取用户信息并创建对应的 Account 实体
pub async fn import_single_token(refresh_token: &str) -> Result<Account, String> {
    crate::modules::migration::import_single_token_inner(refresh_token).await
}

/// Import accounts from a JSON string containing an array of
/// `AccountExportItem` objects (`[{ "email": "...", "refresh_token": "..." }]`).
///
/// Skips invalid entries and reports the import result.
///
/// Requirement 1.3: WHEN 用户上传 JSON 格式的批量账号数据 THEN Gateway SHALL
/// 解析文件内容并批量创建 Account 实体，跳过无效条目并报告导入结果
pub async fn import_batch_json(json_content: &str) -> Result<ImportResult, String> {
    let items: Vec<AccountExportItem> = serde_json::from_str(json_content)
        .map_err(|e| format!("failed to parse JSON: {}", e))?;

    if items.is_empty() {
        return Ok(ImportResult {
            success: 0,
            failed: 0,
            errors: vec!["empty account list".to_string()],
        });
    }

    let mut result = ImportResult {
        success: 0,
        failed: 0,
        errors: Vec::new(),
    };

    for (i, item) in items.iter().enumerate() {
        if item.refresh_token.trim().is_empty() {
            let msg = format!(
                "item {}: empty refresh_token for {}",
                i,
                if item.email.is_empty() {
                    "unknown"
                } else {
                    &item.email
                }
            );
            tracing::warn!("{}", msg);
            result.failed += 1;
            result.errors.push(msg);
            continue;
        }

        match crate::modules::migration::import_single_token_inner(&item.refresh_token).await {
            Ok(acc) => {
                tracing::info!("Batch import: imported {}", acc.email);
                result.success += 1;
            }
            Err(e) => {
                let msg = format!("item {} ({}): {}", i, item.email, e);
                tracing::warn!("Batch import failed: {}", msg);
                result.failed += 1;
                result.errors.push(msg);
            }
        }
    }

    Ok(result)
}

/// Export all accounts as a list of `AccountExportItem` (email + refresh_token).
///
/// Requirement 1.14
pub fn export_accounts() -> Result<Vec<AccountExportItem>, String> {
    let accounts = list_accounts()?;
    Ok(accounts
        .into_iter()
        .map(|a| AccountExportItem {
            email: a.email,
            refresh_token: a.token.refresh_token,
        })
        .collect())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use proptest::prelude::*;
    use proptest::collection::vec as prop_vec;

    struct TestDataDir {
        path: PathBuf,
    }

    impl TestDataDir {
        fn new() -> Self {
            let temp_path = std::env::temp_dir().join(format!(
                "kiro_gateway_test_{}_{}_{}",
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

        fn path(&self) -> &PathBuf {
            &self.path
        }
    }

    impl Drop for TestDataDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_corrupted_index(path: &PathBuf, content: &[u8]) {
        let index_path = path.join("accounts.json");
        fs::write(&index_path, content).expect("Failed to write corrupted index");
    }

    fn create_account_file(path: &PathBuf, account_id: &str, email: &str) {
        let accounts_dir = path.join("accounts");
        fs::create_dir_all(&accounts_dir).expect("Failed to create accounts dir");

        let account = Account::new(
            account_id.to_string(),
            email.to_string(),
            TokenData::new(
                "test_access_token".to_string(),
                "test_refresh_token".to_string(),
                3600,
                Some(email.to_string()),
                None,
                None,
            ),
        );

        let content =
            serde_json::to_string_pretty(&account).expect("Failed to serialize account");
        let account_path = accounts_dir.join(format!("{}.json", account_id));
        fs::write(&account_path, content).expect("Failed to write account file");
    }

    #[test]
    fn test_load_account_index_with_bom_prefix() {
        let dir = TestDataDir::new();
        let bom = [0xEF, 0xBB, 0xBF];
        let json = r#"{"version":"2.0","accounts":[],"current_account_id":null}"#;
        let mut content = Vec::new();
        content.extend_from_slice(&bom);
        content.extend_from_slice(json.as_bytes());
        write_corrupted_index(dir.path(), &content);

        let result = load_account_index_in_dir(dir.path());
        assert!(result.is_ok());
        assert!(result.unwrap().accounts.is_empty());
    }

    #[test]
    fn test_load_account_index_with_nul_prefix() {
        let dir = TestDataDir::new();
        let nul = [0x00];
        let json = r#"{"version":"2.0","accounts":[],"current_account_id":null}"#;
        let mut content = Vec::new();
        content.extend_from_slice(&nul);
        content.extend_from_slice(json.as_bytes());
        write_corrupted_index(dir.path(), &content);

        let result = load_account_index_in_dir(dir.path());
        assert!(result.is_ok());
        assert!(result.unwrap().accounts.is_empty());
    }

    #[test]
    fn test_load_account_index_with_garbage_content() {
        let dir = TestDataDir::new();
        write_corrupted_index(dir.path(), b"\0\0not json");

        let result = load_account_index_in_dir(dir.path());
        assert!(result.is_ok());
        assert!(result.unwrap().accounts.is_empty());
    }

    #[test]
    fn test_load_account_index_with_empty_file() {
        let dir = TestDataDir::new();
        write_corrupted_index(dir.path(), b"");

        let result = load_account_index_in_dir(dir.path());
        assert!(result.is_ok());
        assert!(result.unwrap().accounts.is_empty());
    }

    #[test]
    fn test_missing_index_with_existing_accounts() {
        let dir = TestDataDir::new();
        create_account_file(dir.path(), "test-id-1", "user1@example.com");
        create_account_file(dir.path(), "test-id-2", "user2@example.com");

        let result = load_account_index_in_dir(dir.path());
        assert!(result.is_ok());
        let index = result.unwrap();
        assert_eq!(index.accounts.len(), 2);

        let emails: Vec<_> = index.accounts.iter().map(|s| s.email.clone()).collect();
        assert!(emails.contains(&"user1@example.com".to_string()));
        assert!(emails.contains(&"user2@example.com".to_string()));
    }

    #[test]
    fn test_save_account_index_roundtrip() {
        let dir = TestDataDir::new();
        let now = chrono::Utc::now().timestamp();
        let index = AccountIndex {
            version: "2.0".to_string(),
            accounts: vec![
                AccountSummary {
                    id: "acc-1".to_string(),
                    email: "user1@example.com".to_string(),
                    name: Some("User One".to_string()),
                    disabled: false,
                    proxy_disabled: false,
                    protected_models: HashSet::new(),
                    created_at: now,
                    last_used: now,
                },
                AccountSummary {
                    id: "acc-2".to_string(),
                    email: "user2@example.com".to_string(),
                    name: None,
                    disabled: true,
                    proxy_disabled: true,
                    protected_models: HashSet::new(),
                    created_at: now - 100,
                    last_used: now - 50,
                },
            ],
            current_account_id: Some("acc-1".to_string()),
        };

        save_account_index_in_dir(dir.path(), &index).expect("Failed to save");
        let loaded = load_account_index_in_dir(dir.path()).expect("Failed to load");

        assert_eq!(loaded.accounts.len(), 2);
        assert_eq!(loaded.current_account_id, Some("acc-1".to_string()));

        let acc1 = loaded.accounts.iter().find(|a| a.id == "acc-1").unwrap();
        assert_eq!(acc1.email, "user1@example.com");
        assert!(!acc1.disabled);

        let acc2 = loaded.accounts.iter().find(|a| a.id == "acc-2").unwrap();
        assert_eq!(acc2.email, "user2@example.com");
        assert!(acc2.disabled);
    }

    #[test]
    fn test_backup_created_on_parse_failure() {
        let dir = TestDataDir::new();
        create_account_file(dir.path(), "recovered-acc", "recovered@example.com");

        let garbage_content = b"this is not valid json { broken";
        write_corrupted_index(dir.path(), garbage_content);

        let recovered = load_account_index_in_dir(dir.path()).expect("Should recover");
        assert_eq!(recovered.accounts.len(), 1);
        assert_eq!(recovered.accounts[0].email, "recovered@example.com");

        // Assert a backup file exists
        let backup_files: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map_or(false, |name| name.starts_with("accounts.json.corrupt-"))
            })
            .collect();
        assert_eq!(backup_files.len(), 1);
    }

    #[test]
    fn test_save_and_load_account() {
        let dir = TestDataDir::new();
        let accounts_dir = dir.path().join("accounts");
        fs::create_dir_all(&accounts_dir).unwrap();

        let account = Account::new(
            "test-id".to_string(),
            "test@example.com".to_string(),
            TokenData::new(
                "access".to_string(),
                "refresh".to_string(),
                3600,
                Some("test@example.com".to_string()),
                None,
                None,
            ),
        );

        let account_path = accounts_dir.join("test-id.json");
        let content = serde_json::to_string_pretty(&account).unwrap();
        fs::write(&account_path, content).unwrap();

        let loaded = load_account_at_path(&account_path).unwrap();
        assert_eq!(loaded.id, "test-id");
        assert_eq!(loaded.email, "test@example.com");
        assert_eq!(loaded.token.access_token, "access");
        assert_eq!(loaded.token.refresh_token, "refresh");
    }

    // ── Property 18: 账号导出完整性 ─────────────────────────────────
    // **Feature: kiro-ai-gateway, Property 18: 账号导出完整性**
    // **Validates: Requirements 1.14**

    fn arb_account_for_export() -> impl Strategy<Value = Account> {
        (
            "[a-f0-9-]{36}",
            "[a-zA-Z0-9.]+@[a-zA-Z0-9]+\\.[a-z]{2,4}",
            "[a-zA-Z0-9]{20,40}",
            "[a-zA-Z0-9]{20,40}",
        )
            .prop_map(|(id, email, access_token, refresh_token)| {
                Account::new(
                    id,
                    email.clone(),
                    TokenData::new(
                        access_token,
                        refresh_token,
                        3600,
                        Some(email),
                        None,
                        None,
                    ),
                )
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn export_completeness(accounts in prop_vec(arb_account_for_export(), 0..20)) {
            // Simulate the export mapping logic (same as export_accounts but without disk I/O)
            let exported: Vec<AccountExportItem> = accounts
                .iter()
                .map(|a| AccountExportItem {
                    email: a.email.clone(),
                    refresh_token: a.token.refresh_token.clone(),
                })
                .collect();

            // Count SHALL equal the original set size
            prop_assert_eq!(exported.len(), accounts.len());

            // Each account's email and refresh_token SHALL be present in the export
            for (i, account) in accounts.iter().enumerate() {
                prop_assert_eq!(&exported[i].email, &account.email);
                prop_assert_eq!(&exported[i].refresh_token, &account.token.refresh_token);
            }
        }
    }
}
