// OpenAI Handler - /v1/chat/completions, /v1/models, /v1/images/generations, /v1/images/edits, /v1/completions
//
// Requirements covered:
// - 2.1: POST /v1/chat/completions → Gemini
// - 2.6: stream: true → SSE
// - 2.7: stream: false → aggregate stream to JSON
// - 2.10: POST /v1/images/generations → Imagen 3
// - 2.12: GET /v1/models → OpenAI format model list
// - 2.13: POST /v1/completions → legacy/Codex compat
// - 2.14: POST /v1/images/edits → Image editing API
// - 7.1: Imagen 3 image generation integration
// - 7.2: Resolution/aspect ratio parameter passing
// - 7.4: Image attachment handling (base64/URL → inlineData)
// - 7.5: Image thinking mode control (enabled/disabled)

use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::Engine;
use serde_json::{json, Value};
use tracing::{debug, error, info};

use super::common::{apply_retry_strategy, determine_retry_strategy, should_rotate_account};
use super::AppState;
use crate::proxy::mappers::openai::{
    transform_openai_request, transform_openai_response, OpenAIRequest,
};
use crate::proxy::session_manager::SessionManager;

const MAX_RETRY_ATTEMPTS: usize = 3;

/// Handle OpenAI Chat Completions: POST /v1/chat/completions [Req 2.1]
pub async fn handle_chat_completions(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let openai_req: OpenAIRequest = serde_json::from_value(body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid request: {}", e)))?;

    let trace_id = format!("req_{}", chrono::Utc::now().timestamp_subsec_millis());
    info!(
        "[{}] OpenAI Chat Request: {} | {} messages | stream: {}",
        trace_id,
        openai_req.model,
        openai_req.messages.len(),
        openai_req.stream
    );

    let upstream = state.upstream.clone();
    let token_manager = state.token_manager.clone();
    let pool_size = token_manager.len();
    let max_attempts = MAX_RETRY_ATTEMPTS.min(pool_size.saturating_add(1)).max(2);

    let mut last_error = String::new();
    let mut last_email: Option<String> = None;

    // Model route resolution (outside loop for consistent header)
    let mapped_model = crate::proxy::common::model_mapping::map_model(
        &openai_req.model,
        &*state.custom_mapping.read().await,
        false,
    );

    for attempt in 0..max_attempts {
        // Extract session ID for sticky scheduling
        let session_id =
            SessionManager::extract_openai_session_id(&serde_json::to_value(&openai_req).unwrap());

        // Get token via P2C selection
        let token = match token_manager
            .get_token(&mapped_model, Some(&session_id))
            .await
        {
            Ok(t) => t,
            Err(e) => {
                return Ok((
                    StatusCode::SERVICE_UNAVAILABLE,
                    [("X-Mapped-Model", mapped_model.as_str())],
                    format!("Token error: {}", e),
                )
                    .into_response());
            }
        };

        last_email = Some(token.email.clone());
        let project_id = token.project_id.clone().unwrap_or_default();
        info!("✓ Using account: {}", token.email);

        // Transform request
        let (gemini_body, session_id, message_count) =
            transform_openai_request(&openai_req, &project_id, &mapped_model);

        // Determine streaming mode
        let client_wants_stream = openai_req.stream;
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
                    "OpenAI Request failed on attempt {}/{}: {}",
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
            // Mark success
            token_manager.mark_success(&token.account_id);

            if actual_stream {
                use axum::body::Body;
                use crate::proxy::mappers::openai::streaming::create_openai_sse_stream;

                let openai_stream = create_openai_sse_stream(
                    Box::pin(response.bytes_stream()),
                    openai_req.model.clone(),
                    session_id.clone(),
                    message_count,
                );

                if client_wants_stream {
                    // Client wants SSE stream [Req 2.6]
                    let body = Body::from_stream(openai_stream);
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
                    // Aggregate stream to JSON [Req 2.7]
                    use crate::proxy::mappers::openai::collector::collect_stream_to_json;
                    match collect_stream_to_json(Box::pin(openai_stream)).await {
                        Ok(full_response) => {
                            info!("[{}] ✓ Stream collected to JSON", trace_id);
                            return Ok((
                                StatusCode::OK,
                                [
                                    ("X-Account-Email", token.email.as_str()),
                                    ("X-Mapped-Model", mapped_model.as_str()),
                                ],
                                Json(serde_json::to_value(full_response).unwrap()),
                            )
                                .into_response());
                        }
                        Err(e) => {
                            error!("[{}] Stream collection error: {}", trace_id, e);
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

            let openai_response =
                transform_openai_response(&gemini_resp, Some(&session_id), message_count);
            return Ok((
                StatusCode::OK,
                [
                    ("X-Account-Email", token.email.as_str()),
                    ("X-Mapped-Model", mapped_model.as_str()),
                ],
                Json(serde_json::to_value(openai_response).unwrap()),
            )
                .into_response());
        }

        // Handle errors with retry
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
                    "OpenAI Upstream {} on {} attempt {}/{}, rotating account",
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
                    "message": error_text,
                    "type": "upstream_error",
                    "code": status_code
                }
            })),
        )
            .into_response());
    }

    // All attempts exhausted
    if let Some(email) = last_email {
        Ok((
            StatusCode::TOO_MANY_REQUESTS,
            [
                ("X-Account-Email", email),
                ("X-Mapped-Model", mapped_model),
            ],
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

/// Handle OpenAI Model List: GET /v1/models [Req 2.12]
pub async fn handle_list_models(State(state): State<AppState>) -> impl IntoResponse {
    let custom_mapping = state.custom_mapping.read().await;
    let mut model_ids: Vec<String> =
        crate::proxy::common::model_mapping::get_supported_models();

    // Add custom mapping keys
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
                "object": "model",
                "created": 1706745600,
                "owned_by": "kiro-ai-gateway"
            })
        })
        .collect();

    Json(json!({
        "object": "list",
        "data": data
    }))
}


/// Handle OpenAI Images Generations: POST /v1/images/generations [Req 2.10]
/// Handle OpenAI Images Generations: POST /v1/images/generations [Req 2.10, 7.1, 7.2]
pub async fn handle_images_generations(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let prompt = body.get("prompt").and_then(|v| v.as_str()).ok_or((
        StatusCode::BAD_REQUEST,
        "Missing 'prompt' field".to_string(),
    ))?;

    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("gemini-3-pro-image");

    let n = body.get("n").and_then(|v| v.as_u64()).unwrap_or(1) as usize;

    let size = body
        .get("size")
        .and_then(|v| v.as_str())
        .unwrap_or("1024x1024");

    let response_format = body
        .get("response_format")
        .and_then(|v| v.as_str())
        .unwrap_or("b64_json");

    let quality = body
        .get("quality")
        .and_then(|v| v.as_str())
        .unwrap_or("standard");

    let image_size = body
        .get("image_size")
        .or(body.get("imageSize"))
        .and_then(|v| v.as_str());

    let style = body
        .get("style")
        .and_then(|v| v.as_str())
        .unwrap_or("vivid");

    info!(
        "[Images] Request: model={}, prompt={:.50}..., n={}, size={}, quality={}, style={}",
        model, prompt, n, size, quality, style
    );

    // Parse image config (unified logic with dynamic aspect ratio and quality mapping) [Req 7.2]
    let (image_config, _) = crate::proxy::common::common_utils::parse_image_config_with_params(
        model,
        Some(size),
        Some(quality),
        image_size,
    );

    // Prompt enhancement based on quality and style
    let mut final_prompt = prompt.to_string();
    if quality == "hd" {
        final_prompt.push_str(", (high quality, highly detailed, 4k resolution, hdr)");
    }
    match style {
        "vivid" => final_prompt.push_str(", (vivid colors, dramatic lighting, rich details)"),
        "natural" => final_prompt.push_str(", (natural lighting, realistic, photorealistic)"),
        _ => {}
    }

    let upstream = state.upstream.clone();
    let token_manager = state.token_manager.clone();
    let max_pool_size = token_manager.len();
    let max_attempts = MAX_RETRY_ATTEMPTS
        .min(max_pool_size.saturating_add(1))
        .max(2);

    let mut tasks = Vec::new();

    for _ in 0..n {
        let upstream = upstream.clone();
        let token_manager = token_manager.clone();
        let final_prompt = final_prompt.clone();
        let image_config = image_config.clone();
        let response_format = response_format.to_string();

        tasks.push(tokio::spawn(async move {
            let mut last_error = String::new();

            for attempt in 0..max_attempts {
                let token = match token_manager
                    .get_token("gemini-3-pro-image", None)
                    .await
                {
                    Ok(t) => t,
                    Err(e) => {
                        last_error = format!("Token error: {}", e);
                        if attempt < max_attempts - 1 {
                            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                            continue;
                        }
                        break;
                    }
                };

                let project_id = token.project_id.clone().unwrap_or_default();
                let gemini_body = json!({
                    "project": project_id,
                    "requestId": format!("img-{}", uuid::Uuid::new_v4()),
                    "model": "gemini-3-pro-image",
                    "userAgent": "kiro-ai-gateway",
                    "requestType": "image_gen",
                    "request": {
                        "contents": [{
                            "role": "user",
                            "parts": [{"text": final_prompt}]
                        }],
                        "generationConfig": {
                            "candidateCount": 1,
                            "imageConfig": image_config
                        },
                        "safetySettings": [
                            { "category": "HARM_CATEGORY_HARASSMENT", "threshold": "OFF" },
                            { "category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "OFF" },
                            { "category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "threshold": "OFF" },
                            { "category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "OFF" },
                            { "category": "HARM_CATEGORY_CIVIC_INTEGRITY", "threshold": "OFF" },
                        ]
                    }
                });

                match upstream
                    .call_v1_internal(
                        "generateContent",
                        &token.access_token,
                        gemini_body,
                        None,
                        Some(token.account_id.as_str()),
                    )
                    .await
                {
                    Ok(call_result) => {
                        let response = call_result.response;
                        let status = response.status();
                        if !status.is_success() {
                            let status_code = status.as_u16();
                            let err_text = response.text().await.unwrap_or_default();
                            last_error = format!("Upstream error {}: {}", status_code, err_text);

                            // 429/500/503 errors: mark rate limited and retry with rotation
                            if status_code == 429 || status_code == 503 || status_code == 500 {
                                tracing::warn!(
                                    "[Images] Account {} rate limited/error ({}), rotating...",
                                    token.email,
                                    status_code
                                );
                                continue;
                            }
                            // Other errors: return immediately
                            return Err(last_error);
                        }
                        match response.json::<Value>().await {
                            Ok(json) => {
                                token_manager.mark_success(&token.account_id);
                                return Ok((json, response_format.clone(), token.email));
                            }
                            Err(e) => return Err(format!("Parse error: {}", e)),
                        }
                    }
                    Err(e) => {
                        last_error = format!("Network error: {}", e);
                        continue;
                    }
                }
            }
            Err(format!("Max retries exhausted. Last error: {}", last_error))
        }));
    }

    // Collect results
    let mut images: Vec<Value> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut used_email: Option<String> = None;

    for (idx, task) in tasks.into_iter().enumerate() {
        match task.await {
            Ok(Ok((gemini_resp, resp_fmt, email))) => {
                if used_email.is_none() {
                    used_email = Some(email);
                }
                let raw = gemini_resp.get("response").unwrap_or(&gemini_resp);
                if let Some(parts) = raw
                    .get("candidates")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("content"))
                    .and_then(|c| c.get("parts"))
                    .and_then(|p| p.as_array())
                {
                    for part in parts {
                        if let Some(img) = part.get("inlineData") {
                            let data = img.get("data").and_then(|v| v.as_str()).unwrap_or("");
                            if !data.is_empty() {
                                if resp_fmt == "url" {
                                    let mime = img
                                        .get("mimeType")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("image/png");
                                    images.push(json!({"url": format!("data:{};base64,{}", mime, data)}));
                                } else {
                                    images.push(json!({"b64_json": data}));
                                }
                            }
                        }
                    }
                }
            }
            Ok(Err(e)) => {
                error!("[Images] Task {} failed: {}", idx, e);
                errors.push(e);
            }
            Err(e) => {
                error!("[Images] Task {} join error: {}", idx, e);
                errors.push(format!("Task join error: {}", e));
            }
        }
    }

    if images.is_empty() {
        let error_msg = if !errors.is_empty() {
            errors.join("; ")
        } else {
            "No images generated".to_string()
        };
        error!("[Images] All {} requests failed. Errors: {}", n, error_msg);

        // Map upstream status codes correctly
        let status = if error_msg.contains("429") || error_msg.contains("Quota exhausted") {
            StatusCode::TOO_MANY_REQUESTS
        } else if error_msg.contains("503") || error_msg.contains("Service Unavailable") {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::BAD_GATEWAY
        };

        return Err((status, error_msg));
    }

    if !errors.is_empty() {
        tracing::warn!(
            "[Images] Partial success: {} out of {} requests succeeded",
            images.len(),
            n
        );
    }

    info!(
        "[Images] Successfully generated {} out of {} requested image(s)",
        images.len(),
        n
    );

    let openai_response = json!({
        "created": chrono::Utc::now().timestamp(),
        "data": images
    });

    let email_header = used_email.unwrap_or_default();
    Ok((
        StatusCode::OK,
        [
            ("X-Mapped-Model", "dall-e-3"),
            ("X-Account-Email", email_header.as_str()),
        ],
        Json(openai_response),
    )
        .into_response())
}

/// Handle OpenAI Images Edits: POST /v1/images/edits [Req 2.14, 7.4]
///
/// Supports:
/// - Standard image editing (image + prompt)
/// - Image-to-image generation with reference images (image1, image2, etc.)
/// - Mask-based editing (image + mask + prompt)
pub async fn handle_images_edits(
    State(state): State<AppState>,
    mut multipart: axum::extract::Multipart,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    info!("[Images] Received edit request");

    let mut image_data: Option<String> = None;
    let mut mask_data: Option<String> = None;
    let mut reference_images: Vec<String> = Vec::new();
    let mut prompt = String::new();
    let mut n: usize = 1;
    let mut size = "1024x1024".to_string();
    let mut response_format = "b64_json".to_string();
    let mut model = "gemini-3-pro-image".to_string();
    let mut aspect_ratio: Option<String> = None;
    let mut image_size_param: Option<String> = None;
    let mut style: Option<String> = None;

    // Parse multipart form data
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Multipart error: {}", e)))?
    {
        let name = field.name().unwrap_or("").to_string();

        if name == "image" {
            let data = field
                .bytes()
                .await
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("Image read error: {}", e)))?;
            image_data = Some(base64::engine::general_purpose::STANDARD.encode(data));
        } else if name == "mask" {
            let data = field
                .bytes()
                .await
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("Mask read error: {}", e)))?;
            mask_data = Some(base64::engine::general_purpose::STANDARD.encode(data));
        } else if name.starts_with("image") && name != "image_size" {
            // Support image1, image2, etc. as reference images
            let data = field.bytes().await.map_err(|e| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("Reference image read error: {}", e),
                )
            })?;
            reference_images.push(base64::engine::general_purpose::STANDARD.encode(data));
        } else if name == "prompt" {
            prompt = field
                .text()
                .await
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("Prompt read error: {}", e)))?;
        } else if name == "n" {
            if let Ok(val) = field.text().await {
                n = val.parse().unwrap_or(1);
            }
        } else if name == "size" {
            if let Ok(val) = field.text().await {
                size = val;
            }
        } else if name == "image_size" {
            if let Ok(val) = field.text().await {
                image_size_param = Some(val);
            }
        } else if name == "aspect_ratio" {
            if let Ok(val) = field.text().await {
                aspect_ratio = Some(val);
            }
        } else if name == "style" {
            if let Ok(val) = field.text().await {
                style = Some(val);
            }
        } else if name == "response_format" {
            if let Ok(val) = field.text().await {
                response_format = val;
            }
        } else if name == "model" {
            if let Ok(val) = field.text().await {
                if !val.is_empty() {
                    model = val;
                }
            }
        }
    }

    // Validation: require prompt
    if prompt.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Missing prompt".to_string()));
    }

    info!(
        "[Images] Edit request: model={}, prompt={:.50}, n={}, size={}, refs={}, has_main_image={}",
        model,
        prompt,
        n,
        size,
        reference_images.len(),
        image_data.is_some()
    );

    // Parse image config (aspect_ratio param > size param)
    let size_input = aspect_ratio.as_deref().or(Some(&size));
    let quality_input = match image_size_param.as_deref() {
        Some("4K") => Some("hd"),
        Some("2K") => Some("medium"),
        _ => None,
    };

    let (image_config, _) = crate::proxy::common::common_utils::parse_image_config_with_params(
        &model,
        size_input,
        quality_input,
        image_size_param.as_deref(),
    );

    // Build content parts
    let mut contents_parts = Vec::new();

    // Add prompt with optional style
    let mut final_prompt = prompt.clone();
    if let Some(s) = &style {
        final_prompt.push_str(&format!(", style: {}", s));
    }
    contents_parts.push(json!({ "text": final_prompt }));

    // Add main image (standard edit)
    if let Some(data) = &image_data {
        contents_parts.push(json!({
            "inlineData": { "mimeType": "image/png", "data": data }
        }));
    }

    // Add mask (standard edit)
    if let Some(data) = &mask_data {
        contents_parts.push(json!({
            "inlineData": { "mimeType": "image/png", "data": data }
        }));
    }

    // Add reference images (image-to-image)
    for ref_data in &reference_images {
        contents_parts.push(json!({
            "inlineData": { "mimeType": "image/jpeg", "data": ref_data }
        }));
    }

    // Concurrent request execution with retry
    let upstream = state.upstream.clone();
    let token_manager = state.token_manager.clone();
    let max_pool_size = token_manager.len();
    let max_attempts = MAX_RETRY_ATTEMPTS
        .min(max_pool_size.saturating_add(1))
        .max(2);

    let mut tasks = Vec::new();
    for _ in 0..n {
        let upstream = upstream.clone();
        let token_manager = token_manager.clone();
        let contents_parts = contents_parts.clone();
        let image_config = image_config.clone();
        let response_format = response_format.clone();
        let model = model.clone();

        tasks.push(tokio::spawn(async move {
            let mut last_error = String::new();

            for attempt in 0..max_attempts {
                let token = match token_manager
                    .get_token("gemini-3-pro-image", None)
                    .await
                {
                    Ok(t) => t,
                    Err(e) => {
                        last_error = format!("Token error: {}", e);
                        if attempt < max_attempts - 1 {
                            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                            continue;
                        }
                        break;
                    }
                };

                let project_id = token.project_id.clone().unwrap_or_default();
                let gemini_body = json!({
                    "project": project_id,
                    "requestId": format!("img-edit-{}", uuid::Uuid::new_v4()),
                    "model": model,
                    "userAgent": "kiro-ai-gateway",
                    "requestType": "image_gen",
                    "request": {
                        "contents": [{
                            "role": "user",
                            "parts": contents_parts
                        }],
                        "generationConfig": {
                            "candidateCount": 1,
                            "imageConfig": image_config,
                            "maxOutputTokens": 8192,
                            "temperature": 1.0,
                            "topP": 0.95,
                            "topK": 40
                        },
                        "safetySettings": [
                            { "category": "HARM_CATEGORY_HARASSMENT", "threshold": "OFF" },
                            { "category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "OFF" },
                            { "category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "threshold": "OFF" },
                            { "category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "OFF" },
                            { "category": "HARM_CATEGORY_CIVIC_INTEGRITY", "threshold": "OFF" },
                        ]
                    }
                });

                match upstream
                    .call_v1_internal(
                        "generateContent",
                        &token.access_token,
                        gemini_body,
                        None,
                        Some(token.account_id.as_str()),
                    )
                    .await
                {
                    Ok(call_result) => {
                        let response = call_result.response;
                        let status = response.status();
                        if !status.is_success() {
                            let status_code = status.as_u16();
                            let err_text = response.text().await.unwrap_or_default();
                            last_error = format!("Upstream error {}: {}", status_code, err_text);

                            if status_code == 429 || status_code == 503 || status_code == 500 {
                                tracing::warn!(
                                    "[Images] Account {} rate limited/error ({}), rotating...",
                                    token.email,
                                    status_code
                                );
                                continue;
                            }
                            return Err(last_error);
                        }
                        match response.json::<Value>().await {
                            Ok(json) => {
                                token_manager.mark_success(&token.account_id);
                                return Ok((json, response_format.clone(), token.email));
                            }
                            Err(e) => return Err(format!("Parse error: {}", e)),
                        }
                    }
                    Err(e) => {
                        last_error = format!("Network error: {}", e);
                        continue;
                    }
                }
            }
            Err(format!("Max retries exhausted. Last error: {}", last_error))
        }));
    }

    // Collect results
    let mut images: Vec<Value> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut used_email: Option<String> = None;

    for (idx, task) in tasks.into_iter().enumerate() {
        match task.await {
            Ok(Ok((gemini_resp, resp_fmt, email))) => {
                if used_email.is_none() {
                    used_email = Some(email);
                }
                let raw = gemini_resp.get("response").unwrap_or(&gemini_resp);
                if let Some(parts) = raw
                    .get("candidates")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("content"))
                    .and_then(|c| c.get("parts"))
                    .and_then(|p| p.as_array())
                {
                    for part in parts {
                        if let Some(img) = part.get("inlineData") {
                            let data = img.get("data").and_then(|v| v.as_str()).unwrap_or("");
                            if !data.is_empty() {
                                if resp_fmt == "url" {
                                    let mime = img
                                        .get("mimeType")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("image/png");
                                    images.push(json!({"url": format!("data:{};base64,{}", mime, data)}));
                                } else {
                                    images.push(json!({"b64_json": data}));
                                }
                            }
                        }
                    }
                }
            }
            Ok(Err(e)) => {
                error!("[Images] Edit task {} failed: {}", idx, e);
                errors.push(e);
            }
            Err(e) => {
                error!("[Images] Edit task {} join error: {}", idx, e);
                errors.push(format!("Task join error: {}", e));
            }
        }
    }

    if images.is_empty() {
        let error_msg = if !errors.is_empty() {
            errors.join("; ")
        } else {
            "No images generated".to_string()
        };
        error!("[Images] All {} edit requests failed. Errors: {}", n, error_msg);
        return Err((StatusCode::BAD_GATEWAY, error_msg));
    }

    if !errors.is_empty() {
        tracing::warn!(
            "[Images] Partial success: {} out of {} edit requests succeeded",
            images.len(),
            n
        );
    }

    info!(
        "[Images] Successfully generated {} out of {} requested edited image(s)",
        images.len(),
        n
    );

    let openai_response = json!({
        "created": chrono::Utc::now().timestamp(),
        "data": images
    });

    let email_header = used_email.unwrap_or_default();
    Ok((
        StatusCode::OK,
        [
            ("X-Mapped-Model", "dall-e-3"),
            ("X-Account-Email", email_header.as_str()),
        ],
        Json(openai_response),
    )
        .into_response())
}


/// Handle Legacy Completions: POST /v1/completions [Req 2.13]
///
/// Converts prompt-based requests to chat format and delegates to chat completions logic.
pub async fn handle_completions(
    State(state): State<AppState>,
    Json(mut body): Json<Value>,
) -> Response {
    // Convert prompt to messages format
    if let Some(prompt_val) = body.get("prompt").cloned() {
        let prompt_str = match &prompt_val {
            Value::String(s) => s.clone(),
            Value::Array(arr) => arr
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => prompt_val.to_string(),
        };
        let messages = json!([{"role": "user", "content": prompt_str}]);
        if let Some(obj) = body.as_object_mut() {
            obj.remove("prompt");
            obj.insert("messages".to_string(), messages);
        }
    }

    // Also handle Codex-style input/instructions
    if body.get("input").is_some() || body.get("instructions").is_some() {
        let mut messages = Vec::new();

        if let Some(inst) = body.get("instructions").and_then(|v| v.as_str()) {
            if !inst.is_empty() {
                messages.push(json!({"role": "system", "content": inst}));
            }
        }

        if let Some(input) = body.get("input") {
            if let Some(s) = input.as_str() {
                messages.push(json!({"role": "user", "content": s}));
            } else {
                messages.push(json!({"role": "user", "content": input.to_string()}));
            }
        }

        if !messages.is_empty() {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("messages".to_string(), json!(messages));
            }
        }
    }

    let openai_req: OpenAIRequest = match serde_json::from_value(body) {
        Ok(req) => req,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("Invalid request: {}", e)).into_response();
        }
    };

    let upstream = state.upstream.clone();
    let token_manager = state.token_manager.clone();
    let pool_size = token_manager.len();
    let max_attempts = MAX_RETRY_ATTEMPTS.min(pool_size.saturating_add(1)).max(2);

    let mapped_model = crate::proxy::common::model_mapping::map_model(
        &openai_req.model,
        &*state.custom_mapping.read().await,
        false,
    );

    let mut last_error = String::new();

    for attempt in 0..max_attempts {
        let session_id =
            SessionManager::extract_openai_session_id(&serde_json::to_value(&openai_req).unwrap());

        let token = match token_manager
            .get_token(&mapped_model, Some(&session_id))
            .await
        {
            Ok(t) => t,
            Err(e) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("Token error: {}", e),
                )
                    .into_response();
            }
        };

        let project_id = token.project_id.clone().unwrap_or_default();
        let (gemini_body, _session_id, message_count) =
            transform_openai_request(&openai_req, &project_id, &mapped_model);

        // Always use stream internally for better quota usage
        let call_result = match upstream
            .call_v1_internal(
                "streamGenerateContent",
                &token.access_token,
                gemini_body,
                Some("alt=sse"),
                Some(token.account_id.as_str()),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_error = e;
                continue;
            }
        };

        let response = call_result.response;
        let status = response.status();

        if status.is_success() {
            token_manager.mark_success(&token.account_id);

            // Collect stream and convert to legacy format
            use crate::proxy::mappers::openai::collector::collect_stream_to_json;
            use crate::proxy::mappers::openai::streaming::create_openai_sse_stream;

            let openai_stream = create_openai_sse_stream(
                Box::pin(response.bytes_stream()),
                openai_req.model.clone(),
                _session_id,
                message_count,
            );

            match collect_stream_to_json(Box::pin(openai_stream)).await {
                Ok(chat_resp) => {
                    let choices: Vec<Value> = chat_resp
                        .choices
                        .iter()
                        .map(|c| {
                            json!({
                                "text": match &c.message.content {
                                    Some(crate::proxy::mappers::openai::OpenAIContent::String(s)) => s.clone(),
                                    _ => "".to_string()
                                },
                                "index": c.index,
                                "logprobs": null,
                                "finish_reason": c.finish_reason
                            })
                        })
                        .collect();

                    let legacy_resp = json!({
                        "id": chat_resp.id,
                        "object": "text_completion",
                        "created": chat_resp.created,
                        "model": chat_resp.model,
                        "choices": choices,
                        "usage": chat_resp.usage
                    });

                    return (
                        StatusCode::OK,
                        [
                            ("X-Account-Email", token.email.as_str()),
                            ("X-Mapped-Model", mapped_model.as_str()),
                        ],
                        Json(legacy_resp),
                    )
                        .into_response();
                }
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Stream collection error: {}", e),
                    )
                        .into_response();
                }
            }
        }

        let status_code = status.as_u16();
        let error_text = response.text().await.unwrap_or_default();
        last_error = format!("HTTP {}: {}", status_code, error_text);

        let strategy = determine_retry_strategy(status_code, &error_text, false);
        let trace_id = format!("completions_{}", attempt);
        if !apply_retry_strategy(strategy, attempt, max_attempts, status_code, &trace_id).await {
            break;
        }
    }

    (
        StatusCode::TOO_MANY_REQUESTS,
        format!("All accounts exhausted. Last error: {}", last_error),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_handle_list_models_returns_json() {
        // Verify the model list format is correct
        let models = crate::proxy::common::model_mapping::get_supported_models();
        assert!(!models.is_empty());
    }

    #[test]
    fn test_image_response_extraction_b64() {
        // Test extracting base64 image data from Gemini response format
        let gemini_resp = serde_json::json!({
            "response": {
                "candidates": [{
                    "content": {
                        "parts": [{
                            "inlineData": {
                                "mimeType": "image/png",
                                "data": "iVBORw0KGgoAAAANSUhEUg=="
                            }
                        }]
                    }
                }]
            }
        });

        let raw = gemini_resp.get("response").unwrap();
        let parts = raw
            .get("candidates")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .unwrap();

        assert_eq!(parts.len(), 1);
        let img = parts[0].get("inlineData").unwrap();
        let data = img.get("data").and_then(|v| v.as_str()).unwrap();
        assert_eq!(data, "iVBORw0KGgoAAAANSUhEUg==");
    }

    #[test]
    fn test_image_response_extraction_url_format() {
        // Test URL format response construction
        let mime = "image/png";
        let data = "iVBORw0KGgoAAAANSUhEUg==";
        let url = format!("data:{};base64,{}", mime, data);
        assert!(url.starts_with("data:image/png;base64,"));
        assert!(url.contains(data));
    }

    #[test]
    fn test_image_response_empty_candidates() {
        // Test handling when no image data is returned
        let gemini_resp = serde_json::json!({
            "response": {
                "candidates": [{
                    "content": {
                        "parts": [{ "text": "I cannot generate that image" }]
                    }
                }]
            }
        });

        let raw = gemini_resp.get("response").unwrap();
        let parts = raw
            .get("candidates")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .unwrap();

        // No inlineData parts should be found
        let image_count = parts
            .iter()
            .filter(|p| p.get("inlineData").is_some())
            .count();
        assert_eq!(image_count, 0);
    }

    #[test]
    fn test_image_config_parsing_integration() {
        // Test that parse_image_config_with_params works correctly for handler use
        let (config, _) = crate::proxy::common::common_utils::parse_image_config_with_params(
            "gemini-3-pro-image",
            Some("1920x1080"),
            Some("hd"),
            None,
        );
        assert_eq!(config["aspectRatio"], "16:9");
        assert_eq!(config["imageSize"], "4K");
    }

    #[test]
    fn test_image_config_with_direct_image_size() {
        // Test direct imageSize parameter takes priority
        let (config, _) = crate::proxy::common::common_utils::parse_image_config_with_params(
            "gemini-3-pro-image",
            Some("1024x1024"),
            Some("standard"),
            Some("4K"),
        );
        assert_eq!(config["aspectRatio"], "1:1");
        assert_eq!(config["imageSize"], "4K"); // Direct imageSize wins over quality
    }

    #[test]
    fn test_image_edit_quality_to_image_size_mapping() {
        // Test the quality-to-imageSize mapping used in image edits
        let quality_mappings = vec![
            ("4K", Some("hd")),
            ("2K", Some("medium")),
            ("1K", None), // No mapping for 1K
        ];

        for (image_size, expected_quality) in quality_mappings {
            let quality_input = match image_size {
                "4K" => Some("hd"),
                "2K" => Some("medium"),
                _ => None,
            };
            assert_eq!(quality_input, expected_quality);
        }
    }
}
