use crate::encryption;
use crate::native::emit_log;
use crate::settings;
use crate::state::AppState;
use crate::storage::{self, account_id};
use serde_json::{json, Map, Value};
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Manager, State};

/// `{ "ok": true }` or `{ "ok": false, "error": "..." }`, the reply shape most
/// fallible commands share.
fn ok_reply(r: Result<(), String>) -> Value {
    match r {
        Ok(()) => json!({ "ok": true }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

#[tauri::command]
pub fn show_main_window(app: tauri::AppHandle) {
    app.state::<AppState>()
        .ui_ready
        .store(true, Ordering::SeqCst);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
    }
}

/// Real exit, from the in-app Quit affordance. Sets the flag first so the
/// hide-to-tray close handler and prevent_exit both stand aside.
#[tauri::command]
pub fn quit_app(app: tauri::AppHandle) {
    app.state::<AppState>()
        .quitting
        .store(true, Ordering::SeqCst);
    app.exit(0);
}

/// Creates or removes the tray icon to match the toggle. The setting itself is
/// persisted by settings_save; this only reconciles the icon.
#[tauri::command]
pub fn set_hide_to_tray(app: tauri::AppHandle, enabled: bool) {
    crate::tray::apply_setting(&app, enabled);
}

// ---- settings ----

/// Key material the backend owns. Never handed to the frontend, and never
/// accepted back from it: a stale copy of kdfSalt saved over a freshly
/// rotated one would orphan every passphrase-encrypted cookie.
const PRIVATE_SETTINGS: [&str; 5] = [
    "customKeyEnc",
    "customKey",
    "keyVerifier",
    "_deviceKey",
    encryption::GEN_KEY_SETTING,
];
const BACKEND_OWNED_SETTINGS: [&str; 3] = ["kdfSalt", "_deviceKey", "keySet"];

#[tauri::command]
pub fn settings_load() -> Value {
    let mut s = (*settings::snapshot()).clone();
    for k in PRIVATE_SETTINGS {
        s.remove(k);
    }
    s.insert("keySet".into(), Value::Bool(encryption::passphrase_mode()));
    Value::Object(s)
}

#[tauri::command]
pub fn settings_save(state: State<AppState>, data: Map<String, Value>) -> bool {
    let mut data = data;
    for k in PRIVATE_SETTINGS.iter().chain(BACKEND_OWNED_SETTINGS.iter()) {
        data.remove(*k);
    }
    let multi_instance = data.get("multiInstance").and_then(Value::as_bool);
    let antiafk = data.get("antiAfk").and_then(Value::as_bool);
    // A new interval or key re-arms the same way.
    let antiafk_interval_changed = data.contains_key("antiAfkInterval") || data.contains_key("antiAfkKey");
    let ram_limit_changed = data.contains_key(crate::native::RAM_LIMIT_KEY);
    let install_changed = data.contains_key("robloxInstall");
    let fps_lock_changed = data.contains_key(crate::fps::LOCK_KEY);
    let priority_changed = data.contains_key("lowPriorityMultiInstance");
    let sign_in_changed = data.contains_key("protectSignIn") || multi_instance.is_some();
    let saved = settings::update(|s| {
        s.remove("webhook");
        s.remove("screenshotsEnabled");
        s.extend(data);
    });
    if let Err(e) = &saved {
        eprintln!("[settings] {e}");
    }

    if ram_limit_changed {
        crate::native::apply_ram_limits(&state.app_handle, &state);
    }
    if install_changed {
        crate::native::invalidate_install_cache();
    }
    if priority_changed {
        crate::native::apply_priority_policy(&state);
    }
    if sign_in_changed {
        let app = state.app_handle.clone();
        tauri::async_runtime::spawn(async move {
            let st = app.state::<AppState>();
            crate::native::sync_sign_in_protection(&app, &st).await;
        });
    }
    if fps_lock_changed {
        if let Err(e) = crate::fps::enforce() {
            eprintln!("[fps] {e}");
        }
    }
    if let Some(on) = multi_instance {
        // Releases/re-acquires the mutex inside the one helper process;
        // toggling this no longer starts or kills anything.
        let app = state.app_handle.clone();
        tauri::async_runtime::spawn(async move {
            let st = app.state::<AppState>();
            crate::native::set_multi_instance(&app, &st, on).await;
        });
    }
    // Re-arming at a new interval also goes through start_antiafk: it
    // replaces the helper's existing anti-AFK thread, so no stop is needed.
    let rearm = antiafk_interval_changed && state.antiafk_on.load(Ordering::SeqCst);
    match antiafk {
        Some(false) => crate::native::stop_antiafk(&state),
        Some(true) => spawn_start_antiafk(&state),
        None if rearm => spawn_start_antiafk(&state),
        None => {}
    }
    saved.is_ok()
}

fn spawn_start_antiafk(state: &AppState) {
    let app = state.app_handle.clone();
    tauri::async_runtime::spawn(async move {
        let st = app.state::<AppState>();
        crate::native::start_antiafk(&app, &st).await;
    });
}

// ---- encryption ----
#[tauri::command]
pub fn enc_status(state: State<AppState>) -> Value {
    if !encryption::passphrase_mode() {
        return json!({ "mode": "setup" });
    }
    let unlocked = state.session_pass.lock().unwrap().is_some();
    json!({ "mode": if unlocked { "unlocked" } else { "locked" } })
}

// Async and on the blocking pool: verifying a passphrase is a ~340ms scrypt
// derive, which as a plain command ran on the main thread and froze the
// window for its duration.
#[tauri::command]
pub async fn enc_unlock(app: AppHandle, pass: String) -> Result<Value, ()> {
    let ok =
        tokio::task::spawn_blocking(move || encryption::unlock(&app.state::<AppState>(), &pass))
            .await
            .unwrap_or(false);
    Ok(json!({ "ok": ok }))
}

#[tauri::command]
pub async fn enc_set_key(app: AppHandle, pass: Option<String>) -> Result<Value, ()> {
    let np = pass.unwrap_or_default().trim().to_string();
    let result = tokio::task::spawn_blocking(move || {
        encryption::set_passphrase(&app.state::<AppState>(), &np)
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    Ok(ok_reply(result))
}

#[tauri::command]
pub async fn multiinstance_status(state: State<'_, AppState>) -> Result<Value, ()> {
    // Defaults on, matching the startup path in lib.rs; only an explicit
    // false counts as off.
    let enabled = settings::get_bool("multiInstance", true);
    // "active" means the one helper is up AND actually holding the mutex,
    // a stronger signal than "a child process exists".
    let active = crate::helper::is_holding_mutex(&state).await;
    Ok(json!({ "enabled": enabled, "active": active }))
}

#[tauri::command]
pub fn antiafk_status(state: State<AppState>) -> Value {
    let enabled = settings::get_bool("antiAfk", false);
    let active = state.antiafk_on.load(Ordering::SeqCst);
    json!({ "enabled": enabled, "active": active })
}

// ---- accounts ----
#[tauri::command]
pub fn accounts_load(state: State<AppState>) -> Vec<Value> {
    storage::load_accounts(&state)
}

#[tauri::command]
pub fn accounts_add(state: State<AppState>, account: Map<String, Value>) -> Result<Value, String> {
    let mut a = account;
    a.insert("id".into(), Value::String(uuid::Uuid::new_v4().to_string()));
    a.insert(
        "createdAt".into(),
        Value::String(chrono::Utc::now().to_rfc3339()),
    );
    a.insert("lastUsed".into(), Value::Null);
    let a = Value::Object(a);
    storage::update_accounts(&state, |list| list.push(a.clone()))?;
    Ok(a)
}

#[tauri::command]
pub fn accounts_remove(state: State<AppState>, id: String) -> Result<bool, String> {
    storage::update_accounts(&state, |list| {
        list.retain(|a| account_id(a) != Some(id.as_str()))
    })?;
    // Signal any launch already in flight (or still queued behind
    // launch_lock, which a mass-launch can hold for many seconds) to abandon
    // itself, so deleting an account stops a launch that is mid-flight rather
    // than letting it finish and spawn the removed account.
    crate::native::cancel_launch(&state, &id);
    // Drop bookkeeping so watch_tick stops polling a removed account.
    state.account_pids.lock().unwrap().remove(&id);
    state.watched_accounts.lock().unwrap().remove(&id);
    state.miss_counts.lock().unwrap().remove(&id);
    state.auto_relaunch_history.lock().unwrap().remove(&id);
    Ok(true)
}

#[tauri::command]
pub fn accounts_update(
    state: State<AppState>,
    id: String,
    data: Map<String, Value>,
) -> Result<Option<Value>, String> {
    storage::update_accounts(&state, |list| {
        let a = list
            .iter_mut()
            .find(|a| account_id(a) == Some(id.as_str()))?;
        if let Value::Object(existing) = a {
            existing.extend(data);
        }
        Some(a.clone())
    })
}

#[tauri::command]
pub fn accounts_reorder(state: State<AppState>, ids: Vec<String>) -> Result<bool, String> {
    storage::update_accounts(&state, |list| {
        // Listed ids first, in the given order; everything else keeps its
        // relative order after them.
        let rank = |a: &Value| {
            account_id(a)
                .and_then(|aid| ids.iter().position(|id| id == aid))
                .unwrap_or(usize::MAX)
        };
        list.sort_by_key(rank);
    })?;
    Ok(true)
}

// ---- packages ----
#[tauri::command]
pub fn packages_load() -> Vec<Value> {
    storage::PACKAGES.read()
}
#[tauri::command]
pub fn packages_save(packages: Vec<Value>) -> bool {
    storage::PACKAGES.write(&packages).is_ok()
}

// ---- generated-account history ----
// Async and on the blocking pool: each one encrypts or decrypts two fields per
// entry, up to 500 entries, which as a plain command ran on the main thread and
// froze the window for over 100ms on every Generate.
#[tauri::command]
pub async fn genhistory_read(app: AppHandle) -> Result<Vec<Value>, ()> {
    tokio::task::spawn_blocking(move || storage::GENHISTORY.read(&*app.state::<AppState>()))
        .await
        .map_err(|_| ())
}
#[tauri::command]
pub async fn genhistory_write(app: AppHandle, list: Vec<Value>) -> Result<bool, ()> {
    tokio::task::spawn_blocking(move || {
        storage::GENHISTORY
            .write(&*app.state::<AppState>(), list)
            .is_ok()
    })
    .await
    .map_err(|_| ())
}
#[tauri::command]
pub fn genhistory_clear() -> bool {
    storage::GENHISTORY.clear().is_ok()
}

#[tauri::command]
pub fn gen_key_load(state: State<AppState>) -> String {
    encryption::load_gen_key(&state)
}
#[tauri::command]
pub fn gen_key_save(state: State<AppState>, key: String) -> Result<(), String> {
    encryption::save_gen_key(&state, key.trim())
}

// ---- fflags / fps ----
#[tauri::command]
pub fn fflag_read() -> Value {
    crate::native::get_fflag_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_else(|| Value::Object(Map::new()))
}

#[tauri::command]
pub fn fflag_write(flags: Value) -> bool {
    let paths = crate::native::fflag_paths();
    let Some((primary, rest)) = paths.split_first() else {
        return false;
    };
    let json = serde_json::to_string_pretty(&flags).unwrap_or_else(|_| "{}".into());
    // Atomic: Roblox reads this file at startup, and a truncated one (crash
    // or full disk mid-write) makes it ignore every flag. Only the primary
    // copy decides success; the extra copy for a bootstrapper's active
    // version folder is best-effort (see native::fflag_paths).
    let ok = crate::jsonfile::write_atomic(primary, &json).is_ok();
    if ok {
        for p in rest {
            if let Err(e) = crate::jsonfile::write_atomic(p, &json) {
                eprintln!("[fflags] could not mirror to {}: {e}", p.display());
            }
        }
    }
    ok
}

#[tauri::command]
pub fn fps_read() -> i64 {
    crate::fps::read()
}

/// Sets the FPS cap (0 = unlimited) and makes it the one the FPS guard
/// defends while the lock setting is on.
#[tauri::command]
pub fn fps_write(cap: f64) -> Value {
    match crate::fps::write(cap) {
        Ok(value) => json!({ "ok": true, "value": value }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

// ---- roblox installs (vanilla + bootstrappers) ----
#[tauri::command]
pub fn roblox_list_installs() -> Value {
    crate::native::list_installs()
}

// ---- RAM limiter ----
#[tauri::command]
pub fn roblox_set_account_ram_limit(state: State<AppState>, id: String, mb: f64) -> Value {
    crate::native::set_account_ram_limit(&state, &id, mb)
}

// Polled every few seconds and opens each running process, so not on the main thread.
#[tauri::command]
pub async fn roblox_memory_usage(app: AppHandle) -> Result<Value, ()> {
    tokio::task::spawn_blocking(move || crate::native::memory_usage(&app.state::<AppState>()))
        .await
        .map_err(|_| ())
}

// Reads Windows performance counters, which can take a moment.
#[tauri::command]
pub async fn system_stats() -> Result<Value, ()> {
    tokio::task::spawn_blocking(crate::native::system_stats)
        .await
        .map_err(|_| ())
}

// Auto layout: lines the running windows up small in rows, or puts them back.
#[tauri::command]
pub async fn roblox_layout(app: AppHandle, on: bool, options: Option<crate::native::LayoutOptions>) -> Value {
    crate::native::layout_windows(&app, &app.state::<AppState>(), on, options.unwrap_or_default()).await
}

// A tiny picture of each running client's game, by account id.
#[tauri::command]
pub async fn roblox_previews(app: AppHandle) -> Value {
    crate::native::window_previews(&app, &app.state::<AppState>()).await
}

// ---- roblox process / launch ----
#[tauri::command]
pub async fn roblox_validate_cookie(
    state: State<'_, AppState>,
    cookie: String,
) -> Result<crate::roblox_api::UserInfo, ()> {
    Ok(crate::roblox_api::fetch_user_info(&state, &cookie).await)
}

#[tauri::command]
pub async fn roblox_set_volume(
    app: AppHandle,
    state: State<'_, AppState>,
    percent: f64,
) -> Result<Value, ()> {
    Ok(crate::native::set_roblox_volume(&app, &state, percent).await)
}

#[tauri::command]
pub async fn roblox_kill_all(app: AppHandle, state: State<'_, AppState>) -> Result<Value, ()> {
    let ids: Vec<String> = state
        .watched_accounts
        .lock()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let running_names: Vec<String> = ids
        .iter()
        .map(|id| {
            storage::account_info(&state, id)
                .and_then(|a| a.username)
                .unwrap_or_else(|| id.clone())
        })
        .collect();
    emit_log(
        &app,
        "warn",
        "kill",
        &format!(
            "Killing Roblox instances launched by MultiRoblox ({} running: {})",
            ids.len(),
            if running_names.is_empty() {
                "none".into()
            } else {
                running_names.join(", ")
            }
        ),
        Some(json!({ "count": ids.len(), "accounts": running_names })),
    );
    Ok(crate::native::kill_all_roblox(&app, &state).await)
}

#[tauri::command]
pub async fn roblox_kill_one(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<Value, ()> {
    let info = storage::account_info(&state, &id).unwrap_or_default();
    let username = info.display(&id).to_string();
    let pid = state.account_pids.lock().unwrap().get(&id).copied();
    emit_log(
        &app,
        "warn",
        "kill",
        &format!("Killed Roblox instance for {}", username),
        Some(json!({ "accountId": id, "username": username, "userId": info.user_id, "pid": pid })),
    );
    Ok(crate::native::kill_account_roblox(&app, &state, &id).await)
}

#[tauri::command]
pub async fn roblox_running_count(app: AppHandle, state: State<'_, AppState>) -> Result<u32, ()> {
    Ok(crate::native::count_roblox_processes(&app, &state).await)
}

// Polled by the frontend to self-heal card state if a push event is missed.
#[tauri::command]
pub async fn roblox_watched_ids(state: State<'_, AppState>) -> Result<Vec<String>, ()> {
    Ok(state
        .watched_accounts
        .lock()
        .unwrap()
        .keys()
        .cloned()
        .collect())
}

#[tauri::command]
pub async fn roblox_trim_memory(app: AppHandle, state: State<'_, AppState>) -> Result<Value, ()> {
    Ok(crate::native::trim_roblox_memory(&app, &state).await)
}

#[tauri::command]
pub async fn roblox_trim_account_memory(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<Value, ()> {
    Ok(crate::native::trim_account_memory(&app, &state, &id).await)
}

#[tauri::command]
pub fn roblox_set_account_priority(state: State<AppState>, id: String, priority: String) -> Value {
    crate::native::set_account_priority(&state, &id, &priority)
}

#[tauri::command]
pub async fn roblox_get_game_name(
    state: State<'_, AppState>,
    place_id_or_target: String,
    cookie: String,
) -> Result<Option<String>, ()> {
    Ok(crate::roblox_api::get_game_name(&state, &place_id_or_target, &cookie).await)
}

#[tauri::command]
pub async fn roblox_get_json(state: State<'_, AppState>, url: String) -> Result<Value, String> {
    crate::roblox_api::get_json_public(&state, &url).await
}

#[tauri::command]
pub async fn roblox_public_servers_as(
    state: State<'_, AppState>,
    cookie: String,
    url: String,
) -> Result<Value, String> {
    crate::roblox_api::public_servers_as(&state, &cookie, &url).await
}

#[tauri::command]
pub async fn roblox_headshots(state: State<'_, AppState>, tokens: Vec<String>) -> Result<Value, String> {
    crate::roblox_api::headshots_for_tokens(&state, &tokens).await
}

// Resolves a username to a "placeId:jobId" target string, which the launch
// path already understands as "join this exact running server".
#[tauri::command]
pub async fn roblox_follow_user(
    state: State<'_, AppState>,
    cookie: String,
    username: String,
) -> Result<Value, String> {
    crate::roblox_api::follow_user_target(&state, &cookie, &username).await
}

#[tauri::command]
pub async fn altgen_generate(
    state: State<'_, AppState>,
    api_key: String,
    quantity: i64,
) -> Result<Value, String> {
    crate::roblox_api::altgen_generate(&state, &api_key, quantity).await
}

#[tauri::command]
pub async fn roblox_launch(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    cookie: String,
    target: Option<String>,
    persist_target: Option<bool>,
) -> Result<Value, ()> {
    Ok(crate::native::launch_queued(
        &app,
        &state,
        &id,
        &cookie,
        target.as_deref().unwrap_or(""),
        persist_target.unwrap_or(true),
    )
    .await)
}

// ---- private servers ----
#[tauri::command]
pub async fn servers_list(
    state: State<'_, AppState>,
    id: String,
    place_id: String,
) -> Result<Value, String> {
    crate::servers::list(&state, &id, &place_id).await
}

#[tauri::command]
pub async fn servers_price(
    state: State<'_, AppState>,
    place_id: String,
) -> Result<Value, String> {
    crate::servers::price(&state, &place_id).await
}

#[tauri::command]
pub async fn servers_buy(
    state: State<'_, AppState>,
    id: String,
    universe_id: i64,
    name: String,
    expected_price: i64,
    idempotency_key: String,
) -> Result<Value, String> {
    crate::servers::buy(&state, &id, universe_id, &name, expected_price, &idempotency_key).await
}

#[tauri::command]
pub async fn servers_update(
    state: State<'_, AppState>,
    id: String,
    server_id: i64,
    patch: Map<String, Value>,
) -> Result<Value, String> {
    crate::servers::update(&state, &id, server_id, &patch).await
}

#[tauri::command]
pub async fn servers_renewal(
    state: State<'_, AppState>,
    id: String,
    server_id: i64,
    active: bool,
    price: i64,
) -> Result<Value, String> {
    crate::servers::set_renewal(&state, &id, server_id, active, price).await
}

#[tauri::command]
pub async fn servers_plan(
    state: State<'_, AppState>,
    target: String,
    mode: String,
    ids: Vec<String>,
) -> Result<Value, String> {
    crate::servers::plan(&state, &target, &mode, &ids).await
}

#[tauri::command]
pub fn roblox_launch_cancel(state: State<AppState>, id: String) -> Value {
    json!({ "ok": crate::native::cancel_launch(&state, &id) })
}

// ---- browser login ----
#[tauri::command]
pub async fn roblox_open_login(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<crate::login::LoginResult, ()> {
    Ok(crate::login::open_login(&app, &state).await)
}

#[tauri::command]
pub async fn roblox_login_with_credentials(
    app: AppHandle,
    state: State<'_, AppState>,
    username: String,
    password: String,
) -> Result<crate::login::LoginResult, ()> {
    Ok(crate::login::open_login_with_credentials(&app, &state, &username, &password).await)
}

#[tauri::command]
pub fn login_cancel(state: State<AppState>) {
    crate::login::cancel_login(&state);
}

#[tauri::command]
pub async fn roblox_open_account_browser(
    app: AppHandle,
    state: State<'_, AppState>,
    cookie: String,
) -> Result<Value, ()> {
    Ok(ok_reply(
        crate::login::open_account_in_browser(&app, &state, &cookie).await,
    ))
}

// ---- tracking (screenshots -> Discord webhook) ----
#[tauri::command]
pub async fn tracking_capture_preview(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<Value, ()> {
    match crate::tracking::capture_preview_b64(&app, &state, &id).await {
        Ok(b64) => Ok(json!({ "ok": true, "dataUrl": format!("data:image/png;base64,{}", b64) })),
        Err(e) => Ok(json!({ "ok": false, "error": e })),
    }
}

// Lets the UI reject a bad URL as it's typed instead of silently failing on
// the next capture. The backend check in capture_and_send is the real gate.
#[tauri::command]
pub fn tracking_validate_webhook(url: String) -> Value {
    ok_reply(crate::tracking::validate_webhook_url(&url))
}

fn parse_region(region: &Value) -> Option<(f64, f64, f64, f64)> {
    Some((
        region.get("x")?.as_f64()?,
        region.get("y")?.as_f64()?,
        region.get("w")?.as_f64()?,
        region.get("h")?.as_f64()?,
    ))
}

#[tauri::command]
pub async fn tracking_capture_and_send(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    username: String,
    webhook_url: String,
    regions: Option<Vec<Value>>,
) -> Result<Value, ()> {
    let crops: Vec<(f64, f64, f64, f64)> = regions
        .unwrap_or_default()
        .iter()
        .filter_map(parse_region)
        .collect();
    Ok(ok_reply(
        crate::tracking::capture_and_send(&app, &state, &id, &username, &webhook_url, crops).await,
    ))
}

// Full reset from the lock screen; stops the helper first and waits for it
// to actually exit (it holds its own exe file open, which would otherwise
// make deleting the folder fail on Windows), wipes the entire app-data
// folder, then clears the in-memory state that would otherwise still point
// at it. The frontend only reloads the webview afterwards, not the Rust
// process, so the helper has to be brought back up here.
#[tauri::command]
pub async fn clear_app_data(app: AppHandle, state: State<'_, AppState>) -> Result<Value, ()> {
    crate::native::stop_all_native_helpers(&state).await;
    let dir = crate::paths::app_data_dir();
    let result = tokio::task::spawn_blocking(move || storage::wipe_app_data(&dir))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
    crate::native::reset_state_for_wipe(&state);
    // Re-extracts the embedded helper into the now-empty folder and starts
    // exactly one again, holding the mutex as before.
    crate::native::start_mutex_holder(&app, &state).await;
    Ok(ok_reply(result))
}

// ---- misc ----
// Web links only. This hands the string to Windows to open, and a file: path or
// a registered protocol handler could otherwise run something.
fn is_web_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
}

#[tauri::command]
pub fn open_external(url: String) -> Result<(), String> {
    if !is_web_url(&url) {
        return Err("Only http and https links can be opened.".into());
    }
    tauri_plugin_opener::open_url(&url, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::AccountInfo;

    #[test]
    fn only_web_links_can_be_opened() {
        assert!(is_web_url("https://www.roblox.com/games/1"));
        assert!(is_web_url("https://discord.gg/abc"));
        assert!(is_web_url("http://example.com"));
        for bad in [
            "file:///C:/Windows/System32/calc.exe",
            "C:\\Windows\\System32\\calc.exe",
            "ms-settings:",
            "roblox-player:1+launchmode:app",
            "javascript:alert(1)",
            "",
        ] {
            assert!(!is_web_url(bad), "allowed {bad:?}");
        }
    }

    #[test]
    fn region_parsing_needs_all_four_numbers() {
        assert_eq!(
            parse_region(&json!({"x":1,"y":2,"w":3,"h":4})),
            Some((1.0, 2.0, 3.0, 4.0))
        );
        assert_eq!(parse_region(&json!({"x":1,"y":2,"w":3})), None);
    }

    #[test]
    fn account_info_reads_known_fields() {
        let a = json!({"id":"1","username":"bob","userId":"9","cookie":"c","gameTarget":"123"});
        let info = AccountInfo::from_value(&a);
        assert_eq!(info.display("1"), "bob");
        assert_eq!(info.user_id.as_deref(), Some("9"));
        assert_eq!(info.game_target, "123");
        assert_eq!(AccountInfo::default().display("fallback"), "fallback");
    }
}
