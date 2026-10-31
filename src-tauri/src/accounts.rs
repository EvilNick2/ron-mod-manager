use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

use crate::server::{HttpServer, Me, PackServer, ServerError, User};

pub const DEFAULT_SERVER: &str = "https://packs.evilnick.net";

const SERVERS_FILE: &str = "servers.json";
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
const KEYRING_SERVICE: &str = "ron-mod-manager";
const BAD_BASE: &str = "Enter a server address like https://packs.example.com";
/// The error a sign-in stopped by the user ends with, which the UI does not alert
pub const SIGN_IN_CANCELLED: &str = "Sign-in was cancelled";

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

// Written to a temp file and renamed over the old one, so a failed write never leaves half a list
fn write_servers(dir: &Path, list: &[String]) -> Result<(), String> {
    let path = dir.join(SERVERS_FILE);
    let fail = |e: std::io::Error| format!("Could not save {}: {}", path.display(), e);
    let json = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    fs::create_dir_all(dir).map_err(fail)?;
    let temp = dir.join(format!(".{}.{}.tmp", SERVERS_FILE, uuid::Uuid::new_v4().simple()));
    let written = fs::write(&temp, json).and_then(|_| fs::rename(&temp, &path));
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(fail(e));
    }
    Ok(())
}

fn remember_server_in(dir: &Path, base: &str) -> Result<(), String> {
    let mut list = servers_in(dir);
    if list.iter().any(|b| b == base) {
        return Ok(());
    }
    list.push(base.to_string());
    write_servers(dir, &list)
}

fn forget_server_in(dir: &Path, base: &str) -> Result<(), String> {
    if base == DEFAULT_SERVER {
        return Ok(());
    }
    let mut list = servers_in(dir);
    list.retain(|b| b != base);
    write_servers(dir, &list)
}

// Servers that installed or published packs use, so a signed out one keeps its Settings row
pub(crate) fn pack_servers(mods_dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = crate::vfs::child_dirs(mods_dir)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|name| crate::packs::read_manifest(&mods_dir.join(name)))
        .flat_map(|m| m.source.and_then(|s| s.server).into_iter().chain(m.published.and_then(|p| p.server)))
        .filter_map(|base| normalize_base(&base).ok())
        .collect();
    out.sort();
    out
}

fn listed_in(dir: &Path, mods_dir: Option<&Path>) -> Vec<String> {
    let mut out = servers_in(dir);
    for base in mods_dir.map(pack_servers).unwrap_or_default() {
        if !out.contains(&base) {
            out.push(base);
        }
    }
    out
}

/// Remembered servers, then the servers of the packs in mods_dir
pub fn servers(mods_dir: Option<&Path>) -> Vec<String> {
    match CONFIG_DIR.get() {
        Some(dir) => listed_in(dir, mods_dir),
        None => vec![DEFAULT_SERVER.to_string()],
    }
}

/// Logs a failure, since its callers have already done what the user asked
pub fn remember_server(base: &str) {
    if let Some(dir) = CONFIG_DIR.get() {
        if let Err(e) = remember_server_in(dir, base) {
            eprintln!("[ACCOUNTS] Could not remember {}: {}", base, e);
        }
    }
}

pub fn forget_server(base: &str) -> Result<(), String> {
    match CONFIG_DIR.get() {
        Some(dir) => forget_server_in(dir, base),
        None => Ok(()),
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Session {
    pub token: String,
    pub user: User,
}

/// Where sessions are kept, the OS keyring outside tests
pub trait SessionStore {
    fn load(&self, base: &str) -> Option<String>;
    fn save(&self, base: &str, json: &str) -> Result<(), String>;
    fn clear(&self, base: &str);
}

pub struct KeyringStore;

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
pub struct MemoryStore(std::sync::Mutex<std::collections::BTreeMap<String, String>>);

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

pub fn session_in(store: &dyn SessionStore, base: &str) -> Option<Session> {
    serde_json::from_str(&store.load(base)?).ok()
}

pub fn save_session_in(store: &dyn SessionStore, base: &str, session: &Session) -> Result<(), String> {
    let json = serde_json::to_string(session).map_err(|e| e.to_string())?;
    store.save(base, &json)
}

fn clear_session_in(store: &dyn SessionStore, base: &str) {
    store.clear(base);
}

// Clears the session on signed_out, unless it was replaced by a sign-in after `token` was read
fn check_in<T>(store: &dyn SessionStore, base: &str, token: Option<&str>, r: Result<T, ServerError>) -> Result<T, ServerError> {
    if matches!(&r, Err(e) if e.code == "signed_out") && session_in(store, base).map(|s| s.token).as_deref() == token {
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

/// A server with the session store its results are checked against
pub struct Conn {
    server: Box<dyn PackServer>,
    store: Arc<dyn SessionStore>,
    /// The session token when the Conn was made, so a later sign-in is not overwritten
    token: Option<String>,
}

impl Conn {
    pub fn live(base: &str) -> Conn {
        let token = token(base);
        Conn { server: Box::new(HttpServer::new(base, token.clone())), store: Arc::new(KeyringStore), token }
    }

    #[cfg(test)]
    pub fn new(server: impl PackServer + 'static, store: Arc<dyn SessionStore>) -> Conn {
        let token = session_in(&*store, server.base()).map(|s| s.token);
        Conn { server: Box::new(server), store, token }
    }

    pub fn server(&self) -> &dyn PackServer {
        &*self.server
    }

    pub fn base(&self) -> &str {
        self.server.base()
    }

    pub fn signed_in(&self) -> bool {
        session_in(&*self.store, self.base()).is_some()
    }

    pub fn call<T>(&self, r: Result<T, ServerError>) -> Result<T, ServerError> {
        check_in(&*self.store, self.base(), self.token.as_deref(), r)
    }

    /// Saves the user `me` names over the stored one, keeping the token, so role changes reach the app.
    /// Skipped when the stored session is not the one this Conn was made with.
    pub fn update_user(&self, me: &Me) {
        let Some(mut session) = session_in(&*self.store, self.base()) else { return };
        if self.token.as_deref() != Some(session.token.as_str()) {
            return;
        }
        let user = User { id: me.id.clone(), name: me.name.clone(), role: me.role.clone() };
        if session.user == user {
            return;
        }
        session.user = user;
        if let Err(e) = save_session_in(&*self.store, self.base(), &session) {
            eprintln!("[ACCOUNTS] Could not save the session for {}: {}", self.base(), e);
        }
    }

    /// `call`, with the error as the user sentence naming this server
    pub fn text<T>(&self, r: Result<T, ServerError>) -> Result<T, String> {
        self.call(r).map_err(|e| e.text(self.base()))
    }
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

pub fn wait_for_code(listener: &TcpListener, state: &str, cancel: &dyn Fn() -> bool) -> Result<String, String> {
    let deadline = Instant::now() + SIGN_IN_TIMEOUT;
    loop {
        if cancel() {
            return Err(SIGN_IN_CANCELLED.to_string());
        }
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

// The cancel flag of each sign-in waiting for its browser redirect, keyed by base
#[derive(Default)]
struct SignIns {
    running: BTreeMap<String, Arc<AtomicBool>>,
    /// A cancel that came before its sign-in started, as when the blocking task is slow to start
    early: BTreeMap<String, Instant>,
}

static SIGN_INS: Mutex<SignIns> = Mutex::new(SignIns { running: BTreeMap::new(), early: BTreeMap::new() });

// An early cancel only counts for a sign-in that starts this soon after it, so a stray one cannot stop a later sign-in
const EARLY_CANCEL: Duration = Duration::from_secs(10);

fn sign_ins() -> std::sync::MutexGuard<'static, SignIns> {
    SIGN_INS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Registers a sign-in's cancel flag until dropped
struct SignIn {
    base: String,
    flag: Arc<AtomicBool>,
}

impl SignIn {
    fn begin(base: &str) -> SignIn {
        let mut map = sign_ins();
        let early = map.early.remove(base).is_some_and(|at| at.elapsed() < EARLY_CANCEL);
        let flag = Arc::new(AtomicBool::new(early));
        map.running.insert(base.to_string(), flag.clone());
        SignIn { base: base.to_string(), flag }
    }

    fn cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

impl Drop for SignIn {
    fn drop(&mut self) {
        let mut map = sign_ins();
        if map.running.get(&self.base).is_some_and(|f| Arc::ptr_eq(f, &self.flag)) {
            map.running.remove(&self.base);
        }
    }
}

/// False when no sign-in to base is running yet, in which case the next one to start soon is cancelled
fn cancel_sign_in_for(base: &str) -> bool {
    let mut map = sign_ins();
    match map.running.get(base) {
        Some(flag) => {
            flag.store(true, Ordering::SeqCst);
            true
        }
        None => {
            map.early.insert(base.to_string(), Instant::now());
            false
        }
    }
}

pub fn sign_in_blocking(app: &AppHandle, base: &str) -> Result<User, String> {
    let base = normalize_base(base)?;
    let running = SignIn::begin(&base);
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

    let code = wait_for_code(&listener, &state, &|| running.cancelled())?;
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
pub async fn list_servers(app_handle: AppHandle) -> Result<Vec<ServerStatus>, String> {
    let mods_dir = crate::vfs::get_mods_dir(&app_handle).ok();
    tauri::async_runtime::spawn_blocking(move || {
        servers(mods_dir.as_deref())
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

/// Stops a sign-in waiting for its browser redirect. It then fails with SIGN_IN_CANCELLED.
#[tauri::command]
pub fn cancel_sign_in(base: String) -> Result<(), String> {
    cancel_sign_in_for(&normalize_base(&base)?);
    Ok(())
}

#[tauri::command]
pub async fn sign_out_server(base: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let base = normalize_base(&base)?;
        let _ = client(&base).logout();
        clear_session(&base);
        forget_server(&base)
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
        remember_server_in(&dir, "https://a.test").unwrap();
        remember_server_in(&dir, "https://a.test").unwrap();
        remember_server_in(&dir, DEFAULT_SERVER).unwrap();
        assert_eq!(servers_in(&dir), vec![DEFAULT_SERVER.to_string(), "https://a.test".to_string()]);
        forget_server_in(&dir, DEFAULT_SERVER).unwrap();
        assert_eq!(servers_in(&dir).len(), 2);
        forget_server_in(&dir, "https://a.test").unwrap();
        assert_eq!(servers_in(&dir), vec![DEFAULT_SERVER.to_string()]);
    }

    #[test]
    fn servers_used_by_packs_stay_listed() {
        use crate::packs::{self, PackManifest, PackSource, Published};
        let dir = temp("servers-packs");
        let mods = temp("servers-packs-mods");
        let put = |id: &str, source: Option<PackSource>, published: Option<Published>| {
            let pack_dir = mods.join(id);
            std::fs::create_dir_all(&pack_dir).unwrap();
            let man = PackManifest { pack_id: id.into(), name: id.into(), source, published, ..Default::default() };
            packs::write_manifest(&pack_dir, &man).unwrap();
        };
        let server = |base: &str| Some(PackSource { kind: "server".into(), server: Some(base.into()), ..Default::default() });
        put("Viewed", server("https://viewed.test"), None);
        put("Mine", None, Some(Published { server: Some("https://mine.test".into()), dirty: false }));
        put("Drive", Some(PackSource { kind: "drive".into(), folder_id: Some("F".into()), ..Default::default() }), None);
        put("Default", server(DEFAULT_SERVER), None);
        remember_server_in(&dir, "https://a.test").unwrap();
        assert_eq!(
            listed_in(&dir, Some(&mods)),
            vec![DEFAULT_SERVER.to_string(), "https://a.test".to_string(), "https://mine.test".to_string(), "https://viewed.test".to_string()]
        );
        assert_eq!(listed_in(&dir, None), vec![DEFAULT_SERVER.to_string(), "https://a.test".to_string()]);
        // Listing never remembers a pack's server, so forgetting one still drops it from servers.json
        assert_eq!(servers_in(&dir), vec![DEFAULT_SERVER.to_string(), "https://a.test".to_string()]);
    }

    #[test]
    fn write_servers_replaces_through_a_temp_file() {
        let dir = temp("servers-write");
        let list = vec![DEFAULT_SERVER.to_string(), "https://a.test".to_string()];
        write_servers(&dir, &list).unwrap();
        assert_eq!(read_servers(&dir), list);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);

        // A folder in the file's place makes the write fail and is left alone
        let blocked = temp("servers-blocked");
        std::fs::create_dir(blocked.join(SERVERS_FILE)).unwrap();
        assert!(write_servers(&blocked, &list).is_err());
        assert!(remember_server_in(&blocked, "https://b.test").is_err());
        assert!(forget_server_in(&blocked, "https://b.test").is_err());
        assert!(blocked.join(SERVERS_FILE).is_dir());
        assert_eq!(std::fs::read_dir(&blocked).unwrap().count(), 1);
    }

    #[test]
    fn wait_for_code_stops_when_cancelled() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let started = Instant::now();
        assert_eq!(wait_for_code(&listener, "abc", &|| true), Err(SIGN_IN_CANCELLED.to_string()));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn cancel_sign_in_reaches_a_running_sign_in() {
        let base = "https://cancel.test";
        let running = SignIn::begin(base);
        assert!(!running.cancelled());
        assert!(cancel_sign_in_for(base));
        assert!(running.cancelled());
        drop(running);
        // A new sign-in starts with a fresh flag
        assert!(!SignIn::begin(base).cancelled());
    }

    #[test]
    fn cancel_before_the_sign_in_starts_is_kept_for_it() {
        let base = "https://early-cancel.test";
        assert!(!cancel_sign_in_for(base));
        let first = SignIn::begin(base);
        assert!(first.cancelled());
        drop(first);
        // Used up by that sign-in, so the next one is not cancelled
        assert!(!SignIn::begin(base).cancelled());
    }

    #[test]
    fn signed_out_skips_a_session_replaced_since_the_conn_was_made() {
        use crate::server::fake::{FakeServer, BASE};
        let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::default());
        save_session_in(&*store, BASE, &sample()).unwrap();
        let conn = Conn::new(FakeServer::new(), store.clone());
        let relogin = Session { token: "tok2".into(), user: sample().user };
        save_session_in(&*store, BASE, &relogin).unwrap();
        let out: Result<(), ServerError> = conn.call(Err(ServerError::new("signed_out", "Sign in again")));
        assert_eq!(out.unwrap_err().code, "signed_out");
        assert_eq!(session_in(&*store, BASE), Some(relogin));

        let fresh = Conn::new(FakeServer::new(), store.clone());
        let _ = fresh.call::<()>(Err(ServerError::new("signed_out", "Sign in again")));
        assert_eq!(session_in(&*store, BASE), None);
    }

    #[test]
    fn update_user_skips_a_session_replaced_since_the_conn_was_made() {
        use crate::server::fake::{FakeServer, BASE};
        let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::default());
        save_session_in(&*store, BASE, &sample()).unwrap();
        let conn = Conn::new(FakeServer::new(), store.clone());
        let me = Me { id: "1".into(), name: "Nick".into(), role: Some("uploader".into()), packs: vec![] };

        let relogin = Session { token: "tok2".into(), user: User { id: "2".into(), name: "Other".into(), role: None } };
        save_session_in(&*store, BASE, &relogin).unwrap();
        conn.update_user(&me);
        assert_eq!(session_in(&*store, BASE), Some(relogin));

        let fresh = Conn::new(FakeServer::new(), store.clone());
        let me2 = Me { id: "2".into(), name: "Other".into(), role: Some("owner".into()), packs: vec![] };
        fresh.update_user(&me2);
        assert_eq!(session_in(&*store, BASE).unwrap().user.role.as_deref(), Some("owner"));
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
        let ok: Result<u32, ServerError> = check_in(&store, "https://a.test", Some("tok"), Ok(1));
        assert_eq!(ok, Ok(1));
        assert!(session_in(&store, "https://a.test").is_some());

        let other: Result<u32, ServerError> =
            check_in(&store, "https://a.test", Some("tok"), Err(ServerError::new("unavailable", "down")));
        assert_eq!(other.unwrap_err().code, "unavailable");
        assert!(session_in(&store, "https://a.test").is_some());

        let out: Result<u32, ServerError> =
            check_in(&store, "https://a.test", Some("tok"), Err(ServerError::new("signed_out", "Sign in again")));
        assert_eq!(out.unwrap_err().code, "signed_out");
        assert_eq!(session_in(&store, "https://a.test"), None);
    }
}
