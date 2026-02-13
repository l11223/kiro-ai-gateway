// Token Manager - Core component for managing the in-memory token pool
//
// Requirements covered:
// - 1.6: Mark accounts as validation_blocked with deadline timestamp
// - 1.7: Auto-recover validation_blocked accounts when deadline expires
// - 4.1: P2C load balancing
// - 4.2: CacheFirst scheduling mode
// - 4.3: Balance scheduling mode
// - 4.4: PerformanceFirst scheduling mode
// - 4.12: Sorting by subscription tier > model quota > health score
// - 4.15: Fixed account mode (preferred_account_id)
//
// This module manages:
// - In-memory token pool (DashMap<String, ProxyToken>)
// - Loading accounts from disk with filtering (disabled/proxy_disabled/validation_blocked)
// - Single account reload
// - Complete account removal with associated data cleanup
// - Health score tracking and success recording
// - P2C (Power of Two Choices) load balancing
// - Session stickiness and scheduling modes

use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::models::config::{CircuitBreakerConfig, SchedulingMode, StickySessionConfig};
use crate::proxy::rate_limit::RateLimitTracker;

/// On-disk account state for safety checks
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnDiskAccountState {
    Enabled,
    Disabled,
    Unknown,
}

/// In-memory token cache for proxy request routing
#[derive(Debug, Clone)]
pub struct ProxyToken {
    pub account_id: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
    pub timestamp: i64,
    pub email: String,
    /// Account file path for updates
    pub account_path: PathBuf,
    pub project_id: Option<String>,
    /// Subscription tier: "FREE" | "PRO" | "ULTRA"
    pub subscription_tier: Option<String>,
    /// Remaining quota percentage for priority sorting
    pub remaining_quota: Option<i32>,
    /// Models protected by quota protection
    pub protected_models: HashSet<String>,
    /// Health score (0.0 - 1.0), higher is better
    pub health_score: f32,
    /// Quota reset timestamp for sorting optimization
    pub reset_time: Option<i64>,
    /// Whether account is validation blocked
    pub validation_blocked: bool,
    /// Timestamp until which the account is blocked
    pub validation_blocked_until: i64,
    /// In-memory cache for model-specific quotas
    pub model_quotas: HashMap<String, i32>,
}

/// Core token pool manager
pub struct TokenManager {
    /// In-memory token pool: account_id -> ProxyToken
    tokens: Arc<DashMap<String, ProxyToken>>,
    /// Data directory path
    data_dir: PathBuf,
    /// Rate limit tracker
    rate_limit_tracker: Arc<RateLimitTracker>,
    /// Scheduling configuration
    sticky_config: Arc<RwLock<StickySessionConfig>>,
    /// Session-to-account bindings (session_id -> account_id)
    session_accounts: Arc<DashMap<String, String>>,
    /// Health scores per account
    health_scores: Arc<DashMap<String, f32>>,
    /// Preferred account ID for fixed-account mode
    preferred_account_id: Arc<RwLock<Option<String>>>,
    /// Circuit breaker configuration cache
    circuit_breaker_config: Arc<RwLock<CircuitBreakerConfig>>,
    /// Background auto-cleanup task handle
    auto_cleanup_handle: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Cancellation token for graceful shutdown
    cancel_token: CancellationToken,
}

impl TokenManager {
    /// Create a new TokenManager
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            tokens: Arc::new(DashMap::new()),
            data_dir,
            rate_limit_tracker: Arc::new(RateLimitTracker::new()),
            sticky_config: Arc::new(RwLock::new(StickySessionConfig::default())),
            session_accounts: Arc::new(DashMap::new()),
            health_scores: Arc::new(DashMap::new()),
            preferred_account_id: Arc::new(RwLock::new(None)),
            circuit_breaker_config: Arc::new(RwLock::new(CircuitBreakerConfig::default())),
            auto_cleanup_handle: Arc::new(tokio::sync::Mutex::new(None)),
            cancel_token: CancellationToken::new(),
        }
    }

    /// Start background auto-cleanup task (every 15s, cleans expired rate limit records)
    pub async fn start_auto_cleanup(&self) {
        let tracker = self.rate_limit_tracker.clone();
        let cancel = self.cancel_token.child_token();

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("Auto-cleanup task received cancel signal");
                        break;
                    }
                    _ = interval.tick() => {
                        let cleaned = tracker.cleanup_expired();
                        if cleaned > 0 {
                            tracing::info!(
                                "Auto-cleanup: Removed {} expired rate limit record(s)",
                                cleaned
                            );
                        }
                    }
                }
            }
        });

        // Abort old task to prevent leaks, then store new handle
        let mut guard = self.auto_cleanup_handle.lock().await;
        if let Some(old) = guard.take() {
            old.abort();
            tracing::warn!("Aborted previous auto-cleanup task");
        }
        *guard = Some(handle);

        tracing::info!("Rate limit auto-cleanup task started (interval: 15s)");
    }

    /// Load all accounts from the data directory into the in-memory pool.
    ///
    /// Clears the existing pool and reloads from disk. Filters out:
    /// - disabled accounts
    /// - proxy_disabled accounts (unless reason is "quota_protection")
    /// - validation_blocked accounts (unless block has expired, in which case it auto-recovers)
    ///
    /// Returns the number of successfully loaded accounts.
    pub async fn load_accounts(&self) -> Result<usize, String> {
        let accounts_dir = self.data_dir.join("accounts");

        if !accounts_dir.exists() {
            return Err(format!("Accounts directory does not exist: {:?}", accounts_dir));
        }

        // Clear existing pool to reflect current on-disk state
        self.tokens.clear();

        let entries = std::fs::read_dir(&accounts_dir)
            .map_err(|e| format!("Failed to read accounts directory: {}", e))?;

        let mut count = 0;

        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            match self.load_single_account(&path).await {
                Ok(Some(token)) => {
                    let account_id = token.account_id.clone();
                    self.tokens.insert(account_id, token);
                    count += 1;
                }
                Ok(None) => {
                    // Account filtered out (disabled/blocked/etc.)
                }
                Err(e) => {
                    tracing::debug!("Failed to load account {:?}: {}", path, e);
                }
            }
        }

        Ok(count)
    }

    /// Reload a single account from disk.
    ///
    /// If the account is now disabled or unavailable, it is removed from the pool.
    /// On successful reload, clears any rate limit records for the account.
    pub async fn reload_account(&self, account_id: &str) -> Result<(), String> {
        let path = self
            .data_dir
            .join("accounts")
            .join(format!("{}.json", account_id));

        if !path.exists() {
            return Err(format!("Account file does not exist: {:?}", path));
        }

        match self.load_single_account(&path).await {
            Ok(Some(token)) => {
                self.tokens.insert(account_id.to_string(), token);
                // Clear rate limits on reload
                self.clear_rate_limit(account_id);
                Ok(())
            }
            Ok(None) => {
                // Account is now disabled/blocked - remove from pool
                self.remove_account(account_id);
                Ok(())
            }
            Err(e) => Err(format!("Failed to reload account: {}", e)),
        }
    }

    /// Reload all accounts (convenience wrapper)
    pub async fn reload_all_accounts(&self) -> Result<usize, String> {
        let count = self.load_accounts().await?;
        self.clear_all_rate_limits();
        Ok(count)
    }

    /// Completely remove an account and all associated data from memory.
    ///
    /// Cleans up:
    /// 1. Token pool entry
    /// 2. Health scores
    /// 3. Rate limit records
    /// 4. Session bindings referencing this account
    /// 5. Preferred account status (if this was the preferred account)
    pub fn remove_account(&self, account_id: &str) {
        // 1. Remove from token pool
        if self.tokens.remove(account_id).is_some() {
            tracing::info!("[Proxy] Removed account {} from memory cache", account_id);
        }

        // 2. Clean up health scores
        self.health_scores.remove(account_id);

        // 3. Clean up rate limit records
        self.clear_rate_limit(account_id);

        // 4. Clean up session bindings referencing this account
        self.session_accounts.retain(|_, v| v != account_id);

        // 5. Clear preferred account if it was this one
        if let Ok(mut preferred) = self.preferred_account_id.try_write() {
            if preferred.as_deref() == Some(account_id) {
                *preferred = None;
                tracing::info!(
                    "[Proxy] Cleared preferred account status for {}",
                    account_id
                );
            }
        }
    }

    /// Record a successful request for an account.
    ///
    /// - Increases health score (capped at 1.0)
    /// - Resets rate limit failure count via the tracker
    pub fn mark_success(&self, account_id: &str) {
        // Update health score
        let new_score = self
            .health_scores
            .get(account_id)
            .map(|v| (*v + 0.1).min(1.0))
            .unwrap_or(1.0);
        self.health_scores.insert(account_id.to_string(), new_score);

        // Also update the token's health_score in the pool
        if let Some(mut token) = self.tokens.get_mut(account_id) {
            token.health_score = new_score;
        }

        // Reset rate limit failure count
        self.rate_limit_tracker.mark_success(account_id);
    }

    /// Record a failed request for an account (decreases health score)
    pub fn record_failure(&self, account_id: &str) {
        let new_score = self
            .health_scores
            .get(account_id)
            .map(|v| (*v - 0.2).max(0.0))
            .unwrap_or(0.8);
        self.health_scores.insert(account_id.to_string(), new_score);

        if let Some(mut token) = self.tokens.get_mut(account_id) {
            token.health_score = new_score;
        }
    }

    /// Get the number of tokens in the pool
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// Check if the token pool is empty
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Get a reference to the rate limit tracker
    pub fn rate_limit_tracker(&self) -> &Arc<RateLimitTracker> {
        &self.rate_limit_tracker
    }

    /// Get a reference to the session accounts map
    pub fn session_accounts(&self) -> &Arc<DashMap<String, String>> {
        &self.session_accounts
    }

    /// Get a reference to the tokens map
    pub fn tokens(&self) -> &Arc<DashMap<String, ProxyToken>> {
        &self.tokens
    }

    /// Get sticky session config
    pub async fn get_sticky_config(&self) -> StickySessionConfig {
        self.sticky_config.read().await.clone()
    }

    /// Update sticky session config
    pub async fn update_sticky_config(&self, new_config: StickySessionConfig) {
        let mut config = self.sticky_config.write().await;
        *config = new_config;
    }

    /// Update circuit breaker config
    pub async fn update_circuit_breaker_config(&self, config: CircuitBreakerConfig) {
        let mut cb = self.circuit_breaker_config.write().await;
        *cb = config;
    }

    /// Get circuit breaker config
    pub async fn get_circuit_breaker_config(&self) -> CircuitBreakerConfig {
        self.circuit_breaker_config.read().await.clone()
    }

    /// Set preferred account ID (fixed account mode)
    pub async fn set_preferred_account(&self, account_id: Option<String>) {
        let mut preferred = self.preferred_account_id.write().await;
        *preferred = account_id;
    }

    /// Get preferred account ID
    pub async fn get_preferred_account(&self) -> Option<String> {
        self.preferred_account_id.read().await.clone()
    }

    /// Clear session binding for a specific session
    pub fn clear_session_binding(&self, session_id: &str) {
        self.session_accounts.remove(session_id);
    }

    /// Clear all session bindings
    pub fn clear_all_sessions(&self) {
        self.session_accounts.clear();
    }

    /// Clear rate limit for a specific account
    pub fn clear_rate_limit(&self, account_id: &str) -> bool {
        self.rate_limit_tracker.clear(account_id)
    }

    /// Clear all rate limits (optimistic reset)
    pub fn clear_all_rate_limits(&self) {
        self.rate_limit_tracker.clear_all();
    }

    /// Check if an account is rate limited
    pub fn is_rate_limited(&self, account_id: &str, model: Option<&str>) -> bool {
        self.rate_limit_tracker.is_rate_limited(account_id, model)
    }

    /// Get account ID by email
    pub fn get_account_id_by_email(&self, email: &str) -> Option<String> {
        self.tokens
            .iter()
            .find(|entry| entry.value().email == email)
            .map(|entry| entry.key().clone())
    }

    /// Graceful shutdown: cancel background tasks and wait
    pub async fn graceful_shutdown(&self, timeout: std::time::Duration) {
        self.cancel_token.cancel();

        let mut guard = self.auto_cleanup_handle.lock().await;
        if let Some(handle) = guard.take() {
            let _ = tokio::time::timeout(timeout, handle).await;
        }
    }

    /// P2C pool size: pick 2 random candidates from the top N
    const P2C_POOL_SIZE: usize = 6;

    /// Core scheduling entry point: get a token for the given model and session.
    ///
    /// Implements:
    /// - Fixed account mode (preferred_account_id) [Req 4.15]
    /// - Session stickiness (CacheFirst / Balance) [Req 4.2, 4.3]
    /// - P2C load balancing [Req 4.1]
    /// - PerformanceFirst pure rotation [Req 4.4]
    /// - Sorting: subscription tier > model quota > health score [Req 4.12]
    pub async fn get_token(
        &self,
        model: &str,
        session_id: Option<&str>,
    ) -> Result<ProxyToken, String> {
        // 5-second timeout to prevent deadlocks
        let timeout_duration = std::time::Duration::from_secs(5);
        match tokio::time::timeout(
            timeout_duration,
            self.get_token_internal(model, session_id),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(
                "Token acquisition timeout (5s) - system too busy or deadlock detected".to_string(),
            ),
        }
    }

    /// Internal implementation of the token selection logic.
    async fn get_token_internal(
        &self,
        target_model: &str,
        session_id: Option<&str>,
    ) -> Result<ProxyToken, String> {
        let mut tokens_snapshot: Vec<ProxyToken> =
            self.tokens.iter().map(|e| e.value().clone()).collect();

        if tokens_snapshot.is_empty() {
            return Err("Token pool is empty".to_string());
        }

        let total = tokens_snapshot.len();

        // Normalize target model name to standard ID for quota protection checks
        let normalized_target =
            crate::proxy::common::model_mapping::normalize_to_standard_id(target_model)
                .unwrap_or_else(|| target_model.to_string());

        // Sort candidates: subscription tier > model-specific quota > health score [Req 4.12]
        tokens_snapshot.sort_by(|a, b| Self::compare_tokens(a, b, &normalized_target));

        // Read scheduling config
        let scheduling = self.sticky_config.read().await.clone();

        // ===== Fixed account mode [Req 4.15] =====
        let preferred_id = self.preferred_account_id.read().await.clone();
        if let Some(ref pref_id) = preferred_id {
            if let Some(preferred_token) = tokens_snapshot
                .iter()
                .find(|t| &t.account_id == pref_id)
                .cloned()
            {
                // Check on-disk state
                match Self::get_account_state_on_disk(&preferred_token.account_path).await {
                    OnDiskAccountState::Disabled => {
                        tracing::warn!(
                            "Preferred account {} is disabled on disk, purging and falling back",
                            preferred_token.email
                        );
                        self.remove_account(&preferred_token.account_id);
                        tokens_snapshot
                            .retain(|t| t.account_id != preferred_token.account_id);
                        // Clear preferred
                        let mut preferred = self.preferred_account_id.write().await;
                        if preferred.as_deref() == Some(pref_id.as_str()) {
                            *preferred = None;
                        }
                        if tokens_snapshot.is_empty() {
                            return Err("Token pool is empty".to_string());
                        }
                    }
                    OnDiskAccountState::Unknown => {
                        tracing::warn!(
                            "Preferred account {} state on disk is unavailable, falling back",
                            preferred_token.email
                        );
                        tokens_snapshot
                            .retain(|t| t.account_id != preferred_token.account_id);
                        if tokens_snapshot.is_empty() {
                            return Err("Token pool is empty".to_string());
                        }
                    }
                    OnDiskAccountState::Enabled => {
                        let is_rate_limited = self.rate_limit_tracker.is_rate_limited(
                            &preferred_token.account_id,
                            Some(&normalized_target),
                        );
                        let is_quota_protected =
                            preferred_token.protected_models.contains(&normalized_target);

                        if !is_rate_limited && !is_quota_protected {
                            tracing::info!(
                                "Using preferred account: {} (fixed mode)",
                                preferred_token.email
                            );
                            return Ok(preferred_token);
                        } else {
                            tracing::warn!(
                                "Preferred account {} is {} for {}, falling back to round-robin",
                                preferred_token.email,
                                if is_rate_limited {
                                    "rate-limited"
                                } else {
                                    "quota-protected"
                                },
                                target_model
                            );
                        }
                    }
                }
            } else {
                tracing::warn!(
                    "Preferred account {} not found in pool, falling back to round-robin",
                    pref_id
                );
            }
        }

        // ===== Main scheduling loop =====
        let mut attempted: HashSet<String> = HashSet::new();
        let last_error: Option<String> = None;

        for attempt in 0..total {
            let rotate = attempt > 0;
            let mut target_token: Option<ProxyToken> = None;

            // === Mode A: Sticky session (CacheFirst or Balance with session_id) [Req 4.2, 4.3] ===
            if !rotate
                && session_id.is_some()
                && scheduling.mode != SchedulingMode::PerformanceFirst
            {
                let sid = session_id.unwrap();

                if let Some(bound_id) = self.session_accounts.get(sid).map(|v| v.clone()) {
                    if let Some(bound_token) =
                        tokens_snapshot.iter().find(|t| t.account_id == bound_id)
                    {
                        let reset_sec = self
                            .rate_limit_tracker
                            .get_remaining_wait(&bound_token.account_id, None);

                        if reset_sec > 0 {
                            match scheduling.mode {
                                SchedulingMode::CacheFirst => {
                                    // CacheFirst: wait up to max_wait_seconds [Req 4.2]
                                    if reset_sec <= scheduling.max_wait_seconds {
                                        tracing::debug!(
                                            "CacheFirst: Waiting {}s for bound account {}",
                                            reset_sec,
                                            bound_token.email
                                        );
                                        tokio::time::sleep(std::time::Duration::from_secs(
                                            reset_sec,
                                        ))
                                        .await;
                                        // Re-check after wait
                                        if !self.rate_limit_tracker.is_rate_limited(
                                            &bound_token.account_id,
                                            Some(&normalized_target),
                                        ) && !bound_token
                                            .protected_models
                                            .contains(&normalized_target)
                                        {
                                            target_token = Some(bound_token.clone());
                                        } else {
                                            self.session_accounts.remove(sid);
                                        }
                                    } else {
                                        // Wait too long, unbind and switch
                                        self.session_accounts.remove(sid);
                                    }
                                }
                                SchedulingMode::Balance => {
                                    // Balance: immediately switch [Req 4.3]
                                    tracing::debug!(
                                        "Balance: Bound account {} is rate-limited ({}s), switching",
                                        bound_token.email,
                                        reset_sec
                                    );
                                    self.session_accounts.remove(sid);
                                }
                                SchedulingMode::PerformanceFirst => {
                                    // Should not reach here due to outer check
                                    unreachable!();
                                }
                            }
                        } else if !attempted.contains(&bound_id)
                            && !bound_token.protected_models.contains(&normalized_target)
                        {
                            // Account available, reuse it
                            tracing::debug!(
                                "Sticky Session: Reusing bound account {} for session {}",
                                bound_token.email,
                                sid
                            );
                            target_token = Some(bound_token.clone());
                        } else if bound_token.protected_models.contains(&normalized_target) {
                            tracing::debug!(
                                "Sticky Session: Bound account {} is quota-protected for {}, switching",
                                bound_token.email,
                                normalized_target
                            );
                            self.session_accounts.remove(sid);
                        }
                    } else {
                        // Bound account no longer exists
                        self.session_accounts.remove(sid);
                    }
                }
            }

            // === Mode B/C: P2C selection [Req 4.1] ===
            if target_token.is_none() {
                // Filter out rate-limited accounts
                let non_limited: Vec<ProxyToken> = tokens_snapshot
                    .iter()
                    .filter(|t| {
                        !self.rate_limit_tracker.is_rate_limited(
                            &t.account_id,
                            Some(&normalized_target),
                        )
                    })
                    .cloned()
                    .collect();

                if let Some(selected) =
                    self.select_with_p2c(&non_limited, &attempted, &normalized_target)
                {
                    target_token = Some(selected.clone());

                    // Bind session if sticky mode [Req 4.2, 4.3]
                    if let Some(sid) = session_id {
                        if scheduling.mode != SchedulingMode::PerformanceFirst {
                            self.session_accounts
                                .insert(sid.to_string(), selected.account_id.clone());
                            tracing::debug!(
                                "Sticky Session: Bound new account {} to session {}",
                                selected.email,
                                sid
                            );
                        }
                    }
                }
            }

            let token = match target_token {
                Some(t) => t,
                None => {
                    // Optimistic reset: if shortest wait <= 2s, buffer and retry [Req 4.11]
                    let min_wait = tokens_snapshot
                        .iter()
                        .filter_map(|t| {
                            self.rate_limit_tracker.get_reset_seconds(&t.account_id)
                        })
                        .min();

                    if let Some(wait_sec) = min_wait {
                        if wait_sec <= 2 {
                            let wait_ms = (wait_sec as f64 * 1000.0) as u64;
                            tracing::warn!(
                                "All accounts rate-limited, shortest wait {}s. Buffering {}ms...",
                                wait_sec,
                                wait_ms
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;

                            // Retry after buffer
                            let retry_token = tokens_snapshot.iter().find(|t| {
                                !attempted.contains(&t.account_id)
                                    && !self.rate_limit_tracker.is_rate_limited(
                                        &t.account_id,
                                        Some(&normalized_target),
                                    )
                                    && !t.protected_models.contains(&normalized_target)
                            });

                            if let Some(t) = retry_token {
                                t.clone()
                            } else {
                                // Optimistic reset: clear all rate limits
                                tracing::warn!(
                                    "Buffer failed. Executing optimistic reset for {} accounts...",
                                    tokens_snapshot.len()
                                );
                                self.rate_limit_tracker.clear_all();

                                let final_token = tokens_snapshot.iter().find(|t| {
                                    !attempted.contains(&t.account_id)
                                        && !t.protected_models.contains(&normalized_target)
                                });

                                if let Some(t) = final_token {
                                    t.clone()
                                } else {
                                    return Err(
                                        "All accounts failed after optimistic reset.".to_string(),
                                    );
                                }
                            }
                        } else {
                            return Err(format!(
                                "All accounts limited. Wait {}s.",
                                wait_sec
                            ));
                        }
                    } else {
                        return Err("All accounts failed or unhealthy.".to_string());
                    }
                }
            };

            // Safety net: check on-disk state before returning
            match Self::get_account_state_on_disk(&token.account_path).await {
                OnDiskAccountState::Disabled => {
                    tracing::warn!(
                        "Selected account {} is disabled on disk, purging and retrying",
                        token.email
                    );
                    attempted.insert(token.account_id.clone());
                    self.remove_account(&token.account_id);
                    continue;
                }
                OnDiskAccountState::Unknown => {
                    tracing::warn!(
                        "Selected account {} state on disk is unavailable, skipping",
                        token.email
                    );
                    attempted.insert(token.account_id.clone());
                    continue;
                }
                OnDiskAccountState::Enabled => {}
            }

            return Ok(token);
        }

        Err(last_error.unwrap_or_else(|| "All accounts failed".to_string()))
    }

    /// P2C (Power of Two Choices) selection algorithm [Req 4.1].
    ///
    /// Randomly picks 2 candidates from the top P2C_POOL_SIZE, returns the one
    /// with higher health score (candidates are already sorted by tier/quota).
    fn select_with_p2c<'a>(
        &self,
        candidates: &'a [ProxyToken],
        attempted: &HashSet<String>,
        normalized_target: &str,
    ) -> Option<&'a ProxyToken> {
        use rand::Rng;

        // Filter: skip attempted and quota-protected accounts
        let available: Vec<&ProxyToken> = candidates
            .iter()
            .filter(|t| !attempted.contains(&t.account_id))
            .filter(|t| !t.protected_models.contains(normalized_target))
            .collect();

        if available.is_empty() {
            return None;
        }
        if available.len() == 1 {
            return Some(available[0]);
        }

        // P2C: pick 2 random from top min(P2C_POOL_SIZE, len)
        let pool_size = available.len().min(Self::P2C_POOL_SIZE);
        let mut rng = rand::thread_rng();

        let pick1 = rng.gen_range(0..pool_size);
        let mut pick2 = rng.gen_range(0..pool_size);
        if pick2 == pick1 {
            pick2 = (pick1 + 1) % pool_size;
        }

        let c1 = available[pick1];
        let c2 = available[pick2];

        // Select the one with higher health score
        let selected = if c1.health_score >= c2.health_score {
            c1
        } else {
            c2
        };

        tracing::debug!(
            "[P2C] Selected {} (health={:.2}) from [{} (health={:.2}), {} (health={:.2})]",
            selected.email,
            selected.health_score,
            c1.email,
            c1.health_score,
            c2.email,
            c2.health_score
        );

        Some(selected)
    }

    /// Compare two tokens for sorting [Req 4.12].
    ///
    /// Priority order:
    /// 1. Subscription tier (ULTRA > PRO > FREE)
    /// 2. Target model quota (higher is better)
    /// 3. Health score (higher is better)
    fn compare_tokens(
        a: &ProxyToken,
        b: &ProxyToken,
        normalized_target: &str,
    ) -> std::cmp::Ordering {
        use std::cmp::Ordering;

        let tier_priority = |tier: &Option<String>| {
            let t = tier.as_deref().unwrap_or("").to_lowercase();
            if t.contains("ultra") {
                0
            } else if t.contains("pro") {
                1
            } else if t.contains("free") {
                2
            } else {
                3
            }
        };

        // Priority 1: Subscription tier (ULTRA > PRO > FREE)
        let tier_cmp =
            tier_priority(&a.subscription_tier).cmp(&tier_priority(&b.subscription_tier));
        if tier_cmp != Ordering::Equal {
            return tier_cmp;
        }

        // Priority 2: Target model quota (higher is better)
        let quota_a = a
            .model_quotas
            .get(normalized_target)
            .copied()
            .unwrap_or(a.remaining_quota.unwrap_or(0));
        let quota_b = b
            .model_quotas
            .get(normalized_target)
            .copied()
            .unwrap_or(b.remaining_quota.unwrap_or(0));
        let quota_cmp = quota_b.cmp(&quota_a);
        if quota_cmp != Ordering::Equal {
            return quota_cmp;
        }

        // Priority 3: Health score (higher is better)
        b.health_score
            .partial_cmp(&a.health_score)
            .unwrap_or(Ordering::Equal)
    }
}

// Quota protection methods
impl TokenManager {
    /// Check and apply quota protection for an account during loading.
    ///
    /// Groups models by Standard ID, takes the lowest quota in each group,
    /// and triggers protection if below the configured threshold.
    ///
    /// Also handles migration from old account-level protection (proxy_disabled
    /// with reason "quota_protection") to model-level protection.
    ///
    /// Returns false always - we no longer skip accounts due to quota;
    /// instead, model-level filtering happens in get_token via protected_models.
    ///
    /// Requirements: 4.5, 4.6, 5.5
    pub async fn check_and_protect_quota(
        &self,
        account_json: &mut serde_json::Value,
        account_path: &PathBuf,
    ) -> bool {
        // 1. Load quota protection config
        let config = match crate::modules::config::load_app_config() {
            Ok(cfg) => cfg.quota_protection,
            Err(_) => return false,
        };

        if !config.enabled {
            return false;
        }

        // 2. Get quota info (clone to avoid borrow conflicts)
        let quota = match account_json.get("quota") {
            Some(q) => q.clone(),
            None => return false,
        };

        // 3. [Compatibility] Check if account was disabled by old account-level quota protection
        let is_proxy_disabled = account_json
            .get("proxy_disabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let reason = account_json
            .get("proxy_disabled_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if is_proxy_disabled && reason == "quota_protection" {
            // Migrate from account-level to model-level protection
            return self
                .check_and_restore_quota(account_json, account_path, &quota, &config)
                .await;
        }

        // 4. Get model list
        let models = match quota.get("models").and_then(|m| m.as_array()) {
            Some(m) => m,
            None => return false,
        };

        // 5. Aggregate: group by Standard ID, take minimum percentage per group
        let mut group_min_percentage: HashMap<String, i32> = HashMap::new();

        for model in models {
            let name = model.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let percentage = model
                .get("percentage")
                .and_then(|v| v.as_i64())
                .unwrap_or(100) as i32;

            if let Some(std_id) =
                crate::proxy::common::model_mapping::normalize_to_standard_id(name)
            {
                let entry = group_min_percentage.entry(std_id).or_insert(100);
                if percentage < *entry {
                    *entry = percentage;
                }
            }
        }

        // 6. For each monitored Standard ID, trigger or restore protection
        let threshold = config.threshold_percentage as i32;
        let account_id = account_json
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let mut changed = false;

        for std_id in &config.monitored_models {
            // Get the group's minimum percentage; default to 100 if account lacks this group
            let min_pct = group_min_percentage.get(std_id).cloned().unwrap_or(100);

            if min_pct <= threshold {
                // Trigger protection for the entire group
                if self
                    .trigger_quota_protection(
                        account_json,
                        &account_id,
                        account_path,
                        min_pct,
                        threshold,
                        std_id,
                    )
                    .await
                    .unwrap_or(false)
                {
                    changed = true;
                }
            } else {
                // Only restore if the model was previously protected
                let is_protected = account_json
                    .get("protected_models")
                    .and_then(|v| v.as_array())
                    .map_or(false, |arr| {
                        arr.iter().any(|m| m.as_str() == Some(std_id.as_str()))
                    });

                if is_protected {
                    if self
                        .restore_quota_protection(
                            account_json,
                            &account_id,
                            account_path,
                            std_id,
                        )
                        .await
                        .unwrap_or(false)
                    {
                        changed = true;
                    }
                }
            }
        }

        let _ = changed;

        // Never skip the account; model-level filtering happens in get_token
        false
    }

    /// Trigger quota protection for a specific model.
    ///
    /// Adds the model to the account's protected_models set and persists to disk.
    /// Returns true if the model was newly added (i.e., state changed).
    ///
    /// Requirements: 4.5, 5.5
    pub async fn trigger_quota_protection(
        &self,
        account_json: &mut serde_json::Value,
        account_id: &str,
        account_path: &PathBuf,
        current_val: i32,
        threshold: i32,
        model_name: &str,
    ) -> Result<bool, String> {
        // Initialize protected_models array if missing
        if account_json.get("protected_models").is_none() {
            account_json["protected_models"] = serde_json::Value::Array(Vec::new());
        }

        let protected_models = account_json["protected_models"].as_array_mut().unwrap();

        // Check if already protected
        if !protected_models
            .iter()
            .any(|m| m.as_str() == Some(model_name))
        {
            protected_models.push(serde_json::Value::String(model_name.to_string()));

            tracing::info!(
                "Account {} model {} quota protected ({}% <= {}%), added to protected_models",
                account_id,
                model_name,
                current_val,
                threshold
            );

            // Persist to disk
            std::fs::write(
                account_path,
                serde_json::to_string_pretty(account_json).unwrap(),
            )
            .map_err(|e| format!("Failed to write file: {}", e))?;

            // Update in-memory token if present
            if let Some(mut token) = self.tokens.get_mut(account_id) {
                token.protected_models.insert(model_name.to_string());
            }

            return Ok(true);
        }

        Ok(false)
    }

    /// Restore quota protection for a specific model.
    ///
    /// Removes the model from the account's protected_models set and persists to disk.
    /// Returns true if the model was removed (i.e., state changed).
    ///
    /// Requirements: 4.6
    pub async fn restore_quota_protection(
        &self,
        account_json: &mut serde_json::Value,
        account_id: &str,
        account_path: &PathBuf,
        model_name: &str,
    ) -> Result<bool, String> {
        if let Some(arr) = account_json
            .get_mut("protected_models")
            .and_then(|v| v.as_array_mut())
        {
            let original_len = arr.len();
            arr.retain(|m| m.as_str() != Some(model_name));

            if arr.len() < original_len {
                tracing::info!(
                    "Account {} model {} quota recovered, removed from protected_models",
                    account_id,
                    model_name
                );

                // Persist to disk
                std::fs::write(
                    account_path,
                    serde_json::to_string_pretty(account_json).unwrap(),
                )
                .map_err(|e| format!("Failed to write file: {}", e))?;

                // Update in-memory token if present
                if let Some(mut token) = self.tokens.get_mut(account_id) {
                    token.protected_models.remove(model_name);
                }

                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Migrate from old account-level quota protection to model-level protection.
    ///
    /// Clears proxy_disabled and sets up per-model protected_models based on
    /// current quota data.
    async fn check_and_restore_quota(
        &self,
        account_json: &mut serde_json::Value,
        account_path: &PathBuf,
        quota: &serde_json::Value,
        config: &crate::models::config::QuotaProtectionConfig,
    ) -> bool {
        tracing::info!(
            "Migrating account {} from account-level to model-level quota protection",
            account_json
                .get("email")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        );

        // Clear account-level protection
        account_json["proxy_disabled"] = serde_json::Value::Bool(false);
        account_json["proxy_disabled_reason"] = serde_json::Value::Null;
        account_json["proxy_disabled_at"] = serde_json::Value::Null;

        let threshold = config.threshold_percentage as i32;
        let mut protected_list = Vec::new();

        if let Some(models) = quota.get("models").and_then(|m| m.as_array()) {
            for model in models {
                let name = model.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if !config.monitored_models.iter().any(|m| m == name) {
                    continue;
                }

                let percentage = model
                    .get("percentage")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as i32;
                if percentage <= threshold {
                    protected_list.push(serde_json::Value::String(name.to_string()));
                }
            }
        }

        account_json["protected_models"] = serde_json::Value::Array(protected_list);

        let _ = std::fs::write(
            account_path,
            serde_json::to_string_pretty(account_json).unwrap(),
        );

        // Return false: account can now be loaded (model-level filtering in get_token)
        false
    }

    /// Get model quota from a JSON file on disk for a specific model.
    ///
    /// Reads the account file and looks up the quota percentage for the given
    /// model name (normalized to standard ID).
    #[allow(dead_code)]
    fn get_model_quota_from_json(account_path: &PathBuf, model_name: &str) -> Option<i32> {
        let content = std::fs::read_to_string(account_path).ok()?;
        let account: serde_json::Value = serde_json::from_str(&content).ok()?;
        let models = account.get("quota")?.get("models")?.as_array()?;

        for model in models {
            if let Some(name) = model.get("name").and_then(|v| v.as_str()) {
                if crate::proxy::common::model_mapping::normalize_to_standard_id(name)
                    .unwrap_or_else(|| name.to_string())
                    == model_name
                {
                    return model
                        .get("percentage")
                        .and_then(|v| v.as_i64())
                        .map(|p| p as i32);
                }
            }
        }
        None
    }

    /// Test helper: public access to get_model_quota_from_json
    #[cfg(test)]
    pub fn get_model_quota_from_json_for_test(
        account_path: &PathBuf,
        model_name: &str,
    ) -> Option<i32> {
        Self::get_model_quota_from_json(account_path, model_name)
    }
}

// Private helper methods
impl TokenManager {
    /// Check if an account has been disabled on disk.
    ///
    /// Safety net to avoid selecting a disabled account when the in-memory pool
    /// hasn't been reloaded yet. Tolerant to transient read/parse failures.
    async fn get_account_state_on_disk(account_path: &PathBuf) -> OnDiskAccountState {
        const MAX_RETRIES: usize = 2;
        const RETRY_DELAY_MS: u64 = 5;

        for attempt in 0..=MAX_RETRIES {
            let content = match tokio::fs::read_to_string(account_path).await {
                Ok(c) => c,
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        return OnDiskAccountState::Disabled;
                    }
                    if attempt < MAX_RETRIES {
                        tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
                        continue;
                    }
                    tracing::debug!(
                        "Failed to read account file on disk {:?}: {}",
                        account_path,
                        e
                    );
                    return OnDiskAccountState::Unknown;
                }
            };

            let account = match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(v) => v,
                Err(e) => {
                    if attempt < MAX_RETRIES {
                        tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
                        continue;
                    }
                    tracing::debug!(
                        "Failed to parse account JSON on disk {:?}: {}",
                        account_path,
                        e
                    );
                    return OnDiskAccountState::Unknown;
                }
            };

            let disabled = account
                .get("disabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                || account
                    .get("proxy_disabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                || account
                    .get("quota")
                    .and_then(|q| q.get("is_forbidden"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

            return if disabled {
                OnDiskAccountState::Disabled
            } else {
                OnDiskAccountState::Enabled
            };
        }

        OnDiskAccountState::Unknown
    }

    /// Load a single account from a JSON file.
    ///
    /// Returns:
    /// - Ok(Some(token)) if the account is valid and should be in the pool
    /// - Ok(None) if the account should be skipped (disabled/blocked/etc.)
    /// - Err if the file cannot be read or parsed
    async fn load_single_account(&self, path: &PathBuf) -> Result<Option<ProxyToken>, String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;

        let mut account: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| format!("Failed to parse JSON: {}", e))?;

        // Check if account is manually disabled (not quota_protection)
        let is_proxy_disabled = account
            .get("proxy_disabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let disabled_reason = account
            .get("proxy_disabled_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if is_proxy_disabled && disabled_reason != "quota_protection" {
            tracing::debug!(
                "Account skipped (manual disable): {:?} (email={}, reason={})",
                path,
                account.get("email").and_then(|v| v.as_str()).unwrap_or("<unknown>"),
                disabled_reason
            );
            return Ok(None);
        }

        // Requirement 1.7: Check validation_blocked and auto-recover if expired
        if account
            .get("validation_blocked")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let block_until = account
                .get("validation_blocked_until")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);

            let now = chrono::Utc::now().timestamp();

            if now < block_until {
                // Still blocked
                tracing::debug!(
                    "Skipping validation-blocked account: {:?} (email={}, blocked until {})",
                    path,
                    account.get("email").and_then(|v| v.as_str()).unwrap_or("<unknown>"),
                    chrono::DateTime::from_timestamp(block_until, 0)
                        .map(|dt| dt.format("%H:%M:%S").to_string())
                        .unwrap_or_else(|| block_until.to_string())
                );
                return Ok(None);
            } else {
                // Block expired - auto-recover (Requirement 1.7)
                account["validation_blocked"] = serde_json::json!(false);
                account["validation_blocked_until"] = serde_json::json!(null);
                account["validation_blocked_reason"] = serde_json::Value::Null;

                let updated_json =
                    serde_json::to_string_pretty(&account).map_err(|e| e.to_string())?;
                std::fs::write(path, updated_json).map_err(|e| e.to_string())?;
                tracing::info!(
                    "Validation block expired and cleared for account: {}",
                    account.get("email").and_then(|v| v.as_str()).unwrap_or("<unknown>")
                );
            }
        }

        // Check main disabled flag
        if account
            .get("disabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            tracing::debug!(
                "Skipping disabled account: {:?} (email={})",
                path,
                account.get("email").and_then(|v| v.as_str()).unwrap_or("<unknown>")
            );
            return Ok(None);
        }

        // Safety check: verify state on disk again to handle concurrent writes
        if Self::get_account_state_on_disk(path).await == OnDiskAccountState::Disabled {
            tracing::debug!("Account file {:?} is disabled on disk, skipping.", path);
            return Ok(None);
        }

        // Re-check proxy_disabled after potential quota protection changes
        if account
            .get("proxy_disabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            tracing::debug!(
                "Skipping proxy-disabled account: {:?} (email={})",
                path,
                account.get("email").and_then(|v| v.as_str()).unwrap_or("<unknown>")
            );
            return Ok(None);
        }

        // Extract required fields
        let account_id = account["id"]
            .as_str()
            .ok_or("Missing id field")?
            .to_string();

        let email = account["email"]
            .as_str()
            .ok_or("Missing email field")?
            .to_string();

        let token_obj = account["token"]
            .as_object()
            .ok_or("Missing token field")?;

        let access_token = token_obj["access_token"]
            .as_str()
            .ok_or("Missing access_token")?
            .to_string();

        let refresh_token = token_obj["refresh_token"]
            .as_str()
            .ok_or("Missing refresh_token")?
            .to_string();

        let expires_in = token_obj["expires_in"]
            .as_i64()
            .ok_or("Missing expires_in")?;

        let timestamp = token_obj["expiry_timestamp"]
            .as_i64()
            .ok_or("Missing expiry_timestamp")?;

        let project_id = token_obj
            .get("project_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        // Extract subscription tier
        let subscription_tier = account
            .get("quota")
            .and_then(|q| q.get("subscription_tier"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Extract remaining quota (max percentage across all models)
        let remaining_quota = account
            .get("quota")
            .and_then(|q| self.calculate_quota_stats(q));

        // Extract protected models
        let protected_models: HashSet<String> = account
            .get("protected_models")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        // Get health score from cache or default to 1.0
        let health_score = self
            .health_scores
            .get(&account_id)
            .map(|v| *v)
            .unwrap_or(1.0);

        // Extract earliest quota reset time
        let reset_time = self.extract_earliest_reset_time(&account);

        // Build model quotas cache
        let mut model_quotas = HashMap::new();
        if let Some(models) = account
            .get("quota")
            .and_then(|q| q.get("models"))
            .and_then(|m| m.as_array())
        {
            for model in models {
                if let (Some(name), Some(pct)) = (
                    model.get("name").and_then(|v| v.as_str()),
                    model.get("percentage").and_then(|v| v.as_i64()),
                ) {
                    let standard_id =
                        crate::proxy::common::model_mapping::normalize_to_standard_id(name)
                            .unwrap_or_else(|| name.to_string());
                    model_quotas.insert(standard_id, pct as i32);
                }
            }
        }

        Ok(Some(ProxyToken {
            account_id,
            access_token,
            refresh_token,
            expires_in,
            timestamp,
            email,
            account_path: path.clone(),
            project_id,
            subscription_tier,
            remaining_quota,
            protected_models,
            health_score,
            reset_time,
            validation_blocked: account
                .get("validation_blocked")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            validation_blocked_until: account
                .get("validation_blocked_until")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            model_quotas,
        }))
    }

    /// Calculate the maximum quota percentage across all models
    fn calculate_quota_stats(&self, quota: &serde_json::Value) -> Option<i32> {
        quota
            .get("models")
            .and_then(|m| m.as_array())
            .map(|models| {
                models
                    .iter()
                    .filter_map(|m| m.get("percentage").and_then(|v| v.as_i64()))
                    .max()
                    .map(|v| v as i32)
            })
            .flatten()
    }

    /// Extract the earliest quota reset time from an account's quota data.
    ///
    /// Used for sorting optimization: accounts with sooner reset times
    /// may be preferred as their quota will refresh earlier.
    fn extract_earliest_reset_time(&self, account: &serde_json::Value) -> Option<i64> {
        let models = account
            .get("quota")
            .and_then(|q| q.get("models"))
            .and_then(|m| m.as_array())?;

        let mut earliest: Option<i64> = None;

        for model in models {
            if let Some(reset_str) = model.get("reset_time").and_then(|v| v.as_str()) {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(reset_str) {
                    let ts = dt.timestamp();
                    earliest = Some(earliest.map_or(ts, |e: i64| e.min(ts)));
                }
            }
        }

        earliest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper to create a temporary data directory for tests
    struct TestDataDir {
        path: PathBuf,
    }

    impl TestDataDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "kiro_token_mgr_test_{}",
                uuid::Uuid::new_v4().simple()
            ));
            fs::create_dir_all(path.join("accounts")).expect("Failed to create test dir");
            Self { path }
        }
    }

    impl Drop for TestDataDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// Create a minimal valid account JSON file
    fn create_account_file(dir: &PathBuf, id: &str, email: &str) -> PathBuf {
        let account_json = serde_json::json!({
            "id": id,
            "email": email,
            "name": "Test User",
            "token": {
                "access_token": format!("at_{}", id),
                "refresh_token": format!("rt_{}", id),
                "expires_in": 3600,
                "expiry_timestamp": chrono::Utc::now().timestamp() + 3600,
                "token_type": "Bearer"
            },
            "disabled": false,
            "proxy_disabled": false,
            "validation_blocked": false,
            "protected_models": [],
            "created_at": chrono::Utc::now().timestamp(),
            "last_used": chrono::Utc::now().timestamp()
        });

        let path = dir.join("accounts").join(format!("{}.json", id));
        fs::write(&path, serde_json::to_string_pretty(&account_json).unwrap()).unwrap();
        path
    }

    /// Create a disabled account file
    fn create_disabled_account_file(dir: &PathBuf, id: &str, email: &str) -> PathBuf {
        let account_json = serde_json::json!({
            "id": id,
            "email": email,
            "name": "Disabled User",
            "token": {
                "access_token": format!("at_{}", id),
                "refresh_token": format!("rt_{}", id),
                "expires_in": 3600,
                "expiry_timestamp": chrono::Utc::now().timestamp() + 3600,
                "token_type": "Bearer"
            },
            "disabled": true,
            "proxy_disabled": false,
            "validation_blocked": false,
            "protected_models": [],
            "created_at": chrono::Utc::now().timestamp(),
            "last_used": chrono::Utc::now().timestamp()
        });

        let path = dir.join("accounts").join(format!("{}.json", id));
        fs::write(&path, serde_json::to_string_pretty(&account_json).unwrap()).unwrap();
        path
    }

    /// Create a validation-blocked account file
    fn create_validation_blocked_account(
        dir: &PathBuf,
        id: &str,
        email: &str,
        block_until: i64,
    ) -> PathBuf {
        let account_json = serde_json::json!({
            "id": id,
            "email": email,
            "name": "Blocked User",
            "token": {
                "access_token": format!("at_{}", id),
                "refresh_token": format!("rt_{}", id),
                "expires_in": 3600,
                "expiry_timestamp": chrono::Utc::now().timestamp() + 3600,
                "token_type": "Bearer"
            },
            "disabled": false,
            "proxy_disabled": false,
            "validation_blocked": true,
            "validation_blocked_until": block_until,
            "validation_blocked_reason": "VALIDATION_REQUIRED",
            "protected_models": [],
            "created_at": chrono::Utc::now().timestamp(),
            "last_used": chrono::Utc::now().timestamp()
        });

        let path = dir.join("accounts").join(format!("{}.json", id));
        fs::write(&path, serde_json::to_string_pretty(&account_json).unwrap()).unwrap();
        path
    }

    #[tokio::test]
    async fn test_load_accounts_basic() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");
        create_account_file(&dir.path, "acc2", "user2@test.com");

        let tm = TokenManager::new(dir.path.clone());
        let count = tm.load_accounts().await.unwrap();

        assert_eq!(count, 2);
        assert_eq!(tm.len(), 2);
        assert!(tm.tokens.contains_key("acc1"));
        assert!(tm.tokens.contains_key("acc2"));
    }

    #[tokio::test]
    async fn test_load_accounts_skips_disabled() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");
        create_disabled_account_file(&dir.path, "acc2", "disabled@test.com");

        let tm = TokenManager::new(dir.path.clone());
        let count = tm.load_accounts().await.unwrap();

        assert_eq!(count, 1);
        assert!(tm.tokens.contains_key("acc1"));
        assert!(!tm.tokens.contains_key("acc2"));
    }

    #[tokio::test]
    async fn test_load_accounts_skips_proxy_disabled() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");

        // Create proxy_disabled account
        let account_json = serde_json::json!({
            "id": "acc2",
            "email": "proxy_disabled@test.com",
            "name": "Proxy Disabled",
            "token": {
                "access_token": "at_acc2",
                "refresh_token": "rt_acc2",
                "expires_in": 3600,
                "expiry_timestamp": chrono::Utc::now().timestamp() + 3600,
                "token_type": "Bearer"
            },
            "disabled": false,
            "proxy_disabled": true,
            "proxy_disabled_reason": "manual",
            "validation_blocked": false,
            "protected_models": [],
            "created_at": chrono::Utc::now().timestamp(),
            "last_used": chrono::Utc::now().timestamp()
        });
        let path = dir.path.join("accounts").join("acc2.json");
        fs::write(&path, serde_json::to_string_pretty(&account_json).unwrap()).unwrap();

        let tm = TokenManager::new(dir.path.clone());
        let count = tm.load_accounts().await.unwrap();

        assert_eq!(count, 1);
        assert!(!tm.tokens.contains_key("acc2"));
    }

    #[tokio::test]
    async fn test_validation_blocked_still_active() {
        let dir = TestDataDir::new();
        let future_time = chrono::Utc::now().timestamp() + 3600; // 1 hour from now
        create_validation_blocked_account(&dir.path, "blocked1", "blocked@test.com", future_time);

        let tm = TokenManager::new(dir.path.clone());
        let count = tm.load_accounts().await.unwrap();

        assert_eq!(count, 0);
        assert!(!tm.tokens.contains_key("blocked1"));
    }

    #[tokio::test]
    async fn test_validation_blocked_auto_recovery() {
        let dir = TestDataDir::new();
        let past_time = chrono::Utc::now().timestamp() - 60; // 1 minute ago
        let path =
            create_validation_blocked_account(&dir.path, "recover1", "recover@test.com", past_time);

        let tm = TokenManager::new(dir.path.clone());
        let count = tm.load_accounts().await.unwrap();

        // Account should be recovered and loaded
        assert_eq!(count, 1);
        assert!(tm.tokens.contains_key("recover1"));

        // Verify the file was updated on disk
        let content = fs::read_to_string(&path).unwrap();
        let account: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            account.get("validation_blocked").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert!(
            account.get("validation_blocked_reason").unwrap().is_null()
        );
    }

    #[tokio::test]
    async fn test_reload_account() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        assert_eq!(tm.len(), 1);

        // Modify the account on disk
        let account_json = serde_json::json!({
            "id": "acc1",
            "email": "updated@test.com",
            "name": "Updated User",
            "token": {
                "access_token": "new_at",
                "refresh_token": "new_rt",
                "expires_in": 7200,
                "expiry_timestamp": chrono::Utc::now().timestamp() + 7200,
                "token_type": "Bearer"
            },
            "disabled": false,
            "proxy_disabled": false,
            "validation_blocked": false,
            "protected_models": [],
            "created_at": chrono::Utc::now().timestamp(),
            "last_used": chrono::Utc::now().timestamp()
        });
        let path = dir.path.join("accounts").join("acc1.json");
        fs::write(&path, serde_json::to_string_pretty(&account_json).unwrap()).unwrap();

        tm.reload_account("acc1").await.unwrap();

        let token = tm.tokens.get("acc1").unwrap();
        assert_eq!(token.email, "updated@test.com");
        assert_eq!(token.access_token, "new_at");
    }

    #[tokio::test]
    async fn test_reload_account_removes_when_disabled() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();
        assert_eq!(tm.len(), 1);

        // Disable the account on disk
        let account_json = serde_json::json!({
            "id": "acc1",
            "email": "user1@test.com",
            "name": "Test User",
            "token": {
                "access_token": "at_acc1",
                "refresh_token": "rt_acc1",
                "expires_in": 3600,
                "expiry_timestamp": chrono::Utc::now().timestamp() + 3600,
                "token_type": "Bearer"
            },
            "disabled": true,
            "proxy_disabled": false,
            "validation_blocked": false,
            "protected_models": [],
            "created_at": chrono::Utc::now().timestamp(),
            "last_used": chrono::Utc::now().timestamp()
        });
        let path = dir.path.join("accounts").join("acc1.json");
        fs::write(&path, serde_json::to_string_pretty(&account_json).unwrap()).unwrap();

        tm.reload_account("acc1").await.unwrap();
        assert_eq!(tm.len(), 0);
    }

    #[tokio::test]
    async fn test_remove_account_cleans_all_data() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");
        create_account_file(&dir.path, "acc2", "user2@test.com");

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        // Set up associated data
        tm.health_scores.insert("acc1".to_string(), 0.8);
        tm.session_accounts
            .insert("session1".to_string(), "acc1".to_string());
        tm.session_accounts
            .insert("session2".to_string(), "acc2".to_string());
        {
            let mut preferred = tm.preferred_account_id.write().await;
            *preferred = Some("acc1".to_string());
        }

        // Remove acc1
        tm.remove_account("acc1");

        // Verify cleanup
        assert!(!tm.tokens.contains_key("acc1"));
        assert!(tm.tokens.contains_key("acc2"));
        assert!(!tm.health_scores.contains_key("acc1"));
        assert!(!tm.session_accounts.contains_key("session1"));
        assert!(tm.session_accounts.contains_key("session2"));
        assert!(tm.get_preferred_account().await.is_none());
    }

    #[tokio::test]
    async fn test_mark_success_updates_health() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        // Set initial low health
        tm.health_scores.insert("acc1".to_string(), 0.5);
        if let Some(mut token) = tm.tokens.get_mut("acc1") {
            token.health_score = 0.5;
        }

        tm.mark_success("acc1");

        let score = *tm.health_scores.get("acc1").unwrap();
        assert!((score - 0.6).abs() < f32::EPSILON);

        let token = tm.tokens.get("acc1").unwrap();
        assert!((token.health_score - 0.6).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn test_record_failure_decreases_health() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        tm.record_failure("acc1");

        let score = *tm.health_scores.get("acc1").unwrap();
        assert!((score - 0.8).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn test_health_score_capped_at_bounds() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        // Cap at 1.0
        tm.health_scores.insert("acc1".to_string(), 0.95);
        tm.mark_success("acc1");
        let score = *tm.health_scores.get("acc1").unwrap();
        assert!((score - 1.0).abs() < f32::EPSILON);

        // Cap at 0.0
        tm.health_scores.insert("acc1".to_string(), 0.1);
        tm.record_failure("acc1");
        let score = *tm.health_scores.get("acc1").unwrap();
        assert!((score - 0.0).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn test_load_accounts_empty_dir() {
        let dir = TestDataDir::new();
        let tm = TokenManager::new(dir.path.clone());
        let count = tm.load_accounts().await.unwrap();
        assert_eq!(count, 0);
        assert!(tm.is_empty());
    }

    #[tokio::test]
    async fn test_load_accounts_skips_non_json() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");

        // Create a non-JSON file
        fs::write(dir.path.join("accounts").join("readme.txt"), "not json").unwrap();

        let tm = TokenManager::new(dir.path.clone());
        let count = tm.load_accounts().await.unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_load_accounts_skips_invalid_json() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");

        // Create an invalid JSON file
        fs::write(
            dir.path.join("accounts").join("bad.json"),
            "{ invalid json }",
        )
        .unwrap();

        let tm = TokenManager::new(dir.path.clone());
        let count = tm.load_accounts().await.unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_get_account_id_by_email() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");
        create_account_file(&dir.path, "acc2", "user2@test.com");

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        assert_eq!(
            tm.get_account_id_by_email("user1@test.com"),
            Some("acc1".to_string())
        );
        assert_eq!(
            tm.get_account_id_by_email("user2@test.com"),
            Some("acc2".to_string())
        );
        assert_eq!(tm.get_account_id_by_email("nonexistent@test.com"), None);
    }

    #[tokio::test]
    async fn test_load_with_quota_data() {
        let dir = TestDataDir::new();

        let account_json = serde_json::json!({
            "id": "acc1",
            "email": "user1@test.com",
            "name": "Test User",
            "token": {
                "access_token": "at_acc1",
                "refresh_token": "rt_acc1",
                "expires_in": 3600,
                "expiry_timestamp": chrono::Utc::now().timestamp() + 3600,
                "token_type": "Bearer"
            },
            "disabled": false,
            "proxy_disabled": false,
            "validation_blocked": false,
            "protected_models": ["claude"],
            "quota": {
                "models": [
                    { "name": "gemini-2.5-flash", "percentage": 80, "reset_time": "2025-01-01T00:00:00Z" },
                    { "name": "gemini-3-pro-preview", "percentage": 50, "reset_time": "2025-01-01T00:00:00Z" }
                ],
                "last_updated": chrono::Utc::now().timestamp(),
                "is_forbidden": false,
                "subscription_tier": "PRO"
            },
            "created_at": chrono::Utc::now().timestamp(),
            "last_used": chrono::Utc::now().timestamp()
        });
        let path = dir.path.join("accounts").join("acc1.json");
        fs::write(&path, serde_json::to_string_pretty(&account_json).unwrap()).unwrap();

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        let token = tm.tokens.get("acc1").unwrap();
        assert_eq!(token.subscription_tier.as_deref(), Some("PRO"));
        assert_eq!(token.remaining_quota, Some(80));
        assert!(token.protected_models.contains("claude"));
        assert!(!token.model_quotas.is_empty());
    }

    #[tokio::test]
    async fn test_reload_clears_rate_limits() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        // Set a rate limit
        tm.rate_limit_tracker.parse_from_error(
            "acc1",
            429,
            Some("60"),
            "",
            None,
            &[60, 300],
        );
        assert!(tm.is_rate_limited("acc1", None));

        // Reload should clear rate limits
        tm.reload_account("acc1").await.unwrap();
        assert!(!tm.is_rate_limited("acc1", None));
    }

    // ===== Helper for P2C / scheduling tests =====

    /// Create a test ProxyToken directly (no disk file needed)
    fn create_test_proxy_token(
        id: &str,
        email: &str,
        tier: Option<&str>,
        health_score: f32,
        remaining_quota: Option<i32>,
        model_quotas: HashMap<String, i32>,
        protected_models: HashSet<String>,
    ) -> ProxyToken {
        ProxyToken {
            account_id: id.to_string(),
            access_token: format!("at_{}", id),
            refresh_token: format!("rt_{}", id),
            expires_in: 3600,
            timestamp: chrono::Utc::now().timestamp() + 3600,
            email: email.to_string(),
            account_path: PathBuf::from("/tmp/nonexistent"),
            project_id: None,
            subscription_tier: tier.map(|s| s.to_string()),
            remaining_quota,
            protected_models,
            health_score,
            reset_time: None,
            validation_blocked: false,
            validation_blocked_until: 0,
            model_quotas,
        }
    }

    // ===== compare_tokens sorting tests =====

    #[test]
    fn test_compare_tokens_tier_priority() {
        let ultra = create_test_proxy_token(
            "u1", "ultra@test.com", Some("ULTRA"), 1.0, Some(50), HashMap::new(), HashSet::new(),
        );
        let pro = create_test_proxy_token(
            "p1", "pro@test.com", Some("PRO"), 1.0, Some(50), HashMap::new(), HashSet::new(),
        );
        let free = create_test_proxy_token(
            "f1", "free@test.com", Some("FREE"), 1.0, Some(50), HashMap::new(), HashSet::new(),
        );

        use std::cmp::Ordering;
        assert_eq!(TokenManager::compare_tokens(&ultra, &pro, "model"), Ordering::Less);
        assert_eq!(TokenManager::compare_tokens(&pro, &free, "model"), Ordering::Less);
        assert_eq!(TokenManager::compare_tokens(&ultra, &free, "model"), Ordering::Less);
        assert_eq!(TokenManager::compare_tokens(&free, &ultra, "model"), Ordering::Greater);
    }

    #[test]
    fn test_compare_tokens_model_quota_priority() {
        let mut mq_high = HashMap::new();
        mq_high.insert("gemini-flash".to_string(), 80);
        let mut mq_low = HashMap::new();
        mq_low.insert("gemini-flash".to_string(), 20);

        let high = create_test_proxy_token(
            "h1", "high@test.com", Some("PRO"), 1.0, Some(80), mq_high, HashSet::new(),
        );
        let low = create_test_proxy_token(
            "l1", "low@test.com", Some("PRO"), 1.0, Some(20), mq_low, HashSet::new(),
        );

        use std::cmp::Ordering;
        // Same tier, higher model quota should come first
        assert_eq!(
            TokenManager::compare_tokens(&high, &low, "gemini-flash"),
            Ordering::Less
        );
        assert_eq!(
            TokenManager::compare_tokens(&low, &high, "gemini-flash"),
            Ordering::Greater
        );
    }

    #[test]
    fn test_compare_tokens_health_score_priority() {
        let high_health = create_test_proxy_token(
            "h1", "high@test.com", Some("PRO"), 1.0, Some(50), HashMap::new(), HashSet::new(),
        );
        let low_health = create_test_proxy_token(
            "l1", "low@test.com", Some("PRO"), 0.5, Some(50), HashMap::new(), HashSet::new(),
        );

        use std::cmp::Ordering;
        assert_eq!(
            TokenManager::compare_tokens(&high_health, &low_health, "model"),
            Ordering::Less
        );
    }

    #[test]
    fn test_compare_tokens_full_sorting() {
        let mut tokens = vec![
            create_test_proxy_token(
                "f1", "free@test.com", Some("FREE"), 1.0, Some(90), HashMap::new(), HashSet::new(),
            ),
            create_test_proxy_token(
                "p2", "pro_low@test.com", Some("PRO"), 0.5, Some(50), HashMap::new(), HashSet::new(),
            ),
            create_test_proxy_token(
                "p1", "pro_high@test.com", Some("PRO"), 1.0, Some(50), HashMap::new(), HashSet::new(),
            ),
            create_test_proxy_token(
                "u1", "ultra@test.com", Some("ULTRA"), 1.0, Some(10), HashMap::new(), HashSet::new(),
            ),
        ];

        tokens.sort_by(|a, b| TokenManager::compare_tokens(a, b, "model"));

        // Expected: ULTRA > PRO(high health, same quota) > PRO(low health, same quota) > FREE
        assert_eq!(tokens[0].email, "ultra@test.com");
        assert_eq!(tokens[1].email, "pro_high@test.com");
        assert_eq!(tokens[2].email, "pro_low@test.com");
        assert_eq!(tokens[3].email, "free@test.com");
    }

    // ===== P2C selection tests =====

    #[test]
    fn test_p2c_selects_from_candidates() {
        let tm = TokenManager::new(PathBuf::from("/tmp/test"));

        let t1 = create_test_proxy_token(
            "a1", "a@test.com", Some("PRO"), 0.5, Some(80), HashMap::new(), HashSet::new(),
        );
        let t2 = create_test_proxy_token(
            "b1", "b@test.com", Some("PRO"), 1.0, Some(50), HashMap::new(), HashSet::new(),
        );

        let candidates = vec![t1, t2];
        let attempted: HashSet<String> = HashSet::new();

        // With only 2 candidates, P2C always picks both and selects higher health
        for _ in 0..10 {
            let result = tm.select_with_p2c(&candidates, &attempted, "model");
            assert!(result.is_some());
            assert_eq!(result.unwrap().email, "b@test.com"); // higher health score
        }
    }

    #[test]
    fn test_p2c_skips_attempted() {
        let tm = TokenManager::new(PathBuf::from("/tmp/test"));

        let t1 = create_test_proxy_token(
            "a1", "a@test.com", Some("PRO"), 1.0, Some(80), HashMap::new(), HashSet::new(),
        );
        let t2 = create_test_proxy_token(
            "b1", "b@test.com", Some("PRO"), 1.0, Some(50), HashMap::new(), HashSet::new(),
        );

        let candidates = vec![t1, t2];
        let mut attempted: HashSet<String> = HashSet::new();
        attempted.insert("a1".to_string());

        let result = tm.select_with_p2c(&candidates, &attempted, "model");
        assert!(result.is_some());
        assert_eq!(result.unwrap().email, "b@test.com");
    }

    #[test]
    fn test_p2c_skips_protected_models() {
        let tm = TokenManager::new(PathBuf::from("/tmp/test"));

        let mut protected = HashSet::new();
        protected.insert("gemini-flash".to_string());

        let t1 = create_test_proxy_token(
            "a1", "protected@test.com", Some("PRO"), 1.0, Some(90), HashMap::new(), protected,
        );
        let t2 = create_test_proxy_token(
            "b1", "normal@test.com", Some("PRO"), 1.0, Some(50), HashMap::new(), HashSet::new(),
        );

        let candidates = vec![t1, t2];
        let attempted: HashSet<String> = HashSet::new();

        let result = tm.select_with_p2c(&candidates, &attempted, "gemini-flash");
        assert!(result.is_some());
        assert_eq!(result.unwrap().email, "normal@test.com");
    }

    #[test]
    fn test_p2c_single_candidate() {
        let tm = TokenManager::new(PathBuf::from("/tmp/test"));

        let t1 = create_test_proxy_token(
            "a1", "single@test.com", Some("PRO"), 1.0, Some(50), HashMap::new(), HashSet::new(),
        );

        let candidates = vec![t1];
        let attempted: HashSet<String> = HashSet::new();

        let result = tm.select_with_p2c(&candidates, &attempted, "model");
        assert!(result.is_some());
        assert_eq!(result.unwrap().email, "single@test.com");
    }

    #[test]
    fn test_p2c_empty_candidates() {
        let tm = TokenManager::new(PathBuf::from("/tmp/test"));
        let candidates: Vec<ProxyToken> = vec![];
        let attempted: HashSet<String> = HashSet::new();

        let result = tm.select_with_p2c(&candidates, &attempted, "model");
        assert!(result.is_none());
    }

    #[test]
    fn test_p2c_all_attempted() {
        let tm = TokenManager::new(PathBuf::from("/tmp/test"));

        let t1 = create_test_proxy_token(
            "a1", "a@test.com", Some("PRO"), 1.0, Some(80), HashMap::new(), HashSet::new(),
        );
        let t2 = create_test_proxy_token(
            "b1", "b@test.com", Some("PRO"), 1.0, Some(50), HashMap::new(), HashSet::new(),
        );

        let candidates = vec![t1, t2];
        let mut attempted: HashSet<String> = HashSet::new();
        attempted.insert("a1".to_string());
        attempted.insert("b1".to_string());

        let result = tm.select_with_p2c(&candidates, &attempted, "model");
        assert!(result.is_none());
    }

    // ===== get_token integration tests =====

    #[tokio::test]
    async fn test_get_token_basic() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");
        create_account_file(&dir.path, "acc2", "user2@test.com");

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        let result = tm.get_token("gemini-flash", None).await;
        assert!(result.is_ok());
        let token = result.unwrap();
        assert!(!token.access_token.is_empty());
    }

    #[tokio::test]
    async fn test_get_token_empty_pool() {
        let dir = TestDataDir::new();
        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        let result = tm.get_token("gemini-flash", None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Token pool is empty"));
    }

    #[tokio::test]
    async fn test_get_token_preferred_account() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");
        create_account_file(&dir.path, "acc2", "user2@test.com");

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        // Set preferred account
        tm.set_preferred_account(Some("acc1".to_string())).await;

        // Should always return the preferred account
        for _ in 0..5 {
            let result = tm.get_token("gemini-flash", None).await;
            assert!(result.is_ok());
            assert_eq!(result.unwrap().account_id, "acc1");
        }
    }

    #[tokio::test]
    async fn test_get_token_preferred_account_rate_limited_falls_back() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");
        create_account_file(&dir.path, "acc2", "user2@test.com");

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        // Set preferred account and rate-limit it
        tm.set_preferred_account(Some("acc1".to_string())).await;
        tm.rate_limit_tracker.parse_from_error("acc1", 429, Some("60"), "", None, &[60, 300]);

        // Should fall back to acc2
        let result = tm.get_token("gemini-flash", None).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().account_id, "acc2");
    }

    #[tokio::test]
    async fn test_get_token_session_stickiness_balance() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");
        create_account_file(&dir.path, "acc2", "user2@test.com");

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        // Default mode is Balance
        let session_id = "test-session-123";

        // First call binds a session
        let result1 = tm.get_token("gemini-flash", Some(session_id)).await;
        assert!(result1.is_ok());
        let first_account = result1.unwrap().account_id;

        // Verify session binding was created
        assert!(tm.session_accounts.contains_key(session_id));

        // Second call should reuse the same account (sticky)
        let result2 = tm.get_token("gemini-flash", Some(session_id)).await;
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap().account_id, first_account);
    }

    #[tokio::test]
    async fn test_get_token_balance_mode_switches_on_rate_limit() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");
        create_account_file(&dir.path, "acc2", "user2@test.com");

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        let session_id = "test-session-456";

        // First call binds session
        let result1 = tm.get_token("gemini-flash", Some(session_id)).await;
        assert!(result1.is_ok());
        let first_account = result1.unwrap().account_id;

        // Rate-limit the bound account
        tm.rate_limit_tracker.parse_from_error(
            &first_account,
            429,
            Some("60"),
            "",
            None,
            &[60, 300],
        );

        // Balance mode: should switch to the other account
        let result2 = tm.get_token("gemini-flash", Some(session_id)).await;
        assert!(result2.is_ok());
        assert_ne!(result2.unwrap().account_id, first_account);
    }

    #[tokio::test]
    async fn test_get_token_performance_first_no_stickiness() {
        let dir = TestDataDir::new();
        create_account_file(&dir.path, "acc1", "user1@test.com");
        create_account_file(&dir.path, "acc2", "user2@test.com");

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        // Set PerformanceFirst mode
        tm.update_sticky_config(StickySessionConfig {
            mode: SchedulingMode::PerformanceFirst,
            max_wait_seconds: 60,
        })
        .await;

        let session_id = "test-session-789";

        // Call with session_id
        let _result = tm.get_token("gemini-flash", Some(session_id)).await;

        // PerformanceFirst should NOT create session bindings
        assert!(!tm.session_accounts.contains_key(session_id));
    }

    #[tokio::test]
    async fn test_get_token_skips_quota_protected() {
        let dir = TestDataDir::new();

        // Create account with protected model (using normalized standard ID)
        let account_json = serde_json::json!({
            "id": "acc1",
            "email": "protected@test.com",
            "name": "Protected User",
            "token": {
                "access_token": "at_acc1",
                "refresh_token": "rt_acc1",
                "expires_in": 3600,
                "expiry_timestamp": chrono::Utc::now().timestamp() + 3600,
                "token_type": "Bearer"
            },
            "disabled": false,
            "proxy_disabled": false,
            "validation_blocked": false,
            "protected_models": ["gemini-3-flash"],
            "created_at": chrono::Utc::now().timestamp(),
            "last_used": chrono::Utc::now().timestamp()
        });
        let path = dir.path.join("accounts").join("acc1.json");
        fs::write(&path, serde_json::to_string_pretty(&account_json).unwrap()).unwrap();

        create_account_file(&dir.path, "acc2", "normal@test.com");

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        // "gemini-flash" normalizes to "gemini-3-flash" which is in acc1's protected_models
        let result = tm.get_token("gemini-flash", None).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().account_id, "acc2");
    }

    /// Helper: create an account file with quota data for quota protection tests
    fn create_account_with_quota(
        dir: &PathBuf,
        id: &str,
        email: &str,
        models: Vec<(&str, i32)>,
        protected_models: Vec<&str>,
    ) -> PathBuf {
        let model_quotas: Vec<serde_json::Value> = models
            .iter()
            .map(|(name, pct)| {
                serde_json::json!({
                    "name": name,
                    "percentage": pct,
                    "reset_time": "2025-01-01T00:00:00Z"
                })
            })
            .collect();

        let protected: Vec<serde_json::Value> = protected_models
            .iter()
            .map(|m| serde_json::Value::String(m.to_string()))
            .collect();

        let account_json = serde_json::json!({
            "id": id,
            "email": email,
            "name": "Test User",
            "token": {
                "access_token": format!("at_{}", id),
                "refresh_token": format!("rt_{}", id),
                "expires_in": 3600,
                "expiry_timestamp": chrono::Utc::now().timestamp() + 3600,
                "token_type": "Bearer"
            },
            "disabled": false,
            "proxy_disabled": false,
            "validation_blocked": false,
            "protected_models": protected,
            "quota": {
                "models": model_quotas,
                "last_updated": chrono::Utc::now().timestamp(),
                "is_forbidden": false,
                "subscription_tier": "PRO"
            },
            "created_at": chrono::Utc::now().timestamp(),
            "last_used": chrono::Utc::now().timestamp()
        });

        let path = dir.join("accounts").join(format!("{}.json", id));
        fs::write(&path, serde_json::to_string_pretty(&account_json).unwrap()).unwrap();
        path
    }

    #[tokio::test]
    async fn test_trigger_quota_protection_adds_model() {
        let dir = TestDataDir::new();
        let path = create_account_with_quota(
            &dir.path,
            "acc1",
            "user@test.com",
            vec![("gemini-2.5-flash", 5)],
            vec![],
        );

        let tm = TokenManager::new(dir.path.clone());

        let content = fs::read_to_string(&path).unwrap();
        let mut account_json: serde_json::Value = serde_json::from_str(&content).unwrap();

        let changed = tm
            .trigger_quota_protection(&mut account_json, "acc1", &path, 5, 10, "gemini-3-flash")
            .await
            .unwrap();

        assert!(changed);

        // Verify in JSON
        let protected = account_json["protected_models"].as_array().unwrap();
        assert!(protected.iter().any(|m| m.as_str() == Some("gemini-3-flash")));

        // Verify persisted to disk
        let disk_content = fs::read_to_string(&path).unwrap();
        let disk_json: serde_json::Value = serde_json::from_str(&disk_content).unwrap();
        let disk_protected = disk_json["protected_models"].as_array().unwrap();
        assert!(disk_protected.iter().any(|m| m.as_str() == Some("gemini-3-flash")));
    }

    #[tokio::test]
    async fn test_trigger_quota_protection_idempotent() {
        let dir = TestDataDir::new();
        let path = create_account_with_quota(
            &dir.path,
            "acc1",
            "user@test.com",
            vec![("gemini-2.5-flash", 5)],
            vec!["gemini-3-flash"],
        );

        let tm = TokenManager::new(dir.path.clone());

        let content = fs::read_to_string(&path).unwrap();
        let mut account_json: serde_json::Value = serde_json::from_str(&content).unwrap();

        // Already protected - should not change
        let changed = tm
            .trigger_quota_protection(&mut account_json, "acc1", &path, 5, 10, "gemini-3-flash")
            .await
            .unwrap();

        assert!(!changed);
    }

    #[tokio::test]
    async fn test_restore_quota_protection_removes_model() {
        let dir = TestDataDir::new();
        let path = create_account_with_quota(
            &dir.path,
            "acc1",
            "user@test.com",
            vec![("gemini-2.5-flash", 80)],
            vec!["gemini-3-flash"],
        );

        let tm = TokenManager::new(dir.path.clone());

        let content = fs::read_to_string(&path).unwrap();
        let mut account_json: serde_json::Value = serde_json::from_str(&content).unwrap();

        let changed = tm
            .restore_quota_protection(&mut account_json, "acc1", &path, "gemini-3-flash")
            .await
            .unwrap();

        assert!(changed);

        // Verify removed from JSON
        let protected = account_json["protected_models"].as_array().unwrap();
        assert!(!protected.iter().any(|m| m.as_str() == Some("gemini-3-flash")));

        // Verify persisted to disk
        let disk_content = fs::read_to_string(&path).unwrap();
        let disk_json: serde_json::Value = serde_json::from_str(&disk_content).unwrap();
        let disk_protected = disk_json["protected_models"].as_array().unwrap();
        assert!(!disk_protected.iter().any(|m| m.as_str() == Some("gemini-3-flash")));
    }

    #[tokio::test]
    async fn test_restore_quota_protection_noop_when_not_protected() {
        let dir = TestDataDir::new();
        let path = create_account_with_quota(
            &dir.path,
            "acc1",
            "user@test.com",
            vec![("gemini-2.5-flash", 80)],
            vec![],
        );

        let tm = TokenManager::new(dir.path.clone());

        let content = fs::read_to_string(&path).unwrap();
        let mut account_json: serde_json::Value = serde_json::from_str(&content).unwrap();

        let changed = tm
            .restore_quota_protection(&mut account_json, "acc1", &path, "gemini-3-flash")
            .await
            .unwrap();

        assert!(!changed);
    }

    #[tokio::test]
    async fn test_trigger_updates_in_memory_token() {
        let dir = TestDataDir::new();
        let path = create_account_with_quota(
            &dir.path,
            "acc1",
            "user@test.com",
            vec![("gemini-2.5-flash", 5)],
            vec![],
        );

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        let content = fs::read_to_string(&path).unwrap();
        let mut account_json: serde_json::Value = serde_json::from_str(&content).unwrap();

        tm.trigger_quota_protection(&mut account_json, "acc1", &path, 5, 10, "gemini-3-flash")
            .await
            .unwrap();

        // Verify in-memory token was updated
        let token = tm.tokens.get("acc1").unwrap();
        assert!(token.protected_models.contains("gemini-3-flash"));
    }

    #[tokio::test]
    async fn test_restore_updates_in_memory_token() {
        let dir = TestDataDir::new();
        let path = create_account_with_quota(
            &dir.path,
            "acc1",
            "user@test.com",
            vec![("gemini-2.5-flash", 80)],
            vec!["gemini-3-flash"],
        );

        let tm = TokenManager::new(dir.path.clone());
        tm.load_accounts().await.unwrap();

        let content = fs::read_to_string(&path).unwrap();
        let mut account_json: serde_json::Value = serde_json::from_str(&content).unwrap();

        tm.restore_quota_protection(&mut account_json, "acc1", &path, "gemini-3-flash")
            .await
            .unwrap();

        // Verify in-memory token was updated
        let token = tm.tokens.get("acc1").unwrap();
        assert!(!token.protected_models.contains("gemini-3-flash"));
    }

    #[tokio::test]
    async fn test_check_and_restore_quota_migration() {
        let dir = TestDataDir::new();

        // Create account with old-style account-level quota protection
        let account_json = serde_json::json!({
            "id": "acc1",
            "email": "migrated@test.com",
            "name": "Migrated User",
            "token": {
                "access_token": "at_acc1",
                "refresh_token": "rt_acc1",
                "expires_in": 3600,
                "expiry_timestamp": chrono::Utc::now().timestamp() + 3600,
                "token_type": "Bearer"
            },
            "disabled": false,
            "proxy_disabled": true,
            "proxy_disabled_reason": "quota_protection",
            "proxy_disabled_at": chrono::Utc::now().timestamp(),
            "validation_blocked": false,
            "protected_models": [],
            "quota": {
                "models": [
                    {"name": "gemini-2.5-flash", "percentage": 5, "reset_time": "2025-01-01T00:00:00Z"},
                    {"name": "gemini-2.5-pro", "percentage": 80, "reset_time": "2025-01-01T00:00:00Z"}
                ],
                "last_updated": chrono::Utc::now().timestamp(),
                "is_forbidden": false,
                "subscription_tier": "PRO"
            },
            "created_at": chrono::Utc::now().timestamp(),
            "last_used": chrono::Utc::now().timestamp()
        });

        let path = dir.path.join("accounts").join("acc1.json");
        fs::write(&path, serde_json::to_string_pretty(&account_json).unwrap()).unwrap();

        let tm = TokenManager::new(dir.path.clone());

        let content = fs::read_to_string(&path).unwrap();
        let mut json: serde_json::Value = serde_json::from_str(&content).unwrap();

        let config = crate::models::config::QuotaProtectionConfig {
            enabled: true,
            threshold_percentage: 10,
            monitored_models: vec![
                "gemini-3-flash".to_string(),
                "gemini-3-pro-high".to_string(),
            ],
        };

        let quota = json.get("quota").unwrap().clone();
        tm.check_and_restore_quota(&mut json, &path, &quota, &config)
            .await;

        // proxy_disabled should be cleared
        assert_eq!(json["proxy_disabled"].as_bool(), Some(false));
        assert!(json["proxy_disabled_reason"].is_null());
        assert!(json["proxy_disabled_at"].is_null());

        // Verify persisted to disk
        let disk_content = fs::read_to_string(&path).unwrap();
        let disk_json: serde_json::Value = serde_json::from_str(&disk_content).unwrap();
        assert_eq!(disk_json["proxy_disabled"].as_bool(), Some(false));
    }

    #[tokio::test]
    async fn test_get_model_quota_from_json() {
        let dir = TestDataDir::new();
        let path = create_account_with_quota(
            &dir.path,
            "acc1",
            "user@test.com",
            vec![
                ("gemini-2.5-flash", 42),
                ("gemini-2.5-pro", 88),
            ],
            vec![],
        );

        // "gemini-3-flash" is the standard ID for flash models
        let quota = TokenManager::get_model_quota_from_json_for_test(&path, "gemini-3-flash");
        assert_eq!(quota, Some(42));

        let quota = TokenManager::get_model_quota_from_json_for_test(&path, "gemini-3-pro-high");
        assert_eq!(quota, Some(88));

        let quota = TokenManager::get_model_quota_from_json_for_test(&path, "nonexistent");
        assert_eq!(quota, None);
    }

}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;
    use std::time::{Duration, SystemTime};

    use crate::proxy::rate_limit::RateLimitReason;

    /// Build a minimal ProxyToken for testing (no disk dependency)
    fn make_proxy_token(account_id: &str, email: &str) -> ProxyToken {
        ProxyToken {
            account_id: account_id.to_string(),
            access_token: format!("at_{}", account_id),
            refresh_token: format!("rt_{}", account_id),
            expires_in: 3600,
            timestamp: 0,
            email: email.to_string(),
            account_path: PathBuf::from("/tmp/fake"),
            project_id: None,
            subscription_tier: None,
            remaining_quota: None,
            protected_models: HashSet::new(),
            health_score: 1.0,
            reset_time: None,
            validation_blocked: false,
            validation_blocked_until: 0,
            model_quotas: HashMap::new(),
        }
    }

    // **Feature: kiro-ai-gateway, Property 17: 账号删除完整性**
    // **Validates: Requirements 1.12**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_account_deletion_integrity(
            target_id in "[a-z0-9]{4,12}",
            other_id in "[a-z0-9]{4,12}",
            session_count in 0usize..5,
            health in 0.0f32..=1.0f32,
        ) {
            // Ensure target and other IDs are different
            prop_assume!(target_id != other_id);

            // Create a TokenManager with a dummy data dir (we won't touch disk)
            let tm = TokenManager::new(PathBuf::from("/tmp/prop_test_dummy"));

            // --- Populate all data structures for the target account ---

            // 1. Token pool
            tm.tokens.insert(target_id.clone(), make_proxy_token(&target_id, &format!("{}@test.com", target_id)));
            tm.tokens.insert(other_id.clone(), make_proxy_token(&other_id, &format!("{}@test.com", other_id)));

            // 2. Health scores
            tm.health_scores.insert(target_id.clone(), health);
            tm.health_scores.insert(other_id.clone(), 0.9);

            // 3. Session bindings pointing to the target account
            let mut target_sessions = Vec::new();
            for i in 0..session_count {
                let sid = format!("sid-target-{}", i);
                tm.session_accounts.insert(sid.clone(), target_id.clone());
                target_sessions.push(sid);
            }
            // Also add a session for the other account
            let other_session = "sid-other-0".to_string();
            tm.session_accounts.insert(other_session.clone(), other_id.clone());

            // 4. Rate limit records (account-level)
            let future = SystemTime::now() + Duration::from_secs(3600);
            tm.rate_limit_tracker.set_lockout_until(
                &target_id,
                future,
                RateLimitReason::QuotaExhausted,
                None,
            );

            // --- Perform deletion ---
            tm.remove_account(&target_id);

            // --- Verify: target account is gone from ALL data structures ---

            // Token pool
            prop_assert!(
                !tm.tokens.contains_key(&target_id),
                "Deleted account {} still in token pool", target_id
            );

            // Health scores
            prop_assert!(
                !tm.health_scores.contains_key(&target_id),
                "Deleted account {} still in health scores", target_id
            );

            // Session bindings: no session should point to the deleted account
            for entry in tm.session_accounts.iter() {
                prop_assert!(
                    entry.value() != &target_id,
                    "Session {} still bound to deleted account {}", entry.key(), target_id
                );
            }

            // Rate limit records (account-level)
            prop_assert!(
                !tm.rate_limit_tracker.is_rate_limited(&target_id, None),
                "Deleted account {} still has rate limit records", target_id
            );

            // --- Verify: other account data is untouched ---
            prop_assert!(
                tm.tokens.contains_key(&other_id),
                "Other account {} was incorrectly removed", other_id
            );
            prop_assert!(
                tm.health_scores.contains_key(&other_id),
                "Other account {} health score was incorrectly removed", other_id
            );
            prop_assert!(
                tm.session_accounts.contains_key(&other_session),
                "Other account session was incorrectly removed"
            );
        }
    }

    // **Feature: kiro-ai-gateway, Property 11: 配额保护触发与恢复对称性**
    // **Validates: Requirements 4.5, 4.6**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_quota_protection_trigger_restore_symmetry(
            threshold in 1i32..99,
            // quota_below is a percentage at or below the threshold
            quota_below_offset in 0i32..50,
            // quota_above is a percentage strictly above the threshold
            quota_above_offset in 1i32..50,
            model_name in "[a-z]{3,10}(-[a-z0-9]{1,5}){0,2}",
            account_id in "[a-z0-9]{4,12}",
        ) {
            let quota_below = (threshold - quota_below_offset).max(0);
            let _quota_above = (threshold + quota_above_offset).min(100);

            // --- Setup: create a temp data dir with an account file ---
            let dir_path = std::env::temp_dir().join(format!(
                "kiro_prop11_test_{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(dir_path.join("accounts")).unwrap();

            let now = chrono::Utc::now().timestamp();
            let account_json_val = serde_json::json!({
                "id": account_id,
                "email": format!("{}@test.com", account_id),
                "name": "Test User",
                "token": {
                    "access_token": format!("at_{}", account_id),
                    "refresh_token": format!("rt_{}", account_id),
                    "expires_in": 3600,
                    "expiry_timestamp": now + 3600,
                    "token_type": "Bearer"
                },
                "disabled": false,
                "proxy_disabled": false,
                "validation_blocked": false,
                "protected_models": [],
                "quota": {
                    "models": [],
                    "last_updated": now,
                    "is_forbidden": false,
                    "subscription_tier": "PRO"
                },
                "created_at": now,
                "last_used": now
            });

            let path = dir_path.join("accounts").join(format!("{}.json", account_id));
            std::fs::write(&path, serde_json::to_string_pretty(&account_json_val).unwrap()).unwrap();

            let rt = tokio::runtime::Runtime::new().unwrap();
            let tm = TokenManager::new(dir_path.clone());

            // Insert an in-memory token so trigger/restore can update it
            let mut proxy_token = make_proxy_token(&account_id, &format!("{}@test.com", account_id));
            proxy_token.account_path = path.clone();
            tm.tokens.insert(account_id.clone(), proxy_token);

            let mut account_json = account_json_val.clone();

            // === Phase 1: Trigger protection (quota below threshold) ===
            let trigger_result = rt.block_on(tm.trigger_quota_protection(
                &mut account_json,
                &account_id,
                &path,
                quota_below,
                threshold,
                &model_name,
            )).unwrap();

            // Model should be newly added
            prop_assert!(
                trigger_result,
                "trigger_quota_protection should return true when model is newly protected"
            );

            // Verify model is in protected_models (in-memory JSON)
            let protected = account_json["protected_models"].as_array().unwrap();
            prop_assert!(
                protected.iter().any(|m| m.as_str() == Some(&model_name)),
                "Model '{}' should be in protected_models after trigger", model_name
            );

            // Verify in-memory token was updated
            {
                let token = tm.tokens.get(&account_id).unwrap();
                prop_assert!(
                    token.protected_models.contains(&model_name),
                    "In-memory token should contain '{}' in protected_models after trigger", model_name
                );
            }

            // Verify persisted to disk
            let disk_content = std::fs::read_to_string(&path).unwrap();
            let disk_json: serde_json::Value = serde_json::from_str(&disk_content).unwrap();
            let disk_protected = disk_json["protected_models"].as_array().unwrap();
            prop_assert!(
                disk_protected.iter().any(|m| m.as_str() == Some(&model_name)),
                "Model '{}' should be persisted to disk after trigger", model_name
            );

            // === Phase 1b: Trigger again (idempotent) ===
            let trigger_again = rt.block_on(tm.trigger_quota_protection(
                &mut account_json,
                &account_id,
                &path,
                quota_below,
                threshold,
                &model_name,
            )).unwrap();

            prop_assert!(
                !trigger_again,
                "trigger_quota_protection should return false when model is already protected"
            );

            // === Phase 2: Restore protection (quota recovered) ===
            let restore_result = rt.block_on(tm.restore_quota_protection(
                &mut account_json,
                &account_id,
                &path,
                &model_name,
            )).unwrap();

            // Model should be removed
            prop_assert!(
                restore_result,
                "restore_quota_protection should return true when model is removed"
            );

            // Verify model is NOT in protected_models (in-memory JSON)
            let protected_after = account_json["protected_models"].as_array().unwrap();
            prop_assert!(
                !protected_after.iter().any(|m| m.as_str() == Some(&model_name)),
                "Model '{}' should NOT be in protected_models after restore", model_name
            );

            // Verify in-memory token was updated
            {
                let token = tm.tokens.get(&account_id).unwrap();
                prop_assert!(
                    !token.protected_models.contains(&model_name),
                    "In-memory token should NOT contain '{}' after restore", model_name
                );
            }

            // Verify persisted to disk
            let disk_content2 = std::fs::read_to_string(&path).unwrap();
            let disk_json2: serde_json::Value = serde_json::from_str(&disk_content2).unwrap();
            let disk_protected2 = disk_json2["protected_models"].as_array().unwrap();
            prop_assert!(
                !disk_protected2.iter().any(|m| m.as_str() == Some(&model_name)),
                "Model '{}' should NOT be persisted to disk after restore", model_name
            );

            // === Phase 2b: Restore again (noop) ===
            let restore_again = rt.block_on(tm.restore_quota_protection(
                &mut account_json,
                &account_id,
                &path,
                &model_name,
            )).unwrap();

            prop_assert!(
                !restore_again,
                "restore_quota_protection should return false when model is not protected"
            );

            // Cleanup
            let _ = std::fs::remove_dir_all(&dir_path);
        }
    }

    // **Feature: kiro-ai-gateway, Property 21: Validation Blocked 自动恢复**
    // **Validates: Requirements 1.7**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_validation_blocked_auto_recovery(
            // How many seconds ago the block expired (1..86400 = 1 second to 1 day ago)
            expired_ago in 1i64..86400,
            reason in "[A-Z_]{4,20}",
        ) {
            // --- Setup: create a temp data dir with an expired validation-blocked account ---
            let now = chrono::Utc::now().timestamp();
            let block_until = now - expired_ago; // already expired

            let dir_path = std::env::temp_dir().join(format!(
                "kiro_prop21_test_{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(dir_path.join("accounts")).unwrap();

            let account_id = "vb_recovery_test";
            let email = "blocked@test.com";

            let account_json = serde_json::json!({
                "id": account_id,
                "email": email,
                "name": "Blocked User",
                "token": {
                    "access_token": "at_vb",
                    "refresh_token": "rt_vb",
                    "expires_in": 3600,
                    "expiry_timestamp": now + 3600,
                    "token_type": "Bearer"
                },
                "disabled": false,
                "proxy_disabled": false,
                "validation_blocked": true,
                "validation_blocked_until": block_until,
                "validation_blocked_reason": reason.clone(),
                "protected_models": [],
                "created_at": now,
                "last_used": now
            });

            let path = dir_path.join("accounts").join(format!("{}.json", account_id));
            std::fs::write(&path, serde_json::to_string_pretty(&account_json).unwrap()).unwrap();

            // --- Act: load accounts via TokenManager ---
            let rt = tokio::runtime::Runtime::new().unwrap();
            let tm = TokenManager::new(dir_path.clone());
            let count = rt.block_on(tm.load_accounts()).unwrap();

            // --- Assert 1: account was loaded (not skipped) ---
            prop_assert_eq!(count, 1, "Expired validation-blocked account should be loaded");
            prop_assert!(
                tm.tokens.contains_key(account_id),
                "Account should be present in token pool after recovery"
            );

            // --- Assert 2: in-memory ProxyToken has validation_blocked cleared ---
            let token = tm.tokens.get(account_id).unwrap();
            prop_assert!(
                !token.validation_blocked,
                "In-memory token should have validation_blocked = false after recovery"
            );

            // --- Assert 3: on-disk JSON has validation_blocked cleared ---
            let disk_content = std::fs::read_to_string(&path).unwrap();
            let disk_account: serde_json::Value = serde_json::from_str(&disk_content).unwrap();

            prop_assert_eq!(
                disk_account.get("validation_blocked").and_then(|v| v.as_bool()),
                Some(false),
                "On-disk validation_blocked should be false after recovery"
            );
            prop_assert!(
                disk_account.get("validation_blocked_until").map_or(true, |v| v.is_null()),
                "On-disk validation_blocked_until should be null after recovery"
            );
            prop_assert!(
                disk_account.get("validation_blocked_reason").map_or(true, |v| v.is_null()),
                "On-disk validation_blocked_reason should be null after recovery"
            );

            // Cleanup
            let _ = std::fs::remove_dir_all(&dir_path);
        }
    }
}

