use crate::encryption;
use crate::settings::{load_settings, save_settings};
use crate::state::AppState;
use crate::storage;
use serde_json::{Map, Value};
use tauri::{AppHandle, Manager, State};

#[tauri::command]
pub fn show_main_window(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
    }
}

// ---- settings ----
#[tauri::command]
pub fn settings_load() -> Value {
    let mut s = load_settings();
    for k in ["customKeyEnc", "customKey", "keyVerifier", "_deviceKey"] {
        s.remove(k);
    }
    let key_set = encryption::passphrase_mode();
    s.insert("keySet".into(), Value::Bool(key_set));
    Value::Object(s)
}

#[tauri::command]
pub fn settings_save(state: State<AppState>, data: Map<String, Value>) -> bool {
    let mut data = data;
    for k in ["customKey", "customKeyEnc", "keyVerifier"] {
        data.remove(k);
    }
    let mut s = load_settings();
    s.remove("webhook");
    s.remove("screenshotsEnabled");
    let had_enc_type = data.contains_key("encryptionType");
    let multi_instance = data.get("multiInstance").cloned();
    let antiafk = data.get("antiAfk").cloned();
    let antiafk_interval_changed = data.contains_key("antiAfkInterval");
    for (k, v) in data {
        s.insert(k, v);
    }
    save_settings(&s);
    if had_enc_type {
        encryption::invalidate_key_cache(&state);
    }
    if let Some(Value::Bool(on)) = multi_instance {
        let app = state.app_handle.clone();
        if on {
            tauri::async_runtime::spawn(async move {
                let st = app.state::<AppState>();
                crate::native::start_mutex_holder(&app, &st).await;
            });
        } else {
            crate::native::stop_mutex_holder(&state);
        }
    }
    if let Some(Value::Bool(on)) = antiafk {
        let app = state.app_handle.clone();
        if on {
            tauri::async_runtime::spawn(async move {
                let st = app.state::<AppState>();
                crate::native::start_antiafk(&app, &st).await;
            });
        } else {
            crate::native::stop_antiafk(&state);
        }
    } else if antiafk_interval_changed && state.antiafk_child.lock().unwrap().is_some() {
        crate::native::stop_antiafk(&state);
        let app = state.app_handle.clone();
        tauri::async_runtime::spawn(async move {
            let st = app.state::<AppState>();
            crate::native::start_antiafk(&app, &st).await;
        });
    }
    true
}

// ---- encryption ----
#[tauri::command]
pub fn enc_status(state: State<AppState>) -> Value {
    if !encryption::passphrase_mode() {
        return serde_json::json!({ "mode": "setup" });
    }
    let unlocked = state.session_pass.lock().unwrap().is_some();
    serde_json::json!({ "mode": if unlocked { "unlocked" } else { "locked" } })
}

#[tauri::command]
pub fn enc_unlock(state: State<AppState>, pass: String) -> Value {
    if pass.is_empty() || !encryption::verify_pass(&pass) {
        return serde_json::json!({ "ok": false });
    }
    *state.session_pass.lock().unwrap() = Some(pass);
    encryption::invalidate_key_cache(&state);
    serde_json::json!({ "ok": true })
}

#[tauri::command]
pub fn enc_set_key(state: State<AppState>, pass: Option<String>) -> Value {
    let np = pass.unwrap_or_default().trim().to_string();
    let raw = storage::load_accounts_raw();
    let accts_dec: Vec<Value> = raw
        .iter()
        .cloned()
        .map(|a| storage::decrypt_account(&state, a))
        .collect();
    for (orig, dec) in raw.iter().zip(accts_dec.iter()) {
        let had_cookie = orig
            .get("cookie")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let now_empty = dec
            .get("cookie")
            .and_then(|v| v.as_str())
            .map(|s| s.is_empty())
            .unwrap_or(true);
        if had_cookie && now_empty {
            return serde_json::json!({ "ok": false, "error": "decrypt failed" });
        }
    }

    if !np.is_empty() {
        encryption::rotate_salt();
        *state.session_pass.lock().unwrap() = Some(np.clone());
        encryption::invalidate_key_cache(&state);
        let mut rest = load_settings();
        rest.remove("customKey");
        rest.remove("customKeyEnc");
        let verifier = encryption::make_verifier(&np);
        rest.insert("keyVerifier".into(), Value::String(verifier));
        rest.insert("encSetupDone".into(), Value::Bool(true));
        save_settings(&rest);
    } else {
        *state.session_pass.lock().unwrap() = None;
        encryption::invalidate_key_cache(&state);
        let mut rest = load_settings();
        rest.remove("customKey");
        rest.remove("customKeyEnc");
        rest.remove("keyVerifier");
        rest.insert("encSetupDone".into(), Value::Bool(true));
        save_settings(&rest);
    }
    encryption::invalidate_key_cache(&state);
    match storage::save_accounts(&state, accts_dec) {
        Ok(_) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    }
}

#[tauri::command]
pub fn multiinstance_status(state: State<AppState>) -> Value {
    let enabled = load_settings()
        .get("multiInstance")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let active = state.mutex_child.lock().unwrap().is_some();
    serde_json::json!({ "enabled": enabled, "active": active })
}

#[tauri::command]
pub fn antiafk_status(state: State<AppState>) -> Value {
    let enabled = load_settings()
        .get("antiAfk")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let active = state.antiafk_child.lock().unwrap().is_some();
    serde_json::json!({ "enabled": enabled, "active": active })
}

// ---- accounts ----
#[tauri::command]
pub fn accounts_load(state: State<AppState>) -> Vec<Value> {
    storage::load_accounts(&state)
}

#[tauri::command]
pub fn accounts_add(state: State<AppState>, account: Map<String, Value>) -> Result<Value, String> {
    let mut accounts = storage::load_accounts(&state);
    let mut a = Value::Object(account);
    a["id"] = Value::String(uuid::Uuid::new_v4().to_string());
    a["createdAt"] = Value::String(chrono::Utc::now().to_rfc3339());
    a["lastUsed"] = Value::Null;
    accounts.push(a.clone());
    storage::save_accounts(&state, accounts)?;
    Ok(a)
}

#[tauri::command]
pub fn accounts_remove(state: State<AppState>, id: String) -> Result<bool, String> {
    let accounts = storage::load_accounts(&state);
    let filtered: Vec<Value> = accounts
        .into_iter()
        .filter(|a| a.get("id").and_then(|v| v.as_str()) != Some(id.as_str()))
        .collect();
    storage::save_accounts(&state, filtered)?;
    Ok(true)
}

#[tauri::command]
pub fn accounts_update(
    state: State<AppState>,
    id: String,
    data: Map<String, Value>,
) -> Result<Option<Value>, String> {
    let mut accounts = storage::load_accounts(&state);
    let idx = accounts
        .iter()
        .position(|a| a.get("id").and_then(|v| v.as_str()) == Some(id.as_str()));
    match idx {
        Some(i) => {
            if let Value::Object(existing) = &mut accounts[i] {
                for (k, v) in data {
                    existing.insert(k, v);
                }
            }
            let updated = accounts[i].clone();
            storage::save_accounts(&state, accounts)?;
            Ok(Some(updated))
        }
        None => Ok(None),
    }
}

#[tauri::command]
pub fn accounts_reorder(state: State<AppState>, ids: Vec<String>) -> Result<bool, String> {
    let accounts = storage::load_accounts(&state);
    let mut reordered: Vec<Value> = Vec::new();
    for id in &ids {
        if let Some(a) = accounts
            .iter()
            .find(|a| a.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
        {
            reordered.push(a.clone());
        }
    }
    let rest: Vec<Value> = accounts
        .into_iter()
        .filter(|a| {
            let aid = a.get("id").and_then(|v| v.as_str()).unwrap_or("");
            !ids.iter().any(|id| id == aid)
        })
        .collect();
    reordered.extend(rest);
    storage::save_accounts(&state, reordered)?;
    Ok(true)
}

// ---- packages ----
#[tauri::command]
pub fn packages_load() -> Vec<Value> {
    storage::load_packages()
}
#[tauri::command]
pub fn packages_save(packages: Vec<Value>) -> bool {
    storage::save_packages(&packages).is_ok()
}

// ---- generated-account history ----
#[tauri::command]
pub fn genhistory_read(state: State<AppState>) -> Vec<Value> {
    storage::read_genhistory(&state)
}
#[tauri::command]
pub fn genhistory_write(state: State<AppState>, list: Vec<Value>) -> bool {
    storage::write_genhistory(&state, list).is_ok()
}
#[tauri::command]
pub fn genhistory_clear() -> bool {
    storage::write_json_array(&crate::paths::genhistory_path(), &[]).is_ok()
}

// ---- fflags / fps ----
#[tauri::command]
pub fn fflag_read() -> Value {
    match crate::native::get_fflag_path() {
        Some(p) if p.exists() => std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| Value::Object(Map::new())),
        _ => Value::Object(Map::new()),
    }
}

#[tauri::command]
pub fn fflag_write(flags: Value) -> bool {
    let Some(p) = crate::native::get_fflag_path() else {
        return false;
    };
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(
        &p,
        serde_json::to_string_pretty(&flags).unwrap_or_else(|_| "{}".into()),
    )
    .is_ok()
}

fn global_settings_path() -> std::path::PathBuf {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    std::path::PathBuf::from(home)
        .join("AppData")
        .join("Local")
        .join("Roblox")
        .join("GlobalBasicSettings_13.xml")
}

#[tauri::command]
pub fn fps_read() -> i64 {
    let p = global_settings_path();
    let Ok(xml) = std::fs::read_to_string(&p) else {
        return 60;
    };
    let re = regex::Regex::new(r#"(?i)<int\s+name="FramerateCap"\s*>(\d+)</int>"#).unwrap();
    re.captures(&xml)
        .and_then(|c| c[1].parse::<i64>().ok())
        .unwrap_or(60)
}

// Roblox's DFIntTaskSchedulerTargetFps Fast Flag must be kept in sync with
// the XML FramerateCap or the internal scheduler falls back toward its own
// default at the extremes (30/360) -- see native::get_fflag_path callers.
fn set_frame_rate_fflag(value: i64) {
    let Some(p) = crate::native::get_fflag_path() else {
        return;
    };
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut flags: Map<String, Value> = if p.exists() {
        std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        Map::new()
    };
    flags.insert(
        "DFIntTaskSchedulerTargetFps".into(),
        Value::Number((if value == 0 { 9999 } else { value }).into()),
    );
    let _ = std::fs::write(
        &p,
        serde_json::to_string_pretty(&flags).unwrap_or_else(|_| "{}".into()),
    );
}

#[tauri::command]
pub fn fps_write(cap: f64) -> Value {
    let p = global_settings_path();
    let Ok(xml) = std::fs::read_to_string(&p) else {
        return serde_json::json!({ "ok": false, "error": "GlobalBasicSettings_13.xml not found - launch Roblox once to create it." });
    };
    let value = cap.round().max(0.0) as i64;
    let re = regex::Regex::new(r#"(?i)<int\s+name="FramerateCap"\s*>\d+</int>"#).unwrap();
    let new_xml = if re.is_match(&xml) {
        re.replace(&xml, format!(r#"<int name="FramerateCap">{}</int>"#, value))
            .to_string()
    } else {
        let re2 = regex::Regex::new(r"</Item>").unwrap();
        re2.replace(
            &xml,
            format!("\t\t<int name=\"FramerateCap\">{}</int>\n</Item>", value),
        )
        .to_string()
    };
    if std::fs::write(&p, new_xml).is_err() {
        return serde_json::json!({ "ok": false, "error": "Failed to write GlobalBasicSettings_13.xml" });
    }
    set_frame_rate_fflag(value);
    serde_json::json!({ "ok": true })
}

// ---- roblox process / launch ----
#[tauri::command]
pub async fn roblox_get_version(state: State<'_, AppState>) -> Result<Option<String>, ()> {
    Ok(crate::roblox_api::get_roblox_version(&state).await)
}

#[tauri::command]
pub async fn roblox_validate_cookie(
    state: State<'_, AppState>,
    cookie: String,
) -> Result<Value, ()> {
    let info = crate::roblox_api::fetch_user_info(&state, &cookie).await;
    Ok(
        serde_json::json!({ "ok": info.ok, "username": info.username, "userId": info.user_id, "reason": info.reason }),
    )
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
    let accounts = storage::load_accounts(&state);
    let running_names: Vec<String> = state
        .watched_accounts
        .lock()
        .unwrap()
        .keys()
        .map(|id| {
            accounts
                .iter()
                .find(|a| a.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
                .and_then(|a| a.get("username"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| id.clone())
        })
        .collect();
    let count = state.watched_accounts.lock().unwrap().len();
    crate::native::emit_log(
        &app,
        "warn",
        "kill",
        &format!(
            "Killed all Roblox instances ({} running: {})",
            count,
            if running_names.is_empty() {
                "none".into()
            } else {
                running_names.join(", ")
            }
        ),
        Some(serde_json::json!({ "count": count, "accounts": running_names })),
    );
    Ok(crate::native::kill_all_roblox(&app, &state).await)
}

#[tauri::command]
pub async fn roblox_kill_one(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<Value, ()> {
    let accounts = storage::load_accounts(&state);
    let acct = accounts
        .iter()
        .find(|a| a.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
        .cloned()
        .unwrap_or(Value::Null);
    let username = acct
        .get("username")
        .and_then(|v| v.as_str())
        .unwrap_or(&id)
        .to_string();
    let user_id = acct.get("userId").and_then(|v| v.as_str());
    let pid = state.account_pids.lock().unwrap().get(&id).copied();
    crate::native::emit_log(
        &app,
        "warn",
        "kill",
        &format!("Killed Roblox instance for {}", username),
        Some(
            serde_json::json!({ "accountId": id, "username": username, "userId": user_id, "pid": pid }),
        ),
    );
    Ok(crate::native::kill_account_roblox(&app, &state, &id).await)
}

#[tauri::command]
pub async fn roblox_running_count(app: AppHandle, state: State<'_, AppState>) -> Result<u32, ()> {
    Ok(crate::native::count_roblox_processes(&app, &state).await)
}

// Ground truth for which accounts the backend still considers launched.
// Frontend polls this every 5s to self-heal card state instead of relying
// solely on push events, which can be missed (listener registered late,
// event dropped, etc).
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
) -> Result<Value, ()> {
    let _guard = state.launch_lock.lock().await;
    Ok(crate::native::do_launch(&app, &state, &id, &cookie, target.as_deref().unwrap_or("")).await)
}

// ---- browser login ----
#[tauri::command]
pub async fn roblox_open_login(app: AppHandle, state: State<'_, AppState>) -> Result<Value, ()> {
    let r = crate::login::open_login(&app, &state).await;
    Ok(
        serde_json::json!({ "success": r.success, "cookie": r.cookie, "username": r.username, "userId": r.user_id, "error": r.error }),
    )
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
    match crate::login::open_account_in_browser(&app, &state, &cookie).await {
        Ok(()) => Ok(serde_json::json!({ "ok": true })),
        Err(e) => Ok(serde_json::json!({ "ok": false, "error": e })),
    }
}

// ---- misc ----
#[tauri::command]
pub fn open_external(url: String) -> Result<(), String> {
    tauri_plugin_opener::open_url(&url, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn check_for_update(state: State<'_, AppState>) -> Result<Value, ()> {
    Ok(crate::update::check_for_update(&state).await)
}
