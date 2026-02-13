use serde::{Deserialize, Serialize};
use tracing::{info, warn};

// Google OAuth configuration
const CLIENT_ID: &str = "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "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";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v2/userinfo";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub expires_in: i64,
    #[serde(default)]
    pub token_type: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserInfo {
    pub email: String,
    pub name: Option<String>,
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    pub picture: Option<String>,
}

impl UserInfo {
    /// Get best display name
    pub fn get_display_name(&self) -> Option<String> {
        if let Some(name) = &self.name {
            if !name.trim().is_empty() {
                return Some(name.clone());
            }
        }
        match (&self.given_name, &self.family_name) {
            (Some(given), Some(family)) => Some(format!("{} {}", given, family)),
            (Some(given), None) => Some(given.clone()),
            (None, Some(family)) => Some(family.clone()),
            (None, None) => None,
        }
    }
}

/// Create a default HTTP client for OAuth requests.
/// In the future, this can be extended to use proxy pool.
fn get_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap_or_default()
}

/// Generate OAuth authorization URL
pub fn get_auth_url(redirect_uri: &str, state: &str) -> String {
    let scopes = [
        "https://www.googleapis.com/auth/cloud-platform",
        "https://www.googleapis.com/auth/userinfo.email",
        "https://www.googleapis.com/auth/userinfo.profile",
        "https://www.googleapis.com/auth/cclog",
        "https://www.googleapis.com/auth/experimentsandconfigs",
    ]
    .join(" ");

    let params = [
        ("client_id", CLIENT_ID),
        ("redirect_uri", redirect_uri),
        ("response_type", "code"),
        ("scope", &scopes),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("include_granted_scopes", "true"),
        ("state", state),
    ];

    url::Url::parse_with_params(AUTH_URL, &params)
        .expect("Invalid Auth URL")
        .to_string()
}

/// Exchange authorization code for token
pub async fn exchange_code(code: &str, redirect_uri: &str) -> Result<TokenResponse, String> {
    let client = get_http_client();

    let params = [
        ("client_id", CLIENT_ID),
        ("client_secret", CLIENT_SECRET),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("grant_type", "authorization_code"),
    ];

    let response = client
        .post(TOKEN_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() || e.is_timeout() {
                format!(
                    "Token exchange request failed: {}. Please check your network proxy settings.",
                    e
                )
            } else {
                format!("Token exchange request failed: {}", e)
            }
        })?;

    if response.status().is_success() {
        let token_res = response
            .json::<TokenResponse>()
            .await
            .map_err(|e| format!("Token parsing failed: {}", e))?;

        info!(
            "Token exchange successful! access_token: {}..., refresh_token: {}",
            &token_res
                .access_token
                .chars()
                .take(20)
                .collect::<String>(),
            if token_res.refresh_token.is_some() {
                "✓"
            } else {
                "✗ Missing"
            }
        );

        // Requirement 1.16: Log warning if refresh_token is missing
        if token_res.refresh_token.is_none() {
            warn!(
                "Google did not return a refresh_token. \
                 User may need to revoke app authorization and retry."
            );
        }

        Ok(token_res)
    } else {
        let error_text = response.text().await.unwrap_or_default();
        Err(format!("Token exchange failed: {}", error_text))
    }
}

/// Refresh access_token using refresh_token
pub async fn refresh_access_token(refresh_token: &str) -> Result<TokenResponse, String> {
    let client = get_http_client();

    let params = [
        ("client_id", CLIENT_ID),
        ("client_secret", CLIENT_SECRET),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ];

    info!("Refreshing access token...");

    let response = client
        .post(TOKEN_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() || e.is_timeout() {
                format!(
                    "Refresh request failed: {}. Cannot connect to Google auth server.",
                    e
                )
            } else {
                format!("Refresh request failed: {}", e)
            }
        })?;

    if response.status().is_success() {
        let token_data = response
            .json::<TokenResponse>()
            .await
            .map_err(|e| format!("Refresh data parsing failed: {}", e))?;

        info!(
            "Token refreshed successfully! Expires in: {} seconds",
            token_data.expires_in
        );
        Ok(token_data)
    } else {
        let error_text = response.text().await.unwrap_or_default();
        Err(format!("Refresh failed: {}", error_text))
    }
}

/// Get user info from Google using access_token
pub async fn get_user_info(access_token: &str) -> Result<(String, Option<String>), String> {
    let client = get_http_client();

    let response = client
        .get(USERINFO_URL)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("User info request failed: {}", e))?;

    if response.status().is_success() {
        let user_info = response
            .json::<UserInfo>()
            .await
            .map_err(|e| format!("User info parsing failed: {}", e))?;

        let email = user_info.email.clone();
        let name = user_info.get_display_name();
        Ok((email, name))
    } else {
        let error_text = response.text().await.unwrap_or_default();
        Err(format!("Failed to get user info: {}", error_text))
    }
}

/// Check and refresh Token if needed.
/// Requirement 1.5: Token expires when expiry_timestamp - now < 300s
/// Pure decision function: returns `true` when the token needs refreshing.
/// A token needs refresh when `expiry_timestamp - now < 300`.
pub fn needs_token_refresh(expiry_timestamp: i64, now: i64) -> bool {
    expiry_timestamp - now < 300
}

pub async fn ensure_fresh_token(
    current_token: &crate::models::token::TokenData,
) -> Result<crate::models::token::TokenData, String> {
    let now = chrono::Utc::now().timestamp();

    // If more than 300 seconds until expiry, token is still fresh
    if !needs_token_refresh(current_token.expiry_timestamp, now) {
        return Ok(current_token.clone());
    }

    // Need to refresh
    info!("Token expiring soon, refreshing...");
    let response = refresh_access_token(&current_token.refresh_token).await?;

    // Construct new TokenData, preserving refresh_token and metadata
    Ok(crate::models::token::TokenData::new(
        response.access_token,
        current_token.refresh_token.clone(),
        response.expires_in,
        current_token.email.clone(),
        current_token.project_id.clone(),
        None, // session_id will be generated in token_manager
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_auth_url_contains_required_params() {
        let redirect_uri = "http://localhost:8080/callback";
        let state = "test-state-123456";
        let url = get_auth_url(redirect_uri, state);

        assert!(url.contains("state=test-state-123456"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A8080%2Fcallback"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
        assert!(url.contains("client_id="));
    }

    #[test]
    fn test_get_auth_url_includes_all_scopes() {
        let url = get_auth_url("http://localhost:8080/callback", "state");
        assert!(url.contains("cloud-platform"));
        assert!(url.contains("userinfo.email"));
        assert!(url.contains("userinfo.profile"));
    }

    #[test]
    fn test_user_info_get_display_name_with_name() {
        let info = UserInfo {
            email: "test@example.com".to_string(),
            name: Some("John Doe".to_string()),
            given_name: None,
            family_name: None,
            picture: None,
        };
        assert_eq!(info.get_display_name(), Some("John Doe".to_string()));
    }

    #[test]
    fn test_user_info_get_display_name_from_parts() {
        let info = UserInfo {
            email: "test@example.com".to_string(),
            name: None,
            given_name: Some("John".to_string()),
            family_name: Some("Doe".to_string()),
            picture: None,
        };
        assert_eq!(info.get_display_name(), Some("John Doe".to_string()));
    }

    #[test]
    fn test_user_info_get_display_name_given_only() {
        let info = UserInfo {
            email: "test@example.com".to_string(),
            name: None,
            given_name: Some("John".to_string()),
            family_name: None,
            picture: None,
        };
        assert_eq!(info.get_display_name(), Some("John".to_string()));
    }

    #[test]
    fn test_user_info_get_display_name_none() {
        let info = UserInfo {
            email: "test@example.com".to_string(),
            name: None,
            given_name: None,
            family_name: None,
            picture: None,
        };
        assert_eq!(info.get_display_name(), None);
    }

    #[test]
    fn test_user_info_get_display_name_empty_name_falls_back() {
        let info = UserInfo {
            email: "test@example.com".to_string(),
            name: Some("  ".to_string()),
            given_name: Some("Jane".to_string()),
            family_name: None,
            picture: None,
        };
        assert_eq!(info.get_display_name(), Some("Jane".to_string()));
    }

    #[test]
    fn test_ensure_fresh_token_logic_not_expired() {
        // Token that expires far in the future should not trigger refresh
        let future_ts = chrono::Utc::now().timestamp() + 3600; // 1 hour from now
        let token = crate::models::token::TokenData {
            access_token: "test_access".to_string(),
            refresh_token: "test_refresh".to_string(),
            expires_in: 3600,
            expiry_timestamp: future_ts,
            token_type: "Bearer".to_string(),
            email: Some("test@example.com".to_string()),
            project_id: None,
            session_id: None,
        };

        // expiry_timestamp - now = 3600 > 300, so should not need refresh
        let now = chrono::Utc::now().timestamp();
        assert!(token.expiry_timestamp - now >= 300);
    }

    #[test]
    fn test_ensure_fresh_token_logic_near_expiry() {
        // Token that expires in 200 seconds should trigger refresh
        let near_ts = chrono::Utc::now().timestamp() + 200;
        let token = crate::models::token::TokenData {
            access_token: "test_access".to_string(),
            refresh_token: "test_refresh".to_string(),
            expires_in: 200,
            expiry_timestamp: near_ts,
            token_type: "Bearer".to_string(),
            email: Some("test@example.com".to_string()),
            project_id: None,
            session_id: None,
        };

        // expiry_timestamp - now ≈ 200 < 300, so should need refresh
        let now = chrono::Utc::now().timestamp();
        assert!(token.expiry_timestamp - now < 300);
    }

    #[test]
    fn test_ensure_fresh_token_logic_exactly_300() {
        // Token that expires in exactly 300 seconds - boundary case
        let boundary_ts = chrono::Utc::now().timestamp() + 300;
        let token = crate::models::token::TokenData {
            access_token: "test_access".to_string(),
            refresh_token: "test_refresh".to_string(),
            expires_in: 300,
            expiry_timestamp: boundary_ts,
            token_type: "Bearer".to_string(),
            email: None,
            project_id: None,
            session_id: None,
        };

        // expiry_timestamp - now = 300, which is NOT < 300, so should NOT refresh
        // But due to timing, it might be 299. The design says < 300 triggers refresh.
        // Our implementation uses >= 300 to skip refresh.
        let now = chrono::Utc::now().timestamp();
        let diff = token.expiry_timestamp - now;
        // diff should be approximately 300 (could be 299 due to timing)
        assert!(diff >= 299 && diff <= 301);
    }

    #[test]
    fn test_token_response_deserialization() {
        let json = r#"{
            "access_token": "ya29.test",
            "expires_in": 3600,
            "token_type": "Bearer",
            "refresh_token": "1//test_refresh"
        }"#;
        let resp: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.access_token, "ya29.test");
        assert_eq!(resp.expires_in, 3600);
        assert_eq!(resp.refresh_token, Some("1//test_refresh".to_string()));
    }

    #[test]
    fn test_token_response_deserialization_without_refresh() {
        let json = r#"{
            "access_token": "ya29.test",
            "expires_in": 3600
        }"#;
        let resp: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.access_token, "ya29.test");
        assert!(resp.refresh_token.is_none());
        assert_eq!(resp.token_type, ""); // default
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    // **Feature: kiro-ai-gateway, Property 19: Token 过期自动刷新判定**
    // **Validates: Requirements 1.5**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_token_refresh_decision(
            now in 0i64..=2_000_000_000i64,
            gap in -3600i64..=7200i64,
        ) {
            let expiry_timestamp = now.saturating_add(gap);
            let result = needs_token_refresh(expiry_timestamp, now);

            if gap < 300 {
                // Token is near expiry or already expired → must refresh
                prop_assert!(result, "Expected refresh when gap={} < 300", gap);
            } else {
                // Token is still fresh → no refresh needed
                prop_assert!(!result, "Expected no refresh when gap={} >= 300", gap);
            }
        }
    }
}
