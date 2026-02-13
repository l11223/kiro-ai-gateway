use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// ProxyAuthMode
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProxyAuthMode {
    Off,
    Strict,
    AllExceptHealth,
    Auto,
}

impl Default for ProxyAuthMode {
    fn default() -> Self {
        Self::Auto
    }
}

// ============================================================================
// Scheduling (Sticky Session)
// ============================================================================

/// 调度模式枚举
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SchedulingMode {
    /// 缓存优先: 尽可能锁定同一账号，限流时优先等待
    CacheFirst,
    /// 平衡模式: 锁定同一账号，限流时立即切换
    Balance,
    /// 性能优先: 纯轮询模式
    PerformanceFirst,
}

impl Default for SchedulingMode {
    fn default() -> Self {
        Self::Balance
    }
}

/// 粘性会话配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StickySessionConfig {
    pub mode: SchedulingMode,
    /// 缓存优先模式下的最大等待时间 (秒)
    pub max_wait_seconds: u64,
}

impl Default for StickySessionConfig {
    fn default() -> Self {
        Self {
            mode: SchedulingMode::Balance,
            max_wait_seconds: 60,
        }
    }
}

// ============================================================================
// Z.ai Configuration
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ZaiDispatchMode {
    Off,
    Exclusive,
    Pooled,
    Fallback,
}

impl Default for ZaiDispatchMode {
    fn default() -> Self {
        Self::Off
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ZaiModelDefaults {
    #[serde(default = "default_zai_opus_model")]
    pub opus: String,
    #[serde(default = "default_zai_sonnet_model")]
    pub sonnet: String,
    #[serde(default = "default_zai_haiku_model")]
    pub haiku: String,
}

impl Default for ZaiModelDefaults {
    fn default() -> Self {
        Self {
            opus: default_zai_opus_model(),
            sonnet: default_zai_sonnet_model(),
            haiku: default_zai_haiku_model(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ZaiMcpConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub web_search_enabled: bool,
    #[serde(default)]
    pub web_reader_enabled: bool,
    #[serde(default)]
    pub vision_enabled: bool,
}

impl Default for ZaiMcpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            web_search_enabled: false,
            web_reader_enabled: false,
            vision_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ZaiConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_zai_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub dispatch_mode: ZaiDispatchMode,
    #[serde(default)]
    pub model_mapping: HashMap<String, String>,
    #[serde(default)]
    pub models: ZaiModelDefaults,
    #[serde(default)]
    pub mcp: ZaiMcpConfig,
}

impl Default for ZaiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_zai_base_url(),
            api_key: String::new(),
            dispatch_mode: ZaiDispatchMode::Off,
            model_mapping: HashMap::new(),
            models: ZaiModelDefaults::default(),
            mcp: ZaiMcpConfig::default(),
        }
    }
}

fn default_zai_base_url() -> String {
    "https://api.z.ai/api/anthropic".to_string()
}

fn default_zai_opus_model() -> String {
    "glm-4.7".to_string()
}

fn default_zai_sonnet_model() -> String {
    "glm-4.7".to_string()
}

fn default_zai_haiku_model() -> String {
    "glm-4.5-air".to_string()
}

// ============================================================================
// Experimental Config
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExperimentalConfig {
    #[serde(default = "default_true")]
    pub enable_signature_cache: bool,
    #[serde(default = "default_true")]
    pub enable_tool_loop_recovery: bool,
    #[serde(default = "default_true")]
    pub enable_cross_model_checks: bool,
    #[serde(default = "default_false")]
    pub enable_usage_scaling: bool,
    #[serde(default = "default_threshold_l1")]
    pub context_compression_threshold_l1: f32,
    #[serde(default = "default_threshold_l2")]
    pub context_compression_threshold_l2: f32,
    #[serde(default = "default_threshold_l3")]
    pub context_compression_threshold_l3: f32,
}

impl Default for ExperimentalConfig {
    fn default() -> Self {
        Self {
            enable_signature_cache: true,
            enable_tool_loop_recovery: true,
            enable_cross_model_checks: true,
            enable_usage_scaling: false,
            context_compression_threshold_l1: 0.4,
            context_compression_threshold_l2: 0.55,
            context_compression_threshold_l3: 0.7,
        }
    }
}

fn default_threshold_l1() -> f32 { 0.4 }
fn default_threshold_l2() -> f32 { 0.55 }
fn default_threshold_l3() -> f32 { 0.7 }
fn default_true() -> bool { true }
fn default_false() -> bool { false }

// ============================================================================
// Thinking Budget
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingBudgetMode {
    Auto,
    Passthrough,
    Custom,
    Adaptive,
}

impl Default for ThinkingBudgetMode {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThinkingBudgetConfig {
    #[serde(default)]
    pub mode: ThinkingBudgetMode,
    #[serde(default = "default_thinking_budget_custom_value")]
    pub custom_value: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

impl Default for ThinkingBudgetConfig {
    fn default() -> Self {
        Self {
            mode: ThinkingBudgetMode::Auto,
            custom_value: default_thinking_budget_custom_value(),
            effort: None,
        }
    }
}

fn default_thinking_budget_custom_value() -> u32 {
    24576
}

// ============================================================================
// Global System Prompt
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlobalSystemPromptConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub content: String,
}

impl Default for GlobalSystemPromptConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            content: String::new(),
        }
    }
}

// ============================================================================
// Debug Logging
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DebugLoggingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub output_dir: Option<String>,
}

impl Default for DebugLoggingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            output_dir: None,
        }
    }
}

// ============================================================================
// Upstream Proxy
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpstreamProxyConfig {
    pub enabled: bool,
    pub url: String,
}

impl Default for UpstreamProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
        }
    }
}

// ============================================================================
// Security Monitor (IP Blacklist / Whitelist)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IpBlacklistConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_block_message")]
    pub block_message: String,
}

impl Default for IpBlacklistConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            block_message: default_block_message(),
        }
    }
}

fn default_block_message() -> String {
    "Access denied".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IpWhitelistConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub whitelist_priority: bool,
}

impl Default for IpWhitelistConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            whitelist_priority: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecurityMonitorConfig {
    #[serde(default)]
    pub blacklist: IpBlacklistConfig,
    #[serde(default)]
    pub whitelist: IpWhitelistConfig,
}

impl Default for SecurityMonitorConfig {
    fn default() -> Self {
        Self {
            blacklist: IpBlacklistConfig::default(),
            whitelist: IpWhitelistConfig::default(),
        }
    }
}

// ============================================================================
// Proxy Pool
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProxySelectionStrategy {
    RoundRobin,
    Random,
    Priority,
    LeastConnections,
    WeightedRoundRobin,
}

impl Default for ProxySelectionStrategy {
    fn default() -> Self {
        Self::Priority
    }
}

/// 代理认证信息
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProxyAuth {
    pub username: String,
    pub password: String,
}

/// 单个代理配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProxyEntry {
    pub id: String,
    pub name: String,
    pub url: String,
    pub auth: Option<ProxyAuth>,
    pub enabled: bool,
    pub priority: i32,
    pub tags: Vec<String>,
    pub max_accounts: Option<usize>,
    pub health_check_url: Option<String>,
    pub last_check_time: Option<i64>,
    pub is_healthy: bool,
    pub latency: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProxyPoolConfig {
    pub enabled: bool,
    pub proxies: Vec<ProxyEntry>,
    pub health_check_interval: u64,
    pub auto_failover: bool,
    pub strategy: ProxySelectionStrategy,
    #[serde(default)]
    pub account_bindings: HashMap<String, String>,
}

impl Default for ProxyPoolConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            proxies: Vec::new(),
            health_check_interval: 300,
            auto_failover: true,
            strategy: ProxySelectionStrategy::Priority,
            account_bindings: HashMap::new(),
        }
    }
}

// ============================================================================
// Cloudflared
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TunnelMode {
    Quick,
    Auth,
}

impl Default for TunnelMode {
    fn default() -> Self {
        Self::Quick
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CloudflaredConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub mode: TunnelMode,
    pub port: u16,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub use_http2: bool,
}

impl Default for CloudflaredConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: TunnelMode::Quick,
            port: 8045,
            token: None,
            use_http2: true,
        }
    }
}

// ============================================================================
// Scheduled Warmup
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduledWarmupConfig {
    pub enabled: bool,
    #[serde(default = "default_warmup_models")]
    pub monitored_models: Vec<String>,
}

fn default_warmup_models() -> Vec<String> {
    vec![
        "gemini-3-flash".to_string(),
        "claude".to_string(),
        "gemini-3-pro-high".to_string(),
        "gemini-3-pro-image".to_string(),
    ]
}

impl ScheduledWarmupConfig {
    pub fn new() -> Self {
        Self {
            enabled: false,
            monitored_models: default_warmup_models(),
        }
    }
}

impl Default for ScheduledWarmupConfig {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Quota Protection
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuotaProtectionConfig {
    pub enabled: bool,
    pub threshold_percentage: u32,
    #[serde(default = "default_monitored_models")]
    pub monitored_models: Vec<String>,
}

fn default_monitored_models() -> Vec<String> {
    vec![
        "claude".to_string(),
        "gemini-3-pro-high".to_string(),
        "gemini-3-flash".to_string(),
        "gemini-3-pro-image".to_string(),
    ]
}

impl QuotaProtectionConfig {
    pub fn new() -> Self {
        Self {
            enabled: false,
            threshold_percentage: 10,
            monitored_models: default_monitored_models(),
        }
    }
}

impl Default for QuotaProtectionConfig {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Pinned Quota Models
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PinnedQuotaModelsConfig {
    #[serde(default = "default_pinned_models")]
    pub models: Vec<String>,
}

fn default_pinned_models() -> Vec<String> {
    vec![
        "gemini-3-pro-high".to_string(),
        "gemini-3-flash".to_string(),
        "gemini-3-pro-image".to_string(),
        "claude-sonnet-4-5-thinking".to_string(),
    ]
}

impl PinnedQuotaModelsConfig {
    pub fn new() -> Self {
        Self {
            models: default_pinned_models(),
        }
    }
}

impl Default for PinnedQuotaModelsConfig {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Circuit Breaker
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CircuitBreakerConfig {
    pub enabled: bool,
    #[serde(default = "default_backoff_steps")]
    pub backoff_steps: Vec<u64>,
}

fn default_backoff_steps() -> Vec<u64> {
    vec![60, 300, 1800, 7200]
}

impl CircuitBreakerConfig {
    pub fn new() -> Self {
        Self {
            enabled: true,
            backoff_steps: default_backoff_steps(),
        }
    }
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// ProxyConfig (main proxy configuration)
// ============================================================================

fn default_request_timeout() -> u64 {
    120
}

/// 反代服务配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProxyConfig {
    pub enabled: bool,
    #[serde(default)]
    pub allow_lan_access: bool,
    #[serde(default)]
    pub auth_mode: ProxyAuthMode,
    pub port: u16,
    pub api_key: String,
    pub admin_password: Option<String>,
    pub auto_start: bool,
    #[serde(default)]
    pub custom_mapping: HashMap<String, String>,
    #[serde(default = "default_request_timeout")]
    pub request_timeout: u64,
    #[serde(default)]
    pub enable_logging: bool,
    #[serde(default)]
    pub debug_logging: DebugLoggingConfig,
    #[serde(default)]
    pub upstream_proxy: UpstreamProxyConfig,
    #[serde(default)]
    pub zai: ZaiConfig,
    #[serde(default)]
    pub user_agent_override: Option<String>,
    #[serde(default)]
    pub scheduling: StickySessionConfig,
    #[serde(default)]
    pub experimental: ExperimentalConfig,
    #[serde(default)]
    pub security_monitor: SecurityMonitorConfig,
    #[serde(default)]
    pub preferred_account_id: Option<String>,
    #[serde(default)]
    pub saved_user_agent: Option<String>,
    #[serde(default)]
    pub thinking_budget: ThinkingBudgetConfig,
    #[serde(default)]
    pub global_system_prompt: GlobalSystemPromptConfig,
    #[serde(default)]
    pub image_thinking_mode: Option<String>,
    #[serde(default)]
    pub proxy_pool: ProxyPoolConfig,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_lan_access: false,
            auth_mode: ProxyAuthMode::default(),
            port: 8045,
            api_key: format!("sk-{}", uuid::Uuid::new_v4().simple()),
            admin_password: None,
            auto_start: false,
            custom_mapping: HashMap::new(),
            request_timeout: default_request_timeout(),
            enable_logging: true,
            debug_logging: DebugLoggingConfig::default(),
            upstream_proxy: UpstreamProxyConfig::default(),
            zai: ZaiConfig::default(),
            scheduling: StickySessionConfig::default(),
            experimental: ExperimentalConfig::default(),
            security_monitor: SecurityMonitorConfig::default(),
            preferred_account_id: None,
            user_agent_override: None,
            saved_user_agent: None,
            thinking_budget: ThinkingBudgetConfig::default(),
            global_system_prompt: GlobalSystemPromptConfig::default(),
            proxy_pool: ProxyPoolConfig::default(),
            image_thinking_mode: None,
        }
    }
}

impl ProxyConfig {
    /// 获取实际的监听地址
    pub fn get_bind_address(&self) -> &str {
        if self.allow_lan_access {
            "0.0.0.0"
        } else {
            "127.0.0.1"
        }
    }
}

// ============================================================================
// AppConfig (top-level application configuration)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppConfig {
    pub language: String,
    pub theme: String,
    pub auto_refresh: bool,
    pub refresh_interval: i32,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub auto_launch: bool,
    #[serde(default)]
    pub scheduled_warmup: ScheduledWarmupConfig,
    #[serde(default)]
    pub quota_protection: QuotaProtectionConfig,
    #[serde(default)]
    pub pinned_quota_models: PinnedQuotaModelsConfig,
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,
    #[serde(default)]
    pub hidden_menu_items: Vec<String>,
    #[serde(default)]
    pub cloudflared: CloudflaredConfig,
}

impl AppConfig {
    pub fn new() -> Self {
        Self {
            language: "zh".to_string(),
            theme: "system".to_string(),
            auto_refresh: true,
            refresh_interval: 15,
            proxy: ProxyConfig::default(),
            auto_launch: false,
            scheduled_warmup: ScheduledWarmupConfig::default(),
            quota_protection: QuotaProtectionConfig::default(),
            pinned_quota_models: PinnedQuotaModelsConfig::default(),
            circuit_breaker: CircuitBreakerConfig::default(),
            hidden_menu_items: Vec::new(),
            cloudflared: CloudflaredConfig::default(),
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self::new()
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use proptest::collection::{hash_map, vec};
    use proptest::prelude::*;

    // ── Arbitrary strategies ───────────────────────────────────────────

    fn arb_proxy_auth_mode() -> impl Strategy<Value = ProxyAuthMode> {
        prop_oneof![
            Just(ProxyAuthMode::Off),
            Just(ProxyAuthMode::Strict),
            Just(ProxyAuthMode::AllExceptHealth),
            Just(ProxyAuthMode::Auto),
        ]
    }

    fn arb_scheduling_mode() -> impl Strategy<Value = SchedulingMode> {
        prop_oneof![
            Just(SchedulingMode::CacheFirst),
            Just(SchedulingMode::Balance),
            Just(SchedulingMode::PerformanceFirst),
        ]
    }

    fn arb_sticky_session_config() -> impl Strategy<Value = StickySessionConfig> {
        (arb_scheduling_mode(), 0u64..=600u64).prop_map(|(mode, max_wait_seconds)| {
            StickySessionConfig { mode, max_wait_seconds }
        })
    }

    fn arb_zai_dispatch_mode() -> impl Strategy<Value = ZaiDispatchMode> {
        prop_oneof![
            Just(ZaiDispatchMode::Off),
            Just(ZaiDispatchMode::Exclusive),
            Just(ZaiDispatchMode::Pooled),
            Just(ZaiDispatchMode::Fallback),
        ]
    }

    fn arb_zai_model_defaults() -> impl Strategy<Value = ZaiModelDefaults> {
        ("[a-zA-Z0-9.-]{3,20}", "[a-zA-Z0-9.-]{3,20}", "[a-zA-Z0-9.-]{3,20}")
            .prop_map(|(opus, sonnet, haiku)| ZaiModelDefaults { opus, sonnet, haiku })
    }

    fn arb_zai_mcp_config() -> impl Strategy<Value = ZaiMcpConfig> {
        (any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>()).prop_map(
            |(enabled, web_search_enabled, web_reader_enabled, vision_enabled)| ZaiMcpConfig {
                enabled,
                web_search_enabled,
                web_reader_enabled,
                vision_enabled,
            },
        )
    }

    fn arb_zai_config() -> impl Strategy<Value = ZaiConfig> {
        (
            any::<bool>(),
            "[a-zA-Z0-9:/._-]{5,40}",
            "[a-zA-Z0-9]{0,30}",
            arb_zai_dispatch_mode(),
            hash_map("[a-zA-Z0-9_-]{3,15}", "[a-zA-Z0-9_-]{3,15}", 0..3),
            arb_zai_model_defaults(),
            arb_zai_mcp_config(),
        )
            .prop_map(|(enabled, base_url, api_key, dispatch_mode, model_mapping, models, mcp)| {
                ZaiConfig { enabled, base_url, api_key, dispatch_mode, model_mapping, models, mcp }
            })
    }

    fn arb_experimental_config() -> impl Strategy<Value = ExperimentalConfig> {
        (
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            0.0f32..=1.0f32,
            0.0f32..=1.0f32,
            0.0f32..=1.0f32,
        )
            .prop_map(
                |(
                    enable_signature_cache,
                    enable_tool_loop_recovery,
                    enable_cross_model_checks,
                    enable_usage_scaling,
                    l1,
                    l2,
                    l3,
                )| ExperimentalConfig {
                    enable_signature_cache,
                    enable_tool_loop_recovery,
                    enable_cross_model_checks,
                    enable_usage_scaling,
                    context_compression_threshold_l1: l1,
                    context_compression_threshold_l2: l2,
                    context_compression_threshold_l3: l3,
                },
            )
    }

    fn arb_thinking_budget_mode() -> impl Strategy<Value = ThinkingBudgetMode> {
        prop_oneof![
            Just(ThinkingBudgetMode::Auto),
            Just(ThinkingBudgetMode::Passthrough),
            Just(ThinkingBudgetMode::Custom),
            Just(ThinkingBudgetMode::Adaptive),
        ]
    }

    fn arb_thinking_budget_config() -> impl Strategy<Value = ThinkingBudgetConfig> {
        (
            arb_thinking_budget_mode(),
            0u32..=100000u32,
            proptest::option::of(prop_oneof!["low", "medium", "high"].boxed()),
        )
            .prop_map(|(mode, custom_value, effort)| ThinkingBudgetConfig {
                mode,
                custom_value,
                effort,
            })
    }

    fn arb_global_system_prompt_config() -> impl Strategy<Value = GlobalSystemPromptConfig> {
        (any::<bool>(), "[a-zA-Z0-9 ]{0,50}").prop_map(|(enabled, content)| {
            GlobalSystemPromptConfig { enabled, content }
        })
    }

    fn arb_debug_logging_config() -> impl Strategy<Value = DebugLoggingConfig> {
        (any::<bool>(), proptest::option::of("[a-zA-Z0-9/_-]{3,30}")).prop_map(
            |(enabled, output_dir)| DebugLoggingConfig { enabled, output_dir },
        )
    }

    fn arb_upstream_proxy_config() -> impl Strategy<Value = UpstreamProxyConfig> {
        (any::<bool>(), "[a-zA-Z0-9:/._-]{0,40}").prop_map(|(enabled, url)| {
            UpstreamProxyConfig { enabled, url }
        })
    }

    fn arb_ip_blacklist_config() -> impl Strategy<Value = IpBlacklistConfig> {
        (any::<bool>(), "[a-zA-Z0-9 ]{1,30}").prop_map(|(enabled, block_message)| {
            IpBlacklistConfig { enabled, block_message }
        })
    }

    fn arb_ip_whitelist_config() -> impl Strategy<Value = IpWhitelistConfig> {
        (any::<bool>(), any::<bool>()).prop_map(|(enabled, whitelist_priority)| {
            IpWhitelistConfig { enabled, whitelist_priority }
        })
    }

    fn arb_security_monitor_config() -> impl Strategy<Value = SecurityMonitorConfig> {
        (arb_ip_blacklist_config(), arb_ip_whitelist_config()).prop_map(|(blacklist, whitelist)| {
            SecurityMonitorConfig { blacklist, whitelist }
        })
    }

    fn arb_proxy_selection_strategy() -> impl Strategy<Value = ProxySelectionStrategy> {
        prop_oneof![
            Just(ProxySelectionStrategy::RoundRobin),
            Just(ProxySelectionStrategy::Random),
            Just(ProxySelectionStrategy::Priority),
            Just(ProxySelectionStrategy::LeastConnections),
            Just(ProxySelectionStrategy::WeightedRoundRobin),
        ]
    }

    fn arb_proxy_auth() -> impl Strategy<Value = ProxyAuth> {
        ("[a-zA-Z0-9]{3,15}", "[a-zA-Z0-9]{3,15}").prop_map(|(username, password)| {
            ProxyAuth { username, password }
        })
    }

    fn arb_proxy_entry() -> impl Strategy<Value = ProxyEntry> {
        (
            "[a-f0-9-]{36}",
            "[a-zA-Z0-9 ]{1,20}",
            "[a-zA-Z0-9:/._-]{5,40}",
            proptest::option::of(arb_proxy_auth()),
            any::<bool>(),
            -10i32..=10i32,
            vec("[a-zA-Z0-9]{2,10}", 0..3),
            proptest::option::of(1usize..=100usize),
            proptest::option::of("[a-zA-Z0-9:/._-]{5,40}"),
            proptest::option::of(0i64..=2_000_000_000i64),
            any::<bool>(),
            proptest::option::of(0u64..=10000u64),
        )
            .prop_map(
                |(id, name, url, auth, enabled, priority, tags, max_accounts, health_check_url, last_check_time, is_healthy, latency)| {
                    ProxyEntry {
                        id, name, url, auth, enabled, priority, tags, max_accounts,
                        health_check_url, last_check_time, is_healthy, latency,
                    }
                },
            )
    }

    fn arb_proxy_pool_config() -> impl Strategy<Value = ProxyPoolConfig> {
        (
            any::<bool>(),
            vec(arb_proxy_entry(), 0..3),
            60u64..=600u64,
            any::<bool>(),
            arb_proxy_selection_strategy(),
            hash_map("[a-f0-9-]{36}", "[a-f0-9-]{36}", 0..3),
        )
            .prop_map(
                |(enabled, proxies, health_check_interval, auto_failover, strategy, account_bindings)| {
                    ProxyPoolConfig {
                        enabled, proxies, health_check_interval, auto_failover, strategy, account_bindings,
                    }
                },
            )
    }

    fn arb_tunnel_mode() -> impl Strategy<Value = TunnelMode> {
        prop_oneof![Just(TunnelMode::Quick), Just(TunnelMode::Auth)]
    }

    fn arb_cloudflared_config() -> impl Strategy<Value = CloudflaredConfig> {
        (
            any::<bool>(),
            arb_tunnel_mode(),
            1024u16..=65535u16,
            proptest::option::of("[a-zA-Z0-9]{10,40}"),
            any::<bool>(),
        )
            .prop_map(|(enabled, mode, port, token, use_http2)| CloudflaredConfig {
                enabled, mode, port, token, use_http2,
            })
    }

    fn arb_scheduled_warmup_config() -> impl Strategy<Value = ScheduledWarmupConfig> {
        (any::<bool>(), vec("[a-zA-Z0-9_-]{3,20}", 0..5)).prop_map(
            |(enabled, monitored_models)| ScheduledWarmupConfig { enabled, monitored_models },
        )
    }

    fn arb_quota_protection_config() -> impl Strategy<Value = QuotaProtectionConfig> {
        (any::<bool>(), 0u32..=100u32, vec("[a-zA-Z0-9_-]{3,20}", 0..5)).prop_map(
            |(enabled, threshold_percentage, monitored_models)| QuotaProtectionConfig {
                enabled, threshold_percentage, monitored_models,
            },
        )
    }

    fn arb_pinned_quota_models_config() -> impl Strategy<Value = PinnedQuotaModelsConfig> {
        vec("[a-zA-Z0-9_-]{3,20}", 0..5)
            .prop_map(|models| PinnedQuotaModelsConfig { models })
    }

    fn arb_circuit_breaker_config() -> impl Strategy<Value = CircuitBreakerConfig> {
        (any::<bool>(), vec(1u64..=10000u64, 1..6)).prop_map(|(enabled, backoff_steps)| {
            CircuitBreakerConfig { enabled, backoff_steps }
        })
    }

    /// Build a ProxyConfig strategy by composing smaller groups.
    fn arb_proxy_config() -> impl Strategy<Value = ProxyConfig> {
        let group1 = (
            any::<bool>(),
            any::<bool>(),
            arb_proxy_auth_mode(),
            1024u16..=65535u16,
            "[a-zA-Z0-9-]{5,40}",
            proptest::option::of("[a-zA-Z0-9]{5,30}"),
            any::<bool>(),
            hash_map("[a-zA-Z0-9_-]{3,15}", "[a-zA-Z0-9_-]{3,15}", 0..3),
            30u64..=600u64,
            any::<bool>(),
        );

        let group2 = (
            arb_debug_logging_config(),
            arb_upstream_proxy_config(),
            arb_zai_config(),
            proptest::option::of("[a-zA-Z0-9 /_-]{3,30}"),
            arb_sticky_session_config(),
            arb_experimental_config(),
            arb_security_monitor_config(),
            proptest::option::of("[a-f0-9-]{36}"),
            proptest::option::of("[a-zA-Z0-9 /_-]{3,30}"),
        );

        let group3 = (
            arb_thinking_budget_config(),
            arb_global_system_prompt_config(),
            proptest::option::of(prop_oneof!["enabled", "disabled"].boxed()),
            arb_proxy_pool_config(),
        );

        (group1, group2, group3).prop_map(|(g1, g2, g3)| ProxyConfig {
            enabled: g1.0,
            allow_lan_access: g1.1,
            auth_mode: g1.2,
            port: g1.3,
            api_key: g1.4,
            admin_password: g1.5,
            auto_start: g1.6,
            custom_mapping: g1.7,
            request_timeout: g1.8,
            enable_logging: g1.9,
            debug_logging: g2.0,
            upstream_proxy: g2.1,
            zai: g2.2,
            user_agent_override: g2.3,
            scheduling: g2.4,
            experimental: g2.5,
            security_monitor: g2.6,
            preferred_account_id: g2.7,
            saved_user_agent: g2.8,
            thinking_budget: g3.0,
            global_system_prompt: g3.1,
            image_thinking_mode: g3.2,
            proxy_pool: g3.3,
        })
    }

    /// Build an AppConfig strategy.
    fn arb_app_config() -> impl Strategy<Value = AppConfig> {
        (
            prop_oneof!["zh", "en", "ja", "ko", "ru", "es"].boxed(),
            prop_oneof!["system", "light", "dark"].boxed(),
            any::<bool>(),
            1i32..=120i32,
            arb_proxy_config(),
            any::<bool>(),
            arb_scheduled_warmup_config(),
            arb_quota_protection_config(),
            arb_pinned_quota_models_config(),
            arb_circuit_breaker_config(),
            vec("[a-zA-Z0-9_-]{3,15}", 0..5),
            arb_cloudflared_config(),
        )
            .prop_map(
                |(language, theme, auto_refresh, refresh_interval, proxy, auto_launch,
                  scheduled_warmup, quota_protection, pinned_quota_models, circuit_breaker,
                  hidden_menu_items, cloudflared)| {
                    AppConfig {
                        language, theme, auto_refresh, refresh_interval, proxy, auto_launch,
                        scheduled_warmup, quota_protection, pinned_quota_models, circuit_breaker,
                        hidden_menu_items, cloudflared,
                    }
                },
            )
    }

    // ── Property 2: AppConfig 序列化往返一致性 ─────────────────────────
    // **Feature: kiro-ai-gateway, Property 2: AppConfig 序列化往返一致性**
    // **Validates: Requirements 12.5**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn app_config_serialization_roundtrip(config in arb_app_config()) {
            let json = serde_json::to_string(&config)
                .expect("AppConfig serialization should not fail");
            let deserialized: AppConfig = serde_json::from_str(&json)
                .expect("AppConfig deserialization should not fail");
            prop_assert_eq!(&config, &deserialized);
        }
    }
}
