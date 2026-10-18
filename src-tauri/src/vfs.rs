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

#[tauri::command]
pub async fn apply_preset(app_handle: tauri::AppHandle, name: String, mod_ids: Vec<String>) -> Result<(), String> {
    let all_mods = scan_local_mods(app_handle.clone()).await?;
    let preset_set: std::collections::HashSet<String> = mod_ids.iter().cloned().collect();
    
    for m in &all_mods {
        if !preset_set.contains(&m.id) && m.enabled {
            toggle_mod(app_handle.clone(), m.id.clone(), false).await?;
        }
    }
    
    for id in mod_ids {
        toggle_mod(app_handle.clone(), id, true).await?;
    }

    let mut config = crate::config::load_config(app_handle.clone())?;
    config.active_preset = Some(name);
    crate::config::save_config(app_handle, config)?;
    
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
                let dir_prefix = format!("{}/", new_prefix);
                writer.add_directory(&dir_prefix, options).map_err(|e| e.to_string())?;
                zip_dir(&path, &dir_prefix, writer, options)?;
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

#[tauri::command]
pub async fn export_preset(app_handle: tauri::AppHandle, preset_name: String) -> Result<(), String> {
    let config = crate::config::load_config(app_handle.clone())?;
    let mod_ids = config.presets.get(&preset_name).ok_or("Preset not found")?;
    
    let result = rfd::AsyncFileDialog::new()
        .set_title("Export Preset Zip Archive")
        .add_filter("zip", &["zip"])
        .set_file_name(&format!("{}.zip", preset_name))
        .save_file()
        .await;
        
    let save_path = match result {
        Some(f) => f.path().to_path_buf(),
        None => return Err("Export cancelled".to_string()),
    };
    
    let mods_dir = get_mods_dir(&app_handle)?;
    
    let file = std::fs::File::create(&save_path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);
        
    let preset_json_content = serde_json::json!({
        "name": preset_name,
        "mod_ids": mod_ids
    }).to_string();
    
    zip.start_file("preset.json", options.clone()).map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut zip, preset_json_content.as_bytes()).map_err(|e| e.to_string())?;
    
    for mod_id in mod_ids {
        let mod_dir = mods_dir.join(mod_id);
        if !mod_dir.exists() { continue; }
        
        let prefix = format!("{}/", mod_id);
        zip.add_directory(&prefix, options.clone()).map_err(|e| e.to_string())?;
        zip_dir(&mod_dir, &mod_id, &mut zip, options.clone())?;
    }
    
    zip.finish().map_err(|e| format!("Failed to complete zip: {}", e))?;
    
    Ok(())
}

#[tauri::command]
pub async fn import_preset(app_handle: tauri::AppHandle) -> Result<String, String> {
    let result = rfd::AsyncFileDialog::new()
        .set_title("Select Preset Zip Archive")
        .add_filter("zip", &["zip"])
        .pick_file()
        .await;
        
    let archive_path = match result {
        Some(f) => f.path().to_path_buf(),
        None => return Err("Import cancelled".to_string()),
    };
    
    let mods_dir = get_mods_dir(&app_handle)?;
    
    let file = std::fs::File::open(&archive_path).map_err(|e| format!("Failed to open zip: {}", e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Invalid zip archive: {}", e))?;
    
    let mut preset_json_content = String::new();
    
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        if file.name() == "preset.json" {
            std::io::Read::read_to_string(&mut file, &mut preset_json_content).ok();
            break;
        }
    }
    
    if preset_json_content.is_empty() {
        return Err("Invalid preset archive: missing preset.json".to_string());
    }
    
    let preset: serde_json::Value = serde_json::from_str(&preset_json_content).map_err(|e| format!("Bad preset.json: {}", e))?;
    let name = preset.get("name").and_then(|n| n.as_str()).unwrap_or("Imported Preset").to_string();
    let mod_ids: Vec<String> = preset.get("mod_ids")
        .and_then(|ids| ids.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
        
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        let name_str = file.name().to_string();
        if name_str == "preset.json" || name_str.starts_with("__MACOSX") {
            continue;
        }
        
        let outpath = match file.enclosed_name() {
            Some(path) => mods_dir.join(path),
            None => continue,
        };
        
        if file.is_dir() {
            std::fs::create_dir_all(&outpath).map_err(|e| format!("Failed to create directory in preset: {}", e))?;
        } else {
            if let Some(p) = outpath.parent() {
                std::fs::create_dir_all(p).map_err(|e| format!("Failed to create parent directory for {}: {}", outpath.display(), e))?;
            }
            let mut outfile = std::fs::File::create(&outpath).map_err(|e| format!("Failed to create file {}: {}", outpath.display(), e))?;
            std::io::copy(&mut file, &mut outfile).map_err(|e| format!("Failed to extract file {}: {}", outpath.display(), e))?;
        }
    }
    
    let mut config = crate::config::load_config(app_handle.clone())?;
    config.presets.insert(name.clone(), mod_ids);
    crate::config::save_config(app_handle, config)?;
    
    Ok(name)
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
}
