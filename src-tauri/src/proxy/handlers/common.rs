// Common handler utilities - Retry strategies and shared logic
//
// Provides unified retry/backoff strategies and model detection for all handlers.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use tokio::time::{sleep, Duration};
use tracing::{debug, info};

use super::AppState;

// ===== Unified Retry & Backoff Strategy =====

/// Retry strategy enum
#[derive(Debug, Clone)]
pub enum RetryStrategy {
    /// No retry, return error immediately
    NoRetry,
    /// Fixed delay
    FixedDelay(Duration),
    /// Linear backoff: base_ms * (attempt + 1)
    LinearBackoff { base_ms: u64 },
    /// Exponential backoff: base_ms * 2^attempt, capped at max_ms
    ExponentialBackoff { base_ms: u64, max_ms: u64 },
}

/// Determine retry strategy based on error status code and error text
pub fn determine_retry_strategy(
    status_code: u16,
    error_text: &str,
    retried_without_thinking: bool,
) -> RetryStrategy {
    match status_code {
        // 400: Only retry on specific Thinking signature failures
        400 if !retried_without_thinking
            && (error_text.contains("Invalid `signature`")
                || error_text.contains("thinking.signature")
                || error_text.contains("thinking.thinking")
                || error_text.contains("Corrupted thought signature")) =>
        {
            RetryStrategy::FixedDelay(Duration::from_millis(200))
        }

        // 429: Rate limit
        429 => {
            if let Some(delay_ms) = crate::proxy::upstream::retry::parse_retry_delay(error_text) {
                let actual_delay = delay_ms.saturating_add(200).min(30_000);
                RetryStrategy::FixedDelay(Duration::from_millis(actual_delay))
            } else {
                RetryStrategy::LinearBackoff { base_ms: 5000 }
            }
        }

        // 503/529: Service unavailable / overloaded
        503 | 529 => RetryStrategy::ExponentialBackoff {
            base_ms: 10000,
            max_ms: 60000,
        },

        // 500: Internal server error
        500 => RetryStrategy::LinearBackoff { base_ms: 3000 },

        // 401/403: Auth/permission errors - brief buffer before account rotation
        401 | 403 => RetryStrategy::FixedDelay(Duration::from_millis(200)),

        // 404: Intermittent account-level issues
        404 => RetryStrategy::FixedDelay(Duration::from_millis(300)),

        // Other errors: no retry
        _ => RetryStrategy::NoRetry,
    }
}

/// Execute backoff strategy and return whether to continue retrying
pub async fn apply_retry_strategy(
    strategy: RetryStrategy,
    attempt: usize,
    max_attempts: usize,
    status_code: u16,
    trace_id: &str,
) -> bool {
    match strategy {
        RetryStrategy::NoRetry => {
            debug!(
                "[{}] Non-retryable error {}, stopping",
                trace_id, status_code
            );
            false
        }
        RetryStrategy::FixedDelay(duration) => {
            info!(
                "[{}] Retry with fixed delay: status={}, attempt={}/{}, delay={}ms",
                trace_id,
                status_code,
                attempt + 1,
                max_attempts,
                duration.as_millis()
            );
            sleep(duration).await;
            true
        }
        RetryStrategy::LinearBackoff { base_ms } => {
            let calculated_ms = base_ms * (attempt as u64 + 1);
            info!(
                "[{}] Retry with linear backoff: status={}, attempt={}/{}, delay={}ms",
                trace_id,
                status_code,
                attempt + 1,
                max_attempts,
                calculated_ms
            );
            sleep(Duration::from_millis(calculated_ms)).await;
            true
        }
        RetryStrategy::ExponentialBackoff { base_ms, max_ms } => {
            let calculated_ms = (base_ms * 2_u64.pow(attempt as u32)).min(max_ms);
            info!(
                "[{}] Retry with exponential backoff: status={}, attempt={}/{}, delay={}ms",
                trace_id,
                status_code,
                attempt + 1,
                max_attempts,
                calculated_ms
            );
            sleep(Duration::from_millis(calculated_ms)).await;
            true
        }
    }
}

/// Determine whether to rotate to a different account
pub fn should_rotate_account(status_code: u16) -> bool {
    matches!(status_code, 429 | 401 | 403 | 404 | 500)
}

/// Detect model capabilities and configuration
/// POST /v1/models/detect
pub async fn handle_detect_model(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let model_name = body.get("model").and_then(|v| v.as_str()).unwrap_or("");

    if model_name.is_empty() {
        return (StatusCode::BAD_REQUEST, "Missing 'model' field").into_response();
    }

    // 1. Resolve mapping
    let mapped_model = crate::proxy::common::model_mapping::map_model(
        model_name,
        &*state.custom_mapping.read().await,
        false,
    );

    // 2. Resolve capabilities
    let config = crate::proxy::common::common_utils::resolve_request_config(
        model_name,
        &mapped_model,
        &None,
        None,
        None,
        None,
        None,
    );

    // 3. Construct response
    let response = json!({
        "model": model_name,
        "mapped_model": mapped_model,
        "type": config.request_type,
        "features": {
            "has_web_search": config.inject_google_search,
            "is_image_gen": config.request_type == "image_gen"
        }
    });

    Json(response).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_determine_retry_strategy_429() {
        let strategy = determine_retry_strategy(429, "rate limited", false);
        match strategy {
            RetryStrategy::LinearBackoff { base_ms } => assert_eq!(base_ms, 5000),
            _ => panic!("Expected LinearBackoff for 429"),
        }
    }

    #[test]
    fn test_determine_retry_strategy_503() {
        let strategy = determine_retry_strategy(503, "service unavailable", false);
        match strategy {
            RetryStrategy::ExponentialBackoff { base_ms, max_ms } => {
                assert_eq!(base_ms, 10000);
                assert_eq!(max_ms, 60000);
            }
            _ => panic!("Expected ExponentialBackoff for 503"),
        }
    }

    #[test]
    fn test_determine_retry_strategy_400_signature() {
        let strategy =
            determine_retry_strategy(400, "Invalid `signature` in request", false);
        match strategy {
            RetryStrategy::FixedDelay(d) => assert_eq!(d.as_millis(), 200),
            _ => panic!("Expected FixedDelay for 400 signature error"),
        }
    }

    #[test]
    fn test_determine_retry_strategy_400_normal() {
        let strategy = determine_retry_strategy(400, "bad request", false);
        assert!(matches!(strategy, RetryStrategy::NoRetry));
    }

    #[test]
    fn test_determine_retry_strategy_already_retried() {
        let strategy =
            determine_retry_strategy(400, "Invalid `signature`", true);
        assert!(matches!(strategy, RetryStrategy::NoRetry));
    }

    #[test]
    fn test_should_rotate_account() {
        assert!(should_rotate_account(429));
        assert!(should_rotate_account(401));
        assert!(should_rotate_account(403));
        assert!(should_rotate_account(404));
        assert!(should_rotate_account(500));
        assert!(!should_rotate_account(400));
        assert!(!should_rotate_account(503));
        assert!(!should_rotate_account(529));
        assert!(!should_rotate_account(200));
    }
}
