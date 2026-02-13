// Quota Module - Google API quota fetching, caching, and model warmup
//
// Provides:
// - fetch_quota_with_cache(): Fetch account quota from Google API with project_id caching
// - warmup_model_directly(): Trigger model warmup via local proxy internal API
// - get_valid_token_for_warmup(): Get a valid (auto-refreshed) token for warmup tasks

use reqwest;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::models::quota::QuotaData;
use crate::modules::config;

const QUOTA_API_URL: &str =
    "https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal:fetchAvailableModels";
const CLOUD_CODE_BASE_URL: &str = "https://daily-cloudcode-pa.sandbox.googleapis.com";
const DEFAULT_PROJECT_ID: &str = "bamboo-precept-lgxtn";
const MAX_RETRIES: u32 = 3;
const USER_AGENT: &str = "kiro-ai-gateway/1.0";

// ── API response types ──────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct QuotaResponse {
    models: std::collections::HashMap<String, ModelInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ModelInfo {
    #[serde(rename = "quotaInfo")]
    quota_info: Option<QuotaInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
struct QuotaInfo {
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LoadProjectResponse {
    #[serde(rename = "cloudaicompanionProject")]
    project_id: Option<String>,
    #[serde(rename = "currentTier")]
    current_tier: Option<Tier>,
    #[serde(rename = "paidTier")]
    paid_tier: Option<Tier>,
}

#[derive(Debug, Deserialize)]
struct Tier {
    id: Option<String>,
    #[allow(dead_code)]
    #[serde(rename = "quotaTier")]
    quota_tier: Option<String>,
    #[allow(dead_code)]
    name: Option<String>,
    #[allow(dead_code)]
    slug: Option<String>,
}

// ── HTTP client helpers ─────────────────────────────────────────────

fn create_client(timeout_secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

// ── Project ID & subscription tier ──────────────────────────────────

/// Fetch project ID and subscription tier from Google loadCodeAssist API.
/// Returns (project_id, subscription_tier).
async fn fetch_project_id(
    access_token: &str,
    email: &str,
) -> (Option<String>, Option<String>) {
    let client = create_client(15);
    let meta = json!({"metadata": {"ideType": "ANTIGRAVITY"}});

    let res = client
        .post(format!("{}/v1internal:loadCodeAssist", CLOUD_CODE_BASE_URL))
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {}", access_token))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .json(&meta)
        .send()
        .await;

    match res {
        Ok(response) => {
            if response.status().is_success() {
                if let Ok(data) = response.json::<LoadProjectResponse>().await {
                    let project_id = data.project_id.clone();
                    // Priority: paid_tier.id reflects actual subscription better than current_tier
                    let subscription_tier = data
                        .paid_tier
                        .and_then(|t| t.id)
                        .or_else(|| data.current_tier.and_then(|t| t.id));

                    if let Some(ref tier) = subscription_tier {
                        info!("📊 [{}] Subscription: {}", email, tier);
                    }
                    return (project_id, subscription_tier);
                }
            } else {
                warn!(
                    "⚠️  [{}] loadCodeAssist failed: {}",
                    email,
                    response.status()
                );
            }
        }
        Err(e) => {
            warn!("❌ [{}] loadCodeAssist network error: {}", email, e);
        }
    }

    (None, None)
}

// ── Core quota fetching ─────────────────────────────────────────────

/// Unified entry point for fetching account quota.
/// Delegates to `fetch_quota_with_cache` without a cached project_id.
pub async fn fetch_quota(
    access_token: &str,
    email: &str,
) -> Result<(QuotaData, Option<String>), String> {
    fetch_quota_with_cache(access_token, email, None).await
}

/// Fetch quota with optional cached project_id to skip the loadCodeAssist call.
///
/// Returns `(QuotaData, Option<project_id>)`.
/// - If `cached_project_id` is `Some`, reuses it (saves one API call).
/// - On 403 Forbidden, returns a QuotaData with `is_forbidden = true` immediately.
/// - Retries up to `MAX_RETRIES` on transient errors.
pub async fn fetch_quota_with_cache(
    access_token: &str,
    email: &str,
    cached_project_id: Option<&str>,
) -> Result<(QuotaData, Option<String>), String> {
    // Resolve project_id: reuse cache or fetch fresh
    let (project_id, subscription_tier) = if let Some(pid) = cached_project_id {
        (Some(pid.to_string()), None)
    } else {
        fetch_project_id(access_token, email).await
    };

    let final_project_id = project_id
        .as_deref()
        .unwrap_or(DEFAULT_PROJECT_ID);

    let client = create_client(15);
    let payload = json!({ "project": final_project_id });
    let mut last_error: Option<String> = None;

    for attempt in 1..=MAX_RETRIES {
        match client
            .post(QUOTA_API_URL)
            .bearer_auth(access_token)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .json(&payload)
            .send()
            .await
        {
            Ok(response) => {
                if let Err(_) = response.error_for_status_ref() {
                    let status = response.status();

                    // 403 Forbidden → mark as forbidden, no retry
                    if status == reqwest::StatusCode::FORBIDDEN {
                        warn!("[Quota] Account {} returned 403 Forbidden", email);
                        let mut q = QuotaData::new();
                        q.is_forbidden = true;
                        q.subscription_tier = subscription_tier.clone();
                        return Ok((q, project_id.clone()));
                    }

                    let text = response.text().await.unwrap_or_default();
                    if attempt < MAX_RETRIES {
                        warn!(
                            "[Quota] API error: {} - {} (attempt {}/{})",
                            status, text, attempt, MAX_RETRIES
                        );
                        last_error = Some(format!("HTTP {} - {}", status, text));
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        continue;
                    } else {
                        return Err(format!("API Error: {} - {}", status, text));
                    }
                }

                let quota_response: QuotaResponse = response
                    .json()
                    .await
                    .map_err(|e| format!("Failed to parse quota response: {}", e))?;

                let mut quota_data = QuotaData::new();

                for (name, info) in quota_response.models {
                    if let Some(qi) = info.quota_info {
                        let percentage = qi
                            .remaining_fraction
                            .map(|f| (f * 100.0) as i32)
                            .unwrap_or(0);
                        let reset_time = qi.reset_time.unwrap_or_default();

                        // Keep only gemini/claude models
                        if name.contains("gemini") || name.contains("claude") {
                            quota_data.add_model(name, percentage, reset_time);
                        }
                    }
                }

                quota_data.subscription_tier = subscription_tier.clone();
                return Ok((quota_data, project_id.clone()));
            }
            Err(e) => {
                warn!(
                    "[Quota] Request failed: {} (attempt {}/{})",
                    e, attempt, MAX_RETRIES
                );
                last_error = Some(format!("Network error: {}", e));
                if attempt < MAX_RETRIES {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| "Quota fetch failed".to_string()))
}

// ── Token refresh for warmup ────────────────────────────────────────

/// Get a valid (auto-refreshed) access token and project_id for warmup tasks.
///
/// If the token is near expiry, refreshes it and persists the updated account.
/// Returns `(access_token, project_id)`.
pub async fn get_valid_token_for_warmup(
    account: &crate::models::account::Account,
) -> Result<(String, String), String> {
    let mut account = account.clone();

    // Auto-refresh if near expiry
    let new_token = crate::modules::oauth::ensure_fresh_token(&account.token).await?;

    // Persist refreshed token
    if new_token.access_token != account.token.access_token {
        account.token = new_token;
        if let Err(e) = crate::modules::account::save_account(&account) {
            warn!("[Warmup] Failed to save refreshed token: {}", e);
        } else {
            info!(
                "[Warmup] Refreshed and saved new token for {}",
                account.email
            );
        }
    }

    // Fetch project_id
    let (project_id, _) =
        fetch_project_id(&account.token.access_token, &account.email).await;
    let final_pid = project_id.unwrap_or_else(|| DEFAULT_PROJECT_ID.to_string());

    Ok((account.token.access_token, final_pid))
}

// ── Model warmup ────────────────────────────────────────────────────

/// Trigger a model warmup by calling the local proxy's `/internal/warmup` endpoint.
///
/// Uses a no-proxy client for loopback requests (avoids Docker proxy issues).
/// Returns `true` on success, `false` on failure.
pub async fn warmup_model_directly(
    access_token: &str,
    model_name: &str,
    project_id: &str,
    email: &str,
    percentage: i32,
    _account_id: Option<&str>,
) -> bool {
    let port = config::load_app_config()
        .map(|c| c.proxy.port)
        .unwrap_or(8045);

    let warmup_url = format!("http://127.0.0.1:{}/internal/warmup", port);
    let body = json!({
        "email": email,
        "model": model_name,
        "access_token": access_token,
        "project_id": project_id
    });

    // No-proxy client for localhost (prevents Docker routing through external proxies)
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .no_proxy()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let resp = client
        .post(&warmup_url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await;

    match resp {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                info!(
                    "[Warmup] ✓ Triggered {} for {} (was {}%)",
                    model_name, email, percentage
                );
                true
            } else {
                let text = response.text().await.unwrap_or_default();
                warn!(
                    "[Warmup] ✗ {} for {} (was {}%): HTTP {} - {}",
                    model_name, email, percentage, status, text
                );
                false
            }
        }
        Err(e) => {
            warn!(
                "[Warmup] ✗ {} for {} (was {}%): {}",
                model_name, email, percentage, e
            );
            false
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── QuotaResponse deserialization ───────────────────────────────

    #[test]
    fn test_quota_response_deserialize_basic() {
        let json = r#"{
            "models": {
                "gemini-2.0-flash": {
                    "quotaInfo": {
                        "remainingFraction": 0.85,
                        "resetTime": "2025-01-01T00:00:00Z"
                    }
                }
            }
        }"#;
        let resp: QuotaResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.models.len(), 1);
        let info = resp.models.get("gemini-2.0-flash").unwrap();
        let qi = info.quota_info.as_ref().unwrap();
        assert!((qi.remaining_fraction.unwrap() - 0.85).abs() < f64::EPSILON);
        assert_eq!(qi.reset_time.as_deref(), Some("2025-01-01T00:00:00Z"));
    }

    #[test]
    fn test_quota_response_deserialize_no_quota_info() {
        let json = r#"{"models": {"gemini-pro": {}}}"#;
        let resp: QuotaResponse = serde_json::from_str(json).unwrap();
        let info = resp.models.get("gemini-pro").unwrap();
        assert!(info.quota_info.is_none());
    }

    #[test]
    fn test_quota_response_deserialize_empty_models() {
        let json = r#"{"models": {}}"#;
        let resp: QuotaResponse = serde_json::from_str(json).unwrap();
        assert!(resp.models.is_empty());
    }

    #[test]
    fn test_quota_response_deserialize_multiple_models() {
        let json = r#"{
            "models": {
                "gemini-2.0-flash": {
                    "quotaInfo": {"remainingFraction": 1.0, "resetTime": ""}
                },
                "claude-sonnet": {
                    "quotaInfo": {"remainingFraction": 0.0, "resetTime": "2025-06-01T12:00:00Z"}
                },
                "some-other-model": {
                    "quotaInfo": {"remainingFraction": 0.5}
                }
            }
        }"#;
        let resp: QuotaResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.models.len(), 3);
    }

    // ── LoadProjectResponse deserialization ──────────────────────────

    #[test]
    fn test_load_project_response_deserialize() {
        let json = r#"{
            "cloudaicompanionProject": "my-project-123",
            "currentTier": {"id": "FREE", "name": "Free"},
            "paidTier": {"id": "PRO", "quotaTier": "pro", "name": "Pro", "slug": "pro"}
        }"#;
        let resp: LoadProjectResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.project_id.as_deref(), Some("my-project-123"));
        assert_eq!(resp.paid_tier.as_ref().unwrap().id.as_deref(), Some("PRO"));
        assert_eq!(
            resp.current_tier.as_ref().unwrap().id.as_deref(),
            Some("FREE")
        );
    }

    #[test]
    fn test_load_project_response_missing_tiers() {
        let json = r#"{"cloudaicompanionProject": "proj-1"}"#;
        let resp: LoadProjectResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.project_id.as_deref(), Some("proj-1"));
        assert!(resp.current_tier.is_none());
        assert!(resp.paid_tier.is_none());
    }

    #[test]
    fn test_load_project_response_empty() {
        let json = r#"{}"#;
        let resp: LoadProjectResponse = serde_json::from_str(json).unwrap();
        assert!(resp.project_id.is_none());
    }

    // ── Subscription tier priority logic ────────────────────────────

    #[test]
    fn test_subscription_tier_priority_paid_over_current() {
        // paid_tier.id should take priority over current_tier.id
        let json = r#"{
            "cloudaicompanionProject": "proj",
            "currentTier": {"id": "FREE"},
            "paidTier": {"id": "ULTRA"}
        }"#;
        let resp: LoadProjectResponse = serde_json::from_str(json).unwrap();
        let tier = resp
            .paid_tier
            .and_then(|t| t.id)
            .or_else(|| resp.current_tier.and_then(|t| t.id));
        assert_eq!(tier.as_deref(), Some("ULTRA"));
    }

    #[test]
    fn test_subscription_tier_fallback_to_current() {
        let json = r#"{
            "cloudaicompanionProject": "proj",
            "currentTier": {"id": "PRO"}
        }"#;
        let resp: LoadProjectResponse = serde_json::from_str(json).unwrap();
        let tier = resp
            .paid_tier
            .and_then(|t| t.id)
            .or_else(|| resp.current_tier.and_then(|t| t.id));
        assert_eq!(tier.as_deref(), Some("PRO"));
    }

    #[test]
    fn test_subscription_tier_none_when_both_missing() {
        let json = r#"{"cloudaicompanionProject": "proj"}"#;
        let resp: LoadProjectResponse = serde_json::from_str(json).unwrap();
        let tier = resp
            .paid_tier
            .and_then(|t| t.id)
            .or_else(|| resp.current_tier.and_then(|t| t.id));
        assert!(tier.is_none());
    }

    // ── Quota data construction from API response ───────────────────

    #[test]
    fn test_quota_data_from_response_filters_models() {
        // Simulate the filtering logic: only gemini/claude models are kept
        let models = vec![
            ("gemini-2.0-flash".to_string(), 0.85, "2025-01-01T00:00:00Z"),
            ("claude-sonnet".to_string(), 0.5, "2025-02-01T00:00:00Z"),
            ("some-random-model".to_string(), 1.0, ""),
        ];

        let mut quota_data = QuotaData::new();
        for (name, fraction, reset) in models {
            let percentage = (fraction * 100.0) as i32;
            if name.contains("gemini") || name.contains("claude") {
                quota_data.add_model(name, percentage, reset.to_string());
            }
        }

        assert_eq!(quota_data.models.len(), 2);
        assert_eq!(quota_data.models[0].name, "gemini-2.0-flash");
        assert_eq!(quota_data.models[0].percentage, 85);
        assert_eq!(quota_data.models[1].name, "claude-sonnet");
        assert_eq!(quota_data.models[1].percentage, 50);
    }

    #[test]
    fn test_quota_data_percentage_rounding() {
        // remaining_fraction 0.999 → 99%, 0.001 → 0%, 0.505 → 50%
        let cases = vec![(0.999, 99), (0.001, 0), (0.505, 50), (1.0, 100), (0.0, 0)];
        for (fraction, expected) in cases {
            let percentage = (fraction * 100.0) as i32;
            assert_eq!(
                percentage, expected,
                "fraction {} → expected {}, got {}",
                fraction, expected, percentage
            );
        }
    }

    #[test]
    fn test_quota_data_forbidden_flag() {
        let mut q = QuotaData::new();
        assert!(!q.is_forbidden);
        q.is_forbidden = true;
        assert!(q.is_forbidden);
        assert!(q.models.is_empty()); // forbidden response has no models
    }

    // ── Default project ID ──────────────────────────────────────────

    #[test]
    fn test_default_project_id_used_when_none() {
        let pid: Option<String> = None;
        let final_pid = pid.as_deref().unwrap_or(DEFAULT_PROJECT_ID);
        assert_eq!(final_pid, "bamboo-precept-lgxtn");
    }

    #[test]
    fn test_cached_project_id_used_when_some() {
        let pid = Some("my-cached-project".to_string());
        let final_pid = pid.as_deref().unwrap_or(DEFAULT_PROJECT_ID);
        assert_eq!(final_pid, "my-cached-project");
    }

    // ── Warmup URL construction ─────────────────────────────────────

    #[test]
    fn test_warmup_url_construction() {
        let port: u16 = 8045;
        let url = format!("http://127.0.0.1:{}/internal/warmup", port);
        assert_eq!(url, "http://127.0.0.1:8045/internal/warmup");
    }

    #[test]
    fn test_warmup_url_custom_port() {
        let port: u16 = 9090;
        let url = format!("http://127.0.0.1:{}/internal/warmup", port);
        assert_eq!(url, "http://127.0.0.1:9090/internal/warmup");
    }

    #[test]
    fn test_warmup_request_body_construction() {
        let body = json!({
            "email": "test@example.com",
            "model": "gemini-2.0-flash",
            "access_token": "tok_abc",
            "project_id": "proj_123"
        });
        assert_eq!(body["email"], "test@example.com");
        assert_eq!(body["model"], "gemini-2.0-flash");
        assert_eq!(body["access_token"], "tok_abc");
        assert_eq!(body["project_id"], "proj_123");
    }

    // ── Constants ───────────────────────────────────────────────────

    #[test]
    fn test_constants() {
        assert!(QUOTA_API_URL.starts_with("https://"));
        assert!(CLOUD_CODE_BASE_URL.starts_with("https://"));
        assert!(!DEFAULT_PROJECT_ID.is_empty());
        assert!(MAX_RETRIES >= 1);
    }

    // ── QuotaInfo edge cases ────────────────────────────────────────

    #[test]
    fn test_quota_info_missing_fraction() {
        let json = r#"{"resetTime": "2025-01-01T00:00:00Z"}"#;
        let qi: QuotaInfo = serde_json::from_str(json).unwrap();
        assert!(qi.remaining_fraction.is_none());
        let percentage = qi.remaining_fraction.map(|f| (f * 100.0) as i32).unwrap_or(0);
        assert_eq!(percentage, 0);
    }

    #[test]
    fn test_quota_info_missing_reset_time() {
        let json = r#"{"remainingFraction": 0.75}"#;
        let qi: QuotaInfo = serde_json::from_str(json).unwrap();
        assert!(qi.reset_time.is_none());
        let reset = qi.reset_time.unwrap_or_default();
        assert_eq!(reset, "");
    }

    // ── Tier deserialization ────────────────────────────────────────

    #[test]
    fn test_tier_full_fields() {
        let json = r#"{"id": "ULTRA", "quotaTier": "ultra", "name": "Ultra Plan", "slug": "ultra"}"#;
        let tier: Tier = serde_json::from_str(json).unwrap();
        assert_eq!(tier.id.as_deref(), Some("ULTRA"));
        assert_eq!(tier.quota_tier.as_deref(), Some("ultra"));
        assert_eq!(tier.name.as_deref(), Some("Ultra Plan"));
        assert_eq!(tier.slug.as_deref(), Some("ultra"));
    }

    #[test]
    fn test_tier_minimal() {
        let json = r#"{}"#;
        let tier: Tier = serde_json::from_str(json).unwrap();
        assert!(tier.id.is_none());
        assert!(tier.quota_tier.is_none());
    }
}
