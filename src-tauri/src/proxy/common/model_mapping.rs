// Model name mapping and normalization
//
// Requirements covered:
// - 3.1: Exact mapping via custom_mapping HashMap
// - 3.2: Wildcard mapping via series rules (gemini-*, claude-*, etc.)
// - 3.4: Background task (warmup) auto-downgrade to Flash models
// - 3.5: normalize_to_standard_id() - idempotent model name standardization

use once_cell::sync::Lazy;
use std::collections::HashMap;

/// Built-in model alias mapping table.
/// Maps various model names (Claude, OpenAI, Gemini aliases) to their target Gemini model IDs.
static BUILTIN_MODEL_MAP: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();

    // Claude direct support
    m.insert("claude-sonnet-4-5", "claude-sonnet-4-5");
    m.insert("claude-sonnet-4-5-thinking", "claude-sonnet-4-5-thinking");

    // Claude alias mappings
    m.insert("claude-sonnet-4-5-20250929", "claude-sonnet-4-5-thinking");
    m.insert("claude-3-5-sonnet-20241022", "claude-sonnet-4-5");
    m.insert("claude-3-5-sonnet-20240620", "claude-sonnet-4-5");
    m.insert("claude-opus-4", "claude-opus-4-6-thinking");
    m.insert("claude-opus-4-5-thinking", "claude-opus-4-6-thinking");
    m.insert("claude-opus-4-5-20251101", "claude-opus-4-6-thinking");
    m.insert("claude-opus-4-6-thinking", "claude-opus-4-6-thinking");
    m.insert("claude-opus-4-6", "claude-opus-4-6-thinking");
    m.insert("claude-opus-4-6-20260201", "claude-opus-4-6-thinking");
    m.insert("claude-haiku-4", "claude-sonnet-4-5");
    m.insert("claude-3-haiku-20240307", "claude-sonnet-4-5");
    m.insert("claude-haiku-4-5-20251001", "claude-sonnet-4-5");

    // OpenAI alias mappings -> Gemini Flash
    m.insert("gpt-4", "gemini-2.5-flash");
    m.insert("gpt-4-turbo", "gemini-2.5-flash");
    m.insert("gpt-4-turbo-preview", "gemini-2.5-flash");
    m.insert("gpt-4-0125-preview", "gemini-2.5-flash");
    m.insert("gpt-4-1106-preview", "gemini-2.5-flash");
    m.insert("gpt-4-0613", "gemini-2.5-flash");
    m.insert("gpt-4o", "gemini-2.5-flash");
    m.insert("gpt-4o-2024-05-13", "gemini-2.5-flash");
    m.insert("gpt-4o-2024-08-06", "gemini-2.5-flash");
    m.insert("gpt-4o-mini", "gemini-2.5-flash");
    m.insert("gpt-4o-mini-2024-07-18", "gemini-2.5-flash");
    m.insert("gpt-3.5-turbo", "gemini-2.5-flash");
    m.insert("gpt-3.5-turbo-16k", "gemini-2.5-flash");
    m.insert("gpt-3.5-turbo-0125", "gemini-2.5-flash");
    m.insert("gpt-3.5-turbo-1106", "gemini-2.5-flash");
    m.insert("gpt-3.5-turbo-0613", "gemini-2.5-flash");

    // Gemini alias mappings
    m.insert("gemini-2.5-flash-lite", "gemini-2.5-flash");
    m.insert("gemini-2.5-flash-thinking", "gemini-2.5-flash-thinking");
    m.insert("gemini-3-pro-low", "gemini-3-pro-preview");
    m.insert("gemini-3-pro-high", "gemini-3-pro-preview");
    m.insert("gemini-3-pro-preview", "gemini-3-pro-preview");
    m.insert("gemini-3-pro", "gemini-3-pro-preview");
    m.insert("gemini-2.5-flash", "gemini-2.5-flash");
    m.insert("gemini-3-flash", "gemini-3-flash");
    m.insert("gemini-3-pro-image", "gemini-3-pro-image");

    // Virtual ID for background tasks
    m.insert("internal-background-task", "gemini-2.5-flash");

    m
});

/// Flash model used for warmup/background task downgrade (Requirement 3.4)
const WARMUP_FLASH_MODEL: &str = "gemini-2.5-flash";

/// Map a model name through the built-in alias table.
///
/// Strategy:
/// 1. Exact match in BUILTIN_MODEL_MAP
/// 2. Pass-through known prefixes (gemini-*, *-thinking)
/// 3. Pass-through unknown models (let upstream handle errors)
fn map_builtin(input: &str) -> String {
    if let Some(mapped) = BUILTIN_MODEL_MAP.get(input) {
        return mapped.to_string();
    }

    // Pass-through gemini models and thinking variants
    if input.starts_with("gemini-") || input.contains("thinking") {
        return input.to_string();
    }

    // Unknown models pass through directly
    input.to_string()
}

/// Wildcard matching - supports multiple `*` wildcards.
///
/// Case-sensitive. Examples:
/// - `gpt-4*` matches `gpt-4`, `gpt-4-turbo`
/// - `claude-*-sonnet-*` matches `claude-3-5-sonnet-20241022`
/// - `*-thinking` matches `claude-opus-4-5-thinking`
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();

    if parts.len() == 1 {
        return pattern == text;
    }

    let mut text_pos = 0;

    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }

        if i == 0 {
            // First segment must match start
            if !text[text_pos..].starts_with(part) {
                return false;
            }
            text_pos += part.len();
        } else if i == parts.len() - 1 {
            // Last segment must match end
            return text[text_pos..].ends_with(part);
        } else {
            // Middle segments - find next occurrence
            if let Some(pos) = text[text_pos..].find(part) {
                text_pos += pos + part.len();
            } else {
                return false;
            }
        }
    }

    true
}

/// Get the best wildcard match from custom_mapping.
/// When multiple patterns match, the most specific one wins
/// (highest count of non-wildcard characters).
///
/// Requirement 3.2: Wildcard mapping via series rules
pub fn get_wildcard_mapping(
    model: &str,
    custom_mapping: &HashMap<String, String>,
) -> Option<String> {
    let mut best_match: Option<(&str, &str, usize)> = None;

    for (pattern, target) in custom_mapping.iter() {
        if pattern.contains('*') && wildcard_match(pattern, model) {
            let specificity = pattern.chars().count() - pattern.matches('*').count();
            if best_match.is_none() || specificity > best_match.unwrap().2 {
                best_match = Some((pattern.as_str(), target.as_str(), specificity));
            }
        }
    }

    best_match.map(|(_, target, _)| target.to_string())
}

/// Core model routing function.
///
/// Priority: exact custom_mapping > wildcard custom_mapping > built-in mapping
///
/// When `is_warmup` is true, the result is silently downgraded to a Flash model
/// to conserve quota (Requirement 3.4).
///
/// # Arguments
/// - `model`: Original model name from the request
/// - `custom_mapping`: User-defined mapping table
/// - `is_warmup`: Whether this is a background/warmup request
///
/// # Returns
/// The mapped target model name
pub fn map_model(
    model: &str,
    custom_mapping: &HashMap<String, String>,
    is_warmup: bool,
) -> String {
    // Requirement 3.4: warmup requests downgrade to Flash
    if is_warmup {
        return WARMUP_FLASH_MODEL.to_string();
    }

    // Requirement 3.1: Exact match in custom_mapping (highest priority)
    if let Some(target) = custom_mapping.get(model) {
        return target.clone();
    }

    // Requirement 3.2: Wildcard match in custom_mapping
    if let Some(target) = get_wildcard_mapping(model, custom_mapping) {
        return target;
    }

    // Fall back to built-in mapping
    map_builtin(model)
}

/// Normalize any model name to a standard protection ID.
///
/// This is used for quota protection grouping - different model variants
/// that share the same quota pool are mapped to the same standard ID.
///
/// Requirement 3.5: Idempotent - applying twice yields the same result.
///
/// Standard IDs:
/// - `gemini-3-pro-image`: Image generation model
/// - `gemini-3-flash`: All Flash variants
/// - `gemini-3-pro-high`: All Pro variants (excluding image)
/// - `claude`: All Claude variants (Opus, Sonnet, Haiku)
///
/// Returns `None` if the model doesn't match any protected category.
pub fn normalize_to_standard_id(model_name: &str) -> Option<String> {
    let lower = model_name.to_lowercase();

    // 1. gemini-3-pro-image (match before generic "pro")
    if lower == "gemini-3-pro-image" {
        return Some("gemini-3-pro-image".to_string());
    }

    // 2. All flash variants
    if lower.contains("flash") {
        return Some("gemini-3-flash".to_string());
    }

    // 3. All pro variants (excluding image)
    if lower.contains("pro") && !lower.contains("image") {
        return Some("gemini-3-pro-high".to_string());
    }

    // 4. All Claude variants
    if lower.contains("claude")
        || lower.contains("opus")
        || lower.contains("sonnet")
        || lower.contains("haiku")
    {
        return Some("claude".to_string());
    }

    None
}

/// Get all built-in supported model names.
pub fn get_supported_models() -> Vec<String> {
    BUILTIN_MODEL_MAP.keys().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- map_model tests ---

    #[test]
    fn test_exact_custom_mapping() {
        let mut custom = HashMap::new();
        custom.insert("my-model".to_string(), "gemini-3-flash".to_string());

        assert_eq!(map_model("my-model", &custom, false), "gemini-3-flash");
    }

    #[test]
    fn test_custom_mapping_takes_priority_over_builtin() {
        let mut custom = HashMap::new();
        custom.insert("gpt-4".to_string(), "gemini-3-pro-preview".to_string());

        // custom_mapping overrides the built-in gpt-4 -> gemini-2.5-flash
        assert_eq!(map_model("gpt-4", &custom, false), "gemini-3-pro-preview");
    }

    #[test]
    fn test_warmup_always_returns_flash() {
        let mut custom = HashMap::new();
        custom.insert("my-model".to_string(), "gemini-3-pro-preview".to_string());

        // Even with exact custom mapping, warmup returns flash
        assert_eq!(map_model("my-model", &custom, true), WARMUP_FLASH_MODEL);
        assert_eq!(map_model("gpt-4", &custom, true), WARMUP_FLASH_MODEL);
        assert_eq!(
            map_model("unknown-model", &HashMap::new(), true),
            WARMUP_FLASH_MODEL
        );
    }

    #[test]
    fn test_builtin_claude_mapping() {
        let empty = HashMap::new();
        assert_eq!(
            map_model("claude-3-5-sonnet-20241022", &empty, false),
            "claude-sonnet-4-5"
        );
        assert_eq!(
            map_model("claude-opus-4", &empty, false),
            "claude-opus-4-6-thinking"
        );
    }

    #[test]
    fn test_builtin_openai_mapping() {
        let empty = HashMap::new();
        assert_eq!(map_model("gpt-4", &empty, false), "gemini-2.5-flash");
        assert_eq!(map_model("gpt-4o", &empty, false), "gemini-2.5-flash");
        assert_eq!(
            map_model("gpt-3.5-turbo", &empty, false),
            "gemini-2.5-flash"
        );
    }

    #[test]
    fn test_gemini_passthrough() {
        let empty = HashMap::new();
        assert_eq!(
            map_model("gemini-2.5-flash", &empty, false),
            "gemini-2.5-flash"
        );
        assert_eq!(
            map_model("gemini-2.5-flash-mini-test", &empty, false),
            "gemini-2.5-flash-mini-test"
        );
    }

    #[test]
    fn test_unknown_model_passthrough() {
        let empty = HashMap::new();
        assert_eq!(
            map_model("unknown-model", &empty, false),
            "unknown-model"
        );
    }

    // --- wildcard tests ---

    #[test]
    fn test_wildcard_mapping() {
        let mut custom = HashMap::new();
        custom.insert("gpt*".to_string(), "fallback".to_string());
        custom.insert("gpt-4*".to_string(), "specific".to_string());

        // More specific pattern wins
        assert_eq!(map_model("gpt-4-turbo", &custom, false), "specific");
        assert_eq!(map_model("gpt-3.5", &custom, false), "fallback");
    }

    #[test]
    fn test_wildcard_multi_segment() {
        let mut custom = HashMap::new();
        custom.insert(
            "claude-*-sonnet-*".to_string(),
            "sonnet-versioned".to_string(),
        );

        assert_eq!(
            map_model("claude-3-5-sonnet-20241022", &custom, false),
            "sonnet-versioned"
        );
    }

    #[test]
    fn test_wildcard_suffix() {
        let mut custom = HashMap::new();
        custom.insert("*-thinking".to_string(), "thinking-model".to_string());

        assert_eq!(
            map_model("claude-opus-4-5-thinking", &custom, false),
            "thinking-model"
        );
        // Should NOT match models without the suffix
        assert_eq!(
            map_model("claude-opus-4", &custom, false),
            "claude-opus-4-6-thinking" // falls through to builtin
        );
    }

    #[test]
    fn test_wildcard_catch_all() {
        let mut custom = HashMap::new();
        custom.insert("prefix*".to_string(), "prefix-match".to_string());
        custom.insert("*".to_string(), "catch-all".to_string());

        // Specificity: "prefix*" (6) > "*" (0)
        assert_eq!(map_model("prefix-anything", &custom, false), "prefix-match");
        // Catch-all matches everything
        assert_eq!(map_model("random-model", &custom, false), "catch-all");
    }

    #[test]
    fn test_wildcard_multi_star() {
        let mut custom = HashMap::new();
        custom.insert("a*b*c".to_string(), "multi-wild".to_string());

        assert_eq!(map_model("a-test-b-foo-c", &custom, false), "multi-wild");
        // Should not match if pattern doesn't fit
        assert_eq!(
            map_model("a-test-x-foo-y", &custom, false),
            "a-test-x-foo-y" // passthrough
        );
    }

    // --- normalize_to_standard_id tests ---

    #[test]
    fn test_normalize_flash_variants() {
        assert_eq!(
            normalize_to_standard_id("gemini-2.5-flash"),
            Some("gemini-3-flash".to_string())
        );
        assert_eq!(
            normalize_to_standard_id("gemini-3-flash"),
            Some("gemini-3-flash".to_string())
        );
        assert_eq!(
            normalize_to_standard_id("gemini-2.5-flash-thinking"),
            Some("gemini-3-flash".to_string())
        );
    }

    #[test]
    fn test_normalize_pro_variants() {
        assert_eq!(
            normalize_to_standard_id("gemini-3-pro-high"),
            Some("gemini-3-pro-high".to_string())
        );
        assert_eq!(
            normalize_to_standard_id("gemini-3-pro-preview"),
            Some("gemini-3-pro-high".to_string())
        );
    }

    #[test]
    fn test_normalize_pro_image_not_grouped_with_pro() {
        assert_eq!(
            normalize_to_standard_id("gemini-3-pro-image"),
            Some("gemini-3-pro-image".to_string())
        );
    }

    #[test]
    fn test_normalize_claude_variants() {
        assert_eq!(
            normalize_to_standard_id("claude-sonnet-4-5"),
            Some("claude".to_string())
        );
        assert_eq!(
            normalize_to_standard_id("claude-opus-4-6-thinking"),
            Some("claude".to_string())
        );
    }

    #[test]
    fn test_normalize_unknown_returns_none() {
        assert_eq!(normalize_to_standard_id("unknown-model"), None);
    }

    #[test]
    fn test_normalize_idempotent() {
        // Requirement 3.5: applying normalize twice yields the same result
        let models = vec![
            "gemini-2.5-flash",
            "gemini-3-pro-high",
            "gemini-3-pro-image",
            "claude-sonnet-4-5",
        ];

        for model in models {
            let first = normalize_to_standard_id(model);
            if let Some(ref standard_id) = first {
                let second = normalize_to_standard_id(standard_id);
                assert_eq!(first, second, "normalize is not idempotent for {}", model);
            }
        }
    }

    // --- wildcard_match unit tests ---

    #[test]
    fn test_wildcard_match_exact() {
        assert!(wildcard_match("hello", "hello"));
        assert!(!wildcard_match("hello", "world"));
    }

    #[test]
    fn test_wildcard_match_prefix() {
        assert!(wildcard_match("gpt-4*", "gpt-4"));
        assert!(wildcard_match("gpt-4*", "gpt-4-turbo"));
        assert!(!wildcard_match("gpt-4*", "gpt-3.5"));
    }

    #[test]
    fn test_wildcard_match_suffix() {
        assert!(wildcard_match("*-thinking", "claude-opus-4-5-thinking"));
        assert!(!wildcard_match("*-thinking", "claude-opus-4"));
    }

    #[test]
    fn test_wildcard_match_middle() {
        assert!(wildcard_match("claude-*-sonnet-*", "claude-3-5-sonnet-20241022"));
        assert!(!wildcard_match("claude-*-sonnet-*", "claude-3-5-opus-20241022"));
    }

    // --- Property-Based Tests ---

    mod prop_tests {
        use super::*;
        use proptest::prelude::*;
        use proptest::collection::hash_map;

        // **Feature: kiro-ai-gateway, Property 4: 模型名精确映射正确性**
        // **Validates: Requirements 3.1**
        //
        // For any model name and exact mapping table (custom_mapping HashMap),
        // if the model name exists in the mapping table, the mapping result
        // SHALL equal the corresponding value in the table.
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(200))]

            #[test]
            fn prop_exact_custom_mapping_correctness(
                // Generate a non-empty custom_mapping with 1..20 entries
                custom_mapping in hash_map("[a-zA-Z0-9._-]{1,30}", "[a-zA-Z0-9._-]{1,30}", 1..20usize),
            ) {
                // For every key in the mapping, map_model with is_warmup=false
                // must return the corresponding value
                for (model_name, expected_target) in &custom_mapping {
                    let result = map_model(model_name, &custom_mapping, false);
                    prop_assert_eq!(
                        &result,
                        expected_target,
                        "Exact mapping failed: model '{}' should map to '{}' but got '{}'",
                        model_name,
                        expected_target,
                        result
                    );
                }
            }
        }

        // **Feature: kiro-ai-gateway, Property 5: 模型名标准化幂等性**
        // **Validates: Requirements 3.5**
        //
        // For any model name, applying normalize_to_standard_id twice
        // SHALL produce the same result as applying it once (idempotency).
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(200))]

            #[test]
            fn prop_normalize_idempotent(
                model_name in prop::string::string_regex("[a-zA-Z0-9._-]{1,50}")
                    .unwrap()
            ) {
                let first = normalize_to_standard_id(&model_name);
                match &first {
                    Some(standard_id) => {
                        // Applying normalize a second time on the standard ID
                        // must yield the same standard ID.
                        let second = normalize_to_standard_id(standard_id);
                        prop_assert_eq!(
                            &first, &second,
                            "normalize is not idempotent for '{}': first={:?}, second={:?}",
                            model_name, first, second
                        );
                    }
                    None => {
                        // None is a valid fixed point — nothing more to check.
                    }
                }
            }
        }
    }

}
