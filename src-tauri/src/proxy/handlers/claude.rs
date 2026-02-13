// Claude Handler - /v1/messages, /v1/messages/count_tokens
//
// Requirements covered:
// - 2.2: POST /v1/messages → Gemini
// - 2.15: POST /v1/messages/count_tokens

use axum::{
    body::Body,
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures::StreamExt;
use serde_json::{json, Value};
use tracing::{debug, info};

use super::common::{apply_retry_strategy, determine_retry_strategy, should_rotate_account};
use super::AppState;
use crate::proxy::mappers::claude::{
    clean_cache_control_from_messages, create_claude_sse_stream, estimate_token_count,
    merge_consecutive_messages, transform_claude_request, transform_response,
    ClaudeRequest, CountTokensRequest,
    models::GeminiResponse,
};
use crate::proxy::session_manager::SessionManager;

const MAX_RETRY_ATTEMPTS: usize = 3;

/// Handle Claude Messages: POST /v1/messages [Req 2.2]
pub async fn handle_messages(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let trace_id = format!(
        "claude_{}",
        chrono::Utc::now().timestamp_subsec_millis()
    );

    // Parse request
    let mut request: ClaudeRequest = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "type": "error",
                    "error": {
                        "type": "invalid_request_error",
                        "message": format!("Invalid request body: {}", e)
                    }
                })),
            )
                .into_response();
        }
    };

    info!(
        "[{}] Claude Request | Model: {} | Stream: {} | Messages: {}",
        trace_id,
        request.model,
        request.stream,
        request.messages.len(),
    );

    // Pre-process messages
    clean_cache_control_from_messages(&mut request.messages);
    merge_consecutive_messages(&mut request.messages);

    let upstream = state.upstream.clone();
    let token_manager = state.token_manager.clone();
    let pool_size = token_manager.len();
    let max_attempts = MAX_RETRY_ATTEMPTS.min(pool_size.saturating_add(1)).max(2);

    let mut last_error = String::new();
    let mut _last_email: Option<String> = None;
    let mut last_mapped_model: Option<String> = None;

    for attempt in 0..max_attempts {
        // Model route resolution
        let mapped_model = crate::proxy::common::model_mapping::map_model(
            &request.model,
            &*state.custom_mapping.read().await,
            false,
        );
        last_mapped_model = Some(mapped_model.clone());

        // Extract session ID for sticky scheduling
        let session_id_str = SessionManager::extract_session_id(
            &serde_json::to_value(&request).unwrap(),
        );

        // Get token
        let token = match token_manager
            .get_token(&mapped_model, Some(&session_id_str))
            .await
        {
            Ok(t) => t,
            Err(e) => {
                let mapped = last_mapped_model.clone().unwrap_or_default();
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [("X-Mapped-Model", mapped.as_str())],
                    Json(json!({
                        "type": "error",
                        "error": {
                            "type": "overloaded_error",
                            "message": format!("No available accounts: {}", e)
                        }
                    })),
                )
                    .into_response();
            }
        };

        _last_email = Some(token.email.clone());
        let project_id = token.project_id.clone().unwrap_or_default();
        info!("✓ Using account: {}", token.email);

        // Transform request
        let (gemini_body, _session_id, _message_count) =
            match transform_claude_request(&request, &project_id, &mapped_model) {
                Ok(result) => result,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "type": "error",
                            "error": {
                                "type": "invalid_request_error",
                                "message": format!("Transform error: {}", e)
                            }
                        })),
                    )
                        .into_response();
                }
            };

        // Determine streaming
        let client_wants_stream = request.stream;
        let force_stream_internally = !client_wants_stream;
        let actual_stream = client_wants_stream || force_stream_internally;

        let method = if actual_stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };
        let query_string = if actual_stream { Some("alt=sse") } else { None };

        // Send upstream request
        let call_result = match upstream
            .call_v1_internal(
                method,
                &token.access_token,
                gemini_body,
                query_string,
                Some(token.account_id.as_str()),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_error = e.clone();
                debug!(
                    "Claude Request failed on attempt {}/{}: {}",
                    attempt + 1,
                    max_attempts,
                    e
                );
                continue;
            }
        };

        let response = call_result.response;
        let status = response.status();

        if status.is_success() {
            token_manager.mark_success(&token.account_id);

            if actual_stream {
                let claude_stream = create_claude_sse_stream(
                    Box::pin(response.bytes_stream()),
                    trace_id.clone(),
                    token.email.clone(),
                );

                if client_wants_stream {
                    // Return SSE stream
                    let body = Body::from_stream(claude_stream);
                    return Response::builder()
                        .header("Content-Type", "text/event-stream")
                        .header("Cache-Control", "no-cache")
                        .header("Connection", "keep-alive")
                        .header("X-Accel-Buffering", "no")
                        .header("X-Account-Email", &token.email)
                        .header("X-Mapped-Model", &mapped_model)
                        .body(body)
                        .unwrap()
                        .into_response();
                } else {
                    // Aggregate stream to non-streaming response
                    let mut full_text = String::new();
                    let mut stream = Box::pin(claude_stream);

                    while let Some(chunk) = stream.next().await {
                        if let Ok(bytes) = chunk {
                            let text = String::from_utf8_lossy(&bytes);
                            // Extract text content from SSE events
                            for line in text.lines() {
                                if let Some(data) = line.strip_prefix("data: ") {
                                    if let Ok(event) = serde_json::from_str::<Value>(data) {
                                        if let Some(delta) = event
                                            .get("delta")
                                            .and_then(|d| d.get("text"))
                                            .and_then(|t| t.as_str())
                                        {
                                            full_text.push_str(delta);
                                        }
                                    }
                                }
                            }
                        }
                    }

                    let msg_id = uuid::Uuid::new_v4().to_string().replace('-', "");
                    let resp = json!({
                        "id": format!("msg_{}", &msg_id[..24.min(msg_id.len())]),
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "text", "text": full_text}],
                        "model": request.model,
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 0, "output_tokens": 0}
                    });

                    return (
                        StatusCode::OK,
                        [
                            ("X-Account-Email", token.email.as_str()),
                            ("X-Mapped-Model", mapped_model.as_str()),
                        ],
                        Json(resp),
                    )
                        .into_response();
                }
            }

            // Non-streaming response
            let gemini_resp: GeminiResponse = match response.json().await {
                Ok(json) => json,
                Err(e) => {
                    return (
                        StatusCode::BAD_GATEWAY,
                        format!("Parse error: {}", e),
                    )
                        .into_response();
                }
            };

            let claude_response = match transform_response(&gemini_resp) {
                Ok(resp) => serde_json::to_value(resp).unwrap_or(json!({"type": "error"})),
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Transform error: {}", e),
                    )
                        .into_response();
                }
            };

            return (
                StatusCode::OK,
                [
                    ("X-Account-Email", token.email.as_str()),
                    ("X-Mapped-Model", mapped_model.as_str()),
                ],
                Json(claude_response),
            )
                .into_response();
        }

        // Handle errors
        let status_code = status.as_u16();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| format!("HTTP {}", status_code));
        last_error = format!("HTTP {}: {}", status_code, error_text);

        let strategy = determine_retry_strategy(status_code, &error_text, false);

        if apply_retry_strategy(strategy, attempt, max_attempts, status_code, &trace_id).await {
            if should_rotate_account(status_code) {
                tracing::warn!(
                    "Claude Upstream {} on {} attempt {}/{}, rotating",
                    status_code,
                    token.email,
                    attempt + 1,
                    max_attempts
                );
            }
            continue;
        }

        // Non-retryable error
        return (
            StatusCode::from_u16(status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({
                "type": "error",
                "error": {
                    "type": "api_error",
                    "message": error_text
                }
            })),
        )
            .into_response();
    }

    // All attempts exhausted
    let mapped = last_mapped_model.unwrap_or_default();
    (
        StatusCode::TOO_MANY_REQUESTS,
        [("X-Mapped-Model", mapped.as_str())],
        Json(json!({
            "type": "error",
            "error": {
                "type": "overloaded_error",
                "message": format!("All accounts exhausted. Last error: {}", last_error)
            }
        })),
    )
        .into_response()
}

/// Handle Claude Token Count: POST /v1/messages/count_tokens [Req 2.15]
pub async fn handle_count_tokens(
    State(_state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let request: CountTokensRequest = serde_json::from_value(body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid request: {}", e)))?;

    let token_count = estimate_token_count(&request);

    Ok(Json(json!({
        "input_tokens": token_count
    })))
}

/// Handle Claude Model List: GET /v1/models (Anthropic format)
pub async fn handle_list_models(State(state): State<AppState>) -> impl IntoResponse {
    let custom_mapping = state.custom_mapping.read().await;
    let mut model_ids = crate::proxy::common::model_mapping::get_supported_models();

    for key in custom_mapping.keys() {
        if !model_ids.contains(key) {
            model_ids.push(key.clone());
        }
    }
    model_ids.sort();

    let data: Vec<_> = model_ids
        .into_iter()
        .map(|id| {
            json!({
                "id": id,
                "type": "model",
                "display_name": id.clone(),
                "created_at": "2024-01-31T00:00:00Z"
            })
        })
        .collect();

    Json(json!({
        "data": data,
        "has_more": false,
        "first_id": data.first().and_then(|d| d.get("id")).and_then(|v| v.as_str()).unwrap_or(""),
        "last_id": data.last().and_then(|d| d.get("id")).and_then(|v| v.as_str()).unwrap_or("")
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_tokens_basic() {
        let request = CountTokensRequest {
            model: "claude-3-5-sonnet".to_string(),
            messages: vec![crate::proxy::mappers::claude::models::Message {
                role: "user".to_string(),
                content: crate::proxy::mappers::claude::models::MessageContent::String(
                    "Hello, world!".to_string(),
                ),
            }],
            system: None,
            tools: None,
        };

        let count = estimate_token_count(&request);
        assert!(count > 0, "Token count should be positive");
    }
}
