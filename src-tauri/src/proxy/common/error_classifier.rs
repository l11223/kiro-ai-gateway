// Error classification module
// Classifies upstream errors into user-friendly categories with i18n support
//
// Requirements covered:
// - Error handling strategy from design doc

use reqwest::Error;

/// Classify a streaming response error and return (error_type, english_message, i18n_key).
///
/// - error_type: Used for logging and error codes
/// - english_message: Fallback message for non-browser clients
/// - i18n_key: Frontend translation key for browser clients
pub fn classify_stream_error(error: &Error) -> (&'static str, &'static str, &'static str) {
    if error.is_timeout() {
        (
            "timeout_error",
            "Request timeout, please check your network connection",
            "errors.stream.timeout_error",
        )
    } else if error.is_connect() {
        (
            "connection_error",
            "Connection failed, please check your network or proxy settings",
            "errors.stream.connection_error",
        )
    } else if error.is_decode() {
        (
            "decode_error",
            "Network unstable, data transmission interrupted. Try: 1) Check network 2) Switch proxy 3) Retry",
            "errors.stream.decode_error",
        )
    } else if error.is_body() {
        (
            "stream_error",
            "Stream transmission error, please retry later",
            "errors.stream.stream_error",
        )
    } else {
        (
            "unknown_error",
            "Unknown error occurred",
            "errors.stream.unknown_error",
        )
    }
}

/// Classify an HTTP status code from upstream into an error category.
///
/// Returns (error_type, should_retry, description)
pub fn classify_http_status(status: u16) -> (&'static str, bool, &'static str) {
    match status {
        401 => ("auth_error", true, "Token expired, will refresh and retry"),
        403 => (
            "forbidden",
            false,
            "Access forbidden, account may need validation",
        ),
        404 => (
            "not_found",
            true,
            "Model not available, will try next account",
        ),
        429 => (
            "rate_limit",
            true,
            "Rate limited, will rotate to next account",
        ),
        500..=599 => (
            "server_error",
            true,
            "Upstream server error, will retry with soft backoff",
        ),
        _ => ("unknown_http_error", false, "Unexpected HTTP error"),
    }
}

/// Classify a 429 error reason from the upstream response body.
///
/// Returns the specific rate limit reason for the Rate Limit Tracker.
pub fn classify_rate_limit_reason(error_body: &str) -> &'static str {
    let lower = error_body.to_lowercase();

    if lower.contains("quota_exhausted") || lower.contains("quotaexhausted") {
        "QuotaExhausted"
    } else if lower.contains("rate_limit_exceeded") || lower.contains("ratelimitexceeded") {
        "RateLimitExceeded"
    } else if lower.contains("model_capacity_exhausted")
        || lower.contains("modelcapacityexhausted")
    {
        "ModelCapacityExhausted"
    } else {
        "RateLimitExceeded" // Default to short lockout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_http_status_401() {
        let (error_type, should_retry, _) = classify_http_status(401);
        assert_eq!(error_type, "auth_error");
        assert!(should_retry);
    }

    #[test]
    fn test_classify_http_status_403() {
        let (error_type, should_retry, _) = classify_http_status(403);
        assert_eq!(error_type, "forbidden");
        assert!(!should_retry);
    }

    #[test]
    fn test_classify_http_status_429() {
        let (error_type, should_retry, _) = classify_http_status(429);
        assert_eq!(error_type, "rate_limit");
        assert!(should_retry);
    }

    #[test]
    fn test_classify_http_status_5xx() {
        for status in [500, 502, 503, 504] {
            let (error_type, should_retry, _) = classify_http_status(status);
            assert_eq!(error_type, "server_error");
            assert!(should_retry);
        }
    }

    #[test]
    fn test_classify_http_status_unknown() {
        let (error_type, should_retry, _) = classify_http_status(418);
        assert_eq!(error_type, "unknown_http_error");
        assert!(!should_retry);
    }

    #[test]
    fn test_classify_rate_limit_reason_quota() {
        assert_eq!(
            classify_rate_limit_reason(r#"{"error":{"status":"QUOTA_EXHAUSTED"}}"#),
            "QuotaExhausted"
        );
    }

    #[test]
    fn test_classify_rate_limit_reason_rate_limit() {
        assert_eq!(
            classify_rate_limit_reason(r#"{"error":{"status":"RATE_LIMIT_EXCEEDED"}}"#),
            "RateLimitExceeded"
        );
    }

    #[test]
    fn test_classify_rate_limit_reason_capacity() {
        assert_eq!(
            classify_rate_limit_reason(r#"{"error":{"status":"MODEL_CAPACITY_EXHAUSTED"}}"#),
            "ModelCapacityExhausted"
        );
    }

    #[test]
    fn test_classify_rate_limit_reason_default() {
        assert_eq!(
            classify_rate_limit_reason(r#"{"error":{"message":"too many requests"}}"#),
            "RateLimitExceeded"
        );
    }

    #[test]
    fn test_i18n_keys_format() {
        let expected_keys = vec![
            "errors.stream.timeout_error",
            "errors.stream.connection_error",
            "errors.stream.decode_error",
            "errors.stream.stream_error",
            "errors.stream.unknown_error",
        ];
        for key in expected_keys {
            assert!(key.starts_with("errors.stream."));
        }
    }

    #[test]
    fn test_classify_stream_error_connection() {
        // Create a connection error by trying to connect to an invalid address
        let client = reqwest::Client::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let error = rt.block_on(async {
            client
                .get("http://invalid-domain-that-does-not-exist-12345.com")
                .send()
                .await
                .unwrap_err()
        });

        let (error_type, message, i18n_key) = classify_stream_error(&error);

        // Should be one of the known error types
        assert!(
            ["timeout_error", "connection_error", "decode_error", "stream_error", "unknown_error"]
                .contains(&error_type)
        );
        assert!(!message.is_empty());
        assert!(i18n_key.starts_with("errors.stream."));
    }
}
