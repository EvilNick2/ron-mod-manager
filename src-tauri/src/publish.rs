#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use tauri::Emitter;

use crate::drive::{DriveApi, HttpDrive, Progress, Quota, RemoteFile};
use crate::hashing::{self, FileEntry};
use crate::packs::{self, PackRef, Published};

pub const ROOT_FOLDER: &str = "RoNMM";
pub const MANIFEST_NAME: &str = "manifest.json";
pub const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RemoteManifest {
    pub format: u32,
    pub pack_id: String,
    pub name: String,
    pub version: String,
    pub status: String,
    pub published_at: u64,
    pub mods: Vec<RemoteMod>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RemoteMod {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub folder_id: String,
    pub hash: String,
    pub addons: BTreeMap<String, bool>,
    pub files: Vec<RemoteFileEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RemoteFileEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub file_id: String,
}

#[derive(Clone, Debug)]
pub struct LocalMod {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub dir: PathBuf,
    pub files: Vec<FileEntry>,
    pub addons: BTreeMap<String, bool>,
    pub hash: String,
}

#[derive(Clone, Debug, Default)]
pub struct RemoteModFolder {
    pub folder_id: String,
    pub files: BTreeMap<String, RemoteFile>,
    pub dirs: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default)]
pub struct RemoteState {
    pub manifest_file_id: Option<String>,
    pub manifest: Option<RemoteManifest>,
    pub mods: BTreeMap<String, RemoteModFolder>,
    pub strays: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlannedUpload {
    pub mod_id: String,
    pub path: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PublishPlan {
    pub uploads: Vec<PlannedUpload>,
    pub deletes: Vec<String>,
    pub upload_bytes: u64,
    pub delete_bytes: u64,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind")]
pub enum PublishOutcome {
    Published { folder_id: String, version: String, uploaded: usize, deleted: usize },
    NothingToPublish { folder_id: String },
}

const PROGRESS_EVERY: Duration = Duration::from_millis(250);

struct Throttle {
    last: Option<Instant>,
}

impl Throttle {
    fn new() -> Throttle {
        Throttle { last: None }
    }

    fn ready(&mut self) -> bool {
        let now = Instant::now();
        if self.last.is_some_and(|t| now.duration_since(t) < PROGRESS_EVERY) {
            return false;
        }
        self.last = Some(now);
        true
    }
}

fn progress_event(pack_id: &str, stage: &str, done: usize, total: usize, file: Option<&str>, bytes: (u64, u64)) -> Progress {
    Progress {
        pack_id: pack_id.to_string(),
        stage: stage.to_string(),
        done,
        total,
        file: file.map(str::to_string),
        bytes_done: bytes.0,
        bytes_total: bytes.1,
    }
}

pub fn hash_local_pack(pack_dir: &Path, pack_id: &str, progress: &mut dyn FnMut(Progress)) -> Result<Vec<LocalMod>, String> {
    let manifest = packs::read_manifest(pack_dir).ok_or_else(|| format!("Could not read pack.json for {}", pack_id))?;
    let total = manifest.mod_ids.len();
    let mut mods = Vec::with_capacity(total);
    for (done, id) in manifest.mod_ids.iter().enumerate() {
        let dir = pack_dir.join(id);
        if !dir.is_dir() {
            return Err(format!("Mod {} is missing from {}", id, manifest.name));
        }
        let files = hashing::hash_mod_files(&dir, &mut |path| {
            progress(progress_event(pack_id, "hashing", done, total, Some(path), (0, 0)));
        })?;
        let addons = hashing::addon_states(&dir);
        let hash = hashing::mod_hash(files.iter().map(|f| (f.path.as_str(), f.sha256.as_str())), &addons);
        let meta = crate::vfs::read_meta(&dir);
        let name = meta.as_ref().and_then(|m| m.get("name")).and_then(|n| n.as_str()).unwrap_or(id).to_string();
        let version = crate::vfs::meta_version(meta.as_ref());
        mods.push(LocalMod { id: id.clone(), name, version, dir, files, addons, hash });
    }
    progress(progress_event(pack_id, "hashing", total, total, None, (0, 0)));
    Ok(mods)
}

fn is_mod_folder_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
}

fn read_mod_folder(
    drive: &dyn DriveApi,
    folder_id: &str,
    prefix: &str,
    out: &mut RemoteModFolder,
    strays: &mut Vec<String>,
) -> Result<(), String> {
    for child in drive.list(folder_id)? {
        let path = format!("{}{}", prefix, child.name);
        if child.is_folder() {
            out.dirs.insert(path.clone(), child.id.clone());
            read_mod_folder(drive, &child.id, &format!("{}/", path), out, strays)?;
        } else if out.files.contains_key(&path) {
            strays.push(child.id);
        } else {
            out.files.insert(path, child);
        }
    }
    Ok(())
}

fn parse_manifest(bytes: &[u8]) -> Result<Option<RemoteManifest>, String> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else { return Ok(None) };
    if value.get("format").and_then(|f| f.as_u64()).is_some_and(|f| f > u64::from(FORMAT)) {
        return Err("Update RON Mod Manager to publish this pack".to_string());
    }
    Ok(serde_json::from_value(value).ok())
}

pub fn read_remote(drive: &dyn DriveApi, pack_folder: &str) -> Result<RemoteState, String> {
    let mut state = RemoteState::default();
    for child in drive.list(pack_folder)? {
        if !child.is_folder() && child.name == MANIFEST_NAME && state.manifest_file_id.is_none() {
            state.manifest = parse_manifest(&drive.read(&child.id)?)?;
            state.manifest_file_id = Some(child.id);
        } else if child.is_folder() && is_mod_folder_name(&child.name) && !state.mods.contains_key(&child.name) {
            let mut folder = RemoteModFolder { folder_id: child.id.clone(), ..Default::default() };
            read_mod_folder(drive, &child.id, "", &mut folder, &mut state.strays)?;
            state.mods.insert(child.name, folder);
        } else {
            state.strays.push(child.id);
        }
    }
    Ok(state)
}

// The old manifest's entry for a Drive file, only when it still names that same file
fn recorded<'a>(remote: &'a RemoteState, mod_id: &str, file: &RemoteFile) -> Option<&'a RemoteFileEntry> {
    remote
        .manifest
        .iter()
        .flat_map(|m| m.mods.iter())
        .filter(|m| m.id == mod_id)
        .flat_map(|m| m.files.iter())
        .find(|f| f.file_id == file.id)
}

fn remote_sha<'a>(remote: &'a RemoteState, mod_id: &str, file: &'a RemoteFile) -> Option<&'a str> {
    file.sha256.as_deref().or_else(|| recorded(remote, mod_id, file).map(|r| r.sha256.as_str()))
}

fn same_content(remote: &RemoteState, mod_id: &str, file: &RemoteFile, local: &FileEntry) -> bool {
    match remote_sha(remote, mod_id, file) {
        Some(sha) => sha == local.sha256,
        None => file.size == Some(local.size),
    }
}

impl PublishPlan {
    fn upload(&mut self, mod_id: &str, f: &FileEntry) {
        self.upload_bytes += f.size;
        self.uploads.push(PlannedUpload { mod_id: mod_id.to_string(), path: f.path.clone(), size: f.size });
    }

    fn delete(&mut self, f: &RemoteFile) {
        self.delete_bytes += f.size.unwrap_or(0);
        self.deletes.push(f.id.clone());
    }
}

fn parent_dirs(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(move |(i, _)| &path[..i])
}

pub fn plan_publish(local: &[LocalMod], remote: &RemoteState) -> PublishPlan {
    let mut plan = PublishPlan::default();
    let listed: BTreeMap<&str, &RemoteMod> =
        remote.manifest.iter().flat_map(|m| m.mods.iter()).map(|m| (m.id.as_str(), m)).collect();
    // A manifest left in updating describes the old mods, so identical files on Drive count as uploaded
    let trusted = remote.manifest.as_ref().is_some_and(|m| m.status == "ready");

    for m in local {
        let Some(folder) = remote.mods.get(&m.id) else {
            plan.added.push(m.id.clone());
            for f in &m.files {
                plan.upload(&m.id, f);
            }
            continue;
        };
        let hash_changed = listed.get(m.id.as_str()).is_none_or(|r| r.hash != m.hash);
        if hash_changed {
            plan.changed.push(m.id.clone());
        }
        for f in &m.files {
            let old = folder.files.get(&f.path);
            let upload = match old {
                None => true,
                Some(_) if hashing::is_meta_file(&f.path) && trusted => hash_changed,
                Some(o) => !same_content(remote, &m.id, o, f),
            };
            if upload {
                if let Some(o) = old {
                    plan.delete(o);
                }
                plan.upload(&m.id, f);
            }
        }
        let local_paths: BTreeSet<&str> = m.files.iter().map(|f| f.path.as_str()).collect();
        for (path, o) in &folder.files {
            if !local_paths.contains(path.as_str()) {
                plan.delete(o);
            }
        }
        let needed: BTreeSet<&str> = m.files.iter().flat_map(|f| parent_dirs(&f.path)).collect();
        for (dir, id) in &folder.dirs {
            let parent_kept = dir.rsplit_once('/').is_none_or(|(p, _)| needed.contains(p));
            if !needed.contains(dir.as_str()) && parent_kept {
                plan.deletes.push(id.clone());
            }
        }
    }

    let local_ids: BTreeSet<&str> = local.iter().map(|m| m.id.as_str()).collect();
    for (id, folder) in &remote.mods {
        if !local_ids.contains(id.as_str()) {
            plan.removed.push(id.clone());
            plan.deletes.push(folder.folder_id.clone());
            plan.delete_bytes += folder.files.values().map(|f| f.size.unwrap_or(0)).sum::<u64>();
        }
    }
    plan
}

pub fn space_needed(plan: &PublishPlan) -> u64 {
    plan.upload_bytes.saturating_sub(plan.delete_bytes)
}

pub fn check_space(quota: &Quota, needed: u64) -> Result<(), String> {
    let Some(limit) = quota.limit else { return Ok(()) };
    let free = limit.saturating_sub(quota.usage);
    if needed > free {
        return Err(format!(
            "Publishing needs {} more, your Drive has {} free.",
            format_size(needed),
            format_size(free)
        ));
    }
    Ok(())
}

pub fn format_size(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        format!("{:.1} GB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.0} MB", bytes as f64 / MIB as f64)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

fn write_remote_manifest(
    drive: &dyn DriveApi,
    pack_folder: &str,
    file_id: Option<&str>,
    manifest: &RemoteManifest,
) -> Result<String, String> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|e| e.to_string())?;
    match file_id {
        Some(id) => drive.update_bytes(id, &bytes).map(|_| id.to_string()),
        None => drive.create_bytes(pack_folder, MANIFEST_NAME, &bytes),
    }
}

fn ensure_dir(
    drive: &dyn DriveApi,
    mod_folder: &str,
    dirs: &mut BTreeMap<String, String>,
    dir: &str,
) -> Result<String, String> {
    let mut parent = mod_folder.to_string();
    for (i, part) in dir.split('/').enumerate() {
        let path = dir.split('/').take(i + 1).collect::<Vec<_>>().join("/");
        parent = match dirs.get(&path) {
            Some(id) => id.clone(),
            None => {
                let id = drive.create_folder(Some(&parent), part)?;
                dirs.insert(path, id.clone());
                id
            }
        };
    }
    Ok(parent)
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
fn mark_published(pack_dir: &Path, folder_id: &str, version: &str) -> Result<(), String> {
    let mut manifest = packs::read_manifest(pack_dir).ok_or("Could not read pack.json")?;
    match manifest.published.as_mut() {
        Some(p) => p.folder_id = folder_id.to_string(),
        None => manifest.published = Some(Published { folder_id: folder_id.to_string(), dirty: false }),
    }
    manifest.version = Some(version.to_string());
    packs::write_manifest(pack_dir, &manifest)
}

fn sweep(drive: &dyn DriveApi, strays: &[String]) {
    for id in strays {
        if let Err(e) = drive.delete(id) {
            eprintln!("[PUBLISH] Could not delete leftover Drive file {}: {}", id, e);
        }
    }
}

fn find_pack_folder(drive: &dyn DriveApi, pack_id: &str) -> Result<Option<String>, String> {
    match drive.find_folder(None, ROOT_FOLDER)? {
        Some(root) => drive.find_folder(Some(&root), pack_id),
        None => Ok(None),
    }
}

fn is_empty(state: &RemoteState) -> bool {
    state.manifest_file_id.is_none() && state.mods.is_empty() && state.strays.is_empty()
}

// The pack folder and its contents, or None when no usable folder exists yet. The flag says
// whether the folder came from published.folder_id.
fn locate_remote(
    drive: &dyn DriveApi,
    published: Option<&str>,
    pack_id: &str,
) -> Result<(Option<String>, RemoteState, bool), String> {
    if let Some(id) = published {
        match read_remote(drive, id) {
            Ok(state) if !is_empty(&state) => return Ok((Some(id.to_string()), state, true)),
            Ok(state) => {
                if find_pack_folder(drive, pack_id)?.as_deref() == Some(id) {
                    return Ok((Some(id.to_string()), state, true));
                }
            }
            Err(e) if e.starts_with("Google Drive error 404") => {}
            Err(e) => return Err(e),
        }
    }
    let by_name = find_pack_folder(drive, pack_id)?;
    let state = match &by_name {
        Some(id) => read_remote(drive, id)?,
        None => RemoteState::default(),
    };
    Ok((by_name, state, false))
}

pub fn publish_pack(
    drive: &dyn DriveApi,
    pack_dir: &Path,
    version: &str,
    now: u64,
    progress: &mut dyn FnMut(Progress),
) -> Result<PublishOutcome, String> {
    let manifest = packs::read_manifest(pack_dir).ok_or_else(|| format!("Could not read {}", pack_dir.join("pack.json").display()))?;
    let mods_dir = pack_dir.parent().ok_or("The pack folder has no parent")?;
    packs::ensure_pack_editable(mods_dir, &manifest.pack_id)?;
    let was_dirty = manifest.published.as_ref().map(|p| p.dirty);
    if was_dirty.is_some() {
        set_dirty(pack_dir, false)?;
    }
    let result = run_publish(drive, pack_dir, &manifest, version, now, progress);
    if let (Err(_), Some(dirty)) = (&result, was_dirty) {
        if let Err(e) = set_dirty(pack_dir, dirty) {
            eprintln!("[PUBLISH] Could not mark {} as changed: {}", manifest.pack_id, e);
        }
    }
    result
}

fn run_publish(
    drive: &dyn DriveApi,
    pack_dir: &Path,
    manifest: &packs::PackManifest,
    version: &str,
    now: u64,
    progress: &mut dyn FnMut(Progress),
) -> Result<PublishOutcome, String> {
    let pack_id = manifest.pack_id.clone();
    let local = hash_local_pack(pack_dir, &pack_id, progress)?;

    // Folders are only looked up here, creating them waits until the space check passes
    let published = manifest.published.as_ref().map(|p| p.folder_id.as_str());
    let (known_folder, remote, from_record) = locate_remote(drive, published, &pack_id)?;
    let plan = plan_publish(&local, &remote);
    let share = !from_record;

    if let (Some(old), Some(folder)) = (&remote.manifest, &known_folder) {
        let moves = !plan.uploads.is_empty() || !plan.deletes.is_empty();
        if old.version == version {
            if old.status == "ready" && !moves {
                if share {
                    drive.share_anyone(folder)?;
                }
                mark_published(pack_dir, folder, version)?;
                sweep(drive, &remote.strays);
                return Ok(PublishOutcome::NothingToPublish { folder_id: folder.clone() });
            }
            // An updating manifest with no mods is a failed first publish, any version finishes it
            if old.status == "ready" || !old.mods.is_empty() {
                return Err(format!("Version {} is already published. Enter a new version.", version));
            }
        }
    }

    check_space(&drive.quota()?, space_needed(&plan))?;

    let pack_folder = match known_folder {
        Some(id) => id,
        None => {
            let root = match drive.find_folder(None, ROOT_FOLDER)? {
                Some(id) => id,
                None => drive.create_folder(None, ROOT_FOLDER)?,
            };
            drive.create_folder(Some(&root), &pack_id)?
        }
    };
    if share {
        drive.share_anyone(&pack_folder)?;
    }

    let mut remote_manifest = remote.manifest.clone().unwrap_or_else(|| RemoteManifest {
        format: FORMAT,
        pack_id: pack_id.clone(),
        name: manifest.name.clone(),
        version: version.to_string(),
        status: String::new(),
        published_at: now,
        mods: Vec::new(),
    });
    remote_manifest.format = FORMAT;
    remote_manifest.status = "updating".to_string();
    let manifest_id = write_remote_manifest(drive, &pack_folder, remote.manifest_file_id.as_deref(), &remote_manifest)?;

    let total = plan.deletes.len();
    for (done, id) in plan.deletes.iter().enumerate() {
        drive.delete(id)?;
        progress(progress_event(&pack_id, "deleting", done + 1, total, None, (0, 0)));
    }

    let total = plan.uploads.len();
    let mut done = 0;
    let mut new_ids: BTreeMap<(&str, &str), String> = BTreeMap::new();
    let mut folder_ids: BTreeMap<&str, String> = BTreeMap::new();
    for m in &local {
        let existing = remote.mods.get(&m.id);
        let folder_id = match existing {
            Some(f) => f.folder_id.clone(),
            None => drive.create_folder(Some(&pack_folder), &m.id)?,
        };
        let mut dirs = existing.map(|f| f.dirs.clone()).unwrap_or_default();
        for up in plan.uploads.iter().filter(|u| u.mod_id == m.id) {
            let (parent, name) = match up.path.rsplit_once('/') {
                Some((dir, name)) => (ensure_dir(drive, &folder_id, &mut dirs, dir)?, name),
                None => (folder_id.clone(), up.path.as_str()),
            };
            let mut sent = 0u64;
            let mut throttle = Throttle::new();
            let id = drive.upload(&parent, name, &m.dir.join(&up.path), &mut |n| {
                sent += n;
                if throttle.ready() {
                    progress(progress_event(&pack_id, "uploading", done, total, Some(&up.path), (sent, up.size)));
                }
            })?;
            done += 1;
            progress(progress_event(&pack_id, "uploading", done, total, Some(&up.path), (up.size, up.size)));
            new_ids.insert((m.id.as_str(), up.path.as_str()), id);
        }
        folder_ids.insert(m.id.as_str(), folder_id);
    }

    let mut mods = Vec::with_capacity(local.len());
    for m in &local {
        let mut files = Vec::with_capacity(m.files.len());
        for f in &m.files {
            // Uploaded files carry the local hash. Kept files carry what Drive holds, which can differ for meta files.
            let entry = match new_ids.get(&(m.id.as_str(), f.path.as_str())) {
                Some(id) => RemoteFileEntry { path: f.path.clone(), size: f.size, sha256: f.sha256.clone(), file_id: id.clone() },
                None => {
                    let r = remote
                        .mods
                        .get(&m.id)
                        .and_then(|r| r.files.get(&f.path))
                        .ok_or_else(|| format!("No Drive file for {}/{}", m.id, f.path))?;
                    let old = recorded(&remote, &m.id, r);
                    RemoteFileEntry {
                        path: f.path.clone(),
                        size: r.size.or(old.map(|o| o.size)).unwrap_or(f.size),
                        sha256: remote_sha(&remote, &m.id, r).unwrap_or(&f.sha256).to_string(),
                        file_id: r.id.clone(),
                    }
                }
            };
            files.push(entry);
        }
        mods.push(RemoteMod {
            id: m.id.clone(),
            name: m.name.clone(),
            version: m.version.clone(),
            folder_id: folder_ids[m.id.as_str()].clone(),
            hash: m.hash.clone(),
            addons: m.addons.clone(),
            files,
        });
    }
    let ready = RemoteManifest {
        format: FORMAT,
        pack_id: pack_id.clone(),
        name: manifest.name.clone(),
        version: version.to_string(),
        status: "ready".to_string(),
        published_at: now,
        mods,
    };
    write_remote_manifest(drive, &pack_folder, Some(&manifest_id), &ready)?;

    mark_published(pack_dir, &pack_folder, version)?;
    sweep(drive, &remote.strays);
    Ok(PublishOutcome::Published {
        folder_id: pack_folder,
        version: version.to_string(),
        uploaded: plan.uploads.len(),
        deleted: plan.deletes.len(),
    })
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
            if !manifest.published.as_ref().is_some_and(|p| p.dirty) {
                return None;
            }
            Some(PackRef { pack_id: e.file_name().to_string_lossy().to_string(), name: manifest.name, version: manifest.version })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
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
pub async fn publish_pack_cmd(app_handle: tauri::AppHandle, pack_id: String, version: String) -> Result<PublishOutcome, String> {
    let version = version.trim().to_string();
    if version.is_empty() {
        return Err("Enter a version".to_string());
    }
    let mods_dir = crate::vfs::get_mods_dir(&app_handle)?;
    let pack_dir = mods_dir.join(&pack_id);
    if packs::parse_key(&format!("{}/1", pack_id)).is_err() || packs::read_manifest(&pack_dir).is_none() {
        return Err(format!("No modpack folder for {:?}", pack_id));
    }
    tauri::async_runtime::spawn_blocking(move || {
        // Fail early with a clear sign-in error before any Drive call
        crate::google::access_token()?;
        let drive = HttpDrive::signed_in(Box::new(crate::google::access_token));
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let mut emit = |p: Progress| {
            let _ = app_handle.emit("pack-progress", p);
        };
        publish_pack(&drive, &pack_dir, &version, now, &mut emit)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::fake::FakeDrive;
    use crate::packs::{PackManifest, PackSource};
    use std::fs;

    const GIB: u64 = 1024 * 1024 * 1024;
    const PACK: &str = "Casual-Shitters";
    const A_PAK: &str = "pakchunk999-Mods_AVP_BetterVestsRemastared_P.pak";
    const B_PAK: &str = "pakchunk9999-Mods_CazanusVisceralBlud_P.pak";
    const W_PAK: &str = "pakchunk9999-Mods_wound_P.pak";

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

    // A remote that mirrors the local mods exactly, with readable ids
    fn mirror(local: &[LocalMod], status: &str, listed: bool) -> RemoteState {
        let mut state = RemoteState { manifest_file_id: Some("manifest".into()), ..Default::default() };
        let mut mods = Vec::new();
        for m in local {
            let mut folder = RemoteModFolder { folder_id: format!("folder:{}", m.id), ..Default::default() };
            for f in &m.files {
                if let Some((dir, _)) = f.path.rsplit_once('/') {
                    folder.dirs.insert(dir.to_string(), format!("dir:{}/{}", m.id, dir));
                }
                folder.files.insert(
                    f.path.clone(),
                    RemoteFile {
                        id: format!("id:{}/{}", m.id, f.path),
                        name: f.path.rsplit('/').next().unwrap().to_string(),
                        mime_type: "application/octet-stream".into(),
                        size: Some(f.size),
                        sha256: Some(f.sha256.clone()),
                        resource_key: None,
                    },
                );
            }
            mods.push(RemoteMod {
                id: m.id.clone(),
                name: m.name.clone(),
                version: m.version.clone(),
                folder_id: folder.folder_id.clone(),
                hash: m.hash.clone(),
                addons: m.addons.clone(),
                files: Vec::new(),
            });
            state.mods.insert(m.id.clone(), folder);
        }
        state.manifest = Some(RemoteManifest {
            format: FORMAT,
            pack_id: PACK.into(),
            name: "Casual Shitters".into(),
            version: "1.0".into(),
            status: status.into(),
            published_at: 1,
            mods: if listed { mods } else { Vec::new() },
        });
        state
    }

    fn upload_paths(plan: &PublishPlan) -> Vec<String> {
        plan.uploads.iter().map(|u| format!("{}/{}", u.mod_id, u.path)).collect()
    }

    #[test]
    fn plan_cases() {
        struct Case {
            name: &'static str,
            change: fn(&Path),
            status: &'static str,
            listed: bool,
            uploads: Vec<&'static str>,
            deletes: Vec<&'static str>,
            added: Vec<&'static str>,
            removed: Vec<&'static str>,
            changed: Vec<&'static str>,
        }
        let cases = vec![
            Case {
                name: "unchanged",
                change: |_| {},
                status: "ready",
                listed: true,
                uploads: vec![],
                deletes: vec![],
                added: vec![],
                removed: vec![],
                changed: vec![],
            },
            Case {
                name: "added",
                change: |p| {
                    put_mod(p, "2675", &[("Minervha_Home_CNS.pak", "home".into()), ("metadata.json", meta("Home", None))]);
                    set_mod_ids(p, &["3063", "3311", "2675"]);
                },
                status: "ready",
                listed: true,
                uploads: vec!["2675/Minervha_Home_CNS.pak", "2675/metadata.json"],
                deletes: vec![],
                added: vec!["2675"],
                removed: vec![],
                changed: vec![],
            },
            Case {
                name: "removed",
                change: |p| set_mod_ids(p, &["3311"]),
                status: "ready",
                listed: true,
                uploads: vec![],
                deletes: vec!["folder:3063"],
                added: vec![],
                removed: vec!["3063"],
                changed: vec![],
            },
            Case {
                name: "changed file",
                change: |p| fs::write(p.join("3063").join(A_PAK), "vests-v2").unwrap(),
                status: "ready",
                listed: true,
                uploads: vec!["3063/metadata.json", "3063/pakchunk999-Mods_AVP_BetterVestsRemastared_P.pak"],
                deletes: vec!["id:3063/metadata.json", "id:3063/pakchunk999-Mods_AVP_BetterVestsRemastared_P.pak"],
                added: vec![],
                removed: vec![],
                changed: vec!["3063"],
            },
            Case {
                name: "removed file",
                change: |p| fs::remove_dir_all(p.join("3311/addons")).unwrap(),
                status: "ready",
                listed: true,
                uploads: vec!["3311/metadata.json"],
                deletes: vec!["id:3311/metadata.json", "id:3311/addons/pakchunk9999-Mods_wound_P.pak", "dir:3311/addons"],
                added: vec![],
                removed: vec![],
                changed: vec!["3311"],
            },
            Case {
                name: "addon toggle",
                change: |p| fs::write(p.join("3311/metadata.json"), meta("Visceral Blood", Some(true))).unwrap(),
                status: "ready",
                listed: true,
                uploads: vec!["3311/metadata.json"],
                deletes: vec!["id:3311/metadata.json"],
                added: vec![],
                removed: vec![],
                changed: vec!["3311"],
            },
            Case {
                name: "recovery keeps identical files",
                change: |_| {},
                status: "updating",
                listed: false,
                uploads: vec![],
                deletes: vec![],
                added: vec![],
                removed: vec![],
                changed: vec!["3063", "3311"],
            },
        ];
        for case in cases {
            let pack_dir = pack_fixture("plan", &base_mods());
            let remote = mirror(&local(&pack_dir), case.status, case.listed);
            (case.change)(&pack_dir);
            let plan = plan_publish(&local(&pack_dir), &remote);
            assert_eq!(upload_paths(&plan), case.uploads, "{} uploads", case.name);
            assert_eq!(plan.deletes, case.deletes, "{} deletes", case.name);
            assert_eq!(plan.added, case.added, "{} added", case.name);
            assert_eq!(plan.removed, case.removed, "{} removed", case.name);
            assert_eq!(plan.changed, case.changed, "{} changed", case.name);
            let up: u64 = plan.uploads.iter().map(|u| u.size).sum();
            assert_eq!(plan.upload_bytes, up, "{} upload_bytes", case.name);
        }
    }

    #[test]
    fn plan_counts_removed_bytes_and_treats_missing_sha_by_size() {
        let pack_dir = pack_fixture("planbytes", &base_mods());
        let mods = local(&pack_dir);
        let mut remote = mirror(&mods, "ready", true);
        for f in remote.mods.get_mut("3311").unwrap().files.values_mut() {
            f.sha256 = None;
        }
        set_mod_ids(&pack_dir, &["3311"]);
        let plan = plan_publish(&local(&pack_dir), &remote);
        let removed: u64 = mods[0].files.iter().map(|f| f.size).sum();
        assert_eq!(plan.delete_bytes, removed);
        assert!(plan.uploads.is_empty());
    }

    #[test]
    fn space_cases() {
        let plan = |up: u64, down: u64| PublishPlan { upload_bytes: up, delete_bytes: down, ..Default::default() };
        assert_eq!(space_needed(&plan(5, 3)), 2);
        assert_eq!(space_needed(&plan(3, 5)), 0);
        let unlimited = Quota { limit: None, usage: 50 * GIB };
        assert!(check_space(&unlimited, space_needed(&plan(10 * GIB, 0))).is_ok());
        let full = Quota { limit: Some(15 * GIB), usage: 15 * GIB };
        assert!(check_space(&full, space_needed(&plan(GIB, 2 * GIB))).is_ok());
        let over = Quota { limit: Some(15 * GIB), usage: 16 * GIB };
        assert!(check_space(&over, 0).is_ok());
        let one_free = Quota { limit: Some(15 * GIB), usage: 14 * GIB };
        assert_eq!(
            check_space(&one_free, space_needed(&plan(3 * GIB, GIB))).unwrap_err(),
            "Publishing needs 2.0 GB more, your Drive has 1.0 GB free."
        );
    }

    #[test]
    fn format_size_cases() {
        assert_eq!(format_size(0), "0 KB");
        assert_eq!(format_size(1), "1 KB");
        assert_eq!(format_size(1025), "2 KB");
        assert_eq!(format_size(87 * 1024 * 1024), "87 MB");
        assert_eq!(format_size(GIB + GIB / 10), "1.1 GB");
        assert_eq!(format_size(2 * GIB), "2.0 GB");
    }

    fn pack_folder(fake: &FakeDrive) -> String {
        let root = fake.find_folder(None, ROOT_FOLDER).unwrap().unwrap();
        fake.find_folder(Some(&root), PACK).unwrap().unwrap()
    }

    fn remote_manifest(fake: &FakeDrive) -> RemoteManifest {
        serde_json::from_slice(&fake.file_bytes(&pack_folder(fake), MANIFEST_NAME).unwrap()).unwrap()
    }

    fn publish(fake: &FakeDrive, pack_dir: &Path, version: &str) -> Result<PublishOutcome, String> {
        publish_pack(fake, pack_dir, version, 1792364400, &mut no_progress())
    }

    #[test]
    fn first_publish_layout() {
        let pack_dir = pack_fixture("first", &base_mods());
        fs::write(pack_dir.join("3063/thumbnail.jpg"), "jpg").unwrap();
        let fake = FakeDrive::new();
        let mut stages = BTreeSet::new();
        let out = publish_pack(&fake, &pack_dir, "1.0", 1792364400, &mut |p: Progress| {
            stages.insert(p.stage);
        })
        .unwrap();
        let folder = pack_folder(&fake);
        assert_eq!(out, PublishOutcome::Published { folder_id: folder.clone(), version: "1.0".into(), uploaded: 6, deleted: 0 });
        assert_eq!(
            fake.tree(&folder),
            vec![
                "3063/".to_string(),
                "3063/metadata.json".into(),
                format!("3063/{}", A_PAK),
                "3063/thumbnail.jpg".into(),
                "3311/".into(),
                "3311/addons/".into(),
                format!("3311/addons/{}", W_PAK),
                "3311/metadata.json".into(),
                format!("3311/{}", B_PAK),
                "manifest.json".into(),
            ]
        );
        assert!(fake.is_shared(&folder));
        let root = fake.find_folder(None, ROOT_FOLDER).unwrap().unwrap();
        assert!(!fake.is_shared(&root));
        assert_eq!(stages, ["hashing", "uploading"].iter().map(|s| s.to_string()).collect());

        let man = remote_manifest(&fake);
        assert_eq!((man.format, man.status.as_str(), man.version.as_str()), (1, "ready", "1.0"));
        assert_eq!((man.pack_id.as_str(), man.name.as_str(), man.published_at), (PACK, "Casual Shitters", 1792364400));
        let blood = &man.mods[1];
        assert_eq!((blood.id.as_str(), blood.name.as_str()), ("3311", "Visceral Blood"));
        assert_eq!(blood.version.as_deref(), Some("2.0.2"));
        assert_eq!(man.mods[0].version.as_deref(), Some("1.2"));
        assert_eq!(blood.addons, BTreeMap::from([(W_PAK.to_string(), false)]));
        let addon = blood.files.iter().find(|f| f.path == format!("addons/{}", W_PAK)).unwrap();
        assert_eq!(fake.file_bytes(&blood.folder_id, &addon.path).unwrap(), b"wound");
        assert_eq!(addon.sha256, hashing::sha256_bytes(b"wound"));
        assert_eq!(blood.files.len(), 3);

        let json: serde_json::Value = serde_json::from_slice(&fake.file_bytes(&folder, MANIFEST_NAME).unwrap()).unwrap();
        let keys: Vec<&str> = json.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys.len(), 7);
        for key in ["format", "pack_id", "name", "version", "status", "published_at", "mods"] {
            assert!(keys.contains(&key), "{key}");
        }
        for key in ["id", "name", "version", "folder_id", "hash", "addons", "files"] {
            assert!(json["mods"][0].get(key).is_some(), "{key}");
        }
        for key in ["path", "size", "sha256", "file_id"] {
            assert!(json["mods"][0]["files"][0].get(key).is_some(), "{key}");
        }

        let local = packs::read_manifest(&pack_dir).unwrap();
        assert_eq!(local.published, Some(Published { folder_id: folder, dirty: false }));
        assert_eq!(local.version.as_deref(), Some("1.0"));
    }

    #[test]
    fn second_publish_moves_only_changes() {
        let pack_dir = pack_fixture("second", &base_mods());
        let fake = FakeDrive::new();
        publish(&fake, &pack_dir, "1.0").unwrap();
        let folder = pack_folder(&fake);
        let old = remote_manifest(&fake);
        let old_pak = old.mods[0].files.iter().find(|f| f.path == A_PAK).unwrap().file_id.clone();

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
        let before = fake.uploads();
        let mut events = Vec::new();
        let out = publish_pack(&fake, &pack_dir, "1.1", 1792364400, &mut |p: Progress| events.push(p)).unwrap();
        let last_up = events.iter().rev().find(|p| p.stage == "uploading").unwrap();
        assert_eq!((last_up.done, last_up.total), (5, 5));
        assert_eq!(last_up.bytes_done, last_up.bytes_total);
        let last_del = events.iter().rev().find(|p| p.stage == "deleting").unwrap();
        assert_eq!((last_del.done, last_del.total), (3, 3));
        // the changed pak, the new mod's three files, the changed mod's metadata.json
        assert_eq!(fake.uploads() - before, 5);
        assert_eq!(out, PublishOutcome::Published { folder_id: folder.clone(), version: "1.1".into(), uploaded: 5, deleted: 3 });
        assert_eq!(
            fake.tree(&folder),
            vec![
                "2675/".to_string(),
                "2675/Minervha_Home_CNS.pak".into(),
                "2675/Minervha_Home_CNS.ucas".into(),
                "2675/metadata.json".into(),
                "3063/".into(),
                "3063/metadata.json".into(),
                format!("3063/{}", A_PAK),
                "manifest.json".into(),
            ]
        );
        assert!(fake.read(&old_pak).is_err());
        assert_eq!(fake.file_bytes(&folder, &format!("3063/{}", A_PAK)).unwrap(), b"vests-v2");
        let man = remote_manifest(&fake);
        assert_eq!((man.status.as_str(), man.version.as_str()), ("ready", "1.1"));
        assert_eq!(man.mods.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["3063", "2675"]);
        for m in &man.mods {
            for f in &m.files {
                assert_eq!(fake.read(&f.file_id).unwrap(), fake.file_bytes(&m.folder_id, &f.path).unwrap());
            }
        }
        assert_manifest_matches_drive(&fake);
    }

    fn assert_manifest_matches_drive(fake: &FakeDrive) {
        for m in &remote_manifest(fake).mods {
            for f in &m.files {
                let bytes = fake.read(&f.file_id).unwrap();
                assert_eq!(f.sha256, hashing::sha256_bytes(&bytes), "{}/{}", m.id, f.path);
                assert_eq!(f.size, bytes.len() as u64, "{}/{}", m.id, f.path);
            }
        }
    }

    #[test]
    fn kept_meta_file_is_recorded_as_drive_holds_it() {
        let pack_dir = pack_fixture("keptmeta", &base_mods());
        let fake = FakeDrive::new();
        publish(&fake, &pack_dir, "1.0").unwrap();
        fs::write(pack_dir.join("3063/metadata.json"), r#"{"name":"Better Vests","version":"1.2","enabled":true}"#).unwrap();
        fs::write(pack_dir.join("3311").join(B_PAK), "blood-v2").unwrap();
        let before = fake.uploads();
        publish(&fake, &pack_dir, "1.1").unwrap();
        // 3311's pak and metadata.json, 3063 is unchanged so its local metadata.json stays local
        assert_eq!(fake.uploads() - before, 2);
        assert_manifest_matches_drive(&fake);
    }

    #[test]
    fn recovery_refuses_the_old_version() {
        let pack_dir = pack_fixture("recoverver", &base_mods());
        let fake = FakeDrive::new();
        publish(&fake, &pack_dir, "1.0").unwrap();
        fs::write(pack_dir.join("3063").join(A_PAK), "vests-v2").unwrap();
        packs::mark_pack_dirty(pack_dir.parent().unwrap(), PACK);
        fake.fail_uploads_after(1);
        assert!(publish(&fake, &pack_dir, "1.1").is_err());
        assert_eq!(remote_manifest(&fake).status, "updating");
        assert!(packs::read_manifest(&pack_dir).unwrap().published.unwrap().dirty);
        assert_eq!(publish(&fake, &pack_dir, "1.0").unwrap_err(), "Version 1.0 is already published. Enter a new version.");
        assert!(matches!(publish(&fake, &pack_dir, "1.1").unwrap(), PublishOutcome::Published { .. }));
        let man = remote_manifest(&fake);
        assert_eq!((man.status.as_str(), man.version.as_str()), ("ready", "1.1"));
        assert_manifest_matches_drive(&fake);
    }

    #[test]
    fn stale_folder_id_falls_back() {
        let pack_dir = pack_fixture("stale", &base_mods());
        let fake = FakeDrive::new();
        publish(&fake, &pack_dir, "1.0").unwrap();
        let old = pack_folder(&fake);
        fake.delete(&old).unwrap();
        let out = publish(&fake, &pack_dir, "1.1").unwrap();
        let folder = pack_folder(&fake);
        assert_ne!(folder, old);
        assert_eq!(out, PublishOutcome::Published { folder_id: folder.clone(), version: "1.1".into(), uploaded: 5, deleted: 0 });
        assert!(fake.is_shared(&folder));
        assert_eq!(fake.tree(&folder).len(), 9);
        assert_eq!(packs::read_manifest(&pack_dir).unwrap().published.unwrap().folder_id, folder);
    }

    #[test]
    fn newer_manifest_format_refused() {
        let pack_dir = pack_fixture("format", &base_mods());
        let fake = FakeDrive::new();
        let root = fake.create_folder(None, ROOT_FOLDER).unwrap();
        let folder = fake.create_folder(Some(&root), PACK).unwrap();
        fake.create_bytes(&folder, MANIFEST_NAME, br#"{"format":2,"layout":"new"}"#).unwrap();
        assert_eq!(publish(&fake, &pack_dir, "1.0").unwrap_err(), "Update RON Mod Manager to publish this pack");
        assert_eq!(fake.uploads(), 0);
    }

    #[test]
    fn unparseable_manifest_is_replaced() {
        let pack_dir = pack_fixture("garbled", &base_mods());
        let fake = FakeDrive::new();
        let root = fake.create_folder(None, ROOT_FOLDER).unwrap();
        let folder = fake.create_folder(Some(&root), PACK).unwrap();
        fake.create_bytes(&folder, MANIFEST_NAME, b"{not json").unwrap();
        publish(&fake, &pack_dir, "1.0").unwrap();
        assert_eq!(remote_manifest(&fake).status, "ready");
        assert_eq!(fake.tree(&folder).iter().filter(|p| *p == MANIFEST_NAME).count(), 1);
    }

    #[test]
    fn edit_during_upload_keeps_dirty() {
        let pack_dir = pack_fixture("dirtyrace", &base_mods());
        let fake = FakeDrive::new();
        publish(&fake, &pack_dir, "1.0").unwrap();
        fs::write(pack_dir.join("3063").join(A_PAK), "vests-v2").unwrap();
        let mods_dir = pack_dir.parent().unwrap().to_path_buf();
        publish_pack(&fake, &pack_dir, "1.1", 1792364400, &mut |p: Progress| {
            if p.stage == "uploading" {
                packs::mark_pack_dirty(&mods_dir, PACK);
            }
        })
        .unwrap();
        let man = packs::read_manifest(&pack_dir).unwrap();
        assert!(man.published.unwrap().dirty);
        assert_eq!(man.version.as_deref(), Some("1.1"));
    }

    #[test]
    fn same_version_refused() {
        let pack_dir = pack_fixture("samever", &base_mods());
        let fake = FakeDrive::new();
        publish(&fake, &pack_dir, "1.0").unwrap();
        fs::write(pack_dir.join("3063").join(A_PAK), "vests-v2").unwrap();
        let before = fake.uploads();
        assert_eq!(publish(&fake, &pack_dir, "1.0").unwrap_err(), "Version 1.0 is already published. Enter a new version.");
        assert_eq!(fake.uploads(), before);
        assert_eq!(remote_manifest(&fake).status, "ready");
        assert!(!packs::read_manifest(&pack_dir).unwrap().published.unwrap().dirty);
    }

    // Wraps FakeDrive to fail the by-name lookup or listing one folder
    struct Flaky<'a> {
        inner: &'a FakeDrive,
        find_fails: bool,
        list_fails: Option<(String, String)>,
    }

    impl DriveApi for Flaky<'_> {
        fn list(&self, folder_id: &str) -> Result<Vec<RemoteFile>, String> {
            match &self.list_fails {
                Some((id, err)) if id == folder_id => Err(err.clone()),
                _ => self.inner.list(folder_id),
            }
        }
        fn read(&self, file_id: &str) -> Result<Vec<u8>, String> {
            self.inner.read(file_id)
        }
        fn download(&self, file_id: &str, dest: &Path, on_bytes: &mut dyn FnMut(u64)) -> Result<String, String> {
            self.inner.download(file_id, dest, on_bytes)
        }
        fn find_folder(&self, parent: Option<&str>, name: &str) -> Result<Option<String>, String> {
            if self.find_fails {
                return Err("Could not reach Google Drive: timed out".into());
            }
            self.inner.find_folder(parent, name)
        }
        fn create_folder(&self, parent: Option<&str>, name: &str) -> Result<String, String> {
            self.inner.create_folder(parent, name)
        }
        fn share_anyone(&self, file_id: &str) -> Result<(), String> {
            self.inner.share_anyone(file_id)
        }
        fn upload(&self, parent: &str, name: &str, src: &Path, on_bytes: &mut dyn FnMut(u64)) -> Result<String, String> {
            self.inner.upload(parent, name, src, on_bytes)
        }
        fn create_bytes(&self, parent: &str, name: &str, bytes: &[u8]) -> Result<String, String> {
            self.inner.create_bytes(parent, name, bytes)
        }
        fn update_bytes(&self, file_id: &str, bytes: &[u8]) -> Result<(), String> {
            self.inner.update_bytes(file_id, bytes)
        }
        fn delete(&self, file_id: &str) -> Result<(), String> {
            self.inner.delete(file_id)
        }
        fn quota(&self) -> Result<Quota, String> {
            self.inner.quota()
        }
    }

    #[test]
    fn recorded_folder_does_not_need_the_lookup() {
        let pack_dir = pack_fixture("nolookup", &base_mods());
        let fake = FakeDrive::new();
        publish(&fake, &pack_dir, "1.0").unwrap();
        let folder = pack_folder(&fake);
        fs::write(pack_dir.join("3063").join(A_PAK), "vests-v2").unwrap();
        let flaky = Flaky { inner: &fake, find_fails: true, list_fails: None };
        let out = publish_pack(&flaky, &pack_dir, "1.1", 1792364400, &mut no_progress()).unwrap();
        assert!(matches!(out, PublishOutcome::Published { ref folder_id, .. } if *folder_id == folder));
    }

    #[test]
    fn only_a_drive_404_counts_as_a_stale_folder() {
        let pack_dir = pack_fixture("not404", &base_mods());
        let fake = FakeDrive::new();
        publish(&fake, &pack_dir, "1.0").unwrap();
        // The recorded id names another folder, so a wrong fallback to the by-name folder would succeed
        let elsewhere = fake.create_folder(None, "moved").unwrap();
        let mut man = packs::read_manifest(&pack_dir).unwrap();
        man.published.as_mut().unwrap().folder_id = elsewhere.clone();
        packs::write_manifest(&pack_dir, &man).unwrap();
        fs::write(pack_dir.join("3063").join(A_PAK), "vests-v2").unwrap();
        let err = "Google Drive error 500: backend failed on 404.pak".to_string();
        let flaky = Flaky { inner: &fake, find_fails: false, list_fails: Some((elsewhere.clone(), err.clone())) };
        let before = fake.uploads();
        assert_eq!(publish_pack(&flaky, &pack_dir, "1.1", 1792364400, &mut no_progress()).unwrap_err(), err);
        assert_eq!(fake.uploads(), before);
        assert_eq!(packs::read_manifest(&pack_dir).unwrap().published.unwrap().folder_id, elsewhere);
    }

    #[test]
    fn nothing_to_publish_clears_dirty() {
        let pack_dir = pack_fixture("nothing", &base_mods());
        let fake = FakeDrive::new();
        publish(&fake, &pack_dir, "1.0").unwrap();
        let mut man = packs::read_manifest(&pack_dir).unwrap();
        man.published.as_mut().unwrap().dirty = true;
        packs::write_manifest(&pack_dir, &man).unwrap();
        man.version = Some("0.9".into());
        packs::write_manifest(&pack_dir, &man).unwrap();
        let before = fake.uploads();
        let folder = pack_folder(&fake);
        fake.create_bytes(&folder, "junk.txt", b"x").unwrap();
        assert_eq!(publish(&fake, &pack_dir, "1.0").unwrap(), PublishOutcome::NothingToPublish { folder_id: folder.clone() });
        assert_eq!(fake.uploads(), before);
        let man = packs::read_manifest(&pack_dir).unwrap();
        assert!(!man.published.unwrap().dirty);
        assert_eq!(man.version.as_deref(), Some("1.0"));
        assert!(!fake.tree(&folder).contains(&"junk.txt".to_string()));
    }

    #[test]
    fn publish_finds_existing_folder() {
        let pack_dir = pack_fixture("existing", &base_mods());
        let fake = FakeDrive::new();
        publish(&fake, &pack_dir, "1.0").unwrap();
        let folder = pack_folder(&fake);
        let mut man = packs::read_manifest(&pack_dir).unwrap();
        man.published = None;
        packs::write_manifest(&pack_dir, &man).unwrap();
        fs::write(pack_dir.join("3063").join(A_PAK), "vests-v2").unwrap();
        let out = publish(&fake, &pack_dir, "1.1").unwrap();
        assert!(matches!(out, PublishOutcome::Published { ref folder_id, .. } if *folder_id == folder));
        let root = fake.find_folder(None, ROOT_FOLDER).unwrap().unwrap();
        assert_eq!(fake.list(&root).unwrap().len(), 1);
        assert_eq!(fake.list("root").unwrap().len(), 1);
        assert_eq!(packs::read_manifest(&pack_dir).unwrap().published.unwrap().folder_id, folder);
    }

    #[test]
    fn failed_publish_recovers() {
        let pack_dir = pack_fixture("recover", &base_mods());
        let fake = FakeDrive::new();
        fake.fail_uploads_after(2);
        assert!(publish(&fake, &pack_dir, "1.0").is_err());
        assert_eq!(fake.uploads(), 2);
        assert_eq!(remote_manifest(&fake).status, "updating");
        assert!(packs::read_manifest(&pack_dir).unwrap().published.is_none());

        let out = publish(&fake, &pack_dir, "1.0").unwrap();
        assert_eq!(fake.uploads(), 5);
        assert!(matches!(out, PublishOutcome::Published { uploaded: 3, .. }));
        let folder = pack_folder(&fake);
        assert_eq!(
            fake.tree(&folder),
            vec![
                "3063/".to_string(),
                "3063/metadata.json".into(),
                format!("3063/{}", A_PAK),
                "3311/".into(),
                "3311/addons/".into(),
                format!("3311/addons/{}", W_PAK),
                "3311/metadata.json".into(),
                format!("3311/{}", B_PAK),
                "manifest.json".into(),
            ]
        );
        assert_manifest_matches_drive(&fake);
        let man = remote_manifest(&fake);
        assert_eq!((man.status.as_str(), man.version.as_str()), ("ready", "1.0"));
        let listed: usize = man.mods.iter().map(|m| m.files.len()).sum();
        assert_eq!(listed, 5);
    }

    #[test]
    fn recovery_sweeps_strays() {
        let pack_dir = pack_fixture("strays", &base_mods());
        let fake = FakeDrive::new();
        publish(&fake, &pack_dir, "1.0").unwrap();
        let folder = pack_folder(&fake);
        fake.create_bytes(&folder, "junk.txt", b"x").unwrap();
        fake.create_folder(Some(&folder), "not-a-mod").unwrap();
        fs::write(pack_dir.join("3063").join(A_PAK), "vests-v2").unwrap();
        publish(&fake, &pack_dir, "1.1").unwrap();
        let tree = fake.tree(&folder);
        assert!(!tree.iter().any(|p| p.starts_with("junk") || p.starts_with("not-a-mod")), "{tree:?}");
    }

    #[test]
    fn low_space_writes_nothing() {
        let pack_dir = pack_fixture("lowspace", &base_mods());
        let fake = FakeDrive::new();
        fake.set_quota(Some(10), 0);
        let err = publish(&fake, &pack_dir, "1.0").unwrap_err();
        assert!(err.starts_with("Publishing needs "), "{err}");
        assert_eq!(fake.uploads(), 0);
        assert!(fake.tree("root").iter().all(|p| !p.ends_with(MANIFEST_NAME)));
        assert!(fake.tree("root").is_empty());
        assert!(packs::read_manifest(&pack_dir).unwrap().published.is_none());
    }

    #[test]
    fn linked_pack_cannot_publish() {
        let pack_dir = pack_fixture("linked", &base_mods());
        let mut man = packs::read_manifest(&pack_dir).unwrap();
        man.source = Some(PackSource { kind: "drive".into(), folder_id: "f".into(), resource_key: None });
        packs::write_manifest(&pack_dir, &man).unwrap();
        let fake = FakeDrive::new();
        assert_eq!(
            publish(&fake, &pack_dir, "1.0").unwrap_err(),
            "Casual Shitters is linked to its creator. Fork it to make changes."
        );
        assert!(fake.tree("root").is_empty());
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
        mk("zeta", "Zeta", Some(Published { folder_id: "f1".into(), dirty: true }));
        mk("alpha", "Alpha", Some(Published { folder_id: "f2".into(), dirty: true }));
        mk("clean", "Clean", Some(Published { folder_id: "f3".into(), dirty: false }));
        mk("local", "Local", None);
        let names: Vec<_> = dirty_packs(&mods).into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["Alpha", "Zeta"]);
    }
}
