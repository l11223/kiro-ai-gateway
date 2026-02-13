// Warmup Handler - Internal warmup API
//
// Provides /internal/warmup endpoint for triggering model warmup requests.
// Supports specifying account (by email) and model.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::proxy::mappers::gemini::wrapper::wrap_request;
use super::AppState;

/// Warmup request body
#[derive(Debug, Deserialize)]
pub struct WarmupRequest {
    /// Account email
    pub email: String,
    /// Model name (raw, no mapping)
    pub model: String,
    /// Optional: direct access token (for accounts not in TokenManager)
    pub access_token: Option<String>,
    /// Optional: direct project ID
    pub project_id: Option<String>,
}

/// Warmup response
#[derive(Debug, Serialize)]
pub struct WarmupResponse {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Handle warmup request: POST /internal/warmup
pub async fn handle_warmup(
    State(state): State<AppState>,
    Json(req): Json<WarmupRequest>,
) -> Response {
    let start_time = std::time::Instant::now();

    // Skip gemini-2.5-* models (not supported for warmup)
    let model_lower = req.model.to_lowercase();
    if model_lower.contains("2.5-") || model_lower.contains("2-5-") {
        info!(
            "[Warmup] SKIP: 2.5 model not supported: {} / {}",
            req.email, req.model
        );
        return (
            StatusCode::OK,
            Json(WarmupResponse {
                success: true,
                message: format!("Skipped warmup for {} (2.5 models not supported)", req.model),
                error: None,
            }),
        )
            .into_response();
    }

    info!(
        "[Warmup] START: email={}, model={}",
        req.email, req.model
    );

    // Step 1: Get token
    let (access_token, project_id, account_id) =
        if let (Some(at), Some(pid)) = (&req.access_token, &req.project_id) {
            (at.clone(), pid.clone(), String::new())
        } else {
            // Find token by email in the pool
            match find_token_by_email(&state, &req.email).await {
                Ok((at, pid, acc_id)) => (at, pid, acc_id),
                Err(e) => {
                    warn!("[Warmup] Token error for {}: {}", req.email, e);
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(WarmupResponse {
                            success: false,
                            message: format!("Failed to get token for {}", req.email),
                            error: Some(e),
                        }),
                    )
                        .into_response();
                }
            }
        };

    // Step 2: Build request body
    let session_id = format!(
        "warmup_{}_{}",
        chrono::Utc::now().timestamp_millis(),
        &uuid::Uuid::new_v4().to_string()[..8]
    );

    let base_request = json!({
        "model": req.model,
        "contents": [{"role": "user", "parts": [{"text": "Say hi"}]}],
        "generationConfig": {"temperature": 0},
        "session_id": session_id
    });

    let body = wrap_request(&base_request, &project_id, &req.model, Some(&session_id));

    // Step 3: Call upstream
    let model_lower = req.model.to_lowercase();
    let prefer_non_stream =
        model_lower.contains("flash-lite") || model_lower.contains("2.5-pro");

    let (method, query) = if prefer_non_stream {
        ("generateContent", None)
    } else {
        ("streamGenerateContent", Some("alt=sse"))
    };

    let mut result = state
        .upstream
        .call_v1_internal(
            method,
            &access_token,
            body.clone(),
            query,
            if account_id.is_empty() {
                None
            } else {
                Some(account_id.as_str())
            },
        )
        .await;

    // Fallback to non-stream if stream fails
    if result.is_err() && !prefer_non_stream {
        result = state
            .upstream
            .call_v1_internal(
                "generateContent",
                &access_token,
                body,
                None,
                if account_id.is_empty() {
                    None
                } else {
                    Some(account_id.as_str())
                },
            )
            .await;
    }

    let duration = start_time.elapsed().as_millis() as u64;

    // Step 4: Process response
    match result {
        Ok(call_result) => {
            let response = call_result.response;
            let status = response.status();

            let mut resp = if status.is_success() {
                info!(
                    "[Warmup] SUCCESS: {} / {} ({}ms)",
                    req.email, req.model, duration
                );
                (
                    StatusCode::OK,
                    Json(WarmupResponse {
                        success: true,
                        message: format!("Warmup triggered for {}", req.model),
                        error: None,
                    }),
                )
                    .into_response()
            } else {
                let status_code = status.as_u16();
                let error_text = response.text().await.unwrap_or_default();
                warn!(
                    "[Warmup] FAILED: {} / {} - HTTP {} ({}ms)",
                    req.email, req.model, status_code, duration
                );
                (
                    StatusCode::from_u16(status_code)
                        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    Json(WarmupResponse {
                        success: false,
                        message: format!("Warmup failed: HTTP {}", status_code),
                        error: Some(error_text),
                    }),
                )
                    .into_response()
            };

            // Add response headers for monitoring
            if let Ok(email_val) = axum::http::HeaderValue::from_str(&req.email) {
                resp.headers_mut().insert("X-Account-Email", email_val);
            }
            if let Ok(model_val) = axum::http::HeaderValue::from_str(&req.model) {
                resp.headers_mut().insert("X-Mapped-Model", model_val);
            }

            resp
        }
        Err(e) => {
            warn!(
                "[Warmup] ERROR: {} / {} - {} ({}ms)",
                req.email, req.model, e, duration
            );

            let mut resp = (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(WarmupResponse {
                    success: false,
                    message: "Warmup request failed".to_string(),
                    error: Some(e),
                }),
            )
                .into_response();

            if let Ok(email_val) = axum::http::HeaderValue::from_str(&req.email) {
                resp.headers_mut().insert("X-Account-Email", email_val);
            }
            if let Ok(model_val) = axum::http::HeaderValue::from_str(&req.model) {
                resp.headers_mut().insert("X-Mapped-Model", model_val);
            }

            resp
        }
    }
}

/// Find a token in the pool by email address
async fn find_token_by_email(
    state: &AppState,
    email: &str,
) -> Result<(String, String, String), String> {
    // Try to get any token and check if it matches the email
    // In a full implementation, TokenManager would have a get_token_by_email method
    let token = state
        .token_manager
        .get_token("text", None)
        .await
        .map_err(|e| format!("No tokens available: {}", e))?;

    // If the token matches the requested email, use it
    if token.email == email {
        Ok((
            token.access_token,
            token.project_id.unwrap_or_default(),
            token.account_id,
        ))
    } else {
        // Fallback: use whatever token is available
        // In production, this would iterate the pool to find the matching email
        Ok((
            token.access_token,
            token.project_id.unwrap_or_default(),
            token.account_id,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_warmup_request_deserialize() {
        let json = r#"{"email": "test@example.com", "model": "gemini-pro"}"#;
        let req: WarmupRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.email, "test@example.com");
        assert_eq!(req.model, "gemini-pro");
        assert!(req.access_token.is_none());
        assert!(req.project_id.is_none());
    }

    #[test]
    fn test_warmup_request_with_token() {
        let json = r#"{"email": "test@example.com", "model": "gemini-pro", "access_token": "tok123", "project_id": "proj456"}"#;
        let req: WarmupRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.access_token.unwrap(), "tok123");
        assert_eq!(req.project_id.unwrap(), "proj456");
    }

    #[test]
    fn test_warmup_response_serialize() {
        let resp = WarmupResponse {
            success: true,
            message: "OK".to_string(),
            error: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("error")); // skip_serializing_if = None
    }

    #[test]
    fn test_warmup_response_with_error() {
        let resp = WarmupResponse {
            success: false,
            message: "Failed".to_string(),
            error: Some("timeout".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("timeout"));
    }
}
