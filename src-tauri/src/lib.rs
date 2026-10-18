mod api;
mod config;
mod drive;
mod google;
mod hashing;
mod packs;
mod publish;
mod sso;
mod subscribe;
mod updater;
mod vfs;

use tauri::{Emitter, Manager};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let Ok(mods_dir) = vfs::get_mods_dir(app) else { return };
                let dirty = publish::dirty_packs(&mods_dir);
                if !dirty.is_empty() {
                    api.prevent_close();
                    let _ = window.emit("close-blocked", dirty);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            sso::authenticate_nexus,
            config::load_config,
            config::save_config,
            config::pick_game_folder,
            config::pick_storage_folder,
            config::get_default_storage_path,
            config::auto_detect_game_path,
            config::get_api_key,
            google::google_status,
            google::google_sign_in,
            google::google_sign_out,
            publish::publish_pack_cmd,
            publish::list_dirty_packs,
            publish::force_close,
            subscribe::inspect_pack_link,
            subscribe::add_pack_from_link,
            subscribe::check_pack_updates,
            subscribe::update_linked_pack,
            subscribe::verify_linked_pack,
            subscribe::redownload_mods,
            config::save_preset,
            config::delete_preset,
            config::load_presets,
            api::fetch_mod_metadata,
            api::open_browser_url,
            api::open_game_path,
            api::launch_game,
            api::fetch_trending_mods,
            api::fetch_latest_mods,
            api::search_nexus_mods,
            api::download_online_mod,
            vfs::scan_local_mods,
            vfs::refresh_installed_mod_updates,
            vfs::install_mod_archive,
            vfs::toggle_mod,
            vfs::pick_mod_archive,
            vfs::delete_mod,
            vfs::move_to_pack,
            vfs::remove_from_pack,
            vfs::apply_preset,
            vfs::load_packs,
            vfs::delete_pack,
            vfs::fork_pack,
            vfs::get_pack_size,
            vfs::export_preset,
            vfs::pick_preset_archive,
            vfs::inspect_preset_archive,
            vfs::import_preset,
            vfs::toggle_all_mods,
            vfs::install_addon,
            vfs::toggle_addon,
            vfs::remove_addon,
            updater::check_for_app_update,
            updater::install_app_update
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
