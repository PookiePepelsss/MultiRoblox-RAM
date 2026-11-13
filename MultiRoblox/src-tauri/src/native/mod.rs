// Everything that touches Roblox processes or the resident helper on the
// user's machine, split by concern:
//
//   helper_exe  locating / extracting RobloxNative.exe
//   daemon      standing helper state: singleton mutex, anti-AFK, wipe reset
//   process     enumerating, killing, trimming and prioritising instances
//   install     finding Roblox installs (vanilla and bootstrappers), spawning the client
//   memory      per-instance RAM limiter and memory readout
//   watch       the shared poll that notices instances closing
//   launch      the launch pipeline and its cancellation registry
//   profile     per-account FPS, graphics, custom FFlags and window at launch
//   windows_view  laying the windows out small in rows, and their tiny previews
mod daemon;
mod helper_exe;
mod install;
mod launch;
mod memory;
mod process;
pub mod profile;
mod sysstats;
mod watch;
mod windows_view;

pub use daemon::{
    antiafk_command, reset_state_for_wipe, set_multi_instance, start_antiafk, start_mutex_holder,
    stop_all_native_helpers, stop_antiafk, sync_sign_in_protection,
};
pub use helper_exe::ensure_native_helper;
pub use install::{
    fflag_paths, get_fflag_path, invalidate_cache as invalidate_install_cache, list_installs,
};
pub use launch::{cancel_launch, launch_queued};
pub use sysstats::system_stats;
pub use windows_view::{layout as layout_windows, previews as window_previews, LayoutOptions};
pub use memory::{
    apply_all as apply_ram_limits, set_account_limit as set_account_ram_limit,
    usage as memory_usage, RAM_LIMIT_KEY,
};
pub use process::{
    apply_priority_policy, count_roblox_processes, kill_account_roblox, kill_all_roblox, no_window, set_account_priority,
    set_roblox_volume, system_tool, trim_account_memory, trim_roblox_memory,
};
pub(crate) use process::{no_window_std, taskkill};
pub use watch::POLL_INTERVAL;

use serde_json::Value;
use tauri::{AppHandle, Emitter};

pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// `extra` must stay nested under "meta"; renderer.js's log handler expects it there.
pub fn emit_log(app: &AppHandle, level: &str, category: &str, message: &str, extra: Option<Value>) {
    let payload = serde_json::json!({
        "level": level,
        "category": category,
        "message": message,
        "meta": extra.unwrap_or_else(|| Value::Object(serde_json::Map::new())),
    });
    let _ = app.emit("log:entry", payload);
}
