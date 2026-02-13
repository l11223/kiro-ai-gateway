// User Token management commands
//
// Tauri commands for CRUD operations on user tokens,
// including creation, listing, updating, deletion, renewal,
// IP binding queries, and summary statistics.

use crate::modules::user_token_db::{self, TokenIpBinding, UserToken};
use serde::{Deserialize, Serialize};

// ============================================================================
// Request types
// ============================================================================

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateTokenRequest {
    pub username: String,
    pub expires_type: String,
    pub description: Option<String>,
    pub max_ips: i32,
    pub curfew_start: Option<String>,
    pub curfew_end: Option<String>,
    pub custom_expires_at: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateTokenRequest {
    pub username: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
    pub max_ips: Option<i32>,
    pub curfew_start: Option<Option<String>>,
    pub curfew_end: Option<Option<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserTokenStats {
    pub total_tokens: usize,
    pub active_tokens: usize,
    pub total_users: usize,
    pub today_requests: i64,
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// List all user tokens
#[tauri::command]
pub async fn list_user_tokens() -> Result<Vec<UserToken>, String> {
    user_token_db::list_tokens()
}

/// Create a new user token
#[tauri::command]
pub async fn create_user_token(request: CreateTokenRequest) -> Result<UserToken, String> {
    user_token_db::create_token(
        request.username,
        request.expires_type,
        request.description,
        request.max_ips,
        request.curfew_start,
        request.curfew_end,
        request.custom_expires_at,
    )
}

/// Update an existing user token
#[tauri::command]
pub async fn update_user_token(id: String, request: UpdateTokenRequest) -> Result<(), String> {
    user_token_db::update_token(
        &id,
        request.username,
        request.description,
        request.enabled,
        request.max_ips,
        request.curfew_start,
        request.curfew_end,
    )
}

/// Delete a user token
#[tauri::command]
pub async fn delete_user_token(id: String) -> Result<(), String> {
    user_token_db::delete_token(&id)
}

/// Renew a user token
#[tauri::command]
pub async fn renew_user_token(id: String, expires_type: String) -> Result<(), String> {
    user_token_db::renew_token(&id, &expires_type)
}

/// Get IP bindings for a token
#[tauri::command]
pub async fn get_token_ip_bindings(token_id: String) -> Result<Vec<TokenIpBinding>, String> {
    user_token_db::get_token_ips(&token_id)
}

/// Get user token summary statistics
#[tauri::command]
pub async fn get_user_token_summary() -> Result<UserTokenStats, String> {
    let tokens = user_token_db::list_tokens()?;
    let active_tokens = tokens.iter().filter(|t| t.enabled).count();

    let mut users = std::collections::HashSet::new();
    for t in &tokens {
        users.insert(t.username.clone());
    }

    Ok(UserTokenStats {
        total_tokens: tokens.len(),
        active_tokens,
        total_users: users.len(),
        today_requests: 0,
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_token_request_serialization() {
        let req = CreateTokenRequest {
            username: "alice".to_string(),
            expires_type: "month".to_string(),
            description: Some("Test token".to_string()),
            max_ips: 3,
            curfew_start: Some("23:00".to_string()),
            curfew_end: Some("06:00".to_string()),
            custom_expires_at: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: CreateTokenRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.username, "alice");
        assert_eq!(deserialized.max_ips, 3);
    }

    #[test]
    fn test_update_token_request_serialization() {
        let req = UpdateTokenRequest {
            username: Some("bob".to_string()),
            description: None,
            enabled: Some(false),
            max_ips: Some(5),
            curfew_start: Some(None),
            curfew_end: Some(None),
        };
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: UpdateTokenRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.username, Some("bob".to_string()));
        assert_eq!(deserialized.enabled, Some(false));
    }

    #[test]
    fn test_user_token_stats_serialization() {
        let stats = UserTokenStats {
            total_tokens: 10,
            active_tokens: 8,
            total_users: 5,
            today_requests: 100,
        };
        let json = serde_json::to_string(&stats).unwrap();
        let deserialized: UserTokenStats = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.total_tokens, 10);
        assert_eq!(deserialized.active_tokens, 8);
        assert_eq!(deserialized.total_users, 5);
    }
}
