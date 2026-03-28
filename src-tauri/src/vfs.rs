use std::fs;
use std::sync::Arc;
use std::path::{Path, PathBuf};
use serde::{Serialize, Deserialize};
use tauri::{AppHandle, Manager};

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
    pub author: String,
    pub thumbnail_url: Option<String>,
    pub enabled: bool,
    pub addons: Vec<AddonInfo>,
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

#[tauri::command]
pub async fn scan_local_mods(app_handle: tauri::AppHandle) -> Result<Vec<ModInfo>, String> {
    let mods_dir = get_mods_dir(&app_handle)?;
    let mut mods = Vec::new();

    if let Ok(entries) = fs::read_dir(mods_dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            
            if path.is_dir() {
                let id = entry.file_name().to_string_lossy().to_string();
                let metadata_path = path.join("metadata.json");
                
                let mut info = ModInfo {
                    id: id.clone(),
                    name: id.clone(),
                    description: "No metadata found.".to_string(),
                    version: "1.0.0".to_string(),
                    author: "Unknown".to_string(),
                    thumbnail_url: None,
                    enabled: false,
                    addons: Vec::new(),
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
                        if let Some(version) = parsed.get("version").and_then(|v| v.as_str()) {
                            info.version = version.to_string();
                        }
                        
                        let thumb_path = path.join("thumbnail.jpg");
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
                
                let addons_dir = path.join("addons");
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
                
                mods.push(info);
            }
        }
    }
    
    Ok(mods)
}

#[tauri::command]
pub async fn install_mod_archive(app_handle: tauri::AppHandle, archive_path: String, mod_id: String) -> Result<(), String> {
    let mods_dir = get_mods_dir(&app_handle)?;
    let target_dir = mods_dir.join(&mod_id);
    
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
        let media = Arc::new(rar_stream::LocalFileMedia::new(&archive_path).map_err(|e| format!("Failed to open RAR: {}", e))?);
        let pkg = rar_stream::RarFilesPackage::new(vec![media]);
        let files = pkg.parse(rar_stream::ParseOptions::default()).await
            .map_err(|e| format!("Failed to parse RAR: {}", e))?;

        for file in files {
            let outpath = target_dir.join(&file.name);
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent).map_err(|_| "Failed to create directory in RAR")?;
            }
            
            if file.name.ends_with('/') || file.name.ends_with('\\') {
                continue;
            }

            let content = file.read_to_end().await
                .map_err(|e| format!("Failed to read file {}: {}", file.name, e))?;
            
            std::fs::write(&outpath, content)
                .map_err(|e| format!("Failed to write extracted file {}: {}", file.name, e))?;
        }
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

#[tauri::command]
pub async fn toggle_mod(app_handle: tauri::AppHandle, mod_id: String, enable: bool) -> Result<(), String> {
    let mods_dir = get_mods_dir(&app_handle)?;
    let target_dir = mods_dir.join(&mod_id);
    
    let config = crate::config::load_config(app_handle.clone()).map_err(|e| e.to_string())?;
    let game_path_str = config.game_path.ok_or("Game installation path is not set! Please configure it in the Settings tab.")?;
    let game_path = PathBuf::from(game_path_str);
    
    let mut paks_mod_dir = game_path.clone();
    if !paks_mod_dir.ends_with("Paks") {
        paks_mod_dir.push("ReadyOrNot");
        paks_mod_dir.push("Content");
        paks_mod_dir.push("Paks");
    }
    
    if enable && !paks_mod_dir.exists() {
        std::fs::create_dir_all(&paks_mod_dir).map_err(|e| format!("Failed to create Paks directory: {}", e))?;
    }
    
    fn find_paks(dir: &Path, paks: &mut Vec<PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    if path.file_name().and_then(|n| n.to_str()) == Some("addons") {
                        continue;
                    }
                    find_paks(&path, paks);
                } else if path.is_file() {
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                    if ext == "pak" {
                        paks.push(path);
                    }
                }
            }
        }
    }

    let mut paks = Vec::new();
    find_paks(&target_dir, &mut paks);

    for path in paks {
        let file_name = path.file_name().unwrap();
        let link_name = format!("{}_{}", mod_id, file_name.to_string_lossy());
        let link_path = paks_mod_dir.join(&link_name);
        
        if enable {
            if !link_path.exists() {
                create_link(&path, &link_path).map_err(|e| format!("Failed to inject mod: {}", e))?;
            }
        } else {
            if link_path.exists() {
                std::fs::remove_file(&link_path).map_err(|e| format!("Failed to remove injected mod: {}", e))?;
            }
        }
    }
    
    let metadata_path = target_dir.join("metadata.json");
    if let Ok(content) = std::fs::read_to_string(&metadata_path) {
        if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(obj) = parsed.as_object_mut() {
                obj.insert("enabled".to_string(), serde_json::Value::Bool(enable));
            }
            if let Ok(json_str) = serde_json::to_string_pretty(&parsed) {
                std::fs::write(&metadata_path, json_str).ok();
            }
        }
    }
    
    let addons_dir = target_dir.join("addons");
    if addons_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&addons_dir) {
            for entry in entries.filter_map(Result::ok) {
                let ap = entry.path();
                if ap.is_file() {
                    let ext = ap.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                    if ext == "pak" {
                        let fname = ap.file_name().unwrap().to_string_lossy().to_string();
                        let link_name = format!("{}_addon_{}", mod_id, fname);
                        let link_path = paks_mod_dir.join(&link_name);
                        
                        if !enable {
                            if link_path.exists() {
                                std::fs::remove_file(&link_path).ok();
                            }
                        } else {
                            let addon_enabled = if let Ok(content) = std::fs::read_to_string(&metadata_path) {
                                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                                    parsed.get("addons")
                                        .and_then(|a| a.get(&fname))
                                        .and_then(|a| a.get("enabled"))
                                        .and_then(|e| e.as_bool())
                                        .unwrap_or(false)
                                } else { false }
                            } else { false };
                            
                            if addon_enabled && !link_path.exists() {
                                create_link(&ap, &link_path).ok();
                            }
                        }
                    }
                }
            }
        }
    }
    
    Ok(())
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
    let _ = toggle_mod(app_handle.clone(), mod_id.clone(), false).await;
    
    let mods_dir = get_mods_dir(&app_handle)?;
    let target_dir = mods_dir.join(&mod_id);
    
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
    let mod_dir = mods_dir.join(&mod_id);
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
            if ext == "pak" {
                let outpath = addons_dir.join(&fname);
                let mut outfile = std::fs::File::create(&outpath).map_err(|e| format!("Failed to create addon file: {}", e))?;
                std::io::copy(&mut file, &mut outfile).map_err(|e| format!("Failed to extract addon: {}", e))?;
                added_files.push(fname);
            }
        }
    } else if extension == "rar" {
        let media = Arc::new(rar_stream::LocalFileMedia::new(&archive_path)
            .map_err(|e| format!("Failed to open RAR: {}", e))?);
        let pkg = rar_stream::RarFilesPackage::new(vec![media]);
        let files = pkg.parse(rar_stream::ParseOptions::default()).await
            .map_err(|e| format!("Failed to parse RAR: {}", e))?;

        for file in files {
            let outpath = addons_dir.join(&file.name);
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent).map_err(|_| "Failed to create directory in RAR")?;
            }
            
            if file.name.ends_with('/') || file.name.ends_with('\\') {
                continue;
            }

            let content = file.read_to_end().await
                .map_err(|e| format!("Failed to read file {}: {}", file.name, e))?;
            
            std::fs::write(&outpath, content)
                .map_err(|e| format!("Failed to write extracted file {}: {}", file.name, e))?;
        }
        
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
    let mod_dir = mods_dir.join(&mod_id);
    let addon_path = mod_dir.join("addons").join(&filename);
    
    if !addon_path.exists() {
        return Err(format!("Addon file not found: {}", filename));
    }
    
    let paks_dir = get_paks_dir(&app_handle)?;
    if enable && !paks_dir.exists() {
        std::fs::create_dir_all(&paks_dir).map_err(|e| format!("Failed to create Paks dir: {}", e))?;
    }
    
    let link_name = format!("{}_addon_{}", mod_id, filename);
    let link_path = paks_dir.join(&link_name);
    
    if enable {
        if !link_path.exists() {
            create_link(&addon_path, &link_path).map_err(|e| format!("Failed to inject addon: {}", e))?;
        }
    } else {
        if link_path.exists() {
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

#[tauri::command]
pub async fn remove_addon(app_handle: tauri::AppHandle, mod_id: String, filename: String) -> Result<(), String> {
    let _ = toggle_addon(app_handle.clone(), mod_id.clone(), filename.clone(), false).await;
    
    let mods_dir = get_mods_dir(&app_handle)?;
    let addon_path = mods_dir.join(&mod_id).join("addons").join(&filename);
    
    if addon_path.exists() {
        std::fs::remove_file(&addon_path).map_err(|e| format!("Failed to delete addon: {}", e))?;
    }
    
    let metadata_path = mods_dir.join(&mod_id).join("metadata.json");
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
