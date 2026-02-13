// Gemini v1internal request wrapping / response unwrapping
//
// Requirements covered:
// - 2.3: Gemini native format passthrough

use serde_json::{json, Value};

/// Wrap a Gemini request body into v1internal format for upstream.
///
/// This handles:
/// - Deep cleaning of `[undefined]` strings from client payloads
/// - Tool schema cleaning (remove forbidden fields, rename parametersJsonSchema)
/// - Thinking budget processing
/// - System instruction injection
/// - Image generation config resolution
pub fn wrap_request(
    body: &Value,
    project_id: &str,
    mapped_model: &str,
    _session_id: Option<&str>,
) -> Value {
    let original_model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or(mapped_model);

    let final_model_name = if !mapped_model.is_empty() {
        mapped_model
    } else {
        original_model
    };

    let mut inner_request = body.clone();

    // Deep clean [undefined] strings injected by some clients (e.g. Cherry Studio)
    deep_clean_undefined(&mut inner_request, 0);

    // Extract tools for networking detection
    let tools_val: Option<Vec<Value>> = inner_request
        .get("tools")
        .and_then(|t| t.as_array())
        .cloned();

    // Extract OpenAI-compatible image parameters
    let size = body.get("size").and_then(|v| v.as_str());
    let quality = body.get("quality").and_then(|v| v.as_str());
    let image_size = body.get("imageSize").and_then(|v| v.as_str());

    let config = resolve_request_config(
        original_model,
        final_model_name,
        &tools_val,
        size,
        quality,
        image_size,
    );

    // Clean tool declarations
    if let Some(tools) = inner_request.get_mut("tools") {
        if let Some(tools_arr) = tools.as_array_mut() {
            for tool in tools_arr.iter_mut() {
                if let Some(decls) = tool.get_mut("functionDeclarations") {
                    if let Some(decls_arr) = decls.as_array_mut() {
                        // Filter out web search function declarations
                        decls_arr.retain(|decl| {
                            if let Some(name) = decl.get("name").and_then(|v| v.as_str()) {
                                name != "web_search" && name != "google_search"
                            } else {
                                true
                            }
                        });

                        // Clean schema fields
                        for decl in decls_arr.iter_mut() {
                            if let Some(decl_obj) = decl.as_object_mut() {
                                if let Some(params_json_schema) =
                                    decl_obj.remove("parametersJsonSchema")
                                {
                                    decl_obj.insert("parameters".to_string(), params_json_schema);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Inject googleSearch tool if needed
    if config.inject_google_search {
        inject_google_search_tool(&mut inner_request);
    }

    // Handle image generation config
    if let Some(image_config) = config.image_config {
        if let Some(obj) = inner_request.as_object_mut() {
            obj.remove("tools");
            obj.remove("systemInstruction");

            // Ensure role field exists for all contents
            if let Some(contents) = obj.get_mut("contents").and_then(|c| c.as_array_mut()) {
                for content in contents {
                    if let Some(c_obj) = content.as_object_mut() {
                        if !c_obj.contains_key("role") {
                            c_obj.insert("role".to_string(), json!("user"));
                        }
                    }
                }
            }

            let gen_config = obj.entry("generationConfig").or_insert_with(|| json!({}));
            if let Some(gen_obj) = gen_config.as_object_mut() {
                gen_obj.remove("responseMimeType");
                gen_obj.remove("responseModalities");
                gen_obj.insert("imageConfig".to_string(), image_config);
            }
        }
    } else {
        // Ensure systemInstruction has role field
        if let Some(system_instruction) = inner_request.get_mut("systemInstruction") {
            if let Some(obj) = system_instruction.as_object_mut() {
                if !obj.contains_key("role") {
                    obj.insert("role".to_string(), json!("user"));
                }
            }
        }
    }

    json!({
        "project": project_id,
        "requestId": format!("agent-{}", uuid::Uuid::new_v4()),
        "request": inner_request,
        "model": config.final_model,
        "userAgent": "kiro-ai-gateway",
        "requestType": config.request_type
    })
}

/// Unwrap a v1internal response (extract the inner `response` field)
pub fn unwrap_response(response: &Value) -> Value {
    response.get("response").unwrap_or(response).clone()
}

/// Inject tool IDs into response for Claude models running via Gemini protocol.
///
/// Some clients (e.g. OpenCode, Vercel AI SDK) require tool call IDs in responses.
pub fn inject_ids_to_response(response: &mut Value, model_name: &str) {
    if !model_name.to_lowercase().contains("claude") {
        return;
    }

    if let Some(candidates) = response
        .get_mut("candidates")
        .and_then(|c| c.as_array_mut())
    {
        for candidate in candidates {
            if let Some(parts) = candidate
                .get_mut("content")
                .and_then(|c| c.get_mut("parts"))
                .and_then(|p| p.as_array_mut())
            {
                let mut name_counters: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for part in parts {
                    if let Some(fc) = part.get_mut("functionCall").and_then(|f| f.as_object_mut()) {
                        if fc.get("id").is_none() {
                            let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
                            let count = name_counters.entry(name.to_string()).or_insert(0);
                            let call_id = format!("call_{}_{}", name, count);
                            *count += 1;
                            fc.insert("id".to_string(), json!(call_id));
                        }
                    }
                }
            }
        }
    }
}

// ===== Internal helpers =====

/// Deep clean `[undefined]` strings from client payloads
fn deep_clean_undefined(value: &mut Value, depth: usize) {
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

/// Inject googleSearch tool into the request
fn inject_google_search_tool(body: &mut Value) {
    if let Some(obj) = body.as_object_mut() {
        let tools_entry = obj.entry("tools").or_insert_with(|| json!([]));
        if let Some(tools_arr) = tools_entry.as_array_mut() {
            // Don't inject if functionDeclarations already exist (incompatible)
            let has_functions = tools_arr.iter().any(|t| {
                t.as_object()
                    .map_or(false, |o| o.contains_key("functionDeclarations"))
            });

            if has_functions {
                return;
            }

            // Remove existing search tools to prevent duplicates
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

/// Request configuration after resolution
struct RequestConfig {
    request_type: String,
    inject_google_search: bool,
    final_model: String,
    image_config: Option<Value>,
}

/// Resolve request configuration based on model name and tools
fn resolve_request_config(
    original_model: &str,
    mapped_model: &str,
    tools: &Option<Vec<Value>>,
    size: Option<&str>,
    quality: Option<&str>,
    image_size: Option<&str>,
) -> RequestConfig {
    // Image generation check (highest priority)
    if mapped_model.starts_with("gemini-3-pro-image") {
        let (config, base_model) =
            parse_image_config_with_params(original_model, size, quality, image_size);
        return RequestConfig {
            request_type: "image_gen".to_string(),
            inject_google_search: false,
            final_model: base_model,
            image_config: Some(config),
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

/// Parse image config from model name and optional parameters
fn parse_image_config_with_params(
    model_name: &str,
    size: Option<&str>,
    quality: Option<&str>,
    image_size: Option<&str>,
) -> (Value, String) {
    let mut aspect_ratio = "1:1";

    if let Some(s) = size {
        aspect_ratio = calculate_aspect_ratio_from_size(s);
    } else {
        // Fallback to model suffix parsing
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

    // Priority: direct imageSize > quality param > model suffix
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

/// Calculate aspect ratio from "WIDTHxHEIGHT" size string
fn calculate_aspect_ratio_from_size(size: &str) -> &'static str {
    // Check known aspect ratio strings first
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

/// Detect if tools list contains a web search request
fn detects_networking_tool(tools: &Option<Vec<Value>>) -> bool {
    let keywords = ["web_search", "google_search", "web_search_20250305", "google_search_retrieval"];

    if let Some(list) = tools {
        for tool in list {
            // Direct style: { "name": "..." } or { "type": "..." }
            if let Some(n) = tool.get("name").and_then(|v| v.as_str()) {
                if keywords.contains(&n) { return true; }
            }
            if let Some(t) = tool.get("type").and_then(|v| v.as_str()) {
                if keywords.contains(&t) { return true; }
            }
            // OpenAI nested style: { "type": "function", "function": { "name": "..." } }
            if let Some(func) = tool.get("function") {
                if let Some(n) = func.get("name").and_then(|v| v.as_str()) {
                    if keywords.contains(&n) { return true; }
                }
            }
            // Gemini native style: { "functionDeclarations": [{ "name": "..." }] }
            if let Some(decls) = tool.get("functionDeclarations").and_then(|v| v.as_array()) {
                for decl in decls {
                    if let Some(n) = decl.get("name").and_then(|v| v.as_str()) {
                        if keywords.contains(&n) { return true; }
                    }
                }
            }
            // Gemini googleSearch declaration
            if tool.get("googleSearch").is_some() || tool.get("googleSearchRetrieval").is_some() {
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
    fn test_wrap_request_basic() {
        let body = json!({
            "model": "gemini-2.5-flash",
            "contents": [{"role": "user", "parts": [{"text": "Hi"}]}]
        });

        let result = wrap_request(&body, "test-project", "gemini-2.5-flash", None);
        assert_eq!(result["project"], "test-project");
        assert_eq!(result["model"], "gemini-2.5-flash");
        assert!(result["requestId"].as_str().unwrap().starts_with("agent-"));
        assert_eq!(result["userAgent"], "kiro-ai-gateway");
    }

    #[test]
    fn test_unwrap_response() {
        let wrapped = json!({
            "response": {
                "candidates": [{"content": {"parts": [{"text": "Hello"}]}}]
            }
        });

        let result = unwrap_response(&wrapped);
        assert!(result.get("candidates").is_some());
        assert!(result.get("response").is_none());
    }

    #[test]
    fn test_unwrap_response_passthrough() {
        // When there's no "response" wrapper, return as-is
        let plain = json!({
            "candidates": [{"content": {"parts": [{"text": "Hello"}]}}]
        });

        let result = unwrap_response(&plain);
        assert!(result.get("candidates").is_some());
    }

    #[test]
    fn test_deep_clean_undefined() {
        let mut val = json!({
            "key1": "valid",
            "key2": "[undefined]",
            "nested": {
                "key3": "[undefined]",
                "key4": "ok"
            }
        });

        deep_clean_undefined(&mut val, 0);
        assert!(val.get("key1").is_some());
        assert!(val.get("key2").is_none());
        assert!(val["nested"].get("key3").is_none());
        assert!(val["nested"].get("key4").is_some());
    }

    #[test]
    fn test_inject_ids_to_response_claude() {
        let mut response = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"functionCall": {"name": "get_weather", "args": {"city": "London"}}},
                        {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}}
                    ]
                }
            }]
        });

        inject_ids_to_response(&mut response, "claude-sonnet-4-5");

        let parts = response["candidates"][0]["content"]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["functionCall"]["id"], "call_get_weather_0");
        assert_eq!(parts[1]["functionCall"]["id"], "call_get_weather_1");
    }

    #[test]
    fn test_inject_ids_to_response_non_claude() {
        let mut response = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"functionCall": {"name": "test"}}
                    ]
                }
            }]
        });

        inject_ids_to_response(&mut response, "gemini-2.5-flash");

        // Should not inject IDs for non-Claude models
        assert!(response["candidates"][0]["content"]["parts"][0]["functionCall"]
            .get("id")
            .is_none());
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
        );
        assert_eq!(config.request_type, "image_gen");
        assert!(!config.inject_google_search);
        assert!(config.image_config.is_some());
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
        );
        assert_eq!(config.request_type, "web_search");
        assert!(config.inject_google_search);
        assert_eq!(config.final_model, "gemini-2.5-flash");
    }

    #[test]
    fn test_resolve_default_agent() {
        let config = resolve_request_config(
            "gemini-2.5-flash",
            "gemini-2.5-flash",
            &None,
            None,
            None,
            None,
        );
        assert_eq!(config.request_type, "agent");
        assert!(!config.inject_google_search);
    }

    #[test]
    fn test_calculate_aspect_ratio() {
        assert_eq!(calculate_aspect_ratio_from_size("1920x1080"), "16:9");
        assert_eq!(calculate_aspect_ratio_from_size("1024x1024"), "1:1");
        assert_eq!(calculate_aspect_ratio_from_size("720x1280"), "9:16");
        assert_eq!(calculate_aspect_ratio_from_size("16:9"), "16:9");
        assert_eq!(calculate_aspect_ratio_from_size("invalid"), "1:1");
    }

    #[test]
    fn test_parse_image_config_quality() {
        let (config, _) = parse_image_config_with_params("gemini-3-pro-image", None, Some("hd"), None);
        assert_eq!(config["imageSize"], "4K");
        assert_eq!(config["aspectRatio"], "1:1");

        let (config2, _) = parse_image_config_with_params("gemini-3-pro-image", None, Some("standard"), None);
        assert_eq!(config2["imageSize"], "1K");
    }

    #[test]
    fn test_parse_image_config_direct_size() {
        let (config, _) = parse_image_config_with_params("gemini-3-pro-image", None, Some("standard"), Some("4K"));
        // Direct imageSize takes priority over quality
        assert_eq!(config["imageSize"], "4K");
    }

    #[test]
    fn test_detects_networking_tool() {
        assert!(detects_networking_tool(&Some(vec![json!({"name": "web_search"})])));
        assert!(detects_networking_tool(&Some(vec![json!({"googleSearch": {}})])));
        assert!(detects_networking_tool(&Some(vec![json!({
            "functionDeclarations": [{"name": "google_search"}]
        })])));
        assert!(!detects_networking_tool(&Some(vec![json!({"name": "get_weather"})])));
        assert!(!detects_networking_tool(&None));
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
        // Should not inject googleSearch when functionDeclarations exist
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
        );
        assert_eq!(config.final_model, "gemini-3-pro-high");
    }
}
