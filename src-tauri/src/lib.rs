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
        // A second instance would sweep the first one's staging folders, so it focuses the first and exits
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            // Runs before the window loads, so before the first scan and before any op can stage
            // Listed now, removed off the main thread so a large leftover does not delay the window
            if let Ok(mods_dir) = vfs::get_mods_dir(app.handle()) {
                let leftovers = vfs::leftover_staging(&mods_dir);
                if !leftovers.is_empty() {
                    std::thread::spawn(move || vfs::remove_staging(&leftovers));
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let Ok(mods_dir) = vfs::get_mods_dir(app) else { return };
                if let Some(blocked) = publish::close_blocked(&mods_dir) {
                    api.prevent_close();
                    let _ = window.emit("close-blocked", blocked);
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
            subscribe::cancel_pack_op,
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
