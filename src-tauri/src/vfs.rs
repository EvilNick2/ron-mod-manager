use std::fs;
use std::path::{Path, PathBuf};
use serde::{Serialize, Deserialize};
use tauri::{AppHandle, Manager};
use serde_json::Value;
use crate::packs::{link_prefix, read_manifest, resolve_for_delete, resolve_key, parse_key, KeyKind, PackRef};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AddonInfo {
    pub filename: String,
    pub enabled: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub nexus_mod_id: Option<u32>,
    pub author: String,
    pub thumbnail_url: Option<String>,
    pub enabled: bool,
    pub addons: Vec<AddonInfo>,
    pub pack: Option<PackRef>,
}

pub fn get_mods_dir(app_handle: &AppHandle) -> Result<PathBuf, String> {
    let config = crate::config::load_config(app_handle.clone()).unwrap_or_default();
    
    let mut base_dir = if let Some(path_str) = config.mod_storage_path {
        PathBuf::from(path_str)
    } else {
        let mut d = app_handle.path().config_dir().map_err(|e| e.to_string())?;
        d.push("ron-mod-manager");
        d.push("mods");
        d
    };
    
    base_dir.push("ReadyOrNot");
    
    if !base_dir.exists() {
        fs::create_dir_all(&base_dir).map_err(|e| e.to_string())?;
    }
    
    Ok(base_dir)
}

fn read_mod(dir: &Path, key: String, pack: Option<PackRef>) -> ModInfo {
    let metadata_path = dir.join("metadata.json");
    
    let folder = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut info = ModInfo {
        id: key.clone(),
        name: folder.clone(),
        description: "No metadata found.".to_string(),
        version: "1.0.0".to_string(),
        latest_version: None,
        update_available: false,
        nexus_mod_id: folder.parse::<u32>().ok(),
        author: "Unknown".to_string(),
        thumbnail_url: None,
        enabled: false,
        addons: Vec::new(),
        pack,
    };

    if let Ok(content) = fs::read_to_string(&metadata_path) {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(name) = parsed.get("name").and_then(|n| n.as_str()) {
                info.name = name.to_string();
            }
            if let Some(desc) = parsed.get("summary").or_else(|| parsed.get("description")).and_then(|d| d.as_str()) {
                info.description = desc.to_string();
            }
            if let Some(author) = parsed.get("author").and_then(|a| a.as_str()) {
                info.author = author.to_string();
            }
            if let Some(version) = parsed.get("version")
                .or_else(|| parsed.get("version"))
                .and_then(|v| v.as_str()) {
                info.version = version.to_string();
            }
            if let Some(update_available) = parsed.get("update_available").and_then(|v| v.as_bool()) {
                info.update_available = update_available;
            }
            if let Some(mod_id) = parsed.get("mod_id").and_then(|v| v.as_u64()) {
                info.nexus_mod_id = Some(mod_id as u32);
            }
            
            let thumb_path = dir.join("thumbnail.jpg");
            if let Ok(bytes) = std::fs::read(&thumb_path) {
                use base64::prelude::*;
                let b64 = BASE64_STANDARD.encode(&bytes);
                info.thumbnail_url = Some(format!("data:image/jpeg;base64,{}", b64));
            } else if let Some(pic) = parsed.get("picture_url").and_then(|p| p.as_str()) {
                info.thumbnail_url = Some(pic.to_string());
            }

            if let Some(enabled) = parsed.get("enabled").and_then(|e| e.as_bool()) {
                info.enabled = enabled;
            }
        }
    }
    
    let addons_dir = dir.join("addons");
    if addons_dir.exists() && addons_dir.is_dir() {
        if let Ok(addon_entries) = fs::read_dir(&addons_dir) {
            for addon_entry in addon_entries.filter_map(Result::ok) {
                let ap = addon_entry.path();
                if ap.is_file() {
                    let ext = ap.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                    if ext == "pak" {
                        let filename = ap.file_name().unwrap().to_string_lossy().to_string();
                        let addon_enabled = if let Ok(content) = fs::read_to_string(&metadata_path) {
                            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                                parsed.get("addons")
                                    .and_then(|a| a.get(&filename))
                                    .and_then(|a| a.get("enabled"))
                                    .and_then(|e| e.as_bool())
                                    .unwrap_or(false)
                            } else { false }
                        } else { false };
                        info.addons.push(AddonInfo { filename, enabled: addon_enabled });
                    }
                }
            }
        }
    }

    info
}

fn scan_dir(mods_dir: &Path) -> Vec<ModInfo> {
    let mut mods = Vec::new();
    let Ok(entries) = fs::read_dir(mods_dir) else {
        return mods;
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if !path.is_dir() || name.starts_with('.') {
            continue;
        }

        if let Some(manifest) = read_manifest(&path) {
            let pack = PackRef { pack_id: name.clone(), name: manifest.name, version: manifest.version };
            let Ok(children) = fs::read_dir(&path) else {
                continue;
            };
            for child in children.filter_map(Result::ok) {
                let child_path = child.path();
                let child_name = child.file_name().to_string_lossy().to_string();
                if child_path.is_dir() && !child_name.starts_with('.') {
                    mods.push(read_mod(&child_path, format!("{}/{}", name, child_name), Some(pack.clone())));
                }
            }
        } else {
            mods.push(read_mod(&path, name, None));
        }
    }

    mods
}

#[tauri::command]
pub async fn scan_local_mods(app_handle: tauri::AppHandle) -> Result<Vec<ModInfo>, String> {
    let mods_dir = get_mods_dir(&app_handle)?;
    Ok(scan_dir(&mods_dir))
}

#[tauri::command]
pub async fn refresh_installed_mod_updates(app_handle: tauri::AppHandle) -> Result<usize, String> {
    let Some(_api_key) = crate::config::get_api_key()? else {
        return Ok(0);
    };

    let config = crate::config::load_config(app_handle.clone())?;
    let game_domain = config.game_domain.unwrap_or_else(|| "readyornot".to_string());
    let mods_dir = get_mods_dir(&app_handle)?;
    let mut updated_count = 0usize;

    let Ok(entries) = fs::read_dir(mods_dir) else {
        return Ok(0);
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let mod_id_str = entry.file_name().to_string_lossy().to_string();
        let Ok(mod_id) = mod_id_str.parse::<u32>() else {
            continue;
        };

        let fresh = match crate::api::fetch_mod_metadata(game_domain.clone(), mod_id).await {
            Ok(meta) => meta,
            Err(e) => {
                println!("[UPDATE-CHECK] Failed for mod {}: {}", mod_id, e);
                continue;
            }
        };

        let metadata_path = path.join("metadata.json");
        let existing = fs::read_to_string(&metadata_path)
            .ok()
            .and_then(|c| serde_json::from_str::<Value>(&c).ok())
            .unwrap_or(Value::Object(serde_json::Map::new()));

        let preserved_enabled = existing.get("enabled").cloned();
        let preserved_addons = existing.get("addons").cloned();
        let installed_version = existing.get("installed_version")
            .or_else(|| existing.get("version"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let latest_version = existing.get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let update_available = !installed_version.is_empty()
            && !latest_version.is_empty()
            && installed_version != latest_version;
        let mut merged = fresh;

        if let Some(obj) = merged.as_object_mut() {
            if let Some(enabled) = preserved_enabled {
                obj.insert("enabled".to_string(), enabled);
            }
            if let Some(addons) = preserved_addons {
                obj.insert("addons".to_string(), addons);
            }
            obj.insert(
                "installed_version".to_string(),
                Value::String(if installed_version.is_empty() {
                    latest_version.clone()
                } else {
                    installed_version.clone()
                }),
            );
            obj.insert("latest_version".to_string(), Value::String(latest_version));
            obj.insert("update_available".to_string(), Value::Bool(update_available));
            obj.insert("mod_id".to_string(), Value::Number(serde_json::Number::from(mod_id)));
        }

        if let Ok(json_str) = serde_json::to_string_pretty(&merged) {
            if fs::write(&metadata_path, json_str).is_ok() {
                updated_count += 1;
            }
        }

        if let Some(pic_url) = merged.get("picture_url").and_then(|p| p.as_str()) {
            if let Ok(resp) = reqwest::get(pic_url).await {
                if let Ok(bytes) = resp.bytes().await {
                    fs::write(path.join("thumbnail.jpg"), bytes).ok();
                }
            }
        }
    }

    Ok(updated_count)
}

fn extract_rar(archive: &Path, dest: &Path) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|e| format!("Failed to create directory: {}", e))?;
    let mut open = unrar::Archive::new(archive)
        .open_for_processing()
        .map_err(|e| format!("Failed to open RAR: {}", e))?;
    while let Some(header) = open.read_header().map_err(|e| format!("Failed to read RAR: {}", e))? {
        open = if header.entry().is_file() {
            header.extract_with_base(dest)
        } else {
            header.skip()
        }
        .map_err(|e| format!("Failed to extract RAR: {}", e))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn install_mod_archive(app_handle: tauri::AppHandle, archive_path: String, mod_id: String) -> Result<(), String> {
    let mods_dir = get_mods_dir(&app_handle)?;
    if !matches!(parse_key(&mod_id)?, KeyKind::Loose(_)) {
        return Err(format!("Archives can only be installed to a loose mod, not {}", mod_id));
    }
    let target_dir = resolve_key(&mods_dir, &mod_id)?;
    let paks_dir = get_paks_dir(&app_handle).ok();

    let was_enabled = fs::read_to_string(target_dir.join("metadata.json"))
        .ok()
        .and_then(|c| serde_json::from_str::<Value>(&c).ok())
        .and_then(|v| v.get("enabled").and_then(Value::as_bool))
        .unwrap_or(false);
    if let Some(paks_dir) = &paks_dir {
        unlink_mod(paks_dir, &mod_id)?;
    }

    if target_dir.exists() {
        std::fs::remove_dir_all(&target_dir).ok();
    }
    std::fs::create_dir_all(&target_dir).map_err(|e| format!("Failed to create mod directory: {}", e))?;
    
    let path = std::path::Path::new(&archive_path);
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();

    if extension == "pak" {
        // Just copy the pak file directly
        let file_name = path.file_name().ok_or("Invalid file name")?;
        let outpath = target_dir.join(file_name);
        std::fs::copy(&archive_path, &outpath).map_err(|e| format!("Failed to copy .pak file: {}", e))?;
    } else if extension == "rar" {
        let (a, d) = (path.to_path_buf(), target_dir.clone());
        tauri::async_runtime::spawn_blocking(move || extract_rar(&a, &d))
            .await
            .map_err(|e| e.to_string())??;
    } else {
        // Assume it's a zip and extract
        let file = std::fs::File::open(&archive_path).map_err(|e| format!("Failed to open archive: {}", e))?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Invalid zip archive: {}", e))?;
        
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
            let outpath = match file.enclosed_name() {
                Some(path) => target_dir.join(path),
                None => continue,
            };
            
            if file.is_dir() {
                std::fs::create_dir_all(&outpath).ok();
            } else {
                if let Some(p) = outpath.parent() {
                    std::fs::create_dir_all(p).ok();
                }
                let mut outfile = std::fs::File::create(&outpath).map_err(|_| "Failed to create extracted file")?;
                std::io::copy(&mut file, &mut outfile).map_err(|_| "Failed to write extracted file")?;
            }
        }
    }
    
    if let Ok(mod_id_num) = mod_id.parse::<u32>() {
        if let Ok(metadata) = crate::api::fetch_mod_metadata("readyornot".to_string(), mod_id_num).await {
            let meta_path = target_dir.join("metadata.json");
            if let Ok(json_str) = serde_json::to_string_pretty(&metadata) {
                std::fs::write(&meta_path, json_str).ok();
            }
            
            if let Some(pic_url) = metadata.get("picture_url").and_then(|p| p.as_str()) {
                if let Ok(resp) = reqwest::get(pic_url).await {
                    if let Ok(bytes) = resp.bytes().await {
                        std::fs::write(target_dir.join("thumbnail.jpg"), bytes).ok();
                    }
                }
            }
        }
    }

    set_enabled_flag(&target_dir, was_enabled);
    if was_enabled {
        if let Some(paks_dir) = &paks_dir {
            enable_key(&mods_dir, paks_dir, &mod_id)?;
        }
    }
    
    Ok(())
}

#[cfg(unix)]
fn create_link(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn create_link(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::hard_link(src, dst).or_else(|_| std::fs::copy(src, dst).map(|_| ()))
}

fn is_deployable(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    matches!(ext.as_str(), "pak" | "ucas" | "utoc")
}

fn collect_deployables(mod_dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    if path.file_name().and_then(|n| n.to_str()) != Some("addons") {
                        walk(&path, out);
                    }
                } else if path.is_file() && is_deployable(&path) {
                    out.push(path);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(mod_dir, &mut out);
    out.sort();
    out
}

fn addon_files(addons_dir: &Path, pak_filename: &str) -> Vec<PathBuf> {
    let pak = addons_dir.join(pak_filename);
    let mut out = vec![pak.clone()];
    if let Ok(entries) = std::fs::read_dir(addons_dir) {
        for p in entries.filter_map(Result::ok).map(|e| e.path()) {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            if p.is_file() && p != pak && (ext == "ucas" || ext == "utoc") && p.file_stem() == pak.file_stem() {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn entry_present(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

fn unlink_mod(paks_dir: &Path, key: &str) -> Result<usize, String> {
    let prefix = link_prefix(key);
    let entries = match fs::read_dir(paks_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(format!("Failed to read Paks directory: {}", e)),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read Paks directory: {}", e))?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            fs::remove_file(entry.path()).map_err(|e| format!("Failed to remove injected mod: {}", e))?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(unix)]
fn sweep_orphans(paks_dir: &Path, mods_dir: &Path) -> usize {
    if !mods_dir.exists() {
        return 0;
    }
    let Ok(entries) = fs::read_dir(paks_dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let is_link = path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false);
        if !is_link {
            continue;
        }
        if let Ok(target) = fs::read_link(&path) {
            if target.starts_with(mods_dir) && !target.exists() && fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

#[cfg(windows)]
fn sweep_orphans(_paks_dir: &Path, _mods_dir: &Path) -> usize {
    0
}

fn set_enabled_flag(mod_dir: &Path, enabled: bool) {
    let metadata_path = mod_dir.join("metadata.json");
    let mut parsed = fs::read_to_string(&metadata_path)
        .ok()
        .and_then(|c| serde_json::from_str::<Value>(&c).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = parsed.as_object_mut() {
        obj.insert("enabled".to_string(), Value::Bool(enabled));
    }
    if let Ok(json_str) = serde_json::to_string_pretty(&parsed) {
        fs::write(&metadata_path, json_str).ok();
    }
}

fn enable_key(mods_dir: &Path, paks_dir: &Path, key: &str) -> Result<(), String> {
    parse_key(key)?;
    let mod_dir = resolve_key(mods_dir, key)?;

    let nexus_id = scan_dir(mods_dir).into_iter().find(|m| m.id == key).and_then(|m| m.nexus_mod_id);
    if let Some(nexus_id) = nexus_id {
        for other in scan_dir(mods_dir) {
            if other.id != key && other.nexus_mod_id == Some(nexus_id) && parse_key(&other.id).is_ok() {
                unlink_mod(paks_dir, &other.id)?;
                set_enabled_flag(&resolve_key(mods_dir, &other.id)?, false);
            }
        }
    }

    unlink_mod(paks_dir, key)?;
    fs::create_dir_all(paks_dir).map_err(|e| format!("Failed to create Paks directory: {}", e))?;

    let result = link_mod_files(&mod_dir, paks_dir, key);
    if let Err(e) = result {
        let _ = unlink_mod(paks_dir, key);
        return Err(e);
    }

    set_enabled_flag(&mod_dir, true);
    Ok(())
}

fn shallowest_per_name(files: Vec<PathBuf>, base: &Path) -> Vec<PathBuf> {
    let mut best: std::collections::BTreeMap<std::ffi::OsString, (usize, PathBuf)> = Default::default();
    for path in files {
        let rel = path.strip_prefix(base).unwrap_or(&path);
        let depth = rel.components().count();
        let name = path.file_name().unwrap().to_os_string();
        let better = match best.get(&name) {
            Some((d, cur)) => (depth, rel) < (*d, cur.strip_prefix(base).unwrap_or(cur)),
            None => true,
        };
        if better {
            best.insert(name, (depth, path));
        }
    }
    best.into_values().map(|(_, p)| p).collect()
}

fn link_mod_files(mod_dir: &Path, paks_dir: &Path, key: &str) -> Result<(), String> {
    let prefix = link_prefix(key);
    for path in shallowest_per_name(collect_deployables(mod_dir), mod_dir) {
        let link_path = paks_dir.join(format!("{}{}", prefix, path.file_name().unwrap().to_string_lossy()));
        create_link(&path, &link_path).map_err(|e| format!("Failed to inject mod: {}", e))?;
    }

    let addons_dir = mod_dir.join("addons");
    let metadata: Option<Value> = fs::read_to_string(mod_dir.join("metadata.json"))
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok());
    if let Ok(entries) = fs::read_dir(&addons_dir) {
        for ap in entries.filter_map(Result::ok).map(|e| e.path()) {
            let is_pak = ap.is_file() && ap.extension().and_then(|e| e.to_str()).unwrap_or("").eq_ignore_ascii_case("pak");
            if !is_pak {
                continue;
            }
            let fname = ap.file_name().unwrap().to_string_lossy().to_string();
            let addon_enabled = metadata.as_ref()
                .and_then(|m| m.get("addons"))
                .and_then(|a| a.get(&fname))
                .and_then(|a| a.get("enabled"))
                .and_then(|e| e.as_bool())
                .unwrap_or(false);
            if !addon_enabled {
                continue;
            }
            for file in shallowest_per_name(addon_files(&addons_dir, &fname), &addons_dir) {
                let link_path = paks_dir.join(format!("{}addon_{}", prefix, file.file_name().unwrap().to_string_lossy()));
                create_link(&file, &link_path).map_err(|e| format!("Failed to inject addon: {}", e))?;
            }
        }
    }
    Ok(())
}

fn disable_key(mods_dir: &Path, paks_dir: &Path, key: &str) -> Result<(), String> {
    parse_key(key)?;
    let mod_dir = resolve_key(mods_dir, key)?;
    unlink_mod(paks_dir, key)?;
    set_enabled_flag(&mod_dir, false);
    Ok(())
}

#[tauri::command]
pub async fn toggle_mod(app_handle: tauri::AppHandle, mod_id: String, enable: bool) -> Result<(), String> {
    let config = crate::config::load_config(app_handle.clone()).map_err(|e| e.to_string())?;
    config.game_path.ok_or("Game installation path is not set! Please configure it in the Settings tab.")?;
    let paks_dir = get_paks_dir(&app_handle)?;
    let mods_dir = get_mods_dir(&app_handle)?;
    if enable {
        enable_key(&mods_dir, &paks_dir, &mod_id)
    } else {
        disable_key(&mods_dir, &paks_dir, &mod_id)
    }
}

#[tauri::command]
pub async fn pick_mod_archive() -> Result<Option<String>, String> {
    let result = rfd::AsyncFileDialog::new()
        .set_title("Select Mod Archive (.zip, .pak, .rar)")
        .add_filter("Mod Archives", &["zip", "pak", "rar"])
        .pick_file()
        .await;
        
    Ok(result.map(|f| f.path().to_string_lossy().to_string()))
}

#[tauri::command]
pub async fn delete_mod(app_handle: tauri::AppHandle, mod_id: String) -> Result<(), String> {
    let mods_dir = get_mods_dir(&app_handle)?;
    if parse_key(&mod_id).is_ok() {
        if let Ok(paks_dir) = get_paks_dir(&app_handle) {
            disable_key(&mods_dir, &paks_dir, &mod_id)?;
        }
    }
    let target_dir = resolve_for_delete(&mods_dir, &mod_id)?;
    
    if target_dir.exists() {
        std::fs::remove_dir_all(&target_dir).map_err(|e| format!("Failed to obliterate mod directory: {}", e))?;
    }
    
    Ok(())
}

#[derive(Serialize, Debug, Default)]
pub struct ApplySummary {
    pub enabled: usize,
    pub disabled: usize,
    pub missing: usize,
}

// `disabled` counts mods that had links in Paks or were flagged enabled, not every mod outside the set
fn apply_keys(mods_dir: &Path, paks_dir: &Path, keys: &[String]) -> Result<ApplySummary, String> {
    let wanted: std::collections::HashSet<&String> = keys.iter().collect();
    let mut summary = ApplySummary::default();

    for m in scan_dir(mods_dir) {
        if wanted.contains(&m.id) || parse_key(&m.id).is_err() {
            continue;
        }
        let had_links = unlink_mod(paks_dir, &m.id)? > 0;
        disable_key(mods_dir, paks_dir, &m.id)?;
        if had_links || m.enabled {
            summary.disabled += 1;
        }
    }

    sweep_orphans(paks_dir, mods_dir);

    for key in keys {
        if resolve_key(mods_dir, key).map(|p| p.is_dir()).unwrap_or(false) {
            enable_key(mods_dir, paks_dir, key)?;
            summary.enabled += 1;
        } else {
            summary.missing += 1;
        }
    }
    Ok(summary)
}

#[tauri::command]
pub async fn apply_preset(app_handle: tauri::AppHandle, name: String, mod_ids: Vec<String>) -> Result<ApplySummary, String> {
    let config = crate::config::load_config(app_handle.clone()).map_err(|e| e.to_string())?;
    config.game_path.ok_or("Game installation path is not set! Please configure it in the Settings tab.")?;
    let paks_dir = get_paks_dir(&app_handle)?;
    let mods_dir = get_mods_dir(&app_handle)?;
    let summary = apply_keys(&mods_dir, &paks_dir, &mod_ids)?;

    let mut config = crate::config::load_config(app_handle.clone())?;
    config.active_preset = Some(name);
    crate::config::save_config(app_handle, config)?;

    Ok(summary)
}

#[tauri::command]
pub fn load_packs(app_handle: tauri::AppHandle) -> Result<Vec<crate::packs::PackManifest>, String> {
    let mods_dir = get_mods_dir(&app_handle)?;
    let mut packs: Vec<_> = child_dirs(&mods_dir)?
        .into_iter()
        .filter_map(|name| read_manifest(&mods_dir.join(name)))
        .collect();
    packs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(packs)
}

#[tauri::command]
pub async fn delete_pack(app_handle: tauri::AppHandle, pack_id: String) -> Result<(), String> {
    let mods_dir = get_mods_dir(&app_handle)?;
    let pack_dir = mods_dir.join(&pack_id);
    if parse_key(&format!("{}/1", pack_id)).is_err() || read_manifest(&pack_dir).is_none() {
        return Err(format!("No modpack folder for {:?}", pack_id));
    }
    if let Ok(paks_dir) = get_paks_dir(&app_handle) {
        for name in child_dirs(&pack_dir)? {
            let key = format!("{}/{}", pack_id, name);
            if parse_key(&key).is_ok() {
                disable_key(&mods_dir, &paks_dir, &key)?;
            }
        }
    }
    fs::remove_dir_all(&pack_dir).map_err(|e| format!("Failed to remove modpack folder: {}", e))?;

    let mut config = crate::config::load_config(app_handle.clone())?;
    if config.active_preset.as_deref() == Some(&format!("pack:{}", pack_id)) {
        config.active_preset = None;
        crate::config::save_config(app_handle, config)?;
    }
    Ok(())
}

fn zip_dir(
    dir: &Path,
    prefix: &str,
    writer: &mut zip::ZipWriter<std::fs::File>,
    options: zip::write::SimpleFileOptions,
) -> Result<(), String> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let new_prefix = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{}/{}", prefix, name)
            };
            
            if path.is_dir() {
                writer.add_directory(&format!("{}/", new_prefix), options).map_err(|e| e.to_string())?;
                zip_dir(&path, &new_prefix, writer, options)?;
            } else {
                writer.start_file(&new_prefix, options).map_err(|e| e.to_string())?;
                if let Ok(mut f) = std::fs::File::open(&path) {
                    std::io::copy(&mut f, writer).map_err(|e| e.to_string())?;
                }
            }
        }
    }
    Ok(())
}

fn write_pack_zip(out: &Path, preset_json: &Value, folders: &[(String, PathBuf)]) -> Result<(), String> {
    let file = std::fs::File::create(out).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);

    zip.start_file("preset.json", options).map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut zip, preset_json.to_string().as_bytes()).map_err(|e| e.to_string())?;

    for (name, src) in folders {
        zip.add_directory(&format!("{}/", name), options).map_err(|e| e.to_string())?;
        zip_dir(src, name, &mut zip, options)?;
    }

    zip.finish().map_err(|e| format!("Failed to complete zip: {}", e))?;
    Ok(())
}

// The preset.json and the (zip folder name, source folder) pairs for `pack:<id>` or a saved preset
fn export_plan(
    mods_dir: &Path,
    presets: &std::collections::HashMap<String, Vec<String>>,
    name: &str,
    version: Option<String>,
) -> Result<(Value, Vec<(String, PathBuf)>), String> {
    let (preset_json, keys) = if let Some(pack_id) = name.strip_prefix("pack:") {
        if parse_key(&format!("{}/1", pack_id)).is_err() {
            return Err("Modpack not found".to_string());
        }
        let manifest = read_manifest(&mods_dir.join(pack_id)).ok_or("Modpack not found")?;
        let json = serde_json::json!({
            "name": manifest.name,
            "pack_id": pack_id,
            "version": version.or(manifest.version.clone()),
            "mod_ids": manifest.mod_ids,
        });
        let keys = manifest.mod_ids.iter().map(|id| format!("{}/{}", pack_id, id)).collect::<Vec<_>>();
        (json, keys)
    } else {
        let keys = presets.get(name).ok_or("Preset not found")?.clone();
        (Value::Null, keys)
    };

    let mut seen = std::collections::HashSet::new();
    let mut nexus_ids = Vec::new();
    let mut folders = Vec::new();
    for key in &keys {
        let nexus_id = key.rsplit('/').next().unwrap_or(key).to_string();
        if !seen.insert(nexus_id.clone()) {
            return Err(format!("Two mods in this preset share Nexus id {}", nexus_id));
        }
        let src = resolve_key(mods_dir, key)?;
        if src.is_dir() {
            folders.push((nexus_id.clone(), src));
        }
        nexus_ids.push(nexus_id);
    }

    let preset_json = if preset_json.is_null() {
        serde_json::json!({ "name": name, "mod_ids": nexus_ids })
    } else {
        preset_json
    };
    Ok((preset_json, folders))
}

#[tauri::command]
pub async fn export_preset(app_handle: tauri::AppHandle, preset_name: String, version: Option<String>) -> Result<(), String> {
    let config = crate::config::load_config(app_handle.clone())?;
    let mods_dir = get_mods_dir(&app_handle)?;
    let (preset_json, folders) = export_plan(&mods_dir, &config.presets, &preset_name, version)?;
    let file_name = preset_json["name"].as_str().unwrap_or("preset").to_string();

    let result = rfd::AsyncFileDialog::new()
        .set_title("Export Preset Zip Archive")
        .add_filter("zip", &["zip"])
        .set_file_name(&format!("{}.zip", file_name))
        .save_file()
        .await;
        
    let save_path = match result {
        Some(f) => f.path().to_path_buf(),
        None => return Err("Export cancelled".to_string()),
    };

    write_pack_zip(&save_path, &preset_json, &folders)
}

#[tauri::command]
pub async fn pick_preset_archive() -> Result<Option<String>, String> {
    let result = rfd::AsyncFileDialog::new()
        .set_title("Select Preset Zip Archive")
        .add_filter("zip", &["zip"])
        .pick_file()
        .await;

    Ok(result.map(|f| f.path().to_string_lossy().to_string()))
}

#[tauri::command]
pub async fn inspect_preset_archive(app_handle: tauri::AppHandle, archive_path: String) -> Result<ImportPreview, String> {
    let mods_dir = get_mods_dir(&app_handle)?;
    inspect_archive(Path::new(&archive_path), &mods_dir)
}

#[tauri::command]
pub async fn import_preset(app_handle: tauri::AppHandle, archive_path: String) -> Result<ImportSummary, String> {
    let mods_dir = get_mods_dir(&app_handle)?;
    // without a game path nothing can be linked, so a Paks folder that does not exist is enough
    let paks_dir = get_paks_dir(&app_handle).unwrap_or_else(|_| mods_dir.join(".no-game-path"));
    let summary = import_from_archive(Path::new(&archive_path), &mods_dir, &paks_dir)?;

    let mut config = crate::config::load_config(app_handle.clone())?;
    crate::config::tidy_presets_after_import(&mut config, &summary.pack.pack_id, &summary.superseded_ids, &summary.removed_keys);
    crate::config::save_config(app_handle, config)
        .map_err(|e| format!("The pack was imported but saved presets could not be updated: {}", e))?;

    Ok(summary)
}

#[derive(Serialize, Clone, Debug)]
pub struct ModRef {
    pub key: String,
    pub nexus_mod_id: Option<u32>,
    pub name: String,
    pub version: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct ImportPreview {
    pub pack: PackRef,
    pub installed_version: Option<String>,
    pub added: Vec<ModRef>,
    pub removed: Vec<ModRef>,
    pub changed: Vec<ModRef>,
    pub superseded: Vec<ModRef>,
}

#[derive(Serialize, Debug)]
pub struct ImportSummary {
    pub pack: PackRef,
    pub keys: Vec<String>,
    pub added: usize,
    pub changed: usize,
    pub removed_keys: Vec<String>,
    pub superseded_ids: Vec<String>,
    pub failed_deletes: Vec<String>,
}

struct PackArchive {
    pack: PackRef,
    // mod folder id and that folder's metadata.json, in numeric order
    folders: Vec<(String, Option<Value>)>,
}

struct ImportPlan {
    installed: Option<PathBuf>,
    installed_version: Option<String>,
    added: Vec<ModRef>,
    removed: Vec<ModRef>,
    changed: Vec<ModRef>,
    superseded: Vec<ModRef>,
}

fn is_numeric(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn numeric_order(a: &String, b: &String) -> std::cmp::Ordering {
    (a.len(), a).cmp(&(b.len(), b))
}

fn read_meta(mod_dir: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(mod_dir.join("metadata.json")).ok()?).ok()
}

fn meta_version(meta: Option<&Value>) -> Option<String> {
    let meta = meta?;
    meta.get("installed_version").or_else(|| meta.get("version")).and_then(|v| v.as_str()).map(str::to_string)
}

fn mod_ref(key: String, id: &str, meta: Option<&Value>) -> ModRef {
    let name = meta.and_then(|m| m.get("name")).and_then(|n| n.as_str()).unwrap_or(id).to_string();
    ModRef { key, nexus_mod_id: id.parse().ok(), name, version: meta_version(meta) }
}

fn open_zip(archive: &Path) -> Result<zip::ZipArchive<fs::File>, String> {
    let file = fs::File::open(archive).map_err(|e| format!("Failed to open zip: {}", e))?;
    zip::ZipArchive::new(file).map_err(|e| format!("Invalid zip archive: {}", e))
}

// The mod folder an archive entry belongs to, or None for entries that are not extracted.
fn archive_folder(rel: &Path, is_dir: bool) -> Result<Option<String>, String> {
    if rel.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return Err(format!("Invalid preset archive: unsafe path {:?}", rel));
    }
    let mut parts = rel.components();
    let first = match parts.next() {
        Some(std::path::Component::Normal(s)) => s.to_string_lossy().to_string(),
        _ => return Ok(None),
    };
    let nested = parts.next().is_some();
    if first == "__MACOSX" || (!nested && !is_dir) {
        return Ok(None);
    }
    if !is_numeric(&first) {
        return Err(format!("Invalid preset archive: folder {:?} is not a Nexus mod id", first));
    }
    Ok(Some(first))
}

fn read_pack_archive(zip: &mut zip::ZipArchive<fs::File>) -> Result<PackArchive, String> {
    let mut preset_text: Option<String> = None;
    let mut ids: Vec<String> = Vec::new();
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| format!("Invalid zip archive: {}", e))?;
        if file.name() == "preset.json" {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut file, &mut text).map_err(|e| format!("Bad preset.json: {}", e))?;
            preset_text = Some(text);
            continue;
        }
        let rel = file.enclosed_name().ok_or_else(|| format!("Invalid preset archive: unsafe path {:?}", file.name()))?;
        if let Some(id) = archive_folder(&rel, file.is_dir())? {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    let preset_text = preset_text.ok_or("Invalid preset archive: missing preset.json")?;
    let preset: Value = serde_json::from_str(&preset_text).map_err(|e| format!("Bad preset.json: {}", e))?;
    if ids.is_empty() {
        return Err("Invalid preset archive: it contains no mod folders".to_string());
    }
    ids.sort_by(numeric_order);

    let folders = ids.into_iter().map(|id| {
        let meta = zip.by_name(&format!("{}/metadata.json", id)).ok().and_then(|mut f| {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut f, &mut text).ok()?;
            serde_json::from_str(&text).ok()
        });
        (id, meta)
    }).collect();

    Ok(PackArchive { pack: crate::packs::pack_identity(&preset), folders })
}

fn child_dirs(dir: &Path) -> Result<Vec<String>, String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("Failed to read {}: {}", dir.display(), e))?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read {}: {}", dir.display(), e))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().is_dir() && !name.starts_with('.') {
            out.push(name);
        }
    }
    out.sort_by(numeric_order);
    Ok(out)
}

// The installed folder of this pack, or an error when a folder that is not a pack is in the way.
fn installed_pack_dir(mods_dir: &Path, pack_id: &str) -> Result<Option<PathBuf>, String> {
    let dir = mods_dir.join(pack_id);
    match fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Failed to read {}: {}", dir.display(), e)),
        Ok(md) if md.is_dir() && read_manifest(&dir).is_some() => Ok(Some(dir)),
        Ok(_) => Err(format!(
            "{} already exists in mod storage and is not a modpack. Rename or remove it and try again",
            dir.display()
        )),
    }
}

fn plan_import(archive: &PackArchive, mods_dir: &Path) -> Result<ImportPlan, String> {
    let pack_id = &archive.pack.pack_id;
    let installed = installed_pack_dir(mods_dir, pack_id)?;
    let installed_version = installed.as_deref().and_then(read_manifest).and_then(|m| m.version);
    let on_disk = match &installed {
        Some(dir) => child_dirs(dir)?,
        None => Vec::new(),
    };
    let key = |id: &str| format!("{}/{}", pack_id, id);

    let mut plan = ImportPlan { installed, installed_version, added: vec![], removed: vec![], changed: vec![], superseded: vec![] };
    for (id, meta) in &archive.folders {
        if on_disk.contains(id) {
            let disk_meta = plan.installed.as_ref().and_then(|d| read_meta(&d.join(id)));
            if meta_version(disk_meta.as_ref()) != meta_version(meta.as_ref()) {
                plan.changed.push(mod_ref(key(id), id, meta.as_ref()));
            }
        } else {
            plan.added.push(mod_ref(key(id), id, meta.as_ref()));
        }
        let loose = mods_dir.join(id);
        if loose.is_dir() && read_manifest(&loose).is_none() {
            plan.superseded.push(mod_ref(id.clone(), id, read_meta(&loose).as_ref()));
        }
    }
    for id in &on_disk {
        if !archive.folders.iter().any(|(f, _)| f == id) {
            let disk_meta = plan.installed.as_ref().and_then(|d| read_meta(&d.join(id)));
            plan.removed.push(mod_ref(key(id), id, disk_meta.as_ref()));
        }
    }
    Ok(plan)
}

fn inspect_archive(archive: &Path, mods_dir: &Path) -> Result<ImportPreview, String> {
    let mut zip = open_zip(archive)?;
    let archive = read_pack_archive(&mut zip)?;
    let plan = plan_import(&archive, mods_dir)?;
    Ok(ImportPreview {
        pack: archive.pack,
        installed_version: plan.installed_version,
        added: plan.added,
        removed: plan.removed,
        changed: plan.changed,
        superseded: plan.superseded,
    })
}

fn clear_enabled_flag(mod_dir: &Path) -> Result<(), String> {
    let path = mod_dir.join("metadata.json");
    let Some(mut meta) = read_meta(mod_dir) else {
        return Ok(());
    };
    let Some(obj) = meta.as_object_mut() else {
        return Ok(());
    };
    obj.insert("enabled".to_string(), Value::Bool(false));
    let json = serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| format!("Failed to write {}: {}", path.display(), e))
}

fn stage_pack(zip: &mut zip::ZipArchive<fs::File>, archive: &PackArchive, staged: &Path) -> Result<(), String> {
    fs::create_dir_all(staged).map_err(|e| format!("Failed to create staging folder: {}", e))?;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| format!("Invalid zip archive: {}", e))?;
        if file.name() == "preset.json" {
            continue;
        }
        let rel = file.enclosed_name().ok_or_else(|| format!("Invalid preset archive: unsafe path {:?}", file.name()))?;
        if archive_folder(&rel, file.is_dir())?.is_none() {
            continue;
        }
        let out = staged.join(&rel);
        if file.is_dir() {
            fs::create_dir_all(&out).map_err(|e| format!("Failed to create {}: {}", out.display(), e))?;
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
            }
            let mut outfile = fs::File::create(&out).map_err(|e| format!("Failed to create {}: {}", out.display(), e))?;
            std::io::copy(&mut file, &mut outfile).map_err(|e| format!("Failed to extract {}: {}", out.display(), e))?;
        }
    }

    let mut mod_ids = Vec::new();
    for (id, _) in &archive.folders {
        let dir = staged.join(id);
        fs::create_dir_all(&dir).map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;
        clear_enabled_flag(&dir)?;
        mod_ids.push(id.clone());
    }
    let imported_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    crate::packs::write_manifest(staged, &crate::packs::PackManifest {
        pack_id: archive.pack.pack_id.clone(),
        name: archive.pack.name.clone(),
        version: archive.pack.version.clone(),
        mod_ids,
        imported_at,
    })
}

// Removes the staging folder on a failed import and returns the error to report.
fn abandon_staging(staging: &Path, err: String) -> String {
    match fs::remove_dir_all(staging) {
        Ok(()) => err,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => err,
        Err(e) => format!("{} (could not remove {}: {})", err, staging.display(), e),
    }
}

// Restores the links of keys unlinked by a failed swap. Unlike enable_key it leaves other copies and metadata alone.
fn relink(mods_dir: &Path, paks_dir: &Path, keys: &[String], err: String) -> String {
    let restore = |key: &str| -> Result<(), String> {
        unlink_mod(paks_dir, key)?;
        link_mod_files(&resolve_key(mods_dir, key)?, paks_dir, key)
    };
    let failed: Vec<&str> = keys.iter()
        .filter(|k| restore(k).is_err())
        .map(String::as_str)
        .collect();
    if failed.is_empty() {
        err
    } else {
        format!("{} (could not re-enable {})", err, failed.join(", "))
    }
}

enum SwapError {
    // the library is as it was, staging can be deleted
    RolledBack(String),
    // the old pack is only inside staging, which must be kept
    Stranded(String),
}

// Unlinks the old pack, sets it aside inside staging and moves the staged pack in.
fn swap_pack(mods_dir: &Path, paks_dir: &Path, staging: &Path, staged: &Path, target: &Path, pack_id: &str) -> Result<(), SwapError> {
    let old_keys: Vec<String> = child_dirs(target).map_err(SwapError::RolledBack)?.iter().map(|c| format!("{}/{}", pack_id, c)).collect();
    let mut linked: Vec<String> = Vec::new();
    for key in &old_keys {
        match unlink_mod(paks_dir, key) {
            Ok(0) => {}
            Ok(_) => linked.push(key.clone()),
            Err(e) => {
                linked.push(key.clone());
                return Err(SwapError::RolledBack(relink(mods_dir, paks_dir, &linked, e)));
            }
        }
    }

    let aside = staging.join(format!("old-{}", pack_id));
    if fs::rename(target, &aside).is_err() {
        return Err(SwapError::RolledBack(relink(mods_dir, paks_dir, &linked, "Close Ready or Not and try again".to_string())));
    }
    if let Err(e) = fs::rename(staged, target) {
        let err = format!("Failed to move the new pack into place: {}", e);
        if let Err(back) = fs::rename(&aside, target) {
            return Err(SwapError::Stranded(format!("{}. The previous pack could not be restored ({}) and is kept at {}", err, back, aside.display())));
        }
        return Err(SwapError::RolledBack(relink(mods_dir, paks_dir, &linked, err)));
    }
    Ok(())
}

fn import_from_archive(archive: &Path, mods_dir: &Path, paks_dir: &Path) -> Result<ImportSummary, String> {
    let mut zip = open_zip(archive)?;
    let archive = read_pack_archive(&mut zip)?;
    let plan = plan_import(&archive, mods_dir)?;
    let pack_id = archive.pack.pack_id.clone();

    let staging = mods_dir.join(format!(".import-{}", uuid::Uuid::new_v4()));
    let staged = staging.join(&pack_id);
    if let Err(e) = stage_pack(&mut zip, &archive, &staged) {
        return Err(abandon_staging(&staging, e));
    }

    let target = mods_dir.join(&pack_id);
    if plan.installed.is_some() {
        match swap_pack(mods_dir, paks_dir, &staging, &staged, &target, &pack_id) {
            Ok(()) => {}
            Err(SwapError::RolledBack(e)) => return Err(abandon_staging(&staging, e)),
            Err(SwapError::Stranded(e)) => return Err(e),
        }
    } else if let Err(e) = fs::rename(&staged, &target) {
        return Err(abandon_staging(&staging, format!("Failed to move the new pack into place: {}", e)));
    }

    let mut failed_deletes: Vec<String> = Vec::new();
    let mut superseded_ids: Vec<String> = Vec::new();
    for m in &plan.superseded {
        let dir = mods_dir.join(&m.key);
        superseded_ids.push(m.key.clone());
        if unlink_mod(paks_dir, &m.key).is_err() || fs::remove_dir_all(&dir).is_err() {
            failed_deletes.push(dir.display().to_string());
        }
    }

    sweep_orphans(paks_dir, mods_dir);

    if let Err(e) = fs::remove_dir_all(&staging) {
        if e.kind() != std::io::ErrorKind::NotFound {
            failed_deletes.push(staging.display().to_string());
        }
    }

    Ok(ImportSummary {
        keys: archive.folders.iter().map(|(id, _)| format!("{}/{}", pack_id, id)).collect(),
        added: plan.added.len(),
        changed: plan.changed.len(),
        removed_keys: plan.removed.into_iter().map(|m| m.key).collect(),
        superseded_ids,
        failed_deletes,
        pack: archive.pack,
    })
}

#[tauri::command]
pub async fn toggle_all_mods(app_handle: tauri::AppHandle, enable: bool) -> Result<(), String> {
    let mods = scan_local_mods(app_handle.clone()).await?;
    for m in mods {
        if m.enabled != enable {
            toggle_mod(app_handle.clone(), m.id, enable).await?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn install_addon(app_handle: tauri::AppHandle, mod_id: String, archive_path: String) -> Result<Vec<String>, String> {
    let mods_dir = get_mods_dir(&app_handle)?;
    let mod_dir = resolve_key(&mods_dir, &mod_id)?;
    let addons_dir = mod_dir.join("addons");
    std::fs::create_dir_all(&addons_dir).map_err(|e| format!("Failed to create addons directory: {}", e))?;
    
    let path = std::path::Path::new(&archive_path);
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let mut added_files: Vec<String> = Vec::new();
    
    if extension == "pak" {
        let file_name = path.file_name().ok_or("Invalid file name")?;
        let outpath = addons_dir.join(file_name);
        std::fs::copy(&archive_path, &outpath).map_err(|e| format!("Failed to copy addon .pak: {}", e))?;
        added_files.push(file_name.to_string_lossy().to_string());
    } else if extension == "zip" {
        let file = std::fs::File::open(&archive_path).map_err(|e| format!("Failed to open archive: {}", e))?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Invalid zip archive: {}", e))?;
        
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
            if file.is_dir() { continue; }
            
            let fname = match file.enclosed_name() {
                Some(p) => p.file_name().unwrap_or_default().to_string_lossy().to_string(),
                None => continue,
            };
            
            let ext = std::path::Path::new(&fname).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            if is_deployable(Path::new(&fname)) {
                let outpath = addons_dir.join(&fname);
                let mut outfile = std::fs::File::create(&outpath).map_err(|e| format!("Failed to create addon file: {}", e))?;
                std::io::copy(&mut file, &mut outfile).map_err(|e| format!("Failed to extract addon: {}", e))?;
                if ext == "pak" {
                    added_files.push(fname);
                }
            }
        }
    } else if extension == "rar" {
        let (a, d) = (std::path::PathBuf::from(&archive_path), addons_dir.clone());
        tauri::async_runtime::spawn_blocking(move || extract_rar(&a, &d))
            .await
            .map_err(|e| e.to_string())??;

        if let Ok(entries) = std::fs::read_dir(&addons_dir) {
            for entry in entries.filter_map(Result::ok) {
                let p = entry.path();
                if p.is_file() {
                    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                    if ext == "pak" {
                        added_files.push(p.file_name().unwrap().to_string_lossy().to_string());
                    }
                }
            }
        }
    } else {
        return Err(format!("Unsupported addon format: .{}", extension));
    }
    
    let metadata_path = mod_dir.join("metadata.json");
    if let Ok(content) = std::fs::read_to_string(&metadata_path) {
        if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&content) {
            let addons_obj = parsed.as_object_mut()
                .unwrap()
                .entry("addons")
                .or_insert_with(|| serde_json::json!({}));
            
            if let Some(obj) = addons_obj.as_object_mut() {
                for fname in &added_files {
                    obj.insert(fname.clone(), serde_json::json!({ "enabled": false }));
                }
            }
            
            if let Ok(json_str) = serde_json::to_string_pretty(&parsed) {
                std::fs::write(&metadata_path, json_str).ok();
            }
        }
    }
    
    Ok(added_files)
}

fn get_paks_dir(app_handle: &tauri::AppHandle) -> Result<PathBuf, String> {
    let config = crate::config::load_config(app_handle.clone()).map_err(|e| e.to_string())?;
    let game_path_str = config.game_path.ok_or("Game installation path is not set!")?;
    let mut paks_dir = PathBuf::from(game_path_str);
    if !paks_dir.ends_with("Paks") {
        paks_dir.push("ReadyOrNot");
        paks_dir.push("Content");
        paks_dir.push("Paks");
    }
    Ok(paks_dir)
}

#[tauri::command]
pub async fn toggle_addon(app_handle: tauri::AppHandle, mod_id: String, filename: String, enable: bool) -> Result<(), String> {
    let mods_dir = get_mods_dir(&app_handle)?;
    let mod_dir = resolve_key(&mods_dir, &mod_id)?;
    let addon_path = mod_dir.join("addons").join(&filename);
    
    if !addon_path.exists() {
        return Err(format!("Addon file not found: {}", filename));
    }
    
    let paks_dir = get_paks_dir(&app_handle)?;
    if enable && !paks_dir.exists() {
        std::fs::create_dir_all(&paks_dir).map_err(|e| format!("Failed to create Paks dir: {}", e))?;
    }
    
    let prefix = link_prefix(&mod_id);
    for file in addon_files(&mod_dir.join("addons"), &filename) {
        let link_name = format!("{}addon_{}", prefix, file.file_name().unwrap().to_string_lossy());
        let link_path = paks_dir.join(&link_name);

        if enable {
            if entry_present(&link_path) {
                fs::remove_file(&link_path).map_err(|e| format!("Failed to replace addon link: {}", e))?;
            }
            create_link(&file, &link_path).map_err(|e| format!("Failed to inject addon: {}", e))?;
        } else if entry_present(&link_path) {
            std::fs::remove_file(&link_path).map_err(|e| format!("Failed to remove addon link: {}", e))?;
        }
    }
    
    let metadata_path = mod_dir.join("metadata.json");
    if let Ok(content) = std::fs::read_to_string(&metadata_path) {
        if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(addons_obj) = parsed.get_mut("addons").and_then(|a| a.as_object_mut()) {
                addons_obj.insert(filename.clone(), serde_json::json!({ "enabled": enable }));
            }
            if let Ok(json_str) = serde_json::to_string_pretty(&parsed) {
                std::fs::write(&metadata_path, json_str).ok();
            }
        }
    }
    
    Ok(())
}

fn delete_addon_files(addons_dir: &Path, pak_filename: &str) -> Result<(), String> {
    for file in addon_files(addons_dir, pak_filename) {
        if file.exists() {
            std::fs::remove_file(&file).map_err(|e| format!("Failed to delete addon: {}", e))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_addon(app_handle: tauri::AppHandle, mod_id: String, filename: String) -> Result<(), String> {
    let _ = toggle_addon(app_handle.clone(), mod_id.clone(), filename.clone(), false).await;
    
    let mods_dir = get_mods_dir(&app_handle)?;
    let mod_dir = resolve_key(&mods_dir, &mod_id)?;
    delete_addon_files(&mod_dir.join("addons"), &filename)?;
    
    let metadata_path = mod_dir.join("metadata.json");
    if let Ok(content) = std::fs::read_to_string(&metadata_path) {
        if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(addons_obj) = parsed.get_mut("addons").and_then(|a| a.as_object_mut()) {
                addons_obj.remove(&filename);
            }
            if let Ok(json_str) = serde_json::to_string_pretty(&parsed) {
                std::fs::write(&metadata_path, json_str).ok();
            }
        }
    }
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ron-test-{}-{}", name, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn extract_rar_handles_rar5_and_rar4() {
        for name in ["rar5_small.rar", "rar4_small.rar"] {
            let dest = scratch("rar");
            let archive = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata").join(name);
            extract_rar(&archive, &dest).unwrap();
            assert_eq!(fs::read(dest.join("Mod/a.pak")).unwrap(), b"pak-a ".repeat(200));
            assert_eq!(fs::read(dest.join("Mod/a.ucas")).unwrap(), b"ucas-a ".repeat(200));
            assert_eq!(fs::read(dest.join("Mod/sub/b.utoc")).unwrap(), b"utoc-b ".repeat(50));
        }
    }

    #[test]
    fn extract_rar_rejects_a_non_rar_file() {
        let dir = scratch("rar-junk");
        let junk = dir.join("junk.rar");
        fs::write(&junk, b"not a rar").unwrap();
        let err = extract_rar(&junk, &dir.join("out")).unwrap_err();
        assert!(err.starts_with("Failed to open RAR"), "{err}");
    }

    #[test]
    fn collect_deployables_takes_pak_ucas_utoc_and_skips_addons() {
        let m = scratch("deploy");
        for f in ["a.pak", "a.ucas", "a.UTOC", "readme.txt", "sub/b.pak", "addons/x.pak"] {
            let p = m.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, b"x").unwrap();
        }
        let got: Vec<_> = collect_deployables(&m).iter()
            .map(|p| p.strip_prefix(&m).unwrap().to_string_lossy().replace('\\', "/")).collect();
        assert_eq!(got, ["a.UTOC", "a.pak", "a.ucas", "sub/b.pak"]);
    }

    #[test]
    fn addon_files_pairs_companions_by_stem() {
        let a = scratch("addon");
        for f in ["x.pak", "x.ucas", "x.utoc", "y.ucas"] { fs::write(a.join(f), b"x").unwrap(); }
        let got: Vec<_> = addon_files(&a, "x.pak").iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(got, ["x.pak", "x.ucas", "x.utoc"]);
    }

    #[test]
    fn delete_addon_files_removes_companions_only() {
        let a = scratch("addon-del");
        for f in ["x.pak", "x.ucas", "x.utoc", "y.ucas"] { fs::write(a.join(f), b"x").unwrap(); }
        delete_addon_files(&a, "x.pak").unwrap();
        assert!(!a.join("x.pak").exists());
        assert!(!a.join("x.ucas").exists());
        assert!(!a.join("x.utoc").exists());
        assert!(a.join("y.ucas").exists());
    }

    #[test]
    fn scan_dir_reads_loose_mods_and_packs_and_skips_staging() {
        use crate::packs::{write_manifest, PackManifest};
        let m = scratch("scan");
        fs::create_dir_all(m.join("5543")).unwrap();
        fs::create_dir_all(m.join("notes")).unwrap();
        fs::create_dir_all(m.join(".import-x/Casual-Shitters/1")).unwrap();
        fs::create_dir_all(m.join("Casual-Shitters/3063")).unwrap();
        write_manifest(&m.join("Casual-Shitters"), &PackManifest {
            pack_id: "Casual-Shitters".into(), name: "Casual Shitters".into(),
            version: Some("1.5".into()), mod_ids: vec!["3063".into()], imported_at: 0 }).unwrap();
        let mut keys: Vec<_> = scan_dir(&m).into_iter().map(|i| (i.id, i.pack.map(|p| p.pack_id))).collect();
        keys.sort();
        assert_eq!(keys, [
            ("5543".to_string(), None),
            ("Casual-Shitters/3063".to_string(), Some("Casual-Shitters".to_string())),
            ("notes".to_string(), None),
        ]);
    }

    #[test]
    fn unlink_mod_matches_whole_prefixes_only() {
        let p = scratch("paks");
        for f in ["12_a.pak", "12_addon_b.pak", "123_a.pak", "pakchunk0-Windows.pak",
                  "Casual-Shitters.3063_x.pak", "3063_x.pak"] { fs::write(p.join(f), b"x").unwrap(); }
        assert_eq!(unlink_mod(&p, "12").unwrap(), 2);
        assert_eq!(unlink_mod(&p, "Casual-Shitters/3063").unwrap(), 1);
        let mut left: Vec<_> = fs::read_dir(&p).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, ["123_a.pak", "3063_x.pak", "pakchunk0-Windows.pak"]);
    }

    #[cfg(unix)]
    #[test]
    fn dangling_link_counts_as_present_and_is_removed() {
        let p = scratch("dangling");
        std::os::unix::fs::symlink(p.join("gone.pak"), p.join("5543_gone.pak")).unwrap();
        assert!(entry_present(&p.join("5543_gone.pak")));
        assert_eq!(unlink_mod(&p, "5543").unwrap(), 1);
    }

    #[test]
    fn enable_replaces_a_stale_entry() {
        let m = scratch("mods");
        let p = scratch("paks");
        fs::create_dir_all(m.join("5543")).unwrap();
        fs::write(m.join("5543/a.pak"), b"new").unwrap();
        fs::write(p.join("5543_a.pak"), b"old").unwrap();
        enable_key(&m, &p, "5543").unwrap();
        assert_eq!(fs::read(p.join("5543_a.pak")).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn sweep_orphans_removes_links_into_deleted_folders_only() {
        let m = scratch("mods");
        let p = scratch("paks");
        fs::create_dir_all(m.join("5543")).unwrap();
        fs::write(m.join("5543/a.pak"), b"x").unwrap();
        std::os::unix::fs::symlink(m.join("7/b.pak"), p.join("7_b.pak")).unwrap();
        std::os::unix::fs::symlink(m.join("5543/a.pak"), p.join("5543_a.pak")).unwrap();
        fs::write(p.join("pakchunk0-Windows.pak"), b"x").unwrap();
        assert_eq!(sweep_orphans(&p, &m), 1);
        assert!(!entry_present(&p.join("7_b.pak")));
        assert!(entry_present(&p.join("5543_a.pak")));
        assert!(p.join("pakchunk0-Windows.pak").exists());
    }

    #[test]
    fn enabling_a_pack_copy_disables_the_loose_copy() {
        use crate::packs::{write_manifest, PackManifest};
        let m = scratch("mods");
        let p = scratch("paks");
        fs::create_dir_all(m.join("3063")).unwrap();
        fs::write(m.join("3063/a.pak"), b"loose").unwrap();
        fs::write(m.join("3063/metadata.json"), r#"{"enabled": true}"#).unwrap();
        fs::create_dir_all(m.join("P/3063")).unwrap();
        fs::write(m.join("P/3063/a.pak"), b"packed").unwrap();
        write_manifest(&m.join("P"), &PackManifest {
            pack_id: "P".into(), name: "P".into(), version: None,
            mod_ids: vec!["3063".into()], imported_at: 0 }).unwrap();
        enable_key(&m, &p, "3063").unwrap();
        assert!(entry_present(&p.join("3063_a.pak")));

        enable_key(&m, &p, "P/3063").unwrap();
        assert!(!entry_present(&p.join("3063_a.pak")));
        assert_eq!(fs::read(p.join("P.3063_a.pak")).unwrap(), b"packed");
        let flag = |dir: &str| -> bool {
            let v: Value = serde_json::from_str(&fs::read_to_string(m.join(dir).join("metadata.json")).unwrap()).unwrap();
            v["enabled"].as_bool().unwrap()
        };
        assert!(!flag("3063"));
        assert!(flag("P/3063"));
    }

    #[test]
    fn toggling_a_non_numeric_folder_names_it() {
        let m = scratch("mods");
        let p = scratch("paks");
        fs::create_dir_all(m.join("notes")).unwrap();
        let err = enable_key(&m, &p, "notes").unwrap_err();
        assert!(err.contains("notes"), "{err}");
    }

    #[test]
    fn same_name_files_link_the_shallowest_copy() {
        let m = scratch("mods");
        let p = scratch("paks");
        fs::create_dir_all(m.join("5543/sub")).unwrap();
        fs::write(m.join("5543/a.pak"), b"root").unwrap();
        fs::write(m.join("5543/sub/a.pak"), b"deep").unwrap();
        enable_key(&m, &p, "5543").unwrap();
        let names: Vec<_> = fs::read_dir(&p).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(names, ["5543_a.pak"]);
        assert_eq!(fs::read(p.join("5543_a.pak")).unwrap(), b"root");
    }

    #[cfg(unix)]
    #[test]
    fn failed_enable_leaves_nothing_linked() {
        let m = scratch("mods");
        let p = scratch("paks");
        fs::create_dir_all(m.join("5543")).unwrap();
        fs::write(m.join("5543/a.pak"), b"x").unwrap();
        // link name is 5 bytes longer than the file name, so it exceeds the 255 byte limit
        let long = format!("{}.pak", "b".repeat(250));
        fs::write(m.join("5543").join(&long), b"x").unwrap();
        let err = enable_key(&m, &p, "5543").unwrap_err();
        assert!(err.contains("Failed to inject mod"), "{err}");
        assert_eq!(fs::read_dir(&p).unwrap().count(), 0);
    }

    #[test]
    fn enable_links_enabled_addons_under_pack_key() {
        use crate::packs::{write_manifest, PackManifest};
        let m = scratch("mods");
        let p = scratch("paks");
        fs::create_dir_all(m.join("P/3063/addons")).unwrap();
        write_manifest(&m.join("P"), &PackManifest {
            pack_id: "P".into(), name: "P".into(), version: None,
            mod_ids: vec!["3063".into()], imported_at: 0 }).unwrap();
        fs::write(m.join("P/3063/a.pak"), b"x").unwrap();
        fs::write(m.join("P/3063/addons/x.pak"), b"x").unwrap();
        fs::write(m.join("P/3063/addons/x.ucas"), b"x").unwrap();
        fs::write(m.join("P/3063/metadata.json"), r#"{"addons": {"x.pak": {"enabled": true}}}"#).unwrap();
        enable_key(&m, &p, "P/3063").unwrap();
        let mut got: Vec<_> = fs::read_dir(&p).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        got.sort();
        assert_eq!(got, ["P.3063_a.pak", "P.3063_addon_x.pak", "P.3063_addon_x.ucas"]);
    }

    fn make_pack_zip(path: &Path, preset: serde_json::Value, files: &[(&str, &[u8])]) {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("preset.json", options).unwrap();
        zip.write_all(preset.to_string().as_bytes()).unwrap();
        for (name, bytes) in files {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    fn put(dir: &Path, rel: &str, bytes: &[u8]) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut out: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        out.sort();
        out
    }

    fn tree(dir: &Path) -> Vec<String> {
        fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
            for e in fs::read_dir(dir).unwrap().map(|e| e.unwrap()) {
                let p = e.path();
                let md = p.symlink_metadata().unwrap();
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
                if md.is_dir() {
                    out.push(format!("{}/", rel));
                    walk(base, &p, out);
                } else {
                    out.push(format!("{} {}", rel, md.len()));
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    fn no_staging_left(m: &Path) -> bool {
        names(m).iter().all(|n| !n.starts_with(".import-"))
    }

    fn keys(refs: &[ModRef]) -> Vec<String> {
        refs.iter().map(|r| r.key.clone()).collect()
    }

    fn cs_v15_zip(path: &Path) {
        make_pack_zip(path, serde_json::json!({"name": "CS v1.5", "mod_ids": ["3063", "4296"]}), &[
            ("3063/metadata.json", br#"{"version": "1.0"}"#),
            ("3063/old.pak", b"old"),
            ("4296/metadata.json", br#"{"version": "1.0"}"#),
            ("4296/b.pak", b"b"),
        ]);
    }

    fn cs_v16_zip(path: &Path) {
        make_pack_zip(path, serde_json::json!({"name": "CS v1.6", "mod_ids": ["3063", "7000"]}), &[
            ("3063/metadata.json", br#"{"version": "2.0"}"#),
            ("3063/new.pak", b"new"),
            ("7000/metadata.json", br#"{"version": "1.0"}"#),
            ("7000/c.pak", b"c"),
        ]);
    }

    // CS v1.5 installed with both mods enabled, a second pack Other/3063 and loose 5543
    fn upgrade_setup(m: &Path, p: &Path, z: &Path) {
        use crate::packs::{write_manifest, PackManifest};
        cs_v15_zip(&z.join("v15.zip"));
        import_from_archive(&z.join("v15.zip"), m, p).unwrap();
        enable_key(m, p, "CS/3063").unwrap();
        enable_key(m, p, "CS/4296").unwrap();
        put(m, "Other/3063/other.pak", b"o");
        write_manifest(&m.join("Other"), &PackManifest {
            pack_id: "Other".into(), name: "Other".into(), version: None,
            mod_ids: vec!["3063".into()], imported_at: 0 }).unwrap();
        put(m, "5543/a.pak", b"a");
        enable_key(m, p, "5543").unwrap();
        cs_v16_zip(&z.join("v16.zip"));
    }

    #[test]
    fn import_over_flat_library() {
        let (m, p, z) = (scratch("mods"), scratch("paks"), scratch("zips"));
        for id in ["3063", "3151", "5543"] {
            put(&m, &format!("{}/a.pak", id), id.as_bytes());
            put(&m, &format!("{}/metadata.json", id), br#"{"version": "1"}"#);
            enable_key(&m, &p, id).unwrap();
        }
        let zip = z.join("cs.zip");
        make_pack_zip(&zip, serde_json::json!({"name": "CS v1.5", "mod_ids": ["3063", "3151"]}), &[
            ("3063/metadata.json", br#"{"version": "1", "enabled": true, "addons": {"x.pak": {"enabled": true}}}"#),
            ("3063/a.pak", b"packed"),
            ("3063/addons/x.pak", b"x"),
            ("3151/metadata.json", br#"{"version": "1", "enabled": true}"#),
            ("3151/a.pak", b"packed"),
        ]);
        let s = import_from_archive(&zip, &m, &p).unwrap();
        assert!(m.join("CS/3063").is_dir());
        assert!(m.join("CS/3151").is_dir());
        assert!(!m.join("3063").exists());
        assert!(!m.join("3151").exists());
        assert_eq!(fs::read(m.join("5543/a.pak")).unwrap(), b"5543");
        assert!(names(&p).iter().all(|n| !n.starts_with("3063_") && !n.starts_with("3151_")));
        assert!(entry_present(&p.join("5543_a.pak")));
        assert_eq!(s.superseded_ids, ["3063", "3151"]);
        for id in ["3063", "3151"] {
            assert_eq!(read_json(&m.join("CS").join(id).join("metadata.json"))["enabled"], false);
        }
        assert_eq!(read_json(&m.join("CS/3063/metadata.json"))["addons"], serde_json::json!({"x.pak": {"enabled": true}}));
        assert!(no_staging_left(&m));
    }

    #[test]
    fn upgrade_replaces_only_this_pack() {
        let (m, p, z) = (scratch("mods"), scratch("paks"), scratch("zips"));
        upgrade_setup(&m, &p, &z);
        let s = import_from_archive(&z.join("v16.zip"), &m, &p).unwrap();
        assert!(!m.join("CS/4296").exists());
        let paks: Vec<_> = names(&m.join("CS/3063")).into_iter().filter(|n| n.ends_with(".pak")).collect();
        assert_eq!(paks, ["new.pak"]);
        assert!(m.join("CS/7000").is_dir());
        assert_eq!(fs::read(m.join("Other/3063/other.pak")).unwrap(), b"o");
        assert_eq!(fs::read(m.join("5543/a.pak")).unwrap(), b"a");
        assert!(entry_present(&p.join("5543_a.pak")));
        assert!(names(&p).iter().all(|n| !n.starts_with("CS.")));
        assert_eq!(s.removed_keys, ["CS/4296"]);
        assert_eq!(s.added, 1);
        assert_eq!(s.changed, 1);
        assert_eq!(s.keys, ["CS/3063", "CS/7000"]);
        assert_eq!(read_json(&m.join("CS/pack.json"))["version"], "1.6");
        assert!(no_staging_left(&m));
    }

    #[test]
    fn truncated_zip_changes_nothing() {
        let (m, p, z) = (scratch("mods"), scratch("paks"), scratch("zips"));
        upgrade_setup(&m, &p, &z);
        let bytes = fs::read(z.join("v16.zip")).unwrap();
        let cut = z.join("cut.zip");
        fs::write(&cut, &bytes[..bytes.len() / 2]).unwrap();
        let before = (tree(&m), tree(&p));
        assert!(import_from_archive(&cut, &m, &p).is_err());
        assert_eq!((tree(&m), tree(&p)), before);
        assert!(no_staging_left(&m));
    }

    #[test]
    fn reimport_same_zip_is_stable() {
        let (m, p, z) = (scratch("mods"), scratch("paks"), scratch("zips"));
        upgrade_setup(&m, &p, &z);
        put(&m, "7000/c.pak", b"loose");
        let zip = z.join("v16.zip");
        import_from_archive(&zip, &m, &p).unwrap();
        let count = names(&m).len();
        let again = import_from_archive(&zip, &m, &p).unwrap();
        assert!(again.removed_keys.is_empty() && again.superseded_ids.is_empty());
        assert_eq!((again.added, again.changed), (0, 0));
        let v = inspect_archive(&zip, &m).unwrap();
        assert!(v.added.is_empty() && v.removed.is_empty() && v.changed.is_empty() && v.superseded.is_empty(), "{v:?}");
        assert_eq!(v.installed_version.as_deref(), Some("1.6"));
        assert_eq!(names(&m).len(), count);
        assert!(no_staging_left(&m));
    }

    #[cfg(unix)]
    #[test]
    fn failed_delete_of_old_pack_is_reported() {
        use std::os::unix::fs::PermissionsExt;
        let (m, p, z) = (scratch("mods"), scratch("paks"), scratch("zips"));
        upgrade_setup(&m, &p, &z);
        put(&m, "CS/4296/sub/locked.pak", b"x");
        fs::set_permissions(m.join("CS/4296/sub"), fs::Permissions::from_mode(0o500)).unwrap();
        let s = import_from_archive(&z.join("v16.zip"), &m, &p);
        let staging: Vec<_> = names(&m).into_iter().filter(|n| n.starts_with(".import-")).collect();
        for n in &staging {
            let sub = m.join(n).join("old-CS/4296/sub");
            if sub.exists() {
                fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        if m.join("CS/4296/sub").exists() {
            fs::set_permissions(m.join("CS/4296/sub"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let s = s.unwrap();
        assert!(!s.failed_deletes.is_empty());
        assert_eq!(staging.len(), 1);
        assert_eq!(read_json(&m.join("CS/pack.json"))["version"], "1.6");
        assert!(m.join("CS/7000").is_dir());
        assert!(!m.join("CS/4296").exists());
    }

    #[test]
    fn pack_holds_folders_present() {
        let (m, p, z) = (scratch("mods"), scratch("paks"), scratch("zips"));
        let zip = z.join("n.zip");
        make_pack_zip(&zip, serde_json::json!({"name": "N", "mod_ids": ["1", "2", "3"]}), &[
            ("2/a.pak", b"a"), ("3/a.pak", b"a"), ("4/a.pak", b"a"),
        ]);
        import_from_archive(&zip, &m, &p).unwrap();
        assert_eq!(read_json(&m.join("N/pack.json"))["mod_ids"], serde_json::json!(["2", "3", "4"]));
    }

    #[test]
    fn non_numeric_loose_folder_is_left_alone() {
        let (m, p, z) = (scratch("mods"), scratch("paks"), scratch("zips"));
        put(&m, "notes/readme.txt", b"keep");
        cs_v15_zip(&z.join("v15.zip"));
        import_from_archive(&z.join("v15.zip"), &m, &p).unwrap();
        assert_eq!(fs::read(m.join("notes/readme.txt")).unwrap(), b"keep");
        assert_eq!(names(&m), ["CS", "notes"]);
    }

    #[test]
    fn inspect_lists_removed_and_superseded() {
        let (m, p, z) = (scratch("mods"), scratch("paks"), scratch("zips"));
        upgrade_setup(&m, &p, &z);
        put(&m, "7000/c.pak", b"loose");
        let before = tree(&m);
        let v = inspect_archive(&z.join("v16.zip"), &m).unwrap();
        assert_eq!(v.pack.pack_id, "CS");
        assert_eq!(v.pack.version.as_deref(), Some("1.6"));
        assert_eq!(v.installed_version.as_deref(), Some("1.5"));
        assert_eq!(keys(&v.removed), ["CS/4296"]);
        assert_eq!(keys(&v.added), ["CS/7000"]);
        assert_eq!(keys(&v.changed), ["CS/3063"]);
        assert_eq!(keys(&v.superseded), ["7000"]);
        assert_eq!(tree(&m), before);
    }

    #[test]
    fn import_rejects_non_numeric_top_folder_and_a_non_pack_folder_in_the_way() {
        let (m, p, z) = (scratch("mods"), scratch("paks"), scratch("zips"));
        let bad = z.join("bad.zip");
        make_pack_zip(&bad, serde_json::json!({"name": "CS"}), &[("3063/a.pak", b"a"), ("Stuff/a.pak", b"a")]);
        let err = import_from_archive(&bad, &m, &p).unwrap_err();
        assert!(err.contains("Stuff"), "{err}");
        make_pack_zip(&bad, serde_json::json!({"name": "CS"}), &[("3063/a.pak", b"a"), ("3063/../Stuff/a.pak", b"a")]);
        assert!(import_from_archive(&bad, &m, &p).is_err());
        assert!(inspect_archive(&bad, &m).is_err());
        put(&m, "CS/readme.txt", b"mine");
        cs_v15_zip(&z.join("v15.zip"));
        assert!(import_from_archive(&z.join("v15.zip"), &m, &p).is_err());
        assert_eq!(fs::read(m.join("CS/readme.txt")).unwrap(), b"mine");
        assert_eq!(names(&m), ["CS"]);
    }

    #[cfg(unix)]
    #[test]
    fn failed_rename_aside_restores_links() {
        use std::os::unix::fs::PermissionsExt;
        let (m, p, z) = (scratch("mods"), scratch("paks"), scratch("zips"));
        upgrade_setup(&m, &p, &z);
        let before = (tree(&m), tree(&p));
        // moving a folder to a new parent needs write access to it, to update its ".." entry
        fs::set_permissions(m.join("CS"), fs::Permissions::from_mode(0o500)).unwrap();
        let r = import_from_archive(&z.join("v16.zip"), &m, &p);
        fs::set_permissions(m.join("CS"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(r.unwrap_err(), "Close Ready or Not and try again");
        assert_eq!((tree(&m), tree(&p)), before);
        assert!(no_staging_left(&m));
    }

    fn cs_pack(m: &Path) {
        use crate::packs::{write_manifest, PackManifest};
        put(m, "CS/3311/addons/x.pak", b"x");
        put(m, "CS/3311/a.pak", b"a");
        write_manifest(&m.join("CS"), &PackManifest {
            pack_id: "CS".into(), name: "Casual Shitters".into(), version: Some("1.5".into()),
            mod_ids: vec!["3311".into()], imported_at: 0 }).unwrap();
    }

    #[test]
    fn apply_disables_by_folder_not_flag() {
        let m = scratch("apply-mods");
        let p = scratch("apply-paks");
        put(&m, "5543/a.pak", b"a");
        put(&m, "5543/metadata.json", br#"{"enabled": false}"#);
        put(&m, "3063/b.pak", b"b");
        fs::create_dir_all(&p).unwrap();
        create_link(&m.join("5543/a.pak"), &p.join("5543_a.pak")).unwrap();

        let s = apply_keys(&m, &p, &["3063".to_string()]).unwrap();
        assert!(names(&p).iter().all(|n| !n.starts_with("5543_")));
        assert!(names(&p).iter().any(|n| n.starts_with("3063_")));
        assert!(s.disabled >= 1);
        assert_eq!((s.enabled, s.missing), (1, 0));
    }

    #[test]
    fn apply_counts_missing_keys() {
        let m = scratch("apply-missing-mods");
        let p = scratch("apply-missing-paks");
        fs::create_dir_all(&m).unwrap();
        let s = apply_keys(&m, &p, &["CS/9999".to_string()]).unwrap();
        assert_eq!((s.enabled, s.missing), (0, 1));
    }

    #[test]
    fn exported_zip_has_no_double_slash() {
        let m = scratch("export-mods");
        let out = scratch("export-out").join("cs.zip");
        cs_pack(&m);
        let (json, folders) = export_plan(&m, &Default::default(), "pack:CS", None).unwrap();
        assert_eq!(json["version"], "1.5");
        assert_eq!(json["mod_ids"], serde_json::json!(["3311"]));
        write_pack_zip(&out, &json, &folders).unwrap();
        let zip = zip::ZipArchive::new(fs::File::open(&out).unwrap()).unwrap();
        let entries: Vec<_> = zip.file_names().map(String::from).collect();
        assert!(entries.iter().all(|n| !n.contains("//")), "{:?}", entries);
        assert!(entries.contains(&"3311/addons/x.pak".to_string()));
    }

    #[test]
    fn export_then_import_round_trips_identity() {
        use crate::packs::read_manifest;
        let m = scratch("round-mods");
        let out = scratch("round-out").join("cs.zip");
        cs_pack(&m);
        let (json, folders) = export_plan(&m, &Default::default(), "pack:CS", Some("1.6".into())).unwrap();
        write_pack_zip(&out, &json, &folders).unwrap();

        let m2 = scratch("round-mods2");
        let p2 = scratch("round-paks2");
        fs::create_dir_all(&m2).unwrap();
        import_from_archive(&out, &m2, &p2).unwrap();
        let man = read_manifest(&m2.join("CS")).unwrap();
        assert_eq!(man.pack_id, "CS");
        assert_eq!(man.version, Some("1.6".to_string()));
    }

    #[test]
    fn export_refuses_shared_nexus_ids() {
        let m = scratch("export-collide");
        let mut presets = std::collections::HashMap::new();
        presets.insert("mine".to_string(), vec!["3063".to_string(), "CS/3063".to_string()]);
        let e = export_plan(&m, &presets, "mine", None).unwrap_err();
        assert_eq!(e, "Two mods in this preset share Nexus id 3063");
    }
}
