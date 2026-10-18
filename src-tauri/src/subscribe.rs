use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use tauri::Emitter;

use crate::drive::{DriveApi, HttpDrive, Progress, SIZE_MISMATCH};
use crate::hashing;
use crate::packs::{self, InstalledMod, PackManifest, PackRef, PackSource};
use crate::publish::{progress_event, RemoteFileEntry, RemoteManifest, RemoteMod, Throttle, FORMAT, MANIFEST_NAME};
use crate::vfs::{self, ImportPreview, ImportSummary, ModRef, PackArchive};

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
fn is_reserved_name(name: &str) -> bool {
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

fn stage_remote(
    drive: &dyn DriveApi,
    remote: &RemotePack,
    mod_ids: Vec<String>,
    staged: &Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<(), String> {
    let manifest = &remote.manifest;
    let pack_id = manifest.pack_id.as_str();
    fs::create_dir_all(staged).map_err(|e| format!("Failed to create staging folder: {}", e))?;
    let total = manifest.mods.iter().map(|m| m.files.len()).sum();
    let mut done = 0;
    let mut throttle = Throttle::new();
    for m in &manifest.mods {
        let mod_dir = staged.join(&m.id);
        fs::create_dir_all(&mod_dir).map_err(|e| format!("Failed to create {}: {}", mod_dir.display(), e))?;
        for f in &m.files {
            let dest = mod_dir.join(safe_rel_path(&f.path)?);
            if !dest.starts_with(staged) {
                return Err(format!("Invalid file path in pack: {}", f.path));
            }
            let mut got = 0u64;
            let corrupted = || format!("A download from {} was corrupted ({}). Nothing was changed.", manifest.name, f.path);
            let downloaded = drive.download(&f.file_id, &dest, f.size, &mut |n| {
                got += n;
                if throttle.ready() {
                    progress(progress_event(pack_id, "downloading", done, total, Some(&f.path), (got, f.size)));
                }
            });
            let sha = match downloaded {
                Err(e) if e == SIZE_MISMATCH => return Err(corrupted()),
                other => other?,
            };
            if !sha.eq_ignore_ascii_case(&f.sha256) {
                return Err(corrupted());
            }
            done += 1;
            progress(progress_event(pack_id, "downloading", done, total, Some(&f.path), (f.size, f.size)));
        }
    }

    for m in &manifest.mods {
        let dir = staged.join(&m.id);
        vfs::clear_enabled_flag(&dir)?;
        if !m.addons.is_empty() {
            hashing::set_addon_states(&dir, &m.addons)?;
        }
    }
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
    let staging = mods_dir.join(format!(".import-{}", uuid::Uuid::new_v4()));
    let staged = staging.join(&archive.pack.pack_id);
    let mod_ids = archive.folders.iter().map(|(id, _)| id.clone()).collect();
    if let Err(e) = stage_remote(drive, &remote, mod_ids, &staged, progress) {
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
    let source = local.source.as_ref().ok_or_else(|| format!("{} is not linked to a Google Drive folder", local.name))?;
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
        let folder = fake.create_folder(None, PACK).unwrap();
        let mut out = Vec::new();
        for m in mods {
            let mod_folder = fake.create_folder(Some(&folder), m.id).unwrap();
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
            out.push(rm);
        }
        let manifest = RemoteManifest {
            format: FORMAT,
            pack_id: PACK.into(),
            name: NAME.into(),
            version: "1.0".into(),
            status: "ready".into(),
            published_at: 1792364400,
            mods: out,
        };
        let manifest_id = fake.create_bytes(&folder, MANIFEST_NAME, &serde_json::to_vec(&manifest).unwrap()).unwrap();
        Remote { folder, manifest_id, manifest }
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
}
