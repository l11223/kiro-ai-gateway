// OpenAI 流式转换
//
// Requirements covered:
// - 2.6: SSE (Server-Sent Events) streaming response
// - 2.7: Stream aggregation for non-streaming requests

use bytes::{Bytes, BytesMut};
use chrono::Utc;
use futures::{Stream, StreamExt};
use serde_json::{json, Value};
use std::pin::Pin;
use uuid::Uuid;

/// Create an OpenAI-compatible SSE stream from a Gemini stream.
///
/// Converts Gemini streaming chunks into OpenAI chat.completion.chunk format.
pub fn create_openai_sse_stream(
    mut gemini_stream: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    model: String,
    _session_id: String,
    _message_count: usize,
) -> Pin<Box<dyn Stream<Item = Result<Bytes, String>> + Send>> {
    let mut buffer = BytesMut::new();
    let stream_id = format!("chatcmpl-{}", Uuid::new_v4());
    let created_ts = Utc::now().timestamp();

    let stream = async_stream::stream! {
        let mut emitted_tool_calls = std::collections::HashSet::new();
        let mut final_usage: Option<super::models::OpenAIUsage> = None;
        let mut error_occurred = false;

        let mut heartbeat_interval = tokio::time::interval(std::time::Duration::from_secs(15));
        heartbeat_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                item = gemini_stream.next() => {
                    match item {
                        Some(Ok(bytes)) => {
                            buffer.extend_from_slice(&bytes);
                            while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
                                let line_raw = buffer.split_to(pos + 1);
                                if let Ok(line_str) = std::str::from_utf8(&line_raw) {
                                    let line = line_str.trim();
                                    if line.is_empty() { continue; }
                                    if line.starts_with("data: ") {
                                        let json_part = line.trim_start_matches("data: ").trim();
                                        if json_part == "[DONE]" { continue; }
                                        if let Ok(mut json) = serde_json::from_str::<Value>(json_part) {
                                            let actual_data = if let Some(inner) = json.get_mut("response").map(|v| v.take()) {
                                                inner
                                            } else {
                                                json
                                            };

                                            // Extract usage metadata
                                            if let Some(u) = actual_data.get("usageMetadata") {
                                                final_usage = extract_usage_metadata(u);
                                            }

                                            if let Some(candidates) = actual_data.get("candidates").and_then(|c| c.as_array()) {
                                                for (idx, candidate) in candidates.iter().enumerate() {
                                                    let parts = candidate.get("content")
                                                        .and_then(|c| c.get("parts"))
                                                        .and_then(|p| p.as_array());
                                                    let mut content_out = String::new();
                                                    let mut thought_out = String::new();

                                                    if let Some(parts_list) = parts {
                                                        let mut tool_call_index = 0;
                                                        for part in parts_list {
                                                            let is_thought = part.get("thought")
                                                                .and_then(|v| v.as_bool())
                                                                .unwrap_or(false);

                                                            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                                                if is_thought {
                                                                    thought_out.push_str(text);
                                                                } else {
                                                                    content_out.push_str(text);
                                                                }
                                                            }

                                                            // Inline image
                                                            if let Some(img) = part.get("inlineData") {
                                                                let mime = img.get("mimeType").and_then(|v| v.as_str()).unwrap_or("image/png");
                                                                let data = img.get("data").and_then(|v| v.as_str()).unwrap_or("");
                                                                if !data.is_empty() {
                                                                    content_out.push_str(&format!("![image](data:{};base64,{})", mime, data));
                                                                }
                                                            }

                                                            // Tool calls
                                                            if let Some(func_call) = part.get("functionCall") {
                                                                let call_key = serde_json::to_string(func_call).unwrap_or_default();
                                                                if !emitted_tool_calls.contains(&call_key) {
                                                                    emitted_tool_calls.insert(call_key);
                                                                    let name = func_call.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
                                                                    let args = func_call.get("args").unwrap_or(&json!({})).clone();
                                                                    let args_str = serde_json::to_string(&args).unwrap_or_default();

                                                                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                                                                    use std::hash::{Hash, Hasher};
                                                                    serde_json::to_string(func_call).unwrap_or_default().hash(&mut hasher);
                                                                    let call_id = format!("call_{:x}", hasher.finish());

                                                                    let tool_chunk = json!({
                                                                        "id": &stream_id,
                                                                        "object": "chat.completion.chunk",
                                                                        "created": created_ts,
                                                                        "model": &model,
                                                                        "choices": [{
                                                                            "index": idx as u32,
                                                                            "delta": {
                                                                                "role": "assistant",
                                                                                "tool_calls": [{
                                                                                    "index": tool_call_index,
                                                                                    "id": call_id,
                                                                                    "type": "function",
                                                                                    "function": { "name": name, "arguments": args_str }
                                                                                }]
                                                                            },
                                                                            "finish_reason": serde_json::Value::Null
                                                                        }]
                                                                    });
                                                                    tool_call_index += 1;
                                                                    yield Ok::<Bytes, String>(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&tool_chunk).unwrap_or_default())));
                                                                }
                                                            }
                                                        }
                                                    }

                                                    let gemini_finish = candidate.get("finishReason")
                                                        .and_then(|f| f.as_str())
                                                        .map(|f| match f {
                                                            "STOP" => "stop",
                                                            "MAX_TOKENS" => "length",
                                                            "SAFETY" => "content_filter",
                                                            "RECITATION" => "content_filter",
                                                            _ => f,
                                                        });

                                                    // If tool calls were emitted, force finish_reason to tool_calls
                                                    let finish_reason = if !emitted_tool_calls.is_empty() && gemini_finish.is_some() {
                                                        Some("tool_calls")
                                                    } else {
                                                        gemini_finish
                                                    };

                                                    // Emit reasoning content chunk
                                                    if !thought_out.is_empty() {
                                                        let reasoning_chunk = json!({
                                                            "id": &stream_id,
                                                            "object": "chat.completion.chunk",
                                                            "created": created_ts,
                                                            "model": &model,
                                                            "choices": [{
                                                                "index": idx as u32,
                                                                "delta": {
                                                                    "role": "assistant",
                                                                    "content": serde_json::Value::Null,
                                                                    "reasoning_content": thought_out
                                                                },
                                                                "finish_reason": serde_json::Value::Null
                                                            }]
                                                        });
                                                        yield Ok::<Bytes, String>(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&reasoning_chunk).unwrap_or_default())));
                                                    }

                                                    // Emit content chunk
                                                    if !content_out.is_empty() || finish_reason.is_some() {
                                                        let mut chunk = json!({
                                                            "id": &stream_id,
                                                            "object": "chat.completion.chunk",
                                                            "created": created_ts,
                                                            "model": &model,
                                                            "choices": [{
                                                                "index": idx as u32,
                                                                "delta": { "content": content_out },
                                                                "finish_reason": finish_reason
                                                            }]
                                                        });
                                                        if let Some(ref usage) = final_usage {
                                                            chunk["usage"] = serde_json::to_value(usage).unwrap();
                                                        }
                                                        if finish_reason.is_some() { final_usage = None; }
                                                        yield Ok::<Bytes, String>(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap_or_default())));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Some(Err(e)) => {
                            tracing::error!("OpenAI Stream Error: {}", e);
                            let error_chunk = json!({
                                "id": &stream_id,
                                "object": "chat.completion.chunk",
                                "created": created_ts,
                                "model": &model,
                                "choices": [],
                                "error": {
                                    "type": "stream_error",
                                    "message": format!("Stream error: {}", e),
                                    "code": "stream_error"
                                }
                            });
                            yield Ok(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&error_chunk).unwrap_or_default())));
                            yield Ok(Bytes::from("data: [DONE]\n\n"));
                            error_occurred = true;
                            break;
                        }
                        None => break,
                    }
                }
                _ = heartbeat_interval.tick() => {
                    // SSE heartbeat to keep connection alive (Requirement 2.8)
                    yield Ok::<Bytes, String>(Bytes::from(": ping\n\n"));
                }
            }
        }

        if !error_occurred {
            yield Ok::<Bytes, String>(Bytes::from("data: [DONE]\n\n"));
        }
    };
    Box::pin(stream)
}

/// Extract and convert Gemini usageMetadata to OpenAI usage format.
fn extract_usage_metadata(u: &Value) -> Option<super::models::OpenAIUsage> {
    use super::models::{OpenAIUsage, PromptTokensDetails};

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
}
