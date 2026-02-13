// Gemini Handler - /v1beta/models/:model
//
// Requirements covered:
// - 2.3: POST /v1beta/models/:model → Gemini native passthrough

use axum::{
    extract::{Json, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use serde_json::{json, Value};
use tracing::{debug, error, info};

use super::common::{apply_retry_strategy, determine_retry_strategy, should_rotate_account};
use super::AppState;
use crate::proxy::mappers::gemini::{unwrap_response, wrap_request};
use crate::proxy::session_manager::SessionManager;

const MAX_RETRY_ATTEMPTS: usize = 3;

/// Handle Gemini generateContent / streamGenerateContent
/// Path: /v1beta/models/:model_action (e.g. "gemini-pro:generateContent")
pub async fn handle_generate(
    State(state): State<AppState>,
    Path(model_action): Path<String>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    // Parse model:method
    let (model_name, method) = if let Some((m, action)) = model_action.rsplit_once(':') {
        (m.to_string(), action.to_string())
    } else {
        (model_action, "generateContent".to_string())
    };

    let trace_id = format!("gemini_{}", chrono::Utc::now().timestamp_subsec_millis());
    info!(
        "[{}] Gemini Request: {}/{}",
        trace_id, model_name, method
    );

    // Validate method
    if method != "generateContent" && method != "streamGenerateContent" {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Unsupported method: {}", method),
        ));
    }

    let client_wants_stream = method == "streamGenerateContent";
    let force_stream_internally = !client_wants_stream;
    let is_stream = client_wants_stream || force_stream_internally;

    let upstream = state.upstream.clone();
    let token_manager = state.token_manager.clone();
    let pool_size = token_manager.len();
    let max_attempts = MAX_RETRY_ATTEMPTS.min(pool_size).max(1);

    let mut last_error = String::new();
    let mut last_email: Option<String> = None;

    for attempt in 0..max_attempts {
        // Model route resolution
        let mapped_model = crate::proxy::common::model_mapping::map_model(
            &model_name,
            &*state.custom_mapping.read().await,
            false,
        );

        // Extract session ID
        let session_id = SessionManager::extract_gemini_session_id(&body, &model_name);

        // Get token
        let token = match token_manager
            .get_token(&mapped_model, Some(&session_id))
            .await
        {
            Ok(t) => t,
            Err(e) => {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("Token error: {}", e),
                ));
            }
        };

        last_email = Some(token.email.clone());
        let project_id = token.project_id.clone().unwrap_or_default();
        info!("✓ Using account: {}", token.email);

        // Wrap request with project injection
        let wrapped_body = wrap_request(&body, &project_id, &mapped_model, Some(&session_id));

        // Upstream call
        let query_string = if is_stream { Some("alt=sse") } else { None };
        let upstream_method = if is_stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };

        let call_result = match upstream
            .call_v1_internal(
                upstream_method,
                &token.access_token,
                wrapped_body,
                query_string,
                Some(token.account_id.as_str()),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_error = e.clone();
                debug!(
                    "Gemini Request failed on attempt {}/{}: {}",
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

            if is_stream {
                use axum::body::Body;

                let mut response_stream = Box::pin(response.bytes_stream());
                let mut buffer = BytesMut::new();

                let stream = async_stream::stream! {
                    loop {
                        let item = response_stream.next().await;
                        let bytes = match item {
                            Some(Ok(b)) => b,
                            Some(Err(e)) => {
                                error!("[Gemini-SSE] Connection error: {}", e);
                                yield Err(format!("Stream error: {}", e));
                                break;
                            }
                            None => break,
                        };

                        buffer.extend_from_slice(&bytes);
                        while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
                            let line_raw = buffer.split_to(pos + 1);
                            if let Ok(line_str) = std::str::from_utf8(&line_raw) {
                                let line = line_str.trim();
                                if line.is_empty() { continue; }

                                if line.starts_with("data: ") {
                                    let json_part = line.trim_start_matches("data: ").trim();
                                    if json_part == "[DONE]" {
                                        yield Ok::<Bytes, String>(Bytes::from("data: [DONE]\n\n"));
                                        continue;
                                    }

                                    match serde_json::from_str::<Value>(json_part) {
                                        Ok(json) => {
                                            // Unwrap v1internal response wrapper
                                            if let Some(inner) = json.get("response") {
                                                let new_line = format!("data: {}\n\n", serde_json::to_string(inner).unwrap_or_default());
                                                yield Ok::<Bytes, String>(Bytes::from(new_line));
                                            } else {
                                                yield Ok::<Bytes, String>(Bytes::from(format!("data: {}\n\n", serde_json::to_string(&json).unwrap_or_default())));
                                            }
                                        }
                                        Err(_) => {
                                            yield Ok::<Bytes, String>(Bytes::from(format!("{}\n\n", line)));
                                        }
                                    }
                                } else {
                                    yield Ok::<Bytes, String>(Bytes::from(format!("{}\n\n", line)));
                                }
                            }
                        }
                    }
                };

                if client_wants_stream {
                    let body = Body::from_stream(stream);
                    return Ok(Response::builder()
                        .header("Content-Type", "text/event-stream")
                        .header("Cache-Control", "no-cache")
                        .header("Connection", "keep-alive")
                        .header("X-Accel-Buffering", "no")
                        .header("X-Account-Email", &token.email)
                        .header("X-Mapped-Model", &mapped_model)
                        .body(body)
                        .unwrap()
                        .into_response());
                } else {
                    // Collect stream to JSON
                    use crate::proxy::mappers::gemini::collector::collect_stream_to_json;
                    match collect_stream_to_json(Box::pin(stream)).await {
                        Ok(gemini_resp) => {
                            let unwrapped = unwrap_response(&gemini_resp);
                            return Ok((
                                StatusCode::OK,
                                [
                                    ("X-Account-Email", token.email.as_str()),
                                    ("X-Mapped-Model", mapped_model.as_str()),
                                ],
                                Json(unwrapped),
                            )
                                .into_response());
                        }
                        Err(e) => {
                            return Ok((
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("Stream collection error: {}", e),
                            )
                                .into_response());
                        }
                    }
                }
            }

            // Non-streaming response
            let gemini_resp: Value = response
                .json()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Parse error: {}", e)))?;

            let unwrapped = unwrap_response(&gemini_resp);
            return Ok((
                StatusCode::OK,
                [
                    ("X-Account-Email", token.email.as_str()),
                    ("X-Mapped-Model", mapped_model.as_str()),
                ],
                Json(unwrapped),
            )
                .into_response());
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
                    "Gemini Upstream {} on {} attempt {}/{}, rotating",
                    status_code,
                    token.email,
                    attempt + 1,
                    max_attempts
                );
            }
            continue;
        }

        // Non-retryable error
        return Ok((
            status,
            [
                ("X-Account-Email", token.email.as_str()),
                ("X-Mapped-Model", mapped_model.as_str()),
            ],
            Json(json!({
                "error": {
                    "code": status_code,
                    "message": error_text,
                    "status": "UPSTREAM_ERROR"
                }
            })),
        )
            .into_response());
    }

    if let Some(email) = last_email {
        Ok((
            StatusCode::TOO_MANY_REQUESTS,
            [("X-Account-Email", email)],
            format!("All accounts exhausted. Last error: {}", last_error),
        )
            .into_response())
    } else {
        Ok((
            StatusCode::TOO_MANY_REQUESTS,
            format!("All accounts exhausted. Last error: {}", last_error),
        )
            .into_response())
    }
}

/// Handle Gemini Model List: GET /v1beta/models
pub async fn handle_list_models(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let custom_mapping = state.custom_mapping.read().await;
    let mut model_ids = crate::proxy::common::model_mapping::get_supported_models();

    for key in custom_mapping.keys() {
        if !model_ids.contains(key) {
            model_ids.push(key.clone());
        }
    }
    model_ids.sort();

    let models: Vec<_> = model_ids
        .into_iter()
        .map(|id| {
            json!({
                "name": format!("models/{}", id),
                "version": "001",
                "displayName": id.clone(),
                "description": "",
                "inputTokenLimit": 128000,
                "outputTokenLimit": 8192,
                "supportedGenerationMethods": ["generateContent", "countTokens"],
                "temperature": 1.0,
                "topP": 0.95,
                "topK": 64
            })
        })
        .collect();

    Ok(Json(json!({ "models": models })))
}

/// Handle Gemini Get Model: GET /v1beta/models/:model_name
pub async fn handle_get_model(Path(model_name): Path<String>) -> impl IntoResponse {
    Json(json!({
        "name": format!("models/{}", model_name),
        "displayName": model_name
    }))
}

/// Handle Gemini Count Tokens: POST /v1beta/models/:model/countTokens
pub async fn handle_count_tokens(
    State(state): State<AppState>,
    Path(_model_name): Path<String>,
    Json(_body): Json<Value>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    // Simple token count estimation
    let _token = state
        .token_manager
        .get_token("gemini", None)
        .await
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("Token error: {}", e)))?;

    Ok(Json(json!({"totalTokens": 0})))
}
