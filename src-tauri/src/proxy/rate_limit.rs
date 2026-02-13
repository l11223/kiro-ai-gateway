use dashmap::DashMap;
use regex::Regex;
use std::time::{Duration, SystemTime};

/// Rate limit reason types
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RateLimitReason {
    /// Quota exhausted (QUOTA_EXHAUSTED)
    QuotaExhausted,
    /// Rate limit exceeded - TPM/RPM (RATE_LIMIT_EXCEEDED)
    RateLimitExceeded,
    /// Model capacity exhausted (MODEL_CAPACITY_EXHAUSTED)
    ModelCapacityExhausted,
    /// Server error (5xx)
    ServerError,
    /// Unknown reason
    Unknown,
}

/// Rate limit information
#[derive(Debug, Clone)]
pub struct RateLimitInfo {
    /// Reset time for the rate limit
    pub reset_time: SystemTime,
    /// Retry interval in seconds
    pub retry_after_sec: u64,
    /// Time when the rate limit was detected
    pub detected_at: SystemTime,
    /// Rate limit reason
    pub reason: RateLimitReason,
    /// Associated model (for model-level rate limiting)
    /// None = account-level, Some(model) = model-level
    pub model: Option<String>,
}

/// Failure count expiry: 1 hour (resets if no failure within this period)
const FAILURE_COUNT_EXPIRY_SECONDS: u64 = 3600;

/// Rate limit tracker supporting account-level and model-level rate limiting
pub struct RateLimitTracker {
    limits: DashMap<String, RateLimitInfo>,
    /// Consecutive failure counts (for exponential backoff), with timestamp for auto-expiry
    failure_counts: DashMap<String, (u32, SystemTime)>,
}

impl RateLimitTracker {
    pub fn new() -> Self {
        Self {
            limits: DashMap::new(),
            failure_counts: DashMap::new(),
        }
    }

    /// Generate rate limit key
    /// - Account-level: "account_id"
    /// - Model-level: "account_id:model_id"
    fn get_limit_key(&self, account_id: &str, model: Option<&str>) -> String {
        match model {
            Some(m) if !m.is_empty() => format!("{}:{}", account_id, m),
            _ => account_id.to_string(),
        }
    }

    /// Get remaining wait time in seconds for an account
    /// Checks both account-level and model-level locks
    pub fn get_remaining_wait(&self, account_id: &str, model: Option<&str>) -> u64 {
        let now = SystemTime::now();

        // 1. Check global account lock
        if let Some(info) = self.limits.get(account_id) {
            if info.reset_time > now {
                return info
                    .reset_time
                    .duration_since(now)
                    .unwrap_or(Duration::from_secs(0))
                    .as_secs();
            }
        }

        // 2. If model specified, check model-level lock
        if let Some(m) = model {
            let key = self.get_limit_key(account_id, Some(m));
            if let Some(info) = self.limits.get(&key) {
                if info.reset_time > now {
                    return info
                        .reset_time
                        .duration_since(now)
                        .unwrap_or(Duration::from_secs(0))
                        .as_secs();
                }
            }
        }

        0
    }

    /// Mark account request as successful, reset consecutive failure count
    ///
    /// Resets failure count to zero so next failure starts from the shortest
    /// lockout time. Also clears account-level rate limit records.
    pub fn mark_success(&self, account_id: &str) {
        if self.failure_counts.remove(account_id).is_some() {
            tracing::debug!(
                "Account {} request succeeded, failure count reset",
                account_id
            );
        }
        // Clear account-level rate limit
        self.limits.remove(account_id);
    }

    /// Precisely lock an account until a specific time point
    ///
    /// Uses the account quota's reset_time for precise locking,
    /// which is more accurate than exponential backoff.
    pub fn set_lockout_until(
        &self,
        account_id: &str,
        reset_time: SystemTime,
        reason: RateLimitReason,
        model: Option<String>,
    ) {
        let now = SystemTime::now();
        let retry_sec = reset_time
            .duration_since(now)
            .map(|d| d.as_secs())
            .unwrap_or(60);

        let info = RateLimitInfo {
            reset_time,
            retry_after_sec: retry_sec,
            detected_at: now,
            reason,
            model: model.clone(),
        };

        let key = self.get_limit_key(account_id, model.as_deref());
        self.limits.insert(key, info);

        if let Some(m) = &model {
            tracing::info!(
                "Account {} model {} locked until quota reset, {} seconds remaining",
                account_id,
                m,
                retry_sec
            );
        } else {
            tracing::info!(
                "Account {} locked until quota reset, {} seconds remaining",
                account_id,
                retry_sec
            );
        }
    }

    /// Lock account using ISO 8601 time string
    ///
    /// Parses time strings like "2026-01-08T17:00:00Z"
    pub fn set_lockout_until_iso(
        &self,
        account_id: &str,
        reset_time_str: &str,
        reason: RateLimitReason,
        model: Option<String>,
    ) -> bool {
        match chrono::DateTime::parse_from_rfc3339(reset_time_str) {
            Ok(dt) => {
                let reset_time =
                    SystemTime::UNIX_EPOCH + Duration::from_secs(dt.timestamp() as u64);
                self.set_lockout_until(account_id, reset_time, reason, model);
                true
            }
            Err(e) => {
                tracing::warn!(
                    "Cannot parse quota reset time '{}': {}, using default backoff",
                    reset_time_str,
                    e
                );
                false
            }
        }
    }

    /// Parse rate limit info from error response
    ///
    /// # Arguments
    /// * `account_id` - Account ID
    /// * `status` - HTTP status code
    /// * `retry_after_header` - Retry-After header value
    /// * `body` - Error response body
    /// * `model` - Optional model name for model-level rate limiting
    /// * `backoff_steps` - Exponential backoff configuration (e.g. [60, 300, 1800, 7200])
    pub fn parse_from_error(
        &self,
        account_id: &str,
        status: u16,
        retry_after_header: Option<&str>,
        body: &str,
        model: Option<String>,
        backoff_steps: &[u64],
    ) -> Option<RateLimitInfo> {
        // Support 429 (rate limit) and 500/503/529 (server error soft avoidance) and 404
        if status != 429 && status != 500 && status != 503 && status != 529 && status != 404 {
            return None;
        }

        // 1. Parse rate limit reason type
        let reason = if status == 429 {
            tracing::warn!("Google 429 Error Body: {}", body);
            self.parse_rate_limit_reason(body)
        } else if status == 404 {
            tracing::warn!(
                "Google 404: model unavailable on this account, short lockout before rotation"
            );
            RateLimitReason::ServerError
        } else {
            RateLimitReason::ServerError
        };

        let mut retry_after_sec = None;

        // 2. Extract from Retry-After header
        if let Some(retry_after) = retry_after_header {
            if let Ok(seconds) = retry_after.parse::<u64>() {
                retry_after_sec = Some(seconds);
            }
        }

        // 3. Extract from error message body (try JSON first, then regex)
        if retry_after_sec.is_none() {
            retry_after_sec = self.parse_retry_time_from_body(body);
        }

        // 4. Handle defaults and soft avoidance logic based on reason type
        let retry_sec = match retry_after_sec {
            Some(s) => {
                // Safety buffer: minimum 2 seconds to prevent extremely high-frequency retries
                if s < 2 {
                    2
                } else {
                    s
                }
            }
            None => {
                // Get consecutive failure count for exponential backoff (with auto-expiry)
                // ServerError (5xx) does NOT increment failure_count to avoid polluting 429 backoff
                let failure_count = if reason != RateLimitReason::ServerError {
                    let now = SystemTime::now();
                    let mut entry = self
                        .failure_counts
                        .entry(account_id.to_string())
                        .or_insert((0, now));

                    let elapsed = now
                        .duration_since(entry.1)
                        .unwrap_or(Duration::from_secs(0))
                        .as_secs();
                    if elapsed > FAILURE_COUNT_EXPIRY_SECONDS {
                        tracing::debug!(
                            "Account {} failure count expired ({}s), resetting to 0",
                            account_id,
                            elapsed
                        );
                        *entry = (0, now);
                    }
                    entry.0 += 1;
                    entry.1 = now;
                    entry.0
                } else {
                    // ServerError uses fixed value 1, no accumulation
                    1
                };

                match reason {
                    RateLimitReason::QuotaExhausted => {
                        let index = (failure_count as usize).saturating_sub(1);
                        let lockout = if index < backoff_steps.len() {
                            backoff_steps[index]
                        } else {
                            *backoff_steps.last().unwrap_or(&7200)
                        };

                        tracing::warn!(
                            "QuotaExhausted detected, failure #{}, locking for {} seconds per config",
                            failure_count,
                            lockout
                        );
                        lockout
                    }
                    RateLimitReason::RateLimitExceeded => {
                        tracing::debug!(
                            "RateLimitExceeded (TPM/RPM) detected, using default 5s lockout"
                        );
                        5
                    }
                    RateLimitReason::ModelCapacityExhausted => {
                        let lockout = match failure_count {
                            1 => 5,
                            2 => 10,
                            _ => 15,
                        };
                        tracing::warn!(
                            "ModelCapacityExhausted detected, failure #{}, retrying in {}s",
                            failure_count,
                            lockout
                        );
                        lockout
                    }
                    RateLimitReason::ServerError => {
                        let lockout = if status == 404 { 5 } else { 8 };
                        tracing::warn!(
                            "Server error {} detected, soft avoidance for {}s",
                            status,
                            lockout
                        );
                        lockout
                    }
                    RateLimitReason::Unknown => {
                        tracing::debug!(
                            "Cannot parse 429 rate limit reason, using default 60s lockout"
                        );
                        60
                    }
                }
            }
        };

        let info = RateLimitInfo {
            reset_time: SystemTime::now() + Duration::from_secs(retry_sec),
            retry_after_sec: retry_sec,
            detected_at: SystemTime::now(),
            reason,
            model: model.clone(),
        };

        // Only QuotaExhausted uses model-level isolation; others affect the whole account
        let use_model_key =
            matches!(reason, RateLimitReason::QuotaExhausted) && model.is_some();
        let key = if use_model_key {
            self.get_limit_key(account_id, model.as_deref())
        } else {
            account_id.to_string()
        };

        self.limits.insert(key, info.clone());

        tracing::warn!(
            "Account {} [{}] rate limit type: {:?}, reset delay: {}s",
            account_id,
            status,
            reason,
            retry_sec
        );

        Some(info)
    }

    /// Parse rate limit reason from response body
    pub fn parse_rate_limit_reason(&self, body: &str) -> RateLimitReason {
        // Try to extract reason field from JSON
        let trimmed = body.trim();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(reason_str) = json
                    .get("error")
                    .and_then(|e| e.get("details"))
                    .and_then(|d| d.as_array())
                    .and_then(|a| a.first())
                    .and_then(|o| o.get("reason"))
                    .and_then(|v| v.as_str())
                {
                    return match reason_str {
                        "QUOTA_EXHAUSTED" => RateLimitReason::QuotaExhausted,
                        "RATE_LIMIT_EXCEEDED" => RateLimitReason::RateLimitExceeded,
                        "MODEL_CAPACITY_EXHAUSTED" => RateLimitReason::ModelCapacityExhausted,
                        _ => RateLimitReason::Unknown,
                    };
                }
                // Try text matching from message field as fallback
                if let Some(msg) = json
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                {
                    let msg_lower = msg.to_lowercase();
                    if msg_lower.contains("per minute") || msg_lower.contains("rate limit") {
                        return RateLimitReason::RateLimitExceeded;
                    }
                }
            }
        }

        // Fallback: text-based matching
        let body_lower = body.to_lowercase();
        // Prioritize per-minute limits to avoid misclassifying TPM as Quota
        if body_lower.contains("per minute")
            || body_lower.contains("rate limit")
            || body_lower.contains("too many requests")
        {
            RateLimitReason::RateLimitExceeded
        } else if body_lower.contains("exhausted") || body_lower.contains("quota") {
            RateLimitReason::QuotaExhausted
        } else {
            RateLimitReason::Unknown
        }
    }

    /// Parse duration string: supports "2h1m1s", "42s", "500ms", "510.790006ms" etc.
    pub fn parse_duration_string(&self, s: &str) -> Option<u64> {
        tracing::debug!("[Duration parse] Attempting to parse: '{}'", s);

        let mut hours: u64 = 0;
        let mut minutes: u64 = 0;
        let mut seconds: f64 = 0.0;
        let mut milliseconds: f64 = 0.0;

        // Extract milliseconds first
        if let Ok(re) = Regex::new(r"(\d+(?:\.\d+)?)ms") {
            if let Some(caps) = re.captures(s) {
                milliseconds = caps[1].parse::<f64>().unwrap_or(0.0);
            }
        }

        // Remove "ms" portions from the string so they don't interfere with "m" and "s" parsing
        let s_without_ms = if let Ok(re) = Regex::new(r"\d+(?:\.\d+)?ms") {
            re.replace_all(s, "").to_string()
        } else {
            s.to_string()
        };

        // Extract hours
        if let Ok(re) = Regex::new(r"(\d+)h") {
            if let Some(caps) = re.captures(&s_without_ms) {
                hours = caps[1].parse::<u64>().unwrap_or(0);
            }
        }

        // Extract minutes
        if let Ok(re) = Regex::new(r"(\d+)m") {
            if let Some(caps) = re.captures(&s_without_ms) {
                minutes = caps[1].parse::<u64>().unwrap_or(0);
            }
        }

        // Extract seconds
        if let Ok(re) = Regex::new(r"(\d+(?:\.\d+)?)s") {
            if let Some(caps) = re.captures(&s_without_ms) {
                seconds = caps[1].parse::<f64>().unwrap_or(0.0);
            }
        }

        tracing::debug!(
            "[Duration parse] Extracted: {}h {}m {:.3}s {:.3}ms",
            hours,
            minutes,
            seconds,
            milliseconds
        );

        // Calculate total seconds, milliseconds rounded up
        let total_seconds =
            hours * 3600 + minutes * 60 + seconds.ceil() as u64 + (milliseconds / 1000.0).ceil() as u64;

        if total_seconds == 0 {
            tracing::warn!("[Duration parse] Failed: '{}' (total seconds is 0)", s);
            None
        } else {
            tracing::info!(
                "[Duration parse] Success: '{}' => {}s ({}h {}m {:.1}s {:.1}ms)",
                s,
                total_seconds,
                hours,
                minutes,
                seconds,
                milliseconds
            );
            Some(total_seconds)
        }
    }

    /// Parse retry time from error response body
    fn parse_retry_time_from_body(&self, body: &str) -> Option<u64> {
        // A. Try JSON parsing first
        let trimmed = body.trim();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                // 1. Google's quotaResetDelay format
                if let Some(delay_str) = json
                    .get("error")
                    .and_then(|e| e.get("details"))
                    .and_then(|d| d.as_array())
                    .and_then(|a| a.first())
                    .and_then(|o| o.get("metadata"))
                    .and_then(|m| m.get("quotaResetDelay"))
                    .and_then(|v| v.as_str())
                {
                    tracing::debug!("[JSON parse] Found quotaResetDelay: '{}'", delay_str);
                    if let Some(seconds) = self.parse_duration_string(delay_str) {
                        return Some(seconds);
                    }
                }

                // 2. OpenAI-style retry_after field (numeric)
                if let Some(retry) = json
                    .get("error")
                    .and_then(|e| e.get("retry_after"))
                    .and_then(|v| v.as_u64())
                {
                    return Some(retry);
                }
            }
        }

        // B. Regex fallback patterns
        // Pattern 1: "Try again in 2m 30s"
        if let Ok(re) = Regex::new(r"(?i)try again in (\d+)m\s*(\d+)s") {
            if let Some(caps) = re.captures(body) {
                if let (Ok(m), Ok(s)) = (caps[1].parse::<u64>(), caps[2].parse::<u64>()) {
                    return Some(m * 60 + s);
                }
            }
        }

        // Pattern 2: "Try again in 30s" or "backoff for 42s"
        if let Ok(re) = Regex::new(r"(?i)(?:try again in|backoff for|wait)\s*(\d+)s") {
            if let Some(caps) = re.captures(body) {
                if let Ok(s) = caps[1].parse::<u64>() {
                    return Some(s);
                }
            }
        }

        // Pattern 3: "quota will reset in X seconds"
        if let Ok(re) = Regex::new(r"(?i)quota will reset in (\d+) second") {
            if let Some(caps) = re.captures(body) {
                if let Ok(s) = caps[1].parse::<u64>() {
                    return Some(s);
                }
            }
        }

        // Pattern 4: OpenAI-style "Retry after (\d+) seconds"
        if let Ok(re) = Regex::new(r"(?i)retry after (\d+) second") {
            if let Some(caps) = re.captures(body) {
                if let Ok(s) = caps[1].parse::<u64>() {
                    return Some(s);
                }
            }
        }

        // Pattern 5: Parenthesized form "(wait (\d+)s)"
        if let Ok(re) = Regex::new(r"\(wait (\d+)s\)") {
            if let Some(caps) = re.captures(body) {
                if let Ok(s) = caps[1].parse::<u64>() {
                    return Some(s);
                }
            }
        }

        None
    }

    /// Get rate limit info for an account
    pub fn get(&self, account_id: &str) -> Option<RateLimitInfo> {
        self.limits.get(account_id).map(|r| r.clone())
    }

    /// Check if an account is currently rate limited (supports model-level)
    pub fn is_rate_limited(&self, account_id: &str, model: Option<&str>) -> bool {
        self.get_remaining_wait(account_id, model) > 0
    }

    /// Get seconds until rate limit reset for an account
    pub fn get_reset_seconds(&self, account_id: &str) -> Option<u64> {
        if let Some(info) = self.get(account_id) {
            info.reset_time
                .duration_since(SystemTime::now())
                .ok()
                .map(|d| d.as_secs())
        } else {
            None
        }
    }

    /// Clean up expired rate limit records
    pub fn cleanup_expired(&self) -> usize {
        let now = SystemTime::now();
        let mut count = 0;

        self.limits.retain(|_k, v| {
            if v.reset_time <= now {
                count += 1;
                false
            } else {
                true
            }
        });

        if count > 0 {
            tracing::debug!("Cleaned up {} expired rate limit records", count);
        }

        count
    }

    /// Clear rate limit records for a specific account
    pub fn clear(&self, account_id: &str) -> bool {
        self.limits.remove(account_id).is_some()
    }

    /// Clear all rate limit records (optimistic reset strategy)
    ///
    /// Used when all accounts are rate limited but wait times are short,
    /// clears all records to resolve timing race conditions.
    pub fn clear_all(&self) {
        let count = self.limits.len();
        self.limits.clear();
        tracing::warn!(
            "Optimistic reset: Cleared all {} rate limit record(s)",
            count
        );
    }
}

impl Default for RateLimitTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_retry_time_minutes_seconds() {
        let tracker = RateLimitTracker::new();
        let body = "Rate limit exceeded. Try again in 2m 30s";
        let time = tracker.parse_retry_time_from_body(body);
        assert_eq!(time, Some(150));
    }

    #[test]
    fn test_parse_google_json_delay() {
        let tracker = RateLimitTracker::new();
        let body = r#"{
            "error": {
                "details": [
                    {
                        "metadata": {
                            "quotaResetDelay": "42s"
                        }
                    }
                ]
            }
        }"#;
        let time = tracker.parse_retry_time_from_body(body);
        assert_eq!(time, Some(42));
    }

    #[test]
    fn test_parse_retry_after_ignore_case() {
        let tracker = RateLimitTracker::new();
        let body = "Quota limit hit. Retry After 99 Seconds";
        let time = tracker.parse_retry_time_from_body(body);
        assert_eq!(time, Some(99));
    }

    #[test]
    fn test_get_remaining_wait() {
        let tracker = RateLimitTracker::new();
        tracker.parse_from_error("acc1", 429, Some("30"), "", None, &[]);
        let wait = tracker.get_remaining_wait("acc1", None);
        assert!(wait > 25 && wait <= 30);
    }

    #[test]
    fn test_safety_buffer() {
        let tracker = RateLimitTracker::new();
        // If API returns 1s, we force it to 2s minimum
        tracker.parse_from_error("acc1", 429, Some("1"), "", None, &[]);
        let wait = tracker.get_remaining_wait("acc1", None);
        assert!(wait >= 1 && wait <= 2);
    }

    #[test]
    fn test_tpm_exhausted_is_rate_limit_exceeded() {
        let tracker = RateLimitTracker::new();
        // Simulate real-world TPM error with both "Resource exhausted" and "per minute"
        let body = "Resource has been exhausted (e.g. check quota). Quota limit 'Tokens per minute' exceeded.";
        let reason = tracker.parse_rate_limit_reason(body);
        // Should be identified as RateLimitExceeded, not QuotaExhausted
        assert_eq!(reason, RateLimitReason::RateLimitExceeded);
    }

    #[test]
    fn test_server_error_does_not_accumulate_failure_count() {
        let tracker = RateLimitTracker::new();
        let backoff_steps = vec![60, 300, 1800, 7200];

        // Simulate 5 consecutive 5xx errors
        for i in 1..=5 {
            let info = tracker.parse_from_error(
                "acc1",
                503,
                None,
                "Service Unavailable",
                None,
                &backoff_steps,
            );
            assert!(info.is_some(), "5xx #{} should return RateLimitInfo", i);
            let info = info.unwrap();
            // 5xx should always lock for 8 seconds regardless of failure_count
            assert_eq!(info.retry_after_sec, 8, "5xx #{} should lock for 8s", i);
        }

        // Now trigger a 429 QuotaExhausted (without quotaResetDelay)
        let quota_body = r#"{"error":{"details":[{"reason":"QUOTA_EXHAUSTED"}]}}"#;
        let info =
            tracker.parse_from_error("acc1", 429, None, quota_body, None, &backoff_steps);
        assert!(info.is_some());
        let info = info.unwrap();

        // Key assertion: 429 should start from failure #1 (60s), not inherit 5xx count
        assert_eq!(
            info.retry_after_sec, 60,
            "429 should start from backoff step 0 (60s), not be polluted by 5xx"
        );
    }

    #[test]
    fn test_quota_exhausted_does_accumulate_failure_count() {
        let tracker = RateLimitTracker::new();
        let backoff_steps = vec![60, 300, 1800, 7200];
        let quota_body = r#"{"error":{"details":[{"reason":"QUOTA_EXHAUSTED"}]}}"#;

        // 1st 429 → 60s
        let info =
            tracker.parse_from_error("acc2", 429, None, quota_body, None, &backoff_steps);
        assert_eq!(info.unwrap().retry_after_sec, 60);

        // 2nd 429 → 300s
        let info =
            tracker.parse_from_error("acc2", 429, None, quota_body, None, &backoff_steps);
        assert_eq!(info.unwrap().retry_after_sec, 300);

        // 3rd 429 → 1800s
        let info =
            tracker.parse_from_error("acc2", 429, None, quota_body, None, &backoff_steps);
        assert_eq!(info.unwrap().retry_after_sec, 1800);

        // 4th 429 → 7200s
        let info =
            tracker.parse_from_error("acc2", 429, None, quota_body, None, &backoff_steps);
        assert_eq!(info.unwrap().retry_after_sec, 7200);
    }

    #[test]
    fn test_mark_success_resets_failure_count() {
        let tracker = RateLimitTracker::new();
        let backoff_steps = vec![60, 300, 1800, 7200];
        let quota_body = r#"{"error":{"details":[{"reason":"QUOTA_EXHAUSTED"}]}}"#;

        // Trigger 2 failures → failure_count = 2
        tracker.parse_from_error("acc3", 429, None, quota_body, None, &backoff_steps);
        tracker.parse_from_error("acc3", 429, None, quota_body, None, &backoff_steps);

        // Mark success → resets failure count
        tracker.mark_success("acc3");

        // Next failure should start from step 0 again (60s)
        let info =
            tracker.parse_from_error("acc3", 429, None, quota_body, None, &backoff_steps);
        assert_eq!(info.unwrap().retry_after_sec, 60);
    }

    #[test]
    fn test_clear_all() {
        let tracker = RateLimitTracker::new();
        tracker.parse_from_error("acc1", 429, Some("30"), "", None, &[]);
        tracker.parse_from_error("acc2", 429, Some("60"), "", None, &[]);

        assert!(tracker.is_rate_limited("acc1", None));
        assert!(tracker.is_rate_limited("acc2", None));

        tracker.clear_all();

        assert!(!tracker.is_rate_limited("acc1", None));
        assert!(!tracker.is_rate_limited("acc2", None));
    }

    #[test]
    fn test_cleanup_expired() {
        let tracker = RateLimitTracker::new();

        // Insert an already-expired record
        let expired_info = RateLimitInfo {
            reset_time: SystemTime::now() - Duration::from_secs(10),
            retry_after_sec: 5,
            detected_at: SystemTime::now() - Duration::from_secs(15),
            reason: RateLimitReason::ServerError,
            model: None,
        };
        tracker.limits.insert("expired_acc".to_string(), expired_info);

        // Insert a still-active record
        tracker.parse_from_error("active_acc", 429, Some("60"), "", None, &[]);

        let cleaned = tracker.cleanup_expired();
        assert_eq!(cleaned, 1);
        assert!(!tracker.is_rate_limited("expired_acc", None));
        assert!(tracker.is_rate_limited("active_acc", None));
    }

    #[test]
    fn test_model_level_rate_limiting() {
        let tracker = RateLimitTracker::new();
        let backoff_steps = vec![60, 300, 1800, 7200];
        let quota_body = r#"{"error":{"details":[{"reason":"QUOTA_EXHAUSTED"}]}}"#;

        // Rate limit a specific model
        tracker.parse_from_error(
            "acc1",
            429,
            None,
            quota_body,
            Some("gemini-pro".to_string()),
            &backoff_steps,
        );

        // Model-level should be limited
        assert!(tracker.is_rate_limited("acc1", Some("gemini-pro")));
        // Account-level should NOT be limited (QuotaExhausted uses model isolation)
        assert!(!tracker.is_rate_limited("acc1", None));
        // Different model should NOT be limited
        assert!(!tracker.is_rate_limited("acc1", Some("gemini-flash")));
    }

    #[test]
    fn test_parse_duration_string_various_formats() {
        let tracker = RateLimitTracker::new();

        assert_eq!(tracker.parse_duration_string("42s"), Some(42));
        assert_eq!(tracker.parse_duration_string("2h1m1s"), Some(7261));
        assert_eq!(tracker.parse_duration_string("1h30m"), Some(5400));
        assert_eq!(tracker.parse_duration_string("5m"), Some(300));
        assert_eq!(tracker.parse_duration_string("500ms"), Some(1));
        assert_eq!(tracker.parse_duration_string("1500ms"), Some(2));
        assert_eq!(tracker.parse_duration_string("510.790006ms"), Some(1));
    }

    #[test]
    fn test_parse_rate_limit_reason_json() {
        let tracker = RateLimitTracker::new();

        let body = r#"{"error":{"details":[{"reason":"QUOTA_EXHAUSTED"}]}}"#;
        assert_eq!(
            tracker.parse_rate_limit_reason(body),
            RateLimitReason::QuotaExhausted
        );

        let body = r#"{"error":{"details":[{"reason":"RATE_LIMIT_EXCEEDED"}]}}"#;
        assert_eq!(
            tracker.parse_rate_limit_reason(body),
            RateLimitReason::RateLimitExceeded
        );

        let body = r#"{"error":{"details":[{"reason":"MODEL_CAPACITY_EXHAUSTED"}]}}"#;
        assert_eq!(
            tracker.parse_rate_limit_reason(body),
            RateLimitReason::ModelCapacityExhausted
        );
    }

    #[test]
    fn test_set_lockout_until_iso() {
        let tracker = RateLimitTracker::new();

        // Valid ISO 8601 time in the future
        let future_time = chrono::Utc::now() + chrono::Duration::hours(1);
        let iso_str = future_time.to_rfc3339();

        let result = tracker.set_lockout_until_iso(
            "acc1",
            &iso_str,
            RateLimitReason::QuotaExhausted,
            None,
        );
        assert!(result);
        assert!(tracker.is_rate_limited("acc1", None));

        // Invalid time string
        let result = tracker.set_lockout_until_iso(
            "acc2",
            "not-a-date",
            RateLimitReason::QuotaExhausted,
            None,
        );
        assert!(!result);
    }

    #[test]
    fn test_rate_limit_exceeded_fixed_lockout() {
        let tracker = RateLimitTracker::new();
        let backoff_steps = vec![60, 300, 1800, 7200];
        let body = r#"{"error":{"details":[{"reason":"RATE_LIMIT_EXCEEDED"}]}}"#;

        // RateLimitExceeded should always use 5s fixed lockout
        let info =
            tracker.parse_from_error("acc1", 429, None, body, None, &backoff_steps);
        assert_eq!(info.unwrap().retry_after_sec, 5);

        // Even on second failure, still 5s
        let info =
            tracker.parse_from_error("acc1", 429, None, body, None, &backoff_steps);
        assert_eq!(info.unwrap().retry_after_sec, 5);
    }

    #[test]
    fn test_model_capacity_exhausted_progressive() {
        let tracker = RateLimitTracker::new();
        let backoff_steps = vec![60, 300, 1800, 7200];
        let body = r#"{"error":{"details":[{"reason":"MODEL_CAPACITY_EXHAUSTED"}]}}"#;

        // Progressive: 5, 10, 15, 15...
        let info =
            tracker.parse_from_error("acc1", 429, None, body, None, &backoff_steps);
        assert_eq!(info.unwrap().retry_after_sec, 5);

        let info =
            tracker.parse_from_error("acc1", 429, None, body, None, &backoff_steps);
        assert_eq!(info.unwrap().retry_after_sec, 10);

        let info =
            tracker.parse_from_error("acc1", 429, None, body, None, &backoff_steps);
        assert_eq!(info.unwrap().retry_after_sec, 15);

        // Stays at 15
        let info =
            tracker.parse_from_error("acc1", 429, None, body, None, &backoff_steps);
        assert_eq!(info.unwrap().retry_after_sec, 15);
    }

    // **Feature: kiro-ai-gateway, Property 7: QuotaExhausted 指数退避递增性**
    // **Validates: Requirements 4.8**
    mod prop_quota_exhausted_backoff {
        use super::*;
        use proptest::prelude::*;

        /// Strategy to generate valid backoff_steps arrays:
        /// - Length 1..=8
        /// - Values are monotonically non-decreasing (sorted)
        /// - Each value in range 1..=14400 (up to 4 hours)
        fn backoff_steps_strategy() -> impl Strategy<Value = Vec<u64>> {
            prop::collection::vec(1u64..=14400, 1..=8)
                .prop_map(|mut v| {
                    v.sort();
                    v
                })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn prop_quota_exhausted_exponential_backoff(
                backoff_steps in backoff_steps_strategy(),
                n in 1u32..=20,
            ) {
                let tracker = RateLimitTracker::new();
                let account_id = "prop_test_account";
                // QuotaExhausted body without quotaResetDelay so backoff logic is used
                let quota_body = r#"{"error":{"details":[{"reason":"QUOTA_EXHAUSTED"}]}}"#;

                let mut lockout_times = Vec::new();

                for i in 1..=n {
                    let info = tracker.parse_from_error(
                        account_id,
                        429,
                        None,
                        quota_body,
                        None,
                        &backoff_steps,
                    );
                    prop_assert!(
                        info.is_some(),
                        "parse_from_error should return Some for QuotaExhausted failure #{}",
                        i
                    );
                    let lockout = info.unwrap().retry_after_sec;
                    lockout_times.push(lockout);

                    // Verify: Nth lockout == backoff_steps[min(N-1, len-1)]
                    let expected_index = ((i as usize) - 1).min(backoff_steps.len() - 1);
                    let expected_lockout = backoff_steps[expected_index];
                    prop_assert_eq!(
                        lockout,
                        expected_lockout,
                        "Failure #{}: expected lockout {}s (backoff_steps[{}]), got {}s. steps={:?}",
                        i,
                        expected_lockout,
                        expected_index,
                        lockout,
                        backoff_steps
                    );
                }

                // Verify monotonically non-decreasing
                for j in 1..lockout_times.len() {
                    prop_assert!(
                        lockout_times[j] >= lockout_times[j - 1],
                        "Lockout times should be monotonically non-decreasing: {:?}",
                        lockout_times
                    );
                }
            }
        }
    }

    // **Feature: kiro-ai-gateway, Property 6: 限流时间字符串解析正确性**
    // **Validates: Requirements 4.10**
    mod prop_duration_parse {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn prop_parse_duration_string_correctness(
                h in 0u64..100,
                m in 0u64..60,
                s in 0u64..60,
                ms in 0u64..2000,
            ) {
                // Skip the all-zero case since parse_duration_string returns None for 0 total
                prop_assume!(h > 0 || m > 0 || s > 0 || ms > 0);

                // Build the time string from components
                let mut parts = Vec::new();
                if h > 0 { parts.push(format!("{}h", h)); }
                if m > 0 { parts.push(format!("{}m", m)); }
                if s > 0 { parts.push(format!("{}s", s)); }
                if ms > 0 { parts.push(format!("{}ms", ms)); }
                let input = parts.join("");

                let tracker = RateLimitTracker::new();
                let result = tracker.parse_duration_string(&input);

                // Expected: hours*3600 + minutes*60 + seconds + ceil(ms/1000)
                let expected = h * 3600 + m * 60 + s + ((ms as f64) / 1000.0).ceil() as u64;

                prop_assert_eq!(
                    result,
                    Some(expected),
                    "parse_duration_string(\"{}\") = {:?}, expected Some({})",
                    input,
                    result,
                    expected
                );
            }
        }
    }

    // **Feature: kiro-ai-gateway, Property 8: 5xx 错误不污染退避阶梯**
    // **Validates: Requirements 4.13**
    mod prop_5xx_no_backoff_pollution {
        use super::*;
        use proptest::prelude::*;

        /// Strategy to generate valid backoff_steps arrays:
        /// - Length 1..=8
        /// - Values are monotonically non-decreasing (sorted)
        /// - Each value in range 1..=14400 (up to 4 hours)
        fn backoff_steps_strategy() -> impl Strategy<Value = Vec<u64>> {
            prop::collection::vec(1u64..=14400, 1..=8).prop_map(|mut v| {
                v.sort();
                v
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn prop_5xx_does_not_pollute_backoff_ladder(
                backoff_steps in backoff_steps_strategy(),
                num_5xx in 0u32..=20,
                status_code in prop::sample::select(vec![500u16, 503, 529]),
            ) {
                let tracker = RateLimitTracker::new();
                let account_id = "prop_5xx_test_account";

                // Phase 1: Send num_5xx server errors
                for i in 0..num_5xx {
                    let info = tracker.parse_from_error(
                        account_id,
                        status_code,
                        None,
                        "Internal Server Error",
                        None,
                        &backoff_steps,
                    );
                    prop_assert!(
                        info.is_some(),
                        "5xx #{} should return Some RateLimitInfo",
                        i + 1
                    );
                    let info = info.unwrap();
                    // Each 5xx should lock for exactly 8 seconds
                    prop_assert_eq!(
                        info.retry_after_sec, 8,
                        "5xx #{} should always lock for 8s, got {}s",
                        i + 1,
                        info.retry_after_sec
                    );
                }

                // Phase 2: Trigger one QuotaExhausted (without quotaResetDelay)
                let quota_body = r#"{"error":{"details":[{"reason":"QUOTA_EXHAUSTED"}]}}"#;
                let info = tracker.parse_from_error(
                    account_id,
                    429,
                    None,
                    quota_body,
                    None,
                    &backoff_steps,
                );
                prop_assert!(
                    info.is_some(),
                    "QuotaExhausted after {} 5xx errors should return Some",
                    num_5xx
                );
                let info = info.unwrap();

                // Key assertion: first QuotaExhausted SHALL use backoff_steps[0]
                // regardless of how many 5xx errors preceded it
                prop_assert_eq!(
                    info.retry_after_sec,
                    backoff_steps[0],
                    "After {} 5xx errors, first QuotaExhausted should use backoff_steps[0] = {}s, got {}s. steps={:?}",
                    num_5xx,
                    backoff_steps[0],
                    info.retry_after_sec,
                    backoff_steps
                );
            }
        }
    }

    // **Feature: kiro-ai-gateway, Property 9: 成功请求重置失败计数**
    // **Validates: Requirements 4.9**
    mod prop_success_resets_failure_count {
        use super::*;
        use proptest::prelude::*;

        /// Strategy to generate valid backoff_steps arrays:
        /// - Length 1..=8
        /// - Values are monotonically non-decreasing (sorted)
        /// - Each value in range 1..=14400 (up to 4 hours)
        fn backoff_steps_strategy() -> impl Strategy<Value = Vec<u64>> {
            prop::collection::vec(1u64..=14400, 1..=8).prop_map(|mut v| {
                v.sort();
                v
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn prop_mark_success_resets_failure_count_and_clears_rate_limit(
                backoff_steps in backoff_steps_strategy(),
                num_failures in 1u32..=20,
            ) {
                let tracker = RateLimitTracker::new();
                let account_id = "prop_success_reset_account";
                let quota_body = r#"{"error":{"details":[{"reason":"QUOTA_EXHAUSTED"}]}}"#;

                // Phase 1: Accumulate num_failures QuotaExhausted errors
                for _ in 0..num_failures {
                    let info = tracker.parse_from_error(
                        account_id,
                        429,
                        None,
                        quota_body,
                        None,
                        &backoff_steps,
                    );
                    prop_assert!(info.is_some(), "parse_from_error should return Some for QuotaExhausted");
                }

                // Account should be rate-limited after failures
                prop_assert!(
                    tracker.is_rate_limited(account_id, None),
                    "Account should be rate-limited after {} failures",
                    num_failures
                );

                // Phase 2: Mark success
                tracker.mark_success(account_id);

                // Assertion 1: Account-level rate limit record SHALL be cleared
                prop_assert!(
                    !tracker.is_rate_limited(account_id, None),
                    "Account should NOT be rate-limited after mark_success (had {} failures)",
                    num_failures
                );

                // Assertion 2: Failure count SHALL be reset to 0
                // Verified by: next QuotaExhausted should use backoff_steps[0]
                let info = tracker.parse_from_error(
                    account_id,
                    429,
                    None,
                    quota_body,
                    None,
                    &backoff_steps,
                );
                prop_assert!(info.is_some(), "parse_from_error should return Some after reset");
                let lockout = info.unwrap().retry_after_sec;
                prop_assert_eq!(
                    lockout,
                    backoff_steps[0],
                    "After mark_success with {} prior failures, next QuotaExhausted should use backoff_steps[0] = {}s, got {}s. steps={:?}",
                    num_failures,
                    backoff_steps[0],
                    lockout,
                    backoff_steps
                );
            }
        }
    }


}
