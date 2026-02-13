// Claude streaming response transformation (Gemini SSE → Claude SSE)
//
// Requirements covered:
// - 2.2: Gemini streaming → Anthropic Messages SSE format
// - 2.6: SSE streaming response
// - 2.8: SSE heartbeat to keep connection alive

use super::models::*;
use super::response::to_claude_usage;
use bytes::Bytes;
use serde_json::{json, Value};

/// Block type in the streaming state machine
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    None,
    Text,
    Thinking,
    Function,
}

/// Streaming state machine for Claude SSE conversion
pub struct StreamingState {
    block_type: BlockType,
    pub block_index: usize,
    pub message_start_sent: bool,
    pub message_stop_sent: bool,
    used_tool: bool,
    pending_signature: Option<String>,
}

impl StreamingState {
    pub fn new() -> Self {
        Self {
            block_type: BlockType::None,
            block_index: 0,
            message_start_sent: false,
            message_stop_sent: false,
            used_tool: false,
            pending_signature: None,
        }
    }

    /// Emit an SSE event
    pub fn emit(&self, event_type: &str, data: Value) -> Bytes {
        let sse = format!(
            "event: {}\ndata: {}\n\n",
            event_type,
            serde_json::to_string(&data).unwrap_or_default()
        );
        Bytes::from(sse)
    }

    /// Emit message_start event
    pub fn emit_message_start(&mut self, raw_json: &Value) -> Bytes {
        if self.message_start_sent {
            return Bytes::new();
        }

        let usage = raw_json
            .get("usageMetadata")
            .and_then(|u| serde_json::from_value::<UsageMetadata>(u.clone()).ok())
            .map(|u| to_claude_usage(&u));

        let mut message = json!({
            "id": raw_json.get("responseId")
                .and_then(|v| v.as_str())
                .unwrap_or("msg_unknown"),
            "type": "message",
            "role": "assistant",
            "content": [],
            "model": raw_json.get("modelVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "stop_reason": null,
            "stop_sequence": null,
        });

        if let Some(u) = usage {
            message["usage"] = json!(u);
        }

        let result = self.emit(
            "message_start",
            json!({ "type": "message_start", "message": message }),
        );
        self.message_start_sent = true;
        result
    }

    /// Start a new content block
    pub fn start_block(&mut self, block_type: BlockType, content_block: Value) -> Vec<Bytes> {
        let mut chunks = Vec::new();
        if self.block_type != BlockType::None {
            chunks.extend(self.end_block());
        }

        chunks.push(self.emit(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": self.block_index,
                "content_block": content_block
            }),
        ));

        self.block_type = block_type;
        chunks
    }

    /// End the current content block
    pub fn end_block(&mut self) -> Vec<Bytes> {
        if self.block_type == BlockType::None {
            return vec![];
        }

        let mut chunks = Vec::new();

        // Emit pending signature for thinking blocks
        if self.block_type == BlockType::Thinking {
            if let Some(signature) = self.pending_signature.take() {
                chunks.push(self.emit_delta("signature_delta", json!({ "signature": signature })));
            }
        }

        chunks.push(self.emit(
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": self.block_index }),
        ));

        self.block_index += 1;
        self.block_type = BlockType::None;
        chunks
    }

    /// Emit a delta event
    pub fn emit_delta(&self, delta_type: &str, delta_content: Value) -> Bytes {
        let mut delta = json!({ "type": delta_type });
        if let Value::Object(map) = delta_content {
            for (k, v) in map {
                delta[k] = v;
            }
        }

        self.emit(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": self.block_index,
                "delta": delta
            }),
        )
    }

    /// Emit finish events (message_delta + message_stop)
    pub fn emit_finish(
        &mut self,
        finish_reason: Option<&str>,
        usage_metadata: Option<&UsageMetadata>,
    ) -> Vec<Bytes> {
        let mut chunks = Vec::new();

        // Close last block
        chunks.extend(self.end_block());

        let stop_reason = if self.used_tool {
            "tool_use"
        } else if finish_reason == Some("MAX_TOKENS") {
            "max_tokens"
        } else {
            "end_turn"
        };

        let usage = usage_metadata
            .map(to_claude_usage)
            .unwrap_or(Usage {
                input_tokens: 0,
                output_tokens: 0,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                server_tool_use: None,
            });

        chunks.push(self.emit(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                "usage": usage
            }),
        ));

        if !self.message_stop_sent {
            chunks.push(Bytes::from(
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            ));
            self.message_stop_sent = true;
        }

        chunks
    }

    pub fn mark_tool_used(&mut self) {
        self.used_tool = true;
    }

    pub fn current_block_type(&self) -> BlockType {
        self.block_type
    }

    pub fn store_signature(&mut self, signature: Option<String>) {
        if signature.is_some() {
            self.pending_signature = signature;
        }
    }
}

/// Part processor - handles individual Gemini parts in streaming mode
pub struct PartProcessor<'a> {
    state: &'a mut StreamingState,
}

impl<'a> PartProcessor<'a> {
    pub fn new(state: &'a mut StreamingState) -> Self {
        Self { state }
    }

    /// Process a single Gemini part and return Claude SSE chunks
    pub fn process(&mut self, part: &GeminiPart) -> Vec<Bytes> {
        let mut chunks = Vec::new();
        let signature = part.thought_signature.clone();

        // 1. FunctionCall handling
        if let Some(fc) = &part.function_call {
            chunks.extend(self.process_function_call(fc, signature));
            return chunks;
        }

        // 2. Text handling
        if let Some(text) = &part.text {
            if part.thought.unwrap_or(false) {
                chunks.extend(self.process_thinking(text, signature));
            } else {
                chunks.extend(self.process_text(text, signature));
            }
        }

        // 3. InlineData (Image) handling
        if let Some(img) = &part.inline_data {
            if !img.data.is_empty() {
                let markdown_img = format!("![image](data:{};base64,{})", img.mime_type, img.data);
                chunks.extend(self.process_text(&markdown_img, None));
            }
        }

        chunks
    }

    fn process_thinking(&mut self, text: &str, signature: Option<String>) -> Vec<Bytes> {
        let mut chunks = Vec::new();

        if self.state.current_block_type() != BlockType::Thinking {
            chunks.extend(self.state.start_block(
                BlockType::Thinking,
                json!({ "type": "thinking", "thinking": "" }),
            ));
        }

        if !text.is_empty() {
            chunks.push(
                self.state
                    .emit_delta("thinking_delta", json!({ "thinking": text })),
            );
        }

        self.state.store_signature(signature);
        chunks
    }

    fn process_text(&mut self, text: &str, signature: Option<String>) -> Vec<Bytes> {
        let mut chunks = Vec::new();

        if text.is_empty() {
            return chunks;
        }

        // If we have a signature with text, store it
        if signature.is_some() {
            self.state.store_signature(signature);
        }

        if self.state.current_block_type() != BlockType::Text {
            chunks.extend(
                self.state
                    .start_block(BlockType::Text, json!({ "type": "text", "text": "" })),
            );
        }

        chunks.push(self.state.emit_delta("text_delta", json!({ "text": text })));
        chunks
    }

    fn process_function_call(
        &mut self,
        fc: &FunctionCall,
        signature: Option<String>,
    ) -> Vec<Bytes> {
        let mut chunks = Vec::new();

        self.state.mark_tool_used();

        let tool_id = fc
            .id
            .clone()
            .unwrap_or_else(|| format!("toolu_{}", uuid::Uuid::new_v4()));

        let mut tool_use = json!({
            "type": "tool_use",
            "id": tool_id,
            "name": fc.name,
            "input": {}
        });

        if let Some(ref sig) = signature {
            tool_use["signature"] = json!(sig);
        }

        chunks.extend(self.state.start_block(BlockType::Function, tool_use));

        // Send input as delta
        if let Some(args) = &fc.args {
            let json_str = serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string());
            chunks.push(
                self.state
                    .emit_delta("input_json_delta", json!({ "partial_json": json_str })),
            );
        }

        chunks.extend(self.state.end_block());
        chunks
    }
}

/// Create a Claude SSE stream from a Gemini SSE stream
pub fn create_claude_sse_stream(
    mut gemini_stream: std::pin::Pin<
        Box<dyn futures::Stream<Item = Result<Bytes, reqwest::Error>> + Send>,
    >,
    _trace_id: String,
    _email: String,
) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, String>> + Send>> {
    use bytes::BytesMut;
    use futures::StreamExt;

    Box::pin(async_stream::stream! {
        let mut state = StreamingState::new();
        let mut buffer = BytesMut::new();

        loop {
            // 60-second heartbeat timeout
            let next_chunk = tokio::time::timeout(
                std::time::Duration::from_secs(60),
                gemini_stream.next()
            ).await;

            match next_chunk {
                Ok(Some(chunk_result)) => {
                    match chunk_result {
                        Ok(chunk) => {
                            buffer.extend_from_slice(&chunk);

                            while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
                                let line_raw = buffer.split_to(pos + 1);
                                if let Ok(line_str) = std::str::from_utf8(&line_raw) {
                                    let line = line_str.trim();
                                    if line.is_empty() { continue; }

                                    if let Some(sse_chunks) = process_sse_line(line, &mut state) {
                                        for sse_chunk in sse_chunks {
                                            yield Ok(sse_chunk);
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            yield Err(format!("Stream error: {}", e));
                            break;
                        }
                    }
                }
                Ok(None) => break, // Stream ended
                Err(_) => {
                    // Timeout - send heartbeat (Requirement 2.8)
                    yield Ok(Bytes::from(": ping\n\n"));
                }
            }
        }

        // Flush remaining buffer
        if !buffer.is_empty() {
            if let Ok(line_str) = std::str::from_utf8(&buffer) {
                let line = line_str.trim();
                if !line.is_empty() {
                    if let Some(sse_chunks) = process_sse_line(line, &mut state) {
                        for sse_chunk in sse_chunks {
                            yield Ok(sse_chunk);
                        }
                    }
                }
            }
            buffer.clear();
        }

        // Ensure termination events are sent
        for chunk in emit_force_stop(&mut state) {
            yield Ok(chunk);
        }
    })
}

/// Process a single SSE line from the Gemini stream
fn process_sse_line(line: &str, state: &mut StreamingState) -> Option<Vec<Bytes>> {
    if !line.starts_with("data: ") {
        return None;
    }

    let data_str = line[6..].trim();
    if data_str.is_empty() {
        return None;
    }

    if data_str == "[DONE]" {
        let chunks = emit_force_stop(state);
        return if chunks.is_empty() { None } else { Some(chunks) };
    }

    let json_value: Value = match serde_json::from_str(data_str) {
        Ok(v) => v,
        Err(_) => return None,
    };

    let mut chunks = Vec::new();

    // Unwrap response field if present
    let raw_json = json_value.get("response").unwrap_or(&json_value);

    // Send message_start
    if !state.message_start_sent {
        chunks.push(state.emit_message_start(raw_json));
    }

    // Process all parts
    if let Some(parts) = raw_json
        .get("candidates")
        .and_then(|c| c.get(0))
        .and_then(|cand| cand.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(|p| p.as_array())
    {
        for part_value in parts {
            if let Ok(part) = serde_json::from_value::<GeminiPart>(part_value.clone()) {
                let mut processor = PartProcessor::new(state);
                chunks.extend(processor.process(&part));
            }
        }
    }

    // Check for finish
    if let Some(finish_reason) = raw_json
        .get("candidates")
        .and_then(|c| c.get(0))
        .and_then(|cand| cand.get("finishReason"))
        .and_then(|f| f.as_str())
    {
        let usage = raw_json
            .get("usageMetadata")
            .and_then(|u| serde_json::from_value::<UsageMetadata>(u.clone()).ok());

        chunks.extend(state.emit_finish(Some(finish_reason), usage.as_ref()));
    }

    if chunks.is_empty() {
        None
    } else {
        Some(chunks)
    }
}

/// Emit force stop events if not already sent
pub fn emit_force_stop(state: &mut StreamingState) -> Vec<Bytes> {
    if !state.message_stop_sent {
        let mut chunks = state.emit_finish(None, None);
        if chunks.is_empty() {
            chunks.push(Bytes::from(
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            ));
            state.message_stop_sent = true;
        }
        return chunks;
    }
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_streaming_state_emit() {
        let state = StreamingState::new();
        let chunk = state.emit("test_event", json!({"foo": "bar"}));
        let s = String::from_utf8(chunk.to_vec()).unwrap();
        assert!(s.contains("event: test_event"));
        assert!(s.contains("\"foo\":\"bar\""));
    }

    #[test]
    fn test_process_sse_line_done() {
        let mut state = StreamingState::new();
        let result = process_sse_line("data: [DONE]", &mut state);
        assert!(result.is_some());
        let chunks = result.unwrap();
        let all_text: String = chunks
            .iter()
            .map(|b| String::from_utf8(b.to_vec()).unwrap_or_default())
            .collect();
        assert!(all_text.contains("message_stop"));
    }

    #[test]
    fn test_process_sse_line_with_text() {
        let mut state = StreamingState::new();
        let test_data = r#"data: {"candidates":[{"content":{"parts":[{"text":"Hello"}]}}],"usageMetadata":{},"modelVersion":"test","responseId":"123"}"#;

        let result = process_sse_line(test_data, &mut state);
        assert!(result.is_some());

        let chunks = result.unwrap();
        let all_text: String = chunks
            .iter()
            .map(|b| String::from_utf8(b.to_vec()).unwrap_or_default())
            .collect();

        assert!(all_text.contains("message_start"));
        assert!(all_text.contains("content_block_start"));
        assert!(all_text.contains("Hello"));
    }

    #[test]
    fn test_process_function_call_streaming() {
        let mut state = StreamingState::new();
        state.message_start_sent = true; // Skip message_start

        let fc = FunctionCall {
            name: "test_tool".to_string(),
            args: Some(json!({"arg": "value"})),
            id: Some("call_123".to_string()),
        };

        let part = GeminiPart {
            text: None,
            function_call: Some(fc),
            inline_data: None,
            thought: None,
            thought_signature: None,
            function_response: None,
        };

        let mut processor = PartProcessor::new(&mut state);
        let chunks = processor.process(&part);
        let output: String = chunks
            .iter()
            .map(|b| String::from_utf8(b.to_vec()).unwrap())
            .collect();

        assert!(output.contains("content_block_start"));
        assert!(output.contains("test_tool"));
        assert!(output.contains("input_json_delta"));
        assert!(output.contains("content_block_stop"));
    }

    #[test]
    fn test_thinking_then_text_streaming() {
        let mut state = StreamingState::new();
        state.message_start_sent = true;

        // Process thinking part
        let thinking_part = GeminiPart {
            text: Some("Thinking...".to_string()),
            thought: Some(true),
            thought_signature: Some("sig_abc".to_string()),
            function_call: None,
            function_response: None,
            inline_data: None,
        };

        let mut processor = PartProcessor::new(&mut state);
        let chunks1 = processor.process(&thinking_part);
        let output1: String = chunks1
            .iter()
            .map(|b| String::from_utf8(b.to_vec()).unwrap())
            .collect();
        assert!(output1.contains("thinking"));
        assert!(output1.contains("Thinking..."));

        // Process text part
        let text_part = GeminiPart {
            text: Some("The answer".to_string()),
            thought: None,
            thought_signature: None,
            function_call: None,
            function_response: None,
            inline_data: None,
        };

        let mut processor2 = PartProcessor::new(&mut state);
        let chunks2 = processor2.process(&text_part);
        let output2: String = chunks2
            .iter()
            .map(|b| String::from_utf8(b.to_vec()).unwrap())
            .collect();

        // Should close thinking block and start text block
        assert!(output2.contains("content_block_stop"));
        assert!(output2.contains("text_delta"));
        assert!(output2.contains("The answer"));
    }

    #[test]
    fn test_non_sse_line_ignored() {
        let mut state = StreamingState::new();
        assert!(process_sse_line("not an sse line", &mut state).is_none());
        assert!(process_sse_line("event: something", &mut state).is_none());
    }
}
