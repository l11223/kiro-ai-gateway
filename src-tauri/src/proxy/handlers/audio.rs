// Audio Handler - /v1/audio/transcriptions
//
// Requirements covered:
// - 2.11: POST /v1/audio/transcriptions → Gemini audio transcription
// - 7.3: Audio file forwarded to Gemini transcription API

use axum::{
    extract::{Multipart, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};
use tracing::info;

use crate::proxy::audio::AudioProcessor;

use super::AppState;

/// Handle audio transcription: POST /v1/audio/transcriptions [Req 2.11, 7.3]
pub async fn handle_audio_transcription(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let mut audio_data: Option<Vec<u8>> = None;
    let mut filename: Option<String> = None;
    let mut model = "gemini-2.0-flash-exp".to_string();
    let mut prompt = "Generate a transcript of the speech.".to_string();

    // Parse multipart/form-data
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Parse form error: {}", e)))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                filename = field.file_name().map(|s| s.to_string());
                audio_data = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Read file error: {}", e)))?
                        .to_vec(),
                );
            }
            "model" => {
                model = field.text().await.unwrap_or(model);
            }
            "prompt" => {
                prompt = field.text().await.unwrap_or(prompt);
            }
            _ => {}
        }
    }

    let audio_bytes =
        audio_data.ok_or((StatusCode::BAD_REQUEST, "Missing audio file".to_string()))?;
    let file_name =
        filename.ok_or((StatusCode::BAD_REQUEST, "Cannot get filename".to_string()))?;

    info!(
        "Audio transcription: file={}, size={} bytes, model={}",
        file_name,
        audio_bytes.len(),
        model
    );

    // Validate and prepare audio data using AudioProcessor
    let inline_data = AudioProcessor::prepare_inline_data(&file_name, &audio_bytes)
        .map_err(|e| {
            if e.contains("too large") {
                (StatusCode::PAYLOAD_TOO_LARGE, e)
            } else {
                (StatusCode::BAD_REQUEST, e)
            }
        })?;

    // Build Gemini request with inline audio data
    let gemini_request = json!({
        "contents": [{
            "parts": [
                {"text": prompt},
                {
                    "inlineData": {
                        "mimeType": inline_data.mime_type,
                        "data": inline_data.data
                    }
                }
            ]
        }]
    });

    // Get token
    let token = state
        .token_manager
        .get_token("text", None)
        .await
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, e))?;

    let project_id = token.project_id.clone().unwrap_or_default();
    info!("Using account: {}", token.email);

    // Wrap request for v1internal format
    let wrapped_body = json!({
        "project": project_id,
        "requestId": format!("audio-{}", uuid::Uuid::new_v4()),
        "request": gemini_request,
        "model": model,
        "userAgent": "kiro-ai-gateway",
        "requestType": "text"
    });

    // Send to Gemini
    let response = state
        .upstream
        .call_v1_internal(
            "generateContent",
            &token.access_token,
            wrapped_body,
            None,
            Some(token.account_id.as_str()),
        )
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Upstream error: {}", e)))?
        .response;

    if !response.status().is_success() {
        let error_text = response.text().await.unwrap_or_default();
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("Gemini API error: {}", error_text),
        ));
    }

    let result: Value = response
        .json()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Parse response error: {}", e)))?;

    // Extract text from response (unwrap v1internal)
    let inner_response = result.get("response").unwrap_or(&result);
    let text = inner_response
        .get("candidates")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.get(0))
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("");

    info!("Audio transcription complete, {} chars", text.len());

    Ok((
        StatusCode::OK,
        [("X-Account-Email", token.email.as_str())],
        Json(json!({"text": text})),
    )
        .into_response())
}
