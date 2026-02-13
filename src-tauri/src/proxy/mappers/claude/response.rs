// Gemini → Claude response transformation
//
// Requirements covered:
// - 2.2: Gemini response → Anthropic Messages response

use super::models::*;

/// Convert Gemini UsageMetadata to Claude Usage format
pub fn to_claude_usage(usage_metadata: &UsageMetadata) -> Usage {
    let prompt_tokens = usage_metadata.prompt_token_count.unwrap_or(0);
    let cached_tokens = usage_metadata.cached_content_token_count.unwrap_or(0);

    let (reported_input, reported_cache) = if cached_tokens > 0 && prompt_tokens > 0 {
        (prompt_tokens.saturating_sub(cached_tokens), Some(cached_tokens))
    } else {
        (prompt_tokens, None)
    };

    Usage {
        input_tokens: reported_input,
        output_tokens: usage_metadata.candidates_token_count.unwrap_or(0),
        cache_read_input_tokens: reported_cache,
        cache_creation_input_tokens: Some(0),
        server_tool_use: None,
    }
}

/// Non-streaming response processor
pub struct NonStreamingProcessor {
    content_blocks: Vec<ContentBlock>,
    text_builder: String,
    thinking_builder: String,
    thinking_signature: Option<String>,
    pub has_tool_call: bool,
}

impl NonStreamingProcessor {
    pub fn new() -> Self {
        Self {
            content_blocks: Vec::new(),
            text_builder: String::new(),
            thinking_builder: String::new(),
            thinking_signature: None,
            has_tool_call: false,
        }
    }

    /// Process a Gemini response and convert to Claude response
    pub fn process(&mut self, gemini_response: &GeminiResponse) -> ClaudeResponse {
        let empty_parts = vec![];
        let parts = gemini_response
            .candidates
            .as_ref()
            .and_then(|c| c.get(0))
            .and_then(|candidate| candidate.content.as_ref())
            .map(|content| &content.parts)
            .unwrap_or(&empty_parts);

        for part in parts {
            self.process_part(part);
        }

        // Flush remaining content
        self.flush_thinking();
        self.flush_text();

        self.build_response(gemini_response)
    }

    /// Process a single Gemini part
    fn process_part(&mut self, part: &GeminiPart) {
        let signature = part.thought_signature.clone();

        // 1. FunctionCall handling
        if let Some(fc) = &part.function_call {
            self.flush_thinking();
            self.flush_text();
            self.has_tool_call = true;

            let tool_id = fc
                .id
                .clone()
                .unwrap_or_else(|| format!("toolu_{}", uuid::Uuid::new_v4()));

            let args = fc.args.clone().unwrap_or(serde_json::json!({}));

            self.content_blocks.push(ContentBlock::ToolUse {
                id: tool_id,
                name: fc.name.clone(),
                input: args,
                signature,
                cache_control: None,
            });
            return;
        }

        // 2. Text handling
        if let Some(text) = &part.text {
            if part.thought.unwrap_or(false) {
                // Thinking part
                self.flush_text();
                self.thinking_builder.push_str(text);
                if signature.is_some() {
                    self.thinking_signature = signature;
                }
            } else if !text.is_empty() {
                // Regular text
                self.flush_thinking();
                self.text_builder.push_str(text);
            }
        }

        // 3. InlineData (Image) handling
        if let Some(img) = &part.inline_data {
            self.flush_thinking();
            if !img.data.is_empty() {
                let markdown_img = format!("![image](data:{};base64,{})", img.mime_type, img.data);
                self.text_builder.push_str(&markdown_img);
                self.flush_text();
            }
        }
    }

    fn flush_text(&mut self) {
        if self.text_builder.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.text_builder);
        self.content_blocks.push(ContentBlock::Text { text });
    }

    fn flush_thinking(&mut self) {
        if self.thinking_builder.is_empty() && self.thinking_signature.is_none() {
            return;
        }
        let thinking = std::mem::take(&mut self.thinking_builder);
        let signature = self.thinking_signature.take();
        self.content_blocks.push(ContentBlock::Thinking {
            thinking,
            signature,
            cache_control: None,
        });
    }

    fn build_response(&self, gemini_response: &GeminiResponse) -> ClaudeResponse {
        let finish_reason = gemini_response
            .candidates
            .as_ref()
            .and_then(|c| c.get(0))
            .and_then(|candidate| candidate.finish_reason.as_deref());

        let stop_reason = if self.has_tool_call {
            "tool_use"
        } else if finish_reason == Some("MAX_TOKENS") {
            "max_tokens"
        } else {
            "end_turn"
        };

        let usage = gemini_response
            .usage_metadata
            .as_ref()
            .map(to_claude_usage)
            .unwrap_or(Usage {
                input_tokens: 0,
                output_tokens: 0,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                server_tool_use: None,
            });

        ClaudeResponse {
            id: gemini_response
                .response_id
                .clone()
                .unwrap_or_else(|| format!("msg_{}", uuid::Uuid::new_v4())),
            type_: "message".to_string(),
            role: "assistant".to_string(),
            model: gemini_response.model_version.clone().unwrap_or_default(),
            content: self.content_blocks.clone(),
            stop_reason: stop_reason.to_string(),
            stop_sequence: None,
            usage,
        }
    }
}

/// Transform a Gemini response into a Claude response (non-streaming)
pub fn transform_response(gemini_response: &GeminiResponse) -> Result<ClaudeResponse, String> {
    let mut processor = NonStreamingProcessor::new();
    Ok(processor.process(gemini_response))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_text_response() {
        let gemini_resp = GeminiResponse {
            candidates: Some(vec![Candidate {
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![GeminiPart {
                        text: Some("Hello, world!".to_string()),
                        thought: None,
                        thought_signature: None,
                        function_call: None,
                        function_response: None,
                        inline_data: None,
                    }],
                }),
                finish_reason: Some("STOP".to_string()),
                index: Some(0),
            }]),
            usage_metadata: Some(UsageMetadata {
                prompt_token_count: Some(10),
                candidates_token_count: Some(5),
                total_token_count: Some(15),
                cached_content_token_count: None,
            }),
            model_version: Some("gemini-2.5-flash".to_string()),
            response_id: Some("resp_123".to_string()),
        };

        let result = transform_response(&gemini_resp);
        assert!(result.is_ok());

        let claude_resp = result.unwrap();
        assert_eq!(claude_resp.role, "assistant");
        assert_eq!(claude_resp.stop_reason, "end_turn");
        assert_eq!(claude_resp.content.len(), 1);

        match &claude_resp.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello, world!"),
            _ => panic!("Expected Text block"),
        }

        assert_eq!(claude_resp.usage.input_tokens, 10);
        assert_eq!(claude_resp.usage.output_tokens, 5);
    }

    #[test]
    fn test_thinking_with_signature() {
        let gemini_resp = GeminiResponse {
            candidates: Some(vec![Candidate {
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![
                        GeminiPart {
                            text: Some("Let me think...".to_string()),
                            thought: Some(true),
                            thought_signature: Some("sig123".to_string()),
                            function_call: None,
                            function_response: None,
                            inline_data: None,
                        },
                        GeminiPart {
                            text: Some("The answer is 42".to_string()),
                            thought: None,
                            thought_signature: None,
                            function_call: None,
                            function_response: None,
                            inline_data: None,
                        },
                    ],
                }),
                finish_reason: Some("STOP".to_string()),
                index: Some(0),
            }]),
            usage_metadata: None,
            model_version: Some("gemini-2.5-flash".to_string()),
            response_id: Some("resp_456".to_string()),
        };

        let result = transform_response(&gemini_resp).unwrap();
        assert_eq!(result.content.len(), 2);

        match &result.content[0] {
            ContentBlock::Thinking {
                thinking, signature, ..
            } => {
                assert_eq!(thinking, "Let me think...");
                assert_eq!(signature.as_deref(), Some("sig123"));
            }
            _ => panic!("Expected Thinking block"),
        }

        match &result.content[1] {
            ContentBlock::Text { text } => assert_eq!(text, "The answer is 42"),
            _ => panic!("Expected Text block"),
        }
    }

    #[test]
    fn test_tool_use_response() {
        let gemini_resp = GeminiResponse {
            candidates: Some(vec![Candidate {
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![GeminiPart {
                        text: None,
                        thought: None,
                        thought_signature: None,
                        function_call: Some(FunctionCall {
                            name: "get_weather".to_string(),
                            id: Some("call_abc".to_string()),
                            args: Some(serde_json::json!({"city": "Tokyo"})),
                        }),
                        function_response: None,
                        inline_data: None,
                    }],
                }),
                finish_reason: Some("STOP".to_string()),
                index: Some(0),
            }]),
            usage_metadata: None,
            model_version: Some("gemini-2.5-flash".to_string()),
            response_id: Some("resp_789".to_string()),
        };

        let result = transform_response(&gemini_resp).unwrap();
        assert_eq!(result.stop_reason, "tool_use");
        assert_eq!(result.content.len(), 1);

        match &result.content[0] {
            ContentBlock::ToolUse { id, name, input, .. } => {
                assert_eq!(id, "call_abc");
                assert_eq!(name, "get_weather");
                assert_eq!(input["city"], "Tokyo");
            }
            _ => panic!("Expected ToolUse block"),
        }
    }

    #[test]
    fn test_usage_with_cache() {
        let usage = UsageMetadata {
            prompt_token_count: Some(100),
            candidates_token_count: Some(50),
            total_token_count: Some(150),
            cached_content_token_count: Some(30),
        };

        let claude_usage = to_claude_usage(&usage);
        assert_eq!(claude_usage.input_tokens, 70); // 100 - 30
        assert_eq!(claude_usage.output_tokens, 50);
        assert_eq!(claude_usage.cache_read_input_tokens, Some(30));
    }

    #[test]
    fn test_empty_response() {
        let gemini_resp = GeminiResponse {
            candidates: Some(vec![Candidate {
                content: None,
                finish_reason: Some("STOP".to_string()),
                index: Some(0),
            }]),
            usage_metadata: None,
            model_version: Some("gemini-2.5-flash".to_string()),
            response_id: Some("resp_empty".to_string()),
        };

        let result = transform_response(&gemini_resp).unwrap();
        assert_eq!(result.content.len(), 0);
        assert_eq!(result.stop_reason, "end_turn");
    }

    #[test]
    fn test_inline_image_response() {
        let gemini_resp = GeminiResponse {
            candidates: Some(vec![Candidate {
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![GeminiPart {
                        text: None,
                        thought: None,
                        thought_signature: None,
                        function_call: None,
                        function_response: None,
                        inline_data: Some(InlineData {
                            mime_type: "image/png".to_string(),
                            data: "abc123".to_string(),
                        }),
                    }],
                }),
                finish_reason: Some("STOP".to_string()),
                index: Some(0),
            }]),
            usage_metadata: None,
            model_version: Some("gemini-3-pro-image".to_string()),
            response_id: Some("resp_img".to_string()),
        };

        let result = transform_response(&gemini_resp).unwrap();
        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            ContentBlock::Text { text } => {
                assert!(text.contains("![image](data:image/png;base64,abc123)"));
            }
            _ => panic!("Expected Text block with image markdown"),
        }
    }

    #[test]
    fn test_max_tokens_stop_reason() {
        let gemini_resp = GeminiResponse {
            candidates: Some(vec![Candidate {
                content: Some(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![GeminiPart {
                        text: Some("Partial response...".to_string()),
                        thought: None,
                        thought_signature: None,
                        function_call: None,
                        function_response: None,
                        inline_data: None,
                    }],
                }),
                finish_reason: Some("MAX_TOKENS".to_string()),
                index: Some(0),
            }]),
            usage_metadata: None,
            model_version: Some("gemini-2.5-flash".to_string()),
            response_id: Some("resp_max".to_string()),
        };

        let result = transform_response(&gemini_resp).unwrap();
        assert_eq!(result.stop_reason, "max_tokens");
    }
}
