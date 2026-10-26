use crate::accounts::{pkce_challenge, wait_for_code};
use serde::{Deserialize, Serialize};
use std::net::TcpListener;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri_plugin_opener::OpenerExt;

pub const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const ABOUT_ENDPOINT: &str = "https://www.googleapis.com/drive/v3/about?fields=user(emailAddress)";

fn non_empty(value: Option<&'static str>) -> Option<&'static str> {
    value.filter(|v| !v.is_empty())
}

pub fn client_id() -> Option<&'static str> {
    non_empty(option_env!("GOOGLE_CLIENT_ID"))
}

pub fn client_secret() -> Option<&'static str> {
    non_empty(option_env!("GOOGLE_CLIENT_SECRET"))
}

pub fn api_key() -> Option<&'static str> {
    non_empty(option_env!("GOOGLE_API_KEY"))
}

pub fn auth_url(client_id: &str, redirect_uri: &str, challenge: &str, state: &str) -> String {
    reqwest::Url::parse_with_params(
        AUTH_ENDPOINT,
        &[
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
            ("response_type", "code"),
            ("scope", SCOPE),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("access_type", "offline"),
            ("prompt", "consent"),
        ],
    )
    .expect("auth endpoint is a valid url")
    .to_string()
}

#[derive(Serialize, Deserialize)]
struct StoredAuth {
    refresh_token: String,
    email: String,
}

static TOKEN_CACHE: Mutex<Option<(String, Instant)>> = Mutex::new(None);

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    error: Option<String>,
}

fn auth_entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new("ron-mod-manager", "google_auth").map_err(|e| e.to_string())
}

fn load_stored() -> Result<Option<StoredAuth>, String> {
    match auth_entry()?.get_password() {
        Ok(json) => Ok(serde_json::from_str(&json).ok()),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn save_stored(auth: &StoredAuth) -> Result<(), String> {
    let json = serde_json::to_string(auth).map_err(|e| e.to_string())?;
    auth_entry()?.set_password(&json).map_err(|e| e.to_string())
}

fn clear_stored() -> Result<(), String> {
    match auth_entry()?.delete_password() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

fn clear_cache() {
    *TOKEN_CACHE.lock().unwrap() = None;
}

#[allow(dead_code)]
pub fn access_token() -> Result<String, String> {
    {
        let cache = TOKEN_CACHE.lock().unwrap();
        if let Some((token, expiry)) = cache.as_ref() {
            if Instant::now() + Duration::from_secs(60) < *expiry {
                return Ok(token.clone());
            }
        }
    }
    let stored = load_stored()?.ok_or("Sign in with Google in Settings first")?;
    let (id, secret) = client_pair()?;
    let resp: TokenResponse = reqwest::blocking::Client::new()
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", stored.refresh_token.as_str()),
            ("client_id", id),
            ("client_secret", secret),
        ])
        .send()
        .and_then(|r| r.json())
        .map_err(|e| format!("Google token refresh failed: {}", e))?;
    if let Some(err) = resp.error {
        if err == "invalid_grant" {
            let _ = clear_stored();
            clear_cache();
            return Err("Your Google sign-in expired. Sign in again in Settings".to_string());
        }
        return Err(format!("Google token refresh failed: {}", err));
    }
    let token = resp.access_token.ok_or("Google token refresh failed: no access token")?;
    let expiry = Instant::now() + Duration::from_secs(resp.expires_in.unwrap_or(3600));
    *TOKEN_CACHE.lock().unwrap() = Some((token.clone(), expiry));
    Ok(token)
}

fn client_pair() -> Result<(&'static str, &'static str), String> {
    match (client_id(), client_secret()) {
        (Some(id), Some(secret)) => Ok((id, secret)),
        _ => Err("Google sign-in is not available in this build".to_string()),
    }
}

#[derive(Serialize)]
pub struct GoogleStatus {
    pub sign_in_available: bool,
    pub links_available: bool,
    pub email: Option<String>,
}

fn sign_in_blocking(app_handle: tauri::AppHandle) -> Result<String, String> {
    let (id, secret) = client_pair()?;
    let verifier = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let state = uuid::Uuid::new_v4().simple().to_string();

    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{}", port);

    let url = auth_url(id, &redirect_uri, &pkce_challenge(&verifier), &state);
    app_handle
        .opener()
        .open_url(&url, None::<&str>)
        .map_err(|e| format!("Failed to open browser: {}", e))?;

    let code = wait_for_code(&listener, &state)?;

    let client = reqwest::blocking::Client::new();
    let resp: TokenResponse = client
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("code_verifier", verifier.as_str()),
            ("client_id", id),
            ("client_secret", secret),
            ("redirect_uri", redirect_uri.as_str()),
        ])
        .send()
        .and_then(|r| r.json())
        .map_err(|e| format!("Google sign-in failed: {}", e))?;
    if let Some(err) = resp.error {
        return Err(format!("Google sign-in failed: {}", err));
    }
    let access = resp.access_token.ok_or("Google sign-in failed: no access token")?;
    let refresh = resp.refresh_token.ok_or("Google sign-in failed: no refresh token")?;

    let about: serde_json::Value = client
        .get(ABOUT_ENDPOINT)
        .bearer_auth(&access)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json())
        .map_err(|e| format!("Google sign-in failed: {}", e))?;
    let email = about["user"]["emailAddress"]
        .as_str()
        .ok_or("Google sign-in failed: no email address")?
        .to_string();

    save_stored(&StoredAuth { refresh_token: refresh, email: email.clone() })?;
    let expiry = Instant::now() + Duration::from_secs(resp.expires_in.unwrap_or(3600));
    *TOKEN_CACHE.lock().unwrap() = Some((access, expiry));
    Ok(email)
}

#[tauri::command]
pub async fn google_status() -> Result<GoogleStatus, String> {
    let email = load_stored().ok().flatten().map(|a| a.email);
    Ok(GoogleStatus {
        sign_in_available: client_id().is_some() && client_secret().is_some() && api_key().is_some(),
        links_available: api_key().is_some(),
        email,
    })
}

#[tauri::command]
pub async fn google_sign_in(app_handle: tauri::AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || sign_in_blocking(app_handle))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn google_sign_out() -> Result<(), String> {
    clear_cache();
    tauri::async_runtime::spawn_blocking(clear_stored)
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_url_carries_pkce_and_scope() {
        let url = auth_url("cid", "http://127.0.0.1:5000", "chal", "st");
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000"));
        assert!(url.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive.file"));
    }
}
