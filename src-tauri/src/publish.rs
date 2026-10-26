use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use tauri::Emitter;

use crate::accounts::{self, Conn};
use crate::changelog::{self, Change};
use crate::hashing::{self, FileEntry};
use crate::packs::{self, ModState, PackManifest, PackRef, Published};
use crate::server::{
    PackServer, Progress, RemoteFileEntry, RemoteManifest, RemoteMod, ServerError, VersionEntry, CANCELLED, FORMAT,
};
use crate::subscribe::{running_packs, OpGuard, OpKind};

const MAX_BLOBS: usize = 2000;
const MAX_BATCH: usize = 50;
// R2 refuses a single PUT larger than 5 GiB
const MAX_BLOB_SIZE: u64 = 5 * 1024 * 1024 * 1024;
const MAX_NEXUS_CALLS: usize = 50;
const NO_UPLOAD_SERVER: &str = "Sign in to a pack server where you can upload first.";
/// Sent as the error when the user can upload to several servers, so the frontend asks which
pub const CHOOSE_SERVER: &str = "choose_server";

#[derive(Clone, Debug)]
pub struct LocalMod {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub nexus_mod_id: Option<u32>,
    pub dir: PathBuf,
    pub files: Vec<FileEntry>,
    pub addons: BTreeMap<String, bool>,
    pub hash: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind")]
pub enum PublishOutcome {
    Published { server: String, version: String, uploaded: usize, reused: usize },
    NothingToPublish { server: String },
}

const PROGRESS_EVERY: Duration = Duration::from_millis(250);

pub(crate) struct Throttle {
    last: Option<Instant>,
}

impl Throttle {
    pub(crate) fn new() -> Throttle {
        Throttle { last: None }
    }

    pub(crate) fn ready(&mut self) -> bool {
        let now = Instant::now();
        if self.last.is_some_and(|t| now.duration_since(t) < PROGRESS_EVERY) {
            return false;
        }
        self.last = Some(now);
        true
    }
}

pub(crate) fn progress_event(pack_id: &str, stage: &str, done: usize, total: usize, file: Option<&str>, bytes: (u64, u64)) -> Progress {
    Progress {
        pack_id: pack_id.to_string(),
        stage: stage.to_string(),
        done,
        total,
        file: file.map(str::to_string),
        bytes_done: bytes.0,
        bytes_total: bytes.1,
        overall_done: 0,
        overall_total: 0,
        wait_secs: 0,
    }
}

pub fn hash_local_pack(pack_dir: &Path, pack_id: &str, progress: &mut dyn FnMut(Progress)) -> Result<Vec<LocalMod>, String> {
    let manifest = packs::read_manifest(pack_dir).ok_or_else(|| format!("Could not read pack.json for {}", pack_id))?;
    let total = manifest.mod_ids.len();
    let mut mods = Vec::with_capacity(total);
    let mut fresh = BTreeMap::new();
    for (done, id) in manifest.mod_ids.iter().enumerate() {
        let dir = pack_dir.join(id);
        if !dir.is_dir() {
            return Err(format!("Mod {} is missing from {}", id, manifest.name));
        }
        let files = hashing::hash_mod_files_cached(&dir, id, &manifest.local_hashes, &mut fresh, &mut |path| {
            progress(progress_event(pack_id, "hashing", done, total, Some(path), (0, 0)));
        })?;
        let meta = crate::vfs::read_meta(&dir);
        let name = meta.as_ref().and_then(|m| m.get("name")).and_then(|n| n.as_str()).unwrap_or(id).to_string();
        check_portable(&name, &files)?;
        let addons = hashing::addon_states(&dir);
        let hash = hashing::mod_hash(files.iter().map(|f| (f.path.as_str(), f.sha256.as_str())), &addons);
        let version = crate::vfs::meta_version(meta.as_ref());
        // As the mod scan reads it
        let nexus_mod_id = meta
            .as_ref()
            .and_then(|m| m.get("mod_id"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .or_else(|| id.parse().ok());
        mods.push(LocalMod { id: id.clone(), name, version, nexus_mod_id, dir, files, addons, hash });
    }
    // Read-modify-write of pack.json, open to the same race as set_dirty
    if let Err(e) = save_local_hashes(pack_dir, fresh) {
        eprintln!("[PUBLISH] Could not save the hash cache for {}: {}", pack_id, e);
    }
    progress(progress_event(pack_id, "hashing", total, total, None, (0, 0)));
    Ok(mods)
}

// Players refuse a pack with a path Windows cannot store, so the creator is stopped first
fn check_portable(mod_name: &str, files: &[FileEntry]) -> Result<(), String> {
    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    for f in files {
        if crate::subscribe::safe_rel_path(&f.path).is_err() {
            return Err(format!("{}: {} cannot be published because Windows cannot store that name. Rename it first.", mod_name, f.path));
        }
        if let Some(other) = seen.insert(f.path.to_lowercase(), &f.path) {
            return Err(format!("{} has two files that differ only in case: {} and {}", mod_name, other, f.path));
        }
    }
    Ok(())
}

// Re-reads pack.json so only local_hashes changes, never a dirty flag set while hashing
fn save_local_hashes(pack_dir: &Path, fresh: BTreeMap<String, packs::CachedHash>) -> Result<(), String> {
    let mut manifest = packs::read_manifest(pack_dir).ok_or("Could not read pack.json")?;
    if manifest.local_hashes == fresh {
        return Ok(());
    }
    manifest.local_hashes = fresh;
    packs::write_manifest(pack_dir, &manifest)
}

// A file edited without a new mtime or size keeps a stale cached hash, so the next publish rehashes it
fn forget_cached_hash(pack_dir: &Path, key: &str) {
    let Some(mut manifest) = packs::read_manifest(pack_dir) else { return };
    if manifest.local_hashes.remove(key).is_some() {
        if let Err(e) = packs::write_manifest(pack_dir, &manifest) {
            eprintln!("[PUBLISH] Could not update the hash cache: {}", e);
        }
    }
}

// MAJOR.MINOR.PATCH with no leading zeros, as semver.org defines the core version
pub fn parse_semver(version: &str) -> Option<(u64, u64, u64)> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut nums = [0u64; 3];
    for (n, part) in nums.iter_mut().zip(&parts) {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) || (part.len() > 1 && part.starts_with('0')) {
            return None;
        }
        *n = part.parse().ok()?;
    }
    Some((nums[0], nums[1], nums[2]))
}

fn set_dirty(pack_dir: &Path, dirty: bool) -> Result<(), String> {
    let mut manifest = packs::read_manifest(pack_dir).ok_or("Could not read pack.json")?;
    if let Some(p) = manifest.published.as_mut() {
        p.dirty = dirty;
        packs::write_manifest(pack_dir, &manifest)?;
    }
    Ok(())
}

// Keeps the dirty flag on disk for an already published pack, so edits made during the publish survive
fn mark_published(pack_dir: &Path, server: &str, version: &str) -> Result<(), String> {
    let mut manifest = packs::read_manifest(pack_dir).ok_or("Could not read pack.json")?;
    match manifest.published.as_mut() {
        Some(p) => p.server = Some(server.to_string()),
        None => manifest.published = Some(Published { server: Some(server.to_string()), dirty: false }),
    }
    manifest.version = Some(version.to_string());
    packs::write_manifest(pack_dir, &manifest)
}

/// The server a publish goes to. `signed_in` pairs each signed-in server with the user's role there.
pub fn pick_server(published: Option<&str>, requested: Option<&str>, signed_in: &[(String, Option<String>)]) -> Result<String, String> {
    if let Some(base) = published {
        return Ok(base.to_string());
    }
    if let Some(base) = requested {
        return accounts::normalize_base(base);
    }
    let able: Vec<&String> = signed_in
        .iter()
        .filter(|(_, role)| matches!(role.as_deref(), Some("uploader" | "owner")))
        .map(|(base, _)| base)
        .collect();
    match able.as_slice() {
        [] => Err(NO_UPLOAD_SERVER.to_string()),
        [one] => Ok(one.to_string()),
        _ => Err(CHOOSE_SERVER.to_string()),
    }
}

struct Blob {
    size: u64,
    src: PathBuf,
    path: String,
    /// The hash cache key, "<mod id>/<path>"
    key: String,
}

/// Every distinct file content in the pack, keyed by sha256
fn blob_list(local: &[LocalMod]) -> Result<BTreeMap<String, Blob>, String> {
    let mut blobs = BTreeMap::new();
    for m in local {
        for f in &m.files {
            if f.size > MAX_BLOB_SIZE {
                return Err(format!("{}: {} is larger than 5 GiB. The server takes files up to 5 GiB.", m.name, f.path));
            }
            blobs.entry(f.sha256.clone()).or_insert_with(|| Blob {
                size: f.size,
                src: m.dir.join(&f.path),
                path: f.path.clone(),
                key: format!("{}/{}", m.id, f.path),
            });
        }
    }
    if blobs.len() > MAX_BLOBS {
        return Err(format!("This pack has more than {} files. The server takes at most {}.", MAX_BLOBS, MAX_BLOBS));
    }
    Ok(blobs)
}

fn local_states(local: &[LocalMod]) -> BTreeMap<String, ModState> {
    local
        .iter()
        .map(|m| {
            let state = ModState { name: m.name.clone(), version: m.version.clone(), hash: m.hash.clone(), nexus_mod_id: m.nexus_mod_id };
            (m.id.clone(), state)
        })
        .collect()
}

fn remote_states(manifest: &RemoteManifest) -> BTreeMap<String, ModState> {
    manifest
        .mods
        .iter()
        .map(|m| {
            let state = ModState { name: m.name.clone(), version: m.version.clone(), hash: m.hash.clone(), nexus_mod_id: m.id.parse().ok() };
            (m.id.clone(), state)
        })
        .collect()
}

fn same_mods(a: &BTreeMap<String, ModState>, b: &BTreeMap<String, ModState>) -> bool {
    a.len() == b.len() && a.iter().all(|(id, s)| b.get(id).is_some_and(|o| o.hash == s.hash))
}

/// The notes for this version, and on a first publish the history it inherits, oldest first
fn changelog_for(
    pack_dir: &Path,
    pack_id: &str,
    remote: Option<&RemoteManifest>,
    local: &[LocalMod],
    nexus: Option<&dyn Fn(u32) -> Result<Value, String>>,
    cancel: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(Progress),
) -> Result<(String, Vec<VersionEntry>), String> {
    progress(progress_event(pack_id, "changelog", 0, 1, None, (0, 0)));
    let saved = packs::read_changelog(pack_dir);
    let first = remote.is_none();
    let old = match remote {
        Some(m) => remote_states(m),
        None => saved.base.as_ref().map(|b| b.mods.clone()).unwrap_or_default(),
    };
    let changes = changelog::diff(&old, &local_states(local));
    let mut lines = BTreeMap::new();
    let updated = changes.iter().filter_map(|c| match c {
        Change::Updated { mod_id, from, to, nexus_mod_id: Some(id), .. } => Some((mod_id, from, to, *id)),
        _ => None,
    });
    // No Nexus key skips every call
    if let Some(fetch) = nexus {
        for (mod_id, from, to, id) in updated.take(MAX_NEXUS_CALLS) {
            if cancel() {
                return Err(CANCELLED.to_string());
            }
            match fetch(id) {
                Ok(log) => {
                    let found = changelog::nexus_lines(&log, from.as_deref(), to.as_deref());
                    if !found.is_empty() {
                        lines.insert(mod_id.clone(), found);
                    }
                }
                Err(e) => eprintln!("[PUBLISH] No Nexus changelog for mod {}: {}", id, e),
            }
        }
    }
    let forked = if first { saved.base.as_ref().map(|b| (b.name.as_str(), b.version.as_deref())) } else { None };
    let notes = changelog::fit(changelog::render(&changes, &lines, forked));
    let history = if first {
        let mut entries = changelog::fit_history(saved.entries);
        entries.reverse();
        entries
    } else {
        Vec::new()
    };
    progress(progress_event(pack_id, "changelog", 1, 1, None, (0, 0)));
    Ok((notes, history))
}

fn local_error(message: String) -> ServerError {
    ServerError::new("local", &message)
}

fn cancelled() -> ServerError {
    ServerError::new("cancelled", CANCELLED)
}

/// One publish's begin, uploads and commit, tried again once when the publish expires
struct Upload<'a> {
    server: &'a dyn PackServer,
    pack_dir: &'a Path,
    pack_id: &'a str,
    blobs: &'a BTreeMap<String, Blob>,
    cancel: &'a dyn Fn() -> bool,
    progress: &'a mut dyn FnMut(Progress),
    publish_id: String,
    done: usize,
    total: usize,
    sent: u64,
    total_bytes: u64,
    put: BTreeSet<String>,
}

impl Upload<'_> {
    fn stop(&self) -> ServerError {
        if !self.publish_id.is_empty() {
            if let Err(e) = self.server.cancel(&self.publish_id) {
                eprintln!("[PUBLISH] Could not cancel the publish on the server: {}", e);
            }
        }
        cancelled()
    }

    fn send(&mut self, name: &str, version: &str, manifest: &RemoteManifest, notes: &str, history: &[VersionEntry]) -> Result<(), ServerError> {
        self.publish_id.clear();
        if (self.cancel)() {
            return Err(cancelled());
        }
        (self.progress)(progress_event(self.pack_id, "checking_server", 0, 1, None, (0, 0)));
        let list: Vec<(String, u64)> = self.blobs.iter().map(|(h, b)| (h.clone(), b.size)).collect();
        let begun = self.server.begin(self.pack_id, name, version, &list)?;
        self.publish_id = begun.publish_id;
        self.done = 0;
        self.total = begun.missing.len();
        self.sent = 0;
        self.total_bytes = begun.missing.iter().filter_map(|h| self.blobs.get(h)).map(|b| b.size).sum();
        self.upload_missing(&begun.missing)?;
        if (self.cancel)() {
            return Err(self.stop());
        }
        match self.server.commit(&self.publish_id, manifest, notes, history) {
            Err(e) if e.code == "not_uploaded" => {
                self.upload_missing(&begun.missing)?;
                self.server.commit(&self.publish_id, manifest, notes, history)
            }
            r => r,
        }
    }

    // Verifies every batch first, so content a failed attempt already sent is not sent again
    fn upload_missing(&mut self, missing: &[String]) -> Result<(), ServerError> {
        let batches: Vec<&[String]> = missing.chunks(MAX_BATCH).collect();
        if batches.is_empty() {
            (self.progress)(progress_event(self.pack_id, "checking_server", 1, 1, None, (0, 0)));
            return Ok(());
        }
        (self.progress)(progress_event(self.pack_id, "checking_server", 0, batches.len(), None, (0, 0)));
        let mut pending = Vec::new();
        for (i, batch) in batches.iter().enumerate() {
            if (self.cancel)() {
                return Err(self.stop());
            }
            let bad = self.server.verify(&self.publish_id, batch)?.bad;
            for h in batch.iter().filter(|h| !bad.contains(h)) {
                self.done += 1;
                self.sent += self.blobs.get(h).map_or(0, |b| b.size);
            }
            if !bad.is_empty() {
                pending.push(bad);
            }
            (self.progress)(progress_event(self.pack_id, "checking_server", i + 1, batches.len(), None, (0, 0)));
        }
        for bad in pending {
            self.settle(bad)?;
        }
        Ok(())
    }

    /// Uploads hashes verify answered bad. Each is sent at most twice, a round answered already_stored does not count.
    fn settle(&mut self, mut pending: Vec<String>) -> Result<(), ServerError> {
        let (mut tries, mut rounds) = (0, 0);
        while !pending.is_empty() && tries < 2 && rounds < 4 {
            rounds += 1;
            if self.upload(&pending)? {
                tries += 1;
            }
            pending = self.server.verify(&self.publish_id, &pending)?.bad;
        }
        match pending.first().and_then(|h| self.blobs.get(h)) {
            None => Ok(()),
            Some(b) => Err(local_error(format!("The pack server did not accept {}. Publish again.", b.path))),
        }
    }

    /// False when nothing was sent because a blob is already stored
    fn upload(&mut self, hashes: &[String]) -> Result<bool, ServerError> {
        let urls = match self.server.upload_urls(&self.publish_id, hashes) {
            // Someone else stored it meanwhile, the caller's verify picks it up
            Err(e) if e.code == "already_stored" => return Ok(false),
            r => r?,
        };
        for h in hashes {
            if (self.cancel)() {
                return Err(self.stop());
            }
            let blob = self.blobs.get(h).ok_or_else(|| local_error(format!("Unknown file {}", h)))?;
            let url = urls.get(h).ok_or_else(|| ServerError::new("unavailable", "The pack server sent no upload link"))?;
            let (pack_id, done, total, before, all) = (self.pack_id, self.done.min(self.total), self.total, self.sent, self.total_bytes);
            let progress = &mut *self.progress;
            let mut sent = 0u64;
            let mut throttle = Throttle::new();
            let result = self.server.put_blob(url, &blob.src, blob.size, h, &mut |n| {
                sent = (sent + n).min(blob.size);
                if throttle.ready() {
                    let event = progress_event(pack_id, "uploading", done, total, Some(&blob.path), (sent, blob.size));
                    progress(event.overall((before + sent).min(all), all));
                }
            });
            if let Err(e) = result {
                return Err(changed_file(self.pack_dir, h, blob).unwrap_or(e));
            }
            self.put.insert(h.clone());
            self.done += 1;
            self.sent += blob.size;
            let event = progress_event(pack_id, "uploading", self.done.min(total), total, Some(&blob.path), (blob.size, blob.size));
            (self.progress)(event.overall(self.sent.min(all), all));
        }
        Ok(true)
    }
}

// A refused upload is often a file edited after hashing, which the user can fix by publishing again
fn changed_file(pack_dir: &Path, sha256: &str, blob: &Blob) -> Option<ServerError> {
    if hashing::sha256_file(&blob.src).ok().as_deref() == Some(sha256) {
        return None;
    }
    forget_cached_hash(pack_dir, &blob.key);
    Some(local_error(format!("{} changed during the publish. Publish again.", blob.path)))
}

pub fn publish_pack(
    conn: &Conn,
    pack_dir: &Path,
    version: &str,
    now: u64,
    nexus: Option<&dyn Fn(u32) -> Result<Value, String>>,
    progress: &mut dyn FnMut(Progress),
) -> Result<PublishOutcome, String> {
    let manifest = packs::read_manifest(pack_dir).ok_or_else(|| format!("Could not read {}", pack_dir.join("pack.json").display()))?;
    let op = OpGuard::begin_named(pack_dir, OpKind::Publish, &manifest.name)?;
    let mods_dir = pack_dir.parent().ok_or("The pack folder has no parent")?;
    packs::ensure_pack_editable(mods_dir, &manifest.pack_id)?;
    let was_dirty = manifest.published.as_ref().is_some_and(|p| p.dirty);
    if was_dirty {
        set_dirty(pack_dir, false)?;
    }
    let result = run_publish(conn, pack_dir, &manifest, version, now, nexus, &|| op.cancelled(), progress);
    // Only ever sets the flag here, since an edit during the publish may have set it already
    if result.is_err() && was_dirty {
        if let Err(e) = set_dirty(pack_dir, true) {
            eprintln!("[PUBLISH] Could not mark {} as changed: {}", manifest.pack_id, e);
        }
    }
    result
}

// The owner and viewers can see another uploader's pack, which begin would refuse as pack_taken.
// True when me() fails, so begin decides.
fn uploads_pack(conn: &Conn, pack_id: &str) -> bool {
    match conn.call(conn.server().me()) {
        Ok(me) => me.packs.iter().find(|p| p.id == pack_id).is_none_or(|p| p.access == "uploader"),
        Err(_) => true,
    }
}

#[allow(clippy::too_many_arguments)]
fn run_publish(
    conn: &Conn,
    pack_dir: &Path,
    manifest: &PackManifest,
    version: &str,
    now: u64,
    nexus: Option<&dyn Fn(u32) -> Result<Value, String>>,
    cancel: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(Progress),
) -> Result<PublishOutcome, String> {
    let pack_id = manifest.pack_id.as_str();
    let base = conn.base().to_string();
    let local = hash_local_pack(pack_dir, pack_id, progress)?;

    let remote = match conn.call(conn.server().manifest(pack_id)) {
        Ok(m) => Some(m),
        Err(e) if e.code == "pack_gone" => None,
        Err(e) => return Err(e.text(&base)),
    };
    if remote.is_some() && manifest.published.as_ref().and_then(|p| p.server.as_deref()).is_none() && !uploads_pack(conn, pack_id) {
        return Err(ServerError::new("pack_taken", "").text(&base));
    }
    if let Some(old) = &remote {
        if old.format > FORMAT {
            return Err("Update RON Mod Manager to publish this pack".to_string());
        }
        if let (Some(new), Some(prev)) = (parse_semver(version), parse_semver(&old.version)) {
            if new < prev {
                return Err(format!("Version {} is lower than the published version {}", version, old.version));
            }
        }
        if old.version == version {
            if same_mods(&remote_states(old), &local_states(&local)) {
                mark_published(pack_dir, &base, &old.version)?;
                return Ok(PublishOutcome::NothingToPublish { server: base });
            }
            return Err(format!("Version {} is already published. Enter a new version.", version));
        }
    }

    let blobs = blob_list(&local)?;
    // Before begin, so slow Nexus answers do not use up the publish's hour
    let (notes, history) = changelog_for(pack_dir, pack_id, remote.as_ref(), &local, nexus, cancel, progress)?;

    let ready = RemoteManifest {
        format: FORMAT,
        pack_id: pack_id.to_string(),
        name: manifest.name.clone(),
        version: version.to_string(),
        status: "ready".to_string(),
        published_at: now,
        mods: local
            .iter()
            .map(|m| RemoteMod {
                id: m.id.clone(),
                name: m.name.clone(),
                version: m.version.clone(),
                hash: m.hash.clone(),
                addons: m.addons.clone(),
                files: m.files.iter().map(|f| RemoteFileEntry { path: f.path.clone(), size: f.size, sha256: f.sha256.clone() }).collect(),
            })
            .collect(),
    };
    let mut up = Upload {
        server: conn.server(),
        pack_dir,
        pack_id,
        blobs: &blobs,
        cancel,
        progress,
        publish_id: String::new(),
        done: 0,
        total: 0,
        sent: 0,
        total_bytes: 0,
        put: BTreeSet::new(),
    };
    let mut result = up.send(&manifest.name, version, &ready, &notes, &history);
    if matches!(&result, Err(e) if e.code == "publish_gone") {
        result = up.send(&manifest.name, version, &ready, &notes, &history);
    }
    conn.text(result)?;
    let uploaded = up.put.len();

    mark_published(pack_dir, &base, version)?;
    let mut log = packs::read_changelog(pack_dir);
    log.entries.insert(0, VersionEntry { version: version.to_string(), published_at: now, notes });
    log.base = None;
    if let Err(e) = packs::write_changelog(pack_dir, &log) {
        eprintln!("[PUBLISH] Could not save the changelog for {}: {}", pack_id, e);
    }
    Ok(PublishOutcome::Published { server: base, version: version.to_string(), uploaded, reused: blobs.len() - uploaded })
}

pub fn dirty_packs(mods_dir: &Path) -> Vec<PackRef> {
    let Ok(entries) = std::fs::read_dir(mods_dir) else {
        return Vec::new();
    };
    let mut out: Vec<PackRef> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let manifest = packs::read_manifest(&e.path())?;
            if !manifest.published.as_ref().is_some_and(|p| p.dirty && p.server.is_some()) {
                return None;
            }
            Some(PackRef { pack_id: e.file_name().to_string_lossy().to_string(), name: manifest.name, version: manifest.version })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn publishing_packs(mods_dir: &Path) -> Vec<PackRef> {
    running_packs(mods_dir, OpKind::Publish)
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct CloseBlocked {
    pub dirty: Vec<PackRef>,
    pub uploading: Vec<PackRef>,
    pub downloading: Vec<PackRef>,
}

/// None when nothing stops the app from closing
pub fn close_blocked(mods_dir: &Path) -> Option<CloseBlocked> {
    let uploading = publishing_packs(mods_dir);
    let downloading = running_packs(mods_dir, OpKind::Download);
    let dirty: Vec<PackRef> = dirty_packs(mods_dir)
        .into_iter()
        .filter(|d| !uploading.iter().any(|u| u.pack_id == d.pack_id))
        .collect();
    if dirty.is_empty() && uploading.is_empty() && downloading.is_empty() {
        None
    } else {
        Some(CloseBlocked { dirty, uploading, downloading })
    }
}

#[tauri::command]
pub fn list_dirty_packs(app_handle: tauri::AppHandle) -> Result<Vec<PackRef>, String> {
    let mods_dir = crate::vfs::get_mods_dir(&app_handle)?;
    Ok(dirty_packs(&mods_dir))
}

#[tauri::command]
pub fn force_close(window: tauri::Window) -> Result<(), String> {
    window.destroy().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn publish_pack_cmd(
    app_handle: tauri::AppHandle,
    pack_id: String,
    version: String,
    server: Option<String>,
) -> Result<PublishOutcome, String> {
    let version = version.trim().to_string();
    if version.is_empty() {
        return Err("Enter a version".to_string());
    }
    if parse_semver(&version).is_none() {
        return Err(format!("{} is not a valid version. Use MAJOR.MINOR.PATCH, for example 1.4.2", version));
    }
    let mods_dir = crate::vfs::get_mods_dir(&app_handle)?;
    let pack_dir = mods_dir.join(&pack_id);
    if packs::read_manifest(&pack_dir).is_none() || packs::parse_key(&format!("{}/1", pack_id)).is_err() {
        return Err(format!("No modpack folder for {:?}", pack_id));
    }
    let config = crate::config::load_config(app_handle.clone()).unwrap_or_default();
    let domain = config.game_domain.unwrap_or_else(|| "readyornot".to_string());
    tauri::async_runtime::spawn_blocking(move || {
        let published = packs::read_manifest(&pack_dir).and_then(|m| m.published).and_then(|p| p.server);
        let signed_in: Vec<(String, Option<String>)> = accounts::servers(None)
            .into_iter()
            .filter_map(|base| accounts::session(&base).map(|s| (base, s.user.role)))
            .collect();
        let base = pick_server(published.as_deref(), server.as_deref(), &signed_in)?;
        let has_key = crate::config::get_api_key().ok().flatten().is_some();
        let fetch = |id: u32| crate::api::fetch_mod_changelogs_blocking(&domain, id);
        let nexus: Option<&dyn Fn(u32) -> Result<Value, String>> = if has_key { Some(&fetch) } else { None };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let mut emit = |p: Progress| {
            let _ = app_handle.emit("pack-progress", p);
        };
        publish_pack(&Conn::live(&base), &pack_dir, &version, now, nexus, &mut emit)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{save_session_in, MemoryStore, Session};
    use crate::packs::{BaseSnapshot, Changelog, PackSource};
    use crate::server::fake::{FakeServer, BASE};
    use crate::server::User;
    use serde_json::json;
    use std::fs;
    use std::sync::{Arc, Mutex};

    const PACK: &str = "Casual-Shitters";
    const UPLOADER: &str = "up";
    const NOW: u64 = 1792364400;
    const A_PAK: &str = "pakchunk999-Mods_AVP_BetterVestsRemastared_P.pak";
    const B_PAK: &str = "pakchunk9999-Mods_CazanusVisceralBlud_P.pak";
    const W_PAK: &str = "pakchunk9999-Mods_wound_P.pak";
    const TAKEN: &str = "Another uploader on this server already uses this pack id. Rename the pack and try again.";

    type ModSpec<'a> = (&'a str, Vec<(&'a str, String)>);

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ron-test-{}-{}", name, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put_mod(pack_dir: &Path, id: &str, files: &[(&str, String)]) {
        for (path, body) in files {
            let full = pack_dir.join(id).join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, body).unwrap();
        }
    }

    fn pack_fixture(name: &str, mods: &[ModSpec]) -> PathBuf {
        let pack_dir = scratch(name).join(PACK);
        fs::create_dir_all(&pack_dir).unwrap();
        for (id, files) in mods {
            put_mod(&pack_dir, id, files);
        }
        let man = PackManifest {
            pack_id: PACK.into(),
            name: "Casual Shitters".into(),
            version: Some("1.0".into()),
            mod_ids: mods.iter().map(|(id, _)| id.to_string()).collect(),
            imported_at: 1,
            ..Default::default()
        };
        packs::write_manifest(&pack_dir, &man).unwrap();
        pack_dir
    }

    fn meta(name: &str, wound: Option<bool>) -> String {
        match wound {
            Some(on) => format!(
                r#"{{"name":"{}","installed_version":"2.0.2","addons":{{"{}":{{"enabled":{}}}}}}}"#,
                name, W_PAK, on
            ),
            None => format!(r#"{{"name":"{}","version":"1.2"}}"#, name),
        }
    }

    // Five files: 3063 has metadata.json and one pak, 3311 has a pak, an addon pak and metadata.json
    fn base_mods() -> Vec<ModSpec<'static>> {
        vec![
            ("3063", vec![(A_PAK, "vests-v1".to_string()), ("metadata.json", meta("Better Vests", None))]),
            (
                "3311",
                vec![
                    (B_PAK, "blood-v1".to_string()),
                    ("addons/pakchunk9999-Mods_wound_P.pak", "wound".to_string()),
                    ("metadata.json", meta("Visceral Blood", Some(false))),
                ],
            ),
        ]
    }

    fn set_mod_ids(pack_dir: &Path, ids: &[&str]) {
        let mut man = packs::read_manifest(pack_dir).unwrap();
        man.mod_ids = ids.iter().map(|s| s.to_string()).collect();
        packs::write_manifest(pack_dir, &man).unwrap();
    }

    fn no_progress() -> impl FnMut(Progress) {
        |_| {}
    }

    fn local(pack_dir: &Path) -> Vec<LocalMod> {
        hash_local_pack(pack_dir, PACK, &mut no_progress()).unwrap()
    }

    /// A server with an uploader signed in to it
    struct World {
        owner: FakeServer,
        store: Arc<MemoryStore>,
    }

    impl World {
        fn new() -> World {
            let owner = FakeServer::new();
            owner.add_user(UPLOADER, "Uploader", Some("uploader"));
            let store = Arc::new(MemoryStore::default());
            let user = User { id: UPLOADER.into(), name: "Uploader".into(), role: Some("uploader".into()) };
            save_session_in(&*store, BASE, &Session { token: "tok".into(), user }).unwrap();
            World { owner, store }
        }

        fn conn(&self) -> Conn {
            Conn::new(self.owner.as_user(UPLOADER), self.store.clone())
        }

        fn remote(&self) -> RemoteManifest {
            self.owner.manifest(PACK).unwrap()
        }

        /// Newest first
        fn versions(&self) -> Vec<VersionEntry> {
            self.owner.changelog(PACK).unwrap()
        }
    }

    fn publish(w: &World, pack_dir: &Path, version: &str) -> Result<PublishOutcome, String> {
        publish_pack(&w.conn(), pack_dir, version, NOW, None, &mut no_progress())
    }

    fn published(server: &str, version: &str, uploaded: usize, reused: usize) -> PublishOutcome {
        PublishOutcome::Published { server: server.into(), version: version.into(), uploaded, reused }
    }

    fn sha(body: &str) -> String {
        hashing::sha256_bytes(body.as_bytes())
    }

    fn assert_server_holds_pack(w: &World, pack_dir: &Path) {
        for m in &w.remote().mods {
            for f in &m.files {
                let bytes = fs::read(pack_dir.join(&m.id).join(&f.path)).unwrap();
                assert_eq!(w.owner.blob(&f.sha256), Some(bytes.clone()), "{}/{}", m.id, f.path);
                assert_eq!(f.sha256, hashing::sha256_bytes(&bytes), "{}/{}", m.id, f.path);
                assert_eq!(f.size, bytes.len() as u64, "{}/{}", m.id, f.path);
            }
        }
    }

    #[test]
    fn first_publish_creates_the_pack() {
        let pack_dir = pack_fixture("first", &base_mods());
        fs::write(pack_dir.join("3063/thumbnail.jpg"), "jpg").unwrap();
        let w = World::new();
        let mut stages = BTreeSet::new();
        let out = publish_pack(&w.conn(), &pack_dir, "1.0.0", NOW, None, &mut |p: Progress| {
            stages.insert(p.stage);
        })
        .unwrap();
        assert_eq!(out, published(BASE, "1.0.0", 6, 0));
        assert_eq!(w.owner.puts(), 6);
        assert_eq!(w.owner.begins(), 1);
        assert_eq!(w.owner.open_publishes(), 0);
        let want: BTreeSet<String> = ["hashing", "changelog", "checking_server", "uploading"].iter().map(|s| s.to_string()).collect();
        assert_eq!(stages, want);

        let man = w.remote();
        assert_eq!((man.format, man.status.as_str(), man.version.as_str()), (2, "ready", "1.0.0"));
        assert_eq!((man.pack_id.as_str(), man.name.as_str()), (PACK, "Casual Shitters"));
        let blood = &man.mods[1];
        assert_eq!((blood.id.as_str(), blood.name.as_str()), ("3311", "Visceral Blood"));
        assert_eq!(blood.version.as_deref(), Some("2.0.2"));
        assert_eq!(man.mods[0].version.as_deref(), Some("1.2"));
        assert_eq!(blood.addons, BTreeMap::from([(W_PAK.to_string(), false)]));
        let addon = blood.files.iter().find(|f| f.path == format!("addons/{}", W_PAK)).unwrap();
        assert_eq!(addon.sha256, sha("wound"));
        assert_eq!(blood.files.len(), 3);
        assert_server_holds_pack(&w, &pack_dir);

        let local = packs::read_manifest(&pack_dir).unwrap();
        assert_eq!(local.published, Some(Published { server: Some(BASE.into()), dirty: false }));
        assert_eq!(local.version.as_deref(), Some("1.0.0"));
        let versions = w.versions();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, "1.0.0");
        assert!(versions[0].notes.starts_with("### Added\n- Better Vests 1.2\n"), "{}", versions[0].notes);
    }

    #[test]
    fn second_publish_uploads_only_changes() {
        let pack_dir = pack_fixture("second", &base_mods());
        let w = World::new();
        publish(&w, &pack_dir, "1.0.0").unwrap();

        fs::write(pack_dir.join("3063").join(A_PAK), "vests-v2").unwrap();
        fs::remove_dir_all(pack_dir.join("3311")).unwrap();
        put_mod(
            &pack_dir,
            "2675",
            &[
                ("Minervha_Home_CNS.pak", "home".into()),
                ("Minervha_Home_CNS.ucas", "ucas".into()),
                ("metadata.json", meta("Home", None)),
            ],
        );
        set_mod_ids(&pack_dir, &["3063", "2675"]);
        let before = w.owner.puts();
        let mut events = Vec::new();
        let out = publish_pack(&w.conn(), &pack_dir, "1.1.0", NOW, None, &mut |p: Progress| events.push(p)).unwrap();
        // the changed pak and the new mod's three files, 3063's metadata.json is already stored
        assert_eq!(w.owner.puts() - before, 4);
        assert_eq!(out, published(BASE, "1.1.0", 4, 1));
        let last = events.iter().rev().find(|p| p.stage == "uploading").unwrap();
        assert_eq!((last.done, last.total), (4, 4));
        assert_eq!(last.bytes_done, last.bytes_total);
        assert!(last.overall_total > last.bytes_total);
        assert_eq!(last.overall_done, last.overall_total);

        let man = w.remote();
        assert_eq!((man.status.as_str(), man.version.as_str()), ("ready", "1.1.0"));
        assert_eq!(man.mods.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["3063", "2675"]);
        assert_eq!(w.owner.pack_blobs(PACK).len(), 5);
        assert_server_holds_pack(&w, &pack_dir);
    }

    #[test]
    fn interrupted_publish_resumes_with_missing_only() {
        let pack_dir = pack_fixture("resume", &base_mods());
        let w = World::new();
        w.owner.fail_puts_after(2);
        assert!(publish(&w, &pack_dir, "1.0.0").is_err());
        assert_eq!(w.owner.puts(), 2);
        assert!(packs::read_manifest(&pack_dir).unwrap().published.is_none());
        assert!(w.owner.manifest(PACK).is_err());

        let out = publish(&w, &pack_dir, "1.0.0").unwrap();
        assert_eq!(w.owner.puts(), 5);
        assert_eq!(out, published(BASE, "1.0.0", 3, 2));
        assert_server_holds_pack(&w, &pack_dir);
    }

    #[test]
    fn expired_publish_starts_over() {
        let pack_dir = pack_fixture("expired", &base_mods());
        let w = World::new();
        w.owner.expire_next_commit();
        let out = publish(&w, &pack_dir, "1.0.0").unwrap();
        assert_eq!(w.owner.begins(), 2);
        assert_eq!(w.owner.puts(), 5);
        assert_eq!(out, published(BASE, "1.0.0", 5, 0));
        assert_eq!(w.remote().version, "1.0.0");
        assert_eq!(w.versions().len(), 1);
    }

    #[test]
    fn not_uploaded_reverifies_and_commits() {
        let pack_dir = pack_fixture("not-uploaded", &base_mods());
        let w = World::new();
        w.owner.drop_upload_before_commit(&sha("blood-v1"));
        let out = publish(&w, &pack_dir, "1.0.0").unwrap();
        assert_eq!(w.owner.puts(), 6);
        assert_eq!(w.owner.begins(), 1);
        assert_eq!(out, published(BASE, "1.0.0", 5, 0));
        assert_eq!(w.owner.blob(&sha("blood-v1")), Some(b"blood-v1".to_vec()));
        assert_server_holds_pack(&w, &pack_dir);
    }

    #[test]
    fn pack_taken_explains_rename() {
        let pack_dir = pack_fixture("taken", &base_mods());
        let w = World::new();
        w.owner.add_user("other", "Other", Some("uploader"));
        let theirs = RemoteManifest {
            format: FORMAT,
            pack_id: PACK.into(),
            name: "Theirs".into(),
            version: "3.0.0".into(),
            status: "ready".into(),
            published_at: 1,
            mods: Vec::new(),
        };
        w.owner.seed_pack("other", &theirs, "Theirs");
        assert_eq!(publish(&w, &pack_dir, "1.0.0").unwrap_err(), TAKEN);
        assert_eq!(w.owner.puts(), 0);
        assert!(packs::read_manifest(&pack_dir).unwrap().published.is_none());
        assert_eq!(w.owner.manifest(PACK).unwrap().name, "Theirs");
    }

    #[test]
    fn owner_publishing_a_taken_id_says_pack_taken() {
        let pack_dir = pack_fixture("taken-owner", &base_mods());
        let w = World::new();
        w.owner.add_user("other", "Other", Some("uploader"));
        let theirs = RemoteManifest {
            format: FORMAT,
            pack_id: PACK.into(),
            name: "Theirs".into(),
            version: "3.0.0".into(),
            status: "ready".into(),
            published_at: 1,
            mods: Vec::new(),
        };
        w.owner.seed_pack("other", &theirs, "Theirs");
        let owner = Conn::new(w.owner.as_user(crate::server::fake::OWNER), w.store.clone());
        for version in ["1.0.0", "3.0.0", "4.0.0"] {
            let err = publish_pack(&owner, &pack_dir, version, NOW, None, &mut no_progress()).unwrap_err();
            assert_eq!(err, TAKEN, "{}", version);
        }
        assert_eq!(w.owner.begins(), 0);
        assert!(packs::read_manifest(&pack_dir).unwrap().published.is_none());
        assert_eq!(w.owner.manifest(PACK).unwrap().name, "Theirs");
    }

    #[test]
    fn drive_published_state_is_ignored() {
        let pack_dir = pack_fixture("drive-state", &base_mods());
        let mut json: Value = serde_json::from_str(&fs::read_to_string(pack_dir.join("pack.json")).unwrap()).unwrap();
        json["published"] = json!({ "folder_id": "1AbCdriveFolder", "dirty": true });
        fs::write(pack_dir.join("pack.json"), json.to_string()).unwrap();
        let man = packs::read_manifest(&pack_dir).unwrap();
        assert_eq!(man.published, Some(Published { server: None, dirty: true }));

        let signed_in = [(BASE.to_string(), Some("uploader".to_string()))];
        assert_eq!(pick_server(None, None, &signed_in).unwrap(), BASE);

        let w = World::new();
        let out = publish(&w, &pack_dir, "1.0.0").unwrap();
        assert_eq!(out, published(BASE, "1.0.0", 5, 0));
        assert_eq!(packs::read_manifest(&pack_dir).unwrap().published, Some(Published { server: Some(BASE.into()), dirty: false }));
        assert!(!fs::read_to_string(pack_dir.join("pack.json")).unwrap().contains("folder_id"));
    }

    #[test]
    fn lower_version_refused() {
        let pack_dir = pack_fixture("semver-lower", &base_mods());
        let w = World::new();
        publish(&w, &pack_dir, "1.2.0").unwrap();
        fs::write(pack_dir.join("3063").join(A_PAK), "vests-v2").unwrap();
        let begins = w.owner.begins();
        assert_eq!(publish(&w, &pack_dir, "1.1.9").unwrap_err(), "Version 1.1.9 is lower than the published version 1.2.0");
        assert_eq!(w.owner.begins(), begins);
        assert!(matches!(publish(&w, &pack_dir, "1.2.1").unwrap(), PublishOutcome::Published { .. }));
        assert_eq!(publish(&w, &pack_dir, "1.2.1").unwrap(), PublishOutcome::NothingToPublish { server: BASE.into() });
        assert_eq!(w.remote().version, "1.2.1");
        assert_eq!(packs::read_manifest(&pack_dir).unwrap().version.as_deref(), Some("1.2.1"));
    }

    #[test]
    fn same_version_same_files_is_nothing_to_publish() {
        let pack_dir = pack_fixture("nothing", &base_mods());
        let w = World::new();
        publish(&w, &pack_dir, "1.0.0").unwrap();
        let mut man = packs::read_manifest(&pack_dir).unwrap();
        man.published.as_mut().unwrap().dirty = true;
        man.version = Some("0.9.0".into());
        packs::write_manifest(&pack_dir, &man).unwrap();
        let (puts, begins) = (w.owner.puts(), w.owner.begins());

        assert_eq!(publish(&w, &pack_dir, "1.0.0").unwrap(), PublishOutcome::NothingToPublish { server: BASE.into() });
        assert_eq!((w.owner.puts(), w.owner.begins()), (puts, begins));
        let man = packs::read_manifest(&pack_dir).unwrap();
        assert!(!man.published.unwrap().dirty);
        assert_eq!(man.version.as_deref(), Some("1.0.0"));
        assert_eq!(w.versions().len(), 1);

        fs::write(pack_dir.join("3063").join(A_PAK), "vests-v2").unwrap();
        assert_eq!(publish(&w, &pack_dir, "1.0.0").unwrap_err(), "Version 1.0.0 is already published. Enter a new version.");
        assert_eq!((w.owner.puts(), w.owner.begins()), (puts, begins));
    }

    #[test]
    fn newer_manifest_format_refused() {
        let pack_dir = pack_fixture("format", &base_mods());
        let w = World::new();
        publish(&w, &pack_dir, "1.0.0").unwrap();
        let mut newer = w.remote();
        newer.format = FORMAT + 1;
        newer.version = "1.0.1".into();
        w.owner.seed_pack(UPLOADER, &newer, "Newer");
        fs::write(pack_dir.join("3063").join(A_PAK), "vests-v2").unwrap();
        assert_eq!(publish(&w, &pack_dir, "1.1.0").unwrap_err(), "Update RON Mod Manager to publish this pack");
        assert_eq!(w.owner.begins(), 1);
    }

    #[test]
    fn more_than_2000_files_refused() {
        let files: Vec<(String, String)> = (0..2001).map(|i| (format!("f{}.pak", i), format!("body {}", i))).collect();
        let spec: Vec<(&str, String)> = files.iter().map(|(p, b)| (p.as_str(), b.clone())).collect();
        let pack_dir = pack_fixture("many", &[("3063", spec)]);
        let w = World::new();
        assert_eq!(publish(&w, &pack_dir, "1.0.0").unwrap_err(), "This pack has more than 2000 files. The server takes at most 2000.");
        assert_eq!(w.owner.begins(), 0);

        // Copies of one content are one blob
        let same: Vec<(&str, String)> = files.iter().map(|(p, _)| (p.as_str(), "same".to_string())).collect();
        let pack_dir = pack_fixture("many-same", &[("3063", same)]);
        assert_eq!(publish(&w, &pack_dir, "1.0.0").unwrap(), published(BASE, "1.0.0", 1, 0));
    }

    #[test]
    fn file_over_5_gib_refused_by_name() {
        let big = LocalMod {
            id: "3063".into(),
            name: "Better Vests".into(),
            version: None,
            nexus_mod_id: None,
            dir: PathBuf::from("3063"),
            files: vec![FileEntry { path: "huge.pak".into(), size: MAX_BLOB_SIZE + 1, sha256: sha("huge") }],
            addons: BTreeMap::new(),
            hash: "h".into(),
        };
        assert_eq!(
            blob_list(&[big]).err().unwrap(),
            "Better Vests: huge.pak is larger than 5 GiB. The server takes files up to 5 GiB."
        );
    }

    #[test]
    fn cancel_cancels_the_server_publish() {
        let pack_dir = pack_fixture("cancel", &base_mods());
        let w = World::new();
        let key = pack_dir.clone();
        let err = publish_pack(&w.conn(), &pack_dir, "1.0.0", NOW, None, &mut |p: Progress| {
            if p.stage == "uploading" {
                crate::subscribe::request_cancel(&key);
            }
        })
        .unwrap_err();
        assert_eq!(err, CANCELLED);
        assert_eq!(w.owner.puts(), 1);
        assert_eq!(w.owner.open_publishes(), 0);
        assert!(w.owner.manifest(PACK).is_err());
        assert!(packs::read_manifest(&pack_dir).unwrap().published.is_none());
        assert!(!crate::subscribe::request_cancel(&pack_dir));

        publish(&w, &pack_dir, "1.0.0").unwrap();
        assert_eq!(w.owner.puts(), 5);
    }

    #[test]
    fn choose_server_when_several() {
        let s = |base: &str, role: Option<&str>| (base.to_string(), role.map(str::to_string));
        let none = "Sign in to a pack server where you can upload first.";
        assert_eq!(pick_server(None, None, &[]).unwrap_err(), none);
        assert_eq!(pick_server(None, None, &[s("https://a.test", None)]).unwrap_err(), none);
        let one = [s("https://a.test", None), s("https://b.test", Some("uploader"))];
        assert_eq!(pick_server(None, None, &one).unwrap(), "https://b.test");
        let two = [s("https://a.test", Some("owner")), s("https://b.test", Some("uploader"))];
        assert_eq!(pick_server(None, None, &two).unwrap_err(), "choose_server");
        assert_eq!(pick_server(None, Some("https://B.test/"), &two).unwrap(), "https://b.test");
        assert!(pick_server(None, Some("not a server"), &two).is_err());
        assert_eq!(pick_server(Some("https://c.test"), Some("https://b.test"), &two).unwrap(), "https://c.test");
        assert_eq!(pick_server(Some("https://c.test"), None, &[]).unwrap(), "https://c.test");
    }

    fn bump_vests(pack_dir: &Path) {
        fs::write(pack_dir.join("3063/metadata.json"), r#"{"name":"Better Vests","version":"1.3"}"#).unwrap();
        fs::write(pack_dir.join("3063").join(A_PAK), "vests-v2").unwrap();
    }

    #[test]
    fn publish_sends_generated_notes() {
        let pack_dir = pack_fixture("notes", &base_mods());
        let w = World::new();
        publish(&w, &pack_dir, "1.0.0").unwrap();
        bump_vests(&pack_dir);
        let calls = Mutex::new(Vec::new());
        let nexus = |id: u32| {
            calls.lock().unwrap().push(id);
            Ok(json!({ "1.2": ["Old news"], "1.3": ["Fixed the vests<br>Lighter plates"] }))
        };
        publish_pack(&w.conn(), &pack_dir, "1.1.0", NOW, Some(&nexus), &mut no_progress()).unwrap();
        assert_eq!(*calls.lock().unwrap(), [3063]);
        let versions = w.versions();
        assert_eq!(versions[0].version, "1.1.0");
        assert_eq!(versions[0].notes, "### Updated\n- Better Vests 1.2 to 1.3\n  - Fixed the vests\n  - Lighter plates");
    }

    #[test]
    fn nexus_failure_never_blocks_publish() {
        let pack_dir = pack_fixture("nexus-down", &base_mods());
        let w = World::new();
        publish(&w, &pack_dir, "1.0.0").unwrap();
        bump_vests(&pack_dir);
        let nexus = |_: u32| -> Result<Value, String> { Err("Nexus API Error: 503".into()) };
        let out = publish_pack(&w.conn(), &pack_dir, "1.1.0", NOW, Some(&nexus), &mut no_progress()).unwrap();
        assert!(matches!(out, PublishOutcome::Published { .. }));
        assert_eq!(w.versions()[0].notes, "### Updated\n- Better Vests 1.2 to 1.3");
    }

    fn entry(version: &str, notes: &str) -> VersionEntry {
        VersionEntry { version: version.into(), published_at: 5, notes: notes.into() }
    }

    fn fork_changelog(pack_dir: &Path) -> Changelog {
        let mut mods = local_states(&local(pack_dir));
        mods.get_mut("3063").unwrap().version = Some("1.1".into());
        let base = BaseSnapshot { name: "Origin".into(), version: Some("2.0.0".into()), mods };
        Changelog { entries: vec![entry("2.0.0", "Second"), entry("1.0.0", "First")], base: Some(base) }
    }

    #[test]
    fn fork_first_publish_sends_history_and_forked_from() {
        let pack_dir = pack_fixture("fork", &base_mods());
        packs::write_changelog(&pack_dir, &fork_changelog(&pack_dir)).unwrap();
        let w = World::new();
        publish(&w, &pack_dir, "2.1.0").unwrap();
        let versions = w.versions();
        assert_eq!(versions.iter().map(|v| v.version.as_str()).collect::<Vec<_>>(), ["2.1.0", "2.0.0", "1.0.0"]);
        assert_eq!(versions[0].notes, "Forked from Origin 2.0.0.\n\n### Updated\n- Better Vests 1.1 to 1.2");
        assert_eq!((versions[1].notes.as_str(), versions[2].notes.as_str()), ("Second", "First"));

        // Only the first publish sends history
        bump_vests(&pack_dir);
        publish(&w, &pack_dir, "2.2.0").unwrap();
        assert_eq!(w.versions().len(), 4);
        assert_eq!(w.versions()[0].notes, "### Updated\n- Better Vests 1.2 to 1.3");
    }

    #[test]
    fn publish_updates_local_changelog() {
        let pack_dir = pack_fixture("local-log", &base_mods());
        packs::write_changelog(&pack_dir, &fork_changelog(&pack_dir)).unwrap();
        let w = World::new();
        publish(&w, &pack_dir, "2.1.0").unwrap();
        let log = packs::read_changelog(&pack_dir);
        assert!(log.base.is_none());
        assert_eq!(log.entries.iter().map(|v| v.version.as_str()).collect::<Vec<_>>(), ["2.1.0", "2.0.0", "1.0.0"]);
        assert_eq!(log.entries[0].notes, w.versions()[0].notes);
        assert_eq!(log.entries[0].published_at, NOW);

        bump_vests(&pack_dir);
        publish(&w, &pack_dir, "2.2.0").unwrap();
        let log = packs::read_changelog(&pack_dir);
        assert_eq!(log.entries.len(), 4);
        assert_eq!(log.entries[0].notes, "### Updated\n- Better Vests 1.2 to 1.3");
    }

    #[test]
    fn blob_still_bad_after_reupload_fails_by_path() {
        let pack_dir = pack_fixture("still-bad", &base_mods());
        let w = World::new();
        w.owner.lose_puts_of(&sha("blood-v1"), 2);
        let err = publish(&w, &pack_dir, "1.0.0").unwrap_err();
        assert_eq!(err, format!("The pack server did not accept {}. Publish again.", B_PAK));
        assert_eq!(w.owner.puts(), 6);
        assert!(w.owner.manifest(PACK).is_err());
        assert!(packs::read_manifest(&pack_dir).unwrap().published.is_none());
    }

    #[test]
    fn already_stored_goes_to_verify() {
        let pack_dir = pack_fixture("already-stored", &base_mods());
        let w = World::new();
        w.owner.store_before_next_urls(b"blood-v1");
        // The round answered already_stored is not one of the two upload tries
        w.owner.lose_puts_of(&sha("wound"), 1);
        let out = publish(&w, &pack_dir, "1.0.0").unwrap();
        assert_eq!(out, published(BASE, "1.0.0", 4, 1));
        assert_eq!(w.owner.puts(), 5);
        assert_server_holds_pack(&w, &pack_dir);
    }

    fn many_mods(version: &str) -> Vec<(String, Vec<(&'static str, String)>)> {
        (1000..1060)
            .map(|id| {
                let meta = format!(r#"{{"name":"Mod {}","version":"{}"}}"#, id, version);
                (id.to_string(), vec![("a.pak", format!("pak {}", id)), ("metadata.json", meta)])
            })
            .collect()
    }

    fn write_mods(pack_dir: &Path, mods: &[(String, Vec<(&str, String)>)]) {
        for (id, files) in mods {
            put_mod(pack_dir, id, files);
        }
    }

    #[test]
    fn at_most_50_nexus_calls() {
        let mods = many_mods("1.0");
        let spec: Vec<ModSpec> = mods.iter().map(|(id, f)| (id.as_str(), f.clone())).collect();
        let pack_dir = pack_fixture("nexus-cap", &spec);
        let w = World::new();
        publish(&w, &pack_dir, "1.0.0").unwrap();
        write_mods(&pack_dir, &many_mods("1.1"));
        let calls = Mutex::new(0);
        let nexus = |_: u32| {
            *calls.lock().unwrap() += 1;
            Ok(json!({}))
        };
        publish_pack(&w.conn(), &pack_dir, "1.1.0", NOW, Some(&nexus), &mut no_progress()).unwrap();
        assert_eq!(*calls.lock().unwrap(), 50);
        assert_eq!(w.versions()[0].notes.matches(" 1.0 to 1.1").count(), 60);
    }

    #[test]
    fn cancel_during_changelog() {
        let pack_dir = pack_fixture("cancel-notes", &base_mods());
        let w = World::new();
        publish(&w, &pack_dir, "1.0.0").unwrap();
        bump_vests(&pack_dir);
        fs::write(pack_dir.join("3311/metadata.json"), r#"{"name":"Visceral Blood","installed_version":"2.0.3"}"#).unwrap();
        let (begins, puts) = (w.owner.begins(), w.owner.puts());
        let calls = Mutex::new(0);
        let key = pack_dir.clone();
        let nexus = |_: u32| {
            *calls.lock().unwrap() += 1;
            crate::subscribe::request_cancel(&key);
            Err("Nexus API Error: 503".to_string())
        };
        let err = publish_pack(&w.conn(), &pack_dir, "1.1.0", NOW, Some(&nexus), &mut no_progress()).unwrap_err();
        assert_eq!(err, CANCELLED);
        assert_eq!(*calls.lock().unwrap(), 1);
        assert_eq!((w.owner.begins(), w.owner.puts()), (begins, puts));
        assert_eq!(w.owner.open_publishes(), 0);
        assert_eq!(w.remote().version, "1.0.0");
    }

    #[test]
    fn verify_pass_reports_progress_and_checks_cancel() {
        let files: Vec<(String, String)> = (0..120).map(|i| (format!("f{}.pak", i), format!("body {}", i))).collect();
        let spec: Vec<(&str, String)> = files.iter().map(|(p, b)| (p.as_str(), b.clone())).collect();
        let pack_dir = pack_fixture("verify-pass", &[("3063", spec)]);
        let w = World::new();
        let mut checks = Vec::new();
        publish_pack(&w.conn(), &pack_dir, "1.0.0", NOW, None, &mut |p: Progress| {
            if p.stage == "checking_server" {
                checks.push((p.done, p.total));
            }
        })
        .unwrap();
        assert_eq!(checks, [(0, 1), (0, 3), (1, 3), (2, 3), (3, 3)]);

        let pack_dir = pack_fixture("verify-cancel", &[("3063", files.iter().map(|(p, b)| (p.as_str(), b.clone())).collect())]);
        let w = World::new();
        let key = pack_dir.clone();
        let err = publish_pack(&w.conn(), &pack_dir, "1.0.0", NOW, None, &mut |p: Progress| {
            if p.stage == "checking_server" && p.done == 1 {
                crate::subscribe::request_cancel(&key);
            }
        })
        .unwrap_err();
        assert_eq!(err, CANCELLED);
        assert_eq!(w.owner.puts(), 0);
        assert_eq!(w.owner.open_publishes(), 0);
    }

    #[test]
    fn parse_semver_cases() {
        assert_eq!(parse_semver("1.4.2"), Some((1, 4, 2)));
        assert_eq!(parse_semver("0.0.0"), Some((0, 0, 0)));
        assert_eq!(parse_semver("10.20.300"), Some((10, 20, 300)));
        for bad in ["", "1", "1.4", "1.4.2.0", "01.4.2", "1.04.2", "1.4.x", "v1.4.2", "1.4.2-beta", " 1.4.2", "1..2"] {
            assert_eq!(parse_semver(bad), None, "{bad}");
        }
        assert!(parse_semver("1.10.0") > parse_semver("1.9.9"));
    }

    #[test]
    fn edit_during_upload_keeps_dirty() {
        let pack_dir = pack_fixture("dirtyrace", &base_mods());
        let w = World::new();
        publish(&w, &pack_dir, "1.0.0").unwrap();
        fs::write(pack_dir.join("3063").join(A_PAK), "vests-v2").unwrap();
        let mods_dir = pack_dir.parent().unwrap().to_path_buf();
        publish_pack(&w.conn(), &pack_dir, "1.1.0", NOW, None, &mut |p: Progress| {
            if p.stage == "uploading" {
                packs::mark_pack_dirty(&mods_dir, PACK);
            }
        })
        .unwrap();
        let man = packs::read_manifest(&pack_dir).unwrap();
        assert!(man.published.unwrap().dirty);
        assert_eq!(man.version.as_deref(), Some("1.1.0"));
    }

    #[test]
    fn linked_pack_cannot_publish() {
        let pack_dir = pack_fixture("linked", &base_mods());
        let mut man = packs::read_manifest(&pack_dir).unwrap();
        man.source = Some(PackSource { kind: "server".into(), server: Some(BASE.into()), ..Default::default() });
        packs::write_manifest(&pack_dir, &man).unwrap();
        let w = World::new();
        assert_eq!(
            publish(&w, &pack_dir, "1.0.0").unwrap_err(),
            "Casual Shitters is linked to its creator. Fork it to make changes."
        );
        assert_eq!(w.owner.begins(), 0);
    }

    #[test]
    fn publishing_guard_clears_on_drop_and_panic() {
        let mods = scratch("publishing-guard");
        let id = "Guard-Pack";
        let listed = |mods: &Path| publishing_packs(mods).iter().any(|p| p.pack_id == id);
        {
            let _guard = OpGuard::begin(&mods.join(id), OpKind::Publish).unwrap();
            assert!(listed(&mods));
            assert!(OpGuard::begin(&mods.join(id), OpKind::Publish).is_none());
        }
        assert!(!listed(&mods));
        let caught = std::panic::catch_unwind(|| {
            let _guard = OpGuard::begin(&mods.join(id), OpKind::Publish).unwrap();
            panic!("publish died");
        });
        assert!(caught.is_err());
        assert!(!listed(&mods));
        assert!(OpGuard::begin(&mods.join(id), OpKind::Publish).is_some());
    }

    #[test]
    fn close_blocked_tells_uploads_from_downloads() {
        let mods = scratch("close-blocked");
        let mk = |id: &str, name: &str, dirty: bool| {
            let dir = mods.join(id);
            fs::create_dir_all(&dir).unwrap();
            let published = Some(Published { server: Some(BASE.into()), dirty });
            let man = PackManifest { pack_id: id.into(), name: name.into(), published, ..Default::default() };
            packs::write_manifest(&dir, &man).unwrap();
        };
        mk("up", "Up", true);
        mk("down", "Down", false);
        mk("edit", "Edit", true);
        assert_eq!(close_blocked(&mods).unwrap().dirty.len(), 2);
        let _up = OpGuard::begin(&mods.join("up"), OpKind::Publish).unwrap();
        let _down = OpGuard::begin(&mods.join("down"), OpKind::Download).unwrap();
        let _local = OpGuard::begin(&mods.join("local-op"), OpKind::Local).unwrap();
        let blocked = close_blocked(&mods).unwrap();
        let names = |refs: &[PackRef]| refs.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&blocked.uploading), vec!["Up"]);
        assert_eq!(names(&blocked.downloading), vec!["Down"]);
        assert_eq!(names(&blocked.dirty), vec!["Edit"]);
    }

    #[test]
    fn close_blocked_while_only_downloading() {
        let mods = scratch("close-downloading");
        assert!(close_blocked(&mods).is_none());
        let guard = OpGuard::begin(&mods.join("Fresh-Pack"), OpKind::Download).unwrap();
        let blocked = close_blocked(&mods).unwrap();
        assert_eq!(blocked.downloading.len(), 1);
        assert_eq!(blocked.downloading[0].pack_id, "Fresh-Pack");
        assert!(blocked.dirty.is_empty() && blocked.uploading.is_empty());
        drop(guard);
        assert!(close_blocked(&mods).is_none());
    }

    #[test]
    fn publish_error_keeps_dirty_set_during_publish() {
        let pack_dir = pack_fixture("dirty-on-error", &base_mods());
        let w = World::new();
        publish(&w, &pack_dir, "1.0.0").unwrap();
        assert!(!packs::read_manifest(&pack_dir).unwrap().published.unwrap().dirty);
        fs::write(pack_dir.join("3063").join(A_PAK), "vests-v2").unwrap();
        w.owner.fail_puts_after(0);
        let mods_dir = pack_dir.parent().unwrap().to_path_buf();
        let err = publish_pack(&w.conn(), &pack_dir, "1.1.0", NOW, None, &mut |p: Progress| {
            if p.stage == "checking_server" {
                packs::mark_pack_dirty(&mods_dir, PACK);
            }
        });
        assert!(err.is_err());
        assert!(packs::read_manifest(&pack_dir).unwrap().published.unwrap().dirty);
    }

    #[test]
    fn publish_error_leaves_clean_pack_clean() {
        let pack_dir = pack_fixture("clean-on-error", &base_mods());
        let w = World::new();
        publish(&w, &pack_dir, "1.0.0").unwrap();
        fs::write(pack_dir.join("3063").join(A_PAK), "vests-v2").unwrap();
        w.owner.fail_puts_after(0);
        assert!(publish(&w, &pack_dir, "1.1.0").is_err());
        assert!(!packs::read_manifest(&pack_dir).unwrap().published.unwrap().dirty);
    }

    #[test]
    fn signed_out_clears_the_session() {
        let pack_dir = pack_fixture("signed-out", &base_mods());
        let w = World::new();
        w.owner.expire_session();
        assert_eq!(publish(&w, &pack_dir, "1.0.0").unwrap_err(), "Signed out of packs.test. Sign in again in Settings.");
        assert!(crate::accounts::session_in(&*w.store, BASE).is_none());
    }

    #[test]
    fn file_changed_after_hashing_refused() {
        let pack_dir = pack_fixture("changed-mid", &base_mods());
        let w = World::new();
        let vests = pack_dir.join("3063").join(A_PAK);
        let err = publish_pack(&w.conn(), &pack_dir, "1.0.0", NOW, None, &mut |p: Progress| {
            // The last hashing event comes after every file is hashed
            if p.stage == "hashing" && p.file.is_none() {
                fs::write(&vests, "vests-edited").unwrap();
            }
        })
        .unwrap_err();
        assert_eq!(err, format!("{} changed during the publish. Publish again.", A_PAK));
        assert!(w.owner.manifest(PACK).is_err());
        assert!(packs::read_manifest(&pack_dir).unwrap().published.is_none());

        publish(&w, &pack_dir, "1.0.0").unwrap();
        assert_server_holds_pack(&w, &pack_dir);
        assert_eq!(w.owner.blob(&sha("vests-edited")), Some(b"vests-edited".to_vec()));
    }

    #[test]
    fn stale_cached_hash_is_caught_and_dropped() {
        let pack_dir = cache_fixture("cache-stale-upload");
        local(&pack_dir);
        let key = format!("3063/{}", A_PAK);
        set_cached_sha(&pack_dir, &key, "bogus");
        let w = World::new();
        let err = publish(&w, &pack_dir, "1.0.0").unwrap_err();
        assert_eq!(err, format!("{} changed during the publish. Publish again.", A_PAK));
        assert!(!packs::read_manifest(&pack_dir).unwrap().local_hashes.contains_key(&key));
        publish(&w, &pack_dir, "1.0.0").unwrap();
        assert_server_holds_pack(&w, &pack_dir);
    }

    #[test]
    fn names_windows_cannot_store_are_refused() {
        let refused = |name: &str, path: &str| {
            let pack_dir = pack_fixture(name, &base_mods());
            put_mod(&pack_dir, "3063", &[(path, "x".to_string())]);
            let w = World::new();
            let err = publish(&w, &pack_dir, "1.0.0").unwrap_err();
            assert_eq!(err, format!("Better Vests: {} cannot be published because Windows cannot store that name. Rename it first.", path));
            assert_eq!(w.owner.begins(), 0);
        };
        refused("win-colon", "a:b.pak");
        refused("win-dot", "trailing.");
        refused("win-space", "trailing ");
        refused("win-reserved", "CON.pak");
        refused("win-reserved-dir", "addons/aux/x.pak");
        refused("win-backslash", "back\\slash.pak");
    }

    #[test]
    fn case_only_duplicates_are_refused() {
        let pack_dir = pack_fixture("win-case", &base_mods());
        put_mod(&pack_dir, "3063", &[("Foo.pak", "a".to_string()), ("foo.pak", "b".to_string())]);
        let w = World::new();
        let err = publish(&w, &pack_dir, "1.0.0").unwrap_err();
        assert_eq!(err, "Better Vests has two files that differ only in case: Foo.pak and foo.pak");
        assert_eq!(w.owner.begins(), 0);
    }

    #[test]
    fn dirty_packs_lists_only_dirty() {
        let mods = scratch("dirty-packs");
        let mk = |id: &str, name: &str, published: Option<Published>| {
            let dir = mods.join(id);
            fs::create_dir_all(&dir).unwrap();
            let man = PackManifest { pack_id: id.into(), name: name.into(), published, ..Default::default() };
            packs::write_manifest(&dir, &man).unwrap();
        };
        let at = |dirty: bool| Some(Published { server: Some(BASE.into()), dirty });
        mk("zeta", "Zeta", at(true));
        mk("alpha", "Alpha", at(true));
        mk("clean", "Clean", at(false));
        mk("local", "Local", None);
        mk("drive", "Drive", Some(Published { server: None, dirty: true }));
        let names: Vec<_> = dirty_packs(&mods).into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["Alpha", "Zeta"]);
    }

    #[test]
    fn drive_era_dirty_pack_does_not_block_close() {
        let mods = scratch("close-drive");
        let dir = mods.join("drive");
        fs::create_dir_all(&dir).unwrap();
        let published = Some(Published { server: None, dirty: true });
        packs::write_manifest(&dir, &PackManifest { pack_id: "drive".into(), name: "Drive".into(), published, ..Default::default() }).unwrap();
        assert!(close_blocked(&mods).is_none());
    }

    fn sha_of(mods: &[LocalMod], id: &str, path: &str) -> String {
        let m = mods.iter().find(|m| m.id == id).unwrap();
        m.files.iter().find(|f| f.path == path).unwrap().sha256.clone()
    }

    fn set_cached_sha(pack_dir: &Path, key: &str, sha256: &str) {
        let mut man = packs::read_manifest(pack_dir).unwrap();
        man.local_hashes.get_mut(key).unwrap().sha256 = sha256.to_string();
        packs::write_manifest(pack_dir, &man).unwrap();
    }

    fn set_mtime(path: &Path, time: std::time::SystemTime) {
        fs::File::options().write(true).open(path).unwrap().set_modified(time).unwrap();
    }

    // Recent files are never cached, so cache tests age every file first
    fn cache_fixture(name: &str) -> PathBuf {
        let pack_dir = pack_fixture(name, &base_mods());
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        for entry in walkdir::WalkDir::new(&pack_dir).into_iter().map(Result::unwrap) {
            if entry.file_type().is_file() && entry.file_name() != "pack.json" {
                set_mtime(entry.path(), past);
            }
        }
        pack_dir
    }

    #[test]
    fn cache_skips_unchanged_files() {
        let pack_dir = cache_fixture("cache-skip");
        local(&pack_dir);
        let blood_key = format!("3311/{}", B_PAK);
        set_cached_sha(&pack_dir, &blood_key, "bogus");

        let vests = pack_dir.join("3063").join(A_PAK);
        let old_mtime = fs::metadata(&vests).unwrap().modified().unwrap();
        fs::write(&vests, "vests-v2").unwrap();
        set_mtime(&vests, old_mtime + std::time::Duration::from_secs(10));

        let mods = local(&pack_dir);
        assert_eq!(sha_of(&mods, "3311", B_PAK), "bogus");
        assert_eq!(sha_of(&mods, "3063", A_PAK), hashing::sha256_bytes(b"vests-v2"));
        let man = packs::read_manifest(&pack_dir).unwrap();
        assert_eq!(man.local_hashes[&format!("3063/{}", A_PAK)].sha256, hashing::sha256_bytes(b"vests-v2"));
        assert_eq!(man.local_hashes[&blood_key].sha256, "bogus");
    }

    #[test]
    fn cache_rehashes_resized_file() {
        let pack_dir = cache_fixture("cache-resize");
        local(&pack_dir);
        let key = format!("3063/{}", A_PAK);
        set_cached_sha(&pack_dir, &key, "bogus");

        let vests = pack_dir.join("3063").join(A_PAK);
        let old_mtime = fs::metadata(&vests).unwrap().modified().unwrap();
        fs::write(&vests, "vests-v1-longer").unwrap();
        set_mtime(&vests, old_mtime);

        let mods = local(&pack_dir);
        assert_eq!(sha_of(&mods, "3063", A_PAK), hashing::sha256_bytes(b"vests-v1-longer"));
    }

    #[test]
    fn cache_drops_deleted_files() {
        let pack_dir = cache_fixture("cache-drop");
        local(&pack_dir);
        let addon_key = format!("3311/addons/{}", W_PAK);
        let man = packs::read_manifest(&pack_dir).unwrap();
        assert_eq!(man.local_hashes.len(), 5);
        assert!(man.local_hashes.contains_key(&addon_key));

        fs::remove_file(pack_dir.join("3311/addons").join(W_PAK)).unwrap();
        local(&pack_dir);
        let man = packs::read_manifest(&pack_dir).unwrap();
        assert_eq!(man.local_hashes.len(), 4);
        assert!(!man.local_hashes.contains_key(&addon_key));

        set_mod_ids(&pack_dir, &["3063"]);
        local(&pack_dir);
        let keys: Vec<_> = packs::read_manifest(&pack_dir).unwrap().local_hashes.into_keys().collect();
        assert_eq!(keys, vec!["3063/metadata.json".to_string(), format!("3063/{}", A_PAK)]);
    }

    #[test]
    fn cache_save_keeps_dirty() {
        let pack_dir = cache_fixture("cache-dirty");
        let mut man = packs::read_manifest(&pack_dir).unwrap();
        man.published = Some(Published { server: Some(BASE.into()), dirty: false });
        packs::write_manifest(&pack_dir, &man).unwrap();

        let mods_dir = pack_dir.parent().unwrap().to_path_buf();
        hash_local_pack(&pack_dir, PACK, &mut |_| packs::mark_pack_dirty(&mods_dir, PACK)).unwrap();
        let man = packs::read_manifest(&pack_dir).unwrap();
        assert_eq!(man.published, Some(Published { server: Some(BASE.into()), dirty: true }));
        assert_eq!(man.local_hashes.len(), 5);
    }

    #[test]
    fn nexus_id_comes_from_metadata_or_folder() {
        let pack_dir = pack_fixture("nexus-id", &base_mods());
        fs::write(pack_dir.join("3311/metadata.json"), r#"{"name":"Visceral Blood","mod_id":4242}"#).unwrap();
        let mods = local(&pack_dir);
        assert_eq!(mods.iter().map(|m| m.nexus_mod_id).collect::<Vec<_>>(), [Some(3063), Some(4242)]);
    }
}
