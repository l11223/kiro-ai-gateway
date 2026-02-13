// OpenAI Stream Collector
//
// Used for auto-converting streaming responses to JSON for non-streaming requests.
// Requirement 2.7: Stream aggregation for non-streaming responses.

use super::models::*;
use bytes::Bytes;
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;

/// Collects an OpenAI SSE stream into a complete OpenAIResponse.
///
/// This is used when the client requests `stream: false` but the upstream
/// only supports streaming. We aggregate all chunks into a single response.
pub async fn collect_stream_to_json<S, E>(mut stream: S) -> Result<OpenAIResponse, String>
where
    S: futures::Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut response = OpenAIResponse {
        id: "chatcmpl-unknown".to_string(),
        object: "chat.completion".to_string(),
        created: chrono::Utc::now().timestamp() as u64,
        model: "unknown".to_string(),
        choices: Vec::new(),
        usage: None,
    };

    let mut role: Option<String> = None;
    let mut content_parts: Vec<String> = Vec::new();
    let mut reasoning_parts: Vec<String> = Vec::new();
    let mut finish_reason: Option<String> = None;
    // Tool calls aggregation: index -> (id, type, name, arguments_parts)
    let mut tool_calls_map: HashMap<u32, (String, String, String, Vec<String>)> = HashMap::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| format!("Stream error: {}", e))?;
        let text = String::from_utf8_lossy(&chunk);

        for line in text.lines() {
            let line = line.trim();
            if line.starts_with("data: ") {
                let data_str = line.trim_start_matches("data: ").trim();
                if data_str == "[DONE]" {
                    continue;
                }

                if let Ok(json) = serde_json::from_str::<Value>(data_str) {
                    // Update meta fields
                    if let Some(id) = json.get("id").and_then(|v| v.as_str()) {
                        response.id = id.to_string();
                    }
                    if let Some(model) = json.get("model").and_then(|v| v.as_str()) {
                        response.model = model.to_string();
                    }
                    if let Some(created) = json.get("created").and_then(|v| v.as_u64()) {
                        response.created = created;
                    }

                    // Collect Usage
                    if let Some(usage) = json.get("usage") {
                        if let Ok(u) = serde_json::from_value::<OpenAIUsage>(usage.clone()) {
                            response.usage = Some(u);
                        }
                    }

                    // Collect Choices Delta
                    if let Some(choices) = json.get("choices").and_then(|v| v.as_array()) {
                        if let Some(choice) = choices.first() {
                            if let Some(delta) = choice.get("delta") {
                                if let Some(r) = delta.get("role").and_then(|v| v.as_str()) {
                                    role = Some(r.to_string());
                                }
                                if let Some(c) = delta.get("content").and_then(|v| v.as_str()) {
                                    content_parts.push(c.to_string());
                                }
                                if let Some(rc) =
                                    delta.get("reasoning_content").and_then(|v| v.as_str())
                                {
                                    reasoning_parts.push(rc.to_string());
                                }

                                // Tool calls aggregation by index
                                if let Some(tcs) =
                                    delta.get("tool_calls").and_then(|v| v.as_array())
                                {
                                    for tc in tcs {
                                        let index = tc
                                            .get("index")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(0)
                                            as u32;

                                        let entry =
                                            tool_calls_map.entry(index).or_insert_with(|| {
                                                (
                                                    String::new(),
                                                    "function".to_string(),
                                                    String::new(),
                                                    Vec::new(),
                                                )
                                            });

                                        if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                                            if !id.is_empty() {
                                                entry.0 = id.to_string();
                                            }
                                        }
                                        if let Some(tc_type) =
                                            tc.get("type").and_then(|v| v.as_str())
                                        {
                                            if !tc_type.is_empty() {
                                                entry.1 = tc_type.to_string();
                                            }
                                        }
                                        if let Some(func) = tc.get("function") {
                                            if let Some(name) =
                                                func.get("name").and_then(|v| v.as_str())
                                            {
                                                if !name.is_empty() {
                                                    entry.2 = name.to_string();
                                                }
                                            }
                                            if let Some(args) =
                                                func.get("arguments").and_then(|v| v.as_str())
                                            {
                                                entry.3.push(args.to_string());
                                            }
                                        }
                                    }
                                }
                            }

                            if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str())
                            {
                                finish_reason = Some(fr.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // Construct final message
    let full_content = content_parts.join("");
    let full_reasoning = if reasoning_parts.is_empty() {
        None
    } else {
        Some(reasoning_parts.join(""))
    };

    let final_tool_calls: Option<Vec<ToolCall>> = if tool_calls_map.is_empty() {
        None
    } else {
        let mut calls: Vec<(u32, ToolCall)> = tool_calls_map
            .into_iter()
            .map(|(index, (id, tc_type, name, args_parts))| {
                (
                    index,
                    ToolCall {
                        id,
                        r#type: tc_type,
                        function: ToolFunction {
                            name,
                            arguments: args_parts.join(""),
                        },
                    },
                )
            })
            .collect();
        calls.sort_by_key(|(index, _)| *index);
        Some(calls.into_iter().map(|(_, tc)| tc).collect())
    };

    let message = OpenAIMessage {
        role: role.unwrap_or_else(|| "assistant".to_string()),
        content: Some(OpenAIContent::String(full_content)),
        reasoning_content: full_reasoning,
        tool_calls: final_tool_calls,
        tool_call_id: None,
        name: None,
    };

    response.choices.push(Choice {
        index: 0,
        message,
        finish_reason: finish_reason.or_else(|| Some("stop".to_string())),
    });

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures::stream;

    #[tokio::test]
    async fn test_collect_simple_stream() {
        let chunks = vec![
            Ok::<Bytes, String>(Bytes::from(
                "data: {\"id\":\"chatcmpl-1\",\"model\":\"gemini\",\"created\":1000,\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
            )),
            Ok(Bytes::from(
                "data: {\"id\":\"chatcmpl-1\",\"model\":\"gemini\",\"created\":1000,\"choices\":[{\"delta\":{\"content\":\" World\"},\"finish_reason\":\"stop\"}]}\n\n",
            )),
            Ok(Bytes::from("data: [DONE]\n\n")),
        ];

        let s = stream::iter(chunks);
        let result = collect_stream_to_json(s).await.unwrap();

        assert_eq!(result.id, "chatcmpl-1");
        assert_eq!(result.model, "gemini");
        assert_eq!(result.choices.len(), 1);

        let content = match &result.choices[0].message.content {
            Some(OpenAIContent::String(s)) => s.clone(),
            _ => panic!("Expected string content"),
        };
        assert_eq!(content, "Hello World");
        assert_eq!(
            result.choices[0].finish_reason,
            Some("stop".to_string())
        );
    }

    #[tokio::test]
    async fn test_collect_stream_with_tool_calls() {
        let chunks = vec![
            Ok::<Bytes, String>(Bytes::from(
                "data: {\"id\":\"chatcmpl-2\",\"model\":\"gemini\",\"created\":1000,\"choices\":[{\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\"\"}}]},\"finish_reason\":null}]}\n\n",
            )),
            Ok(Bytes::from(
                "data: {\"id\":\"chatcmpl-2\",\"model\":\"gemini\",\"created\":1000,\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\":\\\"Tokyo\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            )),
            Ok(Bytes::from("data: [DONE]\n\n")),
        ];

        let s = stream::iter(chunks);
        let result = collect_stream_to_json(s).await.unwrap();

        assert!(result.choices[0].message.tool_calls.is_some());
        let tc = &result.choices[0].message.tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.function.name, "get_weather");
        assert_eq!(tc.function.arguments, "{\"city\":\"Tokyo\"}");
    }

    #[tokio::test]
    async fn test_collect_empty_stream() {
        let chunks: Vec<Result<Bytes, String>> =
            vec![Ok(Bytes::from("data: [DONE]\n\n"))];

        let s = stream::iter(chunks);
        let result = collect_stream_to_json(s).await.unwrap();

        assert_eq!(result.choices.len(), 1);
        assert_eq!(
            result.choices[0].finish_reason,
            Some("stop".to_string())
        );
    }

    /// **Feature: kiro-ai-gateway, Property 22: 流式响应聚合完整性**
    /// **Validates: Requirements 2.7**
    ///
    /// For any sequence of SSE chunks in a streaming response, the aggregated
    /// non-streaming response SHALL contain the concatenation of all chunk text contents.
    mod prop_stream_aggregation {
        use super::*;
        use proptest::prelude::*;

        /// Strategy to generate safe text content for SSE chunks.
        /// Avoids characters that would break JSON encoding or SSE framing.
        fn safe_chunk_text() -> impl Strategy<Value = String> {
            "[a-zA-Z0-9 ,.!?]{0,50}".prop_map(|s| s)
        }

        /// Build an SSE data line for a content delta chunk.
        fn make_content_chunk(content: &str, is_first: bool, is_last: bool) -> String {
            let escaped = content.replace('\\', "\\\\").replace('"', "\\\"");
            let role_part = if is_first {
                "\"role\":\"assistant\","
            } else {
                ""
            };
            let finish = if is_last { "\"stop\"" } else { "null" };
            format!(
                "data: {{\"id\":\"chatcmpl-prop\",\"model\":\"test-model\",\"created\":1000,\"choices\":[{{\"delta\":{{{role_part}\"content\":\"{escaped}\"}},\"finish_reason\":{finish}}}]}}\n\n"
            )
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn prop_stream_aggregation_completeness(
                parts in prop::collection::vec(safe_chunk_text(), 1..20)
            ) {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let result = rt.block_on(async {
                    let len = parts.len();
                    let mut sse_chunks: Vec<Result<Bytes, String>> = Vec::with_capacity(len + 1);

                    for (i, part) in parts.iter().enumerate() {
                        let is_first = i == 0;
                        let is_last = i == len - 1;
                        let sse_line = make_content_chunk(part, is_first, is_last);
                        sse_chunks.push(Ok(Bytes::from(sse_line)));
                    }
                    sse_chunks.push(Ok(Bytes::from("data: [DONE]\n\n")));

                    let s = futures::stream::iter(sse_chunks);
                    collect_stream_to_json(s).await.unwrap()
                });

                // Verify aggregated content equals concatenation of all parts
                let expected = parts.join("");
                let actual = match &result.choices[0].message.content {
                    Some(OpenAIContent::String(s)) => s.clone(),
                    _ => panic!("Expected string content"),
                };
                prop_assert_eq!(actual, expected);

                // Verify response metadata is preserved
                prop_assert_eq!(&result.id, "chatcmpl-prop");
                prop_assert_eq!(&result.model, "test-model");
                prop_assert_eq!(result.choices.len(), 1);
                prop_assert_eq!(
                    result.choices[0].finish_reason.as_deref(),
                    Some("stop")
                );
                prop_assert_eq!(&result.choices[0].message.role, "assistant");
            }
        }
    }
}
