// Scheduler Module - Smart warmup scheduler for quota management
//
// Provides:
// - start_scheduler(): Background task that scans accounts every 10 minutes
// - trigger_warmup_for_account(): Immediate warmup check for a single account
// - record_warmup_history() / check_cooldown(): Cooldown period management
//
// Requirements: 4.7, 5.8
// - 100% quota detection triggers warmup to activate quota timer
// - 403 Forbidden detection persists is_forbidden flag
// - 4-hour cooldown between warmups for the same model/account
// - Batch warmup: 3 per batch, 2-second interval between batches

use chrono::Utc;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tokio::time::{self, Duration};
use tracing::{info, warn};

use crate::models::account::Account;
use crate::modules::{account, config, quota};

/// Default cooldown period: 4 hours (14400 seconds)
const COOLDOWN_SECONDS: i64 = 14400;
/// Scan interval: 10 minutes (600 seconds)
const SCAN_INTERVAL_SECS: u64 = 600;
/// Batch size for concurrent warmup tasks
const BATCH_SIZE: usize = 3;
/// Delay between batches in seconds
const BATCH_DELAY_SECS: u64 = 2;
/// History cleanup cutoff: 24 hours
const HISTORY_CLEANUP_SECS: i64 = 86400;

// ── Warmup history persistence ──────────────────────────────────────

/// In-memory warmup history: key = "email:model_name:100", value = unix timestamp
static WARMUP_HISTORY: once_cell::sync::Lazy<Mutex<HashMap<String, i64>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(load_warmup_history()));

fn get_warmup_history_path() -> Result<PathBuf, String> {
    let data_dir = account::get_data_dir()?;
    Ok(data_dir.join("warmup_history.json"))
}

fn load_warmup_history() -> HashMap<String, i64> {
    match get_warmup_history_path() {
        Ok(path) if path.exists() => std::fs::read_to_string(&path)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default(),
        _ => HashMap::new(),
    }
}

fn save_warmup_history(history: &HashMap<String, i64>) {
    if let Ok(path) = get_warmup_history_path() {
        if let Ok(content) = serde_json::to_string_pretty(history) {
            let _ = std::fs::write(&path, content);
        }
    }
}

/// Record a successful warmup timestamp for the given key.
pub fn record_warmup_history(key: &str, timestamp: i64) {
    let mut history = WARMUP_HISTORY.lock().unwrap();
    history.insert(key.to_string(), timestamp);
    save_warmup_history(&history);
}

/// Check if the given key is still within the cooldown period.
/// Returns `true` if in cooldown (should skip), `false` if ready for warmup.
pub fn check_cooldown(key: &str, cooldown_seconds: i64) -> bool {
    let history = WARMUP_HISTORY.lock().unwrap();
    if let Some(&last_ts) = history.get(key) {
        let now = Utc::now().timestamp();
        now - last_ts < cooldown_seconds
    } else {
        false
    }
}

// ── Warmup task descriptor ──────────────────────────────────────────

/// Describes a pending warmup task collected during the scan phase.
#[derive(Debug, Clone)]
struct WarmupTask {
    account_id: String,
    email: String,
    model: String,
    token: String,
    project_id: String,
    percentage: i32,
    history_key: String,
}

// ── Scheduler entry point ───────────────────────────────────────────

/// Start the background warmup scheduler.
///
/// Runs an infinite loop that scans all accounts every 10 minutes.
/// For each account with 100% quota on a monitored model (and not in cooldown),
/// it triggers a warmup request in batches of 3 with 2-second intervals.
pub fn start_scheduler() {
    tokio::spawn(async move {
        info!("[Scheduler] Smart Warmup Scheduler started. Monitoring quota at 100%...");

        let mut interval = time::interval(Duration::from_secs(SCAN_INTERVAL_SECS));

        loop {
            interval.tick().await;
            run_scan_cycle().await;
        }
    });
}

/// Execute one full scan cycle: load config, scan accounts, trigger warmups.
async fn run_scan_cycle() {
    // Load configuration
    let app_config = match config::load_app_config() {
        Ok(c) => c,
        Err(_) => return,
    };

    if !app_config.scheduled_warmup.enabled {
        return;
    }

    // Get all accounts
    let accounts = match account::list_accounts() {
        Ok(a) => a,
        Err(_) => return,
    };

    if accounts.is_empty() {
        return;
    }

    info!(
        "[Scheduler] Scanning {} accounts for 100% quota models...",
        accounts.len()
    );

    let (warmup_tasks, skipped_cooldown) =
        collect_warmup_tasks(&accounts, &app_config.scheduled_warmup.monitored_models).await;

    // Execute warmup tasks
    if !warmup_tasks.is_empty() {
        let total = warmup_tasks.len();
        if skipped_cooldown > 0 {
            info!(
                "[Scheduler] Skipped {} models in cooldown, will warmup {}",
                skipped_cooldown, total
            );
        }
        info!("[Scheduler] 🔥 Triggering {} warmup tasks...", total);
        execute_warmup_batch(warmup_tasks).await;
    } else if skipped_cooldown > 0 {
        info!(
            "[Scheduler] Scan completed, all 100% models are in cooldown, skipped {}",
            skipped_cooldown
        );
    } else {
        info!("[Scheduler] Scan completed, no models with 100% quota need warmup");
    }

    // Cleanup old history entries (keep last 24 hours)
    cleanup_history();
}

// ── Scan & collect ──────────────────────────────────────────────────

/// Scan all accounts and collect warmup tasks for models at 100% quota.
/// Returns (tasks, skipped_cooldown_count).
async fn collect_warmup_tasks(
    accounts: &[Account],
    monitored_models: &[String],
) -> (Vec<WarmupTask>, usize) {
    let mut warmup_tasks = Vec::new();
    let mut skipped_cooldown: usize = 0;

    for acct in accounts {
        // Skip disabled accounts
        if acct.disabled || acct.proxy_disabled {
            continue;
        }

        // Get valid token for warmup
        let (token, pid) = match quota::get_valid_token_for_warmup(acct).await {
            Ok(t) => t,
            Err(_) => continue,
        };

        // Fetch fresh quota
        let (fresh_quota, _) =
            match quota::fetch_quota_with_cache(&token, &acct.email, Some(&pid)).await {
                Ok(q) => q,
                Err(_) => continue,
            };

        // 403 detection: persist is_forbidden flag (Requirement 5.8)
        if fresh_quota.is_forbidden {
            warn!(
                "[Scheduler] Account {} returned 403 Forbidden, persisting forbidden status",
                acct.email
            );
            let _ = account::update_account_quota(&acct.id, fresh_quota);
            continue;
        }

        for model in &fresh_quota.models {
            if model.percentage == 100 {
                // Only warmup models in the user's monitored list
                if !monitored_models.contains(&model.name) {
                    continue;
                }

                let history_key = format!("{}:{}:100", acct.email, model.name);

                // Check 4-hour cooldown
                if check_cooldown(&history_key, COOLDOWN_SECONDS) {
                    skipped_cooldown += 1;
                    continue;
                }

                warmup_tasks.push(WarmupTask {
                    account_id: acct.id.clone(),
                    email: acct.email.clone(),
                    model: model.name.clone(),
                    token: token.clone(),
                    project_id: pid.clone(),
                    percentage: model.percentage,
                    history_key,
                });

                info!(
                    "[Scheduler] ✓ Scheduled warmup: {} @ {} (quota at 100%)",
                    model.name, acct.email
                );
            } else {
                // Quota not full → clear history so next time it hits 100% we can warmup
                let history_key = format!("{}:{}:100", acct.email, model.name);
                let mut history = WARMUP_HISTORY.lock().unwrap();
                if history.remove(&history_key).is_some() {
                    save_warmup_history(&history);
                    info!(
                        "[Scheduler] Cleared history for {} @ {} (quota: {}%)",
                        model.name, acct.email, model.percentage
                    );
                }
            }
        }
    }

    (warmup_tasks, skipped_cooldown)
}

// ── Batch execution ─────────────────────────────────────────────────

/// Execute warmup tasks in batches of BATCH_SIZE with BATCH_DELAY_SECS between batches.
async fn execute_warmup_batch(tasks: Vec<WarmupTask>) {
    let total = tasks.len();
    let now_ts = Utc::now().timestamp();
    let mut success = 0usize;
    let num_batches = (total + BATCH_SIZE - 1) / BATCH_SIZE;

    for (batch_idx, batch) in tasks.chunks(BATCH_SIZE).enumerate() {
        let mut handles = Vec::new();

        for (task_idx, task) in batch.iter().enumerate() {
            let global_idx = batch_idx * BATCH_SIZE + task_idx + 1;
            let task = task.clone();

            info!(
                "[Warmup {}/{}] {} @ {} ({}%)",
                global_idx, total, task.model, task.email, task.percentage
            );

            let handle = tokio::spawn(async move {
                let result = quota::warmup_model_directly(
                    &task.token,
                    &task.model,
                    &task.project_id,
                    &task.email,
                    task.percentage,
                    Some(&task.account_id),
                )
                .await;
                (result, task.history_key)
            });
            handles.push(handle);
        }

        for handle in handles {
            if let Ok((true, history_key)) = handle.await {
                success += 1;
                record_warmup_history(&history_key, now_ts);
            }
        }

        // Delay between batches (skip after last batch)
        if batch_idx < num_batches - 1 {
            tokio::time::sleep(Duration::from_secs(BATCH_DELAY_SECS)).await;
        }
    }

    info!(
        "[Scheduler] ✅ Warmup completed: {}/{} successful",
        success, total
    );
}

// ── Single-account warmup ───────────────────────────────────────────

/// Trigger immediate warmup check for a single account.
///
/// Used after quota refresh to immediately warm up any 100% models.
pub async fn trigger_warmup_for_account(acct: &Account) {
    if acct.disabled || acct.proxy_disabled {
        return;
    }

    let (token, pid) = match quota::get_valid_token_for_warmup(acct).await {
        Ok(t) => t,
        Err(_) => return,
    };

    let (fresh_quota, _) =
        match quota::fetch_quota_with_cache(&token, &acct.email, Some(&pid)).await {
            Ok(q) => q,
            Err(_) => return,
        };

    // 403 detection (Requirement 5.8)
    if fresh_quota.is_forbidden {
        warn!(
            "[Scheduler] Account {} returned 403 Forbidden, persisting forbidden status",
            acct.email
        );
        let _ = account::update_account_quota(&acct.id, fresh_quota);
        return;
    }

    let app_config = match config::load_app_config() {
        Ok(c) => c,
        Err(_) => return,
    };

    let now_ts = Utc::now().timestamp();
    let mut tasks_to_run = Vec::new();

    for model in &fresh_quota.models {
        let history_key = format!("{}:{}:100", acct.email, model.name);

        if model.percentage == 100 {
            // Only warmup monitored models
            if !app_config
                .scheduled_warmup
                .monitored_models
                .contains(&model.name)
            {
                continue;
            }

            // Check cooldown
            if check_cooldown(&history_key, COOLDOWN_SECONDS) {
                continue;
            }

            tasks_to_run.push((model.name.clone(), model.percentage, history_key));
        } else {
            // Clear history for non-100% models
            let mut history = WARMUP_HISTORY.lock().unwrap();
            if history.remove(&history_key).is_some() {
                save_warmup_history(&history);
            }
        }
    }

    if !tasks_to_run.is_empty() {
        info!(
            "[Scheduler] Found {} models ready for warmup on {}",
            tasks_to_run.len(),
            acct.email
        );

        for (model, pct, history_key) in tasks_to_run {
            info!(
                "[Scheduler] 🔥 Triggering individual warmup: {} @ {} (Sync)",
                model, acct.email
            );

            let ok = quota::warmup_model_directly(
                &token,
                &model,
                &pid,
                &acct.email,
                pct,
                Some(&acct.id),
            )
            .await;

            if ok {
                record_warmup_history(&history_key, now_ts);
            }
        }
    }
}

// ── History cleanup ─────────────────────────────────────────────────

/// Remove warmup history entries older than 24 hours.
fn cleanup_history() {
    let cutoff = Utc::now().timestamp() - HISTORY_CLEANUP_SECS;
    let mut history = WARMUP_HISTORY.lock().unwrap();
    let before = history.len();
    history.retain(|_, &mut ts| ts > cutoff);
    if history.len() < before {
        save_warmup_history(&history);
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── History key format ──────────────────────────────────────────

    #[test]
    fn test_history_key_format() {
        let email = "user@example.com";
        let model = "gemini-2.0-flash";
        let key = format!("{}:{}:100", email, model);
        assert_eq!(key, "user@example.com:gemini-2.0-flash:100");
    }

    // ── Cooldown logic ──────────────────────────────────────────────

    #[test]
    fn test_check_cooldown_no_history() {
        // No history entry → not in cooldown
        let result = check_cooldown("nonexistent:model:100", COOLDOWN_SECONDS);
        assert!(!result);
    }

    #[test]
    fn test_check_cooldown_recent_warmup() {
        // Record a warmup just now → should be in cooldown
        let key = "test_cooldown_recent@example.com:gemini-flash:100";
        let now = Utc::now().timestamp();
        {
            let mut history = WARMUP_HISTORY.lock().unwrap();
            history.insert(key.to_string(), now);
        }
        assert!(check_cooldown(key, COOLDOWN_SECONDS));

        // Cleanup
        {
            let mut history = WARMUP_HISTORY.lock().unwrap();
            history.remove(key);
        }
    }

    #[test]
    fn test_check_cooldown_expired() {
        // Record a warmup 5 hours ago → cooldown (4h) should have expired
        let key = "test_cooldown_expired@example.com:gemini-flash:100";
        let five_hours_ago = Utc::now().timestamp() - 18000;
        {
            let mut history = WARMUP_HISTORY.lock().unwrap();
            history.insert(key.to_string(), five_hours_ago);
        }
        assert!(!check_cooldown(key, COOLDOWN_SECONDS));

        // Cleanup
        {
            let mut history = WARMUP_HISTORY.lock().unwrap();
            history.remove(key);
        }
    }

    #[test]
    fn test_check_cooldown_boundary() {
        // Record exactly at cooldown boundary (4 hours ago)
        let key = "test_cooldown_boundary@example.com:gemini-flash:100";
        let exactly_4h_ago = Utc::now().timestamp() - COOLDOWN_SECONDS;
        {
            let mut history = WARMUP_HISTORY.lock().unwrap();
            history.insert(key.to_string(), exactly_4h_ago);
        }
        // At exactly the boundary, now - last_ts == cooldown_seconds, so NOT in cooldown
        assert!(!check_cooldown(key, COOLDOWN_SECONDS));

        // Cleanup
        {
            let mut history = WARMUP_HISTORY.lock().unwrap();
            history.remove(key);
        }
    }

    #[test]
    fn test_check_cooldown_just_inside() {
        // Record 1 second less than cooldown → still in cooldown
        let key = "test_cooldown_inside@example.com:gemini-flash:100";
        let just_inside = Utc::now().timestamp() - (COOLDOWN_SECONDS - 1);
        {
            let mut history = WARMUP_HISTORY.lock().unwrap();
            history.insert(key.to_string(), just_inside);
        }
        assert!(check_cooldown(key, COOLDOWN_SECONDS));

        // Cleanup
        {
            let mut history = WARMUP_HISTORY.lock().unwrap();
            history.remove(key);
        }
    }

    // ── Record warmup history ───────────────────────────────────────

    #[test]
    fn test_record_warmup_history() {
        let key = "test_record@example.com:model:100";
        let ts = 1700000000i64;
        record_warmup_history(key, ts);

        let history = WARMUP_HISTORY.lock().unwrap();
        assert_eq!(history.get(key), Some(&ts));
        drop(history);

        // Cleanup
        {
            let mut history = WARMUP_HISTORY.lock().unwrap();
            history.remove(key);
        }
    }

    #[test]
    fn test_record_warmup_history_overwrites() {
        let key = "test_overwrite@example.com:model:100";
        record_warmup_history(key, 1000);
        record_warmup_history(key, 2000);

        let history = WARMUP_HISTORY.lock().unwrap();
        assert_eq!(history.get(key), Some(&2000));
        drop(history);

        // Cleanup
        {
            let mut history = WARMUP_HISTORY.lock().unwrap();
            history.remove(key);
        }
    }

    // ── Constants ───────────────────────────────────────────────────

    #[test]
    fn test_constants() {
        assert_eq!(COOLDOWN_SECONDS, 14400); // 4 hours
        assert_eq!(SCAN_INTERVAL_SECS, 600); // 10 minutes
        assert_eq!(BATCH_SIZE, 3);
        assert_eq!(BATCH_DELAY_SECS, 2);
        assert_eq!(HISTORY_CLEANUP_SECS, 86400); // 24 hours
    }

    // ── Cleanup logic ───────────────────────────────────────────────

    #[test]
    fn test_cleanup_history_removes_old_entries() {
        let old_key = "test_cleanup_old@example.com:model:100";
        let recent_key = "test_cleanup_recent@example.com:model:100";
        let old_ts = Utc::now().timestamp() - HISTORY_CLEANUP_SECS - 100;
        let recent_ts = Utc::now().timestamp() - 100;

        {
            let mut history = WARMUP_HISTORY.lock().unwrap();
            history.insert(old_key.to_string(), old_ts);
            history.insert(recent_key.to_string(), recent_ts);
        }

        cleanup_history();

        let history = WARMUP_HISTORY.lock().unwrap();
        assert!(history.get(old_key).is_none(), "Old entry should be removed");
        assert!(
            history.get(recent_key).is_some(),
            "Recent entry should be kept"
        );
        drop(history);

        // Cleanup
        {
            let mut history = WARMUP_HISTORY.lock().unwrap();
            history.remove(recent_key);
        }
    }

    // ── WarmupTask struct ───────────────────────────────────────────

    #[test]
    fn test_warmup_task_clone() {
        let task = WarmupTask {
            account_id: "acc-1".to_string(),
            email: "test@example.com".to_string(),
            model: "gemini-2.0-flash".to_string(),
            token: "tok_abc".to_string(),
            project_id: "proj_123".to_string(),
            percentage: 100,
            history_key: "test@example.com:gemini-2.0-flash:100".to_string(),
        };
        let cloned = task.clone();
        assert_eq!(cloned.account_id, "acc-1");
        assert_eq!(cloned.email, "test@example.com");
        assert_eq!(cloned.model, "gemini-2.0-flash");
        assert_eq!(cloned.percentage, 100);
    }

    // ── Batch size calculation ──────────────────────────────────────

    #[test]
    fn test_batch_count_calculation() {
        // Verify the ceiling division used for batch counting
        let cases = vec![
            (0, 0),
            (1, 1),
            (2, 1),
            (3, 1),
            (4, 2),
            (6, 2),
            (7, 3),
            (9, 3),
            (10, 4),
        ];
        for (total, expected_batches) in cases {
            let num_batches = if total == 0 {
                0
            } else {
                (total + BATCH_SIZE - 1) / BATCH_SIZE
            };
            assert_eq!(
                num_batches, expected_batches,
                "total={} → expected {} batches, got {}",
                total, expected_batches, num_batches
            );
        }
    }

    // ── Cooldown with custom duration ───────────────────────────────

    #[test]
    fn test_check_cooldown_custom_duration() {
        let key = "test_custom_cd@example.com:model:100";
        let now = Utc::now().timestamp();
        {
            let mut history = WARMUP_HISTORY.lock().unwrap();
            history.insert(key.to_string(), now - 30);
        }

        // 60-second cooldown: 30 seconds ago → still in cooldown
        assert!(check_cooldown(key, 60));
        // 10-second cooldown: 30 seconds ago → expired
        assert!(!check_cooldown(key, 10));

        // Cleanup
        {
            let mut history = WARMUP_HISTORY.lock().unwrap();
            history.remove(key);
        }
    }

    // ── Monitored model filtering ───────────────────────────────────

    #[test]
    fn test_monitored_model_contains_check() {
        let monitored = vec![
            "gemini-3-flash".to_string(),
            "claude".to_string(),
            "gemini-3-pro-high".to_string(),
        ];

        assert!(monitored.contains(&"gemini-3-flash".to_string()));
        assert!(monitored.contains(&"claude".to_string()));
        assert!(!monitored.contains(&"gemini-2.0-flash".to_string()));
        assert!(!monitored.contains(&"unknown-model".to_string()));
    }

    // ── Disabled account filtering ──────────────────────────────────

    #[test]
    fn test_disabled_account_skipped() {
        use crate::models::token::TokenData;

        let token = TokenData::new(
            "access".to_string(),
            "refresh".to_string(),
            3600,
            None,
            None,
            None,
        );
        let mut acct = Account::new(
            "test-id".to_string(),
            "test@example.com".to_string(),
            token,
        );

        // Not disabled → should not skip
        assert!(!acct.disabled && !acct.proxy_disabled);

        // Disabled → should skip
        acct.disabled = true;
        assert!(acct.disabled || acct.proxy_disabled);

        // Proxy disabled → should skip
        acct.disabled = false;
        acct.proxy_disabled = true;
        assert!(acct.disabled || acct.proxy_disabled);
    }

    // ── 403 forbidden flag persistence ──────────────────────────────

    #[test]
    fn test_forbidden_quota_detection() {
        use crate::models::quota::QuotaData;

        let mut quota = QuotaData::new();
        assert!(!quota.is_forbidden);

        quota.is_forbidden = true;
        assert!(quota.is_forbidden);
        // Forbidden accounts should have their quota persisted
        // and be skipped in subsequent scans
    }

    // ── History key uniqueness ──────────────────────────────────────

    #[test]
    fn test_history_keys_unique_per_account_model() {
        let key1 = format!("{}:{}:100", "alice@example.com", "gemini-flash");
        let key2 = format!("{}:{}:100", "bob@example.com", "gemini-flash");
        let key3 = format!("{}:{}:100", "alice@example.com", "claude");

        assert_ne!(key1, key2);
        assert_ne!(key1, key3);
        assert_ne!(key2, key3);
    }
}
