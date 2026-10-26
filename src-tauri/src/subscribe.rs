use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::Emitter;

use crate::accounts::Conn;
use crate::hashing;
use crate::packs::{self, InstalledMod, PackManifest, PackRef, PackSource};
use crate::publish::{progress_event, Throttle};
use crate::server::{
    nap, parse_invite, InvitePack, Progress, RemoteFileEntry, RemoteManifest, RemoteMod, ServerError, VersionEntry, CANCELLED, FORMAT,
    SIZE_MISMATCH,
};
use crate::vfs::{self, ImportSummary, ModRef, PackArchive, SwapError};

pub(crate) const DRIVE_GONE: &str = "Google Drive packs are no longer supported. Ask the pack's uploader for a new invite link.";
const SERVER_KIND: &str = "server";

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

// Windows device names, refused with or without an extension
pub(crate) fn is_reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => {
            let digit = stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT"));
            // Superscript one, two and three count as digits here too
            digit.is_some_and(|d| {
                (d.len() == 1 && matches!(d.as_bytes()[0], b'1'..=b'9')) || matches!(d, "\u{b9}" | "\u{b2}" | "\u{b3}")
            })
        }
    }
}

pub fn safe_rel_path(path: &str) -> Result<PathBuf, String> {
    let bad = || format!("Invalid file path in pack: {}", path);
    if path.is_empty() || path.starts_with('/') || path.contains(['\\', ':', '\0']) {
        return Err(bad());
    }
    let mut out = PathBuf::new();
    for part in path.split('/') {
        if part.is_empty() || part.ends_with(['.', ' ']) || is_reserved_name(part) {
            return Err(bad());
        }
        out.push(part);
    }
    if !out.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(bad());
    }
    Ok(out)
}

fn validate_pack_id(pack_id: &str) -> Result<(), String> {
    if packs::parse_key(&format!("{}/1", pack_id)).is_err() || is_reserved_name(pack_id) {
        return Err(format!("Invalid pack id in pack: {}", pack_id));
    }
    Ok(())
}

fn validate(manifest: &RemoteManifest) -> Result<(), String> {
    if manifest.format > FORMAT {
        return Err("Update RON Mod Manager to install this pack".to_string());
    }
    validate_pack_id(&manifest.pack_id)?;
    let mut ids = BTreeSet::new();
    for m in &manifest.mods {
        let key_ok = packs::parse_key(&format!("{}/{}", manifest.pack_id, m.id)).is_ok();
        if !is_digits(&m.id) || !key_ok || !ids.insert(m.id.as_str()) {
            return Err(format!("Invalid mod id in pack: {}", m.id));
        }
        let mut paths = BTreeSet::new();
        for f in &m.files {
            safe_rel_path(&f.path)?;
            if !paths.insert(f.path.to_lowercase()) {
                return Err(format!("Invalid file path in pack: {}", f.path));
            }
            if !is_sha256(&f.sha256) {
                return Err(format!("Invalid file hash in pack: {}", f.path));
            }
        }
    }
    Ok(())
}

/// The pack's current manifest, checked before anything is downloaded
fn fetch_remote(conn: &Conn, pack_id: &str) -> Result<RemoteManifest, String> {
    let manifest = conn.text(conn.server().manifest(pack_id))?;
    validate(&manifest)?;
    if manifest.pack_id != pack_id {
        return Err(format!("The server sent a different pack ({})", manifest.pack_id));
    }
    Ok(manifest)
}

// Stands in for a mod's metadata.json before it is downloaded
fn mod_meta(m: &RemoteMod) -> Value {
    serde_json::json!({ "name": m.name, "version": m.version })
}

fn sorted_mods(manifest: &RemoteManifest) -> Vec<&RemoteMod> {
    let mut mods: Vec<&RemoteMod> = manifest.mods.iter().collect();
    mods.sort_by(|a, b| (a.id.len(), &a.id).cmp(&(b.id.len(), &b.id)));
    mods
}

fn pack_archive(manifest: &RemoteManifest) -> PackArchive {
    PackArchive {
        pack: PackRef { pack_id: manifest.pack_id.clone(), name: manifest.name.clone(), version: Some(manifest.version.clone()) },
        folders: sorted_mods(manifest).into_iter().map(|m| (m.id.clone(), Some(mod_meta(m)))).collect(),
    }
}


// Refuses the creator's own pack, a pack from another server, or a folder that differs only in case
fn check_target(mods_dir: &Path, pack_id: &str, base: &str) -> Result<(), String> {
    if let Some(existing) = packs::read_manifest(&mods_dir.join(pack_id)) {
        if existing.published.as_ref().is_some_and(|p| p.server.is_some()) {
            return Err(format!("{} is your own published pack. Fork or delete it before adding it from a link.", existing.name));
        }
        if existing.source.as_ref().is_some_and(|s| s.kind == SERVER_KIND && s.server.as_deref() != Some(base)) {
            return Err(format!("A pack called {} from another server is already installed. Delete or fork it first.", pack_id));
        }
    }
    let folders = vfs::child_dirs(mods_dir).unwrap_or_default();
    if !folders.iter().any(|n| n == pack_id) {
        if let Some(clash) = folders.iter().find(|n| n.eq_ignore_ascii_case(pack_id)) {
            return Err(format!("A modpack named {} is already installed", clash));
        }
    }
    Ok(())
}

fn sum_sizes<'a>(files: impl IntoIterator<Item = &'a RemoteFileEntry>) -> u64 {
    files.into_iter().fold(0u64, |acc, f| acc.saturating_add(f.size))
}

#[derive(Serialize, Debug)]
pub struct InstalledInfo {
    pub version: Option<String>,
    /// "server", "drive" or "local"
    pub from: String,
}

#[derive(Serialize, Debug)]
pub struct LinkInfo {
    pub base: String,
    pub code: String,
    pub kind: String,
    pub pack: Option<InvitePack>,
    pub signed_in: bool,
    pub installed: Option<InstalledInfo>,
}

fn installed_info(mods_dir: &Path, pack_id: &str) -> Option<InstalledInfo> {
    let local = packs::read_manifest(&mods_dir.join(pack_id))?;
    let from = match local.source.map(|s| s.kind) {
        Some(kind) if kind == SERVER_KIND => "server",
        Some(_) => "drive",
        None => "local",
    };
    Some(InstalledInfo { version: local.version, from: from.to_string() })
}

/// What an invite holds, without accepting it. Signed out, it only names the server.
pub fn inspect_invite(conn: &Conn, code: &str, mods_dir: &Path) -> Result<LinkInfo, String> {
    let mut info = LinkInfo {
        base: conn.base().to_string(),
        code: code.to_string(),
        kind: String::new(),
        pack: None,
        signed_in: false,
        installed: None,
    };
    if !conn.signed_in() {
        return Ok(info);
    }
    let invite = match conn.call(conn.server().invite_info(code)) {
        Ok(invite) => invite,
        Err(e) if e.code == "signed_out" => return Ok(info),
        Err(e) => return Err(e.text(conn.base())),
    };
    if let Some(pack) = &invite.pack {
        validate_pack_id(&pack.id)?;
        check_target(mods_dir, &pack.id, conn.base())?;
        info.installed = installed_info(mods_dir, &pack.id);
    }
    info.signed_in = true;
    info.kind = invite.kind;
    info.pack = invite.pack;
    Ok(info)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpKind {
    Download,
    Publish,
    Local,
}

struct RunningOp {
    kind: OpKind,
    flag: Arc<AtomicBool>,
}

// Every op that writes to a pack, keyed by pack folder. The cancel flag is read by downloads and publishes.
static RUNNING: Mutex<BTreeMap<PathBuf, RunningOp>> = Mutex::new(BTreeMap::new());

fn running() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, RunningOp>> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Marks a pack's op as running with a fresh cancel flag until dropped, so an error or a panic still clears it
pub(crate) struct OpGuard {
    pack_dir: PathBuf,
    flag: Arc<AtomicBool>,
}

impl OpGuard {
    /// None while another op for the pack is running
    pub(crate) fn begin(pack_dir: &Path, kind: OpKind) -> Option<OpGuard> {
        let mut ops = running();
        if ops.contains_key(pack_dir) {
            return None;
        }
        let flag = Arc::new(AtomicBool::new(false));
        ops.insert(pack_dir.to_path_buf(), RunningOp { kind, flag: flag.clone() });
        Some(OpGuard { pack_dir: pack_dir.to_path_buf(), flag })
    }

    pub(crate) fn begin_named(pack_dir: &Path, kind: OpKind, name: &str) -> Result<OpGuard, String> {
        OpGuard::begin(pack_dir, kind).ok_or_else(|| format!("{} is busy, try again when it finishes", name))
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}

impl Drop for OpGuard {
    fn drop(&mut self) {
        running().remove(&self.pack_dir);
    }
}

/// Asks the download or publish running for a pack to stop. False when none is running.
pub(crate) fn request_cancel(pack_dir: &Path) -> bool {
    match running().get(pack_dir) {
        Some(op) if matches!(op.kind, OpKind::Download | OpKind::Publish) => {
            op.flag.store(true, Ordering::Relaxed);
            true
        }
        _ => false,
    }
}

/// The packs in mods_dir with an op of this kind running, by name
pub(crate) fn running_packs(mods_dir: &Path, kind: OpKind) -> Vec<PackRef> {
    let dirs: Vec<PathBuf> = running()
        .iter()
        .filter(|(dir, op)| op.kind == kind && dir.parent() == Some(mods_dir))
        .map(|(dir, _)| dir.clone())
        .collect();
    let mut out: Vec<PackRef> = dirs
        .into_iter()
        .filter_map(|dir| {
            let id = dir.file_name()?.to_string_lossy().to_string();
            Some(match packs::read_manifest(&dir) {
                Some(m) => PackRef { pack_id: id, name: m.name, version: m.version },
                None => PackRef { name: id.clone(), pack_id: id, version: None },
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[tauri::command]
pub fn cancel_pack_op(app_handle: tauri::AppHandle, pack_id: String) -> Result<(), String> {
    request_cancel(&vfs::get_mods_dir(&app_handle)?.join(&pack_id));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn stage_remote(
    conn: &Conn,
    manifest: &RemoteManifest,
    source: PackSource,
    mod_ids: Vec<String>,
    staged: &Path,
    cache: &Path,
    cancel: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(Progress),
) -> Result<(), String> {
    fs::create_dir_all(staged).map_err(|e| format!("Failed to create staging folder: {}", e))?;
    let total = manifest.mods.iter().map(|m| m.files.len()).sum();
    let bytes_total = manifest.mods.iter().flat_map(|m| &m.files).fold(0u64, |sum, f| sum.saturating_add(f.size));
    let mut fetch = Fetch { conn, manifest, cache, done: 0, total, bytes_done: 0, bytes_total, throttle: Throttle::new(), cancel, progress };
    for m in &manifest.mods {
        let mod_dir = staged.join(&m.id);
        fs::create_dir_all(&mod_dir).map_err(|e| format!("Failed to create {}: {}", mod_dir.display(), e))?;
        for f in &m.files {
            fetch.file(f, &mod_dir, staged)?;
        }
    }

    for m in &manifest.mods {
        reset_downloaded_mod(&staged.join(&m.id), m)?;
    }
    write_staged_manifest(manifest, source, mod_ids, staged)
}

const HASH_MISMATCH: &str = "The download did not match its expected hash";

// Downloads that passed their hash check are kept here by hash until the pack installs,
// so a failed attempt carries on from where it stopped
fn cache_dir(mods_dir: &Path, pack_id: &str) -> PathBuf {
    mods_dir.join(vfs::DOWNLOAD_CACHE).join(pack_id)
}

// Deletes a pack's cache once it installs or the player cancels, and the cache folder when it is empty
fn drop_cache(cache: &Path) {
    let _ = fs::remove_dir_all(cache);
    if let Some(parent) = cache.parent() {
        let _ = fs::remove_dir(parent);
    }
}

// True when the cached copy is whole. A damaged copy is deleted so it is downloaded again.
fn reuse_cached(cached: &Path, size: u64, sha: &str) -> bool {
    if !is_plain(cached, false) {
        return false;
    }
    let whole = fs::metadata(cached).is_ok_and(|m| m.len() == size) && hashing::sha256_file(cached).is_ok_and(|h| h == sha);
    if !whole {
        let _ = fs::remove_file(cached);
    }
    whole
}

// Hard links the cached file into staging so it takes no extra space, copying where links fail
fn place_cached(cached: &Path, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
    }
    if fs::hard_link(cached, dest).is_ok() {
        return Ok(());
    }
    fs::copy(cached, dest).map(|_| ()).map_err(|e| format!("Failed to copy {} to {}: {}", cached.display(), dest.display(), e))
}

/// The files an installed pack has, by sha256, metadata files left out. A mod with no install
/// record, as in a pack made on this machine, is hashed with the pack's hash cache.
fn pack_files(pack_dir: &Path) -> BTreeMap<String, PathBuf> {
    let mut have: BTreeMap<String, PathBuf> = BTreeMap::new();
    let Some(local) = packs::read_manifest(pack_dir) else { return have };
    let Some(mods_dir) = pack_dir.parent().and_then(|d| d.canonicalize().ok()) else { return have };
    let ids: BTreeSet<&String> = local.mod_ids.iter().chain(local.installed_hashes.keys()).filter(|id| is_digits(id)).collect();
    for id in ids {
        let mod_dir = pack_dir.join(id);
        let files: Vec<(String, String)> = match local.installed_hashes.get(id) {
            Some(record) => record.files.iter().map(|(path, sha)| (path.clone(), sha.clone())).collect(),
            None => match hashing::hash_mod_files_cached(&mod_dir, id, &local.local_hashes, &mut BTreeMap::new(), &mut |_| {}) {
                Ok(entries) => entries.into_iter().map(|f| (f.path, f.sha256)).collect(),
                Err(_) => continue,
            },
        };
        for (path, sha) in files.into_iter().filter(|(path, _)| !hashing::is_meta_file(path)) {
            let Ok(rel) = safe_rel_path(&path) else { continue };
            let full = mod_dir.join(rel);
            // A symlinked folder in the pack must not pull in files from outside the mods folder
            let inside = full.canonicalize().is_ok_and(|real| real.starts_with(&mods_dir));
            if inside && is_plain(&full, false) {
                have.entry(sha.to_ascii_lowercase()).or_insert(full);
            }
        }
    }
    have
}

/// Links every file the installed pack at `from` already has into the download cache of pack_id
/// by sha256, so staging takes it from there in place of downloading it
pub(crate) fn seed_cache(from: &Path, mods_dir: &Path, pack_id: &str, manifest: &RemoteManifest) {
    let have = pack_files(from);
    let cache = cache_dir(mods_dir, pack_id);
    for f in manifest.mods.iter().flat_map(|m| &m.files) {
        let sha = f.sha256.to_ascii_lowercase();
        let dest = cache.join(&sha);
        if let Some(src) = have.get(&sha) {
            if dest.symlink_metadata().is_err() {
                let _ = place_cached(src, &dest);
            }
        }
    }
}

// Seconds to wait on a rate limit answer that names no time
#[cfg(not(test))]
const RATE_LIMIT_WAIT: u64 = 60;
#[cfg(test)]
const RATE_LIMIT_WAIT: u64 = 0;
const RATE_LIMIT_WAITS: usize = 3;
const MAX_RATE_LIMIT_WAIT: u64 = 300;

// Downloads files into staging with their hashes checked, and reports progress for the pack
struct Fetch<'a> {
    conn: &'a Conn,
    manifest: &'a RemoteManifest,
    cache: &'a Path,
    done: usize,
    total: usize,
    bytes_done: u64,
    bytes_total: u64,
    throttle: Throttle,
    cancel: &'a dyn Fn() -> bool,
    progress: &'a mut dyn FnMut(Progress),
}

impl Fetch<'_> {
    fn file(&mut self, f: &RemoteFileEntry, mod_dir: &Path, staged: &Path) -> Result<(), String> {
        let cancel = self.cancel;
        if cancel() {
            return Err(CANCELLED.to_string());
        }
        let dest = mod_dir.join(safe_rel_path(&f.path)?);
        if !dest.starts_with(staged) {
            return Err(format!("Invalid file path in pack: {}", f.path));
        }
        let manifest = self.manifest;
        let corrupted = || format!("A download from {} was corrupted ({}). Nothing was changed.", manifest.name, f.path);
        let sha = f.sha256.to_ascii_lowercase();
        if !is_sha256(&sha) {
            return Err(format!("Invalid file hash in pack: {}", f.path));
        }
        let cached = self.cache.join(&sha);
        if !reuse_cached(&cached, f.size, &sha) {
            fs::create_dir_all(self.cache).map_err(|e| format!("Failed to create {}: {}", self.cache.display(), e))?;
            let part = self.cache.join(format!("{}.part", sha));
            if let Err(e) = self.download(f, &sha, &part) {
                let _ = fs::remove_file(&part);
                return Err(if e == SIZE_MISMATCH || e == HASH_MISMATCH { corrupted() } else { e });
            }
            fs::rename(&part, &cached).map_err(|e| format!("Failed to move {}: {}", part.display(), e))?;
        }
        place_cached(&cached, &dest)?;
        self.done += 1;
        self.bytes_done = self.bytes_done.saturating_add(f.size);
        let event = progress_event(&self.manifest.pack_id, "downloading", self.done, self.total, Some(&f.path), (f.size, f.size));
        (self.progress)(event.overall(self.bytes_done, self.bytes_total));
        Ok(())
    }

    // Downloads one file to dest and checks its hash, waiting out the server's rate limit
    fn download(&mut self, f: &RemoteFileEntry, sha: &str, dest: &Path) -> Result<(), String> {
        let cancel = self.cancel;
        let conn = self.conn;
        let (pack_id, done, total) = (self.manifest.pack_id.as_str(), self.done, self.total);
        let (before, bytes_total) = (self.bytes_done, self.bytes_total);
        let mut waits = 0;
        let downloaded = loop {
            let mut got = 0u64;
            let (throttle, progress) = (&mut self.throttle, &mut *self.progress);
            let result = conn.call(conn.server().download(sha, dest, f.size, cancel, &mut |n| {
                got += n;
                if throttle.ready() {
                    let event = progress_event(pack_id, "downloading", done, total, Some(&f.path), (got, f.size));
                    progress(event.overall(before + got.min(f.size), bytes_total));
                }
            }));
            match result {
                Err(e) if e.code == "rate_limited" && waits < RATE_LIMIT_WAITS => {
                    waits += 1;
                    let secs = e.retry_after.unwrap_or(RATE_LIMIT_WAIT).min(MAX_RATE_LIMIT_WAIT);
                    for left in (0..=secs).rev() {
                        let mut event = progress_event(pack_id, "waiting", done, total, Some(&f.path), (0, f.size));
                        event.wait_secs = left;
                        (self.progress)(event.overall(before, bytes_total));
                        if left > 0 && !nap(Duration::from_secs(1), cancel) {
                            return Err(CANCELLED.to_string());
                        }
                    }
                }
                other => break other,
            }
        };
        match downloaded {
            Ok(hash) if hash.eq_ignore_ascii_case(sha) => Ok(()),
            Ok(_) => Err(HASH_MISMATCH.to_string()),
            Err(e) => Err(download_error(&e, conn.base())),
        }
    }
}

fn download_error(e: &ServerError, base: &str) -> String {
    match e.code.as_str() {
        "size_mismatch" => SIZE_MISMATCH.to_string(),
        "cancelled" => CANCELLED.to_string(),
        _ => e.text(base),
    }
}

// A freshly downloaded metadata.json starts off and takes its addon states from the manifest
fn reset_downloaded_mod(dir: &Path, m: &RemoteMod) -> Result<(), String> {
    vfs::clear_enabled_flag(dir)?;
    if !m.addons.is_empty() {
        hashing::set_addon_states(dir, &m.addons)?;
    }
    Ok(())
}

fn write_staged_manifest(manifest: &RemoteManifest, source: PackSource, mod_ids: Vec<String>, staged: &Path) -> Result<(), String> {
    let installed_hashes = manifest
        .mods
        .iter()
        .map(|m| {
            let files = m.files.iter().map(|f| (f.path.clone(), f.sha256.to_ascii_lowercase())).collect();
            (m.id.clone(), InstalledMod { hash: m.hash.clone(), files })
        })
        .collect();
    let imported_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    packs::write_manifest(
        staged,
        &PackManifest {
            pack_id: manifest.pack_id.clone(),
            name: manifest.name.clone(),
            version: Some(manifest.version.clone()),
            mod_ids,
            imported_at,
            source: Some(source),
            installed_hashes,
            published: None,
            local_hashes: BTreeMap::new(),
        },
    )
}

// The saved changelog moves with the pack, so its base and a fallback survive the replace
fn carry_changelog(pack_dir: &Path, staged: &Path) -> Result<(), String> {
    let old = pack_dir.join(packs::CHANGELOG_FILE);
    if !old.is_file() {
        return Ok(());
    }
    fs::copy(&old, staged.join(packs::CHANGELOG_FILE))
        .map(|_| ())
        .map_err(|e| format!("Failed to copy {}: {}", old.display(), e))
}

/// Saves the server's changelog over the local entries, keeping `base`. None when the server call failed.
fn refresh_changelog(conn: &Conn, pack_dir: &Path, pack_id: &str) -> Option<Vec<VersionEntry>> {
    let entries = conn.call(conn.server().changelog(pack_id)).ok()?;
    let mut saved = packs::read_changelog(pack_dir);
    saved.entries = entries.clone();
    if let Err(e) = packs::write_changelog(pack_dir, &saved) {
        eprintln!("[SUBSCRIBE] Could not save the changelog of {}: {}", pack_id, e);
    }
    Some(entries)
}

fn uploader_name(conn: &Conn, pack_id: &str) -> Option<String> {
    let me = conn.call(conn.server().me()).ok()?;
    me.packs.into_iter().find(|p| p.id == pack_id).map(|p| p.uploader.name)
}

/// Installs the server's current version of a pack, replacing an installed copy in place
pub fn install_pack(
    conn: &Conn,
    pack_id: &str,
    invite: Option<&str>,
    uploader: Option<String>,
    mods_dir: &Path,
    paks_dir: &Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<ImportSummary, String> {
    install_seeded(conn, pack_id, invite, uploader, None, mods_dir, paks_dir, progress)
}

// install_pack, reusing the files of the pack at seed_from, or else of the copy it replaces
#[allow(clippy::too_many_arguments)]
fn install_seeded(
    conn: &Conn,
    pack_id: &str,
    invite: Option<&str>,
    uploader: Option<String>,
    seed_from: Option<&Path>,
    mods_dir: &Path,
    paks_dir: &Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<ImportSummary, String> {
    let manifest = fetch_remote(conn, pack_id)?;
    check_target(mods_dir, pack_id, conn.base())?;
    let archive = pack_archive(&manifest);
    let plan = vfs::plan_import(&archive, mods_dir)?;
    let target = mods_dir.join(pack_id);
    let op = OpGuard::begin_named(&target, OpKind::Download, &manifest.name)?;

    let previous = plan
        .installed
        .as_deref()
        .and_then(packs::read_manifest)
        .and_then(|m| m.source)
        .filter(|s| s.kind == SERVER_KIND && s.server.as_deref() == Some(conn.base()));
    let source = PackSource {
        kind: SERVER_KIND.to_string(),
        server: Some(conn.base().to_string()),
        invite: invite.map(str::to_string).or_else(|| previous.as_ref().and_then(|s| s.invite.clone())),
        uploader: uploader.or_else(|| previous.and_then(|s| s.uploader)),
        ..Default::default()
    };

    let staging = mods_dir.join(format!(".import-{}", uuid::Uuid::new_v4()));
    let staged = staging.join(pack_id);
    let mod_ids = archive.folders.iter().map(|(id, _)| id.clone()).collect();
    let cache = cache_dir(mods_dir, pack_id);
    let result = (|| {
        if let Some(from) = seed_from.or(plan.installed.as_deref()) {
            seed_cache(from, mods_dir, pack_id, &manifest);
        }
        stage_remote(conn, &manifest, source, mod_ids, &staged, &cache, &|| op.cancelled(), progress)?;
        match &plan.installed {
            Some(old) => carry_changelog(old, &staged),
            None => Ok(()),
        }
    })();
    if let Err(e) = result {
        if e == CANCELLED {
            drop_cache(&cache);
        }
        return Err(vfs::abandon_staging(&staging, e));
    }
    let summary = vfs::finish_import(&archive, plan, mods_dir, paks_dir, &staging)?;
    drop_cache(&cache);
    refresh_changelog(conn, &target, pack_id);
    Ok(summary)
}

/// Accepts an invite link. An uploader invite installs nothing and gives None.
pub fn add_from_invite(
    conn: &Conn,
    link: &str,
    mods_dir: &Path,
    paks_dir: &Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<Option<ImportSummary>, String> {
    let (_, code) = parse_invite(link)?;
    let accepted = conn.text(conn.server().accept_invite(&code))?;
    crate::accounts::remember_server(conn.base());
    if accepted.kind == "uploader" {
        return Ok(None);
    }
    let pack_id = accepted.pack_id.ok_or_else(|| "That invite has no pack".to_string())?;
    validate_pack_id(&pack_id)?;
    let uploader = uploader_name(conn, &pack_id);
    install_pack(conn, &pack_id, Some(link.trim()), uploader, mods_dir, paks_dir, progress).map(Some)
}

const OWN_PUBLISH: &str = "You publish this pack. Fork it to follow another pack.";
const UPLOADER_INVITE: &str = "That is an uploader invite, not a pack link.";

#[derive(Serialize, Debug)]
pub struct SwitchPreview {
    pub base: String,
    pub new_pack_id: String,
    pub name: String,
    pub version: String,
    pub uploader: Option<String>,
    pub reused_files: usize,
    pub total_files: usize,
    pub download_bytes: u64,
}

// The installed pack to switch, refused when the user publishes it
fn switch_source(mods_dir: &Path, pack_id: &str) -> Result<(PathBuf, PackManifest), String> {
    let pack_dir = mods_dir.join(pack_id);
    let local = packs::parse_key(&format!("{}/1", pack_id))
        .ok()
        .and_then(|_| packs::read_manifest(&pack_dir))
        .ok_or_else(|| format!("No modpack folder for {:?}", pack_id))?;
    if local.published.as_ref().is_some_and(|p| p.server.is_some()) {
        return Err(OWN_PUBLISH.to_string());
    }
    Ok((pack_dir, local))
}

fn check_switch_target(mods_dir: &Path, pack_id: &str, new_pack_id: &str, base: &str) -> Result<(), String> {
    validate_pack_id(new_pack_id)?;
    if new_pack_id != pack_id && packs::read_manifest(&mods_dir.join(new_pack_id)).is_some() {
        return Err(format!("{} is already installed. Delete it first or update it instead.", new_pack_id));
    }
    check_target(mods_dir, new_pack_id, base)
}

/// What switching pack_id to the pack behind link would reuse and download. Accepts the invite
/// and writes nothing to disk.
pub fn preview_switch_with(conn: &Conn, pack_id: &str, link: &str, mods_dir: &Path) -> Result<SwitchPreview, String> {
    let (old_dir, _) = switch_source(mods_dir, pack_id)?;
    let (_, code) = parse_invite(link)?;
    // Asked first so an uploader invite is not used up
    let info = conn.text(conn.server().invite_info(&code))?;
    if info.kind == "uploader" {
        return Err(UPLOADER_INVITE.to_string());
    }
    let accepted = conn.text(conn.server().accept_invite(&code))?;
    crate::accounts::remember_server(conn.base());
    let new_pack_id = accepted.pack_id.ok_or_else(|| UPLOADER_INVITE.to_string())?;
    check_switch_target(mods_dir, pack_id, &new_pack_id, conn.base())?;
    let manifest = fetch_remote(conn, &new_pack_id)?;

    let have = pack_files(&old_dir);
    let cache = cache_dir(mods_dir, &new_pack_id);
    let files: Vec<&RemoteFileEntry> = manifest.mods.iter().flat_map(|m| &m.files).collect();
    // Cache entries are counted by size only and the install hashes them again, so the count can be optimistic
    let reused = |f: &RemoteFileEntry| {
        let sha = f.sha256.to_ascii_lowercase();
        have.contains_key(&sha) || cache.join(&sha).symlink_metadata().is_ok_and(|m| m.is_file() && m.len() == f.size)
    };
    let (kept, fetched): (Vec<&RemoteFileEntry>, Vec<&RemoteFileEntry>) = files.iter().partition(|f| reused(f));
    Ok(SwitchPreview {
        base: conn.base().to_string(),
        new_pack_id,
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        uploader: info.pack.map(|p| p.uploader.name),
        reused_files: kept.len(),
        total_files: files.len(),
        download_bytes: sum_sizes(fetched),
    })
}

#[derive(Serialize, Debug)]
pub struct SwitchSummary {
    #[serde(flatten)]
    pub import: ImportSummary,
    // "<key>: <error>" for each mod that was on before the switch but could not be turned on
    pub not_enabled: Vec<String>,
}

/// Installs new_pack_id with the files of pack_id reused, then deletes pack_id. A failed or
/// cancelled install leaves pack_id as it was. Mods keep their on or off state by mod id, and
/// mods new to the pack are on when the pack is the active preset, as in an update.
#[allow(clippy::too_many_arguments)]
pub fn switch_pack_with(
    conn: &Conn,
    pack_id: &str,
    new_pack_id: &str,
    invite: Option<&str>,
    active: bool,
    mods_dir: &Path,
    paks_dir: &Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<SwitchSummary, String> {
    let (old_dir, local) = switch_source(mods_dir, pack_id)?;
    check_switch_target(mods_dir, pack_id, new_pack_id, conn.base())?;
    let moving = new_pack_id != pack_id;
    let _old = if moving { Some(OpGuard::begin_named(&old_dir, OpKind::Local, &local.name)?) } else { None };
    let flags: BTreeMap<String, bool> =
        local.mod_ids.iter().map(|id| (id.clone(), vfs::flag_enabled(&old_dir.join(id)))).collect();
    let uploader = uploader_name(conn, new_pack_id);
    let mut import = install_seeded(conn, new_pack_id, invite, uploader, Some(&old_dir), mods_dir, paks_dir, progress)?;
    let paks = paks_dir.is_dir().then_some(paks_dir);
    if moving {
        if let Err(e) = vfs::remove_pack_dir(mods_dir, paks, pack_id) {
            eprintln!("[SUBSCRIBE] Could not delete {} after switching it: {}", pack_id, e);
            import.failed_deletes.push(old_dir.display().to_string());
        }
    }

    let target = mods_dir.join(new_pack_id);
    let mut not_enabled = Vec::new();
    for key in &import.keys {
        let Some((_, id)) = key.split_once('/') else { continue };
        if !flags.get(id).copied().unwrap_or(active) {
            continue;
        }
        let dir = target.join(id);
        vfs::set_enabled_flag(&dir, true);
        if let Some(paks) = paks {
            if let Err(e) = vfs::enable_key(mods_dir, paks, key) {
                vfs::set_enabled_flag(&dir, false);
                not_enabled.push(format!("{}: {}", key, e));
            }
        }
    }
    if let Some(paks) = paks {
        vfs::sweep_orphans(paks, mods_dir);
    }
    Ok(SwitchSummary { import, not_enabled })
}

/// Moves saved presets and the active one from the old pack to the new, in one config write
pub(crate) fn presets_after_switch(config: &mut crate::config::AppConfig, old_pack_id: &str, summary: &ImportSummary) {
    let new_pack_id = &summary.pack.pack_id;
    crate::config::tidy_presets_after_import(config, new_pack_id, &summary.superseded_ids, &summary.removed_keys);
    if old_pack_id != new_pack_id {
        crate::config::rename_pack_in_presets(config, old_pack_id, new_pack_id, &summary.keys);
    }
    crate::config::move_active_pack(config, old_pack_id, new_pack_id);
}

#[derive(Serialize, Debug)]
pub struct UpdateCheck {
    pub pack_id: String,
    pub status: String,
    pub installed_version: Option<String>,
    pub remote_version: Option<String>,
    pub added: Vec<ModRef>,
    pub changed: Vec<ModRef>,
    pub removed: Vec<ModRef>,
    pub download_bytes: u64,
    pub error: Option<String>,
}

impl UpdateCheck {
    fn new(local: &PackManifest, status: &str) -> UpdateCheck {
        UpdateCheck {
            pack_id: local.pack_id.clone(),
            status: status.to_string(),
            installed_version: local.version.clone(),
            remote_version: None,
            added: Vec::new(),
            changed: Vec::new(),
            removed: Vec::new(),
            download_bytes: 0,
            error: None,
        }
    }

    fn failed(local: &PackManifest, error: String) -> UpdateCheck {
        let status = if error == DRIVE_GONE { "unsupported" } else { "error" };
        UpdateCheck { error: Some(error), ..UpdateCheck::new(local, status) }
    }
}

pub fn check_pack(conn: &Conn, pack_dir: &Path) -> Result<UpdateCheck, String> {
    let local = packs::read_manifest(pack_dir).ok_or_else(|| format!("Could not read {}", pack_dir.join("pack.json").display()))?;
    let remote = fetch_remote(conn, &local.pack_id)?;
    let mut check = UpdateCheck { remote_version: Some(remote.version.clone()), ..UpdateCheck::new(&local, "current") };
    if local.version.as_deref() != Some(remote.version.as_str()) {
        check.status = "available".to_string();
    }

    let key = |id: &str| format!("{}/{}", local.pack_id, id);
    for m in sorted_mods(&remote) {
        let meta = mod_meta(m);
        if !local.mod_ids.contains(&m.id) {
            check.download_bytes = check.download_bytes.saturating_add(sum_sizes(&m.files));
            check.added.push(vfs::mod_ref(key(&m.id), &m.id, Some(&meta)));
            continue;
        }
        let installed = local.installed_hashes.get(&m.id);
        if installed.is_some_and(|i| i.hash == m.hash) {
            continue;
        }
        let have = |path: &str| installed.and_then(|i| i.files.get(path));
        let needed = m
            .files
            .iter()
            .filter(|f| hashing::is_meta_file(&f.path) || !have(&f.path).is_some_and(|sha| sha.eq_ignore_ascii_case(&f.sha256)));
        check.download_bytes = check.download_bytes.saturating_add(sum_sizes(needed));
        check.changed.push(vfs::mod_ref(key(&m.id), &m.id, Some(&meta)));
    }
    for id in &local.mod_ids {
        if !remote.mods.iter().any(|m| &m.id == id) {
            check.removed.push(vfs::mod_ref(key(id), id, vfs::read_meta(&pack_dir.join(id)).as_ref()));
        }
    }
    Ok(check)
}

/// The server a linked pack installs from, or why it has none
fn linked_base(local: &PackManifest) -> Result<String, String> {
    let unlinked = || format!("{} is not linked to a pack server", local.name);
    match &local.source {
        None => Err(unlinked()),
        Some(s) if s.kind != SERVER_KIND => Err(DRIVE_GONE.to_string()),
        Some(s) => s.server.clone().ok_or_else(unlinked),
    }
}

// Keeps the uploader's name on a pack current with what the server reports
fn refresh_uploader(pack_dir: &Path, local: &PackManifest, me: Option<&crate::server::Me>) {
    let Some(name) = me.and_then(|me| me.packs.iter().find(|p| p.id == local.pack_id)).map(|p| p.uploader.name.clone()) else {
        return;
    };
    let mut updated = local.clone();
    let Some(source) = updated.source.as_mut() else { return };
    if source.uploader.as_deref() == Some(name.as_str()) || running().contains_key(pack_dir) {
        return;
    }
    source.uploader = Some(name);
    if let Err(e) = packs::write_manifest(pack_dir, &updated) {
        eprintln!("[SUBSCRIBE] Could not save the uploader of {}: {}", local.pack_id, e);
    }
}

/// One entry for every linked pack, with the reason when it could not be checked
fn check_all(mods_dir: &Path, connect: &dyn Fn(&str) -> Conn) -> Vec<UpdateCheck> {
    let mut servers: BTreeMap<String, (Conn, Option<crate::server::Me>)> = BTreeMap::new();
    let mut out = Vec::new();
    for name in vfs::child_dirs(mods_dir).unwrap_or_default() {
        let pack_dir = mods_dir.join(&name);
        let Some(local) = packs::read_manifest(&pack_dir).filter(|m| m.source.is_some()) else { continue };
        let check = linked_base(&local).and_then(|base| {
            let (conn, me) = servers.entry(base.clone()).or_insert_with(|| {
                let conn = connect(&base);
                let me = conn.call(conn.server().me()).ok();
                (conn, me)
            });
            refresh_uploader(&pack_dir, &local, me.as_ref());
            check_pack(conn, &pack_dir)
        });
        out.push(check.unwrap_or_else(|e| UpdateCheck::failed(&local, e)));
    }
    out
}

#[derive(Serialize, Debug)]
pub struct PackChangelog {
    pub entries: Vec<VersionEntry>,
    /// True when these are the entries saved with the pack, not fresh from its server
    pub saved: bool,
}

fn load_changelog(pack_dir: &Path, connect: &dyn Fn(&str) -> Conn) -> PackChangelog {
    let fresh = packs::read_manifest(pack_dir).and_then(|local| {
        let base = linked_base(&local).ok()?;
        refresh_changelog(&connect(&base), pack_dir, &local.pack_id)
    });
    match fresh {
        Some(entries) => PackChangelog { entries, saved: false },
        None => PackChangelog { entries: packs::read_changelog(pack_dir).entries, saved: true },
    }
}


// Without a game path nothing can be linked, so a Paks folder that does not exist is enough
fn paks_or_placeholder(app_handle: &tauri::AppHandle, mods_dir: &Path) -> PathBuf {
    vfs::get_paks_dir(app_handle).unwrap_or_else(|_| mods_dir.join(".no-game-path"))
}

#[tauri::command]
pub async fn inspect_pack_link(app_handle: tauri::AppHandle, link: String) -> Result<LinkInfo, String> {
    let (base, code) = parse_invite(&link)?;
    let mods_dir = vfs::get_mods_dir(&app_handle)?;
    tauri::async_runtime::spawn_blocking(move || inspect_invite(&Conn::live(&base), &code, &mods_dir))
        .await
        .map_err(|e| e.to_string())?
}

#[derive(Serialize, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AddOutcome {
    Uploader { base: String },
    Installed { pack_id: String, version: String },
}

#[tauri::command]
pub async fn add_pack_from_link(app_handle: tauri::AppHandle, link: String) -> Result<AddOutcome, String> {
    let (base, _) = parse_invite(&link)?;
    let mods_dir = vfs::get_mods_dir(&app_handle)?;
    let paks_dir = paks_or_placeholder(&app_handle, &mods_dir);
    let handle = app_handle.clone();
    let conn_base = base.clone();
    let added = tauri::async_runtime::spawn_blocking(move || {
        let mut emit = |p: Progress| {
            let _ = handle.emit("pack-progress", p);
        };
        add_from_invite(&Conn::live(&conn_base), &link, &mods_dir, &paks_dir, &mut emit)
    })
    .await
    .map_err(|e| e.to_string())??;
    let Some(summary) = added else { return Ok(AddOutcome::Uploader { base }) };

    let mut config = crate::config::load_config(app_handle.clone())?;
    crate::config::tidy_presets_after_import(&mut config, &summary.pack.pack_id, &summary.superseded_ids, &summary.removed_keys);
    crate::config::save_config(app_handle, config)
        .map_err(|e| format!("The pack was added but saved presets could not be updated: {}", e))?;
    Ok(AddOutcome::Installed { pack_id: summary.pack.pack_id, version: summary.pack.version.unwrap_or_default() })
}

#[tauri::command]
pub async fn preview_switch(app_handle: tauri::AppHandle, pack_id: String, link: String) -> Result<SwitchPreview, String> {
    let (base, _) = parse_invite(&link)?;
    let mods_dir = vfs::get_mods_dir(&app_handle)?;
    tauri::async_runtime::spawn_blocking(move || preview_switch_with(&Conn::live(&base), &pack_id, &link, &mods_dir))
        .await
        .map_err(|e| e.to_string())?
}

/// `link` is the invite shown by preview_switch, saved with the pack as a normal install saves it
#[tauri::command]
pub async fn switch_pack(
    app_handle: tauri::AppHandle,
    pack_id: String,
    base: String,
    new_pack_id: String,
    link: Option<String>,
) -> Result<SwitchSummary, String> {
    let base = crate::accounts::normalize_base(&base)?;
    if let Some(link) = &link {
        if parse_invite(link)?.0 != base {
            return Err("That invite link is for another pack server".to_string());
        }
    }
    let mods_dir = vfs::get_mods_dir(&app_handle)?;
    let paks_dir = paks_or_placeholder(&app_handle, &mods_dir);
    let old_id = pack_id.clone();
    let active = crate::config::load_config(app_handle.clone())?.active_preset.as_deref() == Some(format!("pack:{}", pack_id).as_str());
    let handle = app_handle.clone();
    let summary = tauri::async_runtime::spawn_blocking(move || {
        let mut emit = |p: Progress| {
            let _ = handle.emit("pack-progress", p);
        };
        let invite = link.as_deref().map(str::trim);
        switch_pack_with(&Conn::live(&base), &pack_id, &new_pack_id, invite, active, &mods_dir, &paks_dir, &mut emit)
    })
    .await
    .map_err(|e| e.to_string())??;

    let mut config = crate::config::load_config(app_handle.clone())?;
    presets_after_switch(&mut config, &old_id, &summary.import);
    crate::config::save_config(app_handle, config)
        .map_err(|e| format!("The pack was switched but saved presets could not be updated: {}", e))?;
    Ok(summary)
}

#[tauri::command]
pub async fn check_pack_updates(app_handle: tauri::AppHandle) -> Result<Vec<UpdateCheck>, String> {
    let mods_dir = vfs::get_mods_dir(&app_handle)?;
    tauri::async_runtime::spawn_blocking(move || check_all(&mods_dir, &|base: &str| Conn::live(base)))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn pack_changelog(app_handle: tauri::AppHandle, pack_id: String) -> Result<PackChangelog, String> {
    let mods_dir = vfs::get_mods_dir(&app_handle)?;
    packs::parse_key(&format!("{}/1", pack_id)).map_err(|_| format!("No modpack folder for {:?}", pack_id))?;
    let pack_dir = mods_dir.join(&pack_id);
    tauri::async_runtime::spawn_blocking(move || load_changelog(&pack_dir, &|base: &str| Conn::live(base)))
        .await
        .map_err(|e| e.to_string())
}


#[derive(Serialize, Debug)]
pub struct UpdateSummary {
    pub pack: PackRef,
    pub downloaded: usize,
    pub added: usize,
    pub changed: usize,
    pub removed_keys: Vec<String>,
    pub failed_deletes: Vec<String>,
    // "<key>: <error>" for each mod that was updated but could not be turned on
    pub not_enabled: Vec<String>,
}


// The installed pack for an id from the frontend, which must be linked to a pack server
fn linked_pack(mods_dir: &Path, pack_id: &str) -> Result<(PathBuf, PackManifest, String), String> {
    let pack_dir = mods_dir.join(pack_id);
    let local = packs::parse_key(&format!("{}/1", pack_id))
        .ok()
        .and_then(|_| packs::read_manifest(&pack_dir))
        .ok_or_else(|| format!("No modpack folder for {:?}", pack_id))?;
    let base = linked_base(&local)?;
    Ok((pack_dir, local, base))
}

enum Stage<'a> {
    // the installed folder moves across whole
    Keep,
    // these files are downloaded and the rest move across from the installed folder
    Fetch { download: Vec<&'a RemoteFileEntry>, carry: Vec<&'a RemoteFileEntry> },
}

fn is_plain(path: &Path, dir: bool) -> bool {
    path.symlink_metadata().is_ok_and(|m| if dir { m.is_dir() } else { m.is_file() })
}

fn plan_mod<'a>(m: &'a RemoteMod, local: &PackManifest, installed: &Path, forced: bool) -> Stage<'a> {
    let old = local.mod_ids.contains(&m.id);
    let record = local.installed_hashes.get(&m.id);
    if old && !forced && is_plain(installed, true) && record.is_some_and(|r| r.hash == m.hash) {
        return Stage::Keep;
    }
    let reusable = |f: &&RemoteFileEntry| {
        old && !forced
            && !hashing::is_meta_file(&f.path)
            && record.and_then(|r| r.files.get(&f.path)).is_some_and(|sha| sha.eq_ignore_ascii_case(&f.sha256))
            && safe_rel_path(&f.path).is_ok_and(|rel| is_plain(&installed.join(rel), false))
    };
    let (carry, download) = m.files.iter().partition(reusable);
    Stage::Fetch { download, carry }
}

// Renames from the installed pack into staging, kept so they can be undone
#[derive(Default)]
struct Moves(Vec<(PathBuf, PathBuf)>);

impl Moves {
    fn make(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
        }
        fs::rename(from, to).map_err(|e| format!("Failed to move {}: {}", from.display(), e))?;
        self.0.push((from.to_path_buf(), to.to_path_buf()));
        Ok(())
    }

    // Moves everything back in reverse order and removes staging. When a file cannot go back
    // it stops there and keeps staging, since it still holds the rest of the pack.
    fn undo(&self, staging: &Path, err: String) -> Result<String, String> {
        for (from, to) in self.0.iter().rev() {
            if let Err(e) = fs::rename(to, from) {
                return Err(format!(
                    "{}. The previous pack could not be restored ({}) and part of it is kept at {}",
                    err,
                    e,
                    staging.display()
                ));
            }
        }
        Ok(vfs::abandon_staging(staging, err))
    }
}

fn download_changes(
    conn: &Conn,
    manifest: &RemoteManifest,
    plans: &[(&RemoteMod, Stage)],
    staged: &Path,
    cache: &Path,
    cancel: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(Progress),
) -> Result<usize, String> {
    fs::create_dir_all(staged).map_err(|e| format!("Failed to create staging folder: {}", e))?;
    let downloads = || plans.iter().filter_map(|(m, s)| match s {
        Stage::Fetch { download, .. } => Some((*m, download)),
        Stage::Keep => None,
    });
    let total = downloads().map(|(_, d)| d.len()).sum();
    let bytes_total = downloads().flat_map(|(_, d)| d.iter()).fold(0u64, |sum, f| sum.saturating_add(f.size));
    let mut fetch = Fetch { conn, manifest, cache, done: 0, total, bytes_done: 0, bytes_total, throttle: Throttle::new(), cancel, progress };
    for (m, download) in downloads() {
        let mod_dir = staged.join(&m.id);
        fs::create_dir_all(&mod_dir).map_err(|e| format!("Failed to create {}: {}", mod_dir.display(), e))?;
        for f in download {
            fetch.file(f, &mod_dir, staged)?;
        }
    }
    Ok(fetch.done)
}

// Moves the kept files into staging and writes the staged pack.json, changelog, addon states and flags
#[allow(clippy::too_many_arguments)]
fn assemble(
    moves: &mut Moves,
    manifest: &RemoteManifest,
    source: PackSource,
    plans: &[(&RemoteMod, Stage)],
    pack_dir: &Path,
    staged: &Path,
    flags: &BTreeMap<String, bool>,
    active: bool,
) -> Result<(), String> {
    for (m, stage) in plans {
        let (from, to) = (pack_dir.join(&m.id), staged.join(&m.id));
        match stage {
            Stage::Keep => moves.make(&from, &to)?,
            Stage::Fetch { carry, .. } => {
                for f in carry {
                    let rel = safe_rel_path(&f.path)?;
                    moves.make(&from.join(&rel), &to.join(&rel))?;
                }
            }
        }
    }
    write_staged_manifest(manifest, source, plans.iter().map(|(m, _)| m.id.clone()).collect(), staged)?;
    carry_changelog(pack_dir, staged)?;
    for (m, stage) in plans {
        let dir = staged.join(&m.id);
        if matches!(stage, Stage::Fetch { .. }) {
            reset_downloaded_mod(&dir, m)?;
        }
        let on = flags.get(&m.id).copied().unwrap_or(active);
        if vfs::flag_enabled(&dir) != on {
            vfs::set_enabled_flag(&dir, on);
        }
    }
    Ok(())
}

// Restores links of the pack after its files went back into place
fn relink_flagged(mods_dir: &Path, paks_dir: &Path, pack_id: &str, flags: &BTreeMap<String, bool>, err: String) -> String {
    if !paks_dir.is_dir() {
        return err;
    }
    let failed: Vec<String> = flags
        .iter()
        .filter(|(_, on)| **on)
        .map(|(id, _)| format!("{}/{}", pack_id, id))
        .filter(|key| vfs::link_missing(mods_dir, paks_dir, key).is_err())
        .collect();
    if failed.is_empty() {
        err
    } else {
        format!("{}{}{})", err, vfs::RELINK_NOTE, failed.join(", "))
    }
}

pub fn update_pack(
    conn: &Conn,
    mods_dir: &Path,
    paks_dir: &Path,
    pack_id: &str,
    active: bool,
    force: &[String],
    progress: &mut dyn FnMut(Progress),
) -> Result<UpdateSummary, String> {
    let (pack_dir, local, _) = linked_pack(mods_dir, pack_id)?;
    let op = OpGuard::begin_named(&pack_dir, OpKind::Download, &local.name)?;
    let manifest = &fetch_remote(conn, pack_id)?;
    let flags: BTreeMap<String, bool> =
        local.mod_ids.iter().map(|id| (id.clone(), vfs::flag_enabled(&pack_dir.join(id)))).collect();
    let plans: Vec<(&RemoteMod, Stage)> = sorted_mods(manifest)
        .into_iter()
        .map(|m| (m, plan_mod(m, &local, &pack_dir.join(&m.id), force.contains(&m.id))))
        .collect();
    let is_old = |id: &String| local.mod_ids.contains(id);
    let added = plans.iter().filter(|(m, _)| !is_old(&m.id)).count();
    let changed = plans.iter().filter(|(m, s)| is_old(&m.id) && matches!(s, Stage::Fetch { .. })).count();
    let removed_keys = local
        .mod_ids
        .iter()
        .filter(|id| !manifest.mods.iter().any(|m| &m.id == *id))
        .map(|id| format!("{}/{}", pack_id, id))
        .collect();

    // Everything is downloaded before any installed file moves, so a failed or cancelled download
    // leaves the pack untouched. Past this point a cancel is ignored.
    let staging = mods_dir.join(format!(".import-{}", uuid::Uuid::new_v4()));
    let staged = staging.join(pack_id);
    let cache = cache_dir(mods_dir, pack_id);
    let downloaded = match download_changes(conn, manifest, &plans, &staged, &cache, &|| op.cancelled(), progress) {
        Ok(n) => n,
        Err(e) => {
            if e == CANCELLED {
                drop_cache(&cache);
            }
            return Err(vfs::abandon_staging(&staging, e));
        }
    };
    let mut moves = Moves::default();
    if let Err(e) = assemble(&mut moves, manifest, local.source.clone().unwrap_or_default(), &plans, &pack_dir, &staged, &flags, active) {
        return Err(moves.undo(&staging, e).unwrap_or_else(|stranded| stranded));
    }

    let target = mods_dir.join(pack_id);
    match vfs::swap_pack(mods_dir, paks_dir, &staging, &staged, &target, pack_id) {
        Ok(()) => {}
        Err(SwapError::RolledBack(e)) => {
            // relink_flagged below reports what is still unlinked once the files are back
            let base = e.find(vfs::RELINK_NOTE).map_or(e.as_str(), |i| &e[..i]).to_string();
            return Err(match moves.undo(&staging, base) {
                Ok(e) => relink_flagged(mods_dir, paks_dir, pack_id, &flags, e),
                Err(stranded) => stranded,
            });
        }
        Err(SwapError::Stranded(e)) => {
            let aside = staging.join(format!("old-{}", pack_id));
            return Err(format!(
                "{}. The previous pack is split between {} and {}. Its unchanged files were moved to the second folder, and the two folders together form the previous pack.",
                e,
                aside.display(),
                staged.display()
            ));
        }
    }

    let mut not_enabled: Vec<String> = Vec::new();
    if paks_dir.is_dir() {
        for (m, _) in &plans {
            let dir = target.join(&m.id);
            if !vfs::flag_enabled(&dir) {
                continue;
            }
            let key = format!("{}/{}", pack_id, m.id);
            if let Err(e) = vfs::enable_key(mods_dir, paks_dir, &key) {
                vfs::set_enabled_flag(&dir, false);
                not_enabled.push(format!("{}: {}", key, e));
            }
        }
        vfs::sweep_orphans(paks_dir, mods_dir);
    }
    let mut failed_deletes = Vec::new();
    if let Err(e) = fs::remove_dir_all(&staging) {
        if e.kind() != std::io::ErrorKind::NotFound {
            failed_deletes.push(staging.display().to_string());
        }
    }

    drop_cache(&cache);
    refresh_changelog(conn, &target, pack_id);
    Ok(UpdateSummary {
        pack: PackRef { pack_id: pack_id.to_string(), name: manifest.name.clone(), version: Some(manifest.version.clone()) },
        downloaded,
        added,
        changed,
        removed_keys,
        failed_deletes,
        not_enabled,
    })
}

// Non-meta files match the record exactly, none missing and none extra
fn mod_intact(dir: &Path, record: Option<&InstalledMod>) -> bool {
    let Some(record) = record else { return false };
    let Ok(files) = hashing::list_mod_files(dir) else { return false };
    let local: Vec<_> = files.into_iter().filter(|(rel, _, _)| !hashing::is_meta_file(rel)).collect();
    let expected = record.files.keys().filter(|p| !hashing::is_meta_file(p)).count();
    local.len() == expected
        && local.iter().all(|(rel, full, _)| {
            record.files.get(rel).is_some_and(|sha| hashing::sha256_file(full).is_ok_and(|s| s.eq_ignore_ascii_case(sha)))
        })
}

pub fn verify_pack(pack_dir: &Path) -> Result<Vec<String>, String> {
    let local = packs::read_manifest(pack_dir).ok_or_else(|| format!("Could not read {}", pack_dir.join("pack.json").display()))?;
    Ok(local.mod_ids.iter().filter(|id| !mod_intact(&pack_dir.join(id), local.installed_hashes.get(*id))).cloned().collect())
}

async fn run_update(app_handle: tauri::AppHandle, pack_id: String, force: Vec<String>) -> Result<UpdateSummary, String> {
    let mods_dir = vfs::get_mods_dir(&app_handle)?;
    let (_, _, base) = linked_pack(&mods_dir, &pack_id)?;
    let paks_dir = paks_or_placeholder(&app_handle, &mods_dir);
    let config = crate::config::load_config(app_handle.clone())?;
    let active = config.active_preset.as_deref() == Some(format!("pack:{}", pack_id).as_str());
    let handle = app_handle.clone();
    let summary = tauri::async_runtime::spawn_blocking(move || {
        let mut emit = |p: Progress| {
            let _ = handle.emit("pack-progress", p);
        };
        update_pack(&Conn::live(&base), &mods_dir, &paks_dir, &pack_id, active, &force, &mut emit)
    })
    .await
    .map_err(|e| e.to_string())??;

    let mut config = crate::config::load_config(app_handle.clone())?;
    crate::config::strip_keys_from_presets(&mut config, &summary.removed_keys);
    crate::config::save_config(app_handle, config)
        .map_err(|e| format!("The pack was updated but saved presets could not be updated: {}", e))?;
    Ok(summary)
}

#[tauri::command]
pub async fn update_linked_pack(app_handle: tauri::AppHandle, pack_id: String) -> Result<UpdateSummary, String> {
    run_update(app_handle, pack_id, Vec::new()).await
}

#[tauri::command]
pub async fn redownload_mods(app_handle: tauri::AppHandle, pack_id: String, mod_ids: Vec<String>) -> Result<UpdateSummary, String> {
    run_update(app_handle, pack_id, mod_ids).await
}

#[tauri::command]
pub async fn verify_linked_pack(app_handle: tauri::AppHandle, pack_id: String) -> Result<Vec<String>, String> {
    let mods_dir = vfs::get_mods_dir(&app_handle)?;
    let (pack_dir, _, _) = linked_pack(&mods_dir, &pack_id)?;
    tauri::async_runtime::spawn_blocking(move || verify_pack(&pack_dir)).await.map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{save_session_in, session_in, Conn, MemoryStore, Session};
    use crate::hashing;
    use crate::packs;
    use crate::server::fake::{FakeServer, BASE, OWNER};
    use crate::server::{PackServer, User};
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::Arc;

    const PACK: &str = "Casual-Shitters";
    const NAME: &str = "Casual Shitters";
    const A_PAK: &str = "pakchunk999-Mods_AVP_BetterVestsRemastared_P.pak";
    const B_PAK: &str = "pakchunk9999-Mods_CazanusVisceralBlud_P.pak";
    const W_PAK: &str = "pakchunk9999-Mods_wound_P.pak";
    const UPLOADER: &str = "up";
    const VIEWER: &str = "viewer";
    const SIGNED_OUT: &str = "Signed out of packs.test. Sign in again in Settings.";
    const UNAVAILABLE: &str = "The pack server is not answering. Its daily limit may be reached, try again after 00:00 UTC.";

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ron-test-{}-{}", name, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A server with an uploader and a viewer, and a signed in session for it
    struct World {
        owner: FakeServer,
        store: Arc<MemoryStore>,
    }

    impl World {
        fn new() -> World {
            let owner = FakeServer::new();
            owner.add_user(UPLOADER, "Uploader", Some("uploader"));
            owner.add_user(VIEWER, "Viewer", None);
            let store = Arc::new(MemoryStore::default());
            let session = Session { token: "tok".into(), user: User { id: VIEWER.into(), name: "Viewer".into(), role: None } };
            save_session_in(&*store, BASE, &session).unwrap();
            World { owner, store }
        }

        fn conn(&self) -> Conn {
            self.conn_as(VIEWER)
        }

        fn conn_as(&self, user: &str) -> Conn {
            Conn::new(self.owner.as_user(user), self.store.clone())
        }

        fn downloads(&self) -> usize {
            self.owner.downloads()
        }

        fn signed_in(&self) -> bool {
            session_in(&*self.store, BASE).is_some()
        }
    }

    struct FixtureMod {
        id: &'static str,
        files: Vec<(&'static str, &'static str)>,
        addons: Vec<(&'static str, bool)>,
    }

    fn base_mods() -> Vec<FixtureMod> {
        vec![
            FixtureMod {
                id: "3063",
                files: vec![(A_PAK, "vests-v1"), ("metadata.json", r#"{"name":"Better Vests","version":"1.2"}"#)],
                addons: vec![],
            },
            FixtureMod {
                id: "3311",
                files: vec![
                    (B_PAK, "blood-v1"),
                    ("addons/pakchunk9999-Mods_wound_P.pak", "wound"),
                    ("metadata.json", r#"{"name":"Visceral Blood","installed_version":"2.0.2","enabled":true}"#),
                ],
                addons: vec![(W_PAK, true)],
            },
        ]
    }

    fn remote_hash(m: &RemoteMod) -> String {
        hashing::mod_hash(m.files.iter().map(|f| (f.path.as_str(), f.sha256.as_str())), &m.addons)
    }

    /// Publishes the pack as the uploader, with the viewer let in
    fn remote_fixture(w: &World, mods: &[FixtureMod]) -> RemoteManifest {
        remote_fixture_as(w, mods, PACK)
    }

    fn remote_fixture_as(w: &World, mods: &[FixtureMod], pack_id: &str) -> RemoteManifest {
        let manifest = RemoteManifest {
            format: crate::server::FORMAT,
            pack_id: pack_id.into(),
            name: NAME.into(),
            version: "1.0".into(),
            status: "ready".into(),
            published_at: 1792364400,
            mods: mods.iter().map(|m| fixture_mod(w, m)).collect(),
        };
        w.owner.seed_pack(UPLOADER, &manifest, "First");
        w.owner.add_viewer(pack_id, VIEWER);
        w.owner.manifest(pack_id).unwrap()
    }

    /// Stores one mod's files on the server and returns its manifest entry
    fn fixture_mod(w: &World, m: &FixtureMod) -> RemoteMod {
        let files = m
            .files
            .iter()
            .map(|(path, body)| RemoteFileEntry {
                path: path.to_string(),
                size: body.len() as u64,
                sha256: w.owner.store_blob(body.as_bytes()),
            })
            .collect();
        let mut rm = RemoteMod {
            id: m.id.to_string(),
            name: format!("Mod {}", m.id),
            version: m
                .files
                .iter()
                .find(|(p, _)| *p == "metadata.json")
                .and_then(|(_, body)| crate::vfs::meta_version(serde_json::from_str(body).ok().as_ref())),
            hash: String::new(),
            addons: m.addons.iter().map(|(n, on)| (n.to_string(), *on)).collect(),
            files,
        };
        rm.hash = remote_hash(&rm);
        rm
    }

    fn set_remote(w: &World, manifest: &RemoteManifest) {
        w.owner.seed_pack(UPLOADER, manifest, "Next");
    }

    // Leaves out the download cache, which the cache tests check on their own
    fn listing(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = walkdir::WalkDir::new(dir)
            .into_iter()
            .map(|e| e.unwrap().path().strip_prefix(dir).unwrap().display().to_string())
            .filter(|p| !p.starts_with(vfs::DOWNLOAD_CACHE))
            .collect();
        out.sort();
        out
    }

    fn add(w: &World, mods: &Path, paks: &Path) -> Result<ImportSummary, String> {
        add_id(w, PACK, mods, paks)
    }

    fn add_id(w: &World, pack_id: &str, mods: &Path, paks: &Path) -> Result<ImportSummary, String> {
        install_pack(&w.conn(), pack_id, None, None, mods, paks, &mut |_| {})
    }

    #[test]
    fn safe_rel_path_refuses() {
        for bad in ["../x.pak", "/x.pak", "a\\b.pak", "C:x.pak", "a/./b.pak", "", "x.pak.", "x.pak ", "nul", "addons/CON.pak", "lpt1", "conin$", "CONOUT$.pak", "com\u{b9}"] {
            assert_eq!(safe_rel_path(bad).unwrap_err(), format!("Invalid file path in pack: {}", bad), "{bad}");
        }
        assert_eq!(safe_rel_path("addons/x.pak").unwrap(), Path::new("addons").join("x.pak"));
        for ok in ["console.pak", "com10.pak", "lpt0", "nul-mod/x.pak"] {
            assert!(safe_rel_path(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn hostile_manifest_refused_before_download() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let mut hostile = manifest.clone();
        hostile.mods[0].files[0].path = "../../evil.pak".into();
        set_remote(&w, &hostile);
        let mods = scratch("sub-hostile");
        let paks = mods.join("paks");
        assert_eq!(add(&w, &mods, &paks).unwrap_err(), "Invalid file path in pack: ../../evil.pak");
        assert_eq!(w.downloads(), 0);
        assert!(listing(&mods) == vec![String::new()]);

        let mut bad_id = manifest.clone();
        bad_id.mods[0].id = "../3063".into();
        set_remote(&w, &bad_id);
        assert!(add(&w, &mods, &paks).is_err());
        assert_eq!(w.downloads(), 0);
        assert!(listing(&mods) == vec![String::new()]);
    }

    #[test]
    fn inspect_link_reports_sign_in_and_installed() {
        let w = World::new();
        remote_fixture(&w, &base_mods());
        let link = w.owner.as_user(UPLOADER).pack_invite(PACK).unwrap().url;
        let (base, code) = crate::server::parse_invite(&link).unwrap();
        let mods = scratch("sub-inspect");

        let out = Conn::new(w.owner.as_user(VIEWER), Arc::new(MemoryStore::default()));
        let info = inspect_invite(&out, &code, &mods).unwrap();
        assert_eq!((info.base.as_str(), info.code.as_str(), info.signed_in), (BASE, code.as_str(), false));
        assert!(info.pack.is_none() && info.installed.is_none());

        let info = inspect_invite(&w.conn(), &code, &mods).unwrap();
        assert_eq!((info.base.as_str(), info.kind.as_str(), info.signed_in), (base.as_str(), "viewer", true));
        let pack = info.pack.unwrap();
        assert_eq!((pack.id.as_str(), pack.version.as_str(), pack.uploader.name.as_str()), (PACK, "1.0", "Uploader"));
        assert!(info.installed.is_none());
        assert_eq!(w.downloads(), 0);

        add(&w, &mods, &mods.join("paks")).unwrap();
        let installed = inspect_invite(&w.conn(), &code, &mods).unwrap().installed.unwrap();
        assert_eq!((installed.version.as_deref(), installed.from.as_str()), (Some("1.0"), "server"));

        w.owner.expire_session();
        let info = inspect_invite(&w.conn(), &code, &mods).unwrap();
        assert!(!info.signed_in);
        assert!(!w.signed_in());
    }

    #[test]
    fn install_downloads_the_pack() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let mods = scratch("sub-add");
        let paks = scratch("sub-add-paks");
        let mut events: Vec<Progress> = Vec::new();
        let summary = install_pack(&w.conn(), PACK, None, None, &mods, &paks, &mut |p| events.push(p)).unwrap();
        assert_eq!(summary.keys, ["Casual-Shitters/3063", "Casual-Shitters/3311"]);
        assert_eq!(summary.added, 2);
        let last = events.last().unwrap();
        assert_eq!((last.stage.as_str(), last.done, last.total), ("downloading", 5, 5));
        assert_eq!(last.pack_id, PACK);
        let pack_bytes: u64 = manifest.mods.iter().flat_map(|m| &m.files).map(|f| f.size).sum();
        assert_eq!((last.overall_done, last.overall_total), (pack_bytes, pack_bytes));
        let overall: Vec<u64> = events.iter().filter(|p| p.stage == "downloading").map(|p| p.overall_done).collect();
        assert!(overall.windows(2).all(|w| w[0] <= w[1]), "overall progress went backwards: {:?}", overall);

        let pack_dir = mods.join(PACK);
        for m in &manifest.mods {
            for f in &m.files {
                assert!(pack_dir.join(&m.id).join(&f.path).is_file(), "{}/{}", m.id, f.path);
            }
        }
        assert_eq!(fs::read(pack_dir.join("3063").join(A_PAK)).unwrap(), b"vests-v1");
        let man = packs::read_manifest(&pack_dir).unwrap();
        assert_eq!((man.pack_id.as_str(), man.name.as_str(), man.version.as_deref()), (PACK, NAME, Some("1.0")));
        assert_eq!(man.mod_ids, ["3063", "3311"]);
        assert!(man.imported_at > 0);
        assert!(man.published.is_none());
        let source = man.source.unwrap();
        assert_eq!((source.kind.as_str(), source.server.as_deref(), source.folder_id), ("server", Some(BASE), None));
        for m in &manifest.mods {
            let installed = &man.installed_hashes[&m.id];
            assert_eq!(installed.hash, m.hash);
            let files: BTreeMap<String, String> = m.files.iter().map(|f| (f.path.clone(), f.sha256.clone())).collect();
            assert_eq!(installed.files, files);
        }
        assert_eq!(hashing::addon_states(&pack_dir.join("3311")), BTreeMap::from([(W_PAK.to_string(), true)]));
        let meta = crate::vfs::read_meta(&pack_dir.join("3311")).unwrap();
        assert_eq!(meta["enabled"], false);
        assert_eq!(meta["name"], "Visceral Blood");
        assert!(!listing(&mods).iter().any(|p| p.starts_with(".import-")));
        assert_eq!(w.downloads(), 5);
    }

    #[test]
    fn corrupt_download_changes_nothing() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let mods = scratch("sub-corrupt");
        fs::create_dir_all(mods.join("5543")).unwrap();
        fs::write(mods.join("5543/a.pak"), b"a").unwrap();
        let before = listing(&mods);
        w.owner.corrupt_next(&manifest.mods[1].files[0].sha256);
        let err = add(&w, &mods, &mods.join("paks")).unwrap_err();
        assert_eq!(err, format!("A download from {} was corrupted ({}). Nothing was changed.", NAME, B_PAK));
        assert_eq!(listing(&mods), before);
    }

    #[test]
    fn future_format_refused() {
        let w = World::new();
        let mut manifest = remote_fixture(&w, &base_mods());
        manifest.format = crate::server::FORMAT + 1;
        set_remote(&w, &manifest);
        let mods = scratch("sub-refused");
        assert_eq!(add(&w, &mods, &mods.join("paks")).unwrap_err(), "Update RON Mod Manager to install this pack");
        assert_eq!(w.downloads(), 0);
        assert!(listing(&mods) == vec![String::new()]);
    }

    #[test]
    fn check_pack_cases() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let mods = scratch("sub-check");
        add(&w, &mods, &mods.join("paks")).unwrap();
        let pack_dir = mods.join(PACK);
        let pack_json = fs::read(pack_dir.join("pack.json")).unwrap();
        let downloads = w.downloads();

        let current = check_pack(&w.conn(), &pack_dir).unwrap();
        assert_eq!((current.pack_id.as_str(), current.status.as_str()), (PACK, "current"));
        assert_eq!((current.installed_version.as_deref(), current.remote_version.as_deref()), (Some("1.0"), Some("1.0")));
        assert!(current.added.is_empty() && current.changed.is_empty() && current.removed.is_empty());
        assert_eq!((current.download_bytes, current.error.as_deref()), (0, None));

        let mut next = manifest.clone();
        next.version = "1.1".into();
        let blood = &mut next.mods[1];
        let pak = blood.files.iter_mut().find(|f| f.path == B_PAK).unwrap();
        pak.sha256 = w.owner.store_blob(b"blood-v2-longer");
        pak.size = 15;
        blood.hash = remote_hash(blood);
        let meta_size: u64 = blood.files.iter().filter(|f| hashing::is_meta_file(&f.path)).map(|f| f.size).sum();
        set_remote(&w, &next);

        let available = check_pack(&w.conn(), &pack_dir).unwrap();
        assert_eq!(available.status, "available");
        assert_eq!(available.remote_version.as_deref(), Some("1.1"));
        let changed: Vec<&str> = available.changed.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(changed, ["Casual-Shitters/3311"]);
        assert!(available.added.is_empty() && available.removed.is_empty());
        assert_eq!(available.download_bytes, 15 + meta_size);

        assert_eq!(fs::read(pack_dir.join("pack.json")).unwrap(), pack_json);
        assert_eq!(w.downloads(), downloads);
    }

    #[test]
    fn check_pack_sees_added_and_removed_mods() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let mods = scratch("sub-check-mods");
        add(&w, &mods, &mods.join("paks")).unwrap();
        let mut next = manifest.clone();
        next.version = "2.0".into();
        let mut extra = next.mods.remove(0);
        extra.id = "2675".into();
        next.mods.push(extra);
        set_remote(&w, &next);
        let check = check_pack(&w.conn(), &mods.join(PACK)).unwrap();
        assert_eq!(check.added.iter().map(|m| m.key.as_str()).collect::<Vec<_>>(), ["Casual-Shitters/2675"]);
        assert_eq!(check.removed.iter().map(|m| m.key.as_str()).collect::<Vec<_>>(), ["Casual-Shitters/3063"]);
        let size: u64 = next.mods[1].files.iter().map(|f| f.size).sum();
        assert_eq!(check.download_bytes, size);
    }

    fn refused_untouched(w: &World, manifest: &RemoteManifest, err: &str) {
        w.owner.seed_pack(UPLOADER, manifest, "");
        w.owner.add_viewer(&manifest.pack_id, VIEWER);
        let mods = scratch("sub-refused-case");
        assert_eq!(add_id(w, &manifest.pack_id, &mods, &mods.join("paks")).unwrap_err(), err);
        assert!(listing(&mods) == vec![String::new()]);
    }

    #[test]
    fn case_only_duplicate_paths_and_reserved_pack_id_refused() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let mut dup = manifest.clone();
        let mut twin = dup.mods[0].files[0].clone();
        dup.mods[0].files[0].path = "X.pak".into();
        twin.path = "x.pak".into();
        dup.mods[0].files.push(twin);
        refused_untouched(&w, &dup, "Invalid file path in pack: x.pak");
        let mut reserved = manifest.clone();
        reserved.pack_id = "Con".into();
        refused_untouched(&w, &reserved, "Invalid pack id in pack: Con");
        let mut traversal = manifest.clone();
        traversal.pack_id = "../x".into();
        refused_untouched(&w, &traversal, "Invalid pack id in pack: ../x");
        assert_eq!(w.downloads(), 0);
    }

    #[test]
    fn declared_size_is_enforced() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let corrupted = format!("A download from {} was corrupted ({}). Nothing was changed.", NAME, B_PAK);
        for size in [3, 100] {
            let mut lying = manifest.clone();
            lying.mods[1].files[0].size = size;
            set_remote(&w, &lying);
            let mods = scratch("sub-size");
            fs::create_dir_all(mods.join("5543")).unwrap();
            let before = listing(&mods);
            assert_eq!(add(&w, &mods, &mods.join("paks")).unwrap_err(), corrupted, "{size}");
            assert_eq!(listing(&mods), before, "{size}");
        }
    }

    #[test]
    fn own_published_pack_refused() {
        let w = World::new();
        remote_fixture(&w, &base_mods());
        let mods = scratch("sub-own");
        add(&w, &mods, &mods.join("paks")).unwrap();
        let pack_dir = mods.join(PACK);
        let mut man = packs::read_manifest(&pack_dir).unwrap();
        man.source = None;
        man.name = "My Shitters".into();
        man.published = Some(packs::Published { server: Some(BASE.into()), dirty: false });
        packs::write_manifest(&pack_dir, &man).unwrap();
        let before = listing(&mods);
        let downloads = w.downloads();
        let err = "My Shitters is your own published pack. Fork or delete it before adding it from a link.";
        assert_eq!(add(&w, &mods, &mods.join("paks")).unwrap_err(), err);
        assert_eq!(listing(&mods), before);
        assert_eq!(w.downloads(), downloads);
    }

    #[test]
    fn case_only_pack_folder_clash_refused() {
        let w = World::new();
        remote_fixture(&w, &base_mods());
        let mods = scratch("sub-clash");
        let other = mods.join("casual-shitters");
        fs::create_dir_all(&other).unwrap();
        let man = packs::PackManifest { pack_id: "casual-shitters".into(), name: "Other".into(), ..Default::default() };
        packs::write_manifest(&other, &man).unwrap();
        let before = listing(&mods);
        let err = "A modpack named casual-shitters is already installed";
        assert_eq!(add(&w, &mods, &mods.join("paks")).unwrap_err(), err);
        assert_eq!(listing(&mods), before);
        assert_eq!(w.downloads(), 0);
    }

    #[test]
    fn pack_from_another_server_is_refused() {
        let w = World::new();
        remote_fixture(&w, &base_mods());
        let mods = scratch("sub-other-server");
        add(&w, &mods, &mods.join("paks")).unwrap();
        let pack_dir = mods.join(PACK);
        let mut man = packs::read_manifest(&pack_dir).unwrap();
        man.source.as_mut().unwrap().server = Some("https://other.test".into());
        packs::write_manifest(&pack_dir, &man).unwrap();
        let before = listing(&mods);
        let downloads = w.downloads();
        let err = "A pack called Casual-Shitters from another server is already installed. Delete or fork it first.";
        assert_eq!(add(&w, &mods, &mods.join("paks")).unwrap_err(), err);
        assert_eq!(listing(&mods), before);
        assert_eq!(w.downloads(), downloads);
    }

    /// Version 1.1 changes the blood pak without publishing it. Returns the manifest and the new sha.
    fn bumped(w: &World, manifest: &RemoteManifest) -> (RemoteManifest, String) {
        let mut next = manifest.clone();
        next.version = "1.1".into();
        let blood = &mut next.mods[1];
        let pak = blood.files.iter_mut().find(|f| f.path == B_PAK).unwrap();
        pak.sha256 = w.owner.store_blob(b"blood-v2");
        let sha = pak.sha256.clone();
        blood.hash = remote_hash(blood);
        (next, sha)
    }

    fn bump(w: &World, manifest: &RemoteManifest) -> (RemoteManifest, String) {
        let (next, sha) = bumped(w, manifest);
        set_remote(w, &next);
        (next, sha)
    }

    #[test]
    fn add_over_installed_linked_pack_replaces_it() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let (mods, paks) = (scratch("sub-upgrade"), scratch("sub-upgrade-paks"));
        add(&w, &mods, &paks).unwrap();
        let (next, _) = bump(&w, &manifest);
        let downloads = w.downloads();
        let summary = add(&w, &mods, &paks).unwrap();
        assert_eq!(summary.keys, ["Casual-Shitters/3063", "Casual-Shitters/3311"]);
        let pack_dir = mods.join(PACK);
        assert_eq!(fs::read(pack_dir.join("3311").join(B_PAK)).unwrap(), b"blood-v2");
        let man = packs::read_manifest(&pack_dir).unwrap();
        assert_eq!(man.version.as_deref(), Some("1.1"));
        assert_eq!(man.installed_hashes["3311"].hash, next.mods[1].hash);
        assert!(man.source.is_some());
        let top: Vec<String> = fs::read_dir(&mods).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(top, [PACK]);
        // metadata files are never reused, so only they and the blood pak download again
        assert_eq!(w.downloads() - downloads, 3);
    }

    #[test]
    fn corrupt_upgrade_keeps_the_old_pack() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let (mods, paks) = (scratch("sub-upgrade-corrupt"), scratch("sub-upgrade-corrupt-paks"));
        add(&w, &mods, &paks).unwrap();
        let before = listing(&mods);
        let pack_json = fs::read(mods.join(PACK).join("pack.json")).unwrap();
        let (_, changed) = bump(&w, &manifest);
        w.owner.corrupt_next(&changed);
        let err = add(&w, &mods, &paks).unwrap_err();
        assert_eq!(err, format!("A download from {} was corrupted ({}). Nothing was changed.", NAME, B_PAK));
        assert_eq!(listing(&mods), before);
        assert_eq!(fs::read(mods.join(PACK).join("pack.json")).unwrap(), pack_json);
        assert_eq!(fs::read(mods.join(PACK).join("3311").join(B_PAK)).unwrap(), b"blood-v1");
    }

    #[test]
    fn replacing_a_drive_pack_reuses_matching_files() {
        let w = World::new();
        let mods = scratch("sub-drive-reuse");
        let paks = scratch("sub-drive-reuse-paks");
        let pack_dir = mods.join(PACK);
        let mut installed_hashes = BTreeMap::new();
        for (id, files) in [("3063", vec![("a.pak", "vests-v1")]), ("3311", vec![("b.pak", "blood-v1"), ("c.pak", "extra")])] {
            let mut record = packs::InstalledMod::default();
            for (path, body) in files {
                fs::create_dir_all(pack_dir.join(id)).unwrap();
                fs::write(pack_dir.join(id).join(path), body).unwrap();
                record.files.insert(path.to_string(), hashing::sha256_bytes(body.as_bytes()));
            }
            installed_hashes.insert(id.to_string(), record);
        }
        let drive = packs::PackManifest {
            pack_id: PACK.into(),
            name: NAME.into(),
            version: Some("0.9".into()),
            mod_ids: vec!["3063".into(), "3311".into()],
            source: Some(packs::PackSource { kind: "drive".into(), folder_id: Some("F".into()), ..Default::default() }),
            installed_hashes,
            ..Default::default()
        };
        packs::write_manifest(&pack_dir, &drive).unwrap();

        let shared = vec![
            FixtureMod { id: "3063", files: vec![("renamed.pak", "vests-v1")], addons: vec![] },
            FixtureMod { id: "3311", files: vec![("b.pak", "blood-v1")], addons: vec![] },
            FixtureMod { id: "3500", files: vec![("n.pak", "new-v1")], addons: vec![] },
        ];
        remote_fixture(&w, &shared);
        add(&w, &mods, &paks).unwrap();
        assert_eq!(w.downloads(), 1);
        assert_eq!(fs::read(pack_dir.join("3063/renamed.pak")).unwrap(), b"vests-v1");
        assert_eq!(fs::read(pack_dir.join("3311/b.pak")).unwrap(), b"blood-v1");
        assert!(!pack_dir.join("3311/c.pak").exists());
        assert_eq!(fs::read(pack_dir.join("3500/n.pak")).unwrap(), b"new-v1");
        let man = packs::read_manifest(&pack_dir).unwrap();
        assert_eq!(man.source.unwrap().kind, "server");
        assert!(!mods.join(vfs::DOWNLOAD_CACHE).exists());
    }

    const N_PAK: &str = "pakchunk999-Mods_NewMod_P.pak";

    fn new_mod() -> FixtureMod {
        FixtureMod { id: "3500", files: vec![(N_PAK, "new-v1"), ("metadata.json", r#"{"name":"New Mod","version":"0.1"}"#)], addons: vec![] }
    }

    fn update(w: &World, mods: &Path, paks: &Path, active: bool, force: &[&str]) -> Result<UpdateSummary, String> {
        update_id(w, mods, paks, PACK, active, force)
    }

    fn update_id(w: &World, mods: &Path, paks: &Path, pack_id: &str, active: bool, force: &[&str]) -> Result<UpdateSummary, String> {
        let force: Vec<String> = force.iter().map(|s| s.to_string()).collect();
        update_pack(&w.conn(), mods, paks, pack_id, active, &force, &mut |_| {})
    }

    // Every file under the pack folder with its sha256
    fn snapshot(pack_dir: &Path) -> BTreeMap<String, String> {
        hashing::list_mod_files(pack_dir)
            .unwrap()
            .into_iter()
            .map(|(rel, full, _)| (rel, hashing::sha256_file(&full).unwrap()))
            .collect()
    }

    // Paks entries starting with a prefix, with the content each one reads as
    fn links(paks: &Path, prefix: &str) -> BTreeMap<String, Vec<u8>> {
        fs::read_dir(paks)
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
            .map(|e| (e.file_name().to_string_lossy().into_owned(), fs::read(e.path()).unwrap()))
            .collect()
    }

    fn no_staging(mods: &Path) -> bool {
        !fs::read_dir(mods).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".import-"))
    }

    #[cfg(unix)]
    fn inode(path: &Path) -> u64 {
        std::os::unix::fs::MetadataExt::ino(&fs::metadata(path).unwrap())
    }

    #[test]
    fn update_downloads_only_changes() {
        let w = World::new();
        let mut mods_spec = base_mods();
        mods_spec[1].files.push(("thumbnail.jpg", "thumb"));
        let manifest = remote_fixture(&w, &mods_spec);
        let (mods, paks) = (scratch("sub-update"), scratch("sub-update-paks"));
        add(&w, &mods, &paks).unwrap();
        let pack_dir = mods.join(PACK);
        let kept = [pack_dir.join("3063").join(A_PAK), pack_dir.join("3311/addons").join(W_PAK)];
        #[cfg(unix)]
        let inodes: Vec<u64> = kept.iter().map(|p| inode(p)).collect();
        let meta_3063 = fs::read(pack_dir.join("3063/metadata.json")).unwrap();

        let (mut next, _) = bumped(&w, &manifest);
        next.mods.push(fixture_mod(&w, &new_mod()));
        set_remote(&w, &next);
        let before = w.downloads();
        let summary = update(&w, &mods, &paks, false, &[]).unwrap();

        // the blood pak, both meta files of 3311 and the two files of the new mod
        assert_eq!(w.downloads() - before, 5);
        assert_eq!(summary.downloaded, 5);
        assert_eq!((summary.added, summary.changed), (1, 1));
        assert!(summary.removed_keys.is_empty() && summary.failed_deletes.is_empty());
        assert_eq!((summary.pack.pack_id.as_str(), summary.pack.version.as_deref()), (PACK, Some("1.1")));
        #[cfg(unix)]
        assert_eq!(kept.iter().map(|p| inode(p)).collect::<Vec<_>>(), inodes);
        assert_eq!(fs::read(pack_dir.join("3063/metadata.json")).unwrap(), meta_3063);
        assert_eq!(fs::read(pack_dir.join("3311").join(B_PAK)).unwrap(), b"blood-v2");
        assert_eq!(fs::read(pack_dir.join("3500").join(N_PAK)).unwrap(), b"new-v1");

        let man = packs::read_manifest(&pack_dir).unwrap();
        assert_eq!(man.version.as_deref(), Some("1.1"));
        assert_eq!(man.mod_ids, ["3063", "3311", "3500"]);
        assert_eq!(man.source.unwrap().server.as_deref(), Some(BASE));
        assert!(man.published.is_none());
        for m in &next.mods {
            assert_eq!(man.installed_hashes[&m.id].hash, m.hash);
        }
        assert_eq!(hashing::addon_states(&pack_dir.join("3311")), BTreeMap::from([(W_PAK.to_string(), true)]));
        assert!(verify_pack(&pack_dir).unwrap().is_empty());
        assert!(no_staging(&mods));
    }

    // Pack-A is a local pack, active and linked. The linked pack has 3063 on and 3311 off.
    fn two_packs(name: &str) -> (World, PathBuf, PathBuf) {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let (mods, paks) = (scratch(name), scratch(&format!("{}-paks", name)));
        let a = mods.join("Pack-A");
        fs::create_dir_all(a.join("1001")).unwrap();
        fs::write(a.join("1001/a.pak"), b"a").unwrap();
        packs::write_manifest(&a, &packs::PackManifest { pack_id: "Pack-A".into(), name: "A".into(), mod_ids: vec!["1001".into()], ..Default::default() }).unwrap();
        crate::vfs::enable_key(&mods, &paks, "Pack-A/1001").unwrap();
        add(&w, &mods, &paks).unwrap();
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3063", PACK)).unwrap();
        let mut next = manifest.clone();
        next.version = "1.1".into();
        next.mods.push(fixture_mod(&w, &new_mod()));
        set_remote(&w, &next);
        (w, mods, paks)
    }

    fn check_setup(mods: &Path, paks: &Path, a_links: &BTreeMap<String, Vec<u8>>, new_on: bool) {
        let pack_dir = mods.join(PACK);
        assert_eq!(&links(paks, "Pack-A.1001_"), a_links);
        assert!(crate::vfs::flag_enabled(&mods.join("Pack-A/1001")));
        assert_eq!(links(paks, &format!("{}.3063_", PACK)), BTreeMap::from([(format!("{}.3063_{}", PACK, A_PAK), b"vests-v1".to_vec())]));
        assert!(crate::vfs::flag_enabled(&pack_dir.join("3063")));
        assert!(links(paks, &format!("{}.3311_", PACK)).is_empty());
        assert!(!crate::vfs::flag_enabled(&pack_dir.join("3311")));
        let added = links(paks, &format!("{}.3500_", PACK));
        if new_on {
            assert_eq!(added, BTreeMap::from([(format!("{}.3500_{}", PACK, N_PAK), b"new-v1".to_vec())]));
        } else {
            assert!(added.is_empty());
        }
        assert_eq!(crate::vfs::flag_enabled(&pack_dir.join("3500")), new_on);
        assert!(no_staging(mods));
    }

    #[test]
    fn update_inactive_pack_keeps_setup() {
        let (w, mods, paks) = two_packs("sub-inactive");
        let a_links = links(&paks, "Pack-A.1001_");
        assert_eq!(a_links.len(), 1);
        let summary = update(&w, &mods, &paks, false, &[]).unwrap();
        assert_eq!((summary.added, summary.changed, summary.downloaded), (1, 0, 2));
        check_setup(&mods, &paks, &a_links, false);
    }

    #[test]
    fn update_active_pack_enables_added() {
        let (w, mods, paks) = two_packs("sub-active");
        let a_links = links(&paks, "Pack-A.1001_");
        update(&w, &mods, &paks, true, &[]).unwrap();
        check_setup(&mods, &paks, &a_links, true);
    }

    #[test]
    fn update_relinks_changed_pak_in_enabled_mod() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let (mods, paks) = (scratch("sub-relink"), scratch("sub-relink-paks"));
        add(&w, &mods, &paks).unwrap();
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3311", PACK)).unwrap();
        let prefix = format!("{}.3311_", PACK);
        let before = links(&paks, &prefix);
        assert_eq!(before.len(), 2);
        bump(&w, &manifest);
        update(&w, &mods, &paks, true, &[]).unwrap();

        let after = links(&paks, &prefix);
        assert_eq!(after.keys().collect::<Vec<_>>(), before.keys().collect::<Vec<_>>());
        assert_eq!(after[&format!("{}{}", prefix, B_PAK)], b"blood-v2");
        assert_eq!(after[&format!("{}addon_{}", prefix, W_PAK)], b"wound");
        // no entry in Paks is left pointing at a file that is gone
        for e in fs::read_dir(&paks).unwrap() {
            assert!(fs::metadata(e.unwrap().path()).is_ok());
        }
        #[cfg(unix)]
        assert_eq!(fs::read_link(paks.join(format!("{}{}", prefix, B_PAK))).unwrap(), mods.join(PACK).join("3311").join(B_PAK));
        assert!(crate::vfs::flag_enabled(&mods.join(PACK).join("3311")));
    }

    #[test]
    fn failed_download_restores_pack() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let (mods, paks) = (scratch("sub-update-corrupt"), scratch("sub-update-corrupt-paks"));
        add(&w, &mods, &paks).unwrap();
        let pack_dir = mods.join(PACK);
        let before = snapshot(&pack_dir);
        let (mut next, changed) = bumped(&w, &manifest);
        next.mods.push(fixture_mod(&w, &new_mod()));
        set_remote(&w, &next);
        w.owner.corrupt_next(&changed);
        let err = update(&w, &mods, &paks, false, &[]).unwrap_err();
        assert_eq!(err, format!("A download from {} was corrupted ({}). Nothing was changed.", NAME, B_PAK));
        assert_eq!(snapshot(&pack_dir), before);
        assert!(no_staging(&mods));
    }

    // Makes a folder read only. False when that does not stop writes, as when running as root.
    #[cfg(unix)]
    fn lock_dir(dir: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
        let probe = dir.join(".probe");
        if fs::write(&probe, b"").is_ok() {
            let _ = fs::remove_file(&probe);
            unlock_dir(dir);
            return false;
        }
        true
    }

    #[cfg(unix)]
    fn unlock_dir(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn failed_assemble_moves_everything_back() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let (mods, paks) = (scratch("sub-update-assemble"), scratch("sub-update-assemble-paks"));
        add(&w, &mods, &paks).unwrap();
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3063", PACK)).unwrap();
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3311", PACK)).unwrap();
        let pack_dir = mods.join(PACK);
        let before = snapshot(&pack_dir);
        let linked = links(&paks, &format!("{}.", PACK));
        bump(&w, &manifest);
        // 3063 moves across whole first, then the carried addon of 3311 cannot leave its folder
        let addons = pack_dir.join("3311/addons");
        if !lock_dir(&addons) {
            return;
        }
        let result = update(&w, &mods, &paks, true, &[]);
        unlock_dir(&addons);
        let err = result.unwrap_err();
        assert!(err.starts_with(&format!("Failed to move {}", addons.join(W_PAK).display())), "{err}");
        assert_eq!(snapshot(&pack_dir), before);
        assert_eq!(links(&paks, &format!("{}.", PACK)), linked);
        assert!(no_staging(&mods));
    }

    #[test]
    fn link_failure_after_swap_still_returns_the_summary() {
        let (w, mods, paks) = two_packs("sub-not-enabled");
        // a folder where the new mod's links go cannot be cleared by enable_key
        let blocker = paks.join(format!("{}.3500_blocker", PACK));
        fs::create_dir_all(&blocker).unwrap();
        let summary = update(&w, &mods, &paks, true, &[]).unwrap();
        assert_eq!(summary.added, 1);
        assert_eq!(summary.not_enabled.len(), 1);
        assert!(summary.not_enabled[0].starts_with(&format!("{}/3500: ", PACK)), "{:?}", summary.not_enabled);
        assert!(!crate::vfs::flag_enabled(&mods.join(PACK).join("3500")));
        assert!(mods.join(PACK).join("3500").join(N_PAK).is_file());
        assert!(crate::vfs::flag_enabled(&mods.join(PACK).join("3063")));
        assert!(no_staging(&mods));
    }

    #[cfg(unix)]
    #[test]
    fn failed_swap_moves_everything_back() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let (mods, paks) = (scratch("sub-update-swap"), scratch("sub-update-swap-paks"));
        add(&w, &mods, &paks).unwrap();
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3311", PACK)).unwrap();
        let pack_dir = mods.join(PACK);
        let before = snapshot(&pack_dir);
        let linked = links(&paks, &format!("{}.", PACK));
        bump(&w, &manifest);
        // the old links cannot be removed, so the swap rolls back
        if !lock_dir(&paks) {
            return;
        }
        let result = update(&w, &mods, &paks, false, &[]);
        unlock_dir(&paks);
        assert_eq!(result.unwrap_err(), "Close Ready or Not and try again");
        assert_eq!(snapshot(&pack_dir), before);
        assert_eq!(links(&paks, &format!("{}.", PACK)), linked);
        assert!(no_staging(&mods));
    }

    #[test]
    fn verify_finds_tampered_mod_and_redownload_restores_it() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let (mods, paks) = (scratch("sub-verify"), scratch("sub-verify-paks"));
        add(&w, &mods, &paks).unwrap();
        let pack_dir = mods.join(PACK);
        assert!(verify_pack(&pack_dir).unwrap().is_empty());
        fs::write(pack_dir.join("3311").join(B_PAK), b"tampered").unwrap();
        assert_eq!(verify_pack(&pack_dir).unwrap(), ["3311"]);

        let before = w.downloads();
        let summary = update(&w, &mods, &paks, false, &["3311"]).unwrap();
        assert_eq!(w.downloads() - before, manifest.mods[1].files.len());
        assert_eq!((summary.changed, summary.added), (1, 0));
        assert!(verify_pack(&pack_dir).unwrap().is_empty());
        assert_eq!(fs::read(pack_dir.join("3311").join(B_PAK)).unwrap(), b"blood-v1");
    }

    #[test]
    fn verify_reports_extra_missing_and_unrecorded() {
        let w = World::new();
        remote_fixture(&w, &base_mods());
        let mods = scratch("sub-verify-cases");
        add(&w, &mods, &mods.join("paks")).unwrap();
        let pack_dir = mods.join(PACK);
        // a meta file edit is not tampering
        crate::vfs::set_enabled_flag(&pack_dir.join("3063"), true);
        assert!(verify_pack(&pack_dir).unwrap().is_empty());
        fs::write(pack_dir.join("3063/extra.pak"), b"x").unwrap();
        fs::remove_file(pack_dir.join("3311/addons").join(W_PAK)).unwrap();
        assert_eq!(verify_pack(&pack_dir).unwrap(), ["3063", "3311"]);

        let w2 = World::new();
        remote_fixture(&w2, &base_mods());
        let mods2 = scratch("sub-verify-norecord");
        add(&w2, &mods2, &mods2.join("paks")).unwrap();
        let mut man = packs::read_manifest(&mods2.join(PACK)).unwrap();
        man.installed_hashes.remove("3063");
        packs::write_manifest(&mods2.join(PACK), &man).unwrap();
        assert_eq!(verify_pack(&mods2.join(PACK)).unwrap(), ["3063"]);
    }

    #[test]
    fn update_drops_removed_mods_and_unlisted_files() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let (mods, paks) = (scratch("sub-update-drop"), scratch("sub-update-drop-paks"));
        add(&w, &mods, &paks).unwrap();
        let pack_dir = mods.join(PACK);
        fs::write(pack_dir.join("3063/extra.pak"), b"x").unwrap();
        fs::write(pack_dir.join("3311/stray.pak"), b"y").unwrap();
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3063", PACK)).unwrap();
        let gone = format!("{}.3063_", PACK);
        assert_eq!(links(&paks, &gone).len(), 2);
        let (mut next, _) = bumped(&w, &manifest);
        next.mods.remove(0);
        set_remote(&w, &next);
        let summary = update(&w, &mods, &paks, true, &[]).unwrap();
        assert_eq!(summary.removed_keys, [format!("{}/3063", PACK)]);
        assert!(fs::read_dir(&paks).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(&gone)));
        assert!(!pack_dir.join("3063").exists());
        assert!(!pack_dir.join("3311/stray.pak").exists());
        assert_eq!(packs::read_manifest(&pack_dir).unwrap().mod_ids, ["3311"]);
        assert!(no_staging(&mods));
    }

    #[test]
    fn update_refuses_unknown_and_unlinked_packs() {
        let w = World::new();
        remote_fixture(&w, &base_mods());
        let mods = scratch("sub-update-refuse");
        let paks = mods.join("paks");
        let noop = |id: &str| update_id(&w, &mods, &paks, id, false, &[]).unwrap_err();
        assert_eq!(noop("../x"), "No modpack folder for \"../x\"");
        assert_eq!(noop(PACK), format!("No modpack folder for {:?}", PACK));
        add(&w, &mods, &paks).unwrap();
        let mut man = packs::read_manifest(&mods.join(PACK)).unwrap();
        man.source = None;
        packs::write_manifest(&mods.join(PACK), &man).unwrap();
        let downloads = w.downloads();
        assert_eq!(noop(PACK), "Casual Shitters is not linked to a pack server");
        assert_eq!(w.downloads(), downloads);
    }

    #[test]
    fn rate_limit_waits_then_continues() {
        let w = World::new();
        remote_fixture(&w, &base_mods());
        let mods = scratch("sub-rate");
        w.owner.rate_limit_downloads(RATE_LIMIT_WAITS);
        let mut events: Vec<Progress> = Vec::new();
        install_pack(&w.conn(), PACK, None, None, &mods, &mods.join("paks"), &mut |p| events.push(p)).unwrap();
        assert_eq!(w.downloads(), 5);
        assert_eq!(events.iter().filter(|p| p.stage == "waiting").count(), RATE_LIMIT_WAITS);
        assert!(mods.join(PACK).join("3063").join(A_PAK).is_file());
    }

    #[test]
    fn rate_limit_gives_up_after_three_waits() {
        let w = World::new();
        remote_fixture(&w, &base_mods());
        let mods = scratch("sub-rate-long");
        w.owner.rate_limit_downloads(RATE_LIMIT_WAITS + 1);
        let err = add(&w, &mods, &mods.join("paks")).unwrap_err();
        assert_eq!(err, "The pack server asked to slow down. Try again in a minute.");
        assert!(no_staging(&mods));
        assert!(!mods.join(PACK).exists());
    }

    #[test]
    fn signed_out_mid_install_clears_token() {
        let w = World::new();
        remote_fixture(&w, &base_mods());
        let mods = scratch("sub-signed-out");
        let server = w.owner.as_user(OWNER);
        w.owner.cancel_after(2, move || server.expire_session());
        let err = add(&w, &mods, &mods.join("paks")).unwrap_err();
        assert_eq!(err, SIGNED_OUT);
        assert!(!w.signed_in());
        assert_eq!(w.downloads(), 2);
        assert_eq!(fs::read_dir(cache_dir(&mods, PACK)).unwrap().count(), 2);
        assert!(no_staging(&mods));
        assert!(!mods.join(PACK).exists());
    }

    #[test]
    fn blob_gone_keeps_cache() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let mods = scratch("sub-blob-gone");
        let (next, _) = bumped(&w, &manifest);
        // the third file is the old blood pak, gone once 1.1 is committed
        w.owner.replace_version_after(1, next);
        let err = add(&w, &mods, &mods.join("paks")).unwrap_err();
        assert_eq!(err, "The pack changed while downloading. Try again.");
        assert_eq!(w.downloads(), 2);
        assert_eq!(fs::read_dir(cache_dir(&mods, PACK)).unwrap().count(), 2);
        assert!(no_staging(&mods));

        let summary = add(&w, &mods, &mods.join("paks")).unwrap();
        assert_eq!(summary.pack.version.as_deref(), Some("1.1"));
        assert_eq!(w.downloads(), 5);
    }

    #[test]
    fn failed_add_resumes_from_the_cache() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let mods = scratch("sub-resume");
        let last = manifest.mods.last().unwrap().files.last().unwrap().sha256.clone();
        w.owner.corrupt_next(&last);
        add(&w, &mods, &mods.join("paks")).unwrap_err();
        assert_eq!(w.downloads(), 5);
        assert_eq!(fs::read_dir(cache_dir(&mods, PACK)).unwrap().count(), 4);

        add(&w, &mods, &mods.join("paks")).unwrap();
        assert_eq!(w.downloads(), 6);
        assert_eq!(fs::read(mods.join(PACK).join("3063").join(A_PAK)).unwrap(), b"vests-v1");
        assert!(!mods.join(vfs::DOWNLOAD_CACHE).exists());
    }

    #[test]
    fn damaged_cache_entry_is_downloaded_again() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let mods = scratch("sub-bad-cache");
        let f = manifest.mods.iter().flat_map(|m| &m.files).find(|f| f.path == A_PAK).unwrap();
        let cache = cache_dir(&mods, PACK);
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join(f.sha256.to_ascii_lowercase()), b"vests-XX").unwrap();
        add(&w, &mods, &mods.join("paks")).unwrap();
        assert_eq!(w.downloads(), 5);
        assert_eq!(fs::read(mods.join(PACK).join("3063").join(A_PAK)).unwrap(), b"vests-v1");
    }

    #[test]
    fn cancel_add_from_link_changes_nothing() {
        const ID: &str = "Cancel-Add";
        let w = World::new();
        remote_fixture_as(&w, &base_mods(), ID);
        let mods = scratch("sub-cancel-add");
        fs::create_dir_all(mods.join("5543")).unwrap();
        fs::write(mods.join("5543/a.pak"), b"a").unwrap();
        let before = listing(&mods);
        let key = mods.join(ID);
        w.owner.cancel_after(1, move || {
            request_cancel(&key);
        });
        let err = add_id(&w, ID, &mods, &mods.join("paks")).unwrap_err();
        assert_eq!(err, CANCELLED);
        assert_eq!(w.downloads(), 1);
        assert_eq!(listing(&mods), before);
        assert!(!mods.join(vfs::DOWNLOAD_CACHE).exists());
        assert!(no_staging(&mods));
        assert!(!request_cancel(&mods.join(ID)));
    }

    #[test]
    fn cancel_update_keeps_old_pack() {
        const ID: &str = "Cancel-Update";
        let w = World::new();
        let manifest = remote_fixture_as(&w, &base_mods(), ID);
        let (mods, paks) = (scratch("sub-cancel-update"), scratch("sub-cancel-update-paks"));
        add_id(&w, ID, &mods, &paks).unwrap();
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3063", ID)).unwrap();
        let pack_dir = mods.join(ID);
        let before = snapshot(&pack_dir);
        let pack_json = fs::read(pack_dir.join("pack.json")).unwrap();
        let links_before = links(&paks, "");
        assert!(!links_before.is_empty());

        let (mut next, _) = bumped(&w, &manifest);
        next.mods.push(fixture_mod(&w, &new_mod()));
        set_remote(&w, &next);
        let downloads = w.downloads();
        let key = pack_dir.clone();
        w.owner.cancel_after(1, move || {
            request_cancel(&key);
        });
        let err = update_id(&w, &mods, &paks, ID, true, &[]).unwrap_err();
        assert_eq!(err, CANCELLED);
        assert_eq!(w.downloads() - downloads, 1);
        assert_eq!(snapshot(&pack_dir), before);
        assert_eq!(fs::read(pack_dir.join("pack.json")).unwrap(), pack_json);
        assert_eq!(links(&paks, ""), links_before);
        assert!(no_staging(&mods));

        let key = pack_dir.clone();
        w.owner.cancel_after(0, move || {
            request_cancel(&key);
        });
        let err = update_id(&w, &mods, &paks, ID, true, &["3063"]).unwrap_err();
        assert_eq!(err, CANCELLED);
        assert_eq!(snapshot(&pack_dir), before);
        assert!(no_staging(&mods));
    }

    #[test]
    fn cancel_flag_cleared_after_op() {
        const ID: &str = "Cancel-Flag";
        {
            let guard = OpGuard::begin(Path::new("Cancel-Guard"), OpKind::Download).unwrap();
            assert!(!guard.cancelled());
            assert!(request_cancel(Path::new("Cancel-Guard")));
            assert!(guard.cancelled());
        }
        assert!(!request_cancel(Path::new("Cancel-Guard")));
        assert!(!OpGuard::begin(Path::new("Cancel-Guard"), OpKind::Download).unwrap().cancelled());

        let w = World::new();
        let manifest = remote_fixture_as(&w, &base_mods(), ID);
        let (mods, paks) = (scratch("sub-cancel-flag"), scratch("sub-cancel-flag-paks"));
        let key = mods.join(ID);
        assert!(!request_cancel(&key));
        add_id(&w, ID, &mods, &paks).unwrap();
        assert!(!request_cancel(&key));

        bump(&w, &manifest);
        let hook_key = key.clone();
        w.owner.cancel_after(0, move || {
            request_cancel(&hook_key);
        });
        let err = update_id(&w, &mods, &paks, ID, false, &[]).unwrap_err();
        assert_eq!(err, CANCELLED);
        assert!(!request_cancel(&key));

        let summary = update_id(&w, &mods, &paks, ID, false, &[]).unwrap();
        assert_eq!(summary.pack.version.as_deref(), Some("1.1"));
        assert!(no_staging(&mods));
    }

    #[test]
    fn publish_and_download_refuse_each_other() {
        const ID: &str = "Publish-Clash";
        let busy = format!("{} is busy, try again when it finishes", NAME);
        let w = World::new();
        let manifest = remote_fixture_as(&w, &base_mods(), ID);

        // A publish started while an update downloads
        let (mods, paks) = (scratch("sub-clash"), scratch("sub-clash-paks"));
        add_id(&w, ID, &mods, &paks).unwrap();
        bump(&w, &manifest);
        let refused = Arc::new(std::sync::Mutex::new(None));
        let (seen, pack_dir, up) = (refused.clone(), mods.join(ID), w.owner.as_user(UPLOADER));
        w.owner.cancel_after(0, move || {
            let conn = Conn::new(up.as_user(UPLOADER), Arc::new(MemoryStore::default()));
            *seen.lock().unwrap() = crate::publish::publish_pack(&conn, &pack_dir, "9.0.0", 0, None, &mut |_| {}).err();
        });
        update_id(&w, &mods, &paks, ID, false, &[]).unwrap();
        assert_eq!(refused.lock().unwrap().as_deref(), Some(busy.as_str()));

        // A download started while a publish runs
        let (mods, paks) = (scratch("sub-clash-pub"), scratch("sub-clash-pub-paks"));
        let pack_dir = mods.join(ID);
        fs::create_dir_all(pack_dir.join("3063")).unwrap();
        fs::write(pack_dir.join("3063/a.pak"), "a").unwrap();
        let local = PackManifest { pack_id: ID.into(), name: NAME.into(), mod_ids: vec!["3063".into()], ..Default::default() };
        packs::write_manifest(&pack_dir, &local).unwrap();
        let before = listing(&mods);
        let mut refused = None;
        let downloads = w.downloads();
        crate::publish::publish_pack(&w.conn_as(UPLOADER), &pack_dir, "9.0.0", 0, None, &mut |_| {
            if refused.is_none() {
                refused = Some(add_id(&w, ID, &mods, &paks).unwrap_err());
            }
        })
        .unwrap();
        assert_eq!(refused, Some(busy));
        assert_eq!(w.downloads(), downloads);
        assert!(no_staging(&mods));
        let mut after = listing(&mods);
        after.retain(|p| *p != Path::new(ID).join(packs::CHANGELOG_FILE).display().to_string());
        assert_eq!(after, before);
    }

    #[test]
    fn second_op_on_a_pack_is_refused() {
        const ID: &str = "Busy-Pack";
        let w = World::new();
        let manifest = remote_fixture_as(&w, &base_mods(), ID);
        let (mods, paks) = (scratch("sub-busy"), scratch("sub-busy-paks"));
        add_id(&w, ID, &mods, &paks).unwrap();
        bump(&w, &manifest);
        let before = listing(&mods);
        let busy = format!("{} is busy, try again when it finishes", NAME);
        {
            let held = OpGuard::begin(&mods.join(ID), OpKind::Download).unwrap();
            assert!(OpGuard::begin(&mods.join(ID), OpKind::Download).is_none());
            let downloads = w.downloads();
            assert_eq!(add_id(&w, ID, &mods, &paks).unwrap_err(), busy);
            assert_eq!(update_id(&w, &mods, &paks, ID, false, &[]).unwrap_err(), busy);
            assert_eq!(update_id(&w, &mods, &paks, ID, false, &["3063"]).unwrap_err(), busy);
            assert_eq!(w.downloads(), downloads);
            assert_eq!(listing(&mods), before);
            assert!(!held.cancelled());
        }
        assert!(OpGuard::begin(&mods.join(ID), OpKind::Download).is_some());
        let summary = update_id(&w, &mods, &paks, ID, false, &[]).unwrap();
        assert_eq!(summary.pack.version.as_deref(), Some("1.1"));
    }

    #[test]
    fn uploader_invite_installs_nothing() {
        let w = World::new();
        w.owner.add_user("newbie", "Newbie", None);
        let link = w.owner.uploader_invite().unwrap().url;
        let mods = scratch("sub-uploader-invite");
        let out = add_from_invite(&w.conn_as("newbie"), &link, &mods, &mods.join("paks"), &mut |_| {}).unwrap();
        assert!(out.is_none());
        assert!(listing(&mods) == vec![String::new()]);
        assert_eq!(w.owner.as_user("newbie").me().unwrap().role.as_deref(), Some("uploader"));
        assert_eq!(w.downloads(), 0);
    }

    #[test]
    fn viewer_invite_installs_pack() {
        let w = World::new();
        remote_fixture(&w, &base_mods());
        w.owner.add_user("friend", "Friend", None);
        let link = w.owner.as_user(UPLOADER).pack_invite(PACK).unwrap().url;
        let mods = scratch("sub-viewer-invite");
        let summary = add_from_invite(&w.conn_as("friend"), &link, &mods, &mods.join("paks"), &mut |_| {}).unwrap().unwrap();
        assert_eq!(summary.keys, ["Casual-Shitters/3063", "Casual-Shitters/3311"]);
        assert_eq!(summary.pack.version.as_deref(), Some("1.0"));
        assert_eq!(fs::read(mods.join(PACK).join("3063").join(A_PAK)).unwrap(), b"vests-v1");
        assert_eq!(w.downloads(), 5);
        assert!(w.owner.as_user("friend").me().unwrap().packs.iter().any(|p| p.id == PACK));
    }

    #[test]
    fn install_saves_changelog_and_invite() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let link = w.owner.as_user(UPLOADER).pack_invite(PACK).unwrap().url;
        let mods = scratch("sub-changelog");
        let paks = mods.join("paks");
        add_from_invite(&w.conn(), &link, &mods, &paks, &mut |_| {}).unwrap().unwrap();
        let pack_dir = mods.join(PACK);
        let source = packs::read_manifest(&pack_dir).unwrap().source.unwrap();
        assert_eq!(source.kind, "server");
        assert_eq!(source.server.as_deref(), Some(BASE));
        assert_eq!(source.invite.as_deref(), Some(link.as_str()));
        assert_eq!(source.uploader.as_deref(), Some("Uploader"));
        let saved = packs::read_changelog(&pack_dir);
        assert_eq!(saved.entries, w.owner.changelog(PACK).unwrap());
        assert_eq!(saved.entries.len(), 1);
        assert_eq!(saved.entries[0].notes, "First");

        let base = packs::BaseSnapshot { name: "Origin".into(), ..Default::default() };
        packs::write_changelog(&pack_dir, &packs::Changelog { entries: saved.entries.clone(), base: Some(base.clone()) }).unwrap();
        bump(&w, &manifest);
        update(&w, &mods, &paks, false, &[]).unwrap();
        let saved = packs::read_changelog(&pack_dir);
        assert_eq!(saved.entries.iter().map(|e| e.version.as_str()).collect::<Vec<_>>(), ["1.1", "1.0"]);
        assert_eq!(saved.base, Some(base));
        let source = packs::read_manifest(&pack_dir).unwrap().source.unwrap();
        assert_eq!(source.invite.as_deref(), Some(link.as_str()));
    }

    #[test]
    fn changelog_failure_does_not_fail_install() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let (mods, paks) = (scratch("sub-changelog-fail"), scratch("sub-changelog-fail-paks"));
        w.owner.fail_changelog(1);
        add(&w, &mods, &paks).unwrap();
        let pack_dir = mods.join(PACK);
        assert!(packs::read_changelog(&pack_dir).entries.is_empty());

        let old = packs::Changelog {
            entries: vec![VersionEntry { version: "1.0".into(), published_at: 1, notes: "Saved".into() }],
            base: Some(packs::BaseSnapshot { name: "Origin".into(), ..Default::default() }),
        };
        packs::write_changelog(&pack_dir, &old).unwrap();
        bump(&w, &manifest);
        w.owner.fail_changelog(1);
        let summary = update(&w, &mods, &paks, false, &[]).unwrap();
        assert_eq!(summary.pack.version.as_deref(), Some("1.1"));
        assert_eq!(packs::read_changelog(&pack_dir), old);

        w.owner.fail_changelog(1);
        add(&w, &mods, &paks).unwrap();
        assert_eq!(packs::read_changelog(&pack_dir), old);
    }

    #[test]
    fn pack_changelog_falls_back_to_saved() {
        let w = World::new();
        let manifest = remote_fixture(&w, &base_mods());
        let mods = scratch("sub-pack-changelog");
        add(&w, &mods, &mods.join("paks")).unwrap();
        let pack_dir = mods.join(PACK);
        bump(&w, &manifest);
        let connect = |_: &str| w.conn();

        let fresh = load_changelog(&pack_dir, &connect);
        assert!(!fresh.saved);
        assert_eq!(fresh.entries.len(), 2);
        assert_eq!(packs::read_changelog(&pack_dir).entries, fresh.entries);

        let local = packs::Changelog {
            entries: vec![VersionEntry { version: "1.0".into(), published_at: 1, notes: "Saved".into() }],
            base: None,
        };
        packs::write_changelog(&pack_dir, &local).unwrap();
        w.owner.fail_daily_cap(1);
        let fallback = load_changelog(&pack_dir, &connect);
        assert!(fallback.saved);
        assert_eq!(fallback.entries, local.entries);

        let plain = mods.join("Plain");
        fs::create_dir_all(&plain).unwrap();
        packs::write_manifest(&plain, &PackManifest { pack_id: "Plain".into(), name: "Plain".into(), ..Default::default() }).unwrap();
        let offline = load_changelog(&plain, &connect);
        assert!(offline.saved && offline.entries.is_empty());
    }

    fn drive_pack(mods: &Path, id: &str) -> PathBuf {
        let dir = mods.join(id);
        fs::create_dir_all(dir.join("1001")).unwrap();
        fs::write(dir.join("1001/a.pak"), b"a").unwrap();
        let source = packs::PackSource { kind: "drive".into(), folder_id: Some("F".into()), ..Default::default() };
        let man = PackManifest { pack_id: id.into(), name: id.into(), version: Some("1".into()), mod_ids: vec!["1001".into()], source: Some(source), ..Default::default() };
        packs::write_manifest(&dir, &man).unwrap();
        dir
    }

    #[test]
    fn drive_packs_report_unsupported() {
        let w = World::new();
        let mods = scratch("sub-drive-unsupported");
        let dir = drive_pack(&mods, "Old-Drive");
        let checks = check_all(&mods, &|_: &str| w.conn());
        assert_eq!(checks.len(), 1);
        assert_eq!((checks[0].pack_id.as_str(), checks[0].status.as_str()), ("Old-Drive", "unsupported"));
        assert_eq!(checks[0].installed_version.as_deref(), Some("1"));
        assert_eq!(checks[0].error.as_deref(), Some(DRIVE_GONE));
        assert_eq!(update_id(&w, &mods, &mods.join("paks"), "Old-Drive", false, &[]).unwrap_err(), DRIVE_GONE);
        assert_eq!(update_id(&w, &mods, &mods.join("paks"), "Old-Drive", false, &["1001"]).unwrap_err(), DRIVE_GONE);
        assert_eq!(linked_pack(&mods, "Old-Drive").unwrap_err(), DRIVE_GONE);
        assert!(dir.join("1001/a.pak").is_file());
        assert_eq!(w.downloads(), 0);
    }

    #[test]
    fn checks_never_drop_a_pack() {
        let w = World::new();
        remote_fixture(&w, &base_mods());
        remote_fixture_as(&w, &base_mods(), "Gone-Pack");
        let mods = scratch("sub-check-all");
        add(&w, &mods, &mods.join("paks")).unwrap();
        add_id(&w, "Gone-Pack", &mods, &mods.join("paks")).unwrap();
        w.owner.as_user(UPLOADER).delete_pack("Gone-Pack").unwrap();
        drive_pack(&mods, "Old-Drive");
        let plain = mods.join("Plain");
        fs::create_dir_all(&plain).unwrap();
        packs::write_manifest(&plain, &PackManifest { pack_id: "Plain".into(), name: "Plain".into(), ..Default::default() }).unwrap();
        let pack_dir = mods.join(PACK);
        let mut man = packs::read_manifest(&pack_dir).unwrap();
        man.source.as_mut().unwrap().uploader = Some("Old name".into());
        packs::write_manifest(&pack_dir, &man).unwrap();

        let mut checks = check_all(&mods, &|_: &str| w.conn());
        checks.sort_by(|a, b| a.pack_id.cmp(&b.pack_id));
        let got: Vec<(&str, &str)> = checks.iter().map(|c| (c.pack_id.as_str(), c.status.as_str())).collect();
        assert_eq!(got, [(PACK, "current"), ("Gone-Pack", "error"), ("Old-Drive", "unsupported")]);
        assert_eq!(
            checks[1].error.as_deref(),
            Some("This pack is no longer available to you. It was removed from the server or you were removed from it.")
        );
        assert_eq!(checks[1].installed_version.as_deref(), Some("1.0"));
        let source = packs::read_manifest(&pack_dir).unwrap().source.unwrap();
        assert_eq!(source.uploader.as_deref(), Some("Uploader"));
    }

    #[test]
    fn daily_cap_shows_on_the_pack() {
        let w = World::new();
        remote_fixture(&w, &base_mods());
        let mods = scratch("sub-daily-cap");
        add(&w, &mods, &mods.join("paks")).unwrap();
        w.owner.fail_daily_cap(10);
        let checks = check_all(&mods, &|_: &str| w.conn());
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, "error");
        assert_eq!(checks[0].error.as_deref(), Some(UNAVAILABLE));
        assert!(w.signed_in());
    }

    const FORK: &str = "Casual-Fork";
    const OWN_PUBLISH: &str = "You publish this pack. Fork it to follow another pack.";

    fn plain_mod(id: &'static str, files: Vec<(&'static str, &'static str)>) -> FixtureMod {
        FixtureMod { id, files, addons: vec![] }
    }

    // Three files, of which the fork keeps two and adds one
    fn old_mods() -> Vec<FixtureMod> {
        vec![plain_mod("3063", vec![(A_PAK, "vests-v1")]), plain_mod("3311", vec![("b.pak", "blood-v1"), ("c.pak", "extra")])]
    }

    fn fork_mods() -> Vec<FixtureMod> {
        vec![
            plain_mod("3063", vec![(A_PAK, "vests-v1")]),
            plain_mod("3311", vec![("b.pak", "blood-v1")]),
            plain_mod("3500", vec![("n.pak", "new-v1")]),
        ]
    }

    fn invite(w: &World, pack_id: &str) -> String {
        w.owner.as_user(UPLOADER).pack_invite(pack_id).unwrap().url
    }

    fn preview(w: &World, pack_id: &str, link: &str, mods: &Path) -> Result<SwitchPreview, String> {
        preview_switch_with(&w.conn(), pack_id, link, mods)
    }

    fn switch(w: &World, pack_id: &str, new_pack_id: &str, link: &str, mods: &Path, paks: &Path) -> Result<ImportSummary, String> {
        switch_as(w, pack_id, new_pack_id, link, false, mods, paks).map(|s| s.import)
    }

    fn switch_as(w: &World, pack_id: &str, new_pack_id: &str, link: &str, active: bool, mods: &Path, paks: &Path) -> Result<SwitchSummary, String> {
        switch_pack_with(&w.conn(), pack_id, new_pack_id, Some(link), active, mods, paks, &mut |_| {})
    }

    /// The old pack installed, the fork on the server and the fork's invite link
    fn fork_world(name: &str) -> (World, PathBuf, PathBuf, String) {
        let w = World::new();
        remote_fixture(&w, &old_mods());
        let (mods, paks) = (scratch(name), scratch(&format!("{}-paks", name)));
        add(&w, &mods, &paks).unwrap();
        remote_fixture_as(&w, &fork_mods(), FORK);
        let link = invite(&w, FORK);
        (w, mods, paks, link)
    }

    // Every entry under dir with its content, the download cache included
    fn tree(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
        walkdir::WalkDir::new(dir)
            .into_iter()
            .map(|e| e.unwrap())
            .map(|e| {
                let rel = e.path().strip_prefix(dir).unwrap().display().to_string();
                (rel, e.file_type().is_file().then(|| fs::read(e.path()).unwrap()))
            })
            .collect()
    }

    #[test]
    fn preview_counts_reused_files() {
        let (w, mods, _, link) = fork_world("switch-preview");
        let p = preview(&w, PACK, &link, &mods).unwrap();
        assert_eq!((p.base.as_str(), p.new_pack_id.as_str(), p.name.as_str(), p.version.as_str()), (BASE, FORK, NAME, "1.0"));
        assert_eq!(p.uploader.as_deref(), Some("Uploader"));
        assert_eq!((p.reused_files, p.total_files, p.download_bytes), (2, 3, 6));

        let cache = cache_dir(&mods, FORK);
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join(hashing::sha256_bytes(b"new-v1")), b"new-v1").unwrap();
        let p = preview(&w, PACK, &link, &mods).unwrap();
        assert_eq!((p.reused_files, p.total_files, p.download_bytes), (3, 3, 0));

        let up = w.owner.uploader_invite().unwrap().url;
        assert_eq!(preview(&w, PACK, &up, &mods).unwrap_err(), "That is an uploader invite, not a pack link.");
        assert_eq!(w.owner.as_user(VIEWER).me().unwrap().role, None);
        assert_eq!(w.downloads(), 3);
    }

    #[test]
    fn preview_writes_nothing() {
        let (w, mods, _, link) = fork_world("switch-preview-nothing");
        let cache = cache_dir(&mods, FORK);
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join(hashing::sha256_bytes(b"new-v1")), b"damage").unwrap();
        let before = tree(&mods);
        let downloads = w.downloads();
        let p = preview(&w, PACK, &link, &mods).unwrap();
        assert_eq!(p.new_pack_id, FORK);
        assert_eq!(tree(&mods), before);
        assert_eq!(w.downloads(), downloads);
    }

    #[test]
    fn switch_to_fork_reuses_files() {
        let (w, mods, paks, link) = fork_world("switch-fork");
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3063", PACK)).unwrap();
        assert!(!links(&paks, "").is_empty());
        let downloads = w.downloads();
        let summary = switch(&w, PACK, FORK, &link, &mods, &paks).unwrap();
        assert_eq!(summary.pack.pack_id, FORK);
        assert_eq!(w.downloads() - downloads, 1);
        assert!(!mods.join(PACK).exists());
        assert!(links(&paks, "Casual-Shitters.").is_empty());

        let dir = mods.join(FORK);
        assert_eq!(fs::read(dir.join("3063").join(A_PAK)).unwrap(), b"vests-v1");
        assert_eq!(fs::read(dir.join("3311/b.pak")).unwrap(), b"blood-v1");
        assert_eq!(fs::read(dir.join("3500/n.pak")).unwrap(), b"new-v1");
        assert!(!dir.join("3311/c.pak").exists());
        let man = packs::read_manifest(&dir).unwrap();
        assert_eq!(man.pack_id, FORK);
        let source = man.source.unwrap();
        assert_eq!((source.kind.as_str(), source.server.as_deref()), ("server", Some(BASE)));
        assert_eq!((source.invite.as_deref(), source.uploader.as_deref()), (Some(link.as_str()), Some("Uploader")));
        assert_eq!(packs::read_changelog(&dir).entries, w.owner.changelog(FORK).unwrap());
        assert!(!mods.join(vfs::DOWNLOAD_CACHE).exists());
        assert!(no_staging(&mods));
    }

    #[test]
    fn switch_moves_active_preset() {
        let (w, mods, paks, link) = fork_world("switch-preset");
        let summary = switch(&w, PACK, FORK, &link, &mods, &paks).unwrap();
        let mut config = crate::config::AppConfig { active_preset: Some(format!("pack:{}", PACK)), ..Default::default() };
        config.presets.insert("Mine".into(), vec!["5543".into()]);
        presets_after_switch(&mut config, PACK, &summary);
        assert_eq!(config.active_preset.as_deref(), Some("pack:Casual-Fork"));
        assert_eq!(config.presets["Mine"], ["5543"]);
    }

    #[test]
    fn switch_refuses_own_published_pack() {
        let (w, mods, paks, link) = fork_world("switch-own");
        let pack_dir = mods.join(PACK);
        let mut man = packs::read_manifest(&pack_dir).unwrap();
        man.source = None;
        man.published = Some(packs::Published { server: Some(BASE.into()), dirty: false });
        packs::write_manifest(&pack_dir, &man).unwrap();
        let before = tree(&mods);
        let downloads = w.downloads();
        assert_eq!(preview(&w, PACK, &link, &mods).unwrap_err(), OWN_PUBLISH);
        assert_eq!(switch(&w, PACK, FORK, &link, &mods, &paks).unwrap_err(), OWN_PUBLISH);
        assert_eq!(tree(&mods), before);
        assert_eq!(w.downloads(), downloads);

        // A Drive publish counts as never published
        man.published = Some(packs::Published { server: None, dirty: false });
        packs::write_manifest(&pack_dir, &man).unwrap();
        assert_eq!(preview(&w, PACK, &link, &mods).unwrap().reused_files, 2);
    }

    #[test]
    fn switch_refuses_installed_target() {
        let (w, mods, paks, link) = fork_world("switch-installed");
        add_id(&w, FORK, &mods, &paks).unwrap();
        let before = tree(&mods);
        let downloads = w.downloads();
        let err = "Casual-Fork is already installed. Delete it first or update it instead.";
        assert_eq!(preview(&w, PACK, &link, &mods).unwrap_err(), err);
        assert_eq!(switch(&w, PACK, FORK, &link, &mods, &paks).unwrap_err(), err);
        assert_eq!(tree(&mods), before);
        assert_eq!(w.downloads(), downloads);
    }

    #[test]
    fn failed_switch_keeps_old_pack() {
        let (w, mods, paks, link) = fork_world("switch-failed");
        let pack_dir = mods.join(PACK);
        let before = snapshot(&pack_dir);
        let untouched = |mods: &Path| {
            assert_eq!(snapshot(&pack_dir), before);
            assert!(!mods.join(FORK).exists());
            assert!(no_staging(mods));
        };

        let server = w.owner.as_user(OWNER);
        w.owner.cancel_after(0, move || server.fail_daily_cap(1));
        assert_eq!(switch(&w, PACK, FORK, &link, &mods, &paks).unwrap_err(), UNAVAILABLE);
        untouched(&mods);

        let key = mods.join(FORK);
        w.owner.cancel_after(0, move || {
            request_cancel(&key);
        });
        assert_eq!(switch(&w, PACK, FORK, &link, &mods, &paks).unwrap_err(), CANCELLED);
        untouched(&mods);

        switch(&w, PACK, FORK, &link, &mods, &paks).unwrap();
        assert!(!pack_dir.exists());
    }

    // A pack.json and files for a pack that was not installed from a pack server
    fn offline_pack(mods: &Path, id: &str, source: Option<packs::PackSource>, record: bool) -> PathBuf {
        let pack_dir = mods.join(id);
        let mut installed_hashes = BTreeMap::new();
        for m in old_mods() {
            let mut rec = packs::InstalledMod::default();
            for (path, body) in m.files {
                fs::create_dir_all(pack_dir.join(m.id)).unwrap();
                fs::write(pack_dir.join(m.id).join(path), body).unwrap();
                rec.files.insert(path.to_string(), hashing::sha256_bytes(body.as_bytes()));
            }
            if record {
                installed_hashes.insert(m.id.to_string(), rec);
            }
        }
        let man = PackManifest {
            pack_id: id.into(),
            name: id.into(),
            version: Some("0.9".into()),
            mod_ids: vec!["3063".into(), "3311".into()],
            source,
            installed_hashes,
            ..Default::default()
        };
        packs::write_manifest(&pack_dir, &man).unwrap();
        pack_dir
    }

    #[test]
    fn switch_drive_pack_to_server() {
        let w = World::new();
        let (mods, paks) = (scratch("switch-drive"), scratch("switch-drive-paks"));
        let drive = packs::PackSource { kind: "drive".into(), folder_id: Some("F".into()), ..Default::default() };
        let pack_dir = offline_pack(&mods, PACK, Some(drive), true);
        remote_fixture(&w, &fork_mods());
        let link = invite(&w, PACK);
        let p = preview(&w, PACK, &link, &mods).unwrap();
        assert_eq!((p.new_pack_id.as_str(), p.reused_files, p.total_files), (PACK, 2, 3));

        switch(&w, PACK, PACK, &link, &mods, &paks).unwrap();
        assert_eq!(w.downloads(), 1);
        assert_eq!(fs::read(pack_dir.join("3500/n.pak")).unwrap(), b"new-v1");
        assert!(!pack_dir.join("3311/c.pak").exists());
        let source = packs::read_manifest(&pack_dir).unwrap().source.unwrap();
        assert_eq!((source.kind.as_str(), source.server.as_deref(), source.folder_id), ("server", Some(BASE), None));
        assert_eq!(source.invite.as_deref(), Some(link.as_str()));
        assert_eq!(packs::read_changelog(&pack_dir).entries, w.owner.changelog(PACK).unwrap());
        assert!(!mods.join(vfs::DOWNLOAD_CACHE).exists());
    }

    #[test]
    fn switch_local_pack_hashes_its_files() {
        let w = World::new();
        let (mods, paks) = (scratch("switch-local"), scratch("switch-local-paks"));
        let old = offline_pack(&mods, "My-Pack", None, false);
        remote_fixture_as(&w, &fork_mods(), FORK);
        let link = invite(&w, FORK);
        assert_eq!(preview(&w, "My-Pack", &link, &mods).unwrap().reused_files, 2);
        switch(&w, "My-Pack", FORK, &link, &mods, &paks).unwrap();
        assert_eq!(w.downloads(), 1);
        assert!(!old.exists());
        assert_eq!(fs::read(mods.join(FORK).join("3063").join(A_PAK)).unwrap(), b"vests-v1");
    }

    #[test]
    fn switch_keeps_enabled_mods_and_order() {
        let (w, mods, paks, link) = fork_world("switch-keep-setup");
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3063", PACK)).unwrap();
        let summary = switch_as(&w, PACK, FORK, &link, true, &mods, &paks).unwrap();
        assert!(summary.not_enabled.is_empty());
        let dir = mods.join(FORK);
        assert_eq!(packs::read_manifest(&dir).unwrap().mod_ids, ["3063", "3311", "3500"]);
        // 3063 stays on, 3311 stays off, 3500 is new to an active pack so it comes on
        let on: Vec<bool> = ["3063", "3311", "3500"].iter().map(|id| vfs::flag_enabled(&dir.join(id))).collect();
        assert_eq!(on, [true, false, true]);
        assert!(links(&paks, "Casual-Shitters.").is_empty());
        assert_eq!(links(&paks, "Casual-Fork.3063_").into_values().collect::<Vec<_>>(), [b"vests-v1".to_vec()]);
        assert!(links(&paks, "Casual-Fork.3311_").is_empty());
        assert_eq!(links(&paks, "Casual-Fork.3500_").len(), 1);

        // Not the active pack, so a mod new to it stays off
        let (w, mods, paks, link) = fork_world("switch-keep-setup-inactive");
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3311", PACK)).unwrap();
        switch_as(&w, PACK, FORK, &link, false, &mods, &paks).unwrap();
        let dir = mods.join(FORK);
        let on: Vec<bool> = ["3063", "3311", "3500"].iter().map(|id| vfs::flag_enabled(&dir.join(id))).collect();
        assert_eq!(on, [false, true, false]);
        assert_eq!(links(&paks, "").keys().map(|k| k.as_str()).collect::<Vec<_>>(), ["Casual-Fork.3311_b.pak"]);
    }

    #[test]
    fn switch_renames_preset_keys() {
        let (w, mods, paks, link) = fork_world("switch-preset-keys");
        let summary = switch(&w, PACK, FORK, &link, &mods, &paks).unwrap();
        let mut config = crate::config::AppConfig { active_preset: Some(format!("pack:{}", PACK)), ..Default::default() };
        let old = |id: &str| format!("{}/{}", PACK, id);
        config.presets.insert("Mine".into(), vec!["5543".into(), old("3063"), old("4296"), old("3311")]);
        config.presets.insert("Only gone".into(), vec![old("4296")]);
        presets_after_switch(&mut config, PACK, &summary);
        assert_eq!(config.presets["Mine"], ["5543", "Casual-Fork/3063", "Casual-Fork/3311"]);
        assert!(!config.presets.contains_key("Only gone"));
        assert_eq!(config.active_preset.as_deref(), Some("pack:Casual-Fork"));
    }

    #[test]
    fn drive_era_publish_is_not_own_pack() {
        let w = World::new();
        remote_fixture(&w, &old_mods());
        let (mods, paks) = (scratch("sub-drive-era"), scratch("sub-drive-era-paks"));
        add(&w, &mods, &paks).unwrap();
        let pack_dir = mods.join(PACK);
        let mut man = packs::read_manifest(&pack_dir).unwrap();
        man.published = Some(packs::Published { server: None, dirty: false });
        packs::write_manifest(&pack_dir, &man).unwrap();
        add(&w, &mods, &paks).unwrap();

        let mut man = packs::read_manifest(&pack_dir).unwrap();
        man.source = Some(packs::PackSource { kind: "drive".into(), folder_id: Some("F".into()), ..Default::default() });
        man.published = Some(packs::Published { server: None, dirty: false });
        packs::write_manifest(&pack_dir, &man).unwrap();
        let link = invite(&w, PACK);
        assert_eq!(preview(&w, PACK, &link, &mods).unwrap().reused_files, 3);
        switch(&w, PACK, PACK, &link, &mods, &paks).unwrap();
        assert_eq!(packs::read_manifest(&pack_dir).unwrap().source.unwrap().kind, "server");
    }

    #[cfg(unix)]
    #[test]
    fn switch_skips_files_behind_a_symlinked_folder() {
        let (w, mods, paks, link) = fork_world("switch-symlink");
        let outside = scratch("switch-symlink-outside");
        fs::write(outside.join("b.pak"), b"blood-v1").unwrap();
        fs::write(outside.join("c.pak"), b"extra").unwrap();
        let blood = mods.join(PACK).join("3311");
        fs::remove_dir_all(&blood).unwrap();
        std::os::unix::fs::symlink(&outside, &blood).unwrap();

        assert_eq!(preview(&w, PACK, &link, &mods).unwrap().reused_files, 1);
        let downloads = w.downloads();
        switch(&w, PACK, FORK, &link, &mods, &paks).unwrap();
        assert_eq!(w.downloads() - downloads, 2);
        assert_eq!(fs::read(mods.join(FORK).join("3311/b.pak")).unwrap(), b"blood-v1");
        assert_ne!(inode(&mods.join(FORK).join("3311/b.pak")), inode(&outside.join("b.pak")));
        assert!(outside.join("b.pak").is_file());
    }
}
