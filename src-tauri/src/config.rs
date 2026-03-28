use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct AppConfig {
    pub game_path: Option<String>,
    pub mod_storage_path: Option<String>,
    pub enable_blur: Option<bool>,
    pub active_preset: Option<String>,
    #[serde(default)]
    pub presets: HashMap<String, Vec<String>>,
}

pub fn get_config_path(app_handle: &AppHandle) -> Result<PathBuf, String> {
    let mut config_dir = app_handle
        .path()
        .config_dir()
        .map_err(|e| format!("Could not resolve app config directory: {}", e))?;
    
    config_dir.push("ron-mod-manager");
    
    if !config_dir.exists() {
        fs::create_dir_all(&config_dir).map_err(|e| format!("Failed to create config dir: {}", e))?;
    }
    
    Ok(config_dir.join("config.json"))
}

#[tauri::command]
pub fn load_config(app_handle: tauri::AppHandle) -> Result<AppConfig, String> {
    let path = get_config_path(&app_handle)?;
    
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    
    let contents = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let config: AppConfig = serde_json::from_str(&contents).unwrap_or_default();
    
    Ok(config)
}

#[tauri::command]
pub fn save_config(app_handle: tauri::AppHandle, config: AppConfig) -> Result<(), String> {
    let path = get_config_path(&app_handle)?;
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    
    fs::write(path, json).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn save_preset(app_handle: tauri::AppHandle, name: String, mod_ids: Vec<String>) -> Result<(), String> {
    let mut config = load_config(app_handle.clone())?;
    config.presets.insert(name, mod_ids);
    save_config(app_handle, config)
}

#[tauri::command]
pub fn delete_preset(app_handle: tauri::AppHandle, name: String) -> Result<(), String> {
    let mut config = load_config(app_handle.clone())?;
    config.presets.remove(&name);
    save_config(app_handle, config)
}

#[tauri::command]
pub fn load_presets(app_handle: tauri::AppHandle) -> Result<HashMap<String, Vec<String>>, String> {
    let config = load_config(app_handle)?;
    Ok(config.presets)
}

#[tauri::command]
pub fn auto_detect_game_path() -> Result<Option<String>, String> {
    let mut steam_paths = Vec::new();
    
    #[cfg(windows)]
    {
        if let Ok(program_files) = std::env::var("ProgramFiles(x86)") {
            steam_paths.push(PathBuf::from(program_files).join("Steam"));
        }
        if let Ok(program_files) = std::env::var("ProgramFiles") {
            steam_paths.push(PathBuf::from(program_files).join("Steam"));
        }
    }
    
    #[cfg(unix)]
    {
        if let Ok(home) = std::env::var("HOME") {
            steam_paths.push(PathBuf::from(&home).join(".local/share/Steam"));
            steam_paths.push(PathBuf::from(&home).join(".steam/steam"));
            steam_paths.push(PathBuf::from(&home).join(".steam/root"));
            steam_paths.push(PathBuf::from(&home).join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
        }
    }
    
    let mut all_libraries = Vec::new();
    for base_steam in &steam_paths {
        all_libraries.push(base_steam.clone());
        let vdf_path = base_steam.join("steamapps").join("libraryfolders.vdf");
        
        if let Ok(content) = fs::read_to_string(&vdf_path) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("\"path\"") {
                    let parts: Vec<&str> = trimmed.split('"').collect();
                    if parts.len() >= 4 {
                        let lib_path = parts[3];
                        let clean_path = lib_path.replace("\\\\", "\\"); 
                        all_libraries.push(PathBuf::from(clean_path));
                    }
                }
            }
        }
    }
    
    for lib in all_libraries {
        let game_dir = lib.join("steamapps").join("common").join("Ready Or Not");
        if game_dir.exists() {
            return Ok(Some(game_dir.to_string_lossy().to_string()));
        }
    }
    
    Ok(None)
}

#[tauri::command]
pub fn get_api_key() -> Result<Option<String>, String> {
    let entry = keyring::Entry::new("ron-mod-manager", "nexus_api_key").map_err(|e| e.to_string())?;
    match entry.get_password() {
        Ok(pw) => Ok(Some(pw)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub fn save_api_key(api_key: String) -> Result<(), String> {
    let entry = keyring::Entry::new("ron-mod-manager", "nexus_api_key").map_err(|e| e.to_string())?;
    entry.set_password(&api_key).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn pick_game_folder() -> Result<Option<String>, String> {
    let result = rfd::AsyncFileDialog::new()
        .set_title("Select Ready or Not Installation Folder")
        .pick_folder()
        .await;
        
    Ok(result.map(|f| f.path().to_string_lossy().to_string()))
}

#[tauri::command]
pub async fn pick_storage_folder() -> Result<Option<String>, String> {
    let result = rfd::AsyncFileDialog::new()
        .set_title("Select Local Mod Storage Base Folder")
        .pick_folder()
        .await;
        
    Ok(result.map(|f| f.path().to_string_lossy().to_string()))
}

#[tauri::command]
pub fn get_default_storage_path(app_handle: tauri::AppHandle) -> String {
    if let Ok(mut config_dir) = app_handle.path().config_dir() {
        config_dir.push("ron-mod-manager");
        return config_dir.join("mods").to_string_lossy().to_string();
    }
    "".to_string()
}
