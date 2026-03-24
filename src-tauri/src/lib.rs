mod api;
mod config;
mod sso;
mod vfs;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            sso::authenticate_nexus,
            config::load_config,
            config::save_config,
            config::pick_game_folder,
            config::pick_storage_folder,
            config::get_default_storage_path,
            config::auto_detect_game_path,
            config::get_api_key,
            api::fetch_mod_metadata,
            api::open_browser_url,
            api::fetch_trending_mods,
            api::fetch_latest_mods,
            api::search_nexus_mods,
            api::download_online_mod,
            vfs::scan_local_mods,
            vfs::install_mod_archive,
            vfs::toggle_mod,
            vfs::pick_mod_archive,
            vfs::delete_mod,
            vfs::apply_preset,
            vfs::export_preset,
            vfs::import_preset,
            config::save_preset,
            config::delete_preset,
            config::load_presets
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
