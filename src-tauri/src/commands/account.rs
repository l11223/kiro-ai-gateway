use crate::models::account::Account;
use crate::models::token::TokenData;
use crate::modules::{account, oauth, quota};
use serde::Serialize;
use tracing::info;

#[derive(Serialize)]
pub struct AccountListResponse {
    pub accounts: Vec<Account>,
}

#[tauri::command]
pub async fn list_accounts() -> Result<AccountListResponse, String> {
    let accounts = account::list_accounts()?;
    Ok(AccountListResponse { accounts })
}

#[tauri::command]
pub async fn get_current_account() -> Result<Option<Account>, String> {
    let id = account::get_current_account_id()?;
    match id {
        Some(id) => Ok(Some(account::load_account(&id)?)),
        None => Ok(None),
    }
}

#[tauri::command]
pub async fn add_account(email: String, refresh_token: String) -> Result<Account, String> {
    let token_resp = oauth::refresh_access_token(&refresh_token)
        .await
        .map_err(|e| format!("Invalid refresh token: {}", e))?;

    let actual_email = if email.is_empty() {
        let (user_email, _) = oauth::get_user_info(&token_resp.access_token)
            .await
            .map_err(|e| format!("Failed to get user info: {}", e))?;
        user_email
    } else {
        email
    };

    let token_data = TokenData::new(
        token_resp.access_token,
        refresh_token,
        token_resp.expires_in,
        Some(actual_email.clone()),
        None,
        None,
    );

    let acc = account::add_account(actual_email.clone(), None, token_data)?;
    info!("Account added: {}", actual_email);
    Ok(acc)
}

#[tauri::command]
pub async fn delete_account(account_id: String) -> Result<(), String> {
    account::delete_account(&account_id)
}

#[tauri::command]
pub async fn delete_accounts(account_ids: Vec<String>) -> Result<(), String> {
    account::delete_accounts(&account_ids)
}

#[tauri::command]
pub async fn switch_account(account_id: String) -> Result<(), String> {
    account::set_current_account_id(&account_id)
}

#[tauri::command]
pub async fn fetch_account_quota(account_id: String) -> Result<crate::models::quota::QuotaData, String> {
    let mut acc = account::load_account(&account_id)?;
    // Ensure fresh access token
    let fresh_token = oauth::ensure_fresh_token(&acc.token).await?;
    acc.token = fresh_token;

    let (quota_data, project_id) = quota::fetch_quota(&acc.token.access_token, &acc.email).await?;
    acc.quota = Some(quota_data.clone());
    if let Some(pid) = project_id {
        acc.token.project_id = Some(pid);
    }
    account::save_account(&acc)?;
    Ok(quota_data)
}

#[derive(Serialize)]
pub struct RefreshStats {
    pub total: usize,
    pub success: usize,
    pub failed: usize,
    pub details: Vec<String>,
}

#[tauri::command]
pub async fn refresh_all_quotas() -> Result<RefreshStats, String> {
    let accounts = account::list_accounts()?;
    let total = accounts.len();
    let mut success = 0;
    let mut failed = 0;
    let mut details = Vec::new();

    for acc in &accounts {
        let mut updated = acc.clone();
        match oauth::ensure_fresh_token(&acc.token).await {
            Ok(fresh_token) => {
                updated.token = fresh_token;
                match quota::fetch_quota(&updated.token.access_token, &updated.email).await {
                    Ok((quota_data, project_id)) => {
                        updated.quota = Some(quota_data);
                        if let Some(pid) = project_id {
                            updated.token.project_id = Some(pid);
                        }
                        let _ = account::save_account(&updated);
                        success += 1;
                    }
                    Err(e) => {
                        failed += 1;
                        details.push(format!("{}: {}", acc.email, e));
                    }
                }
            }
            Err(e) => {
                failed += 1;
                details.push(format!("{}: token refresh failed - {}", acc.email, e));
            }
        }
    }

    Ok(RefreshStats { total, success, failed, details })
}

#[tauri::command]
pub async fn start_oauth_login() -> Result<Account, String> {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("Failed to bind local server: {}", e))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to get port: {}", e))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{}", port);
    let state = uuid::Uuid::new_v4().to_string();
    let auth_url = oauth::get_auth_url(&redirect_uri, &state);

    let _ = open::that(&auth_url);
    info!("OAuth: opened browser, redirect to port {}", port);

    let redirect_uri_clone = redirect_uri.clone();
    let (code, _) = tokio::task::spawn_blocking(move || -> Result<(String, String), String> {
        listener.set_nonblocking(false).ok();
        let (mut stream, _) = listener
            .accept()
            .map_err(|e| format!("Failed to accept: {}", e))?;

        let mut buf = [0u8; 4096];
        let n = stream
            .read(&mut buf)
            .map_err(|e| format!("Failed to read: {}", e))?;
        let request = String::from_utf8_lossy(&buf[..n]).to_string();

        let query = request
            .split_whitespace()
            .nth(1)
            .and_then(|path| path.split('?').nth(1))
            .ok_or("No query parameters")?;

        let params: std::collections::HashMap<&str, &str> = query
            .split('&')
            .filter_map(|p| {
                let mut parts = p.splitn(2, '=');
                Some((parts.next()?, parts.next()?))
            })
            .collect();

        let code = params.get("code").ok_or("No code in callback")?.to_string();
        let st = params.get("state").unwrap_or(&"").to_string();

        let html = "<html><body><h2>Login successful! You can close this tab.</h2><script>window.close()</script></body></html>";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
            html.len(),
            html
        );
        let _ = stream.write_all(resp.as_bytes());
        Ok((code, st))
    })
    .await
    .map_err(|e| format!("OAuth task failed: {}", e))??;

    let token_response = oauth::exchange_code(&code, &redirect_uri_clone).await?;
    let refresh_token = token_response
        .refresh_token
        .ok_or("No refresh token received")?;
    let (email, _) = oauth::get_user_info(&token_response.access_token).await?;

    let token_data = TokenData::new(
        token_response.access_token,
        refresh_token,
        token_response.expires_in,
        Some(email.clone()),
        None,
        None,
    );

    let acc = account::add_account(email.clone(), None, token_data)?;
    info!("OAuth login successful: {}", email);
    Ok(acc)
}

#[tauri::command]
pub async fn complete_oauth_login() -> Result<Account, String> {
    Err("OAuth login is handled in start_oauth_login".to_string())
}

#[tauri::command]
pub async fn cancel_oauth_login() -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub async fn reorder_accounts(account_ids: Vec<String>) -> Result<(), String> {
    account::reorder_accounts(&account_ids)
}

#[tauri::command]
pub async fn toggle_proxy_status(account_id: String, enable: bool, reason: Option<String>) -> Result<(), String> {
    let mut acc = account::load_account(&account_id)?;
    acc.proxy_disabled = !enable;
    if let Some(r) = reason {
        acc.disabled_reason = Some(r);
    }
    account::save_account(&acc)
}

#[tauri::command]
pub async fn warm_up_all_accounts() -> Result<String, String> {
    let accounts = account::list_accounts()?;
    let mut results = Vec::new();
    for acc in &accounts {
        match quota::get_valid_token_for_warmup(acc).await {
            Ok((token, pid)) => {
                // Warmup top models
                if let Some(ref q) = acc.quota {
                    for model in q.models.iter().take(3) {
                        let ok = quota::warmup_model_directly(
                            &token, &model.name, &pid, &acc.email, model.percentage, Some(&acc.id),
                        ).await;
                        if ok {
                            results.push(format!("{}/{}: OK", acc.email, model.name));
                        }
                    }
                } else {
                    results.push(format!("{}: no quota data", acc.email));
                }
            }
            Err(e) => results.push(format!("{}: {}", acc.email, e)),
        }
    }
    Ok(results.join("\n"))
}

#[tauri::command]
pub async fn warm_up_account(account_id: String) -> Result<String, String> {
    let acc = account::load_account(&account_id)?;
    let (token, pid) = quota::get_valid_token_for_warmup(&acc).await?;
    let mut results = Vec::new();
    if let Some(ref q) = acc.quota {
        for model in q.models.iter().take(3) {
            let ok = quota::warmup_model_directly(
                &token, &model.name, &pid, &acc.email, model.percentage, Some(&acc.id),
            ).await;
            results.push(if ok { format!("{}: OK", model.name) } else { format!("{}: failed", model.name) });
        }
    }
    Ok(results.join("\n"))
}

#[derive(Serialize)]
pub struct ExportAccountItem {
    pub email: String,
    pub refresh_token: String,
}

#[derive(Serialize)]
pub struct ExportAccountsResponse {
    pub accounts: Vec<ExportAccountItem>,
}

#[tauri::command]
pub async fn export_accounts(account_ids: Vec<String>) -> Result<ExportAccountsResponse, String> {
    let all = account::list_accounts()?;
    let items: Vec<ExportAccountItem> = all
        .into_iter()
        .filter(|a| account_ids.contains(&a.id))
        .map(|a| ExportAccountItem {
            email: a.email,
            refresh_token: a.token.refresh_token,
        })
        .collect();
    Ok(ExportAccountsResponse { accounts: items })
}

#[tauri::command]
pub async fn update_account_label(account_id: String, label: String) -> Result<(), String> {
    let mut acc = account::load_account(&account_id)?;
    acc.custom_label = if label.is_empty() { None } else { Some(label) };
    account::save_account(&acc)
}

#[tauri::command]
pub async fn import_v1_accounts() -> Result<Vec<Account>, String> {
    Err("V1 import not available".to_string())
}

#[tauri::command]
pub async fn import_from_db() -> Result<Account, String> {
    Err("DB import not available".to_string())
}

#[tauri::command]
pub async fn import_custom_db(path: String) -> Result<Account, String> {
    Err(format!("Custom DB import not available: {}", path))
}

#[tauri::command]
pub async fn sync_account_from_db() -> Result<Option<Account>, String> {
    Ok(None)
}

#[tauri::command]
pub async fn read_text_file(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| format!("Failed to read file: {}", e))
}
