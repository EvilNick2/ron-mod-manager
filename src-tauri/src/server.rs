use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use base64::Engine;
use reqwest::blocking::{Body, Client, RequestBuilder, Response};
use reqwest::header::{RANGE, RETRY_AFTER};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::hashing;

pub const FORMAT: u32 = 2;
pub const SIZE_MISMATCH: &str = "The download did not match its expected size";
pub const CANCELLED: &str = "Cancelled";

const MAX_ATTEMPTS: usize = 5;
const NOT_INVITE: &str = "That is not a pack invite link.";

#[derive(Clone, Debug, PartialEq)]
pub struct ServerError {
    pub code: String,
    pub message: String,
    pub retry_after: Option<u64>,
}

impl ServerError {
    pub fn new(code: &str, message: &str) -> ServerError {
        ServerError { code: code.to_string(), message: message.to_string(), retry_after: None }
    }

    /// The user sentence, naming `base` where the code needs it
    pub fn text(&self, base: &str) -> String {
        describe(&self.code, &self.message, base)
    }
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&describe(&self.code, &self.message, ""))
    }
}

/// The host of `base`, with its port when that is not the scheme's default
fn host_of(base: &str) -> String {
    let Ok(url) = reqwest::Url::parse(base) else { return base.to_string() };
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{}:{}", host, port),
        (Some(host), None) => host.to_string(),
        (None, _) => base.to_string(),
    }
}

/// One URL path segment, with everything but unreserved characters percent-encoded
fn seg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

pub fn describe(code: &str, message: &str, base: &str) -> String {
    let host = host_of(base);
    let host = if host.is_empty() { "the pack server" } else { host.as_str() };
    match code {
        "signed_out" => format!("Signed out of {}. Sign in again in Settings.", host),
        "unavailable" => "The pack server is not answering. Its daily limit may be reached, try again after 00:00 UTC.".into(),
        "rate_limited" => "The pack server asked to slow down. Try again in a minute.".into(),
        "pack_gone" => "This pack is no longer available to you. It was removed from the server or you were removed from it.".into(),
        "invite_gone" => "This invite link has expired or was reset. Ask for a new one.".into(),
        "server_full" => "This pack server is full.".into(),
        "storage_full" => "The pack server is out of space.".into(),
        "op_budget" => "The pack server's monthly upload budget is used up.".into(),
        "pack_taken" => "Someone else already has a pack with this id on the server. Rename the pack, or ask its creator to add you as an editor.".into(),
        "pack_behind" => "A newer version of this pack is on the server. Update to it, make your changes again and publish.".into(),
        "blob_gone" => "The pack changed while downloading. Try again.".into(),
        "publish_cleared" => "The pack's creator stopped this publish.".into(),
        _ => message.to_string(),
    }
}

fn valid_code(code: &str) -> bool {
    !code.is_empty() && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The server base, normalized as sessions are keyed, and the invite code
pub fn parse_invite(link: &str) -> Result<(String, String), String> {
    let link = link.trim();
    let link = link.strip_suffix('/').unwrap_or(link);
    let (base, code) = link.rsplit_once("/i/").ok_or(NOT_INVITE)?;
    if !valid_code(code) {
        return Err(NOT_INVITE.to_string());
    }
    let base = crate::accounts::normalize_base(base).map_err(|_| NOT_INVITE.to_string())?;
    Ok((base, code.to_string()))
}

/// The `x-amz-checksum-sha256` value, base64 of the raw hash bytes
pub fn checksum_b64(sha256_hex: &str) -> String {
    let raw: Option<Vec<u8>> = (0..sha256_hex.len())
        .step_by(2)
        .map(|i| sha256_hex.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()))
        .collect();
    base64::engine::general_purpose::STANDARD.encode(raw.unwrap_or_default())
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct User {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MePack {
    pub id: String,
    pub name: String,
    pub version: String,
    pub uploader: User,
    pub access: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Me {
    pub id: String,
    pub name: String,
    pub role: Option<String>,
    pub packs: Vec<MePack>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct InvitePack {
    pub id: String,
    pub name: String,
    pub version: String,
    pub uploader: User,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct InviteInfo {
    pub kind: String,
    pub expires_at: u64,
    pub pack: Option<InvitePack>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Accepted {
    pub kind: String,
    pub pack_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Begun {
    pub publish_id: String,
    pub missing: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Verified {
    pub verified: Vec<String>,
    pub bad: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Link {
    pub url: String,
    pub expires_at: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Viewer {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AdminUser {
    pub id: String,
    pub name: String,
    pub role: Option<String>,
    pub packs: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RemovedViewer {
    pub invite_reset: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct VersionEntry {
    pub version: String,
    pub published_at: u64,
    pub notes: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RemoteManifest {
    pub format: u32,
    pub pack_id: String,
    pub name: String,
    pub version: String,
    pub status: String,
    pub published_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_by: Option<String>,
    pub mods: Vec<RemoteMod>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RemoteMod {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub hash: String,
    pub addons: BTreeMap<String, bool>,
    pub files: Vec<RemoteFileEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RemoteFileEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

pub trait PackServer: Send + Sync {
    fn base(&self) -> &str;
    fn me(&self) -> Result<Me, ServerError>;
    fn invite_info(&self, code: &str) -> Result<InviteInfo, ServerError>;
    fn accept_invite(&self, code: &str) -> Result<Accepted, ServerError>;
    fn manifest(&self, pack_id: &str) -> Result<RemoteManifest, ServerError>;
    /// Fails with SIZE_MISMATCH, leaving no file, when the content is not exactly expected_size bytes
    fn download(
        &self,
        sha256: &str,
        dest: &Path,
        expected_size: u64,
        cancel: &dyn Fn() -> bool,
        on_bytes: &mut dyn FnMut(u64),
    ) -> Result<String, ServerError>;
    fn begin(&self, pack_id: &str, name: &str, version: &str, base: Option<&str>, blobs: &[(String, u64)]) -> Result<Begun, ServerError>;
    fn upload_urls(&self, publish_id: &str, hashes: &[String]) -> Result<BTreeMap<String, String>, ServerError>;
    fn put_blob(&self, url: &str, src: &Path, size: u64, sha256: &str, on_bytes: &mut dyn FnMut(u64)) -> Result<(), ServerError>;
    fn verify(&self, publish_id: &str, hashes: &[String]) -> Result<Verified, ServerError>;
    /// `history` is oldest first and only seeds a pack that has no versions yet. Gives the time the
    /// server stored, None from a server too old to send it.
    fn commit(&self, publish_id: &str, manifest: &RemoteManifest, notes: &str, history: &[VersionEntry]) -> Result<Option<u64>, ServerError>;
    /// Newest first
    fn changelog(&self, pack_id: &str) -> Result<Vec<VersionEntry>, ServerError>;
    fn cancel(&self, publish_id: &str) -> Result<(), ServerError>;
    fn pack_invite(&self, pack_id: &str) -> Result<Link, ServerError>;
    fn reset_invite(&self, pack_id: &str) -> Result<Link, ServerError>;
    fn viewers(&self, pack_id: &str) -> Result<Vec<Viewer>, ServerError>;
    fn remove_viewer(&self, pack_id: &str, user_id: &str) -> Result<RemovedViewer, ServerError>;
    fn editor_invite(&self, pack_id: &str) -> Result<Link, ServerError>;
    fn editors(&self, pack_id: &str) -> Result<Vec<Viewer>, ServerError>;
    fn remove_editor(&self, pack_id: &str, user_id: &str) -> Result<(), ServerError>;
    /// How many open publishes of the pack it expired
    fn clear_hold(&self, pack_id: &str) -> Result<u32, ServerError>;
    fn leave(&self, pack_id: &str) -> Result<(), ServerError>;
    fn delete_pack(&self, pack_id: &str) -> Result<(), ServerError>;
    fn uploader_invite(&self) -> Result<Link, ServerError>;
    fn users(&self) -> Result<Vec<AdminUser>, ServerError>;
    fn remove_user(&self, user_id: &str) -> Result<(), ServerError>;
}

#[derive(Serialize, Clone, Debug)]
pub struct Progress {
    pub pack_id: String,
    pub stage: String,
    pub done: usize,
    pub total: usize,
    pub file: Option<String>,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Bytes across the whole stage, zero when the stage does not move bytes
    pub overall_done: u64,
    pub overall_total: u64,
    /// Seconds left before a download held back by the server rate limit is retried
    pub wait_secs: u64,
}

impl Progress {
    pub fn overall(mut self, done: u64, total: u64) -> Progress {
        self.overall_done = done;
        self.overall_total = total;
        self
    }
}

// Sleeps in short steps so a cancel is seen quickly. False when cancelled.
pub(crate) fn nap(total: Duration, cancel: &dyn Fn() -> bool) -> bool {
    let step = Duration::from_millis(100);
    let mut slept = Duration::ZERO;
    while slept < total {
        if cancel() {
            return false;
        }
        std::thread::sleep(step.min(total - slept));
        slept += step;
    }
    !cancel()
}

fn unavailable(message: impl fmt::Display) -> ServerError {
    ServerError::new("unavailable", &message.to_string())
}

fn cancelled() -> ServerError {
    ServerError::new("cancelled", CANCELLED)
}

fn io_error(message: String) -> ServerError {
    ServerError::new("io", &message)
}

fn error_from(resp: Response) -> ServerError {
    #[derive(Deserialize)]
    struct Body {
        error: String,
        #[serde(default)]
        message: String,
    }
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok());
    let text = resp.text().unwrap_or_default();
    let mut err = match serde_json::from_str::<Body>(&text) {
        Ok(b) => ServerError::new(&b.error, &b.message),
        Err(_) => unavailable(format!("The pack server answered {}", status)),
    };
    err.retry_after = retry_after;
    err
}

/// R2 answers a refused PUT with an XML body naming the reason in its Code element
fn upload_refused(status: u16, body: &str) -> ServerError {
    let code = body
        .split_once("<Code>")
        .and_then(|(_, rest)| rest.split_once("</Code>"))
        .map(|(code, _)| code.trim())
        .unwrap_or("");
    ServerError::new("upload_refused", format!("{} {}", status, code).trim())
}

/// Long enough for a slow link, so a stalled PUT still ends
fn put_timeout(size: u64) -> Duration {
    Duration::from_secs(300 + size / (256 * 1024))
}

/// Reports bytes as reqwest reads the body on its own thread
struct Counting {
    file: File,
    tx: Sender<u64>,
}

impl Read for Counting {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.file.read(buf)?;
        if n > 0 {
            let _ = self.tx.send(n as u64);
        }
        Ok(n)
    }
}

pub struct HttpServer {
    client: Client,
    upload_client: Client,
    base: String,
    token: Option<String>,
}

const USER_AGENT: &str = concat!("ron-mod-manager/", env!("CARGO_PKG_VERSION"));

impl HttpServer {
    pub fn new(base: &str, token: Option<String>) -> HttpServer {
        let builder = || Client::builder().user_agent(USER_AGENT).connect_timeout(Duration::from_secs(30));
        HttpServer {
            client: builder().timeout(Duration::from_secs(300)).build().unwrap_or_else(|_| Client::new()),
            // put_blob sets a timeout per request from the blob size
            upload_client: builder().timeout(None).build().unwrap_or_else(|_| Client::new()),
            base: base.trim_end_matches('/').to_string(),
            token,
        }
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let rb = self.client.request(method, format!("{}{}", self.base, path));
        match &self.token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    fn send(&self, rb: RequestBuilder) -> Result<Response, ServerError> {
        let resp = rb.send().map_err(|e| unavailable(format!("Could not reach the pack server: {}", e)))?;
        if resp.status().is_success() {
            Ok(resp)
        } else {
            Err(error_from(resp))
        }
    }

    fn call<T: DeserializeOwned>(&self, rb: RequestBuilder) -> Result<T, ServerError> {
        self.send(rb)?
            .json()
            .map_err(|e| unavailable(format!("Unexpected answer from the pack server: {}", e)))
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ServerError> {
        self.call(self.request(Method::GET, path))
    }

    fn post<T: DeserializeOwned>(&self, path: &str, body: serde_json::Value) -> Result<T, ServerError> {
        self.call(self.request(Method::POST, path).json(&body))
    }

    /// Trades the loopback code for a session token. Needs no token of its own.
    pub fn token_exchange(&self, code: &str, verifier: &str) -> Result<(String, User), ServerError> {
        #[derive(Deserialize)]
        struct Granted {
            token: String,
            user: User,
        }
        let got: Granted = self.post("/auth/token", json!({ "code": code, "code_verifier": verifier }))?;
        Ok((got.token, got.user))
    }

    pub fn logout(&self) -> Result<(), ServerError> {
        self.post::<serde_json::Value>("/auth/logout", json!({})).map(|_| ())
    }

    fn delete(&self, path: &str) -> Result<serde_json::Value, ServerError> {
        self.call(self.request(Method::DELETE, path))
    }

    /// Retries connection failures only. An answer from the server is final.
    fn get_blob(&self, path: &str, from: u64, cancel: &dyn Fn() -> bool) -> Result<Response, ServerError> {
        let mut last = unavailable("Could not reach the pack server");
        for attempt in 0..MAX_ATTEMPTS {
            if cancel() || (attempt > 0 && !nap(Duration::from_secs(1 << (attempt - 1)), cancel)) {
                return Err(cancelled());
            }
            let mut rb = self.request(Method::GET, path);
            if from > 0 {
                rb = rb.header(RANGE, format!("bytes={}-", from));
            }
            match rb.send() {
                Ok(resp) if resp.status().is_success() => return Ok(resp),
                Ok(resp) => return Err(error_from(resp)),
                Err(e) => last = unavailable(format!("Could not reach the pack server: {}", e)),
            }
        }
        Err(last)
    }
}

impl PackServer for HttpServer {
    fn base(&self) -> &str {
        &self.base
    }

    fn me(&self) -> Result<Me, ServerError> {
        self.get("/me")
    }

    fn invite_info(&self, code: &str) -> Result<InviteInfo, ServerError> {
        self.get(&format!("/invites/{}", seg(code)))
    }

    fn accept_invite(&self, code: &str) -> Result<Accepted, ServerError> {
        self.post(&format!("/invites/{}/accept", seg(code)), json!({}))
    }

    fn manifest(&self, pack_id: &str) -> Result<RemoteManifest, ServerError> {
        self.get(&format!("/packs/{}/manifest", seg(pack_id)))
    }

    fn download(
        &self,
        sha256: &str,
        dest: &Path,
        expected_size: u64,
        cancel: &dyn Fn() -> bool,
        on_bytes: &mut dyn FnMut(u64),
    ) -> Result<String, ServerError> {
        let path = format!("/blobs/{}", seg(sha256));
        let mut resp = self.get_blob(&path, 0, cancel)?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| io_error(format!("Could not create {}: {}", parent.display(), e)))?;
        }
        let mut file = File::create(dest).map_err(|e| io_error(format!("Could not create {}: {}", dest.display(), e)))?;
        let mismatch = || ServerError::new("size_mismatch", SIZE_MISMATCH);
        let result = (|| -> Result<String, ServerError> {
            let mut hasher = Sha256::new();
            let mut written = 0u64;
            let mut failures = 0usize;
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                if cancel() {
                    return Err(cancelled());
                }
                match resp.read(&mut buf) {
                    Ok(0) if written != expected_size => return Err(mismatch()),
                    Ok(0) => return Ok(hashing::hex(&hasher.finalize())),
                    Ok(n) if written + n as u64 > expected_size => return Err(mismatch()),
                    Ok(n) => {
                        hasher.update(&buf[..n]);
                        file.write_all(&buf[..n])
                            .map_err(|e| io_error(format!("Could not write {}: {}", dest.display(), e)))?;
                        written += n as u64;
                        on_bytes(n as u64);
                    }
                    Err(e) => {
                        failures += 1;
                        if failures >= MAX_ATTEMPTS {
                            return Err(unavailable(format!("Download from the pack server failed: {}", e)));
                        }
                        if !nap(Duration::from_secs(1 << (failures - 1).min(3)), cancel) {
                            return Err(cancelled());
                        }
                        resp = match self.get_blob(&path, written, cancel) {
                            Ok(r) => r,
                            // Every byte arrived before the read failed
                            Err(e) if e.code == "bad_range" && written == expected_size => {
                                return Ok(hashing::hex(&hasher.finalize()))
                            }
                            Err(e) => return Err(e),
                        };
                        if written > 0 && resp.status().as_u16() != 206 {
                            file.set_len(0)
                                .and_then(|_| file.seek(SeekFrom::Start(0)).map(|_| ()))
                                .map_err(|e| io_error(format!("Could not rewrite {}: {}", dest.display(), e)))?;
                            hasher = Sha256::new();
                            written = 0;
                        }
                    }
                }
            }
        })();
        if result.is_err() {
            drop(file);
            let _ = fs::remove_file(dest);
        }
        result
    }

    fn begin(&self, pack_id: &str, name: &str, version: &str, base: Option<&str>, blobs: &[(String, u64)]) -> Result<Begun, ServerError> {
        let blobs: Vec<_> = blobs.iter().map(|(sha256, size)| json!({ "sha256": sha256, "size": size })).collect();
        self.post(
            &format!("/packs/{}/publish/begin", seg(pack_id)),
            json!({ "name": name, "version": version, "base": base, "blobs": blobs }),
        )
    }

    fn upload_urls(&self, publish_id: &str, hashes: &[String]) -> Result<BTreeMap<String, String>, ServerError> {
        #[derive(Deserialize)]
        struct Urls {
            urls: BTreeMap<String, String>,
        }
        let got: Urls = self.post(&format!("/publishes/{}/urls", seg(publish_id)), json!({ "blobs": hashes }))?;
        Ok(got.urls)
    }

    fn put_blob(&self, url: &str, src: &Path, size: u64, sha256: &str, on_bytes: &mut dyn FnMut(u64)) -> Result<(), ServerError> {
        let file = File::open(src).map_err(|e| io_error(format!("Could not open {}: {}", src.display(), e)))?;
        let (tx, rx) = mpsc::channel();
        let body = Body::sized(Counting { file, tx }, size);
        let request = self
            .upload_client
            .put(url)
            .timeout(put_timeout(size))
            .header("x-amz-checksum-sha256", checksum_b64(sha256))
            .body(body);
        let sent = std::thread::scope(|s| {
            let handle = s.spawn(move || request.send());
            // Ends once the request, and with it the sender, is dropped
            for n in rx {
                on_bytes(n);
            }
            handle.join()
        });
        let resp = match sent {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => return Err(unavailable(format!("Could not upload to the pack server: {}", e))),
            Err(_) => return Err(unavailable("The upload stopped unexpectedly")),
        };
        if resp.status().is_success() {
            return Ok(());
        }
        let status = resp.status().as_u16();
        Err(upload_refused(status, &resp.text().unwrap_or_default()))
    }

    fn verify(&self, publish_id: &str, hashes: &[String]) -> Result<Verified, ServerError> {
        self.post(&format!("/publishes/{}/verify", seg(publish_id)), json!({ "blobs": hashes }))
    }

    fn commit(&self, publish_id: &str, manifest: &RemoteManifest, notes: &str, history: &[VersionEntry]) -> Result<Option<u64>, ServerError> {
        #[derive(Deserialize)]
        struct Committed {
            #[serde(default)]
            published_at: Option<u64>,
        }
        let got: Committed = self.post(
            &format!("/publishes/{}/commit", seg(publish_id)),
            json!({ "manifest": manifest, "notes": notes, "history": history }),
        )?;
        Ok(got.published_at)
    }

    fn changelog(&self, pack_id: &str) -> Result<Vec<VersionEntry>, ServerError> {
        #[derive(Deserialize)]
        struct Changelog {
            versions: Vec<VersionEntry>,
        }
        let got: Changelog = self.get(&format!("/packs/{}/changelog", seg(pack_id)))?;
        Ok(got.versions)
    }

    fn cancel(&self, publish_id: &str) -> Result<(), ServerError> {
        self.delete(&format!("/publishes/{}", seg(publish_id))).map(|_| ())
    }

    fn pack_invite(&self, pack_id: &str) -> Result<Link, ServerError> {
        self.post(&format!("/packs/{}/invite", seg(pack_id)), json!({}))
    }

    fn reset_invite(&self, pack_id: &str) -> Result<Link, ServerError> {
        self.post(&format!("/packs/{}/invite/reset", seg(pack_id)), json!({}))
    }

    fn viewers(&self, pack_id: &str) -> Result<Vec<Viewer>, ServerError> {
        self.get(&format!("/packs/{}/viewers", seg(pack_id)))
    }

    fn remove_viewer(&self, pack_id: &str, user_id: &str) -> Result<RemovedViewer, ServerError> {
        self.call(self.request(Method::DELETE, &format!("/packs/{}/viewers/{}", seg(pack_id), seg(user_id))))
    }

    fn editor_invite(&self, pack_id: &str) -> Result<Link, ServerError> {
        self.post(&format!("/packs/{}/editor-invite", seg(pack_id)), json!({}))
    }

    fn editors(&self, pack_id: &str) -> Result<Vec<Viewer>, ServerError> {
        self.get(&format!("/packs/{}/editors", seg(pack_id)))
    }

    fn remove_editor(&self, pack_id: &str, user_id: &str) -> Result<(), ServerError> {
        self.delete(&format!("/packs/{}/editors/{}", seg(pack_id), seg(user_id))).map(|_| ())
    }

    fn clear_hold(&self, pack_id: &str) -> Result<u32, ServerError> {
        #[derive(Deserialize)]
        struct Cleared {
            cleared: u32,
        }
        let got: Cleared = self.call(self.request(Method::DELETE, &format!("/packs/{}/hold", seg(pack_id))))?;
        Ok(got.cleared)
    }

    fn leave(&self, pack_id: &str) -> Result<(), ServerError> {
        self.delete(&format!("/packs/{}/membership", seg(pack_id))).map(|_| ())
    }

    fn delete_pack(&self, pack_id: &str) -> Result<(), ServerError> {
        self.delete(&format!("/packs/{}", seg(pack_id))).map(|_| ())
    }

    fn uploader_invite(&self) -> Result<Link, ServerError> {
        self.post("/invites/uploader", json!({}))
    }

    fn users(&self) -> Result<Vec<AdminUser>, ServerError> {
        self.get("/admin/users")
    }

    fn remove_user(&self, user_id: &str) -> Result<(), ServerError> {
        self.delete(&format!("/admin/users/{}", seg(user_id))).map(|_| ())
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex, MutexGuard};

    pub const BASE: &str = "https://packs.test";
    pub const OWNER: &str = "owner";
    const DAY: u64 = 86400;
    const MAX_BLOBS: usize = 2000;
    const MAX_BATCH: usize = 50;
    const MAX_NOTES: usize = 32000;
    const MAX_HISTORY: usize = 99;
    const MAX_HISTORY_BYTES: usize = 1000000;
    const KEEP_VERSIONS: usize = 100;
    const MAX_VERSION: usize = 200;
    const URL_LIFETIME: u64 = 300;
    const PUBLISH_LIFETIME: u64 = 3600;

    struct Pack {
        uploader: String,
        name: String,
        version: String,
        manifest: Option<RemoteManifest>,
        blobs: BTreeSet<String>,
        viewers: BTreeSet<String>,
        editors: BTreeSet<String>,
        /// Oldest first, as the server's row ids run
        versions: Vec<VersionEntry>,
    }

    struct Invite {
        code: String,
        kind: String,
        pack_id: Option<String>,
        created_by: String,
        expires_at: u64,
        used: bool,
    }

    struct Publish {
        pack_id: String,
        user: String,
        name: String,
        version: String,
        base: Option<String>,
        expires_at: u64,
        blobs: BTreeMap<String, u64>,
        verified: BTreeSet<String>,
        cleared: bool,
    }

    #[derive(Default)]
    struct State {
        users: BTreeMap<String, User>,
        signed_in: BTreeSet<String>,
        packs: BTreeMap<String, Pack>,
        invites: Vec<Invite>,
        publishes: BTreeMap<String, Publish>,
        /// What R2 holds, uploaded or not yet verified
        objects: BTreeMap<String, Vec<u8>>,
        /// The blobs table, what a commit made known
        stored: BTreeSet<String>,
        next: u64,
        /// Seconds, moved by advance_clock and by one on each commit
        clock: u64,
        max_users: Option<usize>,
        expired: bool,
        daily_cap: usize,
        rate_limited: usize,
        changelog_fails: usize,
        corrupt: BTreeSet<String>,
        replace: Option<(usize, RemoteManifest)>,
        manifest_answers: BTreeMap<String, RemoteManifest>,
        omit_published_at: bool,
        cancel_hook: Option<(usize, Box<dyn Fn() + Send>)>,
        fail_puts_at: Option<usize>,
        expire_commit: bool,
        clear_commit: bool,
        drop_before_commit: Option<String>,
        lost_puts: BTreeMap<String, usize>,
        stored_before_urls: Option<Vec<u8>>,
        downloads: usize,
        puts: usize,
        begins: usize,
    }

    fn err(code: &str, message: &str) -> ServerError {
        ServerError::new(code, message)
    }

    fn behind() -> ServerError {
        err("pack_behind", "The pack has a newer version. Update your copy before publishing.")
    }

    fn pack_gone() -> ServerError {
        err("pack_gone", "This pack is not available")
    }

    fn publish_gone() -> ServerError {
        err("publish_gone", "This publish is no longer available")
    }

    fn publish_cleared() -> ServerError {
        err("publish_cleared", "The pack's creator stopped this publish.")
    }

    fn bad_request(message: &str) -> ServerError {
        err("bad_request", message)
    }

    impl State {
        fn id(&mut self, prefix: &str) -> String {
            self.next += 1;
            format!("{}{}", prefix, self.next)
        }

        fn role(&self, user: &str) -> Option<String> {
            self.users.get(user).and_then(|u| u.role.clone())
        }

        fn visible(&self, pack_id: &str, user: &str) -> bool {
            self.packs.get(pack_id).is_some_and(|p| {
                p.manifest.is_some()
                    && (self.role(user).as_deref() == Some("owner") || p.uploader == user || p.editors.contains(user) || p.viewers.contains(user))
            })
        }

        /// Creator, editor or owner
        fn managed(&self, pack_id: &str, user: &str) -> Result<(), ServerError> {
            let pack = self.packs.get(pack_id).ok_or_else(pack_gone)?;
            if self.role(user).as_deref() != Some("owner") && pack.uploader != user && !pack.editors.contains(user) {
                return Err(err("forbidden", "Only the pack's creator or an editor can do this"));
            }
            Ok(())
        }

        /// Creator or owner
        fn creator(&self, pack_id: &str, user: &str) -> Result<(), ServerError> {
            let pack = self.packs.get(pack_id).ok_or_else(pack_gone)?;
            if self.role(user).as_deref() != Some("owner") && pack.uploader != user {
                return Err(err("forbidden", "Only the pack's creator can do this"));
            }
            Ok(())
        }

        fn may_publish(&self, pack_id: &str, user: &str) -> bool {
            self.packs.get(pack_id).is_some_and(|p| p.uploader == user || p.editors.contains(user))
        }

        fn uploader(&self, user: &str) -> Result<(), ServerError> {
            match self.role(user) {
                Some(_) => Ok(()),
                None => Err(err("forbidden", "Only uploaders can publish")),
            }
        }

        fn own_publish(&self, publish_id: &str, user: &str) -> Result<(), ServerError> {
            match self.publishes.get(publish_id) {
                Some(p) if p.user == user && p.expires_at > self.clock => Ok(()),
                Some(p) if p.user == user && p.cleared => Err(publish_cleared()),
                _ => Err(publish_gone()),
            }
        }

        fn link(&self, code: &str, expires_at: u64) -> Link {
            Link { url: format!("{}/i/{}", BASE, code), expires_at }
        }

        fn mint(&mut self, kind: &str, pack_id: Option<&str>, by: &str) -> Link {
            let code = self.id("code");
            let expires_at = self.clock + DAY;
            self.invites.push(Invite {
                code: code.clone(),
                kind: kind.to_string(),
                pack_id: pack_id.map(str::to_string),
                created_by: by.to_string(),
                expires_at,
                used: false,
            });
            self.link(&code, expires_at)
        }

        fn live_invite(&self, code: &str) -> Result<&Invite, ServerError> {
            self.invites
                .iter()
                .find(|i| i.code == code && !i.used && i.expires_at > self.clock)
                .ok_or_else(|| err("invite_gone", "This invite is no longer valid"))
        }

        /// People counted against the server's limit, as the server's peopleCount
        fn people(&self) -> usize {
            self.users
                .values()
                .filter(|u| u.role.is_some() || self.packs.values().any(|p| p.viewers.contains(&u.id)))
                .count()
        }

        fn require_room(&self, user: &str) -> Result<(), ServerError> {
            if self.role(user).is_some() || self.packs.values().any(|p| p.viewers.contains(user)) {
                return Ok(());
            }
            if self.max_users.is_some_and(|max| self.people() >= max) {
                return Err(err("server_full", "This server has no room for more people"));
            }
            Ok(())
        }

        fn extend(&mut self, publish_id: &str) {
            let until = self.clock + PUBLISH_LIFETIME;
            if let Some(p) = self.publishes.get_mut(publish_id) {
                p.expires_at = until;
            }
        }

        /// Expires open publishes of the pack by `user`, or by everyone when None
        fn expire_publishes(&mut self, pack_id: &str, user: Option<&str>) -> u32 {
            let now = self.clock;
            let mut n = 0;
            for p in self.publishes.values_mut() {
                if p.pack_id == pack_id && p.expires_at > now && user.is_none_or(|u| p.user == u) {
                    p.expires_at = now;
                    n += 1;
                }
            }
            n
        }

        fn drop_pack(&mut self, pack_id: &str) {
            self.packs.remove(pack_id);
            self.invites.retain(|i| i.pack_id.as_deref() != Some(pack_id));
            self.publishes.retain(|_, p| p.pack_id != pack_id);
        }

        /// The commit statements, shared with the seeding knobs
        fn record(
            &mut self,
            pack_id: &str,
            manifest: &RemoteManifest,
            name: &str,
            version: &str,
            notes: &str,
            history: &[VersionEntry],
            publisher: Option<&str>,
        ) -> u64 {
            self.clock += 1;
            let at = self.clock;
            let hashes: BTreeSet<String> =
                manifest.mods.iter().flat_map(|m| m.files.iter().map(|f| f.sha256.clone())).collect();
            self.stored.extend(hashes.iter().cloned());
            let pack = self.packs.get_mut(pack_id).expect("pack row");
            let mut stored = manifest.clone();
            stored.name = name.to_string();
            stored.version = version.to_string();
            stored.published_at = at;
            stored.published_by = publisher.map(str::to_string);
            pack.name = name.to_string();
            pack.version = version.to_string();
            pack.manifest = Some(stored);
            pack.blobs = hashes;
            if pack.versions.is_empty() {
                pack.versions.extend(history.iter().cloned());
            }
            pack.versions.push(VersionEntry {
                version: version.to_string(),
                published_at: at,
                notes: notes.to_string(),
                publisher: publisher.map(str::to_string),
            });
            let extra = pack.versions.len().saturating_sub(KEEP_VERSIONS);
            pack.versions.drain(..extra);
            at
        }
    }

    /// One user's view of a shared in-memory server
    pub struct FakeServer {
        state: Arc<Mutex<State>>,
        user: String,
    }

    impl FakeServer {
        /// Signed in as the owner
        pub fn new() -> FakeServer {
            let mut state = State { clock: 1_700_000_000, ..Default::default() };
            state.users.insert(OWNER.into(), User { id: OWNER.into(), name: "Owner".into(), role: Some("owner".into()) });
            state.signed_in.insert(OWNER.into());
            FakeServer { state: Arc::new(Mutex::new(state)), user: OWNER.into() }
        }

        /// Adds a signed in user and returns their view
        pub fn add_user(&self, id: &str, name: &str, role: Option<&str>) -> FakeServer {
            let mut st = self.lock();
            st.users.insert(id.into(), User { id: id.into(), name: name.into(), role: role.map(str::to_string) });
            st.signed_in.insert(id.into());
            drop(st);
            self.as_user(id)
        }

        pub fn as_user(&self, id: &str) -> FakeServer {
            FakeServer { state: self.state.clone(), user: id.into() }
        }

        /// Stores bytes in the bucket and returns their hash, as if a PUT had landed
        pub fn store_blob(&self, bytes: &[u8]) -> String {
            let sha = hashing::sha256_bytes(bytes);
            self.lock().objects.insert(sha.clone(), bytes.to_vec());
            sha
        }

        /// Publishes `manifest` as `uploader` without the begin and upload steps. Store its blobs first.
        pub fn seed_pack(&self, uploader: &str, manifest: &RemoteManifest, notes: &str) {
            let mut st = self.lock();
            st.packs.entry(manifest.pack_id.clone()).or_insert_with(|| Pack {
                uploader: uploader.into(),
                name: manifest.name.clone(),
                version: manifest.version.clone(),
                manifest: None,
                blobs: BTreeSet::new(),
                viewers: BTreeSet::new(),
                editors: BTreeSet::new(),
                versions: Vec::new(),
            });
            st.record(&manifest.pack_id, manifest, &manifest.name, &manifest.version, notes, &[], None);
        }

        pub fn add_editor(&self, pack_id: &str, user_id: &str) {
            if let Some(p) = self.lock().packs.get_mut(pack_id) {
                p.viewers.remove(user_id);
                p.editors.insert(user_id.into());
            }
        }

        pub fn add_viewer(&self, pack_id: &str, user_id: &str) {
            if let Some(p) = self.lock().packs.get_mut(pack_id) {
                p.viewers.insert(user_id.into());
            }
        }

        pub fn blob(&self, sha256: &str) -> Option<Vec<u8>> {
            self.lock().objects.get(sha256).cloned()
        }

        pub fn pack_blobs(&self, pack_id: &str) -> Vec<String> {
            self.lock().packs.get(pack_id).map(|p| p.blobs.iter().cloned().collect()).unwrap_or_default()
        }

        pub fn has_pack(&self, pack_id: &str) -> bool {
            self.lock().packs.contains_key(pack_id)
        }

        /// Moves the server clock, so invites and upload URLs expire
        pub fn advance_clock(&self, secs: u64) {
            self.lock().clock += secs;
        }

        /// Accepting an invite answers server_full once this many people are on the server
        pub fn set_max_users(&self, n: usize) {
            self.lock().max_users = Some(n);
        }

        /// Every call answers signed_out from now on
        pub fn expire_session(&self) {
            self.lock().expired = true;
        }

        /// The next n calls answer unavailable
        pub fn fail_daily_cap(&self, n: usize) {
            self.lock().daily_cap = n;
        }

        /// The next n download calls answer rate_limited
        pub fn rate_limit_downloads(&self, n: usize) {
            self.lock().rate_limited = n;
        }

        /// The next n changelog calls answer unavailable
        pub fn fail_changelog(&self, n: usize) {
            self.lock().changelog_fails = n;
        }

        /// The next download of `sha256` arrives with its first byte flipped, as a transfer that went bad
        pub fn corrupt_next(&self, sha256: &str) {
            self.lock().corrupt.insert(sha256.to_string());
        }

        /// Commits answer without published_at from now on, as a server from before that field
        pub fn omit_published_at(&self) {
            self.lock().omit_published_at = true;
        }

        /// The next manifest call for `pack_id` answers `manifest` as it is, as a server that sends the wrong pack
        pub fn answer_manifest_with(&self, pack_id: &str, manifest: RemoteManifest) {
            self.lock().manifest_answers.insert(pack_id.to_string(), manifest);
        }

        /// Once n more downloads have finished, commits `manifest` as the pack's new version, so blobs
        /// only the old version had answer blob_gone. Store the new version's blobs first.
        pub fn replace_version_after(&self, n_downloads: usize, manifest: RemoteManifest) {
            let mut st = self.lock();
            let at = st.downloads + n_downloads;
            st.replace = Some((at, manifest));
        }

        /// Runs `hook` once, as the download call after the next n downloads starts and before
        /// it checks its cancel callback, so a hook that requests a cancel stops that download
        pub fn cancel_after(&self, n: usize, hook: impl Fn() + Send + 'static) {
            let mut st = self.lock();
            let at = st.downloads + n;
            st.cancel_hook = Some((at, Box::new(hook)));
        }

        /// The PUT after the next n accepted ones fails once, as a dropped connection
        pub fn fail_puts_after(&self, n: usize) {
            let mut st = self.lock();
            let at = st.puts + n;
            st.fail_puts_at = Some(at);
        }

        /// The next commit finds its publish expired, as one left open past its hour
        pub fn expire_next_commit(&self) {
            self.lock().expire_commit = true;
        }

        /// The pack's creator clears the hold just before the next commit
        pub fn clear_hold_before_commit(&self) {
            self.lock().clear_commit = true;
        }

        /// Before the next commit, `sha256`'s upload disappears and loses its verified mark
        pub fn drop_upload_before_commit(&self, sha256: &str) {
            self.lock().drop_before_commit = Some(sha256.to_string());
        }

        /// The next n PUTs of `sha256` are accepted but never land, so verify answers bad
        pub fn lose_puts_of(&self, sha256: &str, n: usize) {
            self.lock().lost_puts.insert(sha256.to_string(), n);
        }

        /// The next upload_urls call finds `bytes` already stored, as another uploader's commit landing first
        pub fn store_before_next_urls(&self, bytes: &[u8]) {
            self.lock().stored_before_urls = Some(bytes.to_vec());
        }

        /// Publishes begun and neither committed nor cancelled
        pub fn open_publishes(&self) -> usize {
            let st = self.lock();
            st.publishes.values().filter(|p| p.expires_at > st.clock).count()
        }

        /// Counts finished downloads only
        pub fn downloads(&self) -> usize {
            self.lock().downloads
        }

        /// Counts accepted PUTs only
        pub fn puts(&self) -> usize {
            self.lock().puts
        }

        pub fn begins(&self) -> usize {
            self.lock().begins
        }

        fn lock(&self) -> MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(|e| e.into_inner())
        }

        /// Applies the session and daily cap knobs, as every authenticated route does
        fn gate(&self) -> Result<MutexGuard<'_, State>, ServerError> {
            let mut st = self.lock();
            if st.expired || !st.signed_in.contains(&self.user) {
                return Err(err("signed_out", "Sign in again"));
            }
            if st.daily_cap > 0 {
                st.daily_cap -= 1;
                return Err(err("unavailable", "The pack server answered 503"));
            }
            Ok(st)
        }
    }

    impl PackServer for FakeServer {
        fn base(&self) -> &str {
            BASE
        }

        fn me(&self) -> Result<Me, ServerError> {
            let st = self.gate()?;
            let me = st.users[&self.user].clone();
            let owner = me.role.as_deref() == Some("owner");
            let mut packs: Vec<MePack> = st
                .packs
                .iter()
                .filter(|(id, _)| st.visible(id, &self.user))
                .map(|(id, p)| MePack {
                    id: id.clone(),
                    name: p.name.clone(),
                    version: p.version.clone(),
                    uploader: User {
                        id: p.uploader.clone(),
                        name: st.users.get(&p.uploader).map(|u| u.name.clone()).unwrap_or_default(),
                        role: None,
                    },
                    access: if p.uploader == self.user {
                        "uploader"
                    } else if p.editors.contains(&self.user) {
                        "editor"
                    } else if p.viewers.contains(&self.user) || !owner {
                        "viewer"
                    } else {
                        "owner"
                    }
                    .into(),
                })
                .collect();
            packs.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
            Ok(Me { id: me.id, name: me.name, role: me.role, packs })
        }

        fn invite_info(&self, code: &str) -> Result<InviteInfo, ServerError> {
            let st = self.gate()?;
            let invite = st.live_invite(code)?;
            let pack = match &invite.pack_id {
                Some(id) => {
                    let p = st.packs.get(id).ok_or_else(|| err("invite_gone", "This invite is no longer valid"))?;
                    let name = st.users.get(&p.uploader).map(|u| u.name.clone()).unwrap_or_default();
                    Some(InvitePack {
                        id: id.clone(),
                        name: p.name.clone(),
                        version: p.version.clone(),
                        uploader: User { id: p.uploader.clone(), name, role: None },
                    })
                }
                None => None,
            };
            Ok(InviteInfo { kind: invite.kind.clone(), expires_at: invite.expires_at, pack })
        }

        fn accept_invite(&self, code: &str) -> Result<Accepted, ServerError> {
            let mut st = self.gate()?;
            let (kind, pack_id) = {
                let invite = st.live_invite(code)?;
                (invite.kind.clone(), invite.pack_id.clone())
            };
            if let Some(id) = &pack_id {
                if !st.packs.contains_key(id) {
                    return Err(err("invite_gone", "This invite is no longer valid"));
                }
            }
            st.require_room(&self.user)?;
            let role = st.role(&self.user);
            match pack_id {
                None => {
                    if role.is_none() {
                        st.invites.iter_mut().filter(|i| i.code == code).for_each(|i| i.used = true);
                        if let Some(u) = st.users.get_mut(&self.user) {
                            u.role = Some("uploader".into());
                        }
                    }
                    Ok(Accepted { kind, pack_id: None })
                }
                Some(id) => {
                    let pack = st.packs.get_mut(&id).ok_or_else(|| err("invite_gone", "This invite is no longer valid"))?;
                    let exempt = role.as_deref() == Some("owner") || pack.uploader == self.user;
                    if kind == "editor" {
                        if pack.uploader != self.user && !pack.editors.contains(&self.user) {
                            pack.editors.insert(self.user.clone());
                            pack.viewers.remove(&self.user);
                            st.invites.iter_mut().filter(|i| i.code == code).for_each(|i| i.used = true);
                            if let Some(u) = st.users.get_mut(&self.user) {
                                u.role.get_or_insert_with(|| "uploader".into());
                            }
                        }
                    } else if !exempt {
                        pack.viewers.insert(self.user.clone());
                    }
                    Ok(Accepted { kind, pack_id: Some(id) })
                }
            }
        }

        fn manifest(&self, pack_id: &str) -> Result<RemoteManifest, ServerError> {
            let mut st = self.gate()?;
            if !st.visible(pack_id, &self.user) {
                return Err(pack_gone());
            }
            if let Some(answer) = st.manifest_answers.remove(pack_id) {
                return Ok(answer);
            }
            Ok(st.packs[pack_id].manifest.clone().expect("visible packs have a manifest"))
        }

        fn download(
            &self,
            sha256: &str,
            dest: &Path,
            expected_size: u64,
            cancel: &dyn Fn() -> bool,
            on_bytes: &mut dyn FnMut(u64),
        ) -> Result<String, ServerError> {
            let hook = {
                let mut st = self.lock();
                let due = matches!(&st.cancel_hook, Some((at, _)) if *at == st.downloads);
                if due {
                    st.cancel_hook.take().map(|(_, h)| h)
                } else {
                    None
                }
            };
            if let Some(hook) = hook {
                hook();
            }
            if cancel() {
                let _ = fs::remove_file(dest);
                return Err(cancelled());
            }
            let mut st = self.gate()?;
            if st.rate_limited > 0 {
                st.rate_limited -= 1;
                let mut e = err("rate_limited", "Too many requests");
                e.retry_after = Some(0);
                return Err(e);
            }
            if matches!(&st.replace, Some((at, _)) if *at <= st.downloads) {
                let (_, manifest) = st.replace.take().expect("checked above");
                let (name, version) = (manifest.name.clone(), manifest.version.clone());
                if st.packs.contains_key(&manifest.pack_id) {
                    st.record(&manifest.pack_id, &manifest, &name, &version, "", &[], None);
                }
            }
            let user = self.user.clone();
            let reachable = st.packs.iter().any(|(id, p)| p.blobs.contains(sha256) && st.visible(id, &user));
            let mut bytes = match st.objects.get(sha256) {
                Some(b) if reachable => b.clone(),
                _ => return Err(err("blob_gone", "This file is not available")),
            };
            if st.corrupt.remove(sha256) {
                if let Some(b) = bytes.first_mut() {
                    *b ^= 0xff;
                }
            }
            drop(st);
            if bytes.len() as u64 != expected_size {
                return Err(ServerError::new("size_mismatch", SIZE_MISMATCH));
            }
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| io_error(e.to_string()))?;
            }
            fs::write(dest, &bytes).map_err(|e| io_error(e.to_string()))?;
            self.lock().downloads += 1;
            on_bytes(bytes.len() as u64);
            Ok(hashing::sha256_bytes(&bytes))
        }

        fn begin(&self, pack_id: &str, name: &str, version: &str, base: Option<&str>, blobs: &[(String, u64)]) -> Result<Begun, ServerError> {
            let mut st = self.gate()?;
            if blobs.len() > MAX_BLOBS {
                return Err(bad_request("Blobs must be a list of at most 2000"));
            }
            let mut wanted = BTreeMap::new();
            for (sha, size) in blobs {
                if wanted.insert(sha.clone(), *size).is_some_and(|old| old != *size) {
                    return Err(bad_request("Conflicting sizes"));
                }
            }
            match st.packs.get(pack_id) {
                None => st.uploader(&self.user)?,
                Some(_) if !st.may_publish(pack_id, &self.user) => {
                    return Err(err("pack_taken", "Another uploader owns this pack id"));
                }
                Some(p) if p.manifest.is_some() && base != Some(p.version.as_str()) => return Err(behind()),
                Some(_) => {}
            }
            st.expire_publishes(pack_id, Some(&self.user));
            let now = st.clock;
            let holder = st.publishes.values().find(|p| p.pack_id == pack_id && p.expires_at > now).map(|p| p.user.clone());
            if let Some(holder) = holder {
                let who = st.users.get(&holder).map(|u| u.name.clone()).unwrap_or_default();
                return Err(err("pack_busy", &format!("{} is publishing this pack right now. Try again once it is done.", who)));
            }
            st.packs.entry(pack_id.into()).or_insert_with(|| Pack {
                uploader: self.user.clone(),
                name: name.into(),
                version: version.into(),
                manifest: None,
                blobs: BTreeSet::new(),
                viewers: BTreeSet::new(),
                editors: BTreeSet::new(),
                versions: Vec::new(),
            });
            let missing: Vec<String> = wanted.keys().filter(|h| !st.stored.contains(*h)).cloned().collect();
            let publish_id = st.id("pub");
            st.publishes.insert(
                publish_id.clone(),
                Publish {
                    pack_id: pack_id.into(),
                    user: self.user.clone(),
                    name: name.into(),
                    version: version.into(),
                    base: base.map(str::to_string),
                    expires_at: now + PUBLISH_LIFETIME,
                    blobs: wanted,
                    verified: BTreeSet::new(),
                    cleared: false,
                },
            );
            st.begins += 1;
            Ok(Begun { publish_id, missing })
        }

        fn upload_urls(&self, publish_id: &str, hashes: &[String]) -> Result<BTreeMap<String, String>, ServerError> {
            let mut st = self.gate()?;
            if let Some(bytes) = st.stored_before_urls.take() {
                let sha = hashing::sha256_bytes(&bytes);
                st.objects.insert(sha.clone(), bytes);
                st.stored.insert(sha);
            }
            st.own_publish(publish_id, &self.user)?;
            st.extend(publish_id);
            if hashes.len() > MAX_BATCH {
                return Err(bad_request("Send at most 50 blobs"));
            }
            let publish = &st.publishes[publish_id];
            if hashes.iter().any(|h| !publish.blobs.contains_key(h)) {
                return Err(bad_request("Blob is not part of this publish"));
            }
            if hashes.iter().any(|h| st.stored.contains(h)) {
                return Err(err("already_stored", "A blob is already stored, verify it instead"));
            }
            // Like a presigned URL, it names the blob, its size and its expiry, not the publish
            let expires = st.clock + URL_LIFETIME;
            Ok(hashes.iter().map(|h| (h.clone(), format!("fake://blobs/{}?size={}&expires={}", h, publish.blobs[h], expires))).collect())
        }

        fn put_blob(&self, url: &str, src: &Path, size: u64, sha256: &str, on_bytes: &mut dyn FnMut(u64)) -> Result<(), ServerError> {
            let bytes = fs::read(src).map_err(|e| io_error(format!("Could not open {}: {}", src.display(), e)))?;
            let mut st = self.lock();
            if st.fail_puts_at.is_some_and(|at| st.puts >= at) {
                st.fail_puts_at = None;
                return Err(unavailable("Could not upload to the pack server: connection reset"));
            }
            let signed = url.strip_prefix("fake://blobs/").and_then(|rest| {
                let (sha, query) = rest.split_once("?size=")?;
                let (size, expires) = query.split_once("&expires=")?;
                Some((sha, size.parse::<u64>().ok()?, expires.parse::<u64>().ok()?))
            });
            let (sha, declared_size) = match signed {
                Some((sha, size, expires)) if st.clock < expires => (sha, size),
                _ => return Err(upload_refused(403, "<Error><Code>AccessDenied</Code></Error>")),
            };
            if declared_size != size || bytes.len() as u64 != size {
                return Err(upload_refused(400, "<Error><Code>IncompleteBody</Code></Error>"));
            }
            if sha != sha256 || hashing::sha256_bytes(&bytes) != sha {
                return Err(upload_refused(400, "<Error><Code>BadDigest</Code></Error>"));
            }
            let sha = sha.to_string();
            st.puts += 1;
            match st.lost_puts.get_mut(&sha) {
                Some(n) if *n > 0 => *n -= 1,
                _ => {
                    st.objects.insert(sha, bytes);
                }
            }
            drop(st);
            on_bytes(size);
            Ok(())
        }

        fn verify(&self, publish_id: &str, hashes: &[String]) -> Result<Verified, ServerError> {
            let mut st = self.gate()?;
            st.own_publish(publish_id, &self.user)?;
            st.extend(publish_id);
            if hashes.len() > MAX_BATCH {
                return Err(bad_request("Send at most 50 blobs"));
            }
            let known = st.publishes[publish_id].blobs.clone();
            if hashes.iter().any(|h| !known.contains_key(h)) {
                return Err(bad_request("Blob is not part of this publish"));
            }
            let mut out = Verified { verified: Vec::new(), bad: Vec::new() };
            for h in hashes {
                let ok = st.stored.contains(h) || st.objects.get(h).is_some_and(|b| b.len() as u64 == known[h]);
                if ok {
                    out.verified.push(h.clone());
                } else {
                    st.objects.remove(h);
                    out.bad.push(h.clone());
                }
            }
            let publish = st.publishes.get_mut(publish_id).expect("checked above");
            publish.verified.extend(out.verified.iter().cloned());
            Ok(out)
        }

        fn commit(&self, publish_id: &str, manifest: &RemoteManifest, notes: &str, history: &[VersionEntry]) -> Result<Option<u64>, ServerError> {
            let mut st = self.gate()?;
            if std::mem::take(&mut st.clear_commit) {
                let now = st.clock;
                if let Some(p) = st.publishes.get_mut(publish_id) {
                    p.cleared = true;
                    p.expires_at = now;
                }
            }
            st.own_publish(publish_id, &self.user)?;
            if std::mem::take(&mut st.expire_commit) {
                st.publishes.remove(publish_id);
                return Err(publish_gone());
            }
            if let Some(sha) = st.drop_before_commit.take() {
                st.objects.remove(&sha);
                if let Some(p) = st.publishes.get_mut(publish_id) {
                    p.verified.remove(&sha);
                }
            }
            let history_bytes = serde_json::to_vec(history).map(|v| v.len()).unwrap_or(usize::MAX);
            let long_version = history.iter().any(|e| e.version.encode_utf16().count() > MAX_VERSION);
            if notes.encode_utf16().count() > MAX_NOTES || history.len() > MAX_HISTORY || history_bytes > MAX_HISTORY_BYTES || long_version {
                return Err(err("bad_changelog", "The changelog is too large"));
            }
            let publish = &st.publishes[publish_id];
            let bad = || err("bad_manifest", "The manifest does not match this publish");
            if manifest.format != FORMAT || manifest.pack_id != publish.pack_id {
                return Err(bad());
            }
            let got: BTreeSet<(String, u64)> =
                manifest.mods.iter().flat_map(|m| m.files.iter().map(|f| (f.sha256.clone(), f.size))).collect();
            let want: BTreeSet<(String, u64)> = publish.blobs.iter().map(|(h, s)| (h.clone(), *s)).collect();
            if got != want {
                return Err(bad());
            }
            if publish.blobs.keys().any(|h| !publish.verified.contains(h) && !st.stored.contains(h)) {
                return Err(err("not_uploaded", "Some blobs have not been uploaded and verified"));
            }
            if !st.may_publish(&publish.pack_id, &self.user) {
                return Err(err("pack_taken", "Another uploader owns this pack id"));
            }
            let pack = &st.packs[&publish.pack_id];
            if pack.manifest.is_some() && publish.base.as_deref() != Some(pack.version.as_str()) {
                return Err(behind());
            }
            let publish = st.publishes.remove(publish_id).expect("checked above");
            let publisher = st.users[&self.user].name.clone();
            let at = st.record(&publish.pack_id, manifest, &publish.name, &publish.version, notes, history, Some(&publisher));
            Ok(if st.omit_published_at { None } else { Some(at) })
        }

        fn changelog(&self, pack_id: &str) -> Result<Vec<VersionEntry>, ServerError> {
            let mut st = self.gate()?;
            if st.changelog_fails > 0 {
                st.changelog_fails -= 1;
                return Err(err("unavailable", "The pack server answered 503"));
            }
            if !st.visible(pack_id, &self.user) {
                return Err(pack_gone());
            }
            Ok(st.packs[pack_id].versions.iter().rev().cloned().collect())
        }

        fn cancel(&self, publish_id: &str) -> Result<(), ServerError> {
            let mut st = self.gate()?;
            st.own_publish(publish_id, &self.user)?;
            st.publishes.remove(publish_id);
            Ok(())
        }

        fn pack_invite(&self, pack_id: &str) -> Result<Link, ServerError> {
            let mut st = self.gate()?;
            st.managed(pack_id, &self.user)?;
            let live = st
                .invites
                .iter()
                .filter(|i| i.kind == "viewer" && i.pack_id.as_deref() == Some(pack_id) && !i.used && i.expires_at > st.clock)
                .max_by_key(|i| i.expires_at)
                .map(|i| (i.code.clone(), i.expires_at));
            Ok(match live {
                Some((code, expires_at)) => st.link(&code, expires_at),
                None => st.mint("viewer", Some(pack_id), &self.user),
            })
        }

        fn reset_invite(&self, pack_id: &str) -> Result<Link, ServerError> {
            let mut st = self.gate()?;
            st.creator(pack_id, &self.user)?;
            st.invites.retain(|i| !(i.kind == "viewer" && i.pack_id.as_deref() == Some(pack_id)));
            Ok(st.mint("viewer", Some(pack_id), &self.user))
        }

        fn viewers(&self, pack_id: &str) -> Result<Vec<Viewer>, ServerError> {
            let st = self.gate()?;
            st.managed(pack_id, &self.user)?;
            let mut out: Vec<Viewer> = st.packs[pack_id]
                .viewers
                .iter()
                .filter_map(|id| st.users.get(id))
                .map(|u| Viewer { id: u.id.clone(), name: u.name.clone() })
                .collect();
            out.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
            Ok(out)
        }

        fn remove_viewer(&self, pack_id: &str, user_id: &str) -> Result<RemovedViewer, ServerError> {
            let mut st = self.gate()?;
            st.managed(pack_id, &self.user)?;
            let pack = st.packs.get_mut(pack_id).expect("checked above");
            if !pack.viewers.remove(user_id) {
                return Err(err("user_gone", "This person is not on the server"));
            }
            let before = st.invites.len();
            st.invites.retain(|i| !(i.kind == "viewer" && i.pack_id.as_deref() == Some(pack_id)));
            Ok(RemovedViewer { invite_reset: st.invites.len() != before })
        }

        fn editor_invite(&self, pack_id: &str) -> Result<Link, ServerError> {
            let mut st = self.gate()?;
            st.creator(pack_id, &self.user)?;
            Ok(st.mint("editor", Some(pack_id), &self.user))
        }

        fn editors(&self, pack_id: &str) -> Result<Vec<Viewer>, ServerError> {
            let st = self.gate()?;
            st.creator(pack_id, &self.user)?;
            let mut out: Vec<Viewer> = st.packs[pack_id]
                .editors
                .iter()
                .filter_map(|id| st.users.get(id))
                .map(|u| Viewer { id: u.id.clone(), name: u.name.clone() })
                .collect();
            out.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
            Ok(out)
        }

        fn remove_editor(&self, pack_id: &str, user_id: &str) -> Result<(), ServerError> {
            let mut st = self.gate()?;
            st.creator(pack_id, &self.user)?;
            let pack = st.packs.get_mut(pack_id).expect("checked above");
            if !pack.editors.remove(user_id) {
                return Err(err("user_gone", "This person is not on the server"));
            }
            pack.viewers.insert(user_id.into());
            st.expire_publishes(pack_id, Some(user_id));
            Ok(())
        }

        fn clear_hold(&self, pack_id: &str) -> Result<u32, ServerError> {
            let mut st = self.gate()?;
            st.creator(pack_id, &self.user)?;
            let now = st.clock;
            for p in st.publishes.values_mut() {
                if p.pack_id == pack_id && p.expires_at > now {
                    p.cleared = true;
                }
            }
            Ok(st.expire_publishes(pack_id, None))
        }

        fn leave(&self, pack_id: &str) -> Result<(), ServerError> {
            let mut st = self.gate()?;
            let pack = st.packs.get_mut(pack_id).ok_or_else(pack_gone)?;
            if pack.uploader == self.user {
                return Err(err("own_pack", "You cannot leave your own pack"));
            }
            let was_editor = pack.editors.remove(&self.user);
            if !pack.viewers.remove(&self.user) && !was_editor {
                return Err(pack_gone());
            }
            if was_editor {
                st.expire_publishes(pack_id, Some(&self.user));
            }
            Ok(())
        }

        fn delete_pack(&self, pack_id: &str) -> Result<(), ServerError> {
            let mut st = self.gate()?;
            st.creator(pack_id, &self.user)?;
            st.drop_pack(pack_id);
            Ok(())
        }

        fn uploader_invite(&self) -> Result<Link, ServerError> {
            let mut st = self.gate()?;
            if st.role(&self.user).as_deref() != Some("owner") {
                return Err(err("forbidden", "Only the owner can do this"));
            }
            Ok(st.mint("uploader", None, &self.user))
        }

        fn users(&self) -> Result<Vec<AdminUser>, ServerError> {
            let st = self.gate()?;
            if st.role(&self.user).as_deref() != Some("owner") {
                return Err(err("forbidden", "Only the owner can do this"));
            }
            let mut out: Vec<AdminUser> = st
                .users
                .values()
                .filter(|u| u.role.is_some() || st.packs.values().any(|p| p.viewers.contains(&u.id)))
                .map(|u| AdminUser {
                    id: u.id.clone(),
                    name: u.name.clone(),
                    role: u.role.clone(),
                    packs: st.packs.values().filter(|p| p.uploader == u.id).count() as u32,
                })
                .collect();
            out.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
            Ok(out)
        }

        fn remove_user(&self, user_id: &str) -> Result<(), ServerError> {
            let mut st = self.gate()?;
            if st.role(&self.user).as_deref() != Some("owner") {
                return Err(err("forbidden", "Only the owner can do this"));
            }
            if user_id == self.user {
                return Err(err("self_remove", "The owner cannot remove themselves"));
            }
            if !st.users.contains_key(user_id) {
                return Err(err("user_gone", "This person is not on the server"));
            }
            let owned: Vec<String> = st.packs.iter().filter(|(_, p)| p.uploader == user_id).map(|(id, _)| id.clone()).collect();
            for id in owned {
                st.drop_pack(&id);
            }
            let viewed: BTreeSet<String> =
                st.packs.iter().filter(|(_, p)| p.viewers.contains(user_id) || p.editors.contains(user_id)).map(|(id, _)| id.clone()).collect();
            st.invites.retain(|i| {
                !(i.kind == "viewer" && i.pack_id.as_ref().is_some_and(|p| viewed.contains(p))) && i.created_by != user_id
            });
            st.packs.values_mut().for_each(|p| {
                p.viewers.remove(user_id);
                p.editors.remove(user_id);
            });
            st.signed_in.remove(user_id);
            st.publishes.retain(|_, p| p.user != user_id);
            if let Some(u) = st.users.get_mut(user_id) {
                u.role = None;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    /// Answers one request with `reply` and hands back the raw request text
    fn stub(reply: &'static [u8]) -> (String, JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = sock.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text[..end]
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                        .unwrap_or(0);
                    if got.len() >= end + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            sock.write_all(reply).unwrap();
            String::from_utf8_lossy(&got).to_string()
        });
        (base, handle)
    }

    fn reply(head: &str, body: &str) -> &'static [u8] {
        let text = format!("HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", head, body.len(), body);
        Box::leak(text.into_bytes().into_boxed_slice())
    }

    #[test]
    fn token_exchange_posts_code_and_verifier() {
        let body = r#"{"token":"tok","user":{"id":"1","name":"Nick","role":null}}"#;
        let (base, handle) = stub(reply("200 OK", body));
        let (token, user) = HttpServer::new(&base, None).token_exchange("c0de", "veri").unwrap();
        assert_eq!(token, "tok");
        assert_eq!(user, User { id: "1".into(), name: "Nick".into(), role: None });
        let req = handle.join().unwrap();
        assert!(req.starts_with("POST /auth/token "));
        assert!(req.contains(r#""code":"c0de""#) && req.contains(r#""code_verifier":"veri""#));
    }

    #[test]
    fn token_exchange_surfaces_the_server_error() {
        let (base, _h) = stub(reply("400 Bad Request", r#"{"error":"bad_login","message":"Sign-in failed, try again"}"#));
        let err = HttpServer::new(&base, None).token_exchange("x", "y").unwrap_err();
        assert_eq!(err.code, "bad_login");
    }

    #[test]
    fn logout_sends_the_bearer_token() {
        let (base, handle) = stub(reply("200 OK", r#"{"ok":true}"#));
        HttpServer::new(&base, Some("tok".into())).logout().unwrap();
        let req = handle.join().unwrap();
        assert!(req.starts_with("POST /auth/logout "));
        assert!(req.to_ascii_lowercase().contains("authorization: bearer tok"));
    }

    #[test]
    fn parse_invite_accepts_server_links() {
        let want = ("https://packs.evilnick.net".to_string(), "abc_D-1".to_string());
        assert_eq!(parse_invite("https://packs.evilnick.net/i/abc_D-1").unwrap(), want);
        assert_eq!(parse_invite("  https://packs.evilnick.net/i/abc_D-1/ \n").unwrap(), want);
        assert_eq!(
            parse_invite("http://127.0.0.1:8787/i/xyz").unwrap(),
            ("http://127.0.0.1:8787".to_string(), "xyz".to_string())
        );
        assert_eq!(parse_invite("https://Packs.EvilNick.NET/i/abc_D-1").unwrap(), want);
        assert_eq!(parse_invite("HTTPS://packs.evilnick.net:443/i/abc_D-1").unwrap(), want);
    }

    #[test]
    fn parse_invite_rejects_other_links() {
        for link in [
            "https://drive.google.com/drive/folders/1AbC",
            "https://packs.evilnick.net/i/",
            "https://packs.evilnick.net/i/abc.def",
            "https://packs.evilnick.net/sub/i/abc",
            "http://packs.evilnick.net/i/abc",
            "https://user@packs.evilnick.net/i/abc",
            "",
        ] {
            assert_eq!(parse_invite(link).unwrap_err(), "That is not a pack invite link.");
        }
    }

    #[test]
    fn checksum_b64_matches_r2() {
        let hex = crate::hashing::sha256_bytes(b"hello world");
        assert_eq!(checksum_b64(&hex), "uU0nuZNNPgilLlLX2n2r+sSE7+N6U4DukIj3rOLvzek=");
    }

    #[test]
    fn describe_signed_out_names_host() {
        assert_eq!(
            describe("signed_out", "Sign in again", "https://packs.evilnick.net"),
            "Signed out of packs.evilnick.net. Sign in again in Settings."
        );
        assert_eq!(describe("weird", "Server said so", "https://packs.evilnick.net"), "Server said so");
    }

    #[test]
    fn describe_names_host_and_port_from_the_parsed_base() {
        let out = |base: &str| describe("signed_out", "", base);
        assert_eq!(out("http://127.0.0.1:8787"), "Signed out of 127.0.0.1:8787. Sign in again in Settings.");
        assert_eq!(out("https://packs.test:8443"), "Signed out of packs.test:8443. Sign in again in Settings.");
        assert_eq!(out("https://packs.test:443"), "Signed out of packs.test. Sign in again in Settings.");
        assert_eq!(out(""), "Signed out of the pack server. Sign in again in Settings.");
    }

    #[test]
    fn path_segments_are_percent_encoded() {
        let (base, req) = stub(reply("200 OK\r\nContent-Type: application/json", r#"{"versions":[]}"#));
        HttpServer::new(&base, None).changelog("a b/c?d#e%").unwrap();
        assert!(req.join().unwrap().starts_with("GET /packs/a%20b%2Fc%3Fd%23e%25/changelog "));

        let (base, req) = stub(reply("200 OK\r\nContent-Type: application/json", r#"{"invite_reset":false}"#));
        HttpServer::new(&base, None).remove_viewer("p-1", "u/1").unwrap();
        assert!(req.join().unwrap().starts_with("DELETE /packs/p-1/viewers/u%2F1 "));
    }

    #[test]
    fn commit_reads_published_at_when_the_server_sends_it() {
        let m = manifest("pack-a", "1.0", &[]);
        let (base, req) = stub(reply("200 OK\r\nContent-Type: application/json", r#"{"ok":true,"published_at":1234}"#));
        assert_eq!(HttpServer::new(&base, None).commit("pub/1", &m, "", &[]).unwrap(), Some(1234));
        assert!(req.join().unwrap().starts_with("POST /publishes/pub%2F1/commit "));

        let (base, req) = stub(reply("200 OK\r\nContent-Type: application/json", r#"{"ok":true}"#));
        assert_eq!(HttpServer::new(&base, None).commit("p", &m, "", &[]).unwrap(), None);
        req.join().unwrap();
    }

    /// Answers one connection per reply in turn and hands back each raw request head
    fn stub_seq(replies: Vec<&'static [u8]>) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut heads = Vec::new();
            for reply in replies {
                let (mut sock, _) = listener.accept().unwrap();
                let mut got = Vec::new();
                let mut buf = [0u8; 4096];
                while !String::from_utf8_lossy(&got).contains("\r\n\r\n") {
                    let n = sock.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    got.extend_from_slice(&buf[..n]);
                }
                sock.write_all(reply).unwrap();
                heads.push(String::from_utf8_lossy(&got).to_ascii_lowercase());
            }
            heads
        });
        (base, handle)
    }

    #[test]
    fn http_download_resumes_after_a_dropped_connection() {
        let dir = scratch("server-resume");
        // The first answer promises ten bytes and the connection closes after five
        let (base, req) = stub_seq(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nhello",
            reply("206 Partial Content\r\nContent-Range: bytes 5-9/10", "world"),
        ]);
        let dest = dir.join("blob.part");
        let mut seen = 0;
        let sha = HttpServer::new(&base, None).download("ab", &dest, 10, &|| false, &mut |n| seen += n).unwrap();
        let heads = req.join().unwrap();
        assert_eq!(heads.len(), 2);
        assert!(!heads[0].contains("range:"));
        assert!(heads[1].starts_with("get /blobs/ab ") && heads[1].contains("range: bytes=5-\r\n"));
        assert_eq!(std::fs::read(&dest).unwrap(), b"helloworld");
        assert_eq!(sha, crate::hashing::sha256_bytes(b"helloworld"));
        assert_eq!(seen, 10);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn http_download_restarts_when_the_server_ignores_the_range() {
        let dir = scratch("server-restart");
        let (base, req) = stub_seq(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nhello",
            reply("200 OK", "helloworld"),
        ]);
        let dest = dir.join("blob.part");
        let sha = HttpServer::new(&base, None).download("ab", &dest, 10, &|| false, &mut |_| {}).unwrap();
        assert_eq!(req.join().unwrap().len(), 2);
        assert_eq!(std::fs::read(&dest).unwrap(), b"helloworld");
        assert_eq!(sha, crate::hashing::sha256_bytes(b"helloworld"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_json_error_is_unavailable() {
        let (base, req) = stub(reply("503 Service Unavailable\r\nContent-Type: text/html", "<html></html>"));
        let err = HttpServer::new(&base, Some("t".into())).me().unwrap_err();
        req.join().unwrap();
        assert_eq!(err.code, "unavailable");
        assert_eq!(
            err.to_string(),
            "The pack server is not answering. Its daily limit may be reached, try again after 00:00 UTC."
        );
    }

    #[test]
    fn connection_failure_is_unavailable() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        assert_eq!(HttpServer::new(&base, None).me().unwrap_err().code, "unavailable");
    }

    #[test]
    fn json_error_keeps_code_and_retry_after() {
        let (base, req) = stub(reply(
            "429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 60",
            r#"{"error":"rate_limited","message":"Too many requests"}"#,
        ));
        let err = HttpServer::new(&base, Some("t".into())).manifest("pack-1").unwrap_err();
        let raw = req.join().unwrap();
        assert!(raw.starts_with("GET /packs/pack-1/manifest "));
        assert_eq!(err.code, "rate_limited");
        assert_eq!(err.message, "Too many requests");
        assert_eq!(err.retry_after, Some(60));
    }

    #[test]
    fn http_server_sends_bearer() {
        let (base, req) = stub(reply(
            "200 OK\r\nContent-Type: application/json",
            r#"{"id":"1","name":"Nick","role":"owner","packs":[]}"#,
        ));
        let me = HttpServer::new(&base, Some("tok123".into())).me().unwrap();
        let raw = req.join().unwrap().to_ascii_lowercase();
        assert!(raw.contains("authorization: bearer tok123\r\n"));
        assert!(raw.contains(&format!("user-agent: ron-mod-manager/{}\r\n", env!("CARGO_PKG_VERSION"))));
        assert_eq!(me.role.as_deref(), Some("owner"));
        assert!(me.packs.is_empty());
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ron-test-{}-{}", name, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn http_download_hashes_and_checks_size() {
        let dir = scratch("server-download");
        let (base, req) = stub(reply("200 OK\r\nContent-Type: application/octet-stream", "hello"));
        let server = HttpServer::new(&base, Some("t".into()));
        let mut seen = 0;
        let sha = server.download("ab", &dir.join("a"), 5, &|| false, &mut |n| seen += n).unwrap();
        assert!(req.join().unwrap().starts_with("GET /blobs/ab "));
        assert_eq!(sha, crate::hashing::sha256_bytes(b"hello"));
        assert_eq!(seen, 5);

        let (base, req) = stub(reply("200 OK\r\nContent-Type: application/octet-stream", "hello"));
        let err = HttpServer::new(&base, None).download("ab", &dir.join("b"), 3, &|| false, &mut |_| {}).unwrap_err();
        req.join().unwrap();
        assert_eq!(err.to_string(), SIZE_MISMATCH);
        assert!(!dir.join("b").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn put_timeout_grows_with_size() {
        assert_eq!(put_timeout(0), Duration::from_secs(300));
        assert_eq!(put_timeout(5 * 1024 * 1024 * 1024), Duration::from_secs(300 + 20480));
    }

    #[test]
    fn put_blob_sends_checksum_without_bearer() {
        let dir = scratch("server-put");
        let src = dir.join("f");
        std::fs::write(&src, b"hello world").unwrap();
        let sha = crate::hashing::sha256_bytes(b"hello world");
        let server = HttpServer::new("http://127.0.0.1:1", Some("secret".into()));

        let (base, req) = stub(reply("200 OK", ""));
        let mut seen = 0;
        server.put_blob(&format!("{}/blobs/x?sig=1", base), &src, 11, &sha, &mut |n| seen += n).unwrap();
        let raw = req.join().unwrap().to_ascii_lowercase();
        assert!(raw.starts_with("put /blobs/x?sig=1 "));
        assert!(raw.contains("content-length: 11\r\n"));
        assert!(raw.contains(&format!("x-amz-checksum-sha256: {}\r\n", checksum_b64(&sha).to_ascii_lowercase())));
        assert!(!raw.contains("authorization"));
        assert!(raw.ends_with("hello world"));
        assert_eq!(seen, 11);

        let (base, req) = stub(reply(
            "400 Bad Request\r\nContent-Type: application/xml",
            "<?xml version=\"1.0\"?><Error><Code>BadDigest</Code><Message>x</Message></Error>",
        ));
        let err = server.put_blob(&base, &src, 11, &sha, &mut |_| {}).unwrap_err();
        req.join().unwrap();
        assert_eq!(err.code, "upload_refused");
        assert_eq!(err.message, "400 BadDigest");
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::fake::{FakeServer, OWNER};

    fn manifest(pack_id: &str, version: &str, files: &[&[u8]]) -> RemoteManifest {
        RemoteManifest {
            format: FORMAT,
            pack_id: pack_id.into(),
            name: "Pack".into(),
            version: version.into(),
            status: "ready".into(),
            published_at: 0,
            published_by: None,
            mods: vec![RemoteMod {
                id: "m".into(),
                name: "M".into(),
                version: None,
                hash: "h".into(),
                addons: BTreeMap::new(),
                files: files
                    .iter()
                    .enumerate()
                    .map(|(i, b)| RemoteFileEntry {
                        path: format!("f{}", i),
                        size: b.len() as u64,
                        sha256: crate::hashing::sha256_bytes(b),
                    })
                    .collect(),
            }],
        }
    }

    fn blobs_of(m: &RemoteManifest) -> Vec<(String, u64)> {
        m.mods[0].files.iter().map(|f| (f.sha256.clone(), f.size)).collect()
    }

    /// Runs begin, PUT, verify and commit for `files`
    fn publish(up: &FakeServer, dir: &Path, m: &RemoteManifest, files: &[&[u8]], notes: &str, history: &[VersionEntry]) {
        let base = up.manifest(&m.pack_id).ok().map(|current| current.version);
        let begun = up.begin(&m.pack_id, &m.name, &m.version, base.as_deref(), &blobs_of(m)).unwrap();
        if !begun.missing.is_empty() {
            let urls = up.upload_urls(&begun.publish_id, &begun.missing).unwrap();
            for bytes in files {
                let sha = crate::hashing::sha256_bytes(bytes);
                if let Some(url) = urls.get(&sha) {
                    let src = dir.join(&sha);
                    std::fs::write(&src, bytes).unwrap();
                    up.put_blob(url, &src, bytes.len() as u64, &sha, &mut |_| {}).unwrap();
                }
            }
            let v = up.verify(&begun.publish_id, &begun.missing).unwrap();
            assert!(v.bad.is_empty());
        }
        up.commit(&begun.publish_id, m, notes, history).unwrap();
    }

    #[test]
    fn fake_publish_invite_and_download() {
        let dir = scratch("fake-publish");
        let owner = FakeServer::new();
        let up = owner.add_user("u1", "Uploader", Some("uploader"));
        let viewer = owner.add_user("v1", "Viewer", None);
        let files: [&[u8]; 2] = [b"one", b"two"];
        let m = manifest("pack-a", "1.0", &files);
        publish(&up, &dir, &m, &files, "first", &[]);
        assert_eq!((up.begins(), up.puts()), (1, 2));

        assert_eq!(viewer.manifest("pack-a").unwrap_err().code, "pack_gone");
        let link = up.pack_invite("pack-a").unwrap();
        assert_eq!(up.pack_invite("pack-a").unwrap(), link);
        let (base, code) = parse_invite(&link.url).unwrap();
        assert_eq!(base, viewer.base());
        assert_eq!(viewer.invite_info(&code).unwrap().pack.unwrap().uploader.name, "Uploader");
        assert_eq!(viewer.accept_invite(&code).unwrap().pack_id.as_deref(), Some("pack-a"));
        let me = viewer.me().unwrap();
        assert_eq!((me.packs[0].id.as_str(), me.packs[0].access.as_str()), ("pack-a", "viewer"));
        assert_eq!(owner.me().unwrap().packs[0].access, "owner");
        assert_eq!(up.me().unwrap().packs[0].access, "uploader");

        let got = viewer.manifest("pack-a").unwrap();
        assert_eq!(got.mods, m.mods);
        let f = &got.mods[0].files[0];
        assert_eq!(viewer.download(&f.sha256, &dir.join("d"), f.size, &|| false, &mut |_| {}).unwrap(), f.sha256);
        assert_eq!(std::fs::read(dir.join("d")).unwrap(), b"one");
        assert_eq!(viewer.downloads(), 1);

        // A second publish reuses stored blobs
        let begun = up.begin("pack-a", "Pack", "1.1", Some("1.0"), &blobs_of(&m)).unwrap();
        assert!(begun.missing.is_empty());
        up.cancel(&begun.publish_id).unwrap();
        assert_eq!(up.cancel(&begun.publish_id).unwrap_err().code, "publish_gone");

        assert_eq!(viewer.viewers("pack-a").unwrap_err().code, "forbidden");
        assert_eq!(up.viewers("pack-a").unwrap(), vec![Viewer { id: "v1".into(), name: "Viewer".into() }]);
        assert_eq!(up.remove_viewer("pack-a", "v1").unwrap(), RemovedViewer { invite_reset: true });
        assert_eq!(viewer.invite_info(&code).unwrap_err().code, "invite_gone");
        assert_eq!(viewer.leave("pack-a").unwrap_err().code, "pack_gone");
        assert_eq!(up.leave("pack-a").unwrap_err().code, "own_pack");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn pair(pack: &str) -> (FakeServer, FakeServer, FakeServer) {
        let owner = FakeServer::new();
        let creator = owner.add_user("u1", "Creator", Some("uploader"));
        let editor = owner.add_user("e1", "Editor", Some("uploader"));
        let m = manifest(pack, "1.0", &[]);
        owner.seed_pack("u1", &m, "");
        owner.add_editor(pack, "e1");
        (owner, creator, editor)
    }

    #[test]
    fn fake_editor_invite_makes_an_editor() {
        let owner = FakeServer::new();
        let creator = owner.add_user("u1", "Creator", Some("uploader"));
        let friend = owner.add_user("f1", "Friend", None);
        let other = owner.add_user("f2", "Other", None);
        owner.seed_pack("u1", &manifest("pack-e", "1.0", &[]), "");
        owner.add_viewer("pack-e", "f1");

        assert_eq!(friend.editor_invite("pack-e").unwrap_err().code, "forbidden");
        let link = creator.editor_invite("pack-e").unwrap();
        let (_, code) = parse_invite(&link.url).unwrap();
        assert_eq!(friend.invite_info(&code).unwrap().kind, "editor");
        let got = friend.accept_invite(&code).unwrap();
        assert_eq!((got.kind.as_str(), got.pack_id.as_deref()), ("editor", Some("pack-e")));
        assert_eq!(friend.me().unwrap().packs[0].access, "editor");
        assert_eq!(friend.me().unwrap().role.as_deref(), Some("uploader"));
        assert_eq!(creator.editors("pack-e").unwrap(), vec![Viewer { id: "f1".into(), name: "Friend".into() }]);
        assert!(creator.viewers("pack-e").unwrap().is_empty());
        assert_eq!(other.accept_invite(&code).unwrap_err().code, "invite_gone");

        // The creator and an editor accepting their own invite change nothing
        let again = creator.editor_invite("pack-e").unwrap();
        let (_, code) = parse_invite(&again.url).unwrap();
        assert_eq!(creator.accept_invite(&code).unwrap().kind, "editor");
        assert_eq!(creator.me().unwrap().packs[0].access, "uploader");
        assert_eq!(friend.accept_invite(&code).unwrap().kind, "editor");
        assert_eq!(creator.editors("pack-e").unwrap().len(), 1);

        creator.remove_editor("pack-e", "f1").unwrap();
        assert_eq!(friend.me().unwrap().packs[0].access, "viewer");
        assert!(creator.editors("pack-e").unwrap().is_empty());
        assert_eq!(creator.remove_editor("pack-e", "f1").unwrap_err().code, "user_gone");

        // The owner becomes a real editor, so they can publish
        assert_eq!(owner.accept_invite(&code).unwrap().kind, "editor");
        assert_eq!(owner.me().unwrap().packs[0].access, "editor");
        assert_eq!(owner.me().unwrap().role.as_deref(), Some("owner"));
        assert_eq!(creator.editors("pack-e").unwrap().len(), 1);
        assert_eq!(other.accept_invite(&code).unwrap_err().code, "invite_gone");
        assert!(owner.begin("pack-e", "Pack", "2.0", Some("1.0"), &[]).is_ok());
    }

    #[test]
    fn fake_editor_can_publish() {
        let dir = scratch("fake-editor-publish");
        let (owner, _creator, editor) = pair("pack-p");
        let m = manifest("pack-p", "2.0", &[b"new"]);
        publish(&editor, &dir, &m, &[b"new"], "", &[]);
        assert_eq!(owner.as_user("u1").manifest("pack-p").unwrap().version, "2.0");

        let rival = owner.add_user("u2", "Rival", Some("uploader"));
        assert_eq!(rival.begin("pack-p", "Pack", "3.0", Some("2.0"), &[]).unwrap_err().code, "pack_taken");
        let viewer = owner.add_user("v1", "Viewer", None);
        assert_eq!(viewer.begin("pack-p", "Pack", "3.0", Some("2.0"), &[]).unwrap_err().code, "pack_taken");

        // Once removed, an editor is refused again
        owner.as_user("u1").remove_editor("pack-p", "e1").unwrap();
        assert_eq!(editor.begin("pack-p", "Pack", "3.0", Some("2.0"), &[]).unwrap_err().code, "pack_taken");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fake_refuses_stale_base() {
        let (owner, creator, _editor) = pair("pack-s");
        assert_eq!(creator.begin("pack-s", "Pack", "2.0", None, &[]).unwrap_err().code, "pack_behind");
        assert_eq!(creator.begin("pack-s", "Pack", "2.0", Some("0.9"), &[]).unwrap_err().code, "pack_behind");
        assert_eq!(creator.open_publishes(), 0);

        // The pack moves on after begin
        let begun = creator.begin("pack-s", "Pack", "2.0", Some("1.0"), &[]).unwrap();
        owner.seed_pack("u1", &manifest("pack-s", "1.5", &[]), "");
        let e = creator.commit(&begun.publish_id, &manifest("pack-s", "2.0", &[]), "", &[]).unwrap_err();
        assert_eq!(e.code, "pack_behind");
        assert_eq!(creator.manifest("pack-s").unwrap().version, "1.5");

        // A brand new pack needs no base
        assert!(creator.begin("pack-new", "Pack", "1.0", None, &[]).is_ok());
    }

    #[test]
    fn fake_holds_a_pack_while_publishing() {
        let (owner, creator, editor) = pair("pack-h");
        let first = creator.begin("pack-h", "Pack", "2.0", Some("1.0"), &[]).unwrap();
        let busy = editor.begin("pack-h", "Pack", "2.0", Some("1.0"), &[]).unwrap_err();
        assert_eq!(busy.code, "pack_busy");
        assert_eq!(busy.message, "Creator is publishing this pack right now. Try again once it is done.");

        // The same user takes over their own open publish
        let second = creator.begin("pack-h", "Pack", "2.0", Some("1.0"), &[]).unwrap();
        assert_eq!(creator.cancel(&first.publish_id).unwrap_err().code, "publish_gone");

        assert_eq!(editor.clear_hold("pack-h").unwrap_err().code, "forbidden");
        assert_eq!(owner.clear_hold("pack-h").unwrap(), 1);
        assert_eq!(owner.clear_hold("pack-h").unwrap(), 0);
        assert_eq!(creator.upload_urls(&second.publish_id, &[]).unwrap_err().code, "publish_cleared");
        assert_eq!(creator.verify(&second.publish_id, &[]).unwrap_err().code, "publish_cleared");
        let third = editor.begin("pack-h", "Pack", "2.0", Some("1.0"), &[]).unwrap();

        // A hold lapses after an hour, and any step of the publish extends it
        owner.advance_clock(3000);
        editor.upload_urls(&third.publish_id, &[]).unwrap();
        owner.advance_clock(3000);
        assert_eq!(creator.begin("pack-h", "Pack", "2.0", Some("1.0"), &[]).unwrap_err().code, "pack_busy");
        owner.advance_clock(600);
        assert!(creator.begin("pack-h", "Pack", "2.0", Some("1.0"), &[]).is_ok());
    }

    #[test]
    fn fake_records_publisher() {
        let dir = scratch("fake-publisher");
        let (_owner, creator, editor) = pair("pack-r");
        publish(&editor, &dir, &manifest("pack-r", "2.0", &[]), &[], "", &[]);
        assert_eq!(creator.manifest("pack-r").unwrap().published_by.as_deref(), Some("Editor"));
        let log = creator.changelog("pack-r").unwrap();
        assert_eq!(log[0].publisher.as_deref(), Some("Editor"));
        publish(&creator, &dir, &manifest("pack-r", "3.0", &[]), &[], "", &[]);
        assert_eq!(creator.changelog("pack-r").unwrap()[0].publisher.as_deref(), Some("Creator"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn describe_pack_behind() {
        assert_eq!(
            describe("pack_behind", "ignored", fake::BASE),
            "A newer version of this pack is on the server. Update to it, make your changes again and publish."
        );
        let busy = "Sam is publishing this pack right now. Try again once it is done.";
        assert_eq!(describe("pack_busy", busy, fake::BASE), busy);
    }

    #[test]
    fn fake_commit_needs_verified_uploads_and_matching_manifest() {
        let dir = scratch("fake-commit");
        let owner = FakeServer::new();
        let up = owner.add_user("u1", "Uploader", Some("uploader"));
        let files: [&[u8]; 1] = [b"data"];
        let m = manifest("pack-b", "1.0", &files);
        let begun = up.begin("pack-b", "Pack", "1.0", None, &blobs_of(&m)).unwrap();
        assert_eq!(up.commit(&begun.publish_id, &m, "", &[]).unwrap_err().code, "not_uploaded");
        let mut other = m.clone();
        other.mods[0].files[0].size = 9;
        assert_eq!(up.commit(&begun.publish_id, &other, "", &[]).unwrap_err().code, "bad_manifest");
        assert_eq!(up.commit(&begun.publish_id, &m, &"x".repeat(32001), &[]).unwrap_err().code, "bad_changelog");
        let long = VersionEntry { version: "v".repeat(201), published_at: 1, notes: String::new(), publisher: None };
        assert_eq!(up.commit(&begun.publish_id, &m, "", &[long]).unwrap_err().code, "bad_changelog");

        let sha = &begun.missing[0];
        let url = &up.upload_urls(&begun.publish_id, &begun.missing).unwrap()[sha];
        assert!(url.starts_with(&format!("fake://blobs/{}?", sha)));
        let src = dir.join("src");
        std::fs::write(&src, b"datx").unwrap();
        assert_eq!(up.put_blob(url, &src, 4, sha, &mut |_| {}).unwrap_err().code, "upload_refused");
        std::fs::write(&src, b"data").unwrap();
        up.put_blob(url, &src, 4, sha, &mut |_| {}).unwrap();
        assert_eq!(up.commit(&begun.publish_id, &m, "", &[]).unwrap_err().code, "not_uploaded");
        up.verify(&begun.publish_id, &begun.missing).unwrap();
        up.commit(&begun.publish_id, &m, "", &[]).unwrap();
        assert_eq!(up.pack_blobs("pack-b"), vec![sha.clone()]);
        assert_eq!(up.upload_urls(&begun.publish_id, &begun.missing).unwrap_err().code, "publish_gone");

        let viewer = owner.add_user("v1", "Viewer", None);
        assert_eq!(viewer.begin("pack-b", "Pack", "2", Some("1.0"), &[]).unwrap_err().code, "pack_taken");
        let rival = owner.add_user("u2", "Rival", Some("uploader"));
        assert_eq!(rival.begin("pack-b", "Pack", "2", Some("1.0"), &[]).unwrap_err().code, "pack_taken");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fake_changelog_seeds_once_and_keeps_100() {
        let dir = scratch("fake-changelog");
        let owner = FakeServer::new();
        let up = owner.add_user("u1", "Uploader", Some("uploader"));
        let entry = |v: &str, at: u64| VersionEntry { version: v.into(), published_at: at, notes: String::new(), publisher: None };
        let history: Vec<VersionEntry> = (0..99).map(|i| entry(&format!("h{}", i), i)).collect();
        publish(&up, &dir, &manifest("pack-c", "1.0", &[b"a"]), &[b"a"], "n", &history);
        let log = up.changelog("pack-c").unwrap();
        assert_eq!(log.len(), 100);
        assert_eq!((log[0].version.as_str(), log[0].notes.as_str()), ("1.0", "n"));
        assert_eq!(log[1], history[98]);
        assert_eq!(log[99], history[0]);

        publish(&up, &dir, &manifest("pack-c", "1.1", &[b"a"]), &[b"a"], "", &[entry("x", 1)]);
        let log = up.changelog("pack-c").unwrap();
        assert_eq!(log.len(), 100);
        assert_eq!(log[0].version, "1.1");
        assert_eq!(log[99].version, "h1");
        assert!(log.iter().all(|e| e.version != "x"));
        assert_eq!(owner.add_user("v1", "V", None).changelog("pack-c").unwrap_err().code, "pack_gone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fake_knobs() {
        let dir = scratch("fake-knobs");
        let owner = FakeServer::new();
        let a = owner.store_blob(b"old");
        let b = owner.store_blob(b"new");
        owner.seed_pack(OWNER, &manifest("pack-d", "1.0", &[b"old"]), "");
        let get = |sha: &str| owner.download(sha, &dir.join("x"), 3, &|| false, &mut |_| {});

        assert_eq!(owner.download(&a, &dir.join("x"), 4, &|| false, &mut |_| {}).unwrap_err().code, "size_mismatch");
        assert_eq!(owner.downloads(), 0);
        owner.rate_limit_downloads(1);
        let e = get(&a).unwrap_err();
        assert_eq!((e.code.as_str(), e.retry_after), ("rate_limited", Some(0)));
        owner.replace_version_after(1, manifest("pack-d", "2.0", &[b"new"]));
        get(&a).unwrap();
        assert_eq!(get(&a).unwrap_err().code, "blob_gone");
        get(&b).unwrap();
        assert_eq!(owner.manifest("pack-d").unwrap().version, "2.0");
        assert_eq!(owner.downloads(), 2);

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        owner.cancel_after(0, move || flag.store(true, std::sync::atomic::Ordering::SeqCst));
        let e = owner.download(&b, &dir.join("y"), 3, &|| stop.load(std::sync::atomic::Ordering::SeqCst), &mut |_| {}).unwrap_err();
        assert_eq!(e.code, "cancelled");

        owner.fail_daily_cap(2);
        assert_eq!(owner.me().unwrap_err().code, "unavailable");
        assert_eq!(owner.users().unwrap_err().code, "unavailable");
        owner.me().unwrap();
        owner.expire_session();
        assert_eq!(owner.me().unwrap_err().to_string(), "Signed out of the pack server. Sign in again in Settings.");
        assert_eq!(owner.me().unwrap_err().text(owner.base()), "Signed out of packs.test. Sign in again in Settings.");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fake_invites_expire_after_a_day() {
        let owner = FakeServer::new();
        owner.seed_pack(OWNER, &manifest("pack-g", "1.0", &[]), "");
        let viewer = owner.add_user("v1", "Viewer", None);
        let first = owner.pack_invite("pack-g").unwrap();
        let (_, code) = parse_invite(&first.url).unwrap();
        owner.advance_clock(86399);
        viewer.invite_info(&code).unwrap();
        owner.advance_clock(1);
        assert_eq!(viewer.invite_info(&code).unwrap_err().code, "invite_gone");
        assert_eq!(viewer.accept_invite(&code).unwrap_err().code, "invite_gone");
        let second = owner.pack_invite("pack-g").unwrap();
        assert_ne!(second.url, first.url);
        let (_, code) = parse_invite(&owner.uploader_invite().unwrap().url).unwrap();
        owner.advance_clock(86400);
        assert_eq!(viewer.accept_invite(&code).unwrap_err().code, "invite_gone");
    }

    #[test]
    fn fake_accept_answers_server_full() {
        let owner = FakeServer::new();
        owner.seed_pack(OWNER, &manifest("pack-h", "1.0", &[]), "");
        owner.seed_pack(OWNER, &manifest("pack-i", "1.0", &[]), "");
        let up = owner.add_user("u1", "Uploader", Some("uploader"));
        let viewer = owner.add_user("v1", "Viewer", None);
        let fresh = owner.add_user("n1", "Newbie", None);
        let code = |pack: &str| parse_invite(&owner.pack_invite(pack).unwrap().url).unwrap().1;
        viewer.accept_invite(&code("pack-h")).unwrap();
        // Owner, uploader and viewer fill a server of three
        owner.set_max_users(3);
        assert_eq!(fresh.accept_invite(&code("pack-h")).unwrap_err().code, "server_full");
        let (_, upload) = parse_invite(&owner.uploader_invite().unwrap().url).unwrap();
        assert_eq!(fresh.accept_invite(&upload).unwrap_err().code, "server_full");
        // People already counted still get in
        viewer.accept_invite(&code("pack-i")).unwrap();
        up.accept_invite(&code("pack-i")).unwrap();
        owner.set_max_users(4);
        fresh.accept_invite(&code("pack-h")).unwrap();
    }

    #[test]
    fn fake_upload_url_outlives_its_publish() {
        let dir = scratch("fake-url");
        let owner = FakeServer::new();
        let up = owner.add_user("u1", "Uploader", Some("uploader"));
        let m = manifest("pack-j", "1.0", &[b"data"]);
        let src = dir.join("src");
        std::fs::write(&src, b"data").unwrap();
        let sha = crate::hashing::sha256_bytes(b"data");

        let begun = up.begin("pack-j", "Pack", "1.0", None, &blobs_of(&m)).unwrap();
        let url = up.upload_urls(&begun.publish_id, &begun.missing).unwrap()[&sha].clone();
        up.cancel(&begun.publish_id).unwrap();
        up.put_blob(&url, &src, 4, &sha, &mut |_| {}).unwrap();
        assert_eq!(up.blob(&sha).unwrap(), b"data");

        let begun = up.begin("pack-j", "Pack", "1.0", None, &blobs_of(&m)).unwrap();
        let url = up.upload_urls(&begun.publish_id, &begun.missing).unwrap()[&sha].clone();
        owner.advance_clock(300);
        let e = up.put_blob(&url, &src, 4, &sha, &mut |_| {}).unwrap_err();
        assert_eq!((e.code.as_str(), e.message.as_str()), ("upload_refused", "403 AccessDenied"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fake_commit_answers_published_at() {
        let dir = scratch("fake-published-at");
        let owner = FakeServer::new();
        let up = owner.add_user("u1", "Uploader", Some("uploader"));
        let m = manifest("pack-k", "1.0", &[]);
        let begun = up.begin("pack-k", "Pack", "1.0", None, &[]).unwrap();
        let at = up.commit(&begun.publish_id, &m, "", &[]).unwrap();
        assert_eq!(at, Some(up.changelog("pack-k").unwrap()[0].published_at));
        assert_eq!(at, Some(up.manifest("pack-k").unwrap().published_at));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fake_owner_admin() {
        let owner = FakeServer::new();
        let up = owner.add_user("u1", "Uploader", Some("uploader"));
        let fresh = owner.add_user("n1", "Newbie", None);
        assert_eq!(up.uploader_invite().unwrap_err().code, "forbidden");
        let (_, code) = parse_invite(&owner.uploader_invite().unwrap().url).unwrap();
        assert_eq!(fresh.accept_invite(&code).unwrap(), Accepted { kind: "uploader".into(), pack_id: None });
        assert_eq!(fresh.me().unwrap().role.as_deref(), Some("uploader"));
        assert_eq!(fresh.accept_invite(&code).unwrap_err().code, "invite_gone");
        owner.seed_pack("u1", &manifest("pack-e", "1.0", &[]), "");
        let users = owner.users().unwrap();
        assert_eq!(users.iter().find(|u| u.id == "u1").unwrap().packs, 1);
        assert_eq!(owner.remove_user(OWNER).unwrap_err().code, "self_remove");
        owner.remove_user("u1").unwrap();
        assert!(!owner.has_pack("pack-e"));
        assert_eq!(up.me().unwrap_err().code, "signed_out");
        assert_eq!(owner.remove_user("nobody").unwrap_err().code, "user_gone");
        owner.seed_pack(OWNER, &manifest("pack-f", "1.0", &[]), "");
        owner.delete_pack("pack-f").unwrap();
        assert_eq!(owner.delete_pack("pack-f").unwrap_err().code, "pack_gone");
    }

    #[test]
    fn fake_removing_an_editor_rotates_the_viewer_invite() {
        let (owner, creator, _editor) = pair("pack-i");
        let friend = owner.add_user("f1", "Friend", None);
        let (_, code) = parse_invite(&creator.pack_invite("pack-i").unwrap().url).unwrap();
        assert_eq!(friend.invite_info(&code).unwrap().kind, "viewer");
        owner.remove_user("e1").unwrap();
        assert_eq!(friend.invite_info(&code).unwrap_err().code, "invite_gone");
        assert!(creator.editors("pack-i").unwrap().is_empty());
    }
}
