use std::fs;
use std::path::{Path, PathBuf};
use serde::{Serialize, Deserialize};
use tauri::{AppHandle, Manager};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: String,
    pub thumbnail_url: Option<String>,
    pub enabled: bool,
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
        let file_name = path.file_name().ok_or("Invalid file name")?;
        let outpath = target_dir.join(file_name);
        std::fs::copy(&archive_path, &outpath).map_err(|e| format!("Failed to copy .pak file: {}", e))?;
    } else {
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
    
    if let Ok(entries) = std::fs::read_dir(&target_dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("pak") {
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
        }
    }
    
    let metadata_path = target_dir.join("metadata.json");
    if let Ok(content) = std::fs::read_to_string(&metadata_path) {
        if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(obj) = parsed.as_object_mut() {
                obj.insert("enabled".to_string(), serde_json::Value::Bool(enable));
            }
            if let Ok(json_str) = serde_json::to_string_pretty(&parsed) {
                std::fs::write(metadata_path, json_str).ok();
            }
        }
    }
    
    Ok(())
}

#[tauri::command]
pub async fn pick_mod_archive() -> Result<Option<String>, String> {
    let result = rfd::AsyncFileDialog::new()
        .set_title("Select Mod Archive (.zip, .pak)")
        .add_filter("Mod Archives", &["zip", "pak"])
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
pub async fn apply_preset(app_handle: tauri::AppHandle, mod_ids: Vec<String>) -> Result<(), String> {
    let all_mods = scan_local_mods(app_handle.clone()).await?;
    let preset_set: std::collections::HashSet<&str> = mod_ids.iter().map(|s| s.as_str()).collect();
    
    for m in &all_mods {
        let should_enable = preset_set.contains(m.id.as_str());
        if m.enabled && !should_enable {
            toggle_mod(app_handle.clone(), m.id.clone(), false).await?;
        } else if !m.enabled && should_enable {
            toggle_mod(app_handle.clone(), m.id.clone(), true).await?;
        }
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
            std::fs::create_dir_all(&outpath).ok();
        } else {
            if let Some(p) = outpath.parent() {
                std::fs::create_dir_all(p).ok();
            }
            if let Ok(mut outfile) = std::fs::File::create(&outpath) {
                std::io::copy(&mut file, &mut outfile).ok();
            }
        }
    }
    
    let mut config = crate::config::load_config(app_handle.clone())?;
    config.presets.insert(name.clone(), mod_ids);
    crate::config::save_config(app_handle, config)?;
    
    Ok(name)
}

