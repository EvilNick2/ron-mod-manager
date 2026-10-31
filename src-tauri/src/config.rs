use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct AppConfig {
    pub game_path: Option<String>,
    pub game_app_id: Option<u64>,
    pub game_domain: Option<String>,
    pub launch_options: Option<String>,
    pub mod_storage_path: Option<String>,
    pub enable_blur: Option<bool>,
    pub active_preset: Option<String>,
    #[serde(default)]
    pub presets: HashMap<String, Vec<String>>,
    /// None means on
    #[serde(default)]
    pub report_pack_use: Option<bool>,
    #[serde(default)]
    pub last_reported_play: Option<u64>,
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
    write_config_file(&path, &config)
}

// A load during a save must see a whole file, because load_config turns a parse failure into defaults.
// The temp name is unique so two saves at once cannot write into the same file.
fn write_config_file(path: &Path, config: &AppConfig) -> Result<(), String> {
    let json = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    fs::write(&tmp, json).and_then(|_| fs::rename(&tmp, path)).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })
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
    let steam_paths = crate::usage::steam_roots();

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

pub fn tidy_presets_after_import(config: &mut AppConfig, pack_id: &str, superseded_ids: &[String], removed_keys: &[String]) {
    use crate::packs::pack_identity;
    let mut removed: Vec<String> = Vec::new();
    let pack_prefix = format!("{}/", pack_id);
    config.presets.retain(|name, keys| {
        let own = pack_identity(&serde_json::json!({ "name": name })).pack_id == pack_id
            && keys.iter().all(|k| superseded_ids.contains(k) || k.starts_with(&pack_prefix));
        if own {
            removed.push(name.clone());
        }
        !own
    });
    for (name, keys) in config.presets.iter_mut() {
        let mut out: Vec<String> = Vec::with_capacity(keys.len());
        for key in keys.drain(..) {
            if removed_keys.contains(&key) {
                continue;
            }
            let key = if superseded_ids.contains(&key) { format!("{}/{}", pack_id, key) } else { key };
            if !out.contains(&key) {
                out.push(key);
            }
        }
        *keys = out;
        if keys.is_empty() {
            removed.push(name.clone());
        }
    }
    config.presets.retain(|_, keys| !keys.is_empty());
    if config.active_preset.as_ref().is_some_and(|a| removed.contains(a)) {
        config.active_preset = None;
    }
}

// After a pack update, drops the keys of removed mods and leaves every preset and the active one in place
pub fn strip_keys_from_presets(config: &mut AppConfig, removed_keys: &[String]) {
    for keys in config.presets.values_mut() {
        keys.retain(|k| !removed_keys.contains(k));
    }
}

// The pack shown as active follows it to its new id after a switch
pub fn move_active_pack(config: &mut AppConfig, old_pack_id: &str, new_pack_id: &str) {
    if config.active_preset.as_deref() == Some(format!("pack:{}", old_pack_id).as_str()) {
        config.active_preset = Some(format!("pack:{}", new_pack_id));
    }
}

// Preset keys of the old pack point at the new one where it has that mod, and are dropped where it does not
pub fn rename_pack_in_presets(config: &mut AppConfig, old_pack_id: &str, new_pack_id: &str, new_keys: &[String]) {
    let old_prefix = format!("{}/", old_pack_id);
    let mut emptied: Vec<String> = Vec::new();
    for (name, keys) in config.presets.iter_mut() {
        let mut out: Vec<String> = Vec::with_capacity(keys.len());
        for key in keys.drain(..) {
            let key = match key.strip_prefix(&old_prefix) {
                Some(id) => format!("{}/{}", new_pack_id, id),
                None => key,
            };
            let gone = key.starts_with(&format!("{}/", new_pack_id)) && !new_keys.contains(&key);
            if !gone && !out.contains(&key) {
                out.push(key);
            }
        }
        *keys = out;
        if keys.is_empty() {
            emptied.push(name.clone());
        }
    }
    config.presets.retain(|_, keys| !keys.is_empty());
    if config.active_preset.as_ref().is_some_and(|a| emptied.contains(a)) {
        config.active_preset = None;
    }
}

pub fn rename_key_in_presets(config: &mut AppConfig, old: &str, new: &str) {
    for keys in config.presets.values_mut() {
        let mut out: Vec<String> = Vec::with_capacity(keys.len());
        for key in keys.drain(..) {
            let key = if key == old { new.to_string() } else { key };
            if !out.contains(&key) {
                out.push(key);
            }
        }
        *keys = out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ron-config-{}-{}", name, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names_in(dir: &Path) -> Vec<String> {
        fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect()
    }

    #[test]
    fn saving_replaces_the_file_and_leaves_no_temp() {
        let dir = scratch("replace");
        let path = dir.join("config.json");
        let mut c = AppConfig { report_pack_use: Some(false), ..Default::default() };
        write_config_file(&path, &c).unwrap();
        c.last_reported_play = Some(5);
        write_config_file(&path, &c).unwrap();
        assert_eq!(names_in(&dir), ["config.json"]);
        let read: AppConfig = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!((read.report_pack_use, read.last_reported_play), (Some(false), Some(5)));
    }

    #[test]
    fn a_failed_save_leaves_no_temp() {
        let dir = scratch("failed");
        let target = dir.join("config.json");
        fs::create_dir(&target).unwrap();
        assert!(write_config_file(&target, &AppConfig::default()).is_err());
        assert_eq!(names_in(&dir), ["config.json"]);
    }

    #[test]
    fn a_load_during_saves_never_sees_a_partial_file() {
        let dir = scratch("race");
        let path = dir.join("config.json");
        let mut c = AppConfig::default();
        c.presets.insert("Mine".into(), (0..5000).map(|n| n.to_string()).collect());
        write_config_file(&path, &c).unwrap();
        // Two writers, as the background report and the frontend can be
        let writers: Vec<_> = (0..2)
            .map(|_| {
                let (path, c) = (path.clone(), c.clone());
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        write_config_file(&path, &c).unwrap();
                    }
                })
            })
            .collect();
        while !writers.iter().all(|w| w.is_finished()) {
            let text = fs::read_to_string(&path).unwrap();
            let read: AppConfig = serde_json::from_str(&text).expect("a load must never see a partial file");
            assert_eq!(read.presets["Mine"].len(), 5000);
        }
        for w in writers {
            w.join().unwrap();
        }
        assert_eq!(names_in(&dir), ["config.json"]);
    }

    #[test]
    fn tidy_presets_after_import_cases() {
        let mut c = AppConfig::default();
        c.presets.insert("Casual Shitters v1.5".into(), vec!["3063".into(), "3151".into()]);
        c.presets.insert("Mine".into(), vec!["3063".into(), "5543".into(), "Casual-Shitters/4296".into()]);
        c.presets.insert("Only Removed".into(), vec!["Casual-Shitters/4296".into()]);
        c.active_preset = Some("Only Removed".into());
        tidy_presets_after_import(&mut c, "Casual-Shitters", &["3063".into(), "3151".into()], &["Casual-Shitters/4296".into()]);
        assert!(!c.presets.contains_key("Casual Shitters v1.5"));
        assert_eq!(c.presets["Mine"], ["Casual-Shitters/3063", "5543"]);
        assert!(!c.presets.contains_key("Only Removed"));
        assert_eq!(c.active_preset, None);
    }

    #[test]
    fn strip_keys_keeps_an_active_own_named_preset() {
        let mut c = AppConfig::default();
        c.presets.insert("Casual Shitters".into(), vec!["Casual-Shitters/3063".into(), "Casual-Shitters/3311".into()]);
        c.presets.insert("Mine".into(), vec!["5543".into(), "Casual-Shitters/3063".into()]);
        c.active_preset = Some("Casual Shitters".into());
        strip_keys_from_presets(&mut c, &["Casual-Shitters/3063".into()]);
        assert_eq!(c.presets["Casual Shitters"], ["Casual-Shitters/3311"]);
        assert_eq!(c.presets["Mine"], ["5543"]);
        assert_eq!(c.active_preset.as_deref(), Some("Casual Shitters"));
    }

    #[test]
    fn rename_key_in_presets_rewrites_every_preset() {
        let mut c = AppConfig::default();
        c.presets.insert("A".into(), vec!["1".into(), "7000".into(), "2".into()]);
        c.presets.insert("B".into(), vec!["7000".into(), "CS/7000".into()]);
        c.presets.insert("C".into(), vec!["5".into()]);
        c.active_preset = Some("A".into());
        rename_key_in_presets(&mut c, "7000", "CS/7000");
        assert_eq!(c.presets["A"], ["1", "CS/7000", "2"]);
        assert_eq!(c.presets["B"], ["CS/7000"]);
        assert_eq!(c.presets["C"], ["5"]);
        assert_eq!(c.active_preset.as_deref(), Some("A"));
    }

    #[test]
    fn move_active_pack_moves_only_that_pack() {
        let mut c = AppConfig::default();
        c.active_preset = Some("pack:Old".into());
        move_active_pack(&mut c, "Other", "New");
        assert_eq!(c.active_preset.as_deref(), Some("pack:Old"));
        move_active_pack(&mut c, "Old", "New");
        assert_eq!(c.active_preset.as_deref(), Some("pack:New"));
        c.active_preset = Some("Old".into());
        move_active_pack(&mut c, "Old", "New");
        assert_eq!(c.active_preset.as_deref(), Some("Old"));
    }

    #[test]
    fn rename_pack_in_presets_cases() {
        let mut c = AppConfig::default();
        c.presets.insert("Mine".into(), vec!["5543".into(), "Old/1".into(), "Old/2".into(), "New/1".into()]);
        c.presets.insert("Gone".into(), vec!["Old/9".into()]);
        c.active_preset = Some("Gone".into());
        rename_pack_in_presets(&mut c, "Old", "New", &["New/1".into(), "New/3".into()]);
        assert_eq!(c.presets["Mine"], ["5543", "New/1"]);
        assert!(!c.presets.contains_key("Gone"));
        assert_eq!(c.active_preset, None);
    }

    #[test]
    fn tidy_keeps_an_own_named_preset_with_other_mods() {
        let mut c = AppConfig::default();
        c.presets.insert("Casual Shitters v1.4".into(), vec!["3063".into(), "Casual-Shitters/4296".into()]);
        c.presets.insert("Casual Shitters".into(), vec!["3063".into(), "5543".into()]);
        c.active_preset = Some("Casual Shitters".into());
        tidy_presets_after_import(&mut c, "Casual-Shitters", &["3063".into()], &[]);
        assert!(!c.presets.contains_key("Casual Shitters v1.4"));
        assert_eq!(c.presets["Casual Shitters"], ["Casual-Shitters/3063", "5543"]);
        assert_eq!(c.active_preset.as_deref(), Some("Casual Shitters"));
    }
}
