// Enumerating, killing, trimming and prioritising the Roblox instances this
// app launched.
use super::{now_ms, watch};
use crate::state::AppState;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Keeps a console tool from flashing a window.
pub fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    #[cfg(not(windows))]
    let _ = cmd;
}

pub(crate) fn no_window_std(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Full path to a tool in System32, invoked directly rather than through
/// `cmd /c`: no shell parsing of the arguments, one process fewer per call,
/// and no chance of a same-named exe earlier on the search path being run
/// instead. Falls back to the bare name if SystemRoot is somehow unset.
pub fn system_tool(exe: &str) -> PathBuf {
    if let Ok(root) = std::env::var("SystemRoot") {
        let p = PathBuf::from(root).join("System32").join(exe);
        if p.exists() {
            return p;
        }
    }
    PathBuf::from(exe)
}

/// `taskkill /F /T` with the given selectors, windowless, bounded by
/// `timeout`. /T takes each target's own children (RobloxCrashHandler) with
/// it. Errors are logged rather than returned: every caller goes on to
/// verify the outcome its own way.
pub(crate) async fn taskkill(selectors: &[String], timeout: Duration) {
    if !cfg!(windows) {
        return;
    }
    let mut cmd = Command::new(system_tool("taskkill.exe"));
    cmd.args(["/F", "/T"]).args(selectors);
    no_window(&mut cmd);
    cmd.kill_on_drop(true);
    match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => eprintln!("[taskkill] {e}"),
        Err(_) => eprintln!("[taskkill] timed out"),
    }
}

// Compiled once rather than per call; the pattern is a constant, so the
// unwrap can only ever fire on first use.
static TASKLIST_PID_RE: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r#""RobloxPlayerBeta\.exe","(\d+)""#).unwrap());

// None means tasklist failed to run; callers must not treat that as
// "confirmed zero processes" or a transient spawn failure looks like a
// closed account.
async fn tasklist(filter_image: &str) -> Option<String> {
    if !cfg!(windows) {
        return Some(String::new());
    }
    let mut cmd = Command::new(system_tool("tasklist.exe"));
    cmd.args([
        "/FI",
        &format!("IMAGENAME eq {}", filter_image),
        "/FO",
        "CSV",
        "/NH",
    ]);
    no_window(&mut cmd);
    cmd.kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(10), cmd.output()).await {
        Ok(Ok(out)) => Some(String::from_utf8_lossy(&out.stdout).into_owned()),
        _ => None,
    }
}

fn parse_pid_list(payload: &str) -> HashSet<u32> {
    payload
        .split(',')
        .filter_map(|s| s.trim().parse::<u32>().ok())
        .collect()
}

// Asks the resident helper instead of shelling out and regex-parsing CSV.
// Falls back to tasklist() only if the helper can't be reached at all.
async fn native_pids(app: &AppHandle, state: &AppState) -> Option<HashSet<u32>> {
    if !cfg!(windows) {
        return Some(HashSet::new());
    }
    match crate::helper::call(app, state, "pids", Duration::from_secs(10)).await {
        Ok(payload) => Some(parse_pid_list(&payload)),
        Err(_) => {
            let out = tasklist("RobloxPlayerBeta.exe").await?;
            Some(
                TASKLIST_PID_RE
                    .captures_iter(&out)
                    .filter_map(|c| c[1].parse::<u32>().ok())
                    .collect(),
            )
        }
    }
}

/// The watcher's last report if it is running, else a one-off enumeration.
pub(super) async fn cached_or_spawn_pids(
    app: &AppHandle,
    state: &AppState,
) -> Option<HashSet<u32>> {
    if let Some(pids) = state.watch_pid_cache.lock().unwrap().clone() {
        return Some(pids);
    }
    native_pids(app, state).await
}

pub async fn set_roblox_volume(app: &AppHandle, state: &AppState, percent: f64) -> Value {
    if !cfg!(windows) {
        return json!({ "ok": false, "count": 0, "error": "Windows only" });
    }
    let pct = percent.round().clamp(0.0, 100.0) as i64;
    let cmd = format!("volume|{}", pct);
    match crate::helper::call(app, state, &cmd, crate::helper::DEFAULT_TIMEOUT).await {
        Ok(payload) => {
            let count = payload.trim().parse::<i64>().unwrap_or(0);
            json!({ "ok": true, "count": count })
        }
        Err(e) => json!({ "ok": false, "count": 0, "error": e }),
    }
}

// Instances this app launched and can still see, which is exactly the set
// Kill acts on. Counting every RobloxPlayerBeta.exe on the machine would
// report windows the user opened themselves; and then Kill would leave them
// running, so the badge and the button would disagree.
pub(super) fn owned_running_count(state: &AppState, alive: &HashSet<u32>) -> u32 {
    state
        .account_pids
        .lock()
        .unwrap()
        .values()
        .filter(|pid| alive.contains(pid))
        .count() as u32
}

pub async fn count_roblox_processes(app: &AppHandle, state: &AppState) -> u32 {
    match cached_or_spawn_pids(app, state).await {
        Some(alive) => owned_running_count(state, &alive),
        None => 0,
    }
}

// Just hands idle physical pages back to the OS; doesn't touch the
// process itself, can't crash it.
#[cfg(windows)]
fn trim_process_memory(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::ProcessStatus::EmptyWorkingSet;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_SET_QUOTA,
    };
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_SET_QUOTA, false, pid)
        else {
            return false;
        };
        let ok = EmptyWorkingSet(handle).is_ok();
        let _ = CloseHandle(handle);
        ok
    }
}
#[cfg(not(windows))]
fn trim_process_memory(_pid: u32) -> bool {
    false
}

// Matches Task Manager's own priority names/mapping (its "Low" is
// IDLE_PRIORITY_CLASS, not a literal low-priority constant).
#[cfg(windows)]
fn priority_class_from_name(
    name: &str,
) -> Option<windows::Win32::System::Threading::PROCESS_CREATION_FLAGS> {
    use windows::Win32::System::Threading::{
        ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS,
        IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, REALTIME_PRIORITY_CLASS,
    };
    Some(match name {
        "realtime" => REALTIME_PRIORITY_CLASS,
        "high" => HIGH_PRIORITY_CLASS,
        "abovenormal" => ABOVE_NORMAL_PRIORITY_CLASS,
        "normal" => NORMAL_PRIORITY_CLASS,
        "belownormal" => BELOW_NORMAL_PRIORITY_CLASS,
        "low" => IDLE_PRIORITY_CLASS,
        _ => return None,
    })
}

#[cfg(windows)]
fn set_process_priority(pid: u32, class_name: &str) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, SetPriorityClass, PROCESS_SET_INFORMATION,
    };
    let Some(class) = priority_class_from_name(class_name) else {
        return false;
    };
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_SET_INFORMATION, false, pid) else {
            return false;
        };
        let ok = SetPriorityClass(handle, class).is_ok();
        let _ = CloseHandle(handle);
        ok
    }
}
#[cfg(not(windows))]
fn set_process_priority(_pid: u32, _class_name: &str) -> bool {
    false
}

// Manually set priorities stick; recorded here so apply_priority_policy's
// automatic multi-instance rule leaves this account alone until it's
// relaunched.
pub fn set_account_priority(state: &AppState, account_id: &str, class_name: &str) -> Value {
    let Some(pid) = state.account_pids.lock().unwrap().get(account_id).copied() else {
        return json!({ "ok": false, "error": "Account isn't running" });
    };
    if set_process_priority(pid, class_name) {
        state
            .manual_priority
            .lock()
            .unwrap()
            .insert(account_id.to_string(), class_name.to_string());
        json!({ "ok": true })
    } else {
        json!({ "ok": false, "error": "Failed to set priority" })
    }
}

pub(super) fn clear_manual_priority(state: &AppState, account_id: &str) {
    state.manual_priority.lock().unwrap().remove(account_id);
}

// User opt-in (Settings -> Performance): once more than one account is
// running, every instance drops to Below Normal, which suits background farming
// but also lowers the one being played, hence off by default. Re-evaluated after
// every launch/kill and when the setting changes, so instances go back to Normal
// once only one is left or the setting is turned off. Accounts with a manual
// override (right-click -> Set priority) are skipped so the automatic rule
// doesn't stomp on it.
pub fn apply_priority_policy(state: &AppState) {
    let enabled = crate::settings::get_bool("lowPriorityMultiInstance", false);
    let accounts: Vec<(String, u32)> = state
        .account_pids
        .lock()
        .unwrap()
        .iter()
        .map(|(id, pid)| (id.clone(), *pid))
        .collect();
    let class_name = if enabled && accounts.len() > 1 {
        "belownormal"
    } else {
        "normal"
    };
    let overridden = state.manual_priority.lock().unwrap();
    for (account_id, pid) in accounts {
        if !overridden.contains_key(&account_id) {
            set_process_priority(pid, class_name);
        }
    }
}

pub async fn trim_roblox_memory(app: &AppHandle, state: &AppState) -> Value {
    let Some(alive_pids) = cached_or_spawn_pids(app, state).await else {
        return json!({ "ok": false, "trimmed": 0, "total": 0, "error": "could not enumerate Roblox processes" });
    };

    // Only instances this app launched, like Kill and the running count; a
    // Roblox the user opened themselves isn't ours to touch.
    let owned: HashSet<u32> = state
        .account_pids
        .lock()
        .unwrap()
        .values()
        .copied()
        .collect();

    // Skip PIDs still inside their launch grace window; trimming while a
    // client is still loading assets can cause a stutter.
    let now = now_ms();
    let launching_pids: HashSet<u32> = {
        let watched = state.watched_accounts.lock().unwrap();
        let pids = state.account_pids.lock().unwrap();
        watched
            .iter()
            .filter(|(_, ready_at)| now < **ready_at)
            .filter_map(|(id, _)| pids.get(id).copied())
            .collect()
    };

    let mut total = 0u32;
    let mut trimmed = 0u32;
    for pid in alive_pids {
        if !owned.contains(&pid) || launching_pids.contains(&pid) {
            continue;
        }
        total += 1;
        if trim_process_memory(pid) {
            trimmed += 1;
        }
    }
    json!({ "ok": true, "trimmed": trimmed, "total": total })
}

pub async fn trim_account_memory(app: &AppHandle, state: &AppState, account_id: &str) -> Value {
    let Some(pid) = state.account_pids.lock().unwrap().get(account_id).copied() else {
        return json!({ "ok": false, "error": "No tracked Roblox instance for this account" });
    };
    if state
        .watched_accounts
        .lock()
        .unwrap()
        .get(account_id)
        .is_some_and(|ready_at| now_ms() < *ready_at)
    {
        return json!({ "ok": false, "error": "Instance is still launching" });
    }
    let Some(alive_pids) = cached_or_spawn_pids(app, state).await else {
        return json!({ "ok": false, "error": "Could not enumerate Roblox processes" });
    };
    if !alive_pids.contains(&pid) {
        return json!({ "ok": false, "error": "This Roblox instance is no longer running" });
    }
    if trim_process_memory(pid) {
        json!({ "ok": true })
    } else {
        json!({ "ok": false, "error": "Could not trim this Roblox instance" })
    }
}

// Waits for specific PIDs to disappear. Scoped to our own processes: a
// blanket "no RobloxPlayerBeta.exe anywhere" wait would never be satisfied
// while an instance the user started outside Vanta Manager is still open.
async fn wait_for_pids_closed(app: &AppHandle, state: &AppState, pids: &[u32], max_wait: Duration) {
    let started = std::time::Instant::now();
    loop {
        match native_pids(app, state).await {
            // Couldn't enumerate; no point spinning on an unanswerable question.
            None => return,
            Some(alive) if !pids.iter().any(|p| alive.contains(p)) => return,
            _ => {}
        }
        if started.elapsed() >= max_wait {
            return;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

fn pid_selectors(pids: &[u32]) -> Vec<String> {
    pids.iter()
        .flat_map(|p| ["/PID".to_string(), p.to_string()])
        .collect()
}

pub async fn kill_all_roblox(app: &AppHandle, state: &AppState) -> Value {
    // First, so a launch that is mid-flight or waiting its turn bails at its
    // next checkpoint instead of spawning after we have already killed and
    // reported. Without this, kill during a package launch is not a stop.
    super::launch::cancel_all_launches(state);
    let watched_ids: Vec<String> = state
        .watched_accounts
        .lock()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    // Snapshot before clearing: these PIDs are the ones this app launched and
    // the only ones it has any business killing.
    let pids: Vec<u32> = state
        .account_pids
        .lock()
        .unwrap()
        .values()
        .copied()
        .collect();
    // Watched accounts with no attributed PID can't be targeted; report them
    // rather than quietly leaving them running.
    let untracked = watched_ids.len().saturating_sub(pids.len());

    state.watched_accounts.lock().unwrap().clear();
    state.miss_counts.lock().unwrap().clear();
    state.account_pids.lock().unwrap().clear();
    state.manual_priority.lock().unwrap().clear();
    watch::stop_watch_poll_if_idle(state);

    let notify = |ids: &[String]| {
        for id in ids {
            let _ = app.emit("roblox:closed", id);
        }
        let _ = app.emit("roblox:allClosed", ());
    };

    if !cfg!(windows) {
        notify(&watched_ids);
        return json!({ "ok": false, "error": "Windows only" });
    }
    if pids.is_empty() {
        notify(&watched_ids);
        return json!({ "ok": true, "killed": 0, "untracked": untracked });
    }

    // One taskkill listing every PID; /T also takes each client's
    // RobloxCrashHandler, which is why that image no longer needs killing
    // by name.
    taskkill(&pid_selectors(&pids), Duration::from_secs(6)).await;
    wait_for_pids_closed(app, state, &pids, Duration::from_secs(5)).await;
    super::daemon::restart_mutex_holder(app, state).await;
    notify(&watched_ids);
    json!({ "ok": true, "killed": pids.len(), "untracked": untracked })
}

pub async fn kill_account_roblox(app: &AppHandle, state: &AppState, account_id: &str) -> Value {
    // Same reasoning as kill_all_roblox: killing an account whose launch has
    // not finished must abandon that launch, not race it.
    super::launch::cancel_launch(state, account_id);
    let pid = state.account_pids.lock().unwrap().remove(account_id);
    state.watched_accounts.lock().unwrap().remove(account_id);
    state.miss_counts.lock().unwrap().remove(account_id);
    clear_manual_priority(state, account_id);
    watch::stop_watch_poll_if_idle(state);

    let notify = || {
        let _ = app.emit("roblox:closed", account_id);
    };

    if !cfg!(windows) {
        notify();
        return json!({ "ok": false, "error": "Windows only" });
    }
    let Some(pid) = pid else {
        notify();
        return json!({ "ok": false, "error": "No tracked process for this account" });
    };
    taskkill(&pid_selectors(&[pid]), Duration::from_secs(4)).await;
    notify();
    apply_priority_policy(state);
    json!({ "ok": true })
}

// Runs before every launch spawn. One request on the pipe the helper already
// holds open, where it used to be a fresh one-shot process each time.
pub(super) async fn close_singleton_handles(app: &AppHandle, state: &AppState) {
    let stable_pids: Vec<String> = state
        .account_pids
        .lock()
        .unwrap()
        .values()
        .map(u32::to_string)
        .collect();
    let cmd = if stable_pids.is_empty() {
        "closehandles".to_string()
    } else {
        format!("closehandles|{}", stable_pids.join(","))
    };
    if let Err(e) = crate::helper::call(app, state, &cmd, crate::helper::SLOW_TIMEOUT).await {
        eprintln!("[closehandles] {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_lists_parse_and_skip_junk() {
        let set = parse_pid_list("12, 34,,x,56");
        assert_eq!(set, HashSet::from([12, 34, 56]));
        assert!(parse_pid_list("").is_empty());
    }

    #[test]
    fn taskkill_selectors_pair_each_pid() {
        assert_eq!(pid_selectors(&[1, 22]), ["/PID", "1", "/PID", "22"]);
    }

    #[test]
    fn tasklist_csv_rows_yield_pids() {
        let out = "\"RobloxPlayerBeta.exe\",\"4242\",\"Console\",\"1\",\"500,000 K\"\r\n";
        let pids: Vec<u32> = TASKLIST_PID_RE
            .captures_iter(out)
            .filter_map(|c| c[1].parse().ok())
            .collect();
        assert_eq!(pids, [4242]);
    }
}
