// OpenAI → Gemini 请求转换
//
// Requirements covered:
// - 2.1: OpenAI ChatCompletion → Gemini generateContent
// - 2.10: /v1/images/generations → Imagen 3
// - 2.13: /v1/completions, /v1/responses → Codex CLI compatibility

use super::models::*;
use serde_json::{json, Value};

/// Transform an OpenAI ChatCompletion request into Gemini generateContent format.
///
/// Returns (gemini_body, session_id, message_count).
pub fn transform_openai_request(
    request: &OpenAIRequest,
    project_id: &str,
    mapped_model: &str,
) -> (Value, String, usize) {
    let session_id = crate::proxy::session_manager::SessionManager::extract_openai_session_id(
        &serde_json::to_value(request).unwrap_or_default(),
    );
    let message_count = request.messages.len();
    let mapped_model_lower = mapped_model.to_lowercase();

    // Determine if this is a thinking model
    let is_thinking_model = mapped_model_lower.contains("gemini")
        && (mapped_model_lower.contains("-thinking")
            || mapped_model_lower.contains("gemini-2.0-pro")
            || mapped_model_lower.contains("gemini-3-pro"))
        && !mapped_model_lower.contains("claude");

    let user_enabled_thinking = request
        .thinking
        .as_ref()
        .map(|t| t.thinking_type.as_deref() == Some("enabled"))
        .unwrap_or(false);
    let user_thinking_budget = request.thinking.as_ref().and_then(|t| t.budget_tokens);

    let actual_include_thinking = is_thinking_model || user_enabled_thinking;

    // Determine if this is an image generation request
    let is_image_gen = request.size.is_some()
        || request.image_size.is_some()
        || mapped_model_lower.contains("image");

    // 1. Extract system instructions
    let mut system_instructions: Vec<String> = request
        .messages
        .iter()
        .filter(|msg| msg.role == "system" || msg.role == "developer")
        .filter_map(|msg| {
            msg.content.as_ref().map(|c| match c {
                OpenAIContent::String(s) => s.clone(),
                OpenAIContent::Array(blocks) => blocks
                    .iter()
                    .filter_map(|b| {
                        if let OpenAIContentBlock::Text { text } = b {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            })
        })
        .collect();

    if let Some(inst) = &request.instructions {
        if !inst.is_empty() {
            system_instructions.insert(0, inst.clone());
        }
    }

    // Pre-scan to map tool_call_id to function name
    let mut tool_id_to_name = std::collections::HashMap::new();
    for msg in &request.messages {
        if let Some(tool_calls) = &msg.tool_calls {
            for call in tool_calls {
                tool_id_to_name.insert(call.id.clone(), call.function.name.clone());
            }
        }
    }

    // 2. Build Gemini contents (filter out system/developer messages)
    let contents: Vec<Value> = request
        .messages
        .iter()
        .filter(|msg| msg.role != "system" && msg.role != "developer")
        .map(|msg| {
            let role = match msg.role.as_str() {
                "assistant" => "model",
                "tool" | "function" => "user",
                _ => &msg.role,
            };

            let mut parts = Vec::new();

            // Handle reasoning_content (thinking)
            if let Some(reasoning) = &msg.reasoning_content {
                if !reasoning.is_empty() && reasoning != "[undefined]" {
                    parts.push(json!({
                        "text": reasoning,
                        "thought": true,
                    }));
                }
            } else if actual_include_thinking && role == "model" {
                // Inject placeholder thinking block for assistant messages
                parts.push(json!({
                    "text": "Applying tool decisions and generating response...",
                    "thought": true,
                }));
            }

            // Handle content (multimodal or text)
            let is_tool_role = msg.role == "tool" || msg.role == "function";
            if let (Some(content), false) = (&msg.content, is_tool_role) {
                match content {
                    OpenAIContent::String(s) => {
                        if !s.is_empty() {
                            parts.push(json!({"text": s}));
                        }
                    }
                    OpenAIContent::Array(blocks) => {
                        for block in blocks {
                            match block {
                                OpenAIContentBlock::Text { text } => {
                                    parts.push(json!({"text": text}));
                                }
                                OpenAIContentBlock::ImageUrl { image_url } => {
                                    if image_url.url.starts_with("data:") {
                                        if let Some(pos) = image_url.url.find(',') {
                                            let mime_part = &image_url.url[5..pos];
                                            let mime_type =
                                                mime_part.split(';').next().unwrap_or("image/jpeg");
                                            let data = &image_url.url[pos + 1..];
                                            parts.push(json!({
                                                "inlineData": { "mimeType": mime_type, "data": data }
                                            }));
                                        }
                                    } else if image_url.url.starts_with("http") {
                                        parts.push(json!({
                                            "fileData": { "fileUri": &image_url.url, "mimeType": "image/jpeg" }
                                        }));
                                    }
                                }
                                OpenAIContentBlock::AudioUrl { .. } => {
                                    // Audio URL handling deferred to audio module
                                }
                            }
                        }
                    }
                }
            }

            // Handle tool calls (assistant message)
            if let Some(tool_calls) = &msg.tool_calls {
                for tc in tool_calls {
                    let args = serde_json::from_str::<Value>(&tc.function.arguments)
                        .unwrap_or(json!({}));
                    let func_call_part = json!({
                        "functionCall": {
                            "name": &tc.function.name,
                            "args": args,
                            "id": &tc.id,
                        }
                    });
                    parts.push(func_call_part);
                }
            }

            // Handle tool response
            if msg.role == "tool" || msg.role == "function" {
                let name = msg.name.as_deref().unwrap_or("unknown");
                let final_name = if let Some(id) = &msg.tool_call_id {
                    tool_id_to_name
                        .get(id)
                        .map(|s| s.as_str())
                        .unwrap_or(name)
                } else {
                    name
                };

                let content_val = match &msg.content {
                    Some(OpenAIContent::String(s)) => s.clone(),
                    Some(OpenAIContent::Array(blocks)) => blocks
                        .iter()
                        .filter_map(|b| {
                            if let OpenAIContentBlock::Text { text } = b {
                                Some(text.clone())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    None => String::new(),
                };

                parts.push(json!({
                    "functionResponse": {
                        "name": final_name,
                        "response": { "result": content_val },
                        "id": msg.tool_call_id.clone().unwrap_or_default()
                    }
                }));
            }

            json!({ "role": role, "parts": parts })
        })
        .filter(|msg| !msg["parts"].as_array().map(|a| a.is_empty()).unwrap_or(true))
        .collect();

    // Merge consecutive same-role messages (Gemini requires user/model alternation)
    let mut merged_contents: Vec<Value> = Vec::new();
    for msg in contents {
        if let Some(last) = merged_contents.last_mut() {
            if last["role"] == msg["role"] {
                if let (Some(last_parts), Some(msg_parts)) =
                    (last["parts"].as_array_mut(), msg["parts"].as_array())
                {
                    last_parts.extend(msg_parts.iter().cloned());
                    continue;
                }
            }
        }
        merged_contents.push(msg);
    }
    let contents = merged_contents;

    // 3. Build generation config
    let mut gen_config = json!({
        "temperature": request.temperature.unwrap_or(1.0),
        "topP": request.top_p.unwrap_or(0.95),
    });

    if let Some(max_tokens) = request.max_tokens {
        gen_config["maxOutputTokens"] = json!(max_tokens);
    }

    if let Some(n) = request.n {
        gen_config["candidateCount"] = json!(n);
    }

    // Inject thinkingConfig for thinking models
    if actual_include_thinking {
        // [Req 7.5] Check image thinking mode - if disabled for image_gen, enforce includeThoughts=false
        let image_thinking_mode = crate::proxy::config::get_image_thinking_mode();
        let is_image_gen_disabled = is_image_gen && image_thinking_mode == "disabled";

        if is_image_gen_disabled {
            gen_config["thinkingConfig"] = json!({
                "includeThoughts": false
            });
        } else {
            let user_budget: i64 = user_thinking_budget.map(|b| b as i64).unwrap_or(24576);

            // Cap budget for Gemini models at 24576
            let is_gemini_limited =
                mapped_model_lower.contains("gemini") && !mapped_model_lower.contains("-image");
            let budget = if is_gemini_limited && user_budget > 24576 {
                24576
            } else {
                user_budget
            };

            gen_config["thinkingConfig"] = json!({
                "includeThoughts": true,
                "thinkingBudget": budget
            });

            // maxOutputTokens must be greater than thinkingBudget
            let overhead = if is_image_gen { 2048 } else { 32768 };
            let min_overhead = if is_image_gen { 1024 } else { 8192 };

            if let Some(max_tokens) = request.max_tokens {
                if (max_tokens as i64) <= budget {
                    gen_config["maxOutputTokens"] = json!(budget + min_overhead);
                }
            } else {
                gen_config["maxOutputTokens"] = json!(budget + overhead);
            }
        }
    }

    if let Some(stop) = &request.stop {
        if stop.is_string() {
            gen_config["stopSequences"] = json!([stop]);
        } else if stop.is_array() {
            gen_config["stopSequences"] = stop.clone();
        }
    }

    if let Some(fmt) = &request.response_format {
        if fmt.r#type == "json_object" {
            gen_config["responseMimeType"] = json!("application/json");
        }
    }

    // Handle image generation config
    if is_image_gen {
        let image_size = request
            .image_size
            .as_deref()
            .or(request.size.as_deref())
            .and_then(parse_image_size);
        if let Some(size) = image_size {
            gen_config["imageConfig"] = json!({ "imageSize": size });
        }
    }

    let mut inner_request = json!({
        "contents": contents,
        "generationConfig": gen_config,
        "safetySettings": [
            { "category": "HARM_CATEGORY_HARASSMENT", "threshold": "OFF" },
            { "category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "OFF" },
            { "category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "threshold": "OFF" },
            { "category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "OFF" },
            { "category": "HARM_CATEGORY_CIVIC_INTEGRITY", "threshold": "OFF" },
        ]
    });

    // 4. Handle Tools
    if let Some(tools) = &request.tools {
        let mut function_declarations: Vec<Value> = Vec::new();
        for tool in tools.iter() {
            let mut gemini_func = if let Some(func) = tool.get("function") {
                func.clone()
            } else {
                let mut func = tool.clone();
                if let Some(obj) = func.as_object_mut() {
                    obj.remove("type");
                    obj.remove("strict");
                    obj.remove("additionalProperties");
                }
                func
            };

            let name_opt = gemini_func
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            if name_opt.is_none() {
                continue; // Skip tools without name
            }

            // Clean up invalid fields at root level
            if let Some(obj) = gemini_func.as_object_mut() {
                obj.remove("format");
                obj.remove("strict");
                obj.remove("additionalProperties");
                obj.remove("type");
                obj.remove("external_web_access");
            }

            if let Some(params) = gemini_func.get_mut("parameters") {
                if let Some(params_obj) = params.as_object_mut() {
                    if !params_obj.contains_key("type") {
                        params_obj.insert("type".to_string(), json!("OBJECT"));
                    }
                }
                enforce_uppercase_types(params);
            } else {
                // Inject default schema for tools without parameters
                gemini_func.as_object_mut().unwrap().insert(
                    "parameters".to_string(),
                    json!({
                        "type": "OBJECT",
                        "properties": {},
                    }),
                );
            }
            function_declarations.push(gemini_func);
        }

        if !function_declarations.is_empty() {
            inner_request["tools"] = json!([{ "functionDeclarations": function_declarations }]);
        }
    }

    // 5. System instruction
    if !system_instructions.is_empty() {
        let parts: Vec<Value> = system_instructions
            .iter()
            .map(|s| json!({"text": s}))
            .collect();
        inner_request["systemInstruction"] = json!({
            "role": "user",
            "parts": parts
        });
    }

    // Remove tools and systemInstruction for image generation
    if is_image_gen {
        if let Some(obj) = inner_request.as_object_mut() {
            obj.remove("tools");
            obj.remove("systemInstruction");
        }
    }

    let final_body = json!({
        "project": project_id,
        "requestId": format!("openai-{}", uuid::Uuid::new_v4()),
        "request": inner_request,
        "model": mapped_model,
        "userAgent": "kiro-ai-gateway",
        "requestType": if is_image_gen { "image_gen" } else { "chat" }
    });

    (final_body, session_id, message_count)
}

/// Transform an OpenAI image generation request into Gemini Imagen 3 format.
pub fn transform_image_request(
    request: &ImageGenerationRequest,
    project_id: &str,
) -> Value {
    let model = request
        .model
        .as_deref()
        .unwrap_or("gemini-3-pro-image");

    let contents = json!([{
        "role": "user",
        "parts": [{ "text": &request.prompt }]
    }]);

    let mut gen_config = json!({});

    if let Some(size) = &request.size {
        if let Some(parsed) = parse_image_size(size) {
            gen_config["imageConfig"] = json!({ "imageSize": parsed });
        }
    }

    let inner_request = json!({
        "contents": contents,
        "generationConfig": gen_config,
        "safetySettings": [
            { "category": "HARM_CATEGORY_HARASSMENT", "threshold": "OFF" },
            { "category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "OFF" },
            { "category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "threshold": "OFF" },
            { "category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "OFF" },
            { "category": "HARM_CATEGORY_CIVIC_INTEGRITY", "threshold": "OFF" },
        ]
    });

    json!({
        "project": project_id,
        "requestId": format!("imagen-{}", uuid::Uuid::new_v4()),
        "request": inner_request,
        "model": model,
        "userAgent": "kiro-ai-gateway",
        "requestType": "image_gen"
    })
}

/// Parse OpenAI-style size string (e.g. "1024x1024") to Gemini imageSize.
fn parse_image_size(size: &str) -> Option<&'static str> {
    match size {
        "256x256" | "512x512" | "1024x1024" => Some("1024x1024"),
        "1024x1792" | "1792x1024" => Some("1024x1792"),
        "4K" | "4k" => Some("4K"),
        _ => {
            // Try to parse WxH format
            if size.contains('x') {
                Some("1024x1024") // Default fallback
            } else {
                None
            }
        }
    }
}

/// Recursively convert JSON Schema `type` values to uppercase (Gemini Protobuf requirement).
fn enforce_uppercase_types(value: &mut Value) {
    if let Value::Object(map) = value {
        if let Some(type_val) = map.get_mut("type") {
            if let Value::String(ref mut s) = type_val {
                *s = s.to_uppercase();
            }
        }
        if let Some(properties) = map.get_mut("properties") {
            if let Value::Object(ref mut props) = properties {
                for v in props.values_mut() {
                    enforce_uppercase_types(v);
                }
            }
        }
        if let Some(items) = map.get_mut("items") {
            enforce_uppercase_types(items);
        }
    } else if let Value::Array(arr) = value {
        for item in arr {
            enforce_uppercase_types(item);
        }
    }
}

/// Return the list of available models in OpenAI format.
/// Requirement 2.12: /v1/models returns OpenAI format model list
pub fn get_openai_model_list() -> OpenAIModelList {
    let models = crate::proxy::common::model_mapping::get_supported_models();
    let created = chrono::Utc::now().timestamp() as u64;

    let data: Vec<OpenAIModel> = models
        .into_iter()
        .map(|id| OpenAIModel {
            id: id.clone(),
            object: "model".to_string(),
            created,
            owned_by: "google".to_string(),
        })
        .collect();

    OpenAIModelList {
        object: "list".to_string(),
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Mutex to serialize tests that modify global image thinking mode state
    static IMAGE_THINKING_LOCK: Mutex<()> = Mutex::new(());

    fn make_simple_request(model: &str, content: &str) -> OpenAIRequest {
        OpenAIRequest {
            model: model.to_string(),
            messages: vec![OpenAIMessage {
                role: "user".to_string(),
                content: Some(OpenAIContent::String(content.to_string())),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            stream: false,
            n: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            instructions: None,
            input: None,
            prompt: None,
            size: None,
            quality: None,
            person_generation: None,
            thinking: None,
            image_size: None,
        }
    }

    #[test]
    fn test_basic_request_transform() {
        let req = make_simple_request("gpt-4", "Hello");
        let (result, sid, msg_count) = transform_openai_request(&req, "test-proj", "gemini-2.5-flash");

        assert_eq!(msg_count, 1);
        assert!(sid.starts_with("sid-"));
        assert_eq!(result["model"], "gemini-2.5-flash");
        assert_eq!(result["requestType"], "chat");

        let contents = result["request"]["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "Hello");
    }

    #[test]
    fn test_system_message_extraction() {
        let req = OpenAIRequest {
            model: "gpt-4".to_string(),
            messages: vec![
                OpenAIMessage {
                    role: "system".to_string(),
                    content: Some(OpenAIContent::String("You are helpful".to_string())),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                OpenAIMessage {
                    role: "user".to_string(),
                    content: Some(OpenAIContent::String("Hello".to_string())),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
            ],
            stream: false,
            n: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            instructions: None,
            input: None,
            prompt: None,
            size: None,
            quality: None,
            person_generation: None,
            thinking: None,
            image_size: None,
        };

        let (result, _, _) = transform_openai_request(&req, "test-proj", "gemini-2.5-flash");

        // System message should be in systemInstruction, not in contents
        let sys = &result["request"]["systemInstruction"];
        assert!(sys.get("parts").is_some());
        assert_eq!(sys["parts"][0]["text"], "You are helpful");

        // Contents should only have the user message
        let contents = result["request"]["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
    }

    #[test]
    fn test_multimodal_image_transform() {
        let req = OpenAIRequest {
            model: "gpt-4-vision".to_string(),
            messages: vec![OpenAIMessage {
                role: "user".to_string(),
                content: Some(OpenAIContent::Array(vec![
                    OpenAIContentBlock::Text {
                        text: "What is in this image?".to_string(),
                    },
                    OpenAIContentBlock::ImageUrl {
                        image_url: OpenAIImageUrl {
                            url: "data:image/png;base64,iVBORw0KGgo=".to_string(),
                            detail: None,
                        },
                    },
                ])),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            stream: false,
            n: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            instructions: None,
            input: None,
            prompt: None,
            size: None,
            quality: None,
            person_generation: None,
            thinking: None,
            image_size: None,
        };

        let (result, _, _) = transform_openai_request(&req, "test-proj", "gemini-1.5-flash");
        let parts = &result["request"]["contents"][0]["parts"];
        let parts_arr = parts.as_array().unwrap();
        assert_eq!(parts_arr.len(), 2);
        assert_eq!(parts_arr[0]["text"], "What is in this image?");
        assert_eq!(parts_arr[1]["inlineData"]["mimeType"], "image/png");
    }

    #[test]
    fn test_tool_calls_transform() {
        let req = OpenAIRequest {
            model: "gpt-4".to_string(),
            messages: vec![
                OpenAIMessage {
                    role: "user".to_string(),
                    content: Some(OpenAIContent::String("Get weather".to_string())),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                OpenAIMessage {
                    role: "assistant".to_string(),
                    content: None,
                    reasoning_content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call_123".to_string(),
                        r#type: "function".to_string(),
                        function: ToolFunction {
                            name: "get_weather".to_string(),
                            arguments: r#"{"city":"Tokyo"}"#.to_string(),
                        },
                    }]),
                    tool_call_id: None,
                    name: None,
                },
                OpenAIMessage {
                    role: "tool".to_string(),
                    content: Some(OpenAIContent::String("Sunny, 25°C".to_string())),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some("call_123".to_string()),
                    name: Some("get_weather".to_string()),
                },
            ],
            stream: false,
            n: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: None,
            response_format: None,
            tools: Some(vec![json!({
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get weather",
                    "parameters": {
                        "type": "object",
                        "properties": { "city": { "type": "string" } }
                    }
                }
            })]),
            tool_choice: None,
            parallel_tool_calls: None,
            instructions: None,
            input: None,
            prompt: None,
            size: None,
            quality: None,
            person_generation: None,
            thinking: None,
            image_size: None,
        };

        let (result, _, _) = transform_openai_request(&req, "test-proj", "gemini-2.5-flash");

        // Check tools are converted
        let tools = &result["request"]["tools"];
        assert!(tools.is_array());
        let func_decls = &tools[0]["functionDeclarations"];
        assert_eq!(func_decls[0]["name"], "get_weather");

        // Check contents have functionCall and functionResponse
        let contents = result["request"]["contents"].as_array().unwrap();
        assert!(contents.len() >= 2);
    }

    #[test]
    fn test_thinking_model_budget_capping() {
        let req = make_simple_request("gemini-3-pro", "test");
        let (result, _, _) = transform_openai_request(&req, "test-proj", "gemini-3-pro");

        let budget = result["request"]["generationConfig"]["thinkingConfig"]["thinkingBudget"]
            .as_i64()
            .unwrap();
        assert_eq!(budget, 24576);
    }

    #[test]
    fn test_non_thinking_model_no_thinking_config() {
        let req = make_simple_request("gpt-4", "Hello");
        let (result, _, _) = transform_openai_request(&req, "test-proj", "gemini-2.5-flash");

        assert!(result["request"]["generationConfig"]
            .get("thinkingConfig")
            .is_none());
    }

    #[test]
    fn test_model_list() {
        let list = get_openai_model_list();
        assert_eq!(list.object, "list");
        assert!(!list.data.is_empty());
        for model in &list.data {
            assert_eq!(model.object, "model");
            assert_eq!(model.owned_by, "google");
        }
    }

    #[test]
    fn test_image_request_transform() {
        let req = ImageGenerationRequest {
            prompt: "A cute cat".to_string(),
            model: Some("gemini-3-pro-image".to_string()),
            n: 1,
            size: Some("1024x1024".to_string()),
            quality: None,
            response_format: None,
            person_generation: None,
        };

        let result = transform_image_request(&req, "test-proj");
        assert_eq!(result["model"], "gemini-3-pro-image");
        assert_eq!(result["requestType"], "image_gen");
        assert_eq!(
            result["request"]["contents"][0]["parts"][0]["text"],
            "A cute cat"
        );
    }

    #[test]
    fn test_consecutive_same_role_merge() {
        let req = OpenAIRequest {
            model: "gpt-4".to_string(),
            messages: vec![
                OpenAIMessage {
                    role: "user".to_string(),
                    content: Some(OpenAIContent::String("Hello".to_string())),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                OpenAIMessage {
                    role: "user".to_string(),
                    content: Some(OpenAIContent::String("World".to_string())),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
            ],
            stream: false,
            n: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            instructions: None,
            input: None,
            prompt: None,
            size: None,
            quality: None,
            person_generation: None,
            thinking: None,
            image_size: None,
        };

        let (result, _, _) = transform_openai_request(&req, "test-proj", "gemini-2.5-flash");
        let contents = result["request"]["contents"].as_array().unwrap();
        // Two consecutive user messages should be merged into one
        assert_eq!(contents.len(), 1);
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn test_enforce_uppercase_types() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "items": { "type": "array", "items": { "type": "integer" } }
            }
        });
        enforce_uppercase_types(&mut schema);
        assert_eq!(schema["type"], "OBJECT");
        assert_eq!(schema["properties"]["name"]["type"], "STRING");
        assert_eq!(schema["properties"]["items"]["type"], "ARRAY");
        assert_eq!(schema["properties"]["items"]["items"]["type"], "INTEGER");
    }

    #[test]
    fn test_stop_sequences() {
        let mut req = make_simple_request("gpt-4", "Hello");
        req.stop = Some(json!(["STOP", "END"]));
        let (result, _, _) = transform_openai_request(&req, "test-proj", "gemini-2.5-flash");
        assert_eq!(
            result["request"]["generationConfig"]["stopSequences"],
            json!(["STOP", "END"])
        );
    }

    #[test]
    fn test_json_response_format() {
        let mut req = make_simple_request("gpt-4", "Hello");
        req.response_format = Some(ResponseFormat {
            r#type: "json_object".to_string(),
        });
        let (result, _, _) = transform_openai_request(&req, "test-proj", "gemini-2.5-flash");
        assert_eq!(
            result["request"]["generationConfig"]["responseMimeType"],
            "application/json"
        );
    }

    #[test]
    fn test_image_gen_thinking_mode_disabled() {
        let _lock = IMAGE_THINKING_LOCK.lock().unwrap();
        // Set global image thinking mode to disabled [Req 7.5]
        crate::proxy::config::update_image_thinking_mode(Some("disabled".to_string()));

        let mut req = make_simple_request("gemini-3-pro-image", "A beautiful sunset");
        req.size = Some("1024x1024".to_string());

        let (result, _, _) = transform_openai_request(&req, "test-proj", "gemini-3-pro-image");

        let gen_config = &result["request"]["generationConfig"];
        let thinking_config = gen_config.get("thinkingConfig");
        assert!(thinking_config.is_some(), "thinkingConfig should be present");
        assert_eq!(
            thinking_config.unwrap()["includeThoughts"], false,
            "includeThoughts should be false when image thinking mode is disabled"
        );

        // Reset
        crate::proxy::config::update_image_thinking_mode(Some("enabled".to_string()));
    }

    #[test]
    fn test_image_gen_thinking_mode_enabled() {
        let _lock = IMAGE_THINKING_LOCK.lock().unwrap();
        // Set global image thinking mode to enabled [Req 7.5]
        crate::proxy::config::update_image_thinking_mode(Some("enabled".to_string()));

        let mut req = make_simple_request("gemini-3-pro-image", "A beautiful sunset");
        req.size = Some("1024x1024".to_string());

        let (result, _, _) = transform_openai_request(&req, "test-proj", "gemini-3-pro-image");

        let gen_config = &result["request"]["generationConfig"];
        let thinking_config = gen_config.get("thinkingConfig");
        assert!(thinking_config.is_some(), "thinkingConfig should be present for image model");
        assert_eq!(
            thinking_config.unwrap()["includeThoughts"], true,
            "includeThoughts should be true when image thinking mode is enabled"
        );
    }

    #[test]
    fn test_image_gen_removes_tools_and_system_instruction() {
        let req = OpenAIRequest {
            model: "gemini-3-pro-image".to_string(),
            messages: vec![
                OpenAIMessage {
                    role: "system".to_string(),
                    content: Some(OpenAIContent::String("You are an artist".to_string())),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                OpenAIMessage {
                    role: "user".to_string(),
                    content: Some(OpenAIContent::String("Draw a cat".to_string())),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
            ],
            stream: false,
            n: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: None,
            response_format: None,
            tools: Some(vec![serde_json::json!({
                "type": "function",
                "function": { "name": "test_tool" }
            })]),
            tool_choice: None,
            parallel_tool_calls: None,
            instructions: None,
            input: None,
            prompt: None,
            size: Some("1024x1024".to_string()),
            quality: None,
            person_generation: None,
            thinking: None,
            image_size: None,
        };

        let (result, _, _) = transform_openai_request(&req, "test-proj", "gemini-3-pro-image");

        // Image generation should strip tools and systemInstruction
        assert!(result["request"].get("tools").is_none(), "tools should be removed for image gen");
        assert!(
            result["request"].get("systemInstruction").is_none(),
            "systemInstruction should be removed for image gen"
        );
        assert_eq!(result["requestType"], "image_gen");
    }

    #[test]
    fn test_image_gen_with_user_thinking_budget() {
        let _lock = IMAGE_THINKING_LOCK.lock().unwrap();
        crate::proxy::config::update_image_thinking_mode(Some("enabled".to_string()));

        let mut req = make_simple_request("gemini-3-pro-image", "A landscape");
        req.size = Some("1024x1024".to_string());
        req.thinking = Some(ThinkingConfig {
            thinking_type: Some("enabled".to_string()),
            budget_tokens: Some(16000),
            effort: None,
        });

        let (result, _, _) = transform_openai_request(&req, "test-proj", "gemini-3-pro-image");

        let gen_config = &result["request"]["generationConfig"];
        let thinking_config = gen_config.get("thinkingConfig").unwrap();
        // When image thinking mode is enabled, includeThoughts should be true
        // and budget should be the user-specified value
        let include_thoughts = thinking_config["includeThoughts"].as_bool().unwrap();
        assert!(include_thoughts, "includeThoughts should be true when image thinking mode is enabled");
        let budget = thinking_config["thinkingBudget"].as_i64().unwrap();
        assert_eq!(budget, 16000);

        // Reset
        crate::proxy::config::update_image_thinking_mode(Some("enabled".to_string()));
    }

    // =========================================================================
    // Property-Based Tests
    // =========================================================================

    mod proptest_roundtrip {
        use super::*;
        use crate::proxy::mappers::openai::response::transform_openai_response;
        use proptest::prelude::*;
        use serde_json::json;

        /// Strategy to generate non-empty text content for user messages.
        fn text_content_strategy() -> impl Strategy<Value = String> {
            "[a-zA-Z0-9 ,.!?]{3,200}".prop_filter("non-empty trimmed", |s| !s.trim().is_empty())
        }


        /// Strategy to generate a simple OpenAI request with text-only user messages.
        fn simple_openai_request_strategy(
        ) -> impl Strategy<Value = (OpenAIRequest, Vec<String>)> {
            // Generate 1-5 user/assistant message pairs with text content
            proptest::collection::vec(text_content_strategy(), 1..=5).prop_map(|texts| {
                let mut messages = Vec::new();
                let user_texts: Vec<String> = texts.clone();

                for (i, text) in texts.into_iter().enumerate() {
                    // Alternate user/assistant to satisfy Gemini's alternation requirement
                    messages.push(OpenAIMessage {
                        role: "user".to_string(),
                        content: Some(OpenAIContent::String(text)),
                        reasoning_content: None,
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    });

                    // Add an assistant response for all but the last message
                    if i < user_texts.len() - 1 {
                        messages.push(OpenAIMessage {
                            role: "assistant".to_string(),
                            content: Some(OpenAIContent::String(format!(
                                "Response to message {}",
                                i
                            ))),
                            reasoning_content: None,
                            tool_calls: None,
                            tool_call_id: None,
                            name: None,
                        });
                    }
                }

                let req = OpenAIRequest {
                    model: "gpt-4".to_string(),
                    messages,
                    stream: false,
                    n: None,
                    max_tokens: None,
                    temperature: None,
                    top_p: None,
                    stop: None,
                    response_format: None,
                    tools: None,
                    tool_choice: None,
                    parallel_tool_calls: None,
                    instructions: None,
                    input: None,
                    prompt: None,
                    size: None,
                    quality: None,
                    person_generation: None,
                    thinking: None,
                    image_size: None,
                };

                (req, user_texts)
            })
        }

        /// Build a simulated Gemini response containing the given text content.
        /// This mimics what the upstream Gemini API would return.
        fn build_gemini_response(response_text: &str) -> serde_json::Value {
            json!({
                "response": {
                    "responseId": "resp-test-123",
                    "modelVersion": "gemini-2.5-flash",
                    "candidates": [{
                        "content": {
                            "role": "model",
                            "parts": [{ "text": response_text }]
                        },
                        "finishReason": "STOP"
                    }],
                    "usageMetadata": {
                        "promptTokenCount": 10,
                        "candidatesTokenCount": 20,
                        "totalTokenCount": 30
                    }
                }
            })
        }

        // **Feature: kiro-ai-gateway, Property 3: OpenAI 协议转换往返一致性**
        // **Validates: Requirements 2.9**
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            /// Property 3a: User text content in the request is preserved through
            /// OpenAI→Gemini request conversion (text appears in Gemini contents).
            #[test]
            fn prop_openai_request_preserves_user_text(
                (req, user_texts) in simple_openai_request_strategy()
            ) {
                let (gemini_body, _sid, _msg_count) =
                    transform_openai_request(&req, "test-proj", "gemini-2.5-flash");

                // Extract all text parts from the Gemini request contents
                let contents = gemini_body["request"]["contents"]
                    .as_array()
                    .expect("contents should be an array");

                let mut gemini_texts: Vec<String> = Vec::new();
                for content in contents {
                    if let Some(parts) = content["parts"].as_array() {
                        for part in parts {
                            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                gemini_texts.push(text.to_string());
                            }
                        }
                    }
                }

                // Every user text from the original request must appear in the Gemini contents
                for user_text in &user_texts {
                    prop_assert!(
                        gemini_texts.iter().any(|gt| gt == user_text),
                        "User text '{}' not found in Gemini contents: {:?}",
                        user_text,
                        gemini_texts
                    );
                }
            }

            /// Property 3b: Text content in a Gemini response is preserved when
            /// converted back to OpenAI format (roundtrip response text preservation).
            #[test]
            fn prop_openai_response_preserves_text(
                response_text in text_content_strategy()
            ) {
                let gemini_resp = build_gemini_response(&response_text);
                let openai_resp = transform_openai_response(&gemini_resp, None, 1);

                prop_assert_eq!(openai_resp.object, "chat.completion");
                prop_assert!(!openai_resp.choices.is_empty(), "Should have at least one choice");

                let choice = &openai_resp.choices[0];
                let content = choice.message.content.as_ref()
                    .expect("Response message should have content");

                match content {
                    OpenAIContent::String(s) => {
                        prop_assert_eq!(
                            s, &response_text,
                            "Response text should be preserved through Gemini→OpenAI conversion"
                        );
                    }
                    _ => {
                        prop_assert!(false, "Expected string content in response");
                    }
                }
            }

            /// Property 3c: Full roundtrip - for any text content, converting an OpenAI
            /// request to Gemini format and then converting a Gemini response (containing
            /// the same text) back to OpenAI format SHALL preserve the text content.
            #[test]
            fn prop_openai_roundtrip_text_preservation(
                user_text in text_content_strategy(),
                response_text in text_content_strategy()
            ) {
                // Step 1: Build an OpenAI request with user text
                let req = OpenAIRequest {
                    model: "gpt-4".to_string(),
                    messages: vec![OpenAIMessage {
                        role: "user".to_string(),
                        content: Some(OpenAIContent::String(user_text.clone())),
                        reasoning_content: None,
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    }],
                    stream: false,
                    n: None,
                    max_tokens: None,
                    temperature: None,
                    top_p: None,
                    stop: None,
                    response_format: None,
                    tools: None,
                    tool_choice: None,
                    parallel_tool_calls: None,
                    instructions: None,
                    input: None,
                    prompt: None,
                    size: None,
                    quality: None,
                    person_generation: None,
                    thinking: None,
                    image_size: None,
                };

                // Step 2: Convert OpenAI request → Gemini format
                let (gemini_body, session_id, msg_count) =
                    transform_openai_request(&req, "test-proj", "gemini-2.5-flash");

                // Verify user text is in the Gemini request
                let contents = gemini_body["request"]["contents"]
                    .as_array()
                    .expect("contents should be an array");
                let mut found_user_text = false;
                for content in contents {
                    if let Some(parts) = content["parts"].as_array() {
                        for part in parts {
                            if part.get("text").and_then(|t| t.as_str()) == Some(&user_text) {
                                found_user_text = true;
                            }
                        }
                    }
                }
                prop_assert!(found_user_text, "User text must be present in Gemini request");

                // Step 3: Simulate Gemini response with response_text
                let gemini_resp = build_gemini_response(&response_text);

                // Step 4: Convert Gemini response → OpenAI format
                let openai_resp = transform_openai_response(
                    &gemini_resp,
                    Some(&session_id),
                    msg_count,
                );

                // Step 5: Verify response text is preserved
                prop_assert!(!openai_resp.choices.is_empty());
                let choice = &openai_resp.choices[0];
                match &choice.message.content {
                    Some(OpenAIContent::String(s)) => {
                        prop_assert_eq!(
                            s, &response_text,
                            "Response text must be preserved through the full roundtrip"
                        );
                    }
                    other => {
                        prop_assert!(
                            false,
                            "Expected String content, got: {:?}",
                            other
                        );
                    }
                }

                // Verify structural properties of the OpenAI response
                prop_assert_eq!(openai_resp.object, "chat.completion");
                prop_assert_eq!(&choice.message.role, "assistant");
                prop_assert!(
                    choice.finish_reason.as_deref() == Some("stop"),
                    "Finish reason should be 'stop' for STOP finishReason"
                );
            }
        }
    }
}
