// Standing state of the one helper process: the singleton mutex and the
// anti-AFK thread. Nothing here spawns or kills a process any more; these
// tell the daemon which state to be in, and wait until it has applied it.
use super::emit_log;
use crate::helper::{self, DEFAULT_TIMEOUT, SLOW_TIMEOUT};
use crate::state::AppState;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Manager};

// ---- singleton mutex ----
// The helper owns ROBLOX_singletonMutex on a dedicated thread for the whole
// session (see helper.rs / MutexHolder in RobloxNative.cs).
pub async fn start_mutex_holder(app: &AppHandle, state: &AppState) {
    if helper::ensure(app, state).await.is_none() {
        eprintln!("[mutex] native helper unavailable");
    }
}

pub async fn set_multi_instance(app: &AppHandle, state: &AppState, on: bool) {
    let cmd = if on { "mutex|on" } else { "mutex|off" };
    match helper::call(app, state, cmd, SLOW_TIMEOUT).await {
        Ok(reply) => helper::note_mutex_held(state, reply.trim() == "1").await,
        Err(e) => eprintln!("[mutex] {}: {}", cmd, e),
    }
}

// Called before every launch. The startup hold can fail or be incomplete (a
// client was already open, or started during the helper's first scan), and
// nothing would otherwise retry it. A no-op in the helper when both names are
// already held, so this costs one pipe round trip on the normal path.
pub(super) async fn ensure_singleton_held(app: &AppHandle, state: &AppState) {
    if !crate::settings::get_bool("multiInstance", true) {
        return;
    }
    set_multi_instance(app, state, true).await;
    if !helper::is_holding_mutex(state).await {
        emit_log(
            app,
            "warn",
            "launch",
            "Could not take Roblox's single-instance lock. If new instances close right away, close any open Roblox windows and try again.",
            None,
        );
    }
}

// ---- shared sign-in file ----
fn sign_in_protection_enabled() -> bool {
    crate::settings::get_bool("protectSignIn", true) && crate::settings::get_bool("multiInstance", true)
}

async fn set_sign_in_lock(app: &AppHandle, state: &AppState, hold: bool) {
    let cmd = if hold { "signin|on" } else { "signin|off" };
    match helper::call(app, state, cmd, DEFAULT_TIMEOUT).await {
        Ok(reply) if hold && reply.trim() != "1" => emit_log(
            app,
            "warn",
            "launch",
            "Could not protect the Roblox sign-in file. Clients may swap accounts (error 773); close any open Roblox windows and try again.",
            None,
        ),
        Ok(_) => {}
        Err(e) => eprintln!("[signin] {cmd}: {e}"),
    }
}

/// Called just before a client starts, so a launch that fails earlier leaves
/// the file alone.
pub(super) async fn protect_sign_in_for_launch(app: &AppHandle, state: &AppState) {
    if sign_in_protection_enabled() {
        set_sign_in_lock(app, state, true).await;
    }
}

/// Gives the file back once nothing this app launched is running, so Roblox
/// opened normally can sign in as usual.
pub(super) async fn release_sign_in_if_idle(app: &AppHandle, state: &AppState) {
    if state.watched_accounts.lock().unwrap().is_empty() {
        set_sign_in_lock(app, state, false).await;
    }
}

/// Matches the lock to the setting, after it changes while instances may be up.
pub async fn sync_sign_in_protection(app: &AppHandle, state: &AppState) {
    let running = !state.watched_accounts.lock().unwrap().is_empty();
    set_sign_in_lock(app, state, running && sign_in_protection_enabled()).await;
}

// Re-acquires the mutex and re-closes any singleton handles. Roblox exiting
// can leave the name in a state where a fresh acquire is the only way to be
// certain we still own it, which is why kill-all runs this.
pub(super) async fn restart_mutex_holder(app: &AppHandle, state: &AppState) {
    match helper::call(app, state, "mutex|rehold", SLOW_TIMEOUT).await {
        Ok(reply) => helper::note_mutex_held(state, reply.trim() == "1").await,
        Err(e) => eprintln!("[mutex] rehold: {}", e),
    }
}

// Stops the helper and waits for it to actually exit; called right before
// wiping app data, since the running helper holds its own exe file (which
// lives in that folder) open, and Windows won't delete a folder out from
// under an open handle.
pub async fn stop_all_native_helpers(state: &AppState) {
    if let Some(handle) = state.watch_handle.lock().unwrap().take() {
        handle.abort();
    }
    state.antiafk_on.store(false, Ordering::SeqCst);
    helper::shutdown(state).await;
}

// Resets every in-memory field that could otherwise point at data which no
// longer exists after a full app-data wipe (a memoized helper-exe path, a
// cached decryption key derived from a now-deleted salt, tracked PIDs for
// accounts that no longer exist, etc). Deliberately leaves login_cancel
// alone; an in-progress login window closing on its own is harmless.
pub fn reset_state_for_wipe(state: &AppState) {
    // A launch still queued when the user wipes would otherwise run on
    // through its network work before do_launch's storage check rejects it.
    super::launch::cancel_all_launches(state);
    *state.session_pass.lock().unwrap() = None;
    crate::encryption::invalidate_key_cache(state);
    *state.native_helper_path.lock().unwrap() = None;
    // Lift the shutdown latch stop_all_native_helpers set, so the helper is
    // allowed to start again once the folder has been re-created. The sweep
    // flag stays set; we just stopped the only helper there was, so there's
    // nothing stray to clear.
    state.helper_shutdown.store(false, Ordering::SeqCst);
    state.account_pids.lock().unwrap().clear();
    state.manual_priority.lock().unwrap().clear();
    state.watched_accounts.lock().unwrap().clear();
    state.miss_counts.lock().unwrap().clear();
    state.auto_relaunch_history.lock().unwrap().clear();
    state.csrf_cache.lock().unwrap().clear();
    state.ticket_cache.lock().unwrap().clear();
    *state.last_launch_ts.lock().unwrap() = 0;
}

// ---- anti-AFK ----
// Runs on a background thread inside the one helper process. Toggling it is a
// request, not a spawn/kill, so switching it on and off repeatedly can't leave
// a stray process behind.

/// Longest allowed gap between taps. Roblox kicks at about 20 minutes, and a
/// tap can fail (the window can't be focused, the PC is locked), so this keeps
/// several minutes of retries in hand. Older settings of 18-19 are lowered.
const ANTIAFK_MAX_SECS: i64 = 17 * 60;

/// Seconds between keep-alive taps. Anything under a minute (including unset)
/// falls back to the maximum.
fn antiafk_interval_secs() -> i64 {
    match crate::settings::get_i64("antiAfkInterval") {
        Some(s) if s >= 60 => s.min(ANTIAFK_MAX_SECS),
        _ => ANTIAFK_MAX_SECS,
    }
}

/// The Windows virtual-key code anti-AFK presses: the chosen one, else 0, which
/// the helper reads as its default (Shift). Out-of-range values are ignored.
fn antiafk_key() -> i64 {
    match crate::settings::get_i64("antiAfkKey") {
        Some(vk) if (1..=255).contains(&vk) => vk,
        _ => 0,
    }
}

/// The helper request that arms anti-AFK at the configured interval and key;
/// shared with helper.rs, which re-sends it after restarting a crashed daemon.
pub fn antiafk_command() -> (String, i64) {
    let secs = antiafk_interval_secs();
    (format!("antiafk|{}|{}", secs, antiafk_key()), secs)
}

pub async fn start_antiafk(app: &AppHandle, state: &AppState) {
    if !cfg!(windows) {
        return;
    }
    let (cmd, secs) = antiafk_command();
    match helper::call(app, state, &cmd, DEFAULT_TIMEOUT).await {
        Ok(_) => {
            state.antiafk_on.store(true, Ordering::SeqCst);
            emit_log(
                app,
                "ok",
                "afk",
                &format!("Anti-AFK started (interval: {} min)", secs / 60),
                Some(serde_json::json!({ "intervalSec": secs })),
            );
        }
        Err(e) => {
            eprintln!("[antiafk] {}", e);
            emit_log(
                app,
                "warn",
                "afk",
                &format!("Could not start Anti-AFK: {}", e),
                None,
            );
        }
    }
}

pub fn stop_antiafk(state: &AppState) {
    if !state.antiafk_on.swap(false, Ordering::SeqCst) {
        return; // wasn't running; don't log a stop that never happened
    }
    emit_log(&state.app_handle, "warn", "afk", "Anti-AFK stopped", None);
    // Fire-and-forget: the helper stops its own thread. Nothing to reap.
    let app = state.app_handle.clone();
    tauri::async_runtime::spawn(async move {
        let st = app.state::<AppState>();
        let _ = helper::call(&app, &st, "antiafk|0", DEFAULT_TIMEOUT).await;
    });
}
