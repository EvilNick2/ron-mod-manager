#![allow(dead_code)]

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::hashing;

pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
pub const SIZE_MISMATCH: &str = "The download did not match its expected size";

const FILES_URL: &str = "https://www.googleapis.com/drive/v3/files";
const UPLOAD_URL: &str = "https://www.googleapis.com/upload/drive/v3/files";
const ABOUT_URL: &str = "https://www.googleapis.com/drive/v3/about";
const UPLOAD_CHUNK: u64 = 8 * 1024 * 1024;
const MAX_ATTEMPTS: usize = 5;

#[derive(Deserialize)]
#[serde(untagged)]
enum SizeValue {
    Text(String),
    Number(u64),
}

fn size_from_json<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    match Option::<SizeValue>::deserialize(d)? {
        None => Ok(None),
        Some(SizeValue::Number(n)) => Ok(Some(n)),
        Some(SizeValue::Text(s)) => s.parse().map(Some).map_err(serde::de::Error::custom),
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFile {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    #[serde(default, deserialize_with = "size_from_json")]
    pub size: Option<u64>,
    #[serde(default, rename = "sha256Checksum")]
    pub sha256: Option<String>,
    #[serde(default)]
    pub resource_key: Option<String>,
}

impl RemoteFile {
    pub fn is_folder(&self) -> bool {
        self.mime_type == FOLDER_MIME
    }
}

pub struct Quota {
    pub limit: Option<u64>,
    pub usage: u64,
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
}

pub trait DriveApi {
    fn list(&self, folder_id: &str) -> Result<Vec<RemoteFile>, String>;
    fn read(&self, file_id: &str) -> Result<Vec<u8>, String>;
    /// Fails with SIZE_MISMATCH, leaving no file, when the content is not exactly expected_size bytes
    fn download(&self, file_id: &str, dest: &Path, expected_size: u64, on_bytes: &mut dyn FnMut(u64)) -> Result<String, String>;
    fn find_folder(&self, parent: Option<&str>, name: &str) -> Result<Option<String>, String>;
    fn create_folder(&self, parent: Option<&str>, name: &str) -> Result<String, String>;
    fn share_anyone(&self, file_id: &str) -> Result<(), String>;
    fn upload(&self, parent: &str, name: &str, src: &Path, on_bytes: &mut dyn FnMut(u64)) -> Result<String, String>;
    fn create_bytes(&self, parent: &str, name: &str, bytes: &[u8]) -> Result<String, String>;
    fn update_bytes(&self, file_id: &str, bytes: &[u8]) -> Result<(), String>;
    fn delete(&self, file_id: &str) -> Result<(), String>;
    fn quota(&self) -> Result<Quota, String>;
}

fn escape_query(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

fn list_query(folder_id: &str) -> String {
    format!("'{}' in parents and trashed=false", escape_query(folder_id))
}

fn resource_key_header(keys: &[(String, String)]) -> Option<String> {
    if keys.is_empty() {
        return None;
    }
    Some(keys.iter().map(|(id, key)| format!("{}/{}", id, key)).collect::<Vec<_>>().join(","))
}

enum Auth {
    Public { api_key: String, resource_keys: Option<String> },
    Bearer(Box<dyn Fn() -> Result<String, String> + Send + Sync>),
}

pub struct HttpDrive {
    client: Client,
    auth: Auth,
}

fn build_client() -> Client {
    Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(300))
        .build()
        .unwrap_or_else(|_| Client::new())
}

fn drive_error(status: u16, body: &str) -> String {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.trim().to_string());
    format!("Google Drive error {}: {}", status, message)
}

fn is_rate_limited(status: u16, body: &str) -> bool {
    if status == 429 || status >= 500 {
        return true;
    }
    if status != 403 {
        return false;
    }
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    v["error"]["errors"].as_array().is_some_and(|errs| {
        errs.iter().any(|e| matches!(e["reason"].as_str(), Some("rateLimitExceeded") | Some("userRateLimitExceeded")))
    })
}

impl HttpDrive {
    pub fn public(api_key: &str, resource_keys: Vec<(String, String)>) -> HttpDrive {
        HttpDrive {
            client: build_client(),
            auth: Auth::Public { api_key: api_key.to_string(), resource_keys: resource_key_header(&resource_keys) },
        }
    }

    pub fn signed_in(token: Box<dyn Fn() -> Result<String, String> + Send + Sync>) -> HttpDrive {
        HttpDrive { client: build_client(), auth: Auth::Bearer(token) }
    }

    fn prepare(&self, rb: RequestBuilder) -> Result<RequestBuilder, String> {
        Ok(match &self.auth {
            Auth::Bearer(token) => rb.bearer_auth(token()?),
            Auth::Public { api_key, resource_keys } => {
                let rb = rb.query(&[("key", api_key.as_str())]);
                match resource_keys {
                    Some(h) => rb.header("X-Goog-Drive-Resource-Keys", h.as_str()),
                    None => rb,
                }
            }
        })
    }

    /// Sends the request built by `build`, retrying rate limits and server errors.
    /// Statuses in `accept` are returned as-is along with every 2xx.
    fn send(&self, accept: &[u16], build: impl Fn() -> RequestBuilder) -> Result<Response, String> {
        let mut last = String::new();
        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(Duration::from_secs(1 << (attempt - 1)));
            }
            let resp = match self.prepare(build())?.send() {
                Ok(r) => r,
                Err(e) => {
                    last = format!("Could not reach Google Drive: {}", e);
                    continue;
                }
            };
            let status = resp.status().as_u16();
            if resp.status().is_success() || accept.contains(&status) {
                return Ok(resp);
            }
            let body = resp.text().unwrap_or_default();
            last = drive_error(status, &body);
            if !is_rate_limited(status, &body) {
                return Err(last);
            }
        }
        Err(last)
    }

    fn json<T: serde::de::DeserializeOwned>(resp: Response) -> Result<T, String> {
        resp.json().map_err(|e| format!("Unexpected response from Google Drive: {}", e))
    }

    fn create_metadata(&self, parent: Option<&str>, name: &str, mime: Option<&str>) -> Result<String, String> {
        let mut meta = serde_json::json!({ "name": name, "parents": [parent.unwrap_or("root")] });
        if let Some(m) = mime {
            meta["mimeType"] = m.into();
        }
        let resp = self.send(&[], || self.client.post(FILES_URL).query(&[("fields", "id")]).json(&meta))?;
        let v: serde_json::Value = Self::json(resp)?;
        v["id"].as_str().map(str::to_string).ok_or_else(|| "Google Drive did not return an id".to_string())
    }

    /// Resumable upload where `read_at(offset, len)` supplies the bytes.
    fn upload_resumable(
        &self,
        parent: &str,
        name: &str,
        total: u64,
        read_at: &mut dyn FnMut(u64, usize) -> Result<Vec<u8>, String>,
        on_bytes: &mut dyn FnMut(u64),
    ) -> Result<String, String> {
        let meta = serde_json::json!({ "name": name, "parents": [parent] });
        let start = self.send(&[], || {
            self.client
                .post(UPLOAD_URL)
                .query(&[("uploadType", "resumable"), ("fields", "id")])
                .header("X-Upload-Content-Length", total.to_string())
                .json(&meta)
        })?;
        let session = start
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| "Google Drive did not start an upload session".to_string())?;

        let finish = |resp: Response| -> Result<String, String> {
            let v: serde_json::Value = Self::json(resp)?;
            v["id"].as_str().map(str::to_string).ok_or_else(|| "Google Drive did not return an id".to_string())
        };

        if total == 0 {
            let resp = self.send(&[], || {
                self.client.put(&session).header("Content-Length", "0").header("Content-Range", "bytes */0")
            })?;
            return finish(resp);
        }

        let mut offset = 0u64;
        let mut failures = 0usize;
        loop {
            let len = UPLOAD_CHUNK.min(total - offset);
            let chunk = read_at(offset, len as usize)?;
            let range = format!("bytes {}-{}/{}", offset, offset + len - 1, total);
            let attempt = self.prepare(self.client.put(&session))?.header("Content-Range", range).body(chunk).send();
            let outcome = match attempt {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if status == 200 || status == 201 {
                        on_bytes(total - offset);
                        return finish(resp);
                    } else if status == 308 {
                        let next = next_offset(&resp);
                        if next > offset {
                            Ok(next)
                        } else {
                            Err("Google Drive made no progress on the upload".to_string())
                        }
                    } else {
                        let body = resp.text().unwrap_or_default();
                        if is_rate_limited(status, &body) {
                            Err(drive_error(status, &body))
                        } else {
                            return Err(drive_error(status, &body));
                        }
                    }
                }
                Err(e) => Err(format!("Could not reach Google Drive: {}", e)),
            };
            match outcome {
                Ok(next) => {
                    on_bytes(next.saturating_sub(offset));
                    offset = next;
                    failures = 0;
                }
                Err(message) => {
                    failures += 1;
                    if failures >= MAX_ATTEMPTS {
                        return Err(message);
                    }
                    std::thread::sleep(Duration::from_secs(1 << (failures - 1).min(3)));
                    // Ask the session how much it has before sending the chunk again
                    let probe = self
                        .prepare(self.client.put(&session))?
                        .header("Content-Range", format!("bytes */{}", total))
                        .header("Content-Length", "0")
                        .send();
                    if let Ok(resp) = probe {
                        let status = resp.status().as_u16();
                        if status == 200 || status == 201 {
                            on_bytes(total - offset);
                            return finish(resp);
                        }
                        if status == 308 {
                            let next = next_offset(&resp);
                            on_bytes(next.saturating_sub(offset));
                            offset = next;
                        }
                    }
                }
            }
        }
    }
}

fn next_offset(resp: &Response) -> u64 {
    resp.headers()
        .get(reqwest::header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit('-').next())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|last| last + 1)
        .unwrap_or(0)
}

impl DriveApi for HttpDrive {
    fn list(&self, folder_id: &str) -> Result<Vec<RemoteFile>, String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            #[serde(default)]
            files: Vec<RemoteFile>,
            next_page_token: Option<String>,
        }
        let q = list_query(folder_id);
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let resp = self.send(&[], || {
                let mut rb = self.client.get(FILES_URL).query(&[
                    ("q", q.as_str()),
                    ("pageSize", "1000"),
                    ("fields", "nextPageToken,files(id,name,mimeType,size,sha256Checksum,resourceKey)"),
                ]);
                if let Some(t) = &token {
                    rb = rb.query(&[("pageToken", t.as_str())]);
                }
                rb
            })?;
            let page: Page = Self::json(resp)?;
            out.extend(page.files);
            match page.next_page_token {
                Some(t) => token = Some(t),
                None => return Ok(out),
            }
        }
    }

    fn read(&self, file_id: &str) -> Result<Vec<u8>, String> {
        let url = format!("{}/{}", FILES_URL, file_id);
        let resp = self.send(&[], || self.client.get(&url).query(&[("alt", "media")]))?;
        resp.bytes().map(|b| b.to_vec()).map_err(|e| format!("Could not read from Google Drive: {}", e))
    }

    fn download(&self, file_id: &str, dest: &Path, expected_size: u64, on_bytes: &mut dyn FnMut(u64)) -> Result<String, String> {
        let url = format!("{}/{}", FILES_URL, file_id);
        let mut resp = self.send(&[], || self.client.get(&url).query(&[("alt", "media")]))?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Could not create {}: {}", parent.display(), e))?;
        }
        let mut file = File::create(dest).map_err(|e| format!("Could not create {}: {}", dest.display(), e))?;
        let result = (|| -> Result<String, String> {
            let mut hasher = Sha256::new();
            let mut written = 0u64;
            let mut failures = 0usize;
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                match resp.read(&mut buf) {
                    Ok(0) if written != expected_size => return Err(SIZE_MISMATCH.to_string()),
                    Ok(0) => return Ok(hashing::hex(&hasher.finalize())),
                    Ok(n) if written + n as u64 > expected_size => return Err(SIZE_MISMATCH.to_string()),
                    Ok(n) => {
                        hasher.update(&buf[..n]);
                        file.write_all(&buf[..n]).map_err(|e| format!("Could not write {}: {}", dest.display(), e))?;
                        written += n as u64;
                        on_bytes(n as u64);
                    }
                    Err(e) => {
                        failures += 1;
                        if failures >= MAX_ATTEMPTS {
                            return Err(format!("Download from Google Drive failed: {}", e));
                        }
                        std::thread::sleep(Duration::from_secs(1 << (failures - 1).min(3)));
                        resp = self.send(&[], || {
                            let rb = self.client.get(&url).query(&[("alt", "media")]);
                            if written > 0 {
                                rb.header("Range", format!("bytes={}-", written))
                            } else {
                                rb
                            }
                        })?;
                        if written > 0 && resp.status().as_u16() != 206 {
                            file.set_len(0).and_then(|_| file.seek(SeekFrom::Start(0)).map(|_| ()))
                                .map_err(|e| format!("Could not rewrite {}: {}", dest.display(), e))?;
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

    fn find_folder(&self, parent: Option<&str>, name: &str) -> Result<Option<String>, String> {
        let q = format!(
            "name='{}' and mimeType='{}' and '{}' in parents and trashed=false",
            escape_query(name),
            FOLDER_MIME,
            escape_query(parent.unwrap_or("root"))
        );
        let resp = self.send(&[], || {
            self.client.get(FILES_URL).query(&[
                ("q", q.as_str()),
                ("pageSize", "1"),
                ("orderBy", "createdTime"),
                ("fields", "files(id)"),
            ])
        })?;
        let v: serde_json::Value = Self::json(resp)?;
        Ok(v["files"][0]["id"].as_str().map(str::to_string))
    }

    fn create_folder(&self, parent: Option<&str>, name: &str) -> Result<String, String> {
        self.create_metadata(parent, name, Some(FOLDER_MIME))
    }

    fn share_anyone(&self, file_id: &str) -> Result<(), String> {
        let url = format!("{}/{}/permissions", FILES_URL, file_id);
        let body = serde_json::json!({ "type": "anyone", "role": "reader" });
        self.send(&[], || self.client.post(&url).json(&body)).map(|_| ())
    }

    fn upload(&self, parent: &str, name: &str, src: &Path, on_bytes: &mut dyn FnMut(u64)) -> Result<String, String> {
        let total = fs::metadata(src).map_err(|e| format!("Could not read {}: {}", src.display(), e))?.len();
        let mut file = File::open(src).map_err(|e| format!("Could not open {}: {}", src.display(), e))?;
        let mut read_at = |offset: u64, len: usize| -> Result<Vec<u8>, String> {
            file.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; len];
            file.read_exact(&mut buf).map_err(|e| format!("Could not read {}: {}", src.display(), e))?;
            Ok(buf)
        };
        self.upload_resumable(parent, name, total, &mut read_at, on_bytes)
    }

    fn create_bytes(&self, parent: &str, name: &str, bytes: &[u8]) -> Result<String, String> {
        let mut read_at = |offset: u64, len: usize| Ok(bytes[offset as usize..offset as usize + len].to_vec());
        self.upload_resumable(parent, name, bytes.len() as u64, &mut read_at, &mut |_| {})
    }

    fn update_bytes(&self, file_id: &str, bytes: &[u8]) -> Result<(), String> {
        let url = format!("{}/{}", UPLOAD_URL, file_id);
        self.send(&[], || self.client.patch(&url).query(&[("uploadType", "media")]).body(bytes.to_vec())).map(|_| ())
    }

    fn delete(&self, file_id: &str) -> Result<(), String> {
        let url = format!("{}/{}", FILES_URL, file_id);
        self.send(&[404], || self.client.delete(&url)).map(|_| ())
    }

    fn quota(&self) -> Result<Quota, String> {
        let resp = self.send(&[], || self.client.get(ABOUT_URL).query(&[("fields", "storageQuota(limit,usage)")]))?;
        let v: serde_json::Value = Self::json(resp)?;
        let num = |key: &str| v["storageQuota"][key].as_str().and_then(|s| s.parse::<u64>().ok());
        Ok(Quota { limit: num("limit"), usage: num("usage").unwrap_or(0) })
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::cell::{Cell, RefCell};

    const ROOT: &str = "root";

    struct Node {
        id: String,
        name: String,
        parent: String,
        folder: bool,
        bytes: Vec<u8>,
        sha256: String,
        shared: bool,
    }

    pub struct FakeDrive {
        nodes: RefCell<Vec<Node>>,
        next_id: Cell<usize>,
        quota: RefCell<(Option<u64>, Option<u64>)>,
        fail_after: Cell<Option<usize>>,
        uploads: Cell<usize>,
        downloads: Cell<usize>,
        lists: Cell<usize>,
        finds: Cell<usize>,
    }

    impl FakeDrive {
        pub fn new() -> FakeDrive {
            FakeDrive {
                nodes: RefCell::new(Vec::new()),
                next_id: Cell::new(1),
                quota: RefCell::new((None, None)),
                fail_after: Cell::new(None),
                uploads: Cell::new(0),
                downloads: Cell::new(0),
                lists: Cell::new(0),
                finds: Cell::new(0),
            }
        }

        /// usage is added to the bytes stored in the fake
        pub fn set_quota(&self, limit: Option<u64>, usage: u64) {
            *self.quota.borrow_mut() = (limit, Some(usage));
        }

        /// Fails the (n+1)th upload from now once, then disarms
        pub fn fail_uploads_after(&self, n: usize) {
            self.fail_after.set(Some(self.uploads.get() + n));
        }

        /// Counts successful upload calls only, create_bytes and update_bytes are manifest writes
        pub fn uploads(&self) -> usize {
            self.uploads.get()
        }

        /// Counts download calls only, read is not counted
        pub fn downloads(&self) -> usize {
            self.downloads.get()
        }

        pub fn lists(&self) -> usize {
            self.lists.get()
        }

        pub fn finds(&self) -> usize {
            self.finds.get()
        }

        pub fn tree(&self, folder_id: &str) -> Vec<String> {
            let mut out = Vec::new();
            self.walk(folder_id, "", &mut out);
            out.sort();
            out
        }

        fn walk(&self, folder_id: &str, prefix: &str, out: &mut Vec<String>) {
            let kids: Vec<(String, String, bool)> = self
                .nodes
                .borrow()
                .iter()
                .filter(|n| n.parent == folder_id)
                .map(|n| (n.id.clone(), n.name.clone(), n.folder))
                .collect();
            for (id, name, folder) in kids {
                if folder {
                    out.push(format!("{}{}/", prefix, name));
                    self.walk(&id, &format!("{}{}/", prefix, name), out);
                } else {
                    out.push(format!("{}{}", prefix, name));
                }
            }
        }

        pub fn file_bytes(&self, folder_id: &str, path: &str) -> Option<Vec<u8>> {
            let mut current = folder_id.to_string();
            let nodes = self.nodes.borrow();
            let parts: Vec<&str> = path.split('/').collect();
            for (i, part) in parts.iter().enumerate() {
                let node = nodes.iter().find(|n| n.parent == current && n.name == *part)?;
                if i == parts.len() - 1 {
                    return if node.folder { None } else { Some(node.bytes.clone()) };
                }
                current = node.id.clone();
            }
            None
        }

        pub fn corrupt(&self, file_id: &str) {
            if let Some(n) = self.nodes.borrow_mut().iter_mut().find(|n| n.id == file_id) {
                match n.bytes.first_mut() {
                    Some(b) => *b ^= 0xff,
                    None => n.bytes.push(0),
                }
            }
        }

        pub fn is_shared(&self, id: &str) -> bool {
            self.nodes.borrow().iter().any(|n| n.id == id && n.shared)
        }

        fn check_parent(&self, parent: &str) -> Result<(), String> {
            if parent == ROOT || self.nodes.borrow().iter().any(|n| n.id == parent && n.folder) {
                Ok(())
            } else {
                Err(format!("Google Drive error 404: File not found: {}", parent))
            }
        }

        fn add(&self, parent: &str, name: &str, folder: bool, bytes: Vec<u8>) -> String {
            let id = format!("fake{:04}x", self.next_id.get());
            self.next_id.set(self.next_id.get() + 1);
            let sha256 = hashing::sha256_bytes(&bytes);
            self.nodes.borrow_mut().push(Node {
                id: id.clone(),
                name: name.to_string(),
                parent: parent.to_string(),
                folder,
                bytes,
                sha256,
                shared: false,
            });
            id
        }

        fn add_file(&self, parent: &str, name: &str, bytes: Vec<u8>) -> Result<String, String> {
            self.check_parent(parent)?;
            Ok(self.add(parent, name, false, bytes))
        }

        fn stored(&self, file_id: &str) -> Result<Vec<u8>, String> {
            self.nodes
                .borrow()
                .iter()
                .find(|n| n.id == file_id && !n.folder)
                .map(|n| n.bytes.clone())
                .ok_or_else(|| format!("Google Drive error 404: File not found: {}", file_id))
        }
    }

    impl DriveApi for FakeDrive {
        fn list(&self, folder_id: &str) -> Result<Vec<RemoteFile>, String> {
            self.lists.set(self.lists.get() + 1);
            self.check_parent(folder_id)?;
            Ok(self
                .nodes
                .borrow()
                .iter()
                .filter(|n| n.parent == folder_id)
                .map(|n| RemoteFile {
                    id: n.id.clone(),
                    name: n.name.clone(),
                    mime_type: if n.folder { FOLDER_MIME } else { "application/octet-stream" }.to_string(),
                    size: if n.folder { None } else { Some(n.bytes.len() as u64) },
                    sha256: if n.folder { None } else { Some(n.sha256.clone()) },
                    resource_key: None,
                })
                .collect())
        }

        fn read(&self, file_id: &str) -> Result<Vec<u8>, String> {
            self.stored(file_id)
        }

        fn download(&self, file_id: &str, dest: &Path, expected_size: u64, on_bytes: &mut dyn FnMut(u64)) -> Result<String, String> {
            let bytes = self.stored(file_id)?;
            self.downloads.set(self.downloads.get() + 1);
            if bytes.len() as u64 != expected_size {
                return Err(SIZE_MISMATCH.to_string());
            }
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::write(dest, &bytes).map_err(|e| e.to_string())?;
            on_bytes(bytes.len() as u64);
            Ok(hashing::sha256_bytes(&bytes))
        }

        fn find_folder(&self, parent: Option<&str>, name: &str) -> Result<Option<String>, String> {
            self.finds.set(self.finds.get() + 1);
            let parent = parent.unwrap_or(ROOT);
            Ok(self.nodes.borrow().iter().find(|n| n.folder && n.parent == parent && n.name == name).map(|n| n.id.clone()))
        }

        fn create_folder(&self, parent: Option<&str>, name: &str) -> Result<String, String> {
            let parent = parent.unwrap_or(ROOT);
            self.check_parent(parent)?;
            Ok(self.add(parent, name, true, Vec::new()))
        }

        fn share_anyone(&self, file_id: &str) -> Result<(), String> {
            match self.nodes.borrow_mut().iter_mut().find(|n| n.id == file_id) {
                Some(n) => {
                    n.shared = true;
                    Ok(())
                }
                None => Err(format!("Google Drive error 404: File not found: {}", file_id)),
            }
        }

        fn upload(&self, parent: &str, name: &str, src: &Path, on_bytes: &mut dyn FnMut(u64)) -> Result<String, String> {
            let bytes = fs::read(src).map_err(|e| format!("Could not read {}: {}", src.display(), e))?;
            let len = bytes.len() as u64;
            if self.fail_after.get().is_some_and(|limit| self.uploads.get() >= limit) {
                self.fail_after.set(None);
                return Err("Google Drive error 500: injected upload failure".to_string());
            }
            let id = self.add_file(parent, name, bytes)?;
            self.uploads.set(self.uploads.get() + 1);
            on_bytes(len);
            Ok(id)
        }

        fn create_bytes(&self, parent: &str, name: &str, bytes: &[u8]) -> Result<String, String> {
            self.add_file(parent, name, bytes.to_vec())
        }

        fn update_bytes(&self, file_id: &str, bytes: &[u8]) -> Result<(), String> {
            match self.nodes.borrow_mut().iter_mut().find(|n| n.id == file_id && !n.folder) {
                Some(n) => {
                    n.bytes = bytes.to_vec();
                    n.sha256 = hashing::sha256_bytes(bytes);
                    Ok(())
                }
                None => Err(format!("Google Drive error 404: File not found: {}", file_id)),
            }
        }

        fn delete(&self, file_id: &str) -> Result<(), String> {
            let mut doomed = vec![file_id.to_string()];
            let mut i = 0;
            while i < doomed.len() {
                let id = doomed[i].clone();
                doomed.extend(self.nodes.borrow().iter().filter(|n| n.parent == id).map(|n| n.id.clone()));
                i += 1;
            }
            self.nodes.borrow_mut().retain(|n| !doomed.contains(&n.id));
            Ok(())
        }

        fn quota(&self) -> Result<Quota, String> {
            let (limit, usage) = *self.quota.borrow();
            let stored: u64 = self.nodes.borrow().iter().map(|n| n.bytes.len() as u64).sum();
            Ok(Quota { limit, usage: usage.unwrap_or(0) + stored })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeDrive;
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn escape_query_quotes() {
        assert_eq!(escape_query("it's a\\b"), "it\\'s a\\\\b");
    }

    #[test]
    fn list_query_filters_trashed() {
        assert_eq!(list_query("abc"), "'abc' in parents and trashed=false");
    }

    #[test]
    fn resource_key_header_joins() {
        let keys = vec![("a".to_string(), "k1".to_string()), ("b".to_string(), "k2".to_string())];
        assert_eq!(resource_key_header(&keys), Some("a/k1,b/k2".to_string()));
        assert_eq!(resource_key_header(&[]), None);
    }

    #[test]
    fn remote_file_parses_drive_json() {
        let json = r#"{"md5Checksum":"5e00d36bd9a2c539f00f6887c4488d95","mimeType":"application/octet-stream","size":"1610612736","id":"1y0Mg0ndMYPdgHI07skpq-MF3Se5HGnYu","name":"spike-big.bin","sha256Checksum":"94e9cf1bad062698dbabecc1344cdcb54ba68ea589e0fcf718add9360b815d79"}"#;
        let f: RemoteFile = serde_json::from_str(json).unwrap();
        assert_eq!(f.size, Some(1610612736));
        assert_eq!(f.name, "spike-big.bin");
        assert_eq!(f.sha256.as_deref(), Some("94e9cf1bad062698dbabecc1344cdcb54ba68ea589e0fcf718add9360b815d79"));
        assert!(!f.is_folder());
    }

    #[test]
    fn remote_folder_parses_without_size() {
        let json = r#"{"id":"f1","name":"Pack","mimeType":"application/vnd.google-apps.folder"}"#;
        let f: RemoteFile = serde_json::from_str(json).unwrap();
        assert!(f.is_folder());
        assert_eq!(f.size, None);
        assert_eq!(f.sha256, None);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ron-test-{}-{}", name, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fake_round_trip() {
        let drive = FakeDrive::new();
        let dir = scratch("drive-round-trip");
        let src = dir.join("a.bin");
        std::fs::write(&src, b"hello").unwrap();

        let folder = drive.create_folder(None, "Pack").unwrap();
        assert_eq!(drive.find_folder(None, "Pack").unwrap(), Some(folder.clone()));
        let sub = drive.create_folder(Some(&folder), "Mod").unwrap();
        let mut seen = 0;
        let id = drive.upload(&sub, "a.bin", &src, &mut |n| seen += n).unwrap();
        assert_eq!(seen, 5);
        assert_eq!(drive.uploads(), 1);

        let listed = drive.list(&sub).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(listed[0].size, Some(5));
        assert_eq!(listed[0].sha256.as_deref(), Some(crate::hashing::sha256_bytes(b"hello").as_str()));
        assert_eq!(drive.list(&folder).unwrap().len(), 1);
        assert_eq!(drive.tree(&folder), vec!["Mod/".to_string(), "Mod/a.bin".to_string()]);
        assert_eq!(drive.file_bytes(&folder, "Mod/a.bin"), Some(b"hello".to_vec()));

        let dest = dir.join("out.bin");
        let sha = drive.download(&id, &dest, 5, &mut |_| {}).unwrap();
        assert_eq!(sha, listed[0].sha256.clone().unwrap());
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
        assert_eq!(drive.downloads(), 1);

        drive.delete(&id).unwrap();
        assert!(drive.list(&sub).unwrap().is_empty());
        drive.delete("nope").unwrap();

        drive.create_bytes(&sub, "b.txt", b"xy").unwrap();
        drive.delete(&folder).unwrap();
        assert!(drive.list(&sub).is_err());
        assert!(drive.tree(&folder).is_empty());
        assert_eq!(drive.find_folder(None, "Pack").unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fake_corrupt_is_caught_by_hash() {
        let drive = FakeDrive::new();
        let dir = scratch("drive-corrupt");
        let folder = drive.create_folder(None, "P").unwrap();
        let id = drive.create_bytes(&folder, "a", b"data").unwrap();
        drive.corrupt(&id);
        let reported = drive.list(&folder).unwrap()[0].sha256.clone().unwrap();
        let got = drive.download(&id, &dir.join("a"), 4, &mut |_| {}).unwrap();
        assert_eq!(drive.download(&id, &dir.join("b"), 3, &mut |_| {}).unwrap_err(), SIZE_MISMATCH);
        assert_eq!(drive.download(&id, &dir.join("b"), 5, &mut |_| {}).unwrap_err(), SIZE_MISMATCH);
        assert!(!dir.join("b").exists());
        assert_ne!(got, reported);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fake_fails_one_upload_after_n_and_tracks_quota_and_sharing() {
        let drive = FakeDrive::new();
        let dir = scratch("drive-fail");
        let src = dir.join("f");
        std::fs::write(&src, b"123").unwrap();
        let folder = drive.create_folder(None, "P").unwrap();
        drive.create_bytes(&folder, "manifest", b"m").unwrap();
        assert_eq!(drive.uploads(), 0);
        drive.fail_uploads_after(1);
        drive.upload(&folder, "a", &src, &mut |_| {}).unwrap();
        drive.create_bytes(&folder, "m2", b"m").unwrap();
        assert!(drive.upload(&folder, "b", &src, &mut |_| {}).is_err());
        assert_eq!(drive.uploads(), 1);
        drive.upload(&folder, "b", &src, &mut |_| {}).unwrap();
        assert_eq!(drive.uploads(), 2);
        assert_eq!(drive.quota().unwrap().usage, 8);
        drive.set_quota(Some(10), 0);
        assert_eq!(drive.quota().unwrap().limit, Some(10));
        assert!(!drive.is_shared(&folder));
        drive.share_anyone(&folder).unwrap();
        assert!(drive.is_shared(&folder));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
