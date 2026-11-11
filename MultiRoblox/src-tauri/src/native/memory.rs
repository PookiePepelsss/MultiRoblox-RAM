// Per-instance RAM limiter and memory readout.
//
// The limit is a hard working-set maximum (SetProcessWorkingSetSizeEx with
// QUOTA_LIMITS_HARDWS_MAX_ENABLE). Windows keeps the instance's physical RAM
// at or under the cap and pages the rest out; nothing is injected and no
// allocation ever fails, so a too-low cap shows up as stutter, never as a
// crash. That is the deliberate choice over a job-object commit limit, which
// would make Roblox's own allocations fail and kill the client.
//
// The cap comes from the account's own `ramLimitMb` when it is set (> 0),
// else from the global `ramLimitMb` setting; 0 or missing means no limit.
use crate::state::AppState;
use crate::storage::{self, account_id as id_of};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

pub const RAM_LIMIT_KEY: &str = "ramLimitMb";
/// Below this Roblox cannot keep its working set resident at all and
/// becomes unusable, so smaller values are raised to it.
pub const MIN_LIMIT_MB: u64 = 512;
/// 1 TB. Anything above is a typo; without a ceiling an absurd value
/// saturated to u64::MAX and wrapped when converted to bytes.
pub const MAX_LIMIT_MB: u64 = 1_048_576;
/// A client is still streaming its first assets for a while after spawning;
/// capping it then turns loading into a crawl, so the cap waits for this.
const APPLY_DELAY: Duration = Duration::from_secs(20);

fn clamp_mb(n: f64) -> u64 {
    (n.round() as u64).clamp(MIN_LIMIT_MB, MAX_LIMIT_MB)
}

fn limit_from(v: Option<&Value>) -> u64 {
    v.and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n > 0.0)
        .map(clamp_mb)
        .unwrap_or(0)
}

fn global_limit_mb() -> u64 {
    limit_from(crate::settings::snapshot().get(RAM_LIMIT_KEY))
}

/// The account's own cap if it has one, else the global one.
fn limit_in(accounts: &[Value], account_id: &str, global_mb: u64) -> u64 {
    let own = accounts
        .iter()
        .find(|a| id_of(a) == Some(account_id))
        .map(|a| limit_from(a.get(RAM_LIMIT_KEY)))
        .unwrap_or(0);
    if own > 0 {
        own
    } else {
        global_mb
    }
}

/// The cap in MB that applies to an account, 0 for none.
pub fn effective_limit_mb(state: &AppState, account_id: &str) -> u64 {
    limit_in(&storage::load_accounts(state), account_id, global_limit_mb())
}

#[cfg(windows)]
fn set_working_set_cap(pid: u32, limit_mb: u64) -> Result<(), String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Memory::{
        SetProcessWorkingSetSizeEx, QUOTA_LIMITS_HARDWS_MAX_DISABLE,
        QUOTA_LIMITS_HARDWS_MAX_ENABLE, QUOTA_LIMITS_HARDWS_MIN_DISABLE,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_SET_QUOTA,
    };
    const MB: usize = 1024 * 1024;
    unsafe {
        let handle = OpenProcess(PROCESS_SET_QUOTA | PROCESS_QUERY_INFORMATION, false, pid)
            .map_err(|e| format!("Could not open the Roblox process: {}", e.message()))?;
        // The minimum is only a soft hint (MIN_DISABLE); Windows requires it
        // to be below the maximum. Removing the cap passes Windows' defaults
        // back with both hard limits disabled.
        let result = if limit_mb == 0 {
            SetProcessWorkingSetSizeEx(
                handle,
                200 * 1024,
                1380 * 1024,
                QUOTA_LIMITS_HARDWS_MIN_DISABLE | QUOTA_LIMITS_HARDWS_MAX_DISABLE,
            )
        } else {
            let max = usize::try_from(limit_mb)
                .ok()
                .and_then(|mb| mb.checked_mul(MB))
                .ok_or("RAM limit is too large")?;
            SetProcessWorkingSetSizeEx(
                handle,
                (max / 8).min(64 * MB),
                max,
                QUOTA_LIMITS_HARDWS_MIN_DISABLE | QUOTA_LIMITS_HARDWS_MAX_ENABLE,
            )
        };
        let _ = CloseHandle(handle);
        result.map_err(|e| format!("Windows refused the memory limit: {}", e.message()))
    }
}

#[cfg(not(windows))]
fn set_working_set_cap(_pid: u32, _limit_mb: u64) -> Result<(), String> {
    Err("Windows only".into())
}

/// (working set, private bytes) in bytes.
#[cfg(windows)]
fn memory_of(pid: u32) -> Option<(u64, u64)> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };
    unsafe {
        let handle = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
            false,
            pid,
        )
        .or_else(|_| OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid))
        .ok()?;
        let mut c = PROCESS_MEMORY_COUNTERS_EX {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        let ok = GetProcessMemoryInfo(
            handle,
            (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            c.cb,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        ok.then_some((c.WorkingSetSize as u64, c.PrivateUsage as u64))
    }
}

#[cfg(not(windows))]
fn memory_of(_pid: u32) -> Option<(u64, u64)> {
    None
}

fn pid_of(state: &AppState, account_id: &str) -> Option<u32> {
    state.account_pids.lock().unwrap().get(account_id).copied()
}

/// Applies (or lifts) the account's cap on its running instance. Ok(mb) is
/// the cap now in force, 0 meaning none.
pub fn apply_limit(state: &AppState, account_id: &str) -> Result<u64, String> {
    let pid = pid_of(state, account_id).ok_or("This account isn't running")?;
    let mb = effective_limit_mb(state, account_id);
    set_working_set_cap(pid, mb)?;
    Ok(mb)
}

/// Re-applies caps to every running instance, e.g. after the global limit
/// changed. Failures are logged, not returned: one instance refusing must not
/// stop the rest.
pub fn apply_all(app: &AppHandle, state: &AppState) {
    let ids: Vec<(String, u32)> = state
        .account_pids
        .lock()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    for (id, pid) in ids {
        // Still loading: its scheduled apply caps it once loading is over.
        if is_loading(pid) {
            continue;
        }
        if let Err(e) = apply_limit(state, &id) {
            super::emit_log(
                app,
                "warn",
                "system",
                &format!("RAM limit not applied: {e}"),
                Some(json!({ "accountId": id })),
            );
        }
    }
}

// Instances still inside APPLY_DELAY, by pid, so a change of the global limit
// doesn't cap a client that is still loading.
static LOADING: once_cell::sync::Lazy<Mutex<HashMap<u32, Instant>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

fn loading() -> std::sync::MutexGuard<'static, HashMap<u32, Instant>> {
    LOADING.lock().unwrap_or_else(|e| e.into_inner())
}

fn is_loading(pid: u32) -> bool {
    loading()
        .get(&pid)
        .is_some_and(|t| t.elapsed() < APPLY_DELAY)
}

/// Called right after a launch spawned `pid`: caps it once loading is over,
/// provided the same instance is still the account's.
pub(super) fn schedule_after_launch(app: &AppHandle, account_id: &str, pid: u32) {
    loading().insert(pid, Instant::now());
    let app = app.clone();
    let account_id = account_id.to_string();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(APPLY_DELAY).await;
        loading().remove(&pid);
        let state = app.state::<AppState>();
        if pid_of(&state, &account_id) != Some(pid) || effective_limit_mb(&state, &account_id) == 0
        {
            return;
        }
        match apply_limit(&state, &account_id) {
            Ok(mb) => super::emit_log(
                &app,
                "info",
                "system",
                &format!("RAM limit of {mb} MB applied"),
                Some(json!({ "accountId": account_id, "pid": pid, "limitMb": mb })),
            ),
            Err(e) => super::emit_log(
                &app,
                "warn",
                "system",
                &format!("RAM limit not applied: {e}"),
                Some(json!({ "accountId": account_id })),
            ),
        }
    });
}

/// Saves an account's own cap (0 clears it, falling back to the global one)
/// and applies it at once if the account is running.
pub fn set_account_limit(state: &AppState, account_id: &str, mb: f64) -> Value {
    let value = if mb.is_finite() && mb > 0.0 {
        json!(clamp_mb(mb))
    } else {
        json!(0)
    };
    let saved = storage::update_accounts(state, |list| {
        let obj = list
            .iter_mut()
            .find(|a| id_of(a) == Some(account_id))?
            .as_object_mut()?;
        obj.insert(RAM_LIMIT_KEY.into(), value.clone());
        Some(())
    });
    match saved {
        Ok(Some(())) => {}
        Ok(None) => return json!({ "ok": false, "error": "Account not found" }),
        Err(e) => return json!({ "ok": false, "error": e }),
    }
    if pid_of(state, account_id).is_none() {
        return json!({ "ok": true, "applied": false, "limitMb": effective_limit_mb(state, account_id) });
    }
    match apply_limit(state, account_id) {
        Ok(mb) => json!({ "ok": true, "applied": true, "limitMb": mb }),
        // The value is already on disk; `saved` tells the UI to keep it even
        // though the running instance refused it.
        Err(e) => json!({ "ok": false, "saved": true, "error": e }),
    }
}

/// Memory per running account: { id: { workingSetMb, privateMb, limitMb, pid } }.
pub fn usage(state: &AppState) -> Value {
    let pids: Vec<(String, u32)> = state
        .account_pids
        .lock()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    if pids.is_empty() {
        return Value::Object(Map::new());
    }
    // Read once for the whole pass: this runs every few seconds, and asking per
    // account copied the entire list once for each running instance.
    let accounts = storage::load_accounts(state);
    let global_mb = global_limit_mb();
    let cpu = super::sysstats::process_cpu(&pids.iter().map(|(_, pid)| *pid).collect::<Vec<_>>());
    let mut out = Map::new();
    for (id, pid) in pids {
        let Some((ws, private)) = memory_of(pid) else {
            continue;
        };
        out.insert(
            id.clone(),
            json!({
                "pid": pid,
                "workingSetMb": ws / (1024 * 1024),
                "privateMb": private / (1024 * 1024),
                "limitMb": limit_in(&accounts, &id, global_mb),
                "cpuPercent": cpu.get(&pid),
            }),
        );
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_limit_wins_over_the_global_one() {
        let accounts = [json!({"id":"a","ramLimitMb":2048}), json!({"id":"b"})];
        assert_eq!(limit_in(&accounts, "a", 1024), 2048);
        assert_eq!(limit_in(&accounts, "b", 1024), 1024);
        assert_eq!(limit_in(&accounts, "gone", 1024), 1024);
        assert_eq!(limit_in(&accounts, "b", 0), 0);
    }

    #[test]
    fn limits_are_parsed_and_floored() {
        assert_eq!(limit_from(None), 0);
        assert_eq!(limit_from(Some(&json!(0))), 0);
        assert_eq!(limit_from(Some(&json!(-5))), 0);
        assert_eq!(limit_from(Some(&json!("2048"))), 0, "only numbers count");
        assert_eq!(limit_from(Some(&json!(100))), MIN_LIMIT_MB);
        assert_eq!(limit_from(Some(&json!(2048.4))), 2048);
        assert_eq!(limit_from(Some(&json!(1e20))), MAX_LIMIT_MB);
    }
}
