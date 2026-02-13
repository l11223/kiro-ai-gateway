// OpenAI 协议响应转换模块
//
// Requirements covered:
// - 2.1: Gemini response → OpenAI ChatCompletion response
// - 2.9: Semantic equivalence of message content through conversion

use super::models::*;
use serde_json::Value;

/// Transform a Gemini response into OpenAI ChatCompletion format.
pub fn transform_openai_response(
    gemini_response: &Value,
    _session_id: Option<&str>,
    _message_count: usize,
) -> OpenAIResponse {
    // Unwrap the response field if present
    let raw = gemini_response
        .get("response")
        .unwrap_or(gemini_response);

    let mut choices = Vec::new();

    if let Some(candidates) = raw.get("candidates").and_then(|c| c.as_array()) {
        for (idx, candidate) in candidates.iter().enumerate() {
            let mut content_out = String::new();
            let mut thought_out = String::new();
            let mut tool_calls = Vec::new();

            if let Some(parts) = candidate
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
            {
                for part in parts {
                    let is_thought_part = part
                        .get("thought")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                        if is_thought_part {
                            thought_out.push_str(text);
                        } else {
                            content_out.push_str(text);
                        }
                    }

                    // Tool call parts
                    if let Some(fc) = part.get("functionCall") {
                        let name = fc
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let args = fc
                            .get("args")
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "{}".to_string());
                        let id = fc
                            .get("id")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("{}-{}", name, uuid::Uuid::new_v4()));

                        tool_calls.push(ToolCall {
                            id,
                            r#type: "function".to_string(),
                            function: ToolFunction {
                                name: name.to_string(),
                                arguments: args,
                            },
                        });
                    }

                    // Inline image data
                    if let Some(img) = part.get("inlineData") {
                        let mime_type = img
                            .get("mimeType")
                            .and_then(|v| v.as_str())
                            .unwrap_or("image/png");
                        let data = img.get("data").and_then(|v| v.as_str()).unwrap_or("");
                        if !data.is_empty() {
                            content_out.push_str(&format!(
                                "![image](data:{};base64,{})",
                                mime_type, data
                            ));
                        }
                    }
                }
            }

            // Extract grounding metadata (web search results)
            if let Some(grounding) = candidate.get("groundingMetadata") {
                let mut grounding_text = String::new();

                if let Some(queries) =
                    grounding.get("webSearchQueries").and_then(|q| q.as_array())
                {
                    let query_list: Vec<&str> =
                        queries.iter().filter_map(|v| v.as_str()).collect();
                    if !query_list.is_empty() {
                        grounding_text.push_str("\n\n---\n**🔍 Search:** ");
                        grounding_text.push_str(&query_list.join(", "));
                    }
                }

                if let Some(chunks) =
                    grounding.get("groundingChunks").and_then(|c| c.as_array())
                {
                    let mut links = Vec::new();
                    for (i, chunk) in chunks.iter().enumerate() {
                        if let Some(web) = chunk.get("web") {
                            let title = web
                                .get("title")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Source");
                            let uri = web.get("uri").and_then(|v| v.as_str()).unwrap_or("#");
                            links.push(format!("[{}] [{}]({})", i + 1, title, uri));
                        }
                    }
                    if !links.is_empty() {
                        grounding_text.push_str("\n\n**🌐 Sources:**\n");
                        grounding_text.push_str(&links.join("\n"));
                    }
                }

                if !grounding_text.is_empty() {
                    content_out.push_str(&grounding_text);
                }
            }

            let finish_reason = candidate
                .get("finishReason")
                .and_then(|f| f.as_str())
                .map(|f| match f {
                    "STOP" => "stop",
                    "MAX_TOKENS" => "length",
                    "SAFETY" => "content_filter",
                    "RECITATION" => "content_filter",
                    _ => "stop",
                })
                .unwrap_or("stop");

            choices.push(Choice {
                index: idx as u32,
                message: OpenAIMessage {
                    role: "assistant".to_string(),
                    content: if content_out.is_empty() {
                        None
                    } else {
                        Some(OpenAIContent::String(content_out))
                    },
                    reasoning_content: if thought_out.is_empty() {
                        None
                    } else {
                        Some(thought_out)
                    },
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                    tool_call_id: None,
                    name: None,
                },
                finish_reason: Some(finish_reason.to_string()),
            });
        }
    }

    // Extract usage metadata
    let usage = raw.get("usageMetadata").and_then(|u| {
        let prompt_tokens = u
            .get("promptTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let completion_tokens = u
            .get("candidatesTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let total_tokens = u
            .get("totalTokenCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let cached_tokens = u
            .get("cachedContentTokenCount")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        Some(OpenAIUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            prompt_tokens_details: cached_tokens.map(|ct| PromptTokensDetails {
                cached_tokens: Some(ct),
            }),
            completion_tokens_details: None,
        })
    });

    OpenAIResponse {
        id: raw
            .get("responseId")
            .and_then(|v| v.as_str())
            .unwrap_or("resp_unknown")
            .to_string(),
        object: "chat.completion".to_string(),
        created: chrono::Utc::now().timestamp() as u64,
        model: raw
            .get("modelVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        choices,
        usage,
    }
}

/// Transform a Gemini image generation response into OpenAI format.
pub fn transform_image_response(gemini_response: &Value) -> ImageGenerationResponse {
    let raw = gemini_response
        .get("response")
        .unwrap_or(gemini_response);

    let mut data = Vec::new();

    if let Some(candidates) = raw.get("candidates").and_then(|c| c.as_array()) {
        for candidate in candidates {
            if let Some(parts) = candidate
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
            {
                for part in parts {
                    if let Some(img) = part.get("inlineData") {
                        let b64 = img
                            .get("data")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !b64.is_empty() {
                            data.push(ImageData {
                                url: None,
                                b64_json: Some(b64),
                                revised_prompt: None,
                            });
                        }
                    }
                }
            }
        }
    }

    ImageGenerationResponse {
        created: chrono::Utc::now().timestamp() as u64,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_basic_response_transform() {
        let gemini_resp = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello!"}]
                },
                "finishReason": "STOP"
            }],
            "modelVersion": "gemini-2.5-flash",
            "responseId": "resp_123"
        });

        let result = transform_openai_response(&gemini_resp, Some("session-123"), 1);
        assert_eq!(result.object, "chat.completion");
        assert_eq!(result.id, "resp_123");
        let content = match result.choices[0].message.content.as_ref().unwrap() {
            OpenAIContent::String(s) => s.clone(),
            _ => panic!("Expected string content"),
        };
        assert_eq!(content, "Hello!");
        assert_eq!(result.choices[0].finish_reason, Some("stop".to_string()));
    }

    #[test]
    fn test_usage_metadata_mapping() {
        let gemini_resp = json!({
            "candidates": [{
                "content": {"parts": [{"text": "Hello!"}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 100,
                "candidatesTokenCount": 50,
                "totalTokenCount": 150,
                "cachedContentTokenCount": 25
            },
            "modelVersion": "gemini-2.5-flash",
            "responseId": "resp_123"
        });

        let result = transform_openai_response(&gemini_resp, Some("session-123"), 1);
        assert!(result.usage.is_some());
        let usage = result.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.total_tokens, 150);
        assert!(usage.prompt_tokens_details.is_some());
        assert_eq!(
            usage.prompt_tokens_details.unwrap().cached_tokens,
            Some(25)
        );
    }

    #[test]
    fn test_response_without_usage() {
        let gemini_resp = json!({
            "candidates": [{
                "content": {"parts": [{"text": "Hello!"}]},
                "finishReason": "STOP"
            }],
            "modelVersion": "gemini-2.5-flash",
            "responseId": "resp_123"
        });

        let result = transform_openai_response(&gemini_resp, Some("session-123"), 1);
        assert!(result.usage.is_none());
    }

    #[test]
    fn test_thinking_content_separation() {
        let gemini_resp = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "Let me think...", "thought": true},
                        {"text": "The answer is 42."}
                    ]
                },
                "finishReason": "STOP"
            }],
            "modelVersion": "gemini-2.5-flash",
            "responseId": "resp_456"
        });

        let result = transform_openai_response(&gemini_resp, None, 1);
        let choice = &result.choices[0];

        // Thinking content should be in reasoning_content
        assert_eq!(
            choice.message.reasoning_content.as_deref(),
            Some("Let me think...")
        );
        // Regular content should be in content
        let content = match choice.message.content.as_ref().unwrap() {
            OpenAIContent::String(s) => s.clone(),
            _ => panic!("Expected string content"),
        };
        assert_eq!(content, "The answer is 42.");
    }

    #[test]
    fn test_tool_call_response() {
        let gemini_resp = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": {
                            "name": "get_weather",
                            "args": {"city": "Tokyo"},
                            "id": "call_abc"
                        }
                    }]
                },
                "finishReason": "STOP"
            }],
            "modelVersion": "gemini-2.5-flash",
            "responseId": "resp_789"
        });

        let result = transform_openai_response(&gemini_resp, None, 1);
        let choice = &result.choices[0];
        assert!(choice.message.tool_calls.is_some());
        let tc = &choice.message.tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.function.name, "get_weather");
        assert_eq!(tc.id, "call_abc");
    }

    #[test]
    fn test_finish_reason_mapping() {
        let test_cases = vec![
            ("STOP", "stop"),
            ("MAX_TOKENS", "length"),
            ("SAFETY", "content_filter"),
            ("RECITATION", "content_filter"),
            ("UNKNOWN", "stop"),
        ];

        for (gemini_reason, expected) in test_cases {
            let gemini_resp = json!({
                "candidates": [{
                    "content": {"parts": [{"text": "test"}]},
                    "finishReason": gemini_reason
                }],
                "modelVersion": "test",
                "responseId": "test"
            });

            let result = transform_openai_response(&gemini_resp, None, 1);
            assert_eq!(
                result.choices[0].finish_reason,
                Some(expected.to_string()),
                "Failed for Gemini reason: {}",
                gemini_reason
            );
        }
    }

    #[test]
    fn test_multiple_candidates() {
        let gemini_resp = json!({
            "candidates": [
                {
                    "content": {"parts": [{"text": "Answer A"}]},
                    "finishReason": "STOP"
                },
                {
                    "content": {"parts": [{"text": "Answer B"}]},
                    "finishReason": "STOP"
                }
            ],
            "modelVersion": "gemini-2.5-flash",
            "responseId": "resp_multi"
        });

        let result = transform_openai_response(&gemini_resp, None, 1);
        assert_eq!(result.choices.len(), 2);
        assert_eq!(result.choices[0].index, 0);
        assert_eq!(result.choices[1].index, 1);
    }

    #[test]
    fn test_nested_response_field() {
        // Some responses wrap in a "response" field
        let gemini_resp = json!({
            "response": {
                "candidates": [{
                    "content": {"parts": [{"text": "Nested!"}]},
                    "finishReason": "STOP"
                }],
                "modelVersion": "gemini-2.5-flash",
                "responseId": "resp_nested"
            }
        });

        let result = transform_openai_response(&gemini_resp, None, 1);
        let content = match result.choices[0].message.content.as_ref().unwrap() {
            OpenAIContent::String(s) => s.clone(),
            _ => panic!("Expected string content"),
        };
        assert_eq!(content, "Nested!");
    }

    #[test]
    fn test_image_response_transform() {
        let gemini_resp = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "inlineData": {
                            "mimeType": "image/png",
                            "data": "iVBORw0KGgo="
                        }
                    }]
                },
                "finishReason": "STOP"
            }]
        });

        let result = transform_image_response(&gemini_resp);
        assert_eq!(result.data.len(), 1);
        assert_eq!(result.data[0].b64_json.as_deref(), Some("iVBORw0KGgo="));
    }

    #[test]
    fn test_inline_image_in_chat_response() {
        let gemini_resp = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "inlineData": {
                            "mimeType": "image/png",
                            "data": "abc123"
                        }
                    }]
                },
                "finishReason": "STOP"
            }],
            "modelVersion": "gemini-3-pro-image",
            "responseId": "resp_img"
        });

        let result = transform_openai_response(&gemini_resp, None, 1);
        let content = match result.choices[0].message.content.as_ref().unwrap() {
            OpenAIContent::String(s) => s.clone(),
            _ => panic!("Expected string content"),
        };
        assert!(content.contains("![image](data:image/png;base64,abc123)"));
    }
}
