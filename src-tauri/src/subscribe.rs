use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::Emitter;

use crate::drive::{nap, DriveApi, HttpDrive, Progress, BLOCKED, CANCELLED, SIZE_MISMATCH};
use crate::hashing;
use crate::packs::{self, InstalledMod, PackManifest, PackRef, PackSource};
use crate::publish::{progress_event, RemoteFileEntry, RemoteManifest, RemoteMod, Throttle, FORMAT, MANIFEST_NAME};
use crate::vfs::{self, ImportPreview, ImportSummary, ModRef, PackArchive, SwapError};

const NOT_A_LINK: &str = "That is not a Google Drive folder link";
const NO_PACK: &str = "No modpack found at that link";
const NO_API_KEY: &str = "This build cannot open Google Drive links";
const MIN_ID_LEN: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct DriveLink {
    pub folder_id: String,
    pub resource_key: Option<String>,
}

impl DriveLink {
    fn resource_keys(&self) -> Vec<(String, String)> {
        self.resource_key.iter().map(|k| (self.folder_id.clone(), k.clone())).collect()
    }
}

fn is_drive_token(s: &str, min_len: usize) -> bool {
    s.len() >= min_len && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

pub fn parse_link(input: &str) -> Result<DriveLink, String> {
    let input = input.trim();
    if is_drive_token(input, MIN_ID_LEN) {
        return Ok(DriveLink { folder_id: input.to_string(), resource_key: None });
    }
    let full = if input.starts_with("drive.google.com/") { format!("https://{}", input) } else { input.to_string() };
    let url = reqwest::Url::parse(&full).map_err(|_| NOT_A_LINK.to_string())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str() != Some("drive.google.com") {
        return Err(NOT_A_LINK.to_string());
    }
    let query = |name: &str| url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned());
    let segments: Vec<&str> = url.path_segments().map(|s| s.filter(|p| !p.is_empty()).collect()).unwrap_or_default();
    let id = match segments.as_slice() {
        ["drive", "folders", id] => Some(id.to_string()),
        ["drive", "u", n, "folders", id] if is_digits(n) => Some(id.to_string()),
        ["open"] => query("id"),
        _ => None,
    };
    let folder_id = id.filter(|id| is_drive_token(id, MIN_ID_LEN)).ok_or(NOT_A_LINK)?;
    let resource_key = match query("resourcekey") {
        Some(k) if is_drive_token(&k, 1) => Some(k),
        Some(_) => return Err(NOT_A_LINK.to_string()),
        None => None,
    };
    Ok(DriveLink { folder_id, resource_key })
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

#[derive(Clone, Debug)]
pub struct RemotePack {
    pub link: DriveLink,
    // kept for the update flow, which rereads this file
    #[allow(dead_code)]
    pub manifest_file_id: String,
    pub manifest: RemoteManifest,
}

// The manifest as published, refusing a newer format. Its contents are not checked here.
fn load_manifest(drive: &dyn DriveApi, link: &DriveLink) -> Result<(String, RemoteManifest), String> {
    let children = match drive.list(&link.folder_id) {
        Ok(c) => c,
        Err(e) if e.starts_with("Google Drive error 404") => return Err(NO_PACK.to_string()),
        Err(e) => return Err(e),
    };
    let file = children.into_iter().find(|f| !f.is_folder() && f.name == MANIFEST_NAME).ok_or(NO_PACK)?;
    let value: Value = serde_json::from_slice(&drive.read(&file.id)?).map_err(|_| NO_PACK.to_string())?;
    if value.get("format").and_then(|f| f.as_u64()).is_some_and(|f| f > u64::from(FORMAT)) {
        return Err("Update RON Mod Manager to install this pack".to_string());
    }
    let manifest = serde_json::from_value(value).map_err(|_| NO_PACK.to_string())?;
    Ok((file.id, manifest))
}

fn validate(manifest: &RemoteManifest) -> Result<(), String> {
    if packs::parse_key(&format!("{}/1", manifest.pack_id)).is_err() || is_reserved_name(&manifest.pack_id) {
        return Err(format!("Invalid pack id in pack: {}", manifest.pack_id));
    }
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
            if !is_drive_token(&f.file_id, 1) {
                return Err(format!("Invalid file id in pack: {}", f.file_id));
            }
        }
    }
    Ok(())
}

pub fn fetch_remote(drive: &dyn DriveApi, link: &DriveLink) -> Result<RemotePack, String> {
    let (manifest_file_id, manifest) = load_manifest(drive, link)?;
    if manifest.status == "updating" {
        return Err(format!("{} is being updated, try again in a few minutes.", manifest.name));
    }
    validate(&manifest)?;
    Ok(RemotePack { link: link.clone(), manifest_file_id, manifest })
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

// Refuses to replace the creator's own pack or to install beside a folder that differs only in case
fn check_target(mods_dir: &Path, pack_id: &str) -> Result<(), String> {
    if let Some(existing) = packs::read_manifest(&mods_dir.join(pack_id)).filter(|m| m.published.is_some()) {
        return Err(format!("{} is your own published pack. Fork or delete it before adding it from a link.", existing.name));
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
pub struct LinkPreview {
    pub preview: ImportPreview,
    pub download_bytes: u64,
}

pub fn inspect_link(drive: &dyn DriveApi, link: &DriveLink, mods_dir: &Path) -> Result<LinkPreview, String> {
    let remote = fetch_remote(drive, link)?;
    check_target(mods_dir, &remote.manifest.pack_id)?;
    let archive = pack_archive(&remote.manifest);
    let plan = vfs::plan_import(&archive, mods_dir)?;
    let download_bytes = sum_sizes(remote.manifest.mods.iter().flat_map(|m| &m.files));
    Ok(LinkPreview {
        preview: ImportPreview {
            pack: archive.pack,
            installed_version: plan.installed_version,
            added: plan.added,
            removed: plan.removed,
            changed: plan.changed,
            superseded: plan.superseded,
        },
        download_bytes,
    })
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

// Every op that writes to a pack, keyed by pack folder. The cancel flag is only read by downloads.
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

/// Asks the download running for a pack to stop. False when none is running.
pub(crate) fn request_cancel(pack_dir: &Path) -> bool {
    match running().get(pack_dir) {
        Some(op) if op.kind == OpKind::Download => {
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

fn stage_remote(
    drive: &dyn DriveApi,
    remote: &RemotePack,
    mod_ids: Vec<String>,
    staged: &Path,
    cancel: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(Progress),
) -> Result<(), String> {
    let manifest = &remote.manifest;
    fs::create_dir_all(staged).map_err(|e| format!("Failed to create staging folder: {}", e))?;
    let total = manifest.mods.iter().map(|m| m.files.len()).sum();
    let bytes_total = manifest.mods.iter().flat_map(|m| &m.files).fold(0u64, |sum, f| sum.saturating_add(f.size));
    let mut fetch = Fetch { drive, manifest, done: 0, total, bytes_done: 0, bytes_total, throttle: Throttle::new(), cancel, progress };
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
    write_staged_manifest(remote, mod_ids, staged)
}

// Seconds to wait before each retry of a file Google blocked as automated traffic
#[cfg(not(test))]
const BLOCKED_WAITS: [u64; 4] = [30, 60, 120, 240];
#[cfg(test)]
const BLOCKED_WAITS: [u64; 4] = [0, 0, 0, 0];

// Downloads files into staging with their hashes checked, and reports progress for the pack
struct Fetch<'a> {
    drive: &'a dyn DriveApi,
    manifest: &'a RemoteManifest,
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
        let (pack_id, done, total) = (self.manifest.pack_id.as_str(), self.done, self.total);
        let (before, bytes_total) = (self.bytes_done, self.bytes_total);
        let corrupted = || format!("A download from {} was corrupted ({}). Nothing was changed.", self.manifest.name, f.path);
        let mut waits = BLOCKED_WAITS.iter();
        let downloaded = loop {
            let mut got = 0u64;
            let (throttle, progress) = (&mut self.throttle, &mut *self.progress);
            let result = self.drive.download(&f.file_id, &dest, f.size, cancel, &mut |n| {
                got += n;
                if throttle.ready() {
                    let event = progress_event(pack_id, "downloading", done, total, Some(&f.path), (got, f.size));
                    progress(event.overall(before + got.min(f.size), bytes_total));
                }
            });
            match (&result, waits.next()) {
                (Err(e), Some(&secs)) if e == BLOCKED => {
                    for left in (1..=secs).rev() {
                        let mut event = progress_event(pack_id, "waiting", done, total, Some(&f.path), (0, f.size));
                        event.wait_secs = left;
                        (self.progress)(event.overall(before, bytes_total));
                        if !nap(Duration::from_secs(1), cancel) {
                            return Err(CANCELLED.to_string());
                        }
                    }
                }
                _ => break result,
            }
        };
        let sha = match downloaded {
            Err(e) if e == SIZE_MISMATCH => return Err(corrupted()),
            other => other?,
        };
        if !sha.eq_ignore_ascii_case(&f.sha256) {
            return Err(corrupted());
        }
        self.done += 1;
        self.bytes_done = before.saturating_add(f.size);
        let event = progress_event(pack_id, "downloading", self.done, total, Some(&f.path), (f.size, f.size));
        (self.progress)(event.overall(self.bytes_done, bytes_total));
        Ok(())
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

fn write_staged_manifest(remote: &RemotePack, mod_ids: Vec<String>, staged: &Path) -> Result<(), String> {
    let manifest = &remote.manifest;
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
            source: Some(PackSource {
                kind: "drive".to_string(),
                folder_id: remote.link.folder_id.clone(),
                resource_key: remote.link.resource_key.clone(),
            }),
            installed_hashes,
            published: None,
            local_hashes: BTreeMap::new(),
        },
    )
}

pub fn add_from_link(
    drive: &dyn DriveApi,
    link: &DriveLink,
    mods_dir: &Path,
    paks_dir: &Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<ImportSummary, String> {
    let remote = fetch_remote(drive, link)?;
    check_target(mods_dir, &remote.manifest.pack_id)?;
    let archive = pack_archive(&remote.manifest);
    let plan = vfs::plan_import(&archive, mods_dir)?;
    let op = OpGuard::begin_named(&mods_dir.join(&remote.manifest.pack_id), OpKind::Download, &remote.manifest.name)?;
    let staging = mods_dir.join(format!(".import-{}", uuid::Uuid::new_v4()));
    let staged = staging.join(&archive.pack.pack_id);
    let mod_ids = archive.folders.iter().map(|(id, _)| id.clone()).collect();
    if let Err(e) = stage_remote(drive, &remote, mod_ids, &staged, &|| op.cancelled(), progress) {
        return Err(vfs::abandon_staging(&staging, e));
    }
    vfs::finish_import(&archive, plan, mods_dir, paks_dir, &staging)
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
}

pub fn check_pack(drive: &dyn DriveApi, pack_dir: &Path) -> Result<UpdateCheck, String> {
    let local = packs::read_manifest(pack_dir).ok_or_else(|| format!("Could not read {}", pack_dir.join("pack.json").display()))?;
    let source = local.source.as_ref().ok_or_else(|| format!("{} is not linked to a Drive folder", local.name))?;
    let link = DriveLink { folder_id: source.folder_id.clone(), resource_key: source.resource_key.clone() };
    let (_, remote) = load_manifest(drive, &link)?;
    if remote.pack_id != local.pack_id {
        return Err(format!("The link for {} now holds a different pack ({})", local.name, remote.pack_id));
    }
    let mut check = UpdateCheck {
        pack_id: local.pack_id.clone(),
        status: "current".to_string(),
        installed_version: local.version.clone(),
        remote_version: Some(remote.version.clone()),
        added: Vec::new(),
        changed: Vec::new(),
        removed: Vec::new(),
        download_bytes: 0,
    };
    if remote.status == "updating" {
        check.status = "updating".to_string();
        return Ok(check);
    }
    validate(&remote)?;
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

fn public_drive(link: &DriveLink) -> Result<HttpDrive, String> {
    let api_key = crate::google::api_key().ok_or(NO_API_KEY)?;
    Ok(HttpDrive::public(api_key, link.resource_keys()))
}

#[tauri::command]
pub async fn inspect_pack_link(app_handle: tauri::AppHandle, link: String) -> Result<LinkPreview, String> {
    crate::google::api_key().ok_or(NO_API_KEY)?;
    let link = parse_link(&link)?;
    let mods_dir = vfs::get_mods_dir(&app_handle)?;
    tauri::async_runtime::spawn_blocking(move || inspect_link(&public_drive(&link)?, &link, &mods_dir))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn add_pack_from_link(app_handle: tauri::AppHandle, link: String) -> Result<ImportSummary, String> {
    crate::google::api_key().ok_or(NO_API_KEY)?;
    let link = parse_link(&link)?;
    let mods_dir = vfs::get_mods_dir(&app_handle)?;
    // without a game path nothing can be linked, so a Paks folder that does not exist is enough
    let paks_dir = vfs::get_paks_dir(&app_handle).unwrap_or_else(|_| mods_dir.join(".no-game-path"));
    let handle = app_handle.clone();
    let summary = tauri::async_runtime::spawn_blocking(move || {
        let drive = public_drive(&link)?;
        let mut emit = |p: Progress| {
            let _ = handle.emit("pack-progress", p);
        };
        add_from_link(&drive, &link, &mods_dir, &paks_dir, &mut emit)
    })
    .await
    .map_err(|e| e.to_string())??;

    let mut config = crate::config::load_config(app_handle.clone())?;
    crate::config::tidy_presets_after_import(&mut config, &summary.pack.pack_id, &summary.superseded_ids, &summary.removed_keys);
    crate::config::save_config(app_handle, config)
        .map_err(|e| format!("The pack was added but saved presets could not be updated: {}", e))?;
    Ok(summary)
}

fn check_all(mods_dir: &Path, api_key: &str) -> Vec<UpdateCheck> {
    let mut out = Vec::new();
    for name in vfs::child_dirs(mods_dir).unwrap_or_default() {
        let pack_dir = mods_dir.join(&name);
        let Some(source) = packs::read_manifest(&pack_dir).and_then(|m| m.source) else { continue };
        let link = DriveLink { folder_id: source.folder_id, resource_key: source.resource_key };
        let drive = HttpDrive::public(api_key, link.resource_keys());
        match check_pack(&drive, &pack_dir) {
            Ok(check) => out.push(check),
            Err(e) => eprintln!("[SUBSCRIBE] Could not check {} for updates: {}", name, e),
        }
    }
    out
}

#[tauri::command]
pub async fn check_pack_updates(app_handle: tauri::AppHandle) -> Result<Vec<UpdateCheck>, String> {
    let api_key = crate::google::api_key().ok_or(NO_API_KEY)?;
    let mods_dir = vfs::get_mods_dir(&app_handle)?;
    tauri::async_runtime::spawn_blocking(move || check_all(&mods_dir, api_key))
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

// The installed pack for an id from the frontend, which must be linked to Drive
fn linked_pack(mods_dir: &Path, pack_id: &str) -> Result<(PathBuf, PackManifest, DriveLink), String> {
    let pack_dir = mods_dir.join(pack_id);
    let local = packs::parse_key(&format!("{}/1", pack_id))
        .ok()
        .and_then(|_| packs::read_manifest(&pack_dir))
        .ok_or_else(|| format!("No modpack folder for {:?}", pack_id))?;
    let source = local.source.as_ref().ok_or_else(|| format!("{} is not linked to a Drive folder", local.name))?;
    let link = DriveLink { folder_id: source.folder_id.clone(), resource_key: source.resource_key.clone() };
    Ok((pack_dir, local, link))
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
    drive: &dyn DriveApi,
    manifest: &RemoteManifest,
    plans: &[(&RemoteMod, Stage)],
    staged: &Path,
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
    let mut fetch = Fetch { drive, manifest, done: 0, total, bytes_done: 0, bytes_total, throttle: Throttle::new(), cancel, progress };
    for (m, download) in downloads() {
        let mod_dir = staged.join(&m.id);
        fs::create_dir_all(&mod_dir).map_err(|e| format!("Failed to create {}: {}", mod_dir.display(), e))?;
        for f in download {
            fetch.file(f, &mod_dir, staged)?;
        }
    }
    Ok(fetch.done)
}

// Moves the kept files into staging and writes the staged pack.json, addon states and flags
fn assemble(
    moves: &mut Moves,
    remote: &RemotePack,
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
    write_staged_manifest(remote, plans.iter().map(|(m, _)| m.id.clone()).collect(), staged)?;
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
    drive: &dyn DriveApi,
    mods_dir: &Path,
    paks_dir: &Path,
    pack_id: &str,
    active: bool,
    force: &[String],
    progress: &mut dyn FnMut(Progress),
) -> Result<UpdateSummary, String> {
    let (pack_dir, local, link) = linked_pack(mods_dir, pack_id)?;
    let op = OpGuard::begin_named(&pack_dir, OpKind::Download, &local.name)?;
    let remote = fetch_remote(drive, &link)?;
    let manifest = &remote.manifest;
    if manifest.pack_id != pack_id {
        return Err(format!("The link for {} now holds a different pack ({})", local.name, manifest.pack_id));
    }
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
    let downloaded = match download_changes(drive, manifest, &plans, &staged, &|| op.cancelled(), progress) {
        Ok(n) => n,
        Err(e) => return Err(vfs::abandon_staging(&staging, e)),
    };
    let mut moves = Moves::default();
    if let Err(e) = assemble(&mut moves, &remote, &plans, &pack_dir, &staged, &flags, active) {
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
    crate::google::api_key().ok_or(NO_API_KEY)?;
    let mods_dir = vfs::get_mods_dir(&app_handle)?;
    let (_, _, link) = linked_pack(&mods_dir, &pack_id)?;
    // without a game path nothing can be linked, so a Paks folder that does not exist is enough
    let paks_dir = vfs::get_paks_dir(&app_handle).unwrap_or_else(|_| mods_dir.join(".no-game-path"));
    let config = crate::config::load_config(app_handle.clone())?;
    let active = config.active_preset.as_deref() == Some(format!("pack:{}", pack_id).as_str());
    let handle = app_handle.clone();
    let summary = tauri::async_runtime::spawn_blocking(move || {
        let drive = public_drive(&link)?;
        let mut emit = |p: Progress| {
            let _ = handle.emit("pack-progress", p);
        };
        update_pack(&drive, &mods_dir, &paks_dir, &pack_id, active, &force, &mut emit)
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
    use crate::drive::fake::FakeDrive;
    use crate::hashing;
    use crate::packs;
    use crate::publish::{RemoteFileEntry, RemoteMod, FORMAT, MANIFEST_NAME};
    use std::collections::BTreeMap;
    use std::fs;

    const SPIKE_ID: &str = "1p0ZI3IoQZYnrQG5rycBmNGC3cIliwF-A";
    const PACK: &str = "Casual-Shitters";
    const NAME: &str = "Casual Shitters";
    const A_PAK: &str = "pakchunk999-Mods_AVP_BetterVestsRemastared_P.pak";
    const B_PAK: &str = "pakchunk9999-Mods_CazanusVisceralBlud_P.pak";
    const W_PAK: &str = "pakchunk9999-Mods_wound_P.pak";

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ron-test-{}-{}", name, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
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

    struct Remote {
        folder: String,
        manifest_id: String,
        manifest: RemoteManifest,
    }

    fn remote_hash(m: &RemoteMod) -> String {
        hashing::mod_hash(m.files.iter().map(|f| (f.path.as_str(), f.sha256.as_str())), &m.addons)
    }

    // Publishes the pack layout and a ready manifest straight into the fake
    fn remote_fixture(fake: &FakeDrive, mods: &[FixtureMod]) -> Remote {
        remote_fixture_as(fake, mods, PACK)
    }

    // Cancel tests use their own pack id, since the cancel flags are shared across test threads
    fn remote_fixture_as(fake: &FakeDrive, mods: &[FixtureMod], pack_id: &str) -> Remote {
        let folder = fake.create_folder(None, pack_id).unwrap();
        let out = mods.iter().map(|m| fixture_mod(fake, &folder, m)).collect();
        let manifest = RemoteManifest {
            format: FORMAT,
            pack_id: pack_id.into(),
            name: NAME.into(),
            version: "1.0".into(),
            status: "ready".into(),
            published_at: 1792364400,
            mods: out,
        };
        let manifest_id = fake.create_bytes(&folder, MANIFEST_NAME, &serde_json::to_vec(&manifest).unwrap()).unwrap();
        Remote { folder, manifest_id, manifest }
    }

    // Uploads one mod's files into the fake pack folder and returns its manifest entry
    fn fixture_mod(fake: &FakeDrive, folder: &str, m: &FixtureMod) -> RemoteMod {
        let mod_folder = fake.create_folder(Some(folder), m.id).unwrap();
        let mut dirs: BTreeMap<String, String> = BTreeMap::new();
        let mut files = Vec::new();
        for (path, body) in &m.files {
            let parent = match path.rsplit_once('/') {
                Some((dir, _)) => dirs
                    .entry(dir.to_string())
                    .or_insert_with(|| fake.create_folder(Some(&mod_folder), dir).unwrap())
                    .clone(),
                None => mod_folder.clone(),
            };
            let name = path.rsplit('/').next().unwrap();
            let file_id = fake.create_bytes(&parent, name, body.as_bytes()).unwrap();
            files.push(RemoteFileEntry {
                path: path.to_string(),
                size: body.len() as u64,
                sha256: hashing::sha256_bytes(body.as_bytes()),
                file_id,
            });
        }
        let mut rm = RemoteMod {
            id: m.id.to_string(),
            name: format!("Mod {}", m.id),
            version: m
                .files
                .iter()
                .find(|(p, _)| *p == "metadata.json")
                .and_then(|(_, body)| crate::vfs::meta_version(serde_json::from_str(body).ok().as_ref())),
            folder_id: mod_folder,
            hash: String::new(),
            addons: m.addons.iter().map(|(n, on)| (n.to_string(), *on)).collect(),
            files,
        };
        rm.hash = remote_hash(&rm);
        rm
    }

    fn set_remote(fake: &FakeDrive, remote: &Remote, manifest: &RemoteManifest) {
        fake.update_bytes(&remote.manifest_id, &serde_json::to_vec(manifest).unwrap()).unwrap();
    }

    fn link(remote: &Remote) -> DriveLink {
        DriveLink { folder_id: remote.folder.clone(), resource_key: None }
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = walkdir::WalkDir::new(dir)
            .into_iter()
            .map(|e| e.unwrap().path().strip_prefix(dir).unwrap().display().to_string())
            .collect();
        out.sort();
        out
    }

    fn add(fake: &FakeDrive, remote: &Remote, mods: &Path, paks: &Path) -> Result<ImportSummary, String> {
        add_from_link(fake, &link(remote), mods, paks, &mut |_| {})
    }

    #[test]
    fn parse_link_cases() {
        let ok = |input: &str| parse_link(input).unwrap();
        let plain = DriveLink { folder_id: SPIKE_ID.into(), resource_key: None };
        assert_eq!(ok(&format!("https://drive.google.com/drive/folders/{}?usp=sharing", SPIKE_ID)), plain);
        assert_eq!(ok(&format!("https://drive.google.com/drive/u/0/folders/{}", SPIKE_ID)), plain);
        assert_eq!(ok(&format!("https://drive.google.com/open?id={}", SPIKE_ID)), plain);
        assert_eq!(
            ok(&format!("https://drive.google.com/drive/folders/{}?usp=sharing&resourcekey=0-abc", SPIKE_ID)),
            DriveLink { folder_id: SPIKE_ID.into(), resource_key: Some("0-abc".into()) }
        );
        assert_eq!(ok(SPIKE_ID), plain);
        assert_eq!(ok(&format!("   https://drive.google.com/drive/folders/{}  \n", SPIKE_ID)), plain);
        for bad in [format!("https://drive.google.com/file/d/{}/view", SPIKE_ID), "hello".into(), "".into()] {
            assert_eq!(parse_link(&bad).unwrap_err(), "That is not a Google Drive folder link", "{bad}");
        }
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
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let mut hostile = remote.manifest.clone();
        hostile.mods[0].files[0].path = "../../evil.pak".into();
        set_remote(&fake, &remote, &hostile);
        let mods = scratch("sub-hostile");
        let paks = mods.join("paks");
        let err = "Invalid file path in pack: ../../evil.pak";
        assert_eq!(inspect_link(&fake, &link(&remote), &mods).unwrap_err(), err);
        assert_eq!(add(&fake, &remote, &mods, &paks).unwrap_err(), err);
        assert_eq!(fake.downloads(), 0);
        assert!(listing(&mods) == vec![String::new()]);

        let mut bad_id = remote.manifest.clone();
        bad_id.mods[0].id = "../3063".into();
        set_remote(&fake, &remote, &bad_id);
        assert!(add(&fake, &remote, &mods, &paks).is_err());
        let mut bad_pack = remote.manifest.clone();
        bad_pack.pack_id = "../x".into();
        set_remote(&fake, &remote, &bad_pack);
        assert!(add(&fake, &remote, &mods, &paks).is_err());
        assert_eq!(fake.downloads(), 0);
        assert!(listing(&mods) == vec![String::new()]);
    }

    #[test]
    fn inspect_link_previews() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let mods = scratch("sub-inspect");
        let preview = inspect_link(&fake, &link(&remote), &mods).unwrap();
        assert_eq!(preview.preview.pack.pack_id, PACK);
        assert_eq!(preview.preview.pack.version.as_deref(), Some("1.0"));
        let added: Vec<&str> = preview.preview.added.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(added, ["Casual-Shitters/3063", "Casual-Shitters/3311"]);
        let total: u64 = remote.manifest.mods.iter().flat_map(|m| &m.files).map(|f| f.size).sum();
        assert_eq!(preview.download_bytes, total);
        assert_eq!(fake.downloads(), 0);
    }

    #[test]
    fn add_from_link_installs() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let mods = scratch("sub-add");
        let paks = scratch("sub-add-paks");
        let mut events: Vec<Progress> = Vec::new();
        let summary = add_from_link(&fake, &link(&remote), &mods, &paks, &mut |p| events.push(p)).unwrap();
        assert_eq!(summary.keys, ["Casual-Shitters/3063", "Casual-Shitters/3311"]);
        assert_eq!(summary.added, 2);
        let last = events.last().unwrap();
        assert_eq!((last.stage.as_str(), last.done, last.total), ("downloading", 5, 5));
        assert_eq!(last.pack_id, PACK);
        let pack_bytes: u64 = remote.manifest.mods.iter().flat_map(|m| &m.files).map(|f| f.size).sum();
        assert_eq!((last.overall_done, last.overall_total), (pack_bytes, pack_bytes));
        let overall: Vec<u64> = events.iter().filter(|p| p.stage == "downloading").map(|p| p.overall_done).collect();
        assert!(overall.windows(2).all(|w| w[0] <= w[1]), "overall progress went backwards: {:?}", overall);

        let pack_dir = mods.join(PACK);
        for m in &remote.manifest.mods {
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
        assert_eq!((source.kind.as_str(), source.folder_id.as_str(), source.resource_key), ("drive", remote.folder.as_str(), None));
        for m in &remote.manifest.mods {
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
        assert_eq!(fake.downloads(), 5);
    }

    #[test]
    fn corrupt_download_changes_nothing() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let mods = scratch("sub-corrupt");
        fs::create_dir_all(mods.join("5543")).unwrap();
        fs::write(mods.join("5543/a.pak"), b"a").unwrap();
        let before = listing(&mods);
        let blood = &remote.manifest.mods[1].files[0];
        fake.corrupt(&blood.file_id);
        let err = add(&fake, &remote, &mods, &mods.join("paks")).unwrap_err();
        assert_eq!(err, format!("A download from {} was corrupted ({}). Nothing was changed.", NAME, B_PAK));
        assert_eq!(listing(&mods), before);
    }

    #[test]
    fn updating_and_future_format_refused() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let mods = scratch("sub-refused");
        let paks = mods.join("paks");
        let mut updating = remote.manifest.clone();
        updating.status = "updating".into();
        set_remote(&fake, &remote, &updating);
        let busy = "Casual Shitters is being updated, try again in a few minutes.";
        assert_eq!(inspect_link(&fake, &link(&remote), &mods).unwrap_err(), busy);
        assert_eq!(add(&fake, &remote, &mods, &paks).unwrap_err(), busy);

        fake.update_bytes(&remote.manifest_id, br#"{"format":2,"layout":"new"}"#).unwrap();
        let newer = "Update RON Mod Manager to install this pack";
        assert_eq!(inspect_link(&fake, &link(&remote), &mods).unwrap_err(), newer);
        assert_eq!(add(&fake, &remote, &mods, &paks).unwrap_err(), newer);

        fake.update_bytes(&remote.manifest_id, b"{not json").unwrap();
        assert_eq!(add(&fake, &remote, &mods, &paks).unwrap_err(), "No modpack found at that link");
        fake.delete(&remote.manifest_id).unwrap();
        assert_eq!(add(&fake, &remote, &mods, &paks).unwrap_err(), "No modpack found at that link");
        assert_eq!(fake.downloads(), 0);
        assert!(listing(&mods) == vec![String::new()]);
    }

    #[test]
    fn check_pack_cases() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let mods = scratch("sub-check");
        add(&fake, &remote, &mods, &mods.join("paks")).unwrap();
        let pack_dir = mods.join(PACK);
        let pack_json = fs::read(pack_dir.join("pack.json")).unwrap();
        let downloads = fake.downloads();

        let current = check_pack(&fake, &pack_dir).unwrap();
        assert_eq!((current.pack_id.as_str(), current.status.as_str()), (PACK, "current"));
        assert_eq!((current.installed_version.as_deref(), current.remote_version.as_deref()), (Some("1.0"), Some("1.0")));
        assert!(current.added.is_empty() && current.changed.is_empty() && current.removed.is_empty());
        assert_eq!(current.download_bytes, 0);

        let mut next = remote.manifest.clone();
        next.version = "1.1".into();
        let blood = &mut next.mods[1];
        let pak = blood.files.iter_mut().find(|f| f.path == B_PAK).unwrap();
        fake.update_bytes(&pak.file_id, b"blood-v2-longer").unwrap();
        pak.size = 15;
        pak.sha256 = hashing::sha256_bytes(b"blood-v2-longer");
        blood.hash = remote_hash(blood);
        let meta_size: u64 = blood.files.iter().filter(|f| hashing::is_meta_file(&f.path)).map(|f| f.size).sum();
        set_remote(&fake, &remote, &next);

        let available = check_pack(&fake, &pack_dir).unwrap();
        assert_eq!(available.status, "available");
        assert_eq!(available.remote_version.as_deref(), Some("1.1"));
        let changed: Vec<&str> = available.changed.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(changed, ["Casual-Shitters/3311"]);
        assert!(available.added.is_empty() && available.removed.is_empty());
        assert_eq!(available.download_bytes, 15 + meta_size);

        next.status = "updating".into();
        set_remote(&fake, &remote, &next);
        let updating = check_pack(&fake, &pack_dir).unwrap();
        assert_eq!(updating.status, "updating");

        assert_eq!(fs::read(pack_dir.join("pack.json")).unwrap(), pack_json);
        assert_eq!(fake.downloads(), downloads);
    }

    #[test]
    fn check_pack_sees_added_and_removed_mods() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let mods = scratch("sub-check-mods");
        add(&fake, &remote, &mods, &mods.join("paks")).unwrap();
        let mut next = remote.manifest.clone();
        next.version = "2.0".into();
        let mut extra = next.mods.remove(0);
        extra.id = "2675".into();
        next.mods.push(extra);
        set_remote(&fake, &remote, &next);
        let check = check_pack(&fake, &mods.join(PACK)).unwrap();
        assert_eq!(check.added.iter().map(|m| m.key.as_str()).collect::<Vec<_>>(), ["Casual-Shitters/2675"]);
        assert_eq!(check.removed.iter().map(|m| m.key.as_str()).collect::<Vec<_>>(), ["Casual-Shitters/3063"]);
        let size: u64 = next.mods[1].files.iter().map(|f| f.size).sum();
        assert_eq!(check.download_bytes, size);
    }

    fn refused_untouched(fake: &FakeDrive, remote: &Remote, manifest: &RemoteManifest, err: &str) {
        set_remote(fake, remote, manifest);
        let mods = scratch("sub-refused-case");
        assert_eq!(add(fake, remote, &mods, &mods.join("paks")).unwrap_err(), err);
        assert!(listing(&mods) == vec![String::new()]);
    }

    #[test]
    fn case_only_duplicate_paths_and_reserved_pack_id_refused() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let mut dup = remote.manifest.clone();
        let mut twin = dup.mods[0].files[0].clone();
        dup.mods[0].files[0].path = "X.pak".into();
        twin.path = "x.pak".into();
        dup.mods[0].files.push(twin);
        refused_untouched(&fake, &remote, &dup, "Invalid file path in pack: x.pak");
        let mut reserved = remote.manifest.clone();
        reserved.pack_id = "Con".into();
        refused_untouched(&fake, &remote, &reserved, "Invalid pack id in pack: Con");
        assert_eq!(fake.downloads(), 0);
    }

    #[test]
    fn declared_size_is_enforced() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let corrupted = format!("A download from {} was corrupted ({}). Nothing was changed.", NAME, B_PAK);
        for size in [3, 100] {
            let mut lying = remote.manifest.clone();
            lying.mods[1].files[0].size = size;
            set_remote(&fake, &remote, &lying);
            let mods = scratch("sub-size");
            fs::create_dir_all(mods.join("5543")).unwrap();
            let before = listing(&mods);
            assert_eq!(add(&fake, &remote, &mods, &mods.join("paks")).unwrap_err(), corrupted, "{size}");
            assert_eq!(listing(&mods), before, "{size}");
        }
    }

    #[test]
    fn own_published_pack_refused() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let mods = scratch("sub-own");
        add(&fake, &remote, &mods, &mods.join("paks")).unwrap();
        let pack_dir = mods.join(PACK);
        let mut man = packs::read_manifest(&pack_dir).unwrap();
        man.source = None;
        man.name = "My Shitters".into();
        man.published = Some(packs::Published { folder_id: remote.folder.clone(), dirty: false });
        packs::write_manifest(&pack_dir, &man).unwrap();
        let before = listing(&mods);
        let downloads = fake.downloads();
        let err = "My Shitters is your own published pack. Fork or delete it before adding it from a link.";
        assert_eq!(inspect_link(&fake, &link(&remote), &mods).unwrap_err(), err);
        assert_eq!(add(&fake, &remote, &mods, &mods.join("paks")).unwrap_err(), err);
        assert_eq!(listing(&mods), before);
        assert_eq!(fake.downloads(), downloads);
    }

    #[test]
    fn case_only_pack_folder_clash_refused() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let mods = scratch("sub-clash");
        let other = mods.join("casual-shitters");
        fs::create_dir_all(&other).unwrap();
        let man = packs::PackManifest { pack_id: "casual-shitters".into(), name: "Other".into(), ..Default::default() };
        packs::write_manifest(&other, &man).unwrap();
        let before = listing(&mods);
        let err = "A modpack named casual-shitters is already installed";
        assert_eq!(add(&fake, &remote, &mods, &mods.join("paks")).unwrap_err(), err);
        assert_eq!(listing(&mods), before);
        assert_eq!(fake.downloads(), 0);
    }

    #[test]
    fn check_pack_refuses_a_different_pack_id() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let mods = scratch("sub-otherid");
        add(&fake, &remote, &mods, &mods.join("paks")).unwrap();
        let mut renamed = remote.manifest.clone();
        renamed.pack_id = "Other-Pack".into();
        set_remote(&fake, &remote, &renamed);
        assert_eq!(
            check_pack(&fake, &mods.join(PACK)).unwrap_err(),
            "The link for Casual Shitters now holds a different pack (Other-Pack)"
        );
    }

    // Version 1.1 changes the blood pak, returns the manifest and the changed file id
    fn bump(fake: &FakeDrive, remote: &Remote) -> (RemoteManifest, String) {
        let mut next = remote.manifest.clone();
        next.version = "1.1".into();
        let blood = &mut next.mods[1];
        let pak = blood.files.iter_mut().find(|f| f.path == B_PAK).unwrap();
        fake.update_bytes(&pak.file_id, b"blood-v2").unwrap();
        pak.sha256 = hashing::sha256_bytes(b"blood-v2");
        let id = pak.file_id.clone();
        blood.hash = remote_hash(blood);
        set_remote(fake, remote, &next);
        (next, id)
    }

    #[test]
    fn add_over_installed_linked_pack_replaces_it() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let (mods, paks) = (scratch("sub-upgrade"), scratch("sub-upgrade-paks"));
        add(&fake, &remote, &mods, &paks).unwrap();
        let (next, _) = bump(&fake, &remote);
        let summary = add(&fake, &remote, &mods, &paks).unwrap();
        assert_eq!(summary.keys, ["Casual-Shitters/3063", "Casual-Shitters/3311"]);
        let pack_dir = mods.join(PACK);
        assert_eq!(fs::read(pack_dir.join("3311").join(B_PAK)).unwrap(), b"blood-v2");
        let man = packs::read_manifest(&pack_dir).unwrap();
        assert_eq!(man.version.as_deref(), Some("1.1"));
        assert_eq!(man.installed_hashes["3311"].hash, next.mods[1].hash);
        assert!(man.source.is_some());
        let top: Vec<String> = fs::read_dir(&mods).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(top, [PACK]);
    }

    #[test]
    fn corrupt_upgrade_keeps_the_old_pack() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let (mods, paks) = (scratch("sub-upgrade-corrupt"), scratch("sub-upgrade-corrupt-paks"));
        add(&fake, &remote, &mods, &paks).unwrap();
        let before = listing(&mods);
        let pack_json = fs::read(mods.join(PACK).join("pack.json")).unwrap();
        let (_, changed) = bump(&fake, &remote);
        fake.corrupt(&changed);
        let err = add(&fake, &remote, &mods, &paks).unwrap_err();
        assert_eq!(err, format!("A download from {} was corrupted ({}). Nothing was changed.", NAME, B_PAK));
        assert_eq!(listing(&mods), before);
        assert_eq!(fs::read(mods.join(PACK).join("pack.json")).unwrap(), pack_json);
        assert_eq!(fs::read(mods.join(PACK).join("3311").join(B_PAK)).unwrap(), b"blood-v1");
    }

    const N_PAK: &str = "pakchunk999-Mods_NewMod_P.pak";

    fn new_mod() -> FixtureMod {
        FixtureMod { id: "3500", files: vec![(N_PAK, "new-v1"), ("metadata.json", r#"{"name":"New Mod","version":"0.1"}"#)], addons: vec![] }
    }

    fn update(fake: &FakeDrive, mods: &Path, paks: &Path, active: bool, force: &[&str]) -> Result<UpdateSummary, String> {
        let force: Vec<String> = force.iter().map(|s| s.to_string()).collect();
        update_pack(fake, mods, paks, PACK, active, &force, &mut |_| {})
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
        let fake = FakeDrive::new();
        let mut mods_spec = base_mods();
        mods_spec[1].files.push(("thumbnail.jpg", "thumb"));
        let remote = remote_fixture(&fake, &mods_spec);
        let (mods, paks) = (scratch("sub-update"), scratch("sub-update-paks"));
        add(&fake, &remote, &mods, &paks).unwrap();
        let pack_dir = mods.join(PACK);
        let kept = [pack_dir.join("3063").join(A_PAK), pack_dir.join("3311/addons").join(W_PAK)];
        #[cfg(unix)]
        let inodes: Vec<u64> = kept.iter().map(|p| inode(p)).collect();
        let meta_3063 = fs::read(pack_dir.join("3063/metadata.json")).unwrap();

        let (mut next, _) = bump(&fake, &remote);
        next.mods.push(fixture_mod(&fake, &remote.folder, &new_mod()));
        set_remote(&fake, &remote, &next);
        let before = fake.downloads();
        let summary = update(&fake, &mods, &paks, false, &[]).unwrap();

        // the blood pak, both meta files of 3311 and the two files of the new mod
        assert_eq!(fake.downloads() - before, 5);
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
        assert_eq!(man.source.unwrap().folder_id, remote.folder);
        assert!(man.published.is_none());
        for m in &next.mods {
            assert_eq!(man.installed_hashes[&m.id].hash, m.hash);
        }
        assert_eq!(hashing::addon_states(&pack_dir.join("3311")), BTreeMap::from([(W_PAK.to_string(), true)]));
        assert!(verify_pack(&pack_dir).unwrap().is_empty());
        assert!(no_staging(&mods));
    }

    // Pack-A is a local pack, active and linked. The linked pack has 3063 on and 3311 off.
    fn two_packs(name: &str) -> (FakeDrive, Remote, PathBuf, PathBuf) {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let (mods, paks) = (scratch(name), scratch(&format!("{}-paks", name)));
        let a = mods.join("Pack-A");
        fs::create_dir_all(a.join("1001")).unwrap();
        fs::write(a.join("1001/a.pak"), b"a").unwrap();
        packs::write_manifest(&a, &packs::PackManifest { pack_id: "Pack-A".into(), name: "A".into(), mod_ids: vec!["1001".into()], ..Default::default() }).unwrap();
        crate::vfs::enable_key(&mods, &paks, "Pack-A/1001").unwrap();
        add(&fake, &remote, &mods, &paks).unwrap();
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3063", PACK)).unwrap();
        let mut next = remote.manifest.clone();
        next.version = "1.1".into();
        next.mods.push(fixture_mod(&fake, &remote.folder, &new_mod()));
        set_remote(&fake, &remote, &next);
        (fake, remote, mods, paks)
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
        let (fake, _remote, mods, paks) = two_packs("sub-inactive");
        let a_links = links(&paks, "Pack-A.1001_");
        assert_eq!(a_links.len(), 1);
        let summary = update(&fake, &mods, &paks, false, &[]).unwrap();
        assert_eq!((summary.added, summary.changed, summary.downloaded), (1, 0, 2));
        check_setup(&mods, &paks, &a_links, false);
    }

    #[test]
    fn update_active_pack_enables_added() {
        let (fake, _remote, mods, paks) = two_packs("sub-active");
        let a_links = links(&paks, "Pack-A.1001_");
        update(&fake, &mods, &paks, true, &[]).unwrap();
        check_setup(&mods, &paks, &a_links, true);
    }

    #[test]
    fn update_relinks_changed_pak_in_enabled_mod() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let (mods, paks) = (scratch("sub-relink"), scratch("sub-relink-paks"));
        add(&fake, &remote, &mods, &paks).unwrap();
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3311", PACK)).unwrap();
        let prefix = format!("{}.3311_", PACK);
        let before = links(&paks, &prefix);
        assert_eq!(before.len(), 2);
        bump(&fake, &remote);
        update(&fake, &mods, &paks, true, &[]).unwrap();

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
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let (mods, paks) = (scratch("sub-update-corrupt"), scratch("sub-update-corrupt-paks"));
        add(&fake, &remote, &mods, &paks).unwrap();
        let pack_dir = mods.join(PACK);
        let before = snapshot(&pack_dir);
        let (mut next, changed) = bump(&fake, &remote);
        next.mods.push(fixture_mod(&fake, &remote.folder, &new_mod()));
        set_remote(&fake, &remote, &next);
        fake.corrupt(&changed);
        let err = update(&fake, &mods, &paks, false, &[]).unwrap_err();
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
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let (mods, paks) = (scratch("sub-update-assemble"), scratch("sub-update-assemble-paks"));
        add(&fake, &remote, &mods, &paks).unwrap();
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3063", PACK)).unwrap();
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3311", PACK)).unwrap();
        let pack_dir = mods.join(PACK);
        let before = snapshot(&pack_dir);
        let linked = links(&paks, &format!("{}.", PACK));
        bump(&fake, &remote);
        // 3063 moves across whole first, then the carried addon of 3311 cannot leave its folder
        let addons = pack_dir.join("3311/addons");
        if !lock_dir(&addons) {
            return;
        }
        let result = update(&fake, &mods, &paks, true, &[]);
        unlock_dir(&addons);
        let err = result.unwrap_err();
        assert!(err.starts_with(&format!("Failed to move {}", addons.join(W_PAK).display())), "{err}");
        assert_eq!(snapshot(&pack_dir), before);
        assert_eq!(links(&paks, &format!("{}.", PACK)), linked);
        assert!(no_staging(&mods));
    }

    #[test]
    fn link_failure_after_swap_still_returns_the_summary() {
        let (fake, _remote, mods, paks) = two_packs("sub-not-enabled");
        // a folder where the new mod's links go cannot be cleared by enable_key
        let blocker = paks.join(format!("{}.3500_blocker", PACK));
        fs::create_dir_all(&blocker).unwrap();
        let summary = update(&fake, &mods, &paks, true, &[]).unwrap();
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
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let (mods, paks) = (scratch("sub-update-swap"), scratch("sub-update-swap-paks"));
        add(&fake, &remote, &mods, &paks).unwrap();
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3311", PACK)).unwrap();
        let pack_dir = mods.join(PACK);
        let before = snapshot(&pack_dir);
        let linked = links(&paks, &format!("{}.", PACK));
        bump(&fake, &remote);
        // the old links cannot be removed, so the swap rolls back
        if !lock_dir(&paks) {
            return;
        }
        let result = update(&fake, &mods, &paks, false, &[]);
        unlock_dir(&paks);
        assert_eq!(result.unwrap_err(), "Close Ready or Not and try again");
        assert_eq!(snapshot(&pack_dir), before);
        assert_eq!(links(&paks, &format!("{}.", PACK)), linked);
        assert!(no_staging(&mods));
    }

    #[test]
    fn verify_finds_tampered_mod_and_redownload_restores_it() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let (mods, paks) = (scratch("sub-verify"), scratch("sub-verify-paks"));
        add(&fake, &remote, &mods, &paks).unwrap();
        let pack_dir = mods.join(PACK);
        assert!(verify_pack(&pack_dir).unwrap().is_empty());
        fs::write(pack_dir.join("3311").join(B_PAK), b"tampered").unwrap();
        assert_eq!(verify_pack(&pack_dir).unwrap(), ["3311"]);

        let before = fake.downloads();
        let summary = update(&fake, &mods, &paks, false, &["3311"]).unwrap();
        assert_eq!(fake.downloads() - before, remote.manifest.mods[1].files.len());
        assert_eq!((summary.changed, summary.added), (1, 0));
        assert!(verify_pack(&pack_dir).unwrap().is_empty());
        assert_eq!(fs::read(pack_dir.join("3311").join(B_PAK)).unwrap(), b"blood-v1");
    }

    #[test]
    fn verify_reports_extra_missing_and_unrecorded() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let mods = scratch("sub-verify-cases");
        add(&fake, &remote, &mods, &mods.join("paks")).unwrap();
        let pack_dir = mods.join(PACK);
        // a meta file edit is not tampering
        crate::vfs::set_enabled_flag(&pack_dir.join("3063"), true);
        assert!(verify_pack(&pack_dir).unwrap().is_empty());
        fs::write(pack_dir.join("3063/extra.pak"), b"x").unwrap();
        fs::remove_file(pack_dir.join("3311/addons").join(W_PAK)).unwrap();
        assert_eq!(verify_pack(&pack_dir).unwrap(), ["3063", "3311"]);

        let fake2 = FakeDrive::new();
        let remote2 = remote_fixture(&fake2, &base_mods());
        let mods2 = scratch("sub-verify-norecord");
        add(&fake2, &remote2, &mods2, &mods2.join("paks")).unwrap();
        let mut man = packs::read_manifest(&mods2.join(PACK)).unwrap();
        man.installed_hashes.remove("3063");
        packs::write_manifest(&mods2.join(PACK), &man).unwrap();
        assert_eq!(verify_pack(&mods2.join(PACK)).unwrap(), ["3063"]);
    }

    #[test]
    fn update_drops_removed_mods_and_unlisted_files() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let (mods, paks) = (scratch("sub-update-drop"), scratch("sub-update-drop-paks"));
        add(&fake, &remote, &mods, &paks).unwrap();
        let pack_dir = mods.join(PACK);
        fs::write(pack_dir.join("3063/extra.pak"), b"x").unwrap();
        fs::write(pack_dir.join("3311/stray.pak"), b"y").unwrap();
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3063", PACK)).unwrap();
        let gone = format!("{}.3063_", PACK);
        assert_eq!(links(&paks, &gone).len(), 2);
        let (mut next, _) = bump(&fake, &remote);
        next.mods.remove(0);
        set_remote(&fake, &remote, &next);
        let summary = update(&fake, &mods, &paks, true, &[]).unwrap();
        assert_eq!(summary.removed_keys, [format!("{}/3063", PACK)]);
        assert!(fs::read_dir(&paks).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(&gone)));
        assert!(!pack_dir.join("3063").exists());
        assert!(!pack_dir.join("3311/stray.pak").exists());
        assert_eq!(packs::read_manifest(&pack_dir).unwrap().mod_ids, ["3311"]);
        assert!(no_staging(&mods));
    }

    #[test]
    fn update_refuses_unknown_and_unlinked_packs() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let mods = scratch("sub-update-refuse");
        let paks = mods.join("paks");
        let noop = |id: &str| update_pack(&fake, &mods, &paks, id, false, &[], &mut |_| {}).unwrap_err();
        assert_eq!(noop("../x"), "No modpack folder for \"../x\"");
        assert_eq!(noop(PACK), format!("No modpack folder for {:?}", PACK));
        add(&fake, &remote, &mods, &paks).unwrap();
        let mut man = packs::read_manifest(&mods.join(PACK)).unwrap();
        man.source = None;
        packs::write_manifest(&mods.join(PACK), &man).unwrap();
        let downloads = fake.downloads();
        assert_eq!(noop(PACK), "Casual Shitters is not linked to a Drive folder");
        assert_eq!(fake.downloads(), downloads);
    }

    #[test]
    fn add_from_link_waits_out_a_block() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let mods = scratch("sub-blocked");
        fake.block_downloads(BLOCKED_WAITS.len());
        let mut events: Vec<Progress> = Vec::new();
        add_from_link(&fake, &link(&remote), &mods, &mods.join("paks"), &mut |p| events.push(p)).unwrap();
        assert_eq!(fake.downloads(), 5);
        assert!(mods.join(PACK).join("3063").join(A_PAK).is_file());
    }

    #[test]
    fn add_from_link_gives_up_when_the_block_outlasts_the_waits() {
        let fake = FakeDrive::new();
        let remote = remote_fixture(&fake, &base_mods());
        let mods = scratch("sub-blocked-long");
        fake.block_downloads(BLOCKED_WAITS.len() + 1);
        let err = add_from_link(&fake, &link(&remote), &mods, &mods.join("paks"), &mut |_| {}).unwrap_err();
        assert_eq!(err, BLOCKED);
        assert!(no_staging(&mods));
    }

    #[test]
    fn cancel_add_from_link_changes_nothing() {
        const ID: &str = "Cancel-Add";
        let fake = FakeDrive::new();
        let remote = remote_fixture_as(&fake, &base_mods(), ID);
        let mods = scratch("sub-cancel-add");
        fs::create_dir_all(mods.join("5543")).unwrap();
        fs::write(mods.join("5543/a.pak"), b"a").unwrap();
        let before = listing(&mods);
        let key = mods.join(ID);
        fake.cancel_after(1, move || {
            request_cancel(&key);
        });
        let err = add(&fake, &remote, &mods, &mods.join("paks")).unwrap_err();
        assert_eq!(err, crate::drive::CANCELLED);
        assert_eq!(fake.downloads(), 1);
        assert_eq!(listing(&mods), before);
        assert!(no_staging(&mods));
        assert!(!request_cancel(&mods.join(ID)));
    }

    #[test]
    fn cancel_update_keeps_old_pack() {
        const ID: &str = "Cancel-Update";
        let fake = FakeDrive::new();
        let remote = remote_fixture_as(&fake, &base_mods(), ID);
        let (mods, paks) = (scratch("sub-cancel-update"), scratch("sub-cancel-update-paks"));
        add(&fake, &remote, &mods, &paks).unwrap();
        crate::vfs::enable_key(&mods, &paks, &format!("{}/3063", ID)).unwrap();
        let pack_dir = mods.join(ID);
        let before = snapshot(&pack_dir);
        let pack_json = fs::read(pack_dir.join("pack.json")).unwrap();
        let links_before = links(&paks, "");
        assert!(!links_before.is_empty());

        let (mut next, _) = bump(&fake, &remote);
        next.mods.push(fixture_mod(&fake, &remote.folder, &new_mod()));
        set_remote(&fake, &remote, &next);
        let downloads = fake.downloads();
        let key = pack_dir.clone();
        fake.cancel_after(1, move || {
            request_cancel(&key);
        });
        let err = update_pack(&fake, &mods, &paks, ID, true, &[], &mut |_| {}).unwrap_err();
        assert_eq!(err, crate::drive::CANCELLED);
        assert_eq!(fake.downloads() - downloads, 1);
        assert_eq!(snapshot(&pack_dir), before);
        assert_eq!(fs::read(pack_dir.join("pack.json")).unwrap(), pack_json);
        assert_eq!(links(&paks, ""), links_before);
        assert!(no_staging(&mods));

        let forced = vec!["3063".to_string()];
        let key = pack_dir.clone();
        fake.cancel_after(0, move || {
            request_cancel(&key);
        });
        let err = update_pack(&fake, &mods, &paks, ID, true, &forced, &mut |_| {}).unwrap_err();
        assert_eq!(err, crate::drive::CANCELLED);
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

        let fake = FakeDrive::new();
        let remote = remote_fixture_as(&fake, &base_mods(), ID);
        let (mods, paks) = (scratch("sub-cancel-flag"), scratch("sub-cancel-flag-paks"));
        let key = mods.join(ID);
        assert!(!request_cancel(&key));
        add(&fake, &remote, &mods, &paks).unwrap();
        assert!(!request_cancel(&key));

        bump(&fake, &remote);
        let hook_key = key.clone();
        fake.cancel_after(0, move || {
            request_cancel(&hook_key);
        });
        let err = update_pack(&fake, &mods, &paks, ID, false, &[], &mut |_| {}).unwrap_err();
        assert_eq!(err, crate::drive::CANCELLED);
        assert!(!request_cancel(&key));

        let summary = update_pack(&fake, &mods, &paks, ID, false, &[], &mut |_| {}).unwrap();
        assert_eq!(summary.pack.version.as_deref(), Some("1.1"));
        assert!(no_staging(&mods));
    }

    #[test]
    fn publish_and_download_refuse_each_other() {
        const ID: &str = "Publish-Clash";
        let busy = format!("{} is busy, try again when it finishes", NAME);
        let fake = FakeDrive::new();
        let remote = remote_fixture_as(&fake, &base_mods(), ID);

        // A publish started while an update downloads
        let (mods, paks) = (scratch("sub-clash"), scratch("sub-clash-paks"));
        add(&fake, &remote, &mods, &paks).unwrap();
        bump(&fake, &remote);
        let refused = std::rc::Rc::new(std::cell::RefCell::new(None));
        let (seen, pack_dir) = (refused.clone(), mods.join(ID));
        fake.cancel_after(0, move || {
            let other = FakeDrive::new();
            *seen.borrow_mut() = crate::publish::publish_pack(&other, &pack_dir, "9.0.0", 0, &mut |_| {}).err();
        });
        update_pack(&fake, &mods, &paks, ID, false, &[], &mut |_| {}).unwrap();
        assert_eq!(refused.borrow().as_deref(), Some(busy.as_str()));

        // A download started while a publish runs
        let (mods, paks) = (scratch("sub-clash-pub"), scratch("sub-clash-pub-paks"));
        let pack_dir = mods.join(ID);
        fs::create_dir_all(pack_dir.join("3063")).unwrap();
        fs::write(pack_dir.join("3063/a.pak"), "a").unwrap();
        let local = PackManifest { pack_id: ID.into(), name: NAME.into(), mod_ids: vec!["3063".into()], ..Default::default() };
        packs::write_manifest(&pack_dir, &local).unwrap();
        let before = listing(&mods);
        let mut refused = None;
        let downloads = fake.downloads();
        crate::publish::publish_pack(&FakeDrive::new(), &pack_dir, "1.0.0", 0, &mut |_| {
            if refused.is_none() {
                refused = Some(add(&fake, &remote, &mods, &paks).unwrap_err());
            }
        })
        .unwrap();
        assert_eq!(refused, Some(busy));
        assert_eq!(fake.downloads(), downloads);
        assert!(no_staging(&mods));
        assert_eq!(listing(&mods).len(), before.len());
    }

    #[test]
    fn second_op_on_a_pack_is_refused() {
        const ID: &str = "Busy-Pack";
        let fake = FakeDrive::new();
        let remote = remote_fixture_as(&fake, &base_mods(), ID);
        let (mods, paks) = (scratch("sub-busy"), scratch("sub-busy-paks"));
        add(&fake, &remote, &mods, &paks).unwrap();
        bump(&fake, &remote);
        let before = listing(&mods);
        let busy = format!("{} is busy, try again when it finishes", NAME);
        {
            let held = OpGuard::begin(&mods.join(ID), OpKind::Download).unwrap();
            assert!(OpGuard::begin(&mods.join(ID), OpKind::Download).is_none());
            let downloads = fake.downloads();
            assert_eq!(add(&fake, &remote, &mods, &paks).unwrap_err(), busy);
            assert_eq!(update_pack(&fake, &mods, &paks, ID, false, &[], &mut |_| {}).unwrap_err(), busy);
            let forced = vec!["3063".to_string()];
            assert_eq!(update_pack(&fake, &mods, &paks, ID, false, &forced, &mut |_| {}).unwrap_err(), busy);
            assert_eq!(fake.downloads(), downloads);
            assert_eq!(listing(&mods), before);
            assert!(!held.cancelled());
        }
        assert!(OpGuard::begin(&mods.join(ID), OpKind::Download).is_some());
        let summary = update_pack(&fake, &mods, &paks, ID, false, &[], &mut |_| {}).unwrap();
        assert_eq!(summary.pack.version.as_deref(), Some("1.1"));
    }
}
