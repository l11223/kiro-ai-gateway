// Claude → Gemini request transformation
//
// Requirements covered:
// - 2.2: Anthropic Messages → Gemini generateContent
// - 2.15: /v1/messages/count_tokens

use super::models::*;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Build safety settings for Gemini API (all filters disabled for proxy compatibility)
fn build_safety_settings() -> Value {
    json!([
        { "category": "HARM_CATEGORY_HARASSMENT", "threshold": "OFF" },
        { "category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "OFF" },
        { "category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "threshold": "OFF" },
        { "category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "OFF" },
        { "category": "HARM_CATEGORY_CIVIC_INTEGRITY", "threshold": "OFF" },
    ])
}

/// Clean cache_control fields from messages (clients may send them back in history)
pub fn clean_cache_control_from_messages(messages: &mut [Message]) {
    for msg in messages.iter_mut() {
        if let MessageContent::Array(blocks) = &mut msg.content {
            for block in blocks.iter_mut() {
                match block {
                    ContentBlock::Thinking { cache_control, .. }
                    | ContentBlock::Image { cache_control, .. }
                    | ContentBlock::Document { cache_control, .. }
                    | ContentBlock::ToolUse { cache_control, .. } => {
                        if cache_control.is_some() {
                            *cache_control = None;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Merge consecutive same-role messages to satisfy Gemini's alternation requirement
pub fn merge_consecutive_messages(messages: &mut Vec<Message>) {
    if messages.len() <= 1 {
        return;
    }

    let mut merged: Vec<Message> = Vec::with_capacity(messages.len());
    let old_messages = std::mem::take(messages);
    let mut messages_iter = old_messages.into_iter();

    if let Some(mut current) = messages_iter.next() {
        for next in messages_iter {
            if current.role == next.role {
                match (&mut current.content, next.content) {
                    (MessageContent::Array(cur), MessageContent::Array(nxt)) => {
                        cur.extend(nxt);
                    }
                    (MessageContent::Array(cur), MessageContent::String(nxt)) => {
                        cur.push(ContentBlock::Text { text: nxt });
                    }
                    (MessageContent::String(cur), MessageContent::String(nxt)) => {
                        *cur = format!("{}\n\n{}", cur, nxt);
                    }
                    (MessageContent::String(cur), MessageContent::Array(nxt)) => {
                        let mut new_blocks = vec![ContentBlock::Text { text: cur.clone() }];
                        new_blocks.extend(nxt);
                        current.content = MessageContent::Array(new_blocks);
                    }
                }
            } else {
                merged.push(current);
                current = next;
            }
        }
        merged.push(current);
    }

    *messages = merged;
}

/// Transform a Claude/Anthropic Messages request into Gemini generateContent format.
///
/// Returns (gemini_body, session_id, message_count).
pub fn transform_claude_request(
    claude_req: &ClaudeRequest,
    project_id: &str,
    mapped_model: &str,
) -> Result<(Value, String, usize), String> {
    let mut cleaned_req = claude_req.clone();

    // Pre-process: merge consecutive same-role messages
    merge_consecutive_messages(&mut cleaned_req.messages);
    // Pre-process: clean cache_control fields
    clean_cache_control_from_messages(&mut cleaned_req.messages);

    let claude_req = &cleaned_req;
    let message_count = claude_req.messages.len();

    // Generate session ID (uses metadata.user_id if available)
    let session_id = crate::proxy::session_manager::SessionManager::extract_session_id(
        &serde_json::to_value(claude_req).unwrap_or_default(),
    );

    let mapped_model_lower = mapped_model.to_lowercase();

    // Determine if thinking is enabled
    let thinking_type = claude_req.thinking.as_ref().map(|t| t.type_.as_str());
    let is_thinking_enabled =
        thinking_type == Some("enabled") || thinking_type == Some("adaptive");

    // Check if target model supports thinking
    let target_supports_thinking = mapped_model_lower.contains("-thinking")
        || mapped_model_lower.contains("gemini-2.0-pro")
        || mapped_model_lower.contains("gemini-3-pro");

    let actual_thinking = is_thinking_enabled && target_supports_thinking;

    // Detect web search tool
    let has_web_search = claude_req
        .tools
        .as_ref()
        .map(|tools| tools.iter().any(|t| t.is_web_search()))
        .unwrap_or(false);

    // Build tool_use id -> name mapping
    let mut tool_id_to_name: HashMap<String, String> = HashMap::new();

    // 1. Build system instruction
    let system_instruction = build_system_instruction(&claude_req.system);

    // 2. Build contents (messages)
    let contents =
        build_contents(&claude_req.messages, &mut tool_id_to_name, actual_thinking)?;

    // 3. Build tools
    let tools = build_tools(&claude_req.tools, has_web_search)?;

    // 4. Build generation config
    let generation_config =
        build_generation_config(claude_req, mapped_model, actual_thinking);

    // 5. Assemble inner request
    let mut inner_request = json!({
        "contents": contents,
        "safetySettings": build_safety_settings(),
    });

    if let Some(sys_inst) = system_instruction {
        inner_request["systemInstruction"] = sys_inst;
    }

    if !generation_config.is_null() {
        inner_request["generationConfig"] = generation_config;
    }

    if let Some(tools_val) = tools {
        inner_request["tools"] = tools_val;
        inner_request["toolConfig"] = json!({
            "functionCallingConfig": { "mode": "VALIDATED" }
        });
    }

    // Inject google search tool if web search was requested
    if has_web_search {
        if let Some(existing_tools) = inner_request.get_mut("tools").and_then(|t| t.as_array_mut())
        {
            existing_tools.push(json!({ "googleSearch": {} }));
        } else {
            inner_request["tools"] = json!([{ "googleSearch": {} }]);
        }
    }

    let request_id = format!("claude-{}", uuid::Uuid::new_v4());

    let body = json!({
        "project": project_id,
        "requestId": request_id,
        "request": inner_request,
        "model": mapped_model,
        "userAgent": "kiro-ai-gateway",
        "requestType": "chat",
    });

    Ok((body, session_id, message_count))
}

/// Build system instruction from Claude system prompt
fn build_system_instruction(system: &Option<SystemPrompt>) -> Option<Value> {
    let mut parts = Vec::new();

    if let Some(sys) = system {
        match sys {
            SystemPrompt::String(text) => {
                if !text.is_empty() {
                    parts.push(json!({"text": text}));
                }
            }
            SystemPrompt::Array(blocks) => {
                for block in blocks {
                    if block.block_type == "text" && !block.text.is_empty() {
                        parts.push(json!({"text": block.text}));
                    }
                }
            }
        }
    }

    if parts.is_empty() {
        return None;
    }

    Some(json!({
        "role": "user",
        "parts": parts
    }))
}

/// Build Gemini contents from Claude messages
fn build_contents(
    messages: &[Message],
    tool_id_to_name: &mut HashMap<String, String>,
    is_thinking_enabled: bool,
) -> Result<Vec<Value>, String> {
    let mut gemini_contents: Vec<Value> = Vec::new();

    for msg in messages {
        let role = match msg.role.as_str() {
            "assistant" => "model",
            "user" => "user",
            _ => &msg.role,
        };

        let parts = build_parts(&msg.content, role == "model", tool_id_to_name, is_thinking_enabled)?;

        if parts.is_empty() {
            continue;
        }

        // Merge with previous message if same role (Gemini requires alternation)
        if let Some(last) = gemini_contents.last_mut() {
            if last["role"].as_str() == Some(role) {
                if let Some(last_parts) = last["parts"].as_array_mut() {
                    if let Some(new_parts) = json!(parts).as_array() {
                        last_parts.extend(new_parts.iter().cloned());
                    }
                    continue;
                }
            }
        }

        gemini_contents.push(json!({
            "role": role,
            "parts": parts
        }));
    }

    Ok(gemini_contents)
}

/// Build Gemini parts from Claude message content
fn build_parts(
    content: &MessageContent,
    is_assistant: bool,
    tool_id_to_name: &mut HashMap<String, String>,
    is_thinking_enabled: bool,
) -> Result<Vec<Value>, String> {
    let mut parts = Vec::new();

    match content {
        MessageContent::String(text) => {
            if !text.is_empty() {
                parts.push(json!({"text": text}));
            }
        }
        MessageContent::Array(blocks) => {
            for block in blocks {
                match block {
                    ContentBlock::Text { text } => {
                        if !text.is_empty() {
                            parts.push(json!({"text": text}));
                        }
                    }
                    ContentBlock::Thinking {
                        thinking,
                        signature,
                        ..
                    } => {
                        if !is_thinking_enabled || thinking.is_empty() {
                            // Downgrade to text when thinking is disabled
                            if !thinking.is_empty() {
                                parts.push(json!({"text": thinking}));
                            }
                        } else {
                            let mut part = json!({
                                "text": thinking,
                                "thought": true,
                            });
                            if let Some(sig) = signature {
                                if !sig.is_empty() {
                                    part["thoughtSignature"] = json!(sig);
                                }
                            }
                            parts.push(part);
                        }
                    }
                    ContentBlock::RedactedThinking { data } => {
                        parts.push(json!({
                            "text": format!("[Redacted Thinking: {}]", data)
                        }));
                    }
                    ContentBlock::Image { source, .. } => {
                        if source.source_type == "base64" {
                            parts.push(json!({
                                "inlineData": {
                                    "mimeType": source.media_type,
                                    "data": source.data
                                }
                            }));
                        }
                    }
                    ContentBlock::Document { source, .. } => {
                        if source.source_type == "base64" {
                            parts.push(json!({
                                "inlineData": {
                                    "mimeType": source.media_type,
                                    "data": source.data
                                }
                            }));
                        }
                    }
                    ContentBlock::ToolUse {
                        id, name, input, ..
                    } => {
                        if is_assistant {
                            tool_id_to_name.insert(id.clone(), name.clone());
                        }
                        parts.push(json!({
                            "functionCall": {
                                "name": name,
                                "args": input,
                                "id": id
                            }
                        }));
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        let func_name = tool_id_to_name
                            .get(tool_use_id)
                            .cloned()
                            .unwrap_or_else(|| tool_use_id.clone());

                        let merged_content = match content {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Array(arr) => arr
                                .iter()
                                .filter_map(|b| {
                                    b.get("text").and_then(|v| v.as_str()).map(|s| s.to_string())
                                })
                                .collect::<Vec<_>>()
                                .join("\n"),
                            _ => content.to_string(),
                        };

                        let result_text = if merged_content.trim().is_empty() {
                            if is_error.unwrap_or(false) {
                                "Tool execution failed with no output.".to_string()
                            } else {
                                "Command executed successfully.".to_string()
                            }
                        } else {
                            // Truncate very large tool results
                            const MAX_TOOL_RESULT_CHARS: usize = 200_000;
                            if merged_content.len() > MAX_TOOL_RESULT_CHARS {
                                let mut truncated: String =
                                    merged_content.chars().take(MAX_TOOL_RESULT_CHARS).collect();
                                truncated.push_str("\n...[truncated output]");
                                truncated
                            } else {
                                merged_content
                            }
                        };

                        parts.push(json!({
                            "functionResponse": {
                                "name": func_name,
                                "response": {"result": result_text},
                                "id": tool_use_id
                            }
                        }));
                    }
                    ContentBlock::ServerToolUse { .. } | ContentBlock::WebSearchToolResult { .. } => {
                        // Skip server tool blocks - they are handled by Gemini natively
                    }
                }
            }
        }
    }

    Ok(parts)
}

/// Build Gemini tools from Claude tool definitions
fn build_tools(
    tools: &Option<Vec<Tool>>,
    _has_web_search: bool,
) -> Result<Option<Value>, String> {
    let tools = match tools {
        Some(t) => t,
        None => return Ok(None),
    };

    let mut function_declarations: Vec<Value> = Vec::new();

    for tool in tools {
        // Skip server tools (web_search etc.) - handled separately
        if tool.is_web_search() {
            continue;
        }

        let name = match &tool.name {
            Some(n) => n.clone(),
            None => continue,
        };

        let mut func_decl = json!({ "name": name });

        if let Some(desc) = &tool.description {
            func_decl["description"] = json!(desc);
        }

        if let Some(schema) = &tool.input_schema {
            let mut params = schema.clone();
            enforce_uppercase_types(&mut params);
            func_decl["parameters"] = params;
        } else {
            func_decl["parameters"] = json!({
                "type": "OBJECT",
                "properties": {},
            });
        }

        function_declarations.push(func_decl);
    }

    if function_declarations.is_empty() {
        return Ok(None);
    }

    Ok(Some(json!([{ "functionDeclarations": function_declarations }])))
}

/// Build generation config from Claude request parameters
fn build_generation_config(
    claude_req: &ClaudeRequest,
    mapped_model: &str,
    is_thinking_enabled: bool,
) -> Value {
    let mut config = json!({});
    let mapped_lower = mapped_model.to_lowercase();

    if let Some(temp) = claude_req.temperature {
        config["temperature"] = json!(temp);
    }

    if let Some(top_p) = claude_req.top_p {
        config["topP"] = json!(top_p);
    }

    if let Some(top_k) = claude_req.top_k {
        config["topK"] = json!(top_k);
    }

    if let Some(max_tokens) = claude_req.max_tokens {
        config["maxOutputTokens"] = json!(max_tokens);
    }

    // Inject thinkingConfig for thinking models
    if is_thinking_enabled {
        let user_budget = claude_req
            .thinking
            .as_ref()
            .and_then(|t| t.budget_tokens)
            .unwrap_or(24576) as i64;

        // Cap budget for Gemini models at 24576
        let is_gemini = mapped_lower.contains("gemini");
        let budget = if is_gemini && user_budget > 24576 {
            24576
        } else {
            user_budget
        };

        config["thinkingConfig"] = json!({
            "includeThoughts": true,
            "thinkingBudget": budget
        });

        // maxOutputTokens must be greater than thinkingBudget
        if let Some(max_tokens) = claude_req.max_tokens {
            if (max_tokens as i64) <= budget {
                config["maxOutputTokens"] = json!(budget + 8192);
            }
        } else {
            config["maxOutputTokens"] = json!(budget + 32768);
        }
    }

    config
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

/// Estimate token count for a Claude request (simple heuristic).
/// Used for /v1/messages/count_tokens endpoint.
pub fn estimate_token_count(request: &CountTokensRequest) -> u32 {
    let mut total_chars: usize = 0;

    // Count system prompt
    if let Some(sys) = &request.system {
        match sys {
            SystemPrompt::String(s) => total_chars += s.len(),
            SystemPrompt::Array(blocks) => {
                for block in blocks {
                    total_chars += block.text.len();
                }
            }
        }
    }

    // Count messages
    for msg in &request.messages {
        match &msg.content {
            MessageContent::String(s) => total_chars += s.len(),
            MessageContent::Array(blocks) => {
                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => total_chars += text.len(),
                        ContentBlock::Thinking { thinking, .. } => total_chars += thinking.len(),
                        ContentBlock::ToolUse { input, .. } => {
                            total_chars += input.to_string().len()
                        }
                        ContentBlock::ToolResult { content, .. } => {
                            total_chars += content.to_string().len()
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Count tools
    if let Some(tools) = &request.tools {
        for tool in tools {
            if let Some(name) = &tool.name {
                total_chars += name.len();
            }
            if let Some(desc) = &tool.description {
                total_chars += desc.len();
            }
            if let Some(schema) = &tool.input_schema {
                total_chars += schema.to_string().len();
            }
        }
    }

    // Rough estimate: ~4 chars per token (common heuristic)
    (total_chars as f64 / 4.0).ceil() as u32
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_simple_request(model: &str, content: &str) -> ClaudeRequest {
        ClaudeRequest {
            model: model.to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: MessageContent::String(content.to_string()),
            }],
            system: None,
            tools: None,
            stream: false,
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
            top_k: None,
            thinking: None,
            metadata: None,
        }
    }

    #[test]
    fn test_basic_request_transform() {
        let req = make_simple_request("claude-3-5-sonnet", "Hello");
        let (result, sid, msg_count) =
            transform_claude_request(&req, "test-proj", "gemini-2.5-flash").unwrap();

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
    fn test_system_prompt_string() {
        let req = ClaudeRequest {
            model: "claude-3-5-sonnet".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: MessageContent::String("Hello".to_string()),
            }],
            system: Some(SystemPrompt::String("You are helpful".to_string())),
            tools: None,
            stream: false,
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
            top_k: None,
            thinking: None,
            metadata: None,
        };

        let (result, _, _) =
            transform_claude_request(&req, "test-proj", "gemini-2.5-flash").unwrap();

        let sys = &result["request"]["systemInstruction"];
        assert!(sys.get("parts").is_some());
        assert_eq!(sys["parts"][0]["text"], "You are helpful");
    }

    #[test]
    fn test_system_prompt_array() {
        let req = ClaudeRequest {
            model: "claude-3-5-sonnet".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: MessageContent::String("Hello".to_string()),
            }],
            system: Some(SystemPrompt::Array(vec![
                SystemBlock {
                    block_type: "text".to_string(),
                    text: "Part 1".to_string(),
                },
                SystemBlock {
                    block_type: "text".to_string(),
                    text: "Part 2".to_string(),
                },
            ])),
            tools: None,
            stream: false,
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
            top_k: None,
            thinking: None,
            metadata: None,
        };

        let (result, _, _) =
            transform_claude_request(&req, "test-proj", "gemini-2.5-flash").unwrap();

        let parts = result["request"]["systemInstruction"]["parts"]
            .as_array()
            .unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["text"], "Part 1");
        assert_eq!(parts[1]["text"], "Part 2");
    }

    #[test]
    fn test_thinking_config() {
        let req = ClaudeRequest {
            model: "claude-3-5-sonnet".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: MessageContent::String("Think about this".to_string()),
            }],
            system: None,
            tools: None,
            stream: false,
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
            top_k: None,
            thinking: Some(ThinkingConfig {
                type_: "enabled".to_string(),
                budget_tokens: Some(10000),
                effort: None,
            }),
            metadata: None,
        };

        let (result, _, _) =
            transform_claude_request(&req, "test-proj", "gemini-2.5-flash-thinking").unwrap();

        let thinking_config = &result["request"]["generationConfig"]["thinkingConfig"];
        assert_eq!(thinking_config["includeThoughts"], true);
        assert_eq!(thinking_config["thinkingBudget"], 10000);
    }

    #[test]
    fn test_thinking_budget_capped_for_gemini() {
        let req = ClaudeRequest {
            model: "claude-3-5-sonnet".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: MessageContent::String("Think hard".to_string()),
            }],
            system: None,
            tools: None,
            stream: false,
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
            top_k: None,
            thinking: Some(ThinkingConfig {
                type_: "enabled".to_string(),
                budget_tokens: Some(100000),
                effort: None,
            }),
            metadata: None,
        };

        let (result, _, _) =
            transform_claude_request(&req, "test-proj", "gemini-2.5-flash-thinking").unwrap();

        let budget = result["request"]["generationConfig"]["thinkingConfig"]["thinkingBudget"]
            .as_i64()
            .unwrap();
        assert_eq!(budget, 24576); // Capped
    }

    #[test]
    fn test_tool_use_transform() {
        let req = ClaudeRequest {
            model: "claude-3-5-sonnet".to_string(),
            messages: vec![
                Message {
                    role: "user".to_string(),
                    content: MessageContent::String("Get weather".to_string()),
                },
                Message {
                    role: "assistant".to_string(),
                    content: MessageContent::Array(vec![ContentBlock::ToolUse {
                        id: "call_123".to_string(),
                        name: "get_weather".to_string(),
                        input: json!({"city": "Tokyo"}),
                        signature: None,
                        cache_control: None,
                    }]),
                },
                Message {
                    role: "user".to_string(),
                    content: MessageContent::Array(vec![ContentBlock::ToolResult {
                        tool_use_id: "call_123".to_string(),
                        content: json!("Sunny, 25°C"),
                        is_error: None,
                    }]),
                },
            ],
            system: None,
            tools: Some(vec![Tool {
                type_: None,
                name: Some("get_weather".to_string()),
                description: Some("Get weather info".to_string()),
                input_schema: Some(json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } }
                })),
            }]),
            stream: false,
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
            top_k: None,
            thinking: None,
            metadata: None,
        };

        let (result, _, _) =
            transform_claude_request(&req, "test-proj", "gemini-2.5-flash").unwrap();

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
    fn test_image_content_transform() {
        let req = ClaudeRequest {
            model: "claude-3-5-sonnet".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: MessageContent::Array(vec![
                    ContentBlock::Text {
                        text: "What is this?".to_string(),
                    },
                    ContentBlock::Image {
                        source: ImageSource {
                            source_type: "base64".to_string(),
                            media_type: "image/png".to_string(),
                            data: "iVBORw0KGgo=".to_string(),
                        },
                        cache_control: None,
                    },
                ]),
            }],
            system: None,
            tools: None,
            stream: false,
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
            top_k: None,
            thinking: None,
            metadata: None,
        };

        let (result, _, _) =
            transform_claude_request(&req, "test-proj", "gemini-2.5-flash").unwrap();

        let parts = result["request"]["contents"][0]["parts"]
            .as_array()
            .unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["text"], "What is this?");
        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
    }

    #[test]
    fn test_merge_consecutive_messages() {
        let mut messages = vec![
            Message {
                role: "user".to_string(),
                content: MessageContent::String("Hello".to_string()),
            },
            Message {
                role: "user".to_string(),
                content: MessageContent::String("World".to_string()),
            },
        ];

        merge_consecutive_messages(&mut messages);
        assert_eq!(messages.len(), 1);
        match &messages[0].content {
            MessageContent::String(s) => assert_eq!(s, "Hello\n\nWorld"),
            _ => panic!("Expected string content"),
        }
    }

    #[test]
    fn test_clean_cache_control() {
        let mut messages = vec![Message {
            role: "assistant".to_string(),
            content: MessageContent::Array(vec![ContentBlock::Thinking {
                thinking: "test".to_string(),
                signature: Some("sig".to_string()),
                cache_control: Some(json!({"type": "ephemeral"})),
            }]),
        }];

        clean_cache_control_from_messages(&mut messages);

        match &messages[0].content {
            MessageContent::Array(blocks) => match &blocks[0] {
                ContentBlock::Thinking { cache_control, .. } => {
                    assert!(cache_control.is_none());
                }
                _ => panic!("Expected Thinking block"),
            },
            _ => panic!("Expected array content"),
        }
    }

    #[test]
    fn test_metadata_user_id_session() {
        let req = ClaudeRequest {
            model: "claude-3-5-sonnet".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: MessageContent::String("Hello".to_string()),
            }],
            system: None,
            tools: None,
            stream: false,
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
            top_k: None,
            thinking: None,
            metadata: Some(Metadata {
                user_id: Some("custom-user-123".to_string()),
            }),
        };

        let (_, sid, _) =
            transform_claude_request(&req, "test-proj", "gemini-2.5-flash").unwrap();
        assert_eq!(sid, "custom-user-123");
    }

    #[test]
    fn test_estimate_token_count() {
        let req = CountTokensRequest {
            model: "claude-3-5-sonnet".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: MessageContent::String("Hello, how are you?".to_string()),
            }],
            system: Some(SystemPrompt::String("You are helpful".to_string())),
            tools: None,
        };

        let count = estimate_token_count(&req);
        assert!(count > 0);
        // "Hello, how are you?" = 19 chars + "You are helpful" = 15 chars = 34 chars / 4 ≈ 9
        assert!(count >= 8 && count <= 12);
    }

    #[test]
    fn test_no_system_instruction_when_empty() {
        let req = make_simple_request("claude-3-5-sonnet", "Hello");
        let (result, _, _) =
            transform_claude_request(&req, "test-proj", "gemini-2.5-flash").unwrap();

        // No system prompt provided, so systemInstruction should not be present
        assert!(result["request"].get("systemInstruction").is_none());
    }

    #[test]
    fn test_thinking_disabled_for_non_thinking_model() {
        let req = ClaudeRequest {
            model: "claude-3-5-sonnet".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: MessageContent::String("Think".to_string()),
            }],
            system: None,
            tools: None,
            stream: false,
            max_tokens: Some(1024),
            temperature: None,
            top_p: None,
            top_k: None,
            thinking: Some(ThinkingConfig {
                type_: "enabled".to_string(),
                budget_tokens: Some(10000),
                effort: None,
            }),
            metadata: None,
        };

        // gemini-2.5-flash is NOT a thinking model
        let (result, _, _) =
            transform_claude_request(&req, "test-proj", "gemini-2.5-flash").unwrap();

        // thinkingConfig should NOT be present since target model doesn't support it
        assert!(result["request"]["generationConfig"]
            .get("thinkingConfig")
            .is_none());
    }
}
