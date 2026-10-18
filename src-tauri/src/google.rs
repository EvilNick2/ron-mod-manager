use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri_plugin_opener::OpenerExt;

pub const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const ABOUT_ENDPOINT: &str = "https://www.googleapis.com/drive/v3/about?fields=user(emailAddress)";
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

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

pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
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

pub fn parse_redirect(request_line: &str, state: &str) -> Result<String, String> {
    let target = request_line
        .split_whitespace()
        .nth(1)
        .ok_or("Google sign-in failed: bad request")?;
    let url = reqwest::Url::parse(&format!("http://127.0.0.1{}", target))
        .map_err(|_| "Google sign-in failed: bad request".to_string())?;
    let mut code = None;
    let mut error = None;
    let mut got_state = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            "state" => got_state = Some(value.into_owned()),
            _ => {}
        }
    }
    if got_state.as_deref() != Some(state) {
        return Err("Google sign-in failed: state mismatch".to_string());
    }
    if let Some(err) = error {
        return Err(if err == "access_denied" {
            "Google sign-in was cancelled".to_string()
        } else {
            format!("Google sign-in failed: {}", err)
        });
    }
    code.ok_or_else(|| "Google sign-in failed: no code".to_string())
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

const DONE_PAGE: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>RON Mod Manager</title></head>\
<body style=\"font-family:sans-serif;text-align:center;margin-top:20vh\">\
<p>Signed in. You can close this tab and return to RON Mod Manager.</p></body></html>";

fn respond(stream: &mut std::net::TcpStream, status: &str, content_type: &str, body: &str) {
    let msg = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        content_type,
        body.len(),
        body
    );
    let _ = stream.write_all(msg.as_bytes());
}

fn wait_for_code(listener: &TcpListener, state: &str) -> Result<String, String> {
    let deadline = Instant::now() + SIGN_IN_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            return Err("Google sign-in timed out".to_string());
        }
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            Err(e) => return Err(format!("Google sign-in failed: {}", e)),
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]);
        let line = request.lines().next().unwrap_or("");
        let request_state = reqwest::Url::parse(&format!(
            "http://127.0.0.1{}",
            line.split_whitespace().nth(1).unwrap_or("/")
        ))
        .ok()
        .and_then(|u| u.query_pairs().find(|(k, _)| k == "state").map(|(_, v)| v.into_owned()));
        if request_state.as_deref() != Some(state) {
            respond(&mut stream, "400 Bad Request", "text/plain", "Unexpected request");
            continue;
        }
        let result = parse_redirect(line, state);
        match &result {
            Ok(_) => respond(&mut stream, "200 OK", "text/html", DONE_PAGE),
            Err(msg) => respond(&mut stream, "400 Bad Request", "text/plain", msg),
        }
        return result;
    }
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
    fn pkce_known_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mJ92K1D_wTA3d9X5uVZr4wq7F3yrmY"),
            "VBYUZCbEyZD9Id7KNltEF7YRgS-qtnrCHU9DTw-d3wc"
        );
    }

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

    #[test]
    fn parse_redirect_cases() {
        assert_eq!(
            parse_redirect("GET /?state=abc&code=4%2Fxyz HTTP/1.1", "abc"),
            Ok("4/xyz".to_string())
        );
        assert_eq!(
            parse_redirect("GET /?state=bad&code=x HTTP/1.1", "abc"),
            Err("Google sign-in failed: state mismatch".to_string())
        );
        assert_eq!(
            parse_redirect("GET /?state=abc&error=access_denied HTTP/1.1", "abc"),
            Err("Google sign-in was cancelled".to_string())
        );
        assert_eq!(
            parse_redirect("GET /?state=abc&error=server_error HTTP/1.1", "abc"),
            Err("Google sign-in failed: server_error".to_string())
        );
        assert!(parse_redirect("GET /favicon.ico HTTP/1.1", "abc").is_err());
    }
}
