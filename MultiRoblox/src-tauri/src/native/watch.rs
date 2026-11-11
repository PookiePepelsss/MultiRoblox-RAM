// The watch loop: one shared poll covers every watched account instead of a
// spawn per account per tick.
use super::process::{
    apply_priority_policy, cached_or_spawn_pids, clear_manual_priority, owned_running_count,
};
use super::{emit_log, now_ms};
use crate::state::AppState;
use crate::storage::AccountInfo;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

static LAST_PUSHED_COUNT: AtomicI64 = AtomicI64::new(-1);

// MISS_THRESHOLD * POLL_INTERVAL gives ~1s worst-case close-detection
// latency while still tolerating transient misses. The PID readings come
// from the resident helper's watcher doing real process enumeration (not
// flaky tasklist parsing), so the card updates and the watcher shuts down
// within about a second of Roblox actually closing.
const MISS_THRESHOLD: u32 = 3;
pub const POLL_INTERVAL: Duration = Duration::from_millis(300);
/// Grace period after a launch before misses count: a client that is still
/// starting up can briefly not show up as alive.
const LAUNCH_DELAY_MS: i64 = 15_000;

// Bounds how often crash-recovery will relaunch a single account so a bad
// cookie/target that crashes immediately on every launch can't spin-loop.
const AUTO_RELAUNCH_MAX: usize = 3;
const AUTO_RELAUNCH_WINDOW_MS: i64 = 10 * 60 * 1000;

fn auto_relaunch_allowed(state: &AppState, account_id: &str) -> bool {
    let now = now_ms();
    let mut hist = state.auto_relaunch_history.lock().unwrap();
    let entry = hist.entry(account_id.to_string()).or_default();
    entry.retain(|t| now - *t < AUTO_RELAUNCH_WINDOW_MS);
    if entry.len() >= AUTO_RELAUNCH_MAX {
        return false;
    }
    entry.push(now);
    true
}

pub(super) fn stop_watch_poll_if_idle(state: &AppState) {
    if state.watched_accounts.lock().unwrap().is_empty() {
        if let Some(handle) = state.watch_handle.lock().unwrap().take() {
            handle.abort();
        }
        stop_pid_watcher(state);
    }
}

// Turns on the helper's PID broadcast thread; it pushes "E|PIDS|..." events
// which helper.rs drops straight into watch_pid_cache. Idempotent; asking
// twice just re-arms the same thread inside the same process.
async fn ensure_pid_watcher(app: &AppHandle, state: &AppState) {
    let cmd = format!("watch|{}", POLL_INTERVAL.as_millis());
    let _ = crate::helper::call(app, state, &cmd, crate::helper::DEFAULT_TIMEOUT).await;
}

fn stop_pid_watcher(state: &AppState) {
    *state.watch_pid_cache.lock().unwrap() = None;
    let app = state.app_handle.clone();
    tauri::async_runtime::spawn(async move {
        let st = app.state::<AppState>();
        // A launch can start watching again between the decision to stop
        // and this request reaching the helper; then the stop must not win.
        if !st.watched_accounts.lock().unwrap().is_empty() {
            return;
        }
        let _ = crate::helper::call(&app, &st, "watch|0", crate::helper::DEFAULT_TIMEOUT).await;
        if st.watched_accounts.lock().unwrap().is_empty() {
            *st.watch_pid_cache.lock().unwrap() = None;
            // A launch in flight has taken the lock but isn't watched yet. If
            // it goes on to fail, its own failure path releases the lock.
            if st.launch_cancel.lock().unwrap().is_empty() {
                super::daemon::release_sign_in_if_idle(&app, &st).await;
            }
        } else {
            ensure_pid_watcher(&app, &st).await;
        }
    });
}

// The check and the spawn happen under one lock so two launches finishing
// together can't both see "no poll yet" and start two. With a single poll
// task awaiting each tick in turn, ticks never overlap, which is why there
// is no re-entrancy guard on watch_tick.
fn start_watch_poll(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut slot = state.watch_handle.lock().unwrap();
    if slot.is_some() {
        return;
    }
    LAST_PUSHED_COUNT.store(-1, Ordering::Relaxed); // a new poll pushes its first reading
    let app = app.clone();
    *slot = Some(tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            watch_tick(&app).await;
        }
    }));
}

pub(super) async fn watch_roblox(app: &AppHandle, account_id: &str) {
    let st = app.state::<AppState>();
    // A Kill between the spawn and here already dropped the pid; watching
    // an account with no pid would report it "closed unexpectedly" (and
    // auto-relaunch it) once the grace window ran out.
    if !st.account_pids.lock().unwrap().contains_key(account_id) {
        return;
    }
    st.watched_accounts
        .lock()
        .unwrap()
        .insert(account_id.to_string(), now_ms() + LAUNCH_DELAY_MS);
    st.miss_counts
        .lock()
        .unwrap()
        .insert(account_id.to_string(), 0);
    start_watch_poll(app);
    ensure_pid_watcher(app, &st).await;
}

async fn watch_tick(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.watched_accounts.lock().unwrap().is_empty() {
        stop_watch_poll_if_idle(&state);
        return;
    }
    // Couldn't get a reading this tick (helper unavailable, spawn/timeout
    // failure), so we can't tell who's alive. Bail without touching miss counts
    // so a still-running account never gets penalized for it.
    let Some(alive_pids) = cached_or_spawn_pids(app, &state).await else {
        return;
    };

    let now = now_ms();
    let watched_snapshot: Vec<(String, i64)> = state
        .watched_accounts
        .lock()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    let mut closed: Vec<String> = Vec::new();
    for (account_id, ready_at) in watched_snapshot {
        if now < ready_at {
            continue;
        }
        // An account is running iff the exact process this app spawned for it
        // is still alive. watch_roblox() is only ever called from do_launch
        // after account_pids.insert, so a watched account always has a PID.
        //
        // There is deliberately no "is any Roblox alive" fallback and no
        // adopting of an unclaimed PID when the tracked one dies: both would
        // misattribute a Roblox the user started themselves, report it as
        // this account, and then taskkill it on Kill Roblox, which is
        // explicitly not ours to touch.
        let running = state
            .account_pids
            .lock()
            .unwrap()
            .get(&account_id)
            .is_some_and(|p| alive_pids.contains(p));
        let mut mc = state.miss_counts.lock().unwrap();
        if running {
            mc.insert(account_id, 0);
        } else {
            let m = mc.entry(account_id.clone()).or_insert(0);
            *m += 1;
            if *m >= MISS_THRESHOLD {
                closed.push(account_id);
            }
        }
    }

    for account_id in &closed {
        handle_closed(app, &state, account_id);
    }

    // Same definition as count_roblox_processes, and computed after the closed
    // accounts above were dropped from account_pids, so the pushed count and
    // the polled one can't disagree.
    // Only when it changed: this tick runs about three times a second, and each
    // emit is a round trip into the webview. The frontend polls every 3s when
    // pushes are quiet, so a skipped push can't leave the badge wrong for long.
    let count = i64::from(owned_running_count(&state, &alive_pids));
    if LAST_PUSHED_COUNT.swap(count, Ordering::Relaxed) != count {
        let _ = app.emit("roblox:count", count);
    }
    stop_watch_poll_if_idle(&state);
}

fn handle_closed(app: &AppHandle, state: &AppState, account_id: &str) {
    state.watched_accounts.lock().unwrap().remove(account_id);
    state.miss_counts.lock().unwrap().remove(account_id);
    let info = crate::storage::account_info(state, account_id).unwrap_or_default();
    let name = info.display(account_id);
    let pid = state.account_pids.lock().unwrap().remove(account_id);
    emit_log(
        app,
        "warn",
        "crash",
        &format!(
            "Roblox closed unexpectedly for {} (missed {} consecutive checks)",
            name, MISS_THRESHOLD
        ),
        Some(serde_json::json!({
            "accountId": account_id,
            "username": info.username,
            "userId": info.user_id,
            "pid": pid,
        })),
    );
    clear_manual_priority(state, account_id);
    let _ = app.emit("roblox:closed", account_id);
    apply_priority_policy(state);

    if crate::settings::get_bool("autoRelaunch", false) && !info.cookie.is_empty() {
        maybe_auto_relaunch(app, state, account_id, &info);
    }
}

fn maybe_auto_relaunch(app: &AppHandle, state: &AppState, account_id: &str, info: &AccountInfo) {
    let name = info.display(account_id);
    if !auto_relaunch_allowed(state, account_id) {
        emit_log(
            app,
            "warn",
            "crash",
            &format!(
                "Auto-relaunch skipped for {}: too many recent crashes",
                name
            ),
            None,
        );
        return;
    }
    emit_log(
        app,
        "info",
        "crash",
        &format!("Auto-relaunching {}...", name),
        None,
    );
    let app = app.clone();
    let account_id = account_id.to_string();
    let name = name.to_string();
    let cookie = info.cookie.clone();
    let target = info.game_target.clone();
    tauri::async_runtime::spawn(async move {
        let st = app.state::<AppState>();
        // Queued and cancellable exactly like a manual launch: the same lock
        // (so a mass crash can't fire concurrent launches with no stagger)
        // and the same cancel registry (so Kill or removing the account
        // stops a relaunch that hasn't spawned yet).
        super::launch::relaunch_with_retry(&app, &st, &account_id, &name, &cookie, &target).await;
    });
}
