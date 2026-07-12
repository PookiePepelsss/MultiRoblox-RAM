// Port of main.js's native-helper orchestration (mutex holder, anti-AFK,
// volume, kill/count, launch, watch loop). Behaviour, timing constants, and
// comments explaining *why* are carried over unchanged from the Electron
// source -- this is a port, not a redesign.
use crate::paths::app_data_dir;
use crate::state::AppState;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

fn hide_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
}

// ---- native helper (RobloxNative.exe) resolution ----
// Prefer a prebuilt exe shipped in resources (built by build.bat). If missing,
// compile the bundled .cs source once via the .NET Framework csc.exe (present
// on every Windows machine) and cache the result in the app data dir.
//
// Tauri's resource_dir() returns a `\\?\`-prefixed extended-length path on
// Windows. csc.exe's older argument parser chokes on that prefix, so it must
// be stripped before being handed to csc -- the exact bug hit (and fixed) the
// first time this migration was built.
fn strip_verbatim_prefix(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p.to_path_buf()
    }
}

fn native_src_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().resource_dir().ok().map(|d| strip_verbatim_prefix(&d).join("resources").join("RobloxNative.cs"))
}
fn bundled_native_exe_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().resource_dir().ok().map(|d| strip_verbatim_prefix(&d).join("resources").join("RobloxNative.exe"))
}

fn find_csc() -> Option<PathBuf> {
    let win = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".to_string());
    for c in [
        Path::new(&win).join("Microsoft.NET").join("Framework64").join("v4.0.30319").join("csc.exe"),
        Path::new(&win).join("Microsoft.NET").join("Framework").join("v4.0.30319").join("csc.exe"),
    ] {
        if c.exists() {
            return Some(c);
        }
    }
    None
}

// Memoized for the app session: Some(Some(path)) = usable exe, Some(None) =
// resolved-to-unavailable, None = not yet resolved.
pub async fn ensure_native_helper(app: &AppHandle, state: &AppState) -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    {
        let cached = state.native_helper_path.lock().unwrap().clone();
        if let Some(resolved) = cached {
            return resolved;
        }
    }
    let resolved = resolve_native_helper(app).await;
    *state.native_helper_path.lock().unwrap() = Some(resolved.clone());
    resolved
}

async fn resolve_native_helper(app: &AppHandle) -> Option<PathBuf> {
    if let Some(b) = bundled_native_exe_path(app) {
        if b.exists() {
            return Some(b);
        }
    }
    let src = native_src_path(app)?;
    if !src.exists() {
        return None;
    }
    let out_exe = app_data_dir().join("RobloxNative.exe");
    if let (Ok(out_meta), Ok(src_meta)) = (std::fs::metadata(&out_exe), std::fs::metadata(&src)) {
        if let (Ok(out_t), Ok(src_t)) = (out_meta.modified(), src_meta.modified()) {
            if out_t >= src_t {
                return Some(out_exe);
            }
        }
    }
    let csc = find_csc()?;
    let mut cmd = Command::new(&csc);
    cmd.args([
        "/nologo",
        "/optimize+",
        "/platform:x64",
        "/target:exe",
        &format!("/out:{}", out_exe.display()),
        &src.to_string_lossy(),
    ]);
    hide_window(&mut cmd);
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    let ok = match cmd.output_timeout(Duration::from_secs(30)).await {
        Some(Ok(output)) => output.status.success() && out_exe.exists(),
        _ => out_exe.exists(),
    };
    if ok {
        Some(out_exe)
    } else {
        None
    }
}

// tokio::process::Child has no built-in timeout on output(); small helper to
// mirror the 30s safety timeout in the Electron version.
trait OutputTimeout {
    async fn output_timeout(self, dur: Duration) -> Option<std::io::Result<std::process::Output>>;
}
impl OutputTimeout for Command {
    async fn output_timeout(mut self, dur: Duration) -> Option<std::io::Result<std::process::Output>> {
        tokio::time::timeout(dur, self.output()).await.ok()
    }
}

// ---- mutex holder ----
// Windows only releases a named mutex when the owning process fully exits.
// stop_mutex_holder awaits the real child-exit event (not just kill()) so a
// respawned holder never races the OS for the handle -- see the Electron
// source's big comment on this exact hazard.
pub async fn start_mutex_holder(app: &AppHandle, state: &AppState) {
    {
        let guard = state.mutex_child.lock().unwrap();
        if guard.is_some() {
            return;
        }
    }
    let Some(exe) = ensure_native_helper(app, state).await else {
        eprintln!("[mutex] native helper unavailable");
        return;
    };
    let mut cmd = Command::new(&exe);
    cmd.arg("mutex").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    hide_window(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return,
    };
    if let Some(stdout) = child.stdout.take() {
        let held = std::sync::Arc::new(AtomicBool::new(false));
        let held2 = held.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.contains("MUTEX_HELD") {
                    held2.store(true, Ordering::SeqCst);
                }
            }
        });
        // Safety fallback only, mirroring the 8s timeout in main.js: normally
        // MUTEX_HELD prints almost immediately, well before the slow handle scan.
        for _ in 0..80 {
            if held.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    eprintln!("[mutex] {}", line.trim());
                }
            }
        });
    }
    *state.mutex_child.lock().unwrap() = Some(child);
}

// Fire-and-forget variant for callers that don't need to know when the OS has
// actually released the mutex (settings toggle, app-exit). start_kill() is a
// synchronous syscall (TerminateProcess), so the signal goes out immediately
// even though this fn isn't async -- only the wait-for-exit is backgrounded.
pub fn stop_mutex_holder(state: &AppState) {
    let child = state.mutex_child.lock().unwrap().take();
    if let Some(mut child) = child {
        let _ = child.start_kill();
        tauri::async_runtime::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        });
    }
}

// Awaitable variant: blocks until the holder process has actually torn down
// (or 2s elapses), not just until kill() was requested. Windows only releases
// a named mutex when the owning process fully exits, so anything that
// respawns a new holder right after calling kill() is racing the OS: if the
// new holder's Mutex(true, "ROBLOX_singletonMutex", out created) runs before
// the old process's handle is actually gone, `created` comes back false and
// RobloxNative's fallback WaitOne(0) can return false (mutex still held
// elsewhere) WITHOUT throwing -- the code doesn't check that return value, so
// it prints MUTEX_HELD and this app believes the mutex is held even though it
// isn't. That's the exact "unheld mutex" scenario that corrupts Roblox's
// install pipeline. Awaiting the real exit event here closes that window.
async fn stop_mutex_holder_and_wait(state: &AppState) {
    let child = state.mutex_child.lock().unwrap().take();
    if let Some(mut child) = child {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    }
}

pub async fn restart_mutex_holder(app: &AppHandle, state: &AppState) {
    stop_mutex_holder_and_wait(state).await; // wait for the OS to actually release the old mutex handle first
    start_mutex_holder(app, state).await;
}

// ---- anti-AFK ----
pub async fn start_antiafk(app: &AppHandle, state: &AppState) {
    if !cfg!(windows) {
        return;
    }
    {
        if state.antiafk_child.lock().unwrap().is_some() {
            return;
        }
    }
    let Some(exe) = ensure_native_helper(app, state).await else {
        eprintln!("[antiafk] native helper unavailable; cannot run anti-AFK");
        return;
    };
    let s = crate::settings::load_settings();
    let mut deadline = s.get("antiAfkInterval").and_then(|v| v.as_i64()).unwrap_or(0);
    if deadline < 60 {
        deadline = 19 * 60; // 19 min, under the ~20-min idle kick
    }
    let mut cmd = Command::new(&exe);
    cmd.args(["antiafk", &deadline.to_string()]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    hide_window(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[antiafk] spawn failed: {}", e);
            return;
        }
    };
    emit_log(app, "ok", "afk", &format!("Anti-AFK started (interval: {} min)", deadline / 60), Some(serde_json::json!({ "intervalSec": deadline })));

    if let Some(stdout) = child.stdout.take() {
        let app2 = app.clone();
        tokio::spawn(async move {
            let mw = regex::Regex::new(r"(?i)tapped\s+(\d+)\s+window").unwrap();
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let t = line.trim();
                if t.is_empty() {
                    continue;
                }
                if let Some(caps) = mw.captures(t) {
                    let n = &caps[1];
                    let plural = if n == "1" { "" } else { "s" };
                    emit_log(&app2, "info", "afk", &format!("Anti-AFK: tapped {} Roblox window{}", n, plural), Some(serde_json::json!({ "windows": n.parse::<i64>().unwrap_or(0) })));
                } else {
                    emit_log(&app2, "info", "afk", &format!("Anti-AFK: {}", t), None);
                }
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let app2 = app.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let t = line.trim();
                if !t.is_empty() {
                    eprintln!("[antiafk] {}", t);
                    emit_log(&app2, "warn", "afk", &format!("Anti-AFK warning: {}", t), None);
                }
            }
        });
    }
    *state.antiafk_child.lock().unwrap() = Some(child);
}

pub fn stop_antiafk(state: &AppState) {
    emit_log(&state.app_handle, "warn", "afk", "Anti-AFK stopped", None);
    let child = state.antiafk_child.lock().unwrap().take();
    if let Some(mut child) = child {
        let _ = child.start_kill(); // synchronous signal -- must not be deferred into a spawn, or app-exit can race past it unsent
    }
}

// renderer.js's onLogEntry handler reads data.meta as a nested object
// (logEntry(data.level, data.category, data.message, data.meta)) and stamps
// its own Date.now() ts, ignoring whatever ts we send -- so `extra` must
// stay nested under "meta", not flattened into the top level.
pub fn emit_log(app: &AppHandle, level: &str, category: &str, message: &str, extra: Option<Value>) {
    let payload = serde_json::json!({
        "level": level,
        "category": category,
        "message": message,
        "meta": extra.unwrap_or_else(|| Value::Object(serde_json::Map::new())),
    });
    let _ = app.emit("log:entry", payload);
}

// ---- Roblox process helpers ----
// None = tasklist itself failed to run (no signal either way). Callers must
// NOT treat that the same as "empty output" (= confirmed zero processes),
// otherwise a transient spawn failure falsely reads as "nothing running" and
// watch_tick() counts it as a miss against accounts that are still alive.
async fn tasklist(filter_image: &str) -> Option<String> {
    if !cfg!(windows) {
        return Some(String::new());
    }
    let mut cmd = Command::new("cmd");
    cmd.args(["/c", &format!(r#"tasklist /FI "IMAGENAME eq {}" /FO CSV /NH"#, filter_image)]);
    hide_window(&mut cmd);
    match cmd.output().await {
        Ok(out) => Some(String::from_utf8_lossy(&out.stdout).to_string()),
        Err(_) => None,
    }
}

pub async fn set_roblox_volume(app: &AppHandle, state: &AppState, percent: f64) -> Value {
    if !cfg!(windows) {
        return serde_json::json!({ "ok": false, "count": 0, "error": "Windows only" });
    }
    let pct = percent.round().clamp(0.0, 100.0) as i64;
    let Some(exe) = ensure_native_helper(app, state).await else {
        return serde_json::json!({ "ok": false, "count": 0, "error": "native helper unavailable" });
    };
    let mut cmd = Command::new(&exe);
    cmd.args(["volume", &pct.to_string()]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    hide_window(&mut cmd);
    match tokio::time::timeout(Duration::from_secs(12), cmd.output()).await {
        Ok(Ok(out)) => {
            let s = String::from_utf8_lossy(&out.stdout);
            let re = regex::Regex::new(r"SET:(\d+)").unwrap();
            let count = re.captures(&s).and_then(|c| c[1].parse::<i64>().ok()).unwrap_or(0);
            serde_json::json!({ "ok": true, "count": count })
        }
        Ok(Err(e)) => serde_json::json!({ "ok": false, "count": 0, "error": e.to_string() }),
        Err(_) => serde_json::json!({ "ok": true, "count": 0 }),
    }
}

pub async fn count_roblox_processes() -> u32 {
    let out = tasklist("RobloxPlayerBeta.exe").await.unwrap_or_default();
    let re = regex::Regex::new(r"(?i)RobloxPlayerBeta\.exe").unwrap();
    re.find_iter(&out).count() as u32
}

// EmptyWorkingSet only forces the process to give back currently-idle
// physical pages back to the OS -- it doesn't touch the process itself
// (no thread/handle to the game logic, nothing killed or suspended), so
// it can't crash Roblox and has zero effect on the tasklist-based
// alive/closed detection above. Pages the process actually needs get
// paged back in on next touch, same as normal working-set trimming.
#[cfg(windows)]
fn trim_process_memory(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::ProcessStatus::EmptyWorkingSet;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_SET_QUOTA};
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_SET_QUOTA, false, pid) else {
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

pub async fn trim_roblox_memory(state: &AppState) -> Value {
    let Some(out) = tasklist("RobloxPlayerBeta.exe").await else {
        return serde_json::json!({ "ok": false, "trimmed": 0, "total": 0, "error": "tasklist failed" });
    };
    let re = regex::Regex::new(r#""RobloxPlayerBeta\.exe","(\d+)""#).unwrap();

    // Skip PIDs still inside their launch grace window (ready_at, same window
    // watch_tick uses) -- trimming a client while it's still loading assets
    // can evict pages it just allocated, causing a stutter mid-load.
    let now = now_ms();
    let launching_pids: std::collections::HashSet<u32> = {
        let watched = state.watched_accounts.lock().unwrap();
        let pids = state.account_pids.lock().unwrap();
        watched.iter().filter(|(_, ready_at)| now < **ready_at).filter_map(|(id, _)| pids.get(id).copied()).collect()
    };

    let mut total = 0u32;
    let mut trimmed = 0u32;
    for cap in re.captures_iter(&out) {
        if let Ok(pid) = cap[1].parse::<u32>() {
            if launching_pids.contains(&pid) {
                continue;
            }
            total += 1;
            if trim_process_memory(pid) {
                trimmed += 1;
            }
        }
    }
    serde_json::json!({ "ok": true, "trimmed": trimmed, "total": total })
}

static ROBLOX_PROC_RE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| regex::Regex::new(r"(?i)RobloxPlayerBeta\.exe|RobloxCrashHandler\.exe").unwrap());

async fn wait_for_roblox_fully_closed(max_wait: Duration) {
    let started = std::time::Instant::now();
    loop {
        let mut cmd = Command::new("cmd");
        cmd.args([
            "/c",
            r#"tasklist /FI "IMAGENAME eq RobloxPlayerBeta.exe" /NH & tasklist /FI "IMAGENAME eq RobloxCrashHandler.exe" /NH"#,
        ]);
        hide_window(&mut cmd);
        let out = match cmd.output().await {
            Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
            Err(_) => return,
        };
        if !ROBLOX_PROC_RE.is_match(&out) || started.elapsed() >= max_wait {
            return;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

pub async fn kill_all_roblox(app: &AppHandle, state: &AppState) -> Value {
    let watched_ids: Vec<String> = state.watched_accounts.lock().unwrap().keys().cloned().collect();
    state.watched_accounts.lock().unwrap().clear();
    state.miss_counts.lock().unwrap().clear();
    stop_watch_poll_if_idle(state);

    let notify = |app: &AppHandle, ids: &[String]| {
        for id in ids {
            let _ = app.emit("roblox:closed", id);
        }
        let _ = app.emit("roblox:allClosed", ());
    };

    if !cfg!(windows) {
        notify(app, &watched_ids);
        return serde_json::json!({ "ok": false, "error": "Windows only" });
    }

    let mut cmd = Command::new("cmd");
    cmd.args(["/c", "taskkill /F /IM RobloxPlayerBeta.exe /T & taskkill /F /IM RobloxCrashHandler.exe /T"]);
    hide_window(&mut cmd);
    state.account_pids.lock().unwrap().clear();
    let had_running = !watched_ids.is_empty();

    let result = tokio::time::timeout(Duration::from_secs(6), cmd.output()).await;
    wait_for_roblox_fully_closed(Duration::from_secs(5)).await;
    if had_running {
        restart_mutex_holder(app, state).await;
    } else {
        start_mutex_holder(app, state).await;
    }
    notify(app, &watched_ids);
    match result {
        Ok(Ok(_)) => serde_json::json!({ "ok": true }),
        _ => serde_json::json!({ "ok": true }),
    }
}

pub async fn kill_account_roblox(app: &AppHandle, state: &AppState, account_id: &str) -> Value {
    let pid = state.account_pids.lock().unwrap().remove(account_id);
    state.watched_accounts.lock().unwrap().remove(account_id);
    state.miss_counts.lock().unwrap().remove(account_id);
    stop_watch_poll_if_idle(state);

    let notify = |app: &AppHandle| {
        let _ = app.emit("roblox:closed", account_id);
    };

    if !cfg!(windows) {
        notify(app);
        return serde_json::json!({ "ok": false, "error": "Windows only" });
    }
    let Some(pid) = pid else {
        notify(app);
        return serde_json::json!({ "ok": false, "error": "No tracked process for this account" });
    };
    let mut cmd = Command::new("cmd");
    cmd.args(["/c", &format!("taskkill /F /PID {} /T", pid)]);
    hide_window(&mut cmd);
    let _ = tokio::time::timeout(Duration::from_secs(4), cmd.output()).await;
    notify(app);
    serde_json::json!({ "ok": true })
}

fn close_singleton_handles_only<'a>(app: &'a AppHandle, state: &'a AppState) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let Some(exe) = ensure_native_helper(app, state).await else { return };
        let mut cmd = Command::new(&exe);
        cmd.arg("closehandles").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        hide_window(&mut cmd);
        let Ok(mut child) = cmd.spawn() else { return };
        if let Some(stdout) = child.stdout.take() {
            let done = std::sync::Arc::new(AtomicBool::new(false));
            let done2 = done.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.contains("HANDLES_DONE") {
                        done2.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            });
            for _ in 0..40 {
                if done.load(Ordering::SeqCst) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let _ = child.start_kill();
    })
}

async fn close_singleton_and_hold_mutex(app: &AppHandle, state: &AppState) {
    if cfg!(windows) {
        start_mutex_holder(app, state).await;
    }
    close_singleton_handles_only(app, state).await;
}

fn get_latest_roblox_version_dir() -> Option<(PathBuf, PathBuf)> {
    let home = dirs_home()?;
    let versions_base = home.join("AppData").join("Local").join("Roblox").join("Versions");
    if !versions_base.exists() {
        return None;
    }
    let mut candidates: Vec<(PathBuf, PathBuf, std::time::SystemTime)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&versions_base) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("version-") {
                continue;
            }
            let dir = versions_base.join(&*name);
            let exe = dir.join("RobloxPlayerBeta.exe");
            if let Ok(meta) = std::fs::metadata(&exe) {
                if let Ok(mtime) = meta.modified() {
                    candidates.push((dir, exe, mtime));
                }
            }
        }
    }
    candidates.sort_by(|a, b| b.2.cmp(&a.2));
    candidates.into_iter().next().map(|(dir, exe, _)| (dir, exe))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var("USERPROFILE").ok().map(PathBuf::from)
}

pub fn get_fflag_path() -> Option<PathBuf> {
    let (dir, _) = get_latest_roblox_version_dir()?;
    Some(dir.join("ClientSettings").join("ClientAppSettings.json"))
}

// ---- watch loop ----
// One shared poll covering every watched account instead of one tasklist
// spawn per account per tick (see main.js's _watchTick comment for the
// reasoning). MISS_THRESHOLD=6 (~30s) tolerates slow tasklist calls under
// real multi-instance CPU/IO contention.
const MISS_THRESHOLD: u32 = 6;
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const LAUNCH_DELAY_MS: i64 = 15_000;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn stop_watch_poll_if_idle(state: &AppState) {
    if state.watched_accounts.lock().unwrap().is_empty() {
        if let Some(handle) = state.watch_handle.lock().unwrap().take() {
            handle.abort();
        }
    }
}

fn start_watch_poll(app: &AppHandle, state_handle: tauri::AppHandle) {
    let already = app.state::<AppState>().watch_handle.lock().unwrap().is_some();
    if already {
        return;
    }
    let handle = tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            watch_tick(&state_handle).await;
        }
    });
    *app.state::<AppState>().watch_handle.lock().unwrap() = Some(handle);
}

pub fn watch_roblox(app: &AppHandle, account_id: &str) {
    let st = app.state::<AppState>();
    st.watched_accounts.lock().unwrap().insert(account_id.to_string(), now_ms() + LAUNCH_DELAY_MS);
    st.miss_counts.lock().unwrap().insert(account_id.to_string(), 0);
    start_watch_poll(app, app.clone());
}

static WATCH_TICK_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

async fn watch_tick(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.watched_accounts.lock().unwrap().is_empty() {
        stop_watch_poll_if_idle(&state);
        return;
    }
    // setInterval-equivalent re-entrancy guard: a slow tasklist spawn under
    // heavy multi-instance load can outlast POLL_INTERVAL. Two concurrent
    // ticks would race on the same account_pids/miss_counts maps and can
    // false-positive a still-running account as closed. Skipping a tick is
    // fine -- the next one 5s later covers the same ground.
    if WATCH_TICK_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        return;
    }
    let out = tasklist("RobloxPlayerBeta.exe").await;
    WATCH_TICK_IN_FLIGHT.store(false, Ordering::SeqCst);
    // tasklist failed to run at all (not "ran and found nothing") -- can't
    // tell who's alive this tick. Bail without touching miss counts so a
    // still-running account never gets penalized for a command failure.
    let Some(out) = out else { return; };

    let re = regex::Regex::new(r#""RobloxPlayerBeta\.exe","(\d+)""#).unwrap();
    let mut alive_pids: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for cap in re.captures_iter(&out) {
        if let Ok(pid) = cap[1].parse::<u32>() {
            alive_pids.insert(pid);
        }
    }

    let now = now_ms();
    let mut closed: Vec<String> = Vec::new();
    let claimed: std::collections::HashSet<u32> = {
        let watched = state.watched_accounts.lock().unwrap();
        let pids = state.account_pids.lock().unwrap();
        watched.keys().filter_map(|id| pids.get(id).copied()).collect()
    };
    let mut orphans: Vec<u32> = alive_pids.iter().filter(|p| !claimed.contains(p)).copied().collect();

    let any_running = !alive_pids.is_empty();
    let watched_snapshot: Vec<(String, i64)> = state.watched_accounts.lock().unwrap().iter().map(|(k, v)| (k.clone(), *v)).collect();
    for (account_id, ready_at) in watched_snapshot {
        if now < ready_at {
            continue;
        }
        let pid = state.account_pids.lock().unwrap().get(&account_id).copied();
        // No tracked PID means this account launched via the URI-handler
        // fallback (direct spawn failed or Roblox wasn't found at the
        // expected install path) -- we can't identify which specific process
        // is "ours", so fall back to the coarse "is anything running at all"
        // signal instead of declaring it closed outright.
        let mut running = match pid {
            Some(p) => alive_pids.contains(&p),
            None => any_running,
        };
        if let Some(_p) = pid {
            if !running && !orphans.is_empty() {
                let adopted = orphans.remove(0);
                state.account_pids.lock().unwrap().insert(account_id.clone(), adopted);
                running = true;
            }
        }
        if !running {
            let misses = {
                let mut mc = state.miss_counts.lock().unwrap();
                let m = mc.entry(account_id.clone()).or_insert(0);
                *m += 1;
                *m
            };
            if misses >= MISS_THRESHOLD {
                closed.push(account_id);
            }
        } else {
            state.miss_counts.lock().unwrap().insert(account_id, 0);
        }
    }

    for account_id in &closed {
        state.watched_accounts.lock().unwrap().remove(account_id);
        state.miss_counts.lock().unwrap().remove(account_id);
        let accounts = crate::storage::load_accounts(&state);
        let acct = accounts.iter().find(|a| a.get("id").and_then(|v| v.as_str()) == Some(account_id.as_str())).cloned().unwrap_or(Value::Null);
        let username = acct.get("username").and_then(|v| v.as_str());
        let user_id = acct.get("userId").and_then(|v| v.as_str());
        let pid = state.account_pids.lock().unwrap().get(account_id).copied();
        emit_log(
            app,
            "warn",
            "crash",
            &format!("Roblox closed unexpectedly for {} (missed {} consecutive checks)", username.unwrap_or(account_id), MISS_THRESHOLD),
            Some(serde_json::json!({ "accountId": account_id, "username": username, "userId": user_id, "pid": pid })),
        );
        state.account_pids.lock().unwrap().remove(account_id);
        let _ = app.emit("roblox:closed", account_id);
    }

    let _ = app.emit("roblox:count", alive_pids.len());
    stop_watch_poll_if_idle(&state);
}

// ---- launch ----
const LAUNCH_STAGGER_MS: i64 = 4_000;

pub async fn do_launch(app: &AppHandle, state: &AppState, account_id: &str, cookie: &str, target: &str) -> Value {
    close_singleton_and_hold_mutex(app, state).await;

    let since_last = now_ms() - *state.last_launch_ts.lock().unwrap();
    if *state.last_launch_ts.lock().unwrap() > 0 && since_last < LAUNCH_STAGGER_MS {
        tokio::time::sleep(Duration::from_millis((LAUNCH_STAGGER_MS - since_last) as u64)).await;
    }

    let csrf_token = crate::roblox_api::get_csrf_token(state, cookie).await;
    let Some(csrf_token) = csrf_token else {
        let accounts = crate::storage::load_accounts(state);
        let username = find_username(&accounts, account_id);
        emit_log(app, "err", "launch", &format!("Launch failed for {}: could not get CSRF token (cookie may be expired)", username.clone().unwrap_or_else(|| account_id.to_string())), Some(serde_json::json!({ "accountId": account_id, "username": username })));
        return serde_json::json!({ "success": false, "error": "Failed to get CSRF token. Is the account cookie still valid?" });
    };

    let ticket_result = crate::roblox_api::get_auth_ticket(state, cookie, Some(csrf_token.clone())).await;
    if !ticket_result.ok {
        let accounts = crate::storage::load_accounts(state);
        let username = find_username(&accounts, account_id);
        emit_log(app, "err", "launch", &format!("Launch failed for {}: auth ticket error - {}", username.clone().unwrap_or_else(|| account_id.to_string()), ticket_result.error.clone().unwrap_or_default()), Some(serde_json::json!({ "accountId": account_id, "username": username })));
        return serde_json::json!({ "success": false, "error": format!("Failed to get auth ticket: {}", ticket_result.error.unwrap_or_default()) });
    }
    let ticket = ticket_result.ticket.unwrap_or_default();

    let t = target.trim();
    let mut launcher_url = String::new();

    // Shorthand for joining one specific running server instance: "placeId:jobId"
    // or "placeId,jobId" (jobId is the server's GUID, e.g. from game.JobId).
    // Uses the same RequestGameJob endpoint as the private-server/share-link
    // paths below, just without a linkCode -- that's what Roblox's own client
    // uses to join a specific public server instance by job id.
    let job_shorthand_re = regex::Regex::new(r"^(\d+)[,:]\s*([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$").unwrap();

    if !t.is_empty() {
        if let Some(caps) = job_shorthand_re.captures(t) {
            launcher_url = format!(
                "https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestGameJob&placeId={}&gameId={}&isPlayTogetherGame=false",
                &caps[1], &caps[2]
            );
        } else if t.chars().all(|c| c.is_ascii_digit()) {
            launcher_url = format!("https://assetgame.roblox.com/game/placelauncher.ashx?request=RequestGame&placeId={}&isPlayTogetherGame=false", t);
        } else {
            let mut raw_url = if t.starts_with("http") { t.to_string() } else { format!("https://{}", t) };
            if let Ok(parsed0) = url::Url::parse(&raw_url) {
                let host = parsed0.host_str().unwrap_or("");
                if host == "ro.blox.com" || host.ends_with(".ro.blox.com") {
                    raw_url = crate::roblox_api::follow_redirect(state, &raw_url).await;
                }
            }
            let parsed = url::Url::parse(&raw_url).ok();
            match parsed {
                None => return serde_json::json!({ "success": false, "error": "Unrecognised input. Enter a place ID, game URL, or private server link." }),
                Some(parsed_url) => {
                    let query: std::collections::HashMap<String, String> = parsed_url.query_pairs().into_owned().collect();
                    let private_code = query.get("privateServerLinkCode").cloned();
                    let share_code = query.get("code").cloned();
                    let share_type = query.get("type").cloned();
                    // jobId/gameInstanceId: join one specific running server instance,
                    // e.g. from a roblox://experiences/start?placeId=X&gameInstanceId=Y
                    // deep link, or a URL with a plain ?jobId= query param.
                    let job_id = query.get("jobId").or_else(|| query.get("gameInstanceId")).cloned();
                    let place_id_re = regex::Regex::new(r"/games/(\d+)").unwrap();
                    let place_id_re2 = regex::Regex::new(r"/(\d+)").unwrap();
                    let path = parsed_url.path();
                    let place_id = place_id_re.captures(path).or_else(|| place_id_re2.captures(path)).map(|c| c[1].to_string()).or_else(|| query.get("placeId").cloned());

                    if let (Some(job_id), Some(place_id)) = (&job_id, &place_id) {
                        launcher_url = format!(
                            "https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestGameJob&placeId={}&gameId={}&isPlayTogetherGame=false",
                            place_id, job_id
                        );
                    } else if let (Some(private_code), Some(place_id)) = (&private_code, &place_id) {
                        let access_code = crate::roblox_api::get_access_code(state, place_id, private_code, cookie, &csrf_token).await;
                        match access_code {
                            None => return serde_json::json!({ "success": false, "error": "Could not resolve private server access code. The link may be expired or you may not have permission." }),
                            Some(access_code) => {
                                launcher_url = format!(
                                    "https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestPrivateGame&placeId={}&accessCode={}&linkCode={}",
                                    place_id, access_code, private_code
                                );
                            }
                        }
                    } else if path == "/share" || (share_code.is_some() && share_type.is_some()) {
                        let Some(code) = share_code else {
                            return serde_json::json!({ "success": false, "error": "Invalid share link -- no code found." });
                        };
                        let resolved = crate::roblox_api::resolve_share_link(state, &code, cookie, Some(&csrf_token)).await;
                        if !resolved.ok {
                            return serde_json::json!({ "success": false, "error": resolved.error.unwrap_or_else(|| "Could not resolve share link. It may be expired or invalid.".into()) });
                        }
                        launcher_url = format!(
                            "https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestGameJob&placeId={}&isPlayTogetherGame=false&linkCode={}",
                            resolved.place_id.unwrap_or_default(),
                            resolved.link_code.unwrap_or_default()
                        );
                    } else if let Some(place_id) = place_id {
                        launcher_url = format!("https://assetgame.roblox.com/game/placelauncher.ashx?request=RequestGame&placeId={}&isPlayTogetherGame=false", place_id);
                    } else {
                        return serde_json::json!({ "success": false, "error": "Could not find a Place ID in the URL." });
                    }
                }
            }
        }
    }

    let launch_time = now_ms();
    let browser_id: u64 = rand::random::<u64>() % 9_000_000_000_000 + 1_000_000_000_000;
    let roblox_uri = if !launcher_url.is_empty() {
        format!(
            "roblox-player:1+launchmode:play+gameinfo:{}+launchtime:{}+placelauncherurl:{}+browsertrackerid:{}+robloxLocale:en_us+gameLocale:en_us+channel:+LaunchExp:InApp",
            ticket, launch_time, urlencoding::encode(&launcher_url), browser_id
        )
    } else {
        format!("roblox-player:1+launchmode:app+gameinfo:{}+launchtime:{}+browsertrackerid:{}+robloxLocale:en_us+gameLocale:en_us", ticket, launch_time, browser_id)
    };

    let roblox_exe = get_latest_roblox_version_dir().map(|(_, exe)| exe);

    let mut spawned_pid: Option<u32> = None;
    if let Some(exe) = &roblox_exe {
        if exe.exists() {
            let mut cmd = Command::new(exe);
            cmd.arg(&roblox_uri).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            hide_window(&mut cmd);
            if let Ok(child) = cmd.spawn() {
                spawned_pid = child.id();
                std::mem::forget(child); // detached: outlives this process, matches child.unref()
            }
        }
    }
    match spawned_pid {
        Some(pid) => {
            state.account_pids.lock().unwrap().insert(account_id.to_string(), pid);
        }
        None => {
            let _ = tauri_plugin_opener::open_url(&roblox_uri, None::<&str>);
        }
    }

    *state.last_launch_ts.lock().unwrap() = now_ms();
    crate::roblox_api::invalidate_ticket(state, cookie);

    let mut accounts = crate::storage::load_accounts(state);
    let (username, user_id) = if let Some(idx) = accounts.iter().position(|a| a.get("id").and_then(|v| v.as_str()) == Some(account_id)) {
        accounts[idx]["lastUsed"] = Value::String(chrono::Utc::now().to_rfc3339());
        let username = accounts[idx].get("username").and_then(|v| v.as_str()).map(|s| s.to_string());
        let user_id = accounts[idx].get("userId").and_then(|v| v.as_str()).map(|s| s.to_string());
        let _ = crate::storage::save_accounts(state, accounts.clone());
        (username, user_id)
    } else {
        (None, None)
    };
    let pid = state.account_pids.lock().unwrap().get(account_id).copied();

    emit_log(
        app,
        "ok",
        "launch",
        &format!("Launched Roblox for {}", username.clone().unwrap_or_else(|| account_id.to_string())),
        Some(serde_json::json!({ "accountId": account_id, "username": username, "userId": user_id, "target": if t.is_empty() { "Roblox home" } else { t }, "pid": pid })),
    );

    watch_roblox(app, account_id);

    if let Some(vol) = crate::settings::load_settings().get("masterVolume").and_then(|v| v.as_f64()) {
        if vol != 100.0 {
            let app2 = app.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(9)).await;
                let st = app2.state::<AppState>();
                set_roblox_volume(&app2, &st, vol).await;
            });
        }
    }

    serde_json::json!({ "success": true })
}

fn find_username(accounts: &[Value], account_id: &str) -> Option<String> {
    accounts.iter().find(|a| a.get("id").and_then(|v| v.as_str()) == Some(account_id)).and_then(|a| a.get("username")).and_then(|v| v.as_str()).map(|s| s.to_string())
}
