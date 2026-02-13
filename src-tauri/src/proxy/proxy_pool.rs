// Proxy pool manager module
//
// Manages a pool of upstream proxies with multiple selection strategies,
// health checking, account-level proxy binding, and automatic failover.
//
// Requirements: 6.9, 6.10, 6.16

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use dashmap::DashMap;
use futures::{stream, StreamExt};
use reqwest::Client;
use tokio::sync::RwLock;

use crate::models::config::{ProxyEntry, ProxyPoolConfig, ProxySelectionStrategy};
use crate::proxy::config::normalize_proxy_url;

// ============================================================================
// Global singleton
// ============================================================================

static GLOBAL_PROXY_POOL: OnceLock<Arc<ProxyPoolManager>> = OnceLock::new();

/// Get the global proxy pool manager (if initialized).
pub fn get_global_proxy_pool() -> Option<Arc<ProxyPoolManager>> {
    GLOBAL_PROXY_POOL.get().cloned()
}

/// Initialize the global proxy pool manager singleton.
/// Returns the `Arc<ProxyPoolManager>` regardless of whether it was freshly
/// created or already existed.
pub fn init_global_proxy_pool(config: Arc<RwLock<ProxyPoolConfig>>) -> Arc<ProxyPoolManager> {
    let manager = Arc::new(ProxyPoolManager::new(config));
    let _ = GLOBAL_PROXY_POOL.set(manager.clone());
    manager
}

// ============================================================================
// PoolProxyConfig – lightweight handle passed to reqwest
// ============================================================================

/// Proxy configuration used to build a `reqwest::Client`.
#[derive(Clone)]
pub struct PoolProxyConfig {
    pub proxy: reqwest::Proxy,
    pub entry_id: String,
}

// ============================================================================
// ProxyPoolManager
// ============================================================================

/// Manages a pool of upstream HTTP proxies with strategy-based selection,
/// per-account binding, health checking, and automatic failover.
pub struct ProxyPoolManager {
    config: Arc<RwLock<ProxyPoolConfig>>,

    /// Usage counter per proxy (proxy_id → request count).
    usage_counter: Arc<DashMap<String, usize>>,

    /// Account-to-proxy bindings (account_id → proxy_id).
    account_bindings: Arc<DashMap<String, String>>,

    /// Round-robin index for the `RoundRobin` strategy.
    round_robin_index: Arc<AtomicUsize>,
}

impl ProxyPoolManager {
    /// Create a new manager, loading persisted bindings from the config.
    pub fn new(config: Arc<RwLock<ProxyPoolConfig>>) -> Self {
        let account_bindings = Arc::new(DashMap::new());

        // Load persisted bindings (non-async – use try_read to avoid deadlock).
        if let Ok(cfg) = config.try_read() {
            for (account_id, proxy_id) in &cfg.account_bindings {
                account_bindings.insert(account_id.clone(), proxy_id.clone());
            }
            if !cfg.account_bindings.is_empty() {
                tracing::info!(
                    "[ProxyPool] Loaded {} account bindings from config",
                    cfg.account_bindings.len()
                );
            }
        }

        Self {
            config,
            usage_counter: Arc::new(DashMap::new()),
            account_bindings,
            round_robin_index: Arc::new(AtomicUsize::new(0)),
        }
    }

    // ========================================================================
    // get_effective_client – main entry point
    // ========================================================================

    /// Return an HTTP client for the given account.
    ///
    /// Resolution order:
    /// 1. Account-level proxy binding (dedicated IP).
    /// 2. Pool-level strategy selection (shared pool).
    /// 3. Global upstream proxy from `AppConfig`.
    /// 4. Direct connection (no proxy).
    pub async fn get_effective_client(
        &self,
        account_id: Option<&str>,
        timeout_secs: u64,
    ) -> Client {
        let mut builder = Client::builder().timeout(Duration::from_secs(timeout_secs));

        let proxy_opt = if let Some(acc_id) = account_id {
            self.get_proxy_for_account(acc_id).await.ok().flatten()
        } else {
            let config = self.config.read().await;
            if config.enabled {
                let res = self.select_proxy_from_pool(&config).await.ok().flatten();
                if res.is_none() {
                    tracing::warn!(
                        "[Proxy] Route: Generic Request -> No available proxy in pool, \
                         falling back to upstream or direct"
                    );
                }
                res
            } else {
                None
            }
        };

        if let Some(proxy_cfg) = proxy_opt {
            builder = builder.proxy(proxy_cfg.proxy);
        } else {
            // Fallback to the single upstream proxy from AppConfig.
            if let Ok(app_cfg) = crate::modules::config::load_app_config() {
                let up = &app_cfg.proxy.upstream_proxy;
                if up.enabled && !up.url.is_empty() {
                    if let Ok(p) = reqwest::Proxy::all(&up.url) {
                        builder = builder.proxy(p);
                    }
                }
            }
        }

        builder.build().unwrap_or_else(|_| Client::new())
    }

    // ========================================================================
    // Proxy resolution helpers
    // ========================================================================

    /// Resolve a proxy for a specific account.
    pub async fn get_proxy_for_account(
        &self,
        account_id: &str,
    ) -> Result<Option<PoolProxyConfig>, String> {
        let config = self.config.read().await;

        if !config.enabled || config.proxies.is_empty() {
            return Ok(None);
        }

        // 1. Prefer account-level binding (dedicated IP).
        if let Some(proxy) = self.get_bound_proxy(account_id, &config)? {
            tracing::info!(
                "[Proxy] Route: Account {} -> Proxy {} (Bound)",
                account_id,
                proxy.entry_id
            );
            return Ok(Some(proxy));
        }

        // 2. Fall back to pool strategy selection.
        let res = self.select_proxy_from_pool(&config).await?;
        if let Some(ref p) = res {
            tracing::info!(
                "[Proxy] Route: Account {} -> Proxy {} (Pool)",
                account_id,
                p.entry_id
            );
        }
        Ok(res)
    }

    /// Look up the proxy bound to `account_id`.
    fn get_bound_proxy(
        &self,
        account_id: &str,
        config: &ProxyPoolConfig,
    ) -> Result<Option<PoolProxyConfig>, String> {
        if let Some(proxy_id) = self.account_bindings.get(account_id) {
            if let Some(entry) = config.proxies.iter().find(|p| p.id == *proxy_id.value()) {
                if entry.enabled {
                    // Auto-failover: skip unhealthy bound proxy.
                    if config.auto_failover && !entry.is_healthy {
                        return Ok(None);
                    }
                    return Ok(Some(Self::build_proxy_config(entry)?));
                }
            }
        }
        Ok(None)
    }

    /// Select a proxy from the pool using the configured strategy.
    /// Bound proxies are excluded from the shared pool to preserve IP isolation.
    async fn select_proxy_from_pool(
        &self,
        config: &ProxyPoolConfig,
    ) -> Result<Option<PoolProxyConfig>, String> {
        let bound_ids: HashSet<String> = self
            .account_bindings
            .iter()
            .map(|kv| kv.value().clone())
            .collect();

        let healthy_proxies: Vec<_> = config
            .proxies
            .iter()
            .filter(|p| {
                if !p.enabled {
                    return false;
                }
                if config.auto_failover && !p.is_healthy {
                    return false;
                }
                // Exclude proxies already bound to specific accounts.
                if bound_ids.contains(&p.id) {
                    return false;
                }
                true
            })
            .collect();

        if healthy_proxies.is_empty() {
            return Ok(None);
        }

        let selected = match config.strategy {
            ProxySelectionStrategy::RoundRobin => self.select_round_robin(&healthy_proxies),
            ProxySelectionStrategy::Random => self.select_random(&healthy_proxies),
            ProxySelectionStrategy::Priority => self.select_by_priority(&healthy_proxies),
            ProxySelectionStrategy::LeastConnections => {
                self.select_least_connections(&healthy_proxies)
            }
            ProxySelectionStrategy::WeightedRoundRobin => self.select_weighted(&healthy_proxies),
        };

        if let Some(entry) = selected {
            *self.usage_counter.entry(entry.id.clone()).or_insert(0) += 1;
            Ok(Some(Self::build_proxy_config(entry)?))
        } else {
            Ok(None)
        }
    }

    // ========================================================================
    // Selection strategies
    // ========================================================================

    fn select_round_robin<'a>(&self, proxies: &[&'a ProxyEntry]) -> Option<&'a ProxyEntry> {
        if proxies.is_empty() {
            return None;
        }
        let index = self.round_robin_index.fetch_add(1, Ordering::Relaxed);
        Some(proxies[index % proxies.len()])
    }

    fn select_random<'a>(&self, proxies: &[&'a ProxyEntry]) -> Option<&'a ProxyEntry> {
        if proxies.is_empty() {
            return None;
        }
        use rand::seq::SliceRandom;
        let mut rng = rand::thread_rng();
        proxies.choose(&mut rng).copied()
    }

    fn select_by_priority<'a>(&self, proxies: &[&'a ProxyEntry]) -> Option<&'a ProxyEntry> {
        proxies.iter().min_by_key(|p| p.priority).copied()
    }

    fn select_least_connections<'a>(&self, proxies: &[&'a ProxyEntry]) -> Option<&'a ProxyEntry> {
        proxies
            .iter()
            .min_by_key(|p| self.usage_counter.get(&p.id).map(|v| *v).unwrap_or(0))
            .copied()
    }

    fn select_weighted<'a>(&self, proxies: &[&'a ProxyEntry]) -> Option<&'a ProxyEntry> {
        // Weighted round-robin: use priority as weight (lower priority = higher weight).
        self.select_by_priority(proxies)
    }

    // ========================================================================
    // Build reqwest::Proxy
    // ========================================================================

    fn build_proxy_config(entry: &ProxyEntry) -> Result<PoolProxyConfig, String> {
        let url = normalize_proxy_url(&entry.url);
        let mut proxy =
            reqwest::Proxy::all(&url).map_err(|e| format!("Invalid proxy URL: {}", e))?;

        if let Some(auth) = &entry.auth {
            proxy = proxy.basic_auth(&auth.username, &auth.password);
        }

        Ok(PoolProxyConfig {
            proxy,
            entry_id: entry.id.clone(),
        })
    }


    // ========================================================================
    // Account binding management
    // ========================================================================

    /// Bind an account to a specific proxy (dedicated IP).
    pub async fn bind_account_to_proxy(
        &self,
        account_id: String,
        proxy_id: String,
    ) -> Result<(), String> {
        {
            let config = self.config.read().await;
            let entry = config
                .proxies
                .iter()
                .find(|p| p.id == proxy_id)
                .ok_or_else(|| format!("Proxy {} not found", proxy_id))?;

            // Enforce max_accounts limit.
            if let Some(max) = entry.max_accounts {
                if max > 0 {
                    let current_count = self
                        .account_bindings
                        .iter()
                        .filter(|kv| *kv.value() == proxy_id)
                        .count();
                    if current_count >= max {
                        return Err(format!(
                            "Proxy {} has reached max accounts limit ({})",
                            proxy_id, max
                        ));
                    }
                }
            }
        }

        self.account_bindings
            .insert(account_id.clone(), proxy_id.clone());
        self.persist_bindings().await;

        tracing::info!(
            "[ProxyPool] Bound account {} to proxy {}",
            account_id,
            proxy_id
        );
        Ok(())
    }

    /// Remove the proxy binding for an account.
    pub async fn unbind_account_proxy(&self, account_id: &str) {
        self.account_bindings.remove(account_id);
        self.persist_bindings().await;
        tracing::info!("[ProxyPool] Unbound account {}", account_id);
    }

    /// Get the proxy ID currently bound to an account.
    pub fn get_account_binding(&self, account_id: &str) -> Option<String> {
        self.account_bindings
            .get(account_id)
            .map(|v| v.value().clone())
    }

    /// Snapshot of all account→proxy bindings.
    pub fn get_all_bindings_snapshot(&self) -> HashMap<String, String> {
        self.account_bindings
            .iter()
            .map(|kv| (kv.key().clone(), kv.value().clone()))
            .collect()
    }

    /// Persist current bindings to the config file on disk.
    async fn persist_bindings(&self) {
        let bindings = self.get_all_bindings_snapshot();

        {
            let mut config = self.config.write().await;
            config.account_bindings = bindings;
        }

        if let Ok(mut app_config) = crate::modules::config::load_app_config() {
            let config = self.config.read().await;
            app_config.proxy.proxy_pool = config.clone();
            if let Err(e) = crate::modules::config::save_app_config(&app_config) {
                tracing::error!("[ProxyPool] Failed to persist bindings: {}", e);
            }
        }
    }

    // ========================================================================
    // Health checking
    // ========================================================================

    /// Run a health check on all enabled proxies (concurrent, capped at 20).
    pub async fn health_check(&self) -> Result<(), String> {
        let proxies_to_check: Vec<ProxyEntry> = {
            let config = self.config.read().await;
            config
                .proxies
                .iter()
                .filter(|p| p.enabled)
                .cloned()
                .collect()
        };

        let concurrency_limit = 20usize;
        let results: Vec<(String, bool, Option<u64>)> = stream::iter(proxies_to_check)
            .map(|proxy| async move {
                let (is_healthy, latency) = Self::check_proxy_health(&proxy).await;
                tracing::info!(
                    "Proxy {} ({}) health check: {} (Latency: {})",
                    proxy.name,
                    proxy.url,
                    if is_healthy { "✓ OK" } else { "✗ FAILED" },
                    latency.map_or("-".to_string(), |ms| format!("{}ms", ms))
                );
                (proxy.id, is_healthy, latency)
            })
            .buffer_unordered(concurrency_limit)
            .collect()
            .await;

        // Batch-update proxy health state.
        let mut config = self.config.write().await;
        for (id, is_healthy, latency) in results {
            if let Some(proxy) = config.proxies.iter_mut().find(|p| p.id == id) {
                proxy.is_healthy = is_healthy;
                proxy.latency = latency;
                proxy.last_check_time = Some(chrono::Utc::now().timestamp());
            }
        }

        Ok(())
    }

    /// Check a single proxy's health by issuing a lightweight HTTP request.
    async fn check_proxy_health(entry: &ProxyEntry) -> (bool, Option<u64>) {
        let check_url = entry
            .health_check_url
            .as_deref()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or("http://cp.cloudflare.com/generate_204");

        let proxy_cfg = match Self::build_proxy_config(entry) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::error!("Proxy {} build config failed: {}", entry.url, e);
                return (false, None);
            }
        };

        let client = match Client::builder()
            .proxy(proxy_cfg.proxy)
            .timeout(Duration::from_secs(10))
            .user_agent("Mozilla/5.0")
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Proxy {} build client failed: {}", entry.url, e);
                return (false, None);
            }
        };

        let start = std::time::Instant::now();
        match client.get(check_url).send().await {
            Ok(resp) => {
                let latency = start.elapsed().as_millis() as u64;
                if resp.status().is_success() {
                    (true, Some(latency))
                } else {
                    tracing::warn!(
                        "Proxy {} health check status error: {}",
                        entry.url,
                        resp.status()
                    );
                    (false, None)
                }
            }
            Err(e) => {
                tracing::warn!("Proxy {} health check request failed: {}", entry.url, e);
                (false, None)
            }
        }
    }

    /// Start a background loop that periodically runs health checks.
    pub fn start_health_check_loop(self: Arc<Self>) {
        tokio::spawn(async move {
            tracing::info!("Starting proxy pool health check loop...");
            loop {
                let enabled = self.config.read().await.enabled;
                if enabled {
                    if let Err(e) = self.health_check().await {
                        tracing::error!("Proxy pool health check failed: {}", e);
                    }
                }

                let interval_secs = {
                    let cfg = self.config.read().await;
                    if !cfg.enabled {
                        60
                    } else {
                        cfg.health_check_interval.max(30)
                    }
                };

                tokio::time::sleep(Duration::from_secs(interval_secs)).await;
            }
        });
    }
}


// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::config::{ProxyAuth, ProxyEntry, ProxyPoolConfig, ProxySelectionStrategy};

    /// Helper: create a test proxy entry.
    fn make_entry(id: &str, priority: i32, enabled: bool, healthy: bool) -> ProxyEntry {
        ProxyEntry {
            id: id.to_string(),
            name: format!("proxy-{}", id),
            url: format!("http://proxy-{}.example.com:8080", id),
            auth: None,
            enabled,
            priority,
            tags: vec![],
            max_accounts: None,
            health_check_url: None,
            last_check_time: None,
            is_healthy: healthy,
            latency: None,
        }
    }

    fn make_config(
        proxies: Vec<ProxyEntry>,
        strategy: ProxySelectionStrategy,
        auto_failover: bool,
    ) -> ProxyPoolConfig {
        ProxyPoolConfig {
            enabled: true,
            proxies,
            health_check_interval: 300,
            auto_failover,
            strategy,
            account_bindings: HashMap::new(),
        }
    }

    fn pool_manager(config: ProxyPoolConfig) -> ProxyPoolManager {
        ProxyPoolManager::new(Arc::new(RwLock::new(config)))
    }

    // ── Strategy tests ──────────────────────────────────────────────────

    #[test]
    fn test_round_robin_cycles() {
        let entries = vec![
            make_entry("a", 1, true, true),
            make_entry("b", 2, true, true),
            make_entry("c", 3, true, true),
        ];
        let mgr = pool_manager(make_config(
            entries.clone(),
            ProxySelectionStrategy::RoundRobin,
            false,
        ));
        let refs: Vec<&ProxyEntry> = entries.iter().collect();

        let first = mgr.select_round_robin(&refs).unwrap();
        let second = mgr.select_round_robin(&refs).unwrap();
        let third = mgr.select_round_robin(&refs).unwrap();
        let fourth = mgr.select_round_robin(&refs).unwrap();

        assert_eq!(first.id, "a");
        assert_eq!(second.id, "b");
        assert_eq!(third.id, "c");
        assert_eq!(fourth.id, "a"); // wraps around
    }

    #[test]
    fn test_random_returns_some() {
        let entries = vec![
            make_entry("a", 1, true, true),
            make_entry("b", 2, true, true),
        ];
        let mgr = pool_manager(make_config(
            entries.clone(),
            ProxySelectionStrategy::Random,
            false,
        ));
        let refs: Vec<&ProxyEntry> = entries.iter().collect();

        let selected = mgr.select_random(&refs);
        assert!(selected.is_some());
    }

    #[test]
    fn test_priority_selects_lowest() {
        let entries = vec![
            make_entry("a", 10, true, true),
            make_entry("b", 1, true, true),
            make_entry("c", 5, true, true),
        ];
        let mgr = pool_manager(make_config(
            entries.clone(),
            ProxySelectionStrategy::Priority,
            false,
        ));
        let refs: Vec<&ProxyEntry> = entries.iter().collect();

        let selected = mgr.select_by_priority(&refs).unwrap();
        assert_eq!(selected.id, "b");
    }

    #[test]
    fn test_least_connections_selects_least_used() {
        let entries = vec![
            make_entry("a", 1, true, true),
            make_entry("b", 1, true, true),
        ];
        let mgr = pool_manager(make_config(
            entries.clone(),
            ProxySelectionStrategy::LeastConnections,
            false,
        ));
        // Simulate usage: "a" has 5 requests, "b" has 2.
        mgr.usage_counter.insert("a".to_string(), 5);
        mgr.usage_counter.insert("b".to_string(), 2);

        let refs: Vec<&ProxyEntry> = entries.iter().collect();
        let selected = mgr.select_least_connections(&refs).unwrap();
        assert_eq!(selected.id, "b");
    }

    #[test]
    fn test_weighted_delegates_to_priority() {
        let entries = vec![
            make_entry("a", 10, true, true),
            make_entry("b", 1, true, true),
        ];
        let mgr = pool_manager(make_config(
            entries.clone(),
            ProxySelectionStrategy::WeightedRoundRobin,
            false,
        ));
        let refs: Vec<&ProxyEntry> = entries.iter().collect();

        let selected = mgr.select_weighted(&refs).unwrap();
        assert_eq!(selected.id, "b");
    }

    #[test]
    fn test_empty_proxies_returns_none() {
        let mgr = pool_manager(make_config(
            vec![],
            ProxySelectionStrategy::RoundRobin,
            false,
        ));
        let refs: Vec<&ProxyEntry> = vec![];
        assert!(mgr.select_round_robin(&refs).is_none());
        assert!(mgr.select_random(&refs).is_none());
        assert!(mgr.select_by_priority(&refs).is_none());
        assert!(mgr.select_least_connections(&refs).is_none());
        assert!(mgr.select_weighted(&refs).is_none());
    }

    // ── Pool selection tests ────────────────────────────────────────────

    #[tokio::test]
    async fn test_select_pool_filters_disabled() {
        let config = make_config(
            vec![
                make_entry("a", 1, false, true), // disabled
                make_entry("b", 2, true, true),
            ],
            ProxySelectionStrategy::Priority,
            false,
        );
        let mgr = pool_manager(config.clone());
        let result = mgr.select_proxy_from_pool(&config).await.unwrap();
        assert_eq!(result.unwrap().entry_id, "b");
    }

    #[tokio::test]
    async fn test_select_pool_filters_unhealthy_with_failover() {
        let config = make_config(
            vec![
                make_entry("a", 1, true, false), // unhealthy
                make_entry("b", 2, true, true),
            ],
            ProxySelectionStrategy::Priority,
            true, // auto_failover enabled
        );
        let mgr = pool_manager(config.clone());
        let result = mgr.select_proxy_from_pool(&config).await.unwrap();
        assert_eq!(result.unwrap().entry_id, "b");
    }

    #[tokio::test]
    async fn test_select_pool_excludes_bound_proxies() {
        let config = make_config(
            vec![
                make_entry("a", 1, true, true),
                make_entry("b", 2, true, true),
            ],
            ProxySelectionStrategy::Priority,
            false,
        );
        let mgr = pool_manager(config.clone());
        // Bind proxy "a" to some account.
        mgr.account_bindings
            .insert("acc-1".to_string(), "a".to_string());

        let result = mgr.select_proxy_from_pool(&config).await.unwrap();
        assert_eq!(result.unwrap().entry_id, "b");
    }

    #[tokio::test]
    async fn test_select_pool_returns_none_when_all_bound() {
        let config = make_config(
            vec![make_entry("a", 1, true, true)],
            ProxySelectionStrategy::Priority,
            false,
        );
        let mgr = pool_manager(config.clone());
        mgr.account_bindings
            .insert("acc-1".to_string(), "a".to_string());

        let result = mgr.select_proxy_from_pool(&config).await.unwrap();
        assert!(result.is_none());
    }

    // ── Bound proxy tests ───────────────────────────────────────────────

    #[test]
    fn test_get_bound_proxy_returns_bound() {
        let config = make_config(
            vec![make_entry("a", 1, true, true)],
            ProxySelectionStrategy::Priority,
            false,
        );
        let mgr = pool_manager(config.clone());
        mgr.account_bindings
            .insert("acc-1".to_string(), "a".to_string());

        let result = mgr.get_bound_proxy("acc-1", &config).unwrap();
        assert_eq!(result.unwrap().entry_id, "a");
    }

    #[test]
    fn test_get_bound_proxy_skips_unhealthy_with_failover() {
        let config = make_config(
            vec![make_entry("a", 1, true, false)], // unhealthy
            ProxySelectionStrategy::Priority,
            true,
        );
        let mgr = pool_manager(config.clone());
        mgr.account_bindings
            .insert("acc-1".to_string(), "a".to_string());

        let result = mgr.get_bound_proxy("acc-1", &config).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_bound_proxy_returns_none_for_unbound() {
        let config = make_config(
            vec![make_entry("a", 1, true, true)],
            ProxySelectionStrategy::Priority,
            false,
        );
        let mgr = pool_manager(config.clone());

        let result = mgr.get_bound_proxy("acc-1", &config).unwrap();
        assert!(result.is_none());
    }

    // ── Binding management tests ────────────────────────────────────────

    #[tokio::test]
    async fn test_bind_and_unbind_account() {
        let config = make_config(
            vec![make_entry("a", 1, true, true)],
            ProxySelectionStrategy::Priority,
            false,
        );
        let mgr = pool_manager(config);

        // Bind
        mgr.account_bindings
            .insert("acc-1".to_string(), "a".to_string());
        assert_eq!(
            mgr.get_account_binding("acc-1"),
            Some("a".to_string())
        );

        // Unbind
        mgr.account_bindings.remove("acc-1");
        assert!(mgr.get_account_binding("acc-1").is_none());
    }

    #[test]
    fn test_get_all_bindings_snapshot() {
        let config = make_config(vec![], ProxySelectionStrategy::Priority, false);
        let mgr = pool_manager(config);

        mgr.account_bindings
            .insert("acc-1".to_string(), "proxy-a".to_string());
        mgr.account_bindings
            .insert("acc-2".to_string(), "proxy-b".to_string());

        let snapshot = mgr.get_all_bindings_snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot.get("acc-1"), Some(&"proxy-a".to_string()));
        assert_eq!(snapshot.get("acc-2"), Some(&"proxy-b".to_string()));
    }

    // ── build_proxy_config tests ────────────────────────────────────────

    #[test]
    fn test_build_proxy_config_valid_url() {
        let entry = make_entry("a", 1, true, true);
        let result = ProxyPoolManager::build_proxy_config(&entry);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().entry_id, "a");
    }

    #[test]
    fn test_build_proxy_config_with_auth() {
        let mut entry = make_entry("a", 1, true, true);
        entry.auth = Some(ProxyAuth {
            username: "user".to_string(),
            password: "pass".to_string(),
        });
        let result = ProxyPoolManager::build_proxy_config(&entry);
        assert!(result.is_ok());
    }

    // ── Disabled pool tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_proxy_for_account_disabled_pool() {
        let mut config = make_config(
            vec![make_entry("a", 1, true, true)],
            ProxySelectionStrategy::Priority,
            false,
        );
        config.enabled = false;
        let mgr = pool_manager(config);

        let result = mgr.get_proxy_for_account("acc-1").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_proxy_for_account_empty_proxies() {
        let config = make_config(vec![], ProxySelectionStrategy::Priority, false);
        let mgr = pool_manager(config);

        let result = mgr.get_proxy_for_account("acc-1").await.unwrap();
        assert!(result.is_none());
    }

    // ── Config loading tests ────────────────────────────────────────────

    #[test]
    fn test_new_loads_bindings_from_config() {
        let mut config = make_config(vec![], ProxySelectionStrategy::Priority, false);
        config
            .account_bindings
            .insert("acc-1".to_string(), "proxy-a".to_string());

        let mgr = pool_manager(config);
        assert_eq!(
            mgr.get_account_binding("acc-1"),
            Some("proxy-a".to_string())
        );
    }

    // ── Usage counter tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_usage_counter_increments_on_selection() {
        let config = make_config(
            vec![make_entry("a", 1, true, true)],
            ProxySelectionStrategy::Priority,
            false,
        );
        let mgr = pool_manager(config.clone());

        // Select twice.
        let _ = mgr.select_proxy_from_pool(&config).await;
        let _ = mgr.select_proxy_from_pool(&config).await;

        let count = mgr.usage_counter.get("a").map(|v| *v).unwrap_or(0);
        assert_eq!(count, 2);
    }
}
