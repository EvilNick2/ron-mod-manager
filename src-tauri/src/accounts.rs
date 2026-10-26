#![allow(dead_code)]

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

use crate::server::{HttpServer, ServerError, User};

pub const DEFAULT_SERVER: &str = "https://packs.evilnick.net";

const SERVERS_FILE: &str = "servers.json";
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
const KEYRING_SERVICE: &str = "ron-mod-manager";
const BAD_BASE: &str = "Enter a server address like https://packs.example.com";

static CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Finds the folder `servers.json` lives in, next to `config.json`
pub fn init(app: &AppHandle) {
    if let Some(dir) = crate::config::get_config_path(app).ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        let _ = CONFIG_DIR.set(dir);
    }
}

pub fn normalize_base(input: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(input.trim()).map_err(|_| BAD_BASE.to_string())?;
    let plain = url.path() == "/" && url.query().is_none() && url.fragment().is_none();
    let no_login = url.username().is_empty() && url.password().is_none();
    let allowed = match url.scheme() {
        "https" => url.host_str().is_some_and(|h| !h.is_empty()),
        "http" => url.host_str() == Some("127.0.0.1") && url.port().is_some(),
        _ => false,
    };
    if !(plain && no_login && allowed) {
        return Err(BAD_BASE.to_string());
    }
    Ok(url.origin().ascii_serialization())
}

fn read_servers(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join(SERVERS_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
        .unwrap_or_default()
}

fn servers_in(dir: &Path) -> Vec<String> {
    let mut out = vec![DEFAULT_SERVER.to_string()];
    for base in read_servers(dir) {
        if !out.contains(&base) {
            out.push(base);
        }
    }
    out
}

fn write_servers(dir: &Path, list: &[String]) {
    if let Ok(json) = serde_json::to_string_pretty(list) {
        let _ = fs::create_dir_all(dir);
        let _ = fs::write(dir.join(SERVERS_FILE), json);
    }
}

fn remember_server_in(dir: &Path, base: &str) {
    let mut list = servers_in(dir);
    if !list.iter().any(|b| b == base) {
        list.push(base.to_string());
        write_servers(dir, &list);
    }
}

fn forget_server_in(dir: &Path, base: &str) {
    if base == DEFAULT_SERVER {
        return;
    }
    let mut list = servers_in(dir);
    list.retain(|b| b != base);
    write_servers(dir, &list);
}

pub fn servers() -> Vec<String> {
    match CONFIG_DIR.get() {
        Some(dir) => servers_in(dir),
        None => vec![DEFAULT_SERVER.to_string()],
    }
}

pub fn remember_server(base: &str) {
    if let Some(dir) = CONFIG_DIR.get() {
        remember_server_in(dir, base);
    }
}

pub fn forget_server(base: &str) {
    if let Some(dir) = CONFIG_DIR.get() {
        forget_server_in(dir, base);
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Session {
    pub token: String,
    pub user: User,
}

/// Where sessions are kept, the OS keyring outside tests
trait SessionStore {
    fn load(&self, base: &str) -> Option<String>;
    fn save(&self, base: &str, json: &str) -> Result<(), String>;
    fn clear(&self, base: &str);
}

struct KeyringStore;

impl KeyringStore {
    fn entry(base: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(KEYRING_SERVICE, &format!("pack_server:{}", base)).map_err(|e| e.to_string())
    }
}

impl SessionStore for KeyringStore {
    fn load(&self, base: &str) -> Option<String> {
        Self::entry(base).ok()?.get_password().ok()
    }

    fn save(&self, base: &str, json: &str) -> Result<(), String> {
        Self::entry(base)?.set_password(json).map_err(|e| e.to_string())
    }

    fn clear(&self, base: &str) {
        if let Ok(entry) = Self::entry(base) {
            let _ = entry.delete_password();
        }
    }
}

#[cfg(test)]
#[derive(Default)]
struct MemoryStore(std::sync::Mutex<std::collections::BTreeMap<String, String>>);

#[cfg(test)]
impl SessionStore for MemoryStore {
    fn load(&self, base: &str) -> Option<String> {
        self.0.lock().unwrap().get(base).cloned()
    }

    fn save(&self, base: &str, json: &str) -> Result<(), String> {
        self.0.lock().unwrap().insert(base.to_string(), json.to_string());
        Ok(())
    }

    fn clear(&self, base: &str) {
        self.0.lock().unwrap().remove(base);
    }
}

fn session_in(store: &dyn SessionStore, base: &str) -> Option<Session> {
    serde_json::from_str(&store.load(base)?).ok()
}

fn save_session_in(store: &dyn SessionStore, base: &str, session: &Session) -> Result<(), String> {
    let json = serde_json::to_string(session).map_err(|e| e.to_string())?;
    store.save(base, &json)
}

fn clear_session_in(store: &dyn SessionStore, base: &str) {
    store.clear(base);
}

fn check_in<T>(store: &dyn SessionStore, base: &str, r: Result<T, ServerError>) -> Result<T, ServerError> {
    if matches!(&r, Err(e) if e.code == "signed_out") {
        clear_session_in(store, base);
    }
    r
}

pub fn session(base: &str) -> Option<Session> {
    session_in(&KeyringStore, base)
}

pub fn token(base: &str) -> Option<String> {
    session(base).map(|s| s.token)
}

fn save_session(base: &str, session: &Session) -> Result<(), String> {
    save_session_in(&KeyringStore, base, session)
}

pub fn clear_session(base: &str) {
    clear_session_in(&KeyringStore, base);
}

pub fn client(base: &str) -> HttpServer {
    HttpServer::new(base, token(base))
}

/// Every caller of a server call routes its result through here
pub fn check<T>(base: &str, r: Result<T, ServerError>) -> Result<T, ServerError> {
    check_in(&KeyringStore, base, r)
}

pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn parse_redirect(request_line: &str, state: &str) -> Result<String, String> {
    let target = request_line.split_whitespace().nth(1).ok_or("Sign-in failed: bad request")?;
    let url = reqwest::Url::parse(&format!("http://127.0.0.1{}", target))
        .map_err(|_| "Sign-in failed: bad request".to_string())?;
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
        return Err("Sign-in failed: state mismatch".to_string());
    }
    if let Some(err) = error {
        return Err(if err == "access_denied" {
            "Sign-in was cancelled".to_string()
        } else {
            format!("Sign-in failed: {}", err)
        });
    }
    code.ok_or_else(|| "Sign-in failed: no code".to_string())
}

pub const DONE_PAGE: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>RON Mod Manager</title></head>\
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

pub fn wait_for_code(listener: &TcpListener, state: &str) -> Result<String, String> {
    let deadline = Instant::now() + SIGN_IN_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            return Err("Sign-in timed out".to_string());
        }
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            Err(e) => return Err(format!("Sign-in failed: {}", e)),
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

pub fn sign_in_blocking(app: &AppHandle, base: &str) -> Result<User, String> {
    let base = normalize_base(base)?;
    let verifier = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let state = uuid::Uuid::new_v4().simple().to_string();

    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{}/cb", port);

    let url = reqwest::Url::parse_with_params(
        &format!("{}/auth/start", base),
        &[
            ("redirect_uri", redirect_uri.as_str()),
            ("state", state.as_str()),
            ("code_challenge", pkce_challenge(&verifier).as_str()),
        ],
    )
    .map_err(|e| e.to_string())?;
    app.opener()
        .open_url(url.as_str(), None::<&str>)
        .map_err(|e| format!("Failed to open browser: {}", e))?;

    let code = wait_for_code(&listener, &state)?;
    let (token, user) = HttpServer::new(&base, None)
        .token_exchange(&code, &verifier)
        .map_err(|e| e.text(&base))?;
    save_session(&base, &Session { token, user: user.clone() })?;
    remember_server(&base);
    Ok(user)
}

#[derive(Serialize)]
pub struct ServerStatus {
    pub base: String,
    pub signed_in: bool,
    pub user: Option<User>,
}

#[tauri::command]
pub async fn list_servers() -> Result<Vec<ServerStatus>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        servers()
            .into_iter()
            .map(|base| {
                let user = session(&base).map(|s| s.user);
                ServerStatus { signed_in: user.is_some(), user, base }
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sign_in_server(app: AppHandle, base: String) -> Result<User, String> {
    tauri::async_runtime::spawn_blocking(move || sign_in_blocking(&app, &base))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn sign_out_server(base: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let base = normalize_base(&base)?;
        let _ = client(&base).logout();
        clear_session(&base);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::ServerError;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ron-test-{}-{}", name, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn pkce_matches_rfc_7636() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn parse_redirect_cases() {
        assert_eq!(parse_redirect("GET /cb?state=abc&code=4%2Fxyz HTTP/1.1", "abc"), Ok("4/xyz".to_string()));
        assert_eq!(
            parse_redirect("GET /cb?state=bad&code=x HTTP/1.1", "abc"),
            Err("Sign-in failed: state mismatch".to_string())
        );
        assert_eq!(
            parse_redirect("GET /cb?state=abc&error=access_denied HTTP/1.1", "abc"),
            Err("Sign-in was cancelled".to_string())
        );
        assert_eq!(
            parse_redirect("GET /cb?state=abc&error=server_error HTTP/1.1", "abc"),
            Err("Sign-in failed: server_error".to_string())
        );
        assert!(parse_redirect("GET /favicon.ico HTTP/1.1", "abc").is_err());
    }

    #[test]
    fn normalize_base_cases() {
        assert_eq!(normalize_base("https://Packs.EvilNick.net/").unwrap(), "https://packs.evilnick.net");
        assert_eq!(normalize_base("  HTTPS://packs.test:8443 ").unwrap(), "https://packs.test:8443");
        assert_eq!(normalize_base("http://127.0.0.1:8787/").unwrap(), "http://127.0.0.1:8787");
        for bad in [
            "",
            "packs.test",
            "http://packs.test",
            "http://localhost:8787",
            "ftp://packs.test",
            "https://packs.test/path",
            "https://packs.test/?a=1",
            "https://user@packs.test",
        ] {
            assert!(normalize_base(bad).is_err(), "{}", bad);
        }
    }

    #[test]
    fn default_server_always_listed() {
        let dir = temp("servers-default");
        assert_eq!(servers_in(&dir), vec![DEFAULT_SERVER.to_string()]);
        std::fs::write(dir.join(SERVERS_FILE), "[]").unwrap();
        assert_eq!(servers_in(&dir), vec![DEFAULT_SERVER.to_string()]);
        std::fs::write(dir.join(SERVERS_FILE), "not json").unwrap();
        assert_eq!(servers_in(&dir), vec![DEFAULT_SERVER.to_string()]);
        std::fs::write(dir.join(SERVERS_FILE), format!("[\"https://a.test\", \"{}\"]", DEFAULT_SERVER)).unwrap();
        assert_eq!(servers_in(&dir), vec![DEFAULT_SERVER.to_string(), "https://a.test".to_string()]);
    }

    #[test]
    fn remember_and_forget_servers() {
        let dir = temp("servers-remember");
        remember_server_in(&dir, "https://a.test");
        remember_server_in(&dir, "https://a.test");
        remember_server_in(&dir, DEFAULT_SERVER);
        assert_eq!(servers_in(&dir), vec![DEFAULT_SERVER.to_string(), "https://a.test".to_string()]);
        forget_server_in(&dir, DEFAULT_SERVER);
        assert_eq!(servers_in(&dir).len(), 2);
        forget_server_in(&dir, "https://a.test");
        assert_eq!(servers_in(&dir), vec![DEFAULT_SERVER.to_string()]);
    }

    fn sample() -> Session {
        Session {
            token: "tok".into(),
            user: crate::server::User { id: "1".into(), name: "Nick".into(), role: None },
        }
    }

    #[test]
    fn session_round_trips_through_the_store() {
        let store = MemoryStore::default();
        assert_eq!(session_in(&store, "https://a.test"), None);
        save_session_in(&store, "https://a.test", &sample()).unwrap();
        assert_eq!(session_in(&store, "https://a.test"), Some(sample()));
        assert_eq!(session_in(&store, "https://b.test"), None);
        clear_session_in(&store, "https://a.test");
        assert_eq!(session_in(&store, "https://a.test"), None);
    }

    #[test]
    fn signed_out_clears_session() {
        let store = MemoryStore::default();
        save_session_in(&store, "https://a.test", &sample()).unwrap();
        let ok: Result<u32, ServerError> = check_in(&store, "https://a.test", Ok(1));
        assert_eq!(ok, Ok(1));
        assert!(session_in(&store, "https://a.test").is_some());

        let other: Result<u32, ServerError> =
            check_in(&store, "https://a.test", Err(ServerError::new("unavailable", "down")));
        assert_eq!(other.unwrap_err().code, "unavailable");
        assert!(session_in(&store, "https://a.test").is_some());

        let out: Result<u32, ServerError> =
            check_in(&store, "https://a.test", Err(ServerError::new("signed_out", "Sign in again")));
        assert_eq!(out.unwrap_err().code, "signed_out");
        assert_eq!(session_in(&store, "https://a.test"), None);
    }
}
