mod commands;
mod crypto;
mod encryption;
mod login;
mod native;
mod paths;
mod roblox_api;
mod settings;
mod state;
mod storage;
mod tracking;

use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            app.manage(AppState::new(handle.clone()));

            {
                let state = app.state::<AppState>();
                encryption::init_encryption(&state);
                storage::migrate_account_encryption_to_keychain(&state);
            }
            encryption::prewarm_key(&handle);

            // Best-effort cleanup of orphaned login-profile temp dirs from a
            // crash or force-kill mid-login (see sweep_stale_login_profiles).
            // Blocking I/O, kept off the async runtime's worker threads.
            std::thread::spawn(login::sweep_stale_login_profiles);

            // Paint the UI immediately (window is created with visible:false and
            // shown by the frontend's show_main_window call once DOM is ready);
            // resolve the native helper and grab the mutex in the background so
            // a fast cold start never blocks first paint. The launch path
            // independently awaits start_mutex_holder before every launch, so
            // the mutex is still guaranteed held before any instance launches.
            if cfg!(windows) {
                let handle2 = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let state = handle2.state::<AppState>();
                    native::start_mutex_holder(&handle2, &state).await;
                });
            }
            let auto_afk = settings::load_settings()
                .get("antiAfk")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if auto_afk {
                let handle3 = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let state = handle3.state::<AppState>();
                    native::start_antiafk(&handle3, &state).await;
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::show_main_window,
            commands::settings_load,
            commands::settings_save,
            commands::enc_status,
            commands::enc_unlock,
            commands::enc_set_key,
            commands::multiinstance_status,
            commands::antiafk_status,
            commands::accounts_load,
            commands::accounts_add,
            commands::accounts_remove,
            commands::accounts_update,
            commands::accounts_reorder,
            commands::packages_load,
            commands::packages_save,
            commands::genhistory_read,
            commands::genhistory_write,
            commands::genhistory_clear,
            commands::fflag_read,
            commands::fflag_write,
            commands::fps_read,
            commands::fps_write,
            commands::roblox_get_version,
            commands::roblox_validate_cookie,
            commands::roblox_set_volume,
            commands::roblox_kill_all,
            commands::roblox_kill_one,
            commands::roblox_running_count,
            commands::roblox_watched_ids,
            commands::roblox_trim_memory,
            commands::roblox_trim_account_memory,
            commands::roblox_get_game_name,
            commands::roblox_get_json,
            commands::altgen_generate,
            commands::roblox_launch,
            commands::roblox_open_login,
            commands::roblox_open_account_browser,
            commands::login_cancel,
            commands::open_external,
            commands::app_version,
            commands::tracking_capture_preview,
            commands::tracking_capture_and_send,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                let state = app_handle.state::<AppState>();
                native::stop_mutex_holder(&state);
                native::stop_antiafk(&state);
            }
        });
}
