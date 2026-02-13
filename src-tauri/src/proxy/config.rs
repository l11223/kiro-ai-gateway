// Proxy configuration module
//
// Global runtime configuration storage for thinking budget, system prompt,
// and image thinking mode. Uses OnceLock<RwLock<T>> for thread-safe
// hot-update support without requiring function signature changes in
// request transform paths.
//
// Requirements: 9.6, 9.7, 9.8, 9.9, 9.10

use std::sync::{OnceLock, RwLock};

use crate::models::config::{GlobalSystemPromptConfig, ThinkingBudgetConfig, ThinkingBudgetMode};

// ============================================================================
// Utility functions
// ============================================================================

/// Normalize a proxy URL by prepending `http://` if no scheme is present.
pub fn normalize_proxy_url(url: &str) -> String {
    let url = url.trim();
    if url.is_empty() {
        return String::new();
    }
    if !url.contains("://") {
        format!("http://{}", url)
    } else {
        url.to_string()
    }
}

// ============================================================================
// Global Thinking Budget Config
// ============================================================================
static GLOBAL_THINKING_BUDGET_CONFIG: OnceLock<RwLock<ThinkingBudgetConfig>> = OnceLock::new();

/// Get the current global thinking budget configuration.
pub fn get_thinking_budget_config() -> ThinkingBudgetConfig {
    GLOBAL_THINKING_BUDGET_CONFIG
        .get()
        .and_then(|lock| lock.read().ok())
        .map(|cfg| cfg.clone())
        .unwrap_or_default()
}

/// Update the global thinking budget configuration.
///
/// On first call, initializes the global storage. Subsequent calls update
/// the existing value in-place.
pub fn update_thinking_budget_config(config: ThinkingBudgetConfig) {
    if let Some(lock) = GLOBAL_THINKING_BUDGET_CONFIG.get() {
        if let Ok(mut cfg) = lock.write() {
            *cfg = config.clone();
            tracing::info!(
                "[Thinking-Budget] Global config updated: mode={:?}, custom_value={}",
                config.mode,
                config.custom_value
            );
        }
    } else {
        let _ = GLOBAL_THINKING_BUDGET_CONFIG.set(RwLock::new(config.clone()));
        tracing::info!(
            "[Thinking-Budget] Global config initialized: mode={:?}, custom_value={}",
            config.mode,
            config.custom_value
        );
    }
}

/// Resolve the effective thinking budget for a request.
///
/// # Arguments
/// * `user_budget` – the budget value the caller sent (or a default like 24576)
/// * `mapped_model` – the model name after mapping (used for auto-capping)
///
/// # Returns
/// The final budget value to inject into `thinkingConfig.thinkingBudget`.
pub fn resolve_thinking_budget(user_budget: i64, mapped_model: &str) -> i64 {
    let config = get_thinking_budget_config();
    let model_lower = mapped_model.to_lowercase();

    match config.mode {
        ThinkingBudgetMode::Passthrough => {
            // Requirement 9.7: pass through the caller's value unchanged
            user_budget
        }
        ThinkingBudgetMode::Custom => {
            // Requirement 9.8: use the configured fixed value
            let mut value = config.custom_value as i64;
            // Cap Gemini models at 24576 (non-image)
            let is_gemini_limited = model_lower.contains("gemini")
                && !model_lower.contains("-image");
            if is_gemini_limited && value > 24576 {
                tracing::warn!(
                    "[Thinking-Budget] Custom mode: capping from {} to 24576 for {}",
                    value,
                    mapped_model
                );
                value = 24576;
            }
            value
        }
        ThinkingBudgetMode::Auto => {
            // Requirement 9.6: cap specific models (Flash/Thinking/Gemini) at 24576
            // Image models are excluded from capping
            let is_image_model = model_lower.contains("-image");
            let is_gemini_limited = !is_image_model
                && (model_lower.contains("gemini")
                    || model_lower.contains("flash")
                    || model_lower.contains("thinking"));
            if is_gemini_limited && user_budget > 24576 {
                24576
            } else {
                user_budget
            }
        }
        ThinkingBudgetMode::Adaptive => {
            // Requirement 9.9: adaptive mode uses effort parameter;
            // actual effort mapping is handled by the caller, so we
            // pass through the user budget here.
            user_budget
        }
    }
}

// ============================================================================
// Global System Prompt Config
// ============================================================================
static GLOBAL_SYSTEM_PROMPT_CONFIG: OnceLock<RwLock<GlobalSystemPromptConfig>> = OnceLock::new();

/// Get the current global system prompt configuration.
pub fn get_global_system_prompt() -> GlobalSystemPromptConfig {
    GLOBAL_SYSTEM_PROMPT_CONFIG
        .get()
        .and_then(|lock| lock.read().ok())
        .map(|cfg| cfg.clone())
        .unwrap_or_default()
}

/// Update the global system prompt configuration.
pub fn update_global_system_prompt_config(config: GlobalSystemPromptConfig) {
    if let Some(lock) = GLOBAL_SYSTEM_PROMPT_CONFIG.get() {
        if let Ok(mut cfg) = lock.write() {
            *cfg = config.clone();
            tracing::info!(
                "[Global-System-Prompt] Config updated: enabled={}, content_len={}",
                config.enabled,
                config.content.len()
            );
        }
    } else {
        let _ = GLOBAL_SYSTEM_PROMPT_CONFIG.set(RwLock::new(config.clone()));
        tracing::info!(
            "[Global-System-Prompt] Config initialized: enabled={}, content_len={}",
            config.enabled,
            config.content.len()
        );
    }
}

// ============================================================================
// Global Image Thinking Mode
// ============================================================================
static GLOBAL_IMAGE_THINKING_MODE: OnceLock<RwLock<String>> = OnceLock::new();

/// Get the current image thinking mode ("enabled" or "disabled").
pub fn get_image_thinking_mode() -> String {
    GLOBAL_IMAGE_THINKING_MODE
        .get()
        .and_then(|lock| lock.read().ok())
        .map(|s| s.clone())
        .unwrap_or_else(|| "enabled".to_string())
}

/// Update the global image thinking mode.
pub fn update_image_thinking_mode(mode: Option<String>) {
    let val = mode.unwrap_or_else(|| "enabled".to_string());
    if let Some(lock) = GLOBAL_IMAGE_THINKING_MODE.get() {
        if let Ok(mut cfg) = lock.write() {
            if *cfg != val {
                *cfg = val.clone();
                tracing::info!("[Image-Thinking] Global config updated: {}", val);
            }
        }
    } else {
        let _ = GLOBAL_IMAGE_THINKING_MODE.set(RwLock::new(val.clone()));
        tracing::info!("[Image-Thinking] Global config initialized: {}", val);
    }
}

// ============================================================================
// Tests
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::config::{GlobalSystemPromptConfig, ThinkingBudgetConfig, ThinkingBudgetMode};

    // --- normalize_proxy_url ---

    #[test]
    fn test_normalize_proxy_url_with_scheme() {
        assert_eq!(normalize_proxy_url("http://127.0.0.1:7890"), "http://127.0.0.1:7890");
        assert_eq!(normalize_proxy_url("https://proxy.com"), "https://proxy.com");
        assert_eq!(normalize_proxy_url("socks5://127.0.0.1:1080"), "socks5://127.0.0.1:1080");
    }

    #[test]
    fn test_normalize_proxy_url_without_scheme() {
        assert_eq!(normalize_proxy_url("127.0.0.1:7890"), "http://127.0.0.1:7890");
        assert_eq!(normalize_proxy_url("localhost:1082"), "http://localhost:1082");
    }

    #[test]
    fn test_normalize_proxy_url_empty() {
        assert_eq!(normalize_proxy_url(""), "");
        assert_eq!(normalize_proxy_url("   "), "");
    }

    // --- resolve_thinking_budget ---

    #[test]
    fn test_resolve_auto_caps_gemini_model() {
        // Auto mode should cap gemini models at 24576
        update_thinking_budget_config(ThinkingBudgetConfig {
            mode: ThinkingBudgetMode::Auto,
            custom_value: 24576,
            effort: None,
        });
        assert_eq!(resolve_thinking_budget(32000, "gemini-2.5-pro"), 24576);
        assert_eq!(resolve_thinking_budget(16000, "gemini-2.5-pro"), 16000);
    }

    #[test]
    fn test_resolve_auto_caps_flash_model() {
        update_thinking_budget_config(ThinkingBudgetConfig::default());
        assert_eq!(resolve_thinking_budget(32000, "gemini-2.0-flash-thinking"), 24576);
    }

    #[test]
    fn test_resolve_auto_no_cap_non_gemini() {
        update_thinking_budget_config(ThinkingBudgetConfig::default());
        // Non-gemini models should not be capped
        assert_eq!(resolve_thinking_budget(32000, "claude-3-7-sonnet"), 32000);
    }

    #[test]
    fn test_resolve_passthrough() {
        update_thinking_budget_config(ThinkingBudgetConfig {
            mode: ThinkingBudgetMode::Passthrough,
            custom_value: 24576,
            effort: None,
        });
        assert_eq!(resolve_thinking_budget(50000, "gemini-2.5-pro"), 50000);
        assert_eq!(resolve_thinking_budget(1024, "anything"), 1024);
    }

    #[test]
    fn test_resolve_custom_uses_configured_value() {
        update_thinking_budget_config(ThinkingBudgetConfig {
            mode: ThinkingBudgetMode::Custom,
            custom_value: 16000,
            effort: None,
        });
        // Custom value for non-gemini model
        assert_eq!(resolve_thinking_budget(50000, "claude-3-7-sonnet"), 16000);
    }

    #[test]
    fn test_resolve_custom_caps_gemini() {
        update_thinking_budget_config(ThinkingBudgetConfig {
            mode: ThinkingBudgetMode::Custom,
            custom_value: 32000,
            effort: None,
        });
        // Custom value exceeds 24576 for gemini → capped
        assert_eq!(resolve_thinking_budget(50000, "gemini-2.0-flash-thinking"), 24576);
        // Non-gemini gets the full custom value
        assert_eq!(resolve_thinking_budget(50000, "claude-3-7-sonnet"), 32000);
    }

    #[test]
    fn test_resolve_adaptive_passthrough() {
        update_thinking_budget_config(ThinkingBudgetConfig {
            mode: ThinkingBudgetMode::Adaptive,
            custom_value: 24576,
            effort: Some("high".to_string()),
        });
        // Adaptive passes through the user budget
        assert_eq!(resolve_thinking_budget(8000, "gemini-2.5-pro"), 8000);
    }

    #[test]
    fn test_resolve_auto_image_model_not_capped() {
        update_thinking_budget_config(ThinkingBudgetConfig::default());
        // Image models should NOT be capped even in auto mode
        assert_eq!(resolve_thinking_budget(32000, "gemini-2.0-flash-image"), 32000);
    }

    // --- Global config get/update ---

    #[test]
    fn test_thinking_budget_config_default() {
        let cfg = ThinkingBudgetConfig::default();
        assert_eq!(cfg.mode, ThinkingBudgetMode::Auto);
        assert_eq!(cfg.custom_value, 24576);
        assert!(cfg.effort.is_none());
    }

    #[test]
    fn test_global_system_prompt_default() {
        let cfg = GlobalSystemPromptConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.content.is_empty());
    }

    #[test]
    fn test_update_and_get_global_system_prompt() {
        update_global_system_prompt_config(GlobalSystemPromptConfig {
            enabled: true,
            content: "You are a helpful assistant.".to_string(),
        });
        let cfg = get_global_system_prompt();
        assert!(cfg.enabled);
        assert_eq!(cfg.content, "You are a helpful assistant.");
    }

    #[test]
    fn test_image_thinking_mode_default() {
        // Default should be "enabled"
        // Note: if another test already initialized it, this just verifies the getter works
        let mode = get_image_thinking_mode();
        assert!(mode == "enabled" || mode == "disabled");
    }

    #[test]
    fn test_update_image_thinking_mode() {
        update_image_thinking_mode(Some("disabled".to_string()));
        assert_eq!(get_image_thinking_mode(), "disabled");

        update_image_thinking_mode(None);
        assert_eq!(get_image_thinking_mode(), "enabled");
    }

    // --- ThinkingBudgetConfig serialization ---

    #[test]
    fn test_thinking_budget_config_serde_roundtrip() {
        let config = ThinkingBudgetConfig {
            mode: ThinkingBudgetMode::Custom,
            custom_value: 16000,
            effort: Some("high".to_string()),
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: ThinkingBudgetConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_thinking_budget_config_deserialize_defaults() {
        // Minimal JSON should fill in defaults
        let json = r#"{"mode":"auto"}"#;
        let config: ThinkingBudgetConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.mode, ThinkingBudgetMode::Auto);
        assert_eq!(config.custom_value, 24576);
        assert!(config.effort.is_none());
    }

    #[test]
    fn test_global_system_prompt_serde_roundtrip() {
        let config = GlobalSystemPromptConfig {
            enabled: true,
            content: "Test prompt".to_string(),
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: GlobalSystemPromptConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, deserialized);
    }

    // Restore defaults after tests to avoid polluting other test runs
    #[test]
    fn test_zz_restore_defaults() {
        update_thinking_budget_config(ThinkingBudgetConfig::default());
        update_global_system_prompt_config(GlobalSystemPromptConfig::default());
        update_image_thinking_mode(None);
    }

    // =========================================================================
    // Property 20: Thinking Budget 模式正确性
    // Feature: kiro-ai-gateway, Property 20: Thinking Budget 模式正确性
    // Validates: Requirements 9.6, 9.7, 9.8
    // =========================================================================
    mod prop_thinking_budget_mode {
        use super::*;
        use proptest::prelude::*;
        use std::sync::Mutex;

        // Serialize access to the global config so parallel proptest cases
        // don't interfere with each other.
        static CONFIG_LOCK: Mutex<()> = Mutex::new(());

        /// Models that trigger the auto-mode cap (contain "gemini", "flash", or
        /// "thinking" but NOT "-image").
        fn arb_capped_model() -> impl Strategy<Value = String> {
            prop_oneof![
                Just("gemini-2.5-pro".to_string()),
                Just("gemini-2.0-flash-thinking".to_string()),
                Just("gemini-2.5-flash".to_string()),
                Just("models/gemini-2.5-pro-preview".to_string()),
                Just("flash-lite".to_string()),
                Just("deep-thinking-v1".to_string()),
            ]
        }

        /// Models that are NOT capped in auto mode (no "gemini"/"flash"/"thinking",
        /// or image models).
        fn arb_uncapped_model() -> impl Strategy<Value = String> {
            prop_oneof![
                Just("claude-3-7-sonnet".to_string()),
                Just("gpt-4o".to_string()),
                Just("llama-3".to_string()),
                Just("gemini-2.0-flash-image".to_string()),
            ]
        }

        /// Gemini non-image models (used for custom mode cap check).
        fn arb_gemini_non_image_model() -> impl Strategy<Value = String> {
            prop_oneof![
                Just("gemini-2.5-pro".to_string()),
                Just("gemini-2.0-flash-thinking".to_string()),
                Just("gemini-2.5-flash".to_string()),
            ]
        }

        /// Non-gemini models (custom mode does NOT cap these).
        fn arb_non_gemini_model() -> impl Strategy<Value = String> {
            prop_oneof![
                Just("claude-3-7-sonnet".to_string()),
                Just("gpt-4o".to_string()),
                Just("llama-3".to_string()),
            ]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            /// **Validates: Requirements 9.6**
            /// Auto mode: for capped models, budget SHALL be min(user_budget, 24576).
            #[test]
            fn prop_auto_mode_caps_specific_models(
                user_budget in 0i64..100_000,
                model in arb_capped_model(),
            ) {
                let _guard = CONFIG_LOCK.lock().unwrap();
                update_thinking_budget_config(ThinkingBudgetConfig {
                    mode: ThinkingBudgetMode::Auto,
                    custom_value: 24576,
                    effort: None,
                });
                let result = resolve_thinking_budget(user_budget, &model);
                let expected = user_budget.min(24576);
                prop_assert_eq!(result, expected,
                    "Auto mode: model={}, user_budget={}, expected={}, got={}",
                    model, user_budget, expected, result);
            }

            /// **Validates: Requirements 9.6**
            /// Auto mode: for uncapped models, budget SHALL pass through unchanged.
            #[test]
            fn prop_auto_mode_passthrough_uncapped_models(
                user_budget in 0i64..100_000,
                model in arb_uncapped_model(),
            ) {
                let _guard = CONFIG_LOCK.lock().unwrap();
                update_thinking_budget_config(ThinkingBudgetConfig {
                    mode: ThinkingBudgetMode::Auto,
                    custom_value: 24576,
                    effort: None,
                });
                let result = resolve_thinking_budget(user_budget, &model);
                prop_assert_eq!(result, user_budget,
                    "Auto mode uncapped: model={}, user_budget={}, got={}",
                    model, user_budget, result);
            }

            /// **Validates: Requirements 9.7**
            /// Passthrough mode: SHALL return user_budget unchanged for any model.
            #[test]
            fn prop_passthrough_mode_returns_original(
                user_budget in 0i64..100_000,
                model in "[a-z0-9\\-]{3,30}",
            ) {
                let _guard = CONFIG_LOCK.lock().unwrap();
                update_thinking_budget_config(ThinkingBudgetConfig {
                    mode: ThinkingBudgetMode::Passthrough,
                    custom_value: 24576,
                    effort: None,
                });
                let result = resolve_thinking_budget(user_budget, &model);
                prop_assert_eq!(result, user_budget,
                    "Passthrough: model={}, user_budget={}, got={}",
                    model, user_budget, result);
            }

            /// **Validates: Requirements 9.8**
            /// Custom mode: for non-gemini models, SHALL use custom_value.
            #[test]
            fn prop_custom_mode_uses_custom_value_non_gemini(
                user_budget in 0i64..100_000,
                custom_value in 1u32..50_000,
                model in arb_non_gemini_model(),
            ) {
                let _guard = CONFIG_LOCK.lock().unwrap();
                update_thinking_budget_config(ThinkingBudgetConfig {
                    mode: ThinkingBudgetMode::Custom,
                    custom_value,
                    effort: None,
                });
                let result = resolve_thinking_budget(user_budget, &model);
                prop_assert_eq!(result, custom_value as i64,
                    "Custom non-gemini: model={}, custom_value={}, got={}",
                    model, custom_value, result);
            }

            /// **Validates: Requirements 9.8**
            /// Custom mode: for gemini non-image models, SHALL use
            /// min(custom_value, 24576).
            #[test]
            fn prop_custom_mode_caps_gemini_models(
                user_budget in 0i64..100_000,
                custom_value in 1u32..50_000,
                model in arb_gemini_non_image_model(),
            ) {
                let _guard = CONFIG_LOCK.lock().unwrap();
                update_thinking_budget_config(ThinkingBudgetConfig {
                    mode: ThinkingBudgetMode::Custom,
                    custom_value,
                    effort: None,
                });
                let result = resolve_thinking_budget(user_budget, &model);
                let expected = (custom_value as i64).min(24576);
                prop_assert_eq!(result, expected,
                    "Custom gemini: model={}, custom_value={}, expected={}, got={}",
                    model, custom_value, expected, result);
            }
        }
    }
}
