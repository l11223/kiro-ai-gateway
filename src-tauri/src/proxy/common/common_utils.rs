// Common utilities for request mapping across all protocols
// Provides unified grounding/networking logic and shared conversion functions
//
// Requirements covered:
// - 2.3: Gemini native format support
// - 2.10: Image generation support

use serde_json::{json, Value};

/// Request configuration after grounding resolution
#[derive(Debug, Clone)]
pub struct RequestConfig {
    /// The request type: "agent", "web_search", or "image_gen"
    pub request_type: String,
    /// Whether to inject the googleSearch tool
    pub inject_google_search: bool,
    /// The final model name (with suffixes stripped)
    pub final_model: String,
    /// Image generation configuration (if request_type is image_gen)
    pub image_config: Option<Value>,
}

/// Resolve request configuration based on model name, tools, and image parameters.
///
/// Determines the request type (agent/web_search/image_gen), whether to inject
/// google search tools, and the final model name for upstream.
pub fn resolve_request_config(
    original_model: &str,
    mapped_model: &str,
    tools: &Option<Vec<Value>>,
    size: Option<&str>,
    quality: Option<&str>,
    image_size: Option<&str>,
    body: Option<&Value>,
) -> RequestConfig {
    // Image generation check (highest priority)
    if mapped_model.starts_with("gemini-3-pro-image") {
        let (mut inferred_config, parsed_base_model) =
            parse_image_config_with_params(original_model, size, quality, image_size);

        // Merge with imageConfig from Gemini request body if present
        if let Some(body_val) = body {
            if let Some(gen_config) = body_val.get("generationConfig") {
                if let Some(body_image_config) = gen_config.get("imageConfig") {
                    if let Some(inferred_obj) = inferred_config.as_object_mut() {
                        if let Some(body_obj) = body_image_config.as_object() {
                            for (key, value) in body_obj {
                                // Shield inferred imageSize from body downgrade
                                let is_size_downgrade = key == "imageSize"
                                    && (value.as_str() == Some("1K") || value.is_null())
                                    && inferred_obj.contains_key("imageSize");

                                if !is_size_downgrade {
                                    inferred_obj.insert(key.clone(), value.clone());
                                }
                            }
                        }
                    }
                }
            }
        }

        return RequestConfig {
            request_type: "image_gen".to_string(),
            inject_google_search: false,
            final_model: parsed_base_model,
            image_config: Some(inferred_config),
        };
    }

    let has_networking_tool = detects_networking_tool(tools);
    let is_online_suffix = original_model.ends_with("-online");
    let enable_networking = is_online_suffix || has_networking_tool;

    let mut final_model = mapped_model.trim_end_matches("-online").to_string();

    // Map preview aliases to physical model names
    final_model = match final_model.as_str() {
        "gemini-3-pro-preview" => "gemini-3-pro-high".to_string(),
        "gemini-3-pro-image-preview" => "gemini-3-pro-image".to_string(),
        "gemini-3-flash-preview" => "gemini-3-flash".to_string(),
        _ => final_model,
    };

    if enable_networking && final_model != "gemini-2.5-flash" {
        final_model = "gemini-2.5-flash".to_string();
    }

    RequestConfig {
        request_type: if enable_networking {
            "web_search".to_string()
        } else {
            "agent".to_string()
        },
        inject_google_search: enable_networking,
        final_model,
        image_config: None,
    }
}

/// Parse image config from model name suffixes and optional OpenAI parameters.
///
/// Priority: direct imageSize > quality param > model suffix
pub fn parse_image_config(model_name: &str) -> (Value, String) {
    parse_image_config_with_params(model_name, None, None, None)
}

/// Extended version that accepts OpenAI size and quality parameters.
///
/// Supports parsing image configuration from:
/// 1. Direct imageSize parameter - highest priority
/// 2. OpenAI API parameters (size, quality) - medium priority
/// 3. Model name suffixes (e.g., -16x9, -4k) - fallback
pub fn parse_image_config_with_params(
    model_name: &str,
    size: Option<&str>,
    quality: Option<&str>,
    image_size: Option<&str>,
) -> (Value, String) {
    let mut aspect_ratio = "1:1";

    if let Some(s) = size {
        aspect_ratio = calculate_aspect_ratio_from_size(s);
    } else {
        let suffixes = [
            ("-21x9", "21:9"), ("-21-9", "21:9"),
            ("-16x9", "16:9"), ("-16-9", "16:9"),
            ("-9x16", "9:16"), ("-9-16", "9:16"),
            ("-4x3", "4:3"),   ("-4-3", "4:3"),
            ("-3x4", "3:4"),   ("-3-4", "3:4"),
            ("-3x2", "3:2"),   ("-3-2", "3:2"),
            ("-2x3", "2:3"),   ("-2-3", "2:3"),
            ("-5x4", "5:4"),   ("-5-4", "5:4"),
            ("-4x5", "4:5"),   ("-4-5", "4:5"),
            ("-1x1", "1:1"),   ("-1-1", "1:1"),
        ];
        for (suffix, ratio) in &suffixes {
            if model_name.contains(suffix) {
                aspect_ratio = ratio;
                break;
            }
        }
    }

    let mut config = serde_json::Map::new();
    config.insert("aspectRatio".to_string(), json!(aspect_ratio));

    if let Some(is) = image_size {
        config.insert("imageSize".to_string(), json!(is.to_uppercase()));
    } else if let Some(q) = quality {
        match q.to_lowercase().as_str() {
            "hd" | "4k" => { config.insert("imageSize".to_string(), json!("4K")); }
            "medium" | "2k" => { config.insert("imageSize".to_string(), json!("2K")); }
            "standard" | "1k" => { config.insert("imageSize".to_string(), json!("1K")); }
            _ => {}
        }
    } else {
        if model_name.contains("-4k") || model_name.contains("-hd") {
            config.insert("imageSize".to_string(), json!("4K"));
        } else if model_name.contains("-2k") {
            config.insert("imageSize".to_string(), json!("2K"));
        }
    }

    (Value::Object(config), "gemini-3-pro-image".to_string())
}

/// Calculate aspect ratio from "WIDTHxHEIGHT" or "W:H" size string.
pub fn calculate_aspect_ratio_from_size(size: &str) -> &'static str {
    match size {
        "21:9" => return "21:9",
        "16:9" => return "16:9",
        "9:16" => return "9:16",
        "4:3" => return "4:3",
        "3:4" => return "3:4",
        "3:2" => return "3:2",
        "2:3" => return "2:3",
        "5:4" => return "5:4",
        "4:5" => return "4:5",
        "1:1" => return "1:1",
        _ => {}
    }

    if let Some((w_str, h_str)) = size.split_once('x') {
        if let (Ok(width), Ok(height)) = (w_str.parse::<f64>(), h_str.parse::<f64>()) {
            if width > 0.0 && height > 0.0 {
                let ratio = width / height;
                let ratios: &[(&str, f64)] = &[
                    ("21:9", 21.0 / 9.0),
                    ("16:9", 16.0 / 9.0),
                    ("4:3", 4.0 / 3.0),
                    ("3:4", 3.0 / 4.0),
                    ("9:16", 9.0 / 16.0),
                    ("3:2", 3.0 / 2.0),
                    ("2:3", 2.0 / 3.0),
                    ("5:4", 5.0 / 4.0),
                    ("4:5", 4.0 / 5.0),
                    ("1:1", 1.0),
                ];
                for (name, target) in ratios {
                    if (ratio - target).abs() < 0.05 {
                        return name;
                    }
                }
            }
        }
    }

    "1:1"
}

/// Inject googleSearch tool into the request body.
///
/// Skips injection if functionDeclarations already exist (incompatible).
/// Removes existing search tools to prevent duplicates.
pub fn inject_google_search_tool(body: &mut Value) {
    if let Some(obj) = body.as_object_mut() {
        let tools_entry = obj.entry("tools").or_insert_with(|| json!([]));
        if let Some(tools_arr) = tools_entry.as_array_mut() {
            let has_functions = tools_arr.iter().any(|t| {
                t.as_object()
                    .map_or(false, |o| o.contains_key("functionDeclarations"))
            });

            if has_functions {
                return;
            }

            tools_arr.retain(|t| {
                if let Some(o) = t.as_object() {
                    !(o.contains_key("googleSearch") || o.contains_key("googleSearchRetrieval"))
                } else {
                    true
                }
            });

            tools_arr.push(json!({ "googleSearch": {} }));
        }
    }
}

/// Deep clean `[undefined]` strings from client payloads.
///
/// Some clients (e.g. Cherry Studio) inject `[undefined]` as placeholder values
/// which cause Gemini API validation failures.
pub fn deep_clean_undefined(value: &mut Value, depth: usize) {
    if depth > 10 {
        return;
    }
    match value {
        Value::Object(map) => {
            map.retain(|_, v| {
                if let Some(s) = v.as_str() {
                    s != "[undefined]"
                } else {
                    true
                }
            });
            for v in map.values_mut() {
                deep_clean_undefined(v, depth + 1);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                deep_clean_undefined(v, depth + 1);
            }
        }
        _ => {}
    }
}

/// Detect if the tool list contains a web search request.
///
/// Supports multiple tool declaration formats:
/// - Direct: `{ "name": "web_search" }`
/// - OpenAI nested: `{ "type": "function", "function": { "name": "web_search" } }`
/// - Gemini native: `{ "functionDeclarations": [{ "name": "google_search" }] }`
/// - Gemini search: `{ "googleSearch": {} }`
pub fn detects_networking_tool(tools: &Option<Vec<Value>>) -> bool {
    let keywords = [
        "web_search",
        "google_search",
        "web_search_20250305",
        "google_search_retrieval",
    ];

    if let Some(list) = tools {
        for tool in list {
            if let Some(n) = tool.get("name").and_then(|v| v.as_str()) {
                if keywords.contains(&n) { return true; }
            }
            if let Some(t) = tool.get("type").and_then(|v| v.as_str()) {
                if keywords.contains(&t) { return true; }
            }
            if let Some(func) = tool.get("function") {
                if let Some(n) = func.get("name").and_then(|v| v.as_str()) {
                    if keywords.contains(&n) { return true; }
                }
            }
            if let Some(decls) = tool.get("functionDeclarations").and_then(|v| v.as_array()) {
                for decl in decls {
                    if let Some(n) = decl.get("name").and_then(|v| v.as_str()) {
                        if keywords.contains(&n) { return true; }
                    }
                }
            }
            if tool.get("googleSearch").is_some() || tool.get("googleSearchRetrieval").is_some() {
                return true;
            }
        }
    }
    false
}

/// Detect if the tool list contains non-networking (local) function tools.
pub fn contains_non_networking_tool(tools: &Option<Vec<Value>>) -> bool {
    let keywords = [
        "web_search",
        "google_search",
        "web_search_20250305",
        "google_search_retrieval",
    ];

    if let Some(list) = tools {
        for tool in list {
            let mut is_networking = false;

            if let Some(n) = tool.get("name").and_then(|v| v.as_str()) {
                if keywords.contains(&n) {
                    is_networking = true;
                }
            } else if let Some(func) = tool.get("function") {
                if let Some(n) = func.get("name").and_then(|v| v.as_str()) {
                    if keywords.contains(&n) {
                        is_networking = true;
                    }
                }
            } else if tool.get("googleSearch").is_some()
                || tool.get("googleSearchRetrieval").is_some()
            {
                is_networking = true;
            } else if let Some(decls) =
                tool.get("functionDeclarations").and_then(|v| v.as_array())
            {
                for decl in decls {
                    if let Some(n) = decl.get("name").and_then(|v| v.as_str()) {
                        if !keywords.contains(&n) {
                            return true;
                        }
                    }
                }
                is_networking = true;
            }

            if !is_networking {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_agent_request() {
        let config =
            resolve_request_config("gpt-4o", "gemini-2.5-flash", &None, None, None, None, None);
        assert_eq!(config.request_type, "agent");
        assert!(!config.inject_google_search);
    }

    #[test]
    fn test_resolve_online_suffix() {
        let config = resolve_request_config(
            "gemini-3-flash-online",
            "gemini-3-flash",
            &None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(config.request_type, "web_search");
        assert!(config.inject_google_search);
        assert_eq!(config.final_model, "gemini-2.5-flash");
    }

    #[test]
    fn test_resolve_image_model() {
        let config = resolve_request_config(
            "gemini-3-pro-image",
            "gemini-3-pro-image",
            &None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(config.request_type, "image_gen");
        assert!(!config.inject_google_search);
        assert!(config.image_config.is_some());
    }

    #[test]
    fn test_image_config_body_merge() {
        let body = json!({
            "generationConfig": {
                "imageConfig": {
                    "aspectRatio": "1:1",
                    "imageSize": "1K"
                }
            }
        });
        let config = resolve_request_config(
            "gemini-3-pro-image-4k",
            "gemini-3-pro-image",
            &None,
            None,
            None,
            None,
            Some(&body),
        );
        let ic = config.image_config.unwrap();
        assert_eq!(ic["imageSize"], "4K"); // Shield from downgrade
        assert_eq!(ic["aspectRatio"], "1:1"); // Body override allowed
    }

    #[test]
    fn test_parse_image_config_suffix() {
        let (config, _) = parse_image_config("gemini-3-pro-image-16x9-4k");
        assert_eq!(config["aspectRatio"], "16:9");
        assert_eq!(config["imageSize"], "4K");
    }

    #[test]
    fn test_parse_image_config_openai_params() {
        let (config, _) =
            parse_image_config_with_params("gemini-3-pro-image", Some("1920x1080"), Some("hd"), None);
        assert_eq!(config["aspectRatio"], "16:9");
        assert_eq!(config["imageSize"], "4K");
    }

    #[test]
    fn test_image_size_priority() {
        let (config, _) =
            parse_image_config_with_params("gemini-3-pro-image", None, Some("standard"), Some("4K"));
        assert_eq!(config["imageSize"], "4K"); // Direct imageSize wins
    }

    #[test]
    fn test_calculate_aspect_ratio() {
        assert_eq!(calculate_aspect_ratio_from_size("1280x720"), "16:9");
        assert_eq!(calculate_aspect_ratio_from_size("1024x1024"), "1:1");
        assert_eq!(calculate_aspect_ratio_from_size("720x1280"), "9:16");
        assert_eq!(calculate_aspect_ratio_from_size("800x600"), "4:3");
        assert_eq!(calculate_aspect_ratio_from_size("1500x1000"), "3:2");
        assert_eq!(calculate_aspect_ratio_from_size("16:9"), "16:9");
        assert_eq!(calculate_aspect_ratio_from_size("invalid"), "1:1");
    }

    #[test]
    fn test_deep_clean_undefined() {
        let mut val = json!({
            "key1": "valid",
            "key2": "[undefined]",
            "nested": {"key3": "[undefined]", "key4": "ok"},
            "arr": ["good", "[undefined]"]
        });
        deep_clean_undefined(&mut val, 0);
        assert!(val.get("key1").is_some());
        assert!(val.get("key2").is_none());
        assert!(val["nested"].get("key3").is_none());
        assert!(val["nested"].get("key4").is_some());
    }

    #[test]
    fn test_detects_networking_tool_various_formats() {
        // Direct name
        assert!(detects_networking_tool(&Some(vec![json!({"name": "web_search"})])));
        // Type field
        assert!(detects_networking_tool(&Some(vec![json!({"type": "google_search"})])));
        // OpenAI nested
        assert!(detects_networking_tool(&Some(vec![json!({
            "type": "function",
            "function": {"name": "web_search"}
        })])));
        // Gemini native
        assert!(detects_networking_tool(&Some(vec![json!({
            "functionDeclarations": [{"name": "google_search"}]
        })])));
        // Gemini search declaration
        assert!(detects_networking_tool(&Some(vec![json!({"googleSearch": {}})])));
        // Non-networking
        assert!(!detects_networking_tool(&Some(vec![json!({"name": "get_weather"})])));
        assert!(!detects_networking_tool(&None));
    }

    #[test]
    fn test_contains_non_networking_tool() {
        assert!(contains_non_networking_tool(&Some(vec![
            json!({"name": "get_weather"})
        ])));
        assert!(!contains_non_networking_tool(&Some(vec![
            json!({"name": "web_search"})
        ])));
        assert!(!contains_non_networking_tool(&None));
    }

    #[test]
    fn test_inject_google_search_tool() {
        let mut body = json!({"contents": []});
        inject_google_search_tool(&mut body);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert!(tools[0].get("googleSearch").is_some());
    }

    #[test]
    fn test_inject_google_search_skips_with_functions() {
        let mut body = json!({
            "tools": [{"functionDeclarations": [{"name": "my_func"}]}]
        });
        inject_google_search_tool(&mut body);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert!(tools[0].get("functionDeclarations").is_some());
    }

    #[test]
    fn test_preview_model_mapping() {
        let config = resolve_request_config(
            "gemini-3-pro-preview",
            "gemini-3-pro-preview",
            &None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(config.final_model, "gemini-3-pro-high");
    }
}
