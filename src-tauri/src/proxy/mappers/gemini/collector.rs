// Gemini Stream Collector
// Collects streaming SSE responses into a complete JSON response
//
// Requirements covered:
// - 2.7: Aggregate streaming response into non-streaming format

use bytes::Bytes;
use futures::StreamExt;
use serde_json::{json, Value};
use tracing::debug;

/// Collect a Gemini SSE stream into a complete Gemini response Value.
///
/// Handles:
/// - Parsing SSE `data:` lines
/// - Unwrapping v1internal response wrappers
/// - Merging adjacent text parts
/// - Capturing usage metadata and finish reason
pub async fn collect_stream_to_json<S, E>(mut stream: S) -> Result<Value, String>
where
    S: futures::Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut collected_response = json!({
        "candidates": [{
            "content": {
                "parts": [],
                "role": "model"
            },
            "finishReason": "STOP",
            "index": 0
        }]
    });

    let mut content_parts: Vec<Value> = Vec::new();
    let mut usage_metadata: Option<Value> = None;
    let mut finish_reason: Option<String> = None;

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| format!("Stream error: {}", e))?;
        let text = std::str::from_utf8(&chunk).unwrap_or("");

        for line in text.lines() {
            let line = line.trim();
            if !line.starts_with("data: ") {
                continue;
            }

            let json_part = line.trim_start_matches("data: ").trim();
            if json_part == "[DONE]" {
                continue;
            }

            if let Ok(mut parsed) = serde_json::from_str::<Value>(json_part) {
                // Unwrap v1internal response wrapper
                let actual_data = if let Some(inner) = parsed.get_mut("response").map(|v| v.take())
                {
                    inner
                } else {
                    parsed
                };

                // Capture usage metadata
                if let Some(usage) = actual_data.get("usageMetadata") {
                    usage_metadata = Some(usage.clone());
                }

                // Capture content parts
                if let Some(candidates) = actual_data.get("candidates").and_then(|c| c.as_array())
                {
                    if let Some(candidate) = candidates.first() {
                        if let Some(fr) = candidate.get("finishReason").and_then(|v| v.as_str()) {
                            finish_reason = Some(fr.to_string());
                        }

                        if let Some(parts) = candidate
                            .get("content")
                            .and_then(|c| c.get("parts"))
                            .and_then(|p| p.as_array())
                        {
                            for part in parts {
                                // Merge adjacent text parts (optimization)
                                if let Some(text_val) = part.get("text").and_then(|v| v.as_str()) {
                                    if let Some(last) = content_parts.last_mut() {
                                        if last.get("text").is_some()
                                            && part.get("thought").is_none()
                                            && last.get("thought").is_none()
                                        {
                                            if let Some(last_text) =
                                                last.get("text").and_then(|v| v.as_str())
                                            {
                                                let new_text =
                                                    format!("{}{}", last_text, text_val);
                                                *last = json!({"text": new_text});
                                                continue;
                                            }
                                        }
                                    }
                                    content_parts.push(part.clone());
                                } else {
                                    content_parts.push(part.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Construct final response
    collected_response["candidates"][0]["content"]["parts"] = json!(content_parts);
    if let Some(fr) = finish_reason {
        collected_response["candidates"][0]["finishReason"] = json!(fr);
    }
    if let Some(usage) = usage_metadata {
        collected_response["usageMetadata"] = usage;
    }

    debug!(
        "[Gemini-Collector] Collected {} parts into complete response",
        content_parts.len()
    );

    Ok(collected_response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    #[tokio::test]
    async fn test_collect_simple_stream() {
        let chunks = vec![
            Ok::<Bytes, String>(Bytes::from(
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}],\"role\":\"model\"},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":5}}\n\n",
            )),
        ];

        let stream = stream::iter(chunks);
        let result = collect_stream_to_json(stream).await.unwrap();

        let parts = result["candidates"][0]["content"]["parts"]
            .as_array()
            .unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "Hello");
        assert!(result.get("usageMetadata").is_some());
    }

    #[tokio::test]
    async fn test_collect_multi_chunk_stream() {
        let chunks = vec![
            Ok::<Bytes, String>(Bytes::from(
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello \"}],\"role\":\"model\"}}]}\n\n",
            )),
            Ok(Bytes::from(
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"World\"}],\"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\n",
            )),
        ];

        let stream = stream::iter(chunks);
        let result = collect_stream_to_json(stream).await.unwrap();

        let parts = result["candidates"][0]["content"]["parts"]
            .as_array()
            .unwrap();
        // Adjacent text parts should be merged
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "Hello World");
    }

    #[tokio::test]
    async fn test_collect_with_done_marker() {
        let chunks = vec![
            Ok::<Bytes, String>(Bytes::from(
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hi\"}],\"role\":\"model\"}}]}\n\ndata: [DONE]\n\n",
            )),
        ];

        let stream = stream::iter(chunks);
        let result = collect_stream_to_json(stream).await.unwrap();

        let parts = result["candidates"][0]["content"]["parts"]
            .as_array()
            .unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "Hi");
    }

    #[tokio::test]
    async fn test_collect_v1internal_wrapped() {
        let chunks = vec![Ok::<Bytes, String>(Bytes::from(
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Wrapped\"}],\"role\":\"model\"},\"finishReason\":\"STOP\"}]}}\n\n",
        ))];

        let stream = stream::iter(chunks);
        let result = collect_stream_to_json(stream).await.unwrap();

        let parts = result["candidates"][0]["content"]["parts"]
            .as_array()
            .unwrap();
        assert_eq!(parts[0]["text"], "Wrapped");
    }

    #[tokio::test]
    async fn test_collect_empty_stream() {
        let chunks: Vec<Result<Bytes, String>> = vec![];
        let stream = stream::iter(chunks);
        let result = collect_stream_to_json(stream).await.unwrap();

        let parts = result["candidates"][0]["content"]["parts"]
            .as_array()
            .unwrap();
        assert!(parts.is_empty());
    }
}
