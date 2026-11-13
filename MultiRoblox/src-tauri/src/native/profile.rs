// Per-account launch settings: FPS cap, graphics level, custom FFlags and how
// the window starts (normal, small or minimized).
//
// Roblox reads its FFlag file (ClientAppSettings.json) and its settings file
// (GlobalBasicSettings_13.xml) once, when a client starts. So an account's own
// values are written just before its client is spawned, and the shared values
// are put back once it has had time to read them. Launches are already one at
// a time; when two accounts in a row want different values, the second waits
// until the first client has had SETTLE to read its own.
//
// Custom FFlags are limited to Roblox's allowlist (anything else is ignored by
// the client since September 2025, so offering it would only mislead). Each
// one applies to every account or to chosen accounts. Flags for every account
// are kept in the file itself, so a Roblox started outside the app gets them
// too; flags for chosen accounts only exist in the file while those launch.
use crate::state::AppState;
use crate::storage::{self, account_id as id_of};
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

/// Roblox's FFlag allowlist (devforum "Allowlist for local client
/// configuration via Fast Flags"), with whether each is a true/false flag.
pub const ALLOWED: [(&str, bool); 18] = [
    ("DFIntCSGLevelOfDetailSwitchingDistance", false),
    ("DFIntCSGLevelOfDetailSwitchingDistanceL12", false),
    ("DFIntCSGLevelOfDetailSwitchingDistanceL23", false),
    ("DFIntCSGLevelOfDetailSwitchingDistanceL34", false),
    ("FFlagHandleAltEnterFullscreenManually", true),
    ("DFFlagTextureQualityOverrideEnabled", true),
    ("DFIntTextureQualityOverride", false),
    ("FIntDebugForceMSAASamples", false),
    ("DFFlagDisableDPIScale", true),
    ("FFlagDebugGraphicsPreferD3D11", true),
    ("FFlagDebugSkyGray", true),
    ("DFFlagDebugPauseVoxelizer", true),
    ("DFIntDebugFRMQualityLevelOverride", false),
    ("FIntFRMMaxGrassDistance", false),
    ("FIntFRMMinGrassDistance", false),
    ("FFlagDebugGraphicsPreferVulkan", true),
    ("FFlagDebugGraphicsPreferOpenGL", true),
    ("FIntGrassMovementReducedMotionFactor", false),
];

/// Allowlisted flags the Mixer and Settings already set (graphics quality,
/// rendering engine, and Settings > Performance > Advanced graphics). They
/// stay there; the custom list offers the rest.
pub const MIXER_FLAGS: [&str; 9] = [
    "DFIntDebugFRMQualityLevelOverride",
    "FFlagDebugGraphicsPreferD3D11",
    "FFlagDebugGraphicsPreferVulkan",
    "FFlagDebugGraphicsPreferOpenGL",
    "FIntDebugForceMSAASamples",
    "DFFlagTextureQualityOverrideEnabled",
    "DFIntTextureQualityOverride",
    "FIntFRMMaxGrassDistance",
    "FIntFRMMinGrassDistance",
];
const GFX_FLAG: &str = "DFIntDebugFRMQualityLevelOverride";

pub const CUSTOM_KEY: &str = "customFFlags";
const FPS_KEY: &str = "launchFps";
const GFX_KEY: &str = "launchGfx";
const WINDOW_KEY: &str = "launchWindow";
/// Settings > Performance > Hide Roblox windows: accounts left on "Default"
/// launch hidden while it's on.
pub const HIDE_KEY: &str = "hideWindows";
/// An account's own Advanced graphics choice: "on", "off", or missing for
/// whatever Settings says.
const ADV_KEY: &str = "launchAdvGfx";
/// The Advanced graphics settings (the switch and its options). Saved under
/// the older name before the rename, so that is read too.
const ADV_SETTING: &str = "advancedGraphics";
const ADV_SETTING_OLD: &str = "lighterRendering";
/// The flags Advanced graphics owns.
const ADV_FLAGS: [&str; 5] = [
    "FIntDebugForceMSAASamples",
    "DFFlagTextureQualityOverrideEnabled",
    "DFIntTextureQualityOverride",
    "FIntFRMMaxGrassDistance",
    "FIntFRMMinGrassDistance",
];

/// How long a new client gets to read the files before they change again.
const SETTLE: Duration = Duration::from_secs(6);

fn is_bool_flag(name: &str) -> Option<bool> {
    ALLOWED.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
}

fn is_custom_flag(name: &str) -> bool {
    is_bool_flag(name).is_some() && !MIXER_FLAGS.contains(&name)
}

/// The value as Roblox's file spells it ("True"/"False", or a whole number),
/// or None when it isn't a value this flag can take.
pub fn coerce(name: &str, value: &Value) -> Option<String> {
    let as_bool = is_bool_flag(name)?;
    if as_bool {
        let b = match value {
            Value::Bool(b) => *b,
            Value::String(s) if s.eq_ignore_ascii_case("true") => true,
            Value::String(s) if s.eq_ignore_ascii_case("false") => false,
            _ => return None,
        };
        return Some(if b { "True" } else { "False" }.into());
    }
    let n = match value {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    (n.is_finite() && n.fract() == 0.0 && n >= i32::MIN as f64 && n <= i32::MAX as f64)
        .then(|| (n as i64).to_string())
}

/// One custom flag: its value, and the accounts it's for (None = all).
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub value: String,
    pub accounts: Option<Vec<String>>,
}

/// The saved list, minus anything that isn't an allowlisted custom flag with
/// a valid value. A later entry for the same flag and scope replaces an earlier one.
pub fn parse_entries(v: Option<&Value>) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for e in v.and_then(Value::as_array).into_iter().flatten() {
        let Some(name) = e.get("name").and_then(Value::as_str) else { continue };
        if !is_custom_flag(name) {
            continue;
        }
        let Some(value) = e.get("value").and_then(|v| coerce(name, v)) else { continue };
        let accounts = e.get("accounts").and_then(Value::as_array).map(|a| {
            a.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>()
        });
        let accounts = accounts.filter(|a| !a.is_empty());
        out.retain(|x| !(x.name == name && x.accounts == accounts));
        out.push(Entry { name: name.into(), value, accounts });
    }
    out
}

fn entries() -> Vec<Entry> {
    parse_entries(crate::settings::snapshot().get(CUSTOM_KEY))
}

fn entries_to_value(list: &[Entry]) -> Value {
    Value::Array(
        list.iter()
            .map(|e| match &e.accounts {
                Some(a) => json!({ "name": e.name, "value": e.value, "accounts": a }),
                None => json!({ "name": e.name, "value": e.value }),
            })
            .collect(),
    )
}

// ---- the FFlag file ----

fn read_file_flags() -> Map<String, Value> {
    crate::native::get_fflag_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Value>(s.trim_start_matches('\u{feff}')).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_file_flags(flags: &Map<String, Value>) -> bool {
    let paths = crate::native::fflag_paths();
    let Some((primary, rest)) = paths.split_first() else {
        return false;
    };
    let text = serde_json::to_string_pretty(&Value::Object(flags.clone())).unwrap_or_else(|_| "{}".into());
    // Atomic: Roblox reads this file at startup, and a truncated one (crash
    // or full disk mid-write) makes it ignore every flag. Only the primary
    // copy decides success; the extra copy for a bootstrapper's active
    // version folder is best-effort (see native::fflag_paths).
    let ok = crate::jsonfile::write_atomic(primary, &text).is_ok();
    if ok {
        for p in rest {
            if let Err(e) = crate::jsonfile::write_atomic(p, &text) {
                eprintln!("[fflags] could not mirror to {}: {e}", p.display());
            }
        }
    }
    ok
}

/// What the file normally holds: while a launch has its own values in the
/// file, the values it replaced. Everything that edits the file (the Mixer,
/// Settings, the custom list) works on this, so a launch in progress can't
/// leak its account's values into the shared ones.
pub fn read_flags() -> Map<String, Value> {
    if let Some(a) = ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return a.base.clone();
    }
    read_file_flags()
}

/// Saves the shared flags. During a launch they are remembered and land in
/// the file when that launch puts the shared values back.
pub fn write_flags(flags: &Map<String, Value>) -> bool {
    let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(a) = active.as_mut() {
        a.base = flags.clone();
        return true;
    }
    drop(active);
    write_file_flags(flags)
}

/// Saves the custom list and brings the file's shared flags in line with it:
/// every custom flag comes out, then the ones for all accounts go back in.
pub fn save_custom(list: &Value) -> Result<Value, String> {
    let clean = parse_entries(Some(list));
    let stored = entries_to_value(&clean);
    crate::settings::update(|s| {
        s.insert(CUSTOM_KEY.into(), stored.clone());
    })?;
    let mut flags = read_flags();
    flags.retain(|k, _| !is_custom_flag(k));
    for e in clean.iter().filter(|e| e.accounts.is_none()) {
        flags.insert(e.name.clone(), Value::String(e.value.clone()));
    }
    if !write_flags(&flags) && crate::native::get_fflag_path().is_some() {
        return Err("Could not write Roblox's FFlag file".into());
    }
    Ok(stored)
}

/// The allowlist for the UI, without the flags the Mixer owns.
pub fn allowed_for_ui() -> Value {
    Value::Array(
        ALLOWED
            .iter()
            .filter(|(n, _)| !MIXER_FLAGS.contains(n))
            .map(|(n, b)| json!({ "name": n, "kind": if *b { "bool" } else { "int" } }))
            .collect(),
    )
}

/// Whether an account's window choice is "Default" (follows Settings).
pub fn follows_window_default(account: &Value) -> bool {
    matches!(account.get(WINDOW_KEY).and_then(Value::as_str), None | Some(""))
}

// ---- Advanced graphics ----

/// The saved Advanced graphics settings.
fn adv_config() -> Value {
    let s = crate::settings::snapshot();
    s.get(ADV_SETTING).or_else(|| s.get(ADV_SETTING_OLD)).cloned().unwrap_or(Value::Null)
}

fn adv_on(cfg: &Value) -> bool {
    cfg.get("on").and_then(Value::as_bool).unwrap_or(false)
}

/// The flags Advanced graphics writes with these options (the same ones the
/// Settings page writes for every account).
fn adv_flags(cfg: &Value) -> Map<String, Value> {
    let msaa = cfg.get("msaa").and_then(Value::as_i64).filter(|n| [1, 2, 4, 8].contains(n)).unwrap_or(1);
    let texture = cfg.get("texture").and_then(Value::as_i64).filter(|n| (0..=3).contains(n)).unwrap_or(0);
    let grass = cfg.get("grass").and_then(Value::as_bool).unwrap_or(true);
    let mut m = Map::new();
    m.insert("FIntDebugForceMSAASamples".into(), Value::String(msaa.to_string()));
    m.insert("DFFlagTextureQualityOverrideEnabled".into(), Value::String("True".into()));
    m.insert("DFIntTextureQualityOverride".into(), Value::String(texture.to_string()));
    if grass {
        m.insert("FIntFRMMaxGrassDistance".into(), Value::String("0".into()));
        m.insert("FIntFRMMinGrassDistance".into(), Value::String("0".into()));
    }
    m
}

// ---- per-launch overrides ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    Normal,
    Small,
    Minimized,
    /// Minimized and then hidden: no window and no taskbar button. Shown
    /// again from the account's menu, or (minimized) when the app exits.
    Hidden,
}

/// What a launch needs from this module afterwards.
pub struct Prepared {
    gen: Option<u64>,
    pub window: WindowMode,
    account_id: String,
}

struct Active {
    gen: u64,
    /// The shared flags the file held before this launch's own were written.
    base: Map<String, Value>,
    /// Identifies this launch's values, to tell whether the next one differs.
    key: String,
    /// When the client was spawned (None until then).
    spawned: Option<Instant>,
}

static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);
static GEN: AtomicU64 = AtomicU64::new(0);

/// Clients whose window the user has shown or hidden by hand. Setting up a
/// window after launch (the second "headless" pass, say) leaves those alone,
/// so it can't undo what the user just did.
static MANUAL: Mutex<Option<std::collections::HashSet<u32>>> = Mutex::new(None);

pub fn note_manual_window(pid: u32) {
    MANUAL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Default::default)
        .insert(pid);
}

/// A new client has this process id; Windows reuses them, so an old note
/// about the same number must not carry over to it.
fn forget_manual_window(pid: u32) {
    if let Some(s) = MANUAL.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        s.remove(&pid);
    }
}

fn manually_changed(pid: u32) -> bool {
    MANUAL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|s| s.contains(&pid))
}

/// An account's own values: (fps, graphics level, extra flags, window).
/// In the extra flags, null means "take this one out of the file".
fn account_values(account: Option<&Value>, list: &[Entry], account_id: &str, adv: &Value, hide_default: bool) -> (Option<i64>, Option<i64>, Map<String, Value>, WindowMode) {
    let num = |k: &str| account.and_then(|a| a.get(k)).and_then(Value::as_f64).filter(|n| n.is_finite());
    let fps = num(FPS_KEY).filter(|n| *n > 0.0).map(crate::fps::normalize);
    let gfx = num(GFX_KEY).map(|n| n.round() as i64).filter(|n| (1..=21).contains(n));
    let window = match account.and_then(|a| a.get(WINDOW_KEY)).and_then(Value::as_str) {
        Some("small") => WindowMode::Small,
        Some("minimized") => WindowMode::Minimized,
        Some("hidden") => WindowMode::Hidden,
        Some("normal") => WindowMode::Normal,
        // "Default": follows Settings.
        _ if hide_default => WindowMode::Hidden,
        _ => WindowMode::Normal,
    };
    let mut extra = Map::new();
    for e in list {
        if e.accounts.as_ref().is_some_and(|a| a.iter().any(|x| x == account_id)) {
            extra.insert(e.name.clone(), Value::String(e.value.clone()));
        }
    }
    if let Some(g) = gfx {
        extra.insert(GFX_FLAG.into(), Value::String(g.to_string()));
    }
    // Advanced graphics: only an account that differs from Settings needs
    // anything here, since the shared file already follows Settings.
    let own_adv = match account.and_then(|a| a.get(ADV_KEY)).and_then(Value::as_str) {
        Some("on") => Some(true),
        Some("off") => Some(false),
        _ => None,
    };
    match own_adv {
        Some(true) if !adv_on(adv) => extra.extend(adv_flags(adv)),
        Some(false) if adv_on(adv) => {
            for f in ADV_FLAGS {
                extra.insert(f.into(), Value::Null);
            }
        }
        _ => {}
    }
    (fps, gfx, extra, window)
}

/// Puts the shared values back if no newer launch has taken over since.
fn restore(gen: u64) {
    let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    if active.as_ref().map(|a| a.gen) != Some(gen) {
        return;
    }
    let a = active.take().unwrap();
    drop(active);
    if !write_file_flags(&a.base) {
        eprintln!("[profile] could not put the shared FFlags back");
    }
    crate::fps::end_override();
}

/// Writes this account's values into Roblox's files, right before its spawn.
pub async fn prepare(state: &AppState, account_id: &str) -> Prepared {
    let accounts = storage::load_accounts(state);
    let account = accounts.iter().find(|a| id_of(a) == Some(account_id));
    let (fps, _gfx, extra, window) = account_values(account, &entries(), account_id, &adv_config(), crate::settings::get_bool(HIDE_KEY, false));
    let key = format!("{:?}|{}", fps, Value::Object(extra.clone()));
    let own = fps.is_some() || !extra.is_empty();

    // A different set of values than the client spawned just before: give
    // that one time to read its own first.
    let wait = {
        let active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        active
            .as_ref()
            .filter(|a| a.key != key)
            .and_then(|a| a.spawned)
            .map(|t| SETTLE.saturating_sub(t.elapsed()))
    };
    if let Some(w) = wait.filter(|w| !w.is_zero()) {
        tokio::time::sleep(w).await;
    }

    if !own {
        // Shared values for this one: put them back now if a launch before
        // it still has its own in the file.
        let gen = ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|a| a.gen);
        if let Some(g) = gen {
            restore(g);
        }
        return Prepared { gen: None, window, account_id: account_id.to_string() };
    }

    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let base = {
        let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        let base = match active.take() {
            Some(a) => a.base,
            None => read_file_flags(),
        };
        *active = Some(Active { gen, base: base.clone(), key, spawned: None });
        base
    };
    let mut flags = base;
    for (k, v) in extra {
        if v.is_null() {
            flags.remove(&k);
        } else {
            flags.insert(k, v);
        }
    }
    if !write_file_flags(&flags) {
        eprintln!("[profile] could not write this account's FFlags");
    }
    match fps {
        Some(v) => {
            if let Err(e) = crate::fps::begin_override(v) {
                eprintln!("[profile] could not set this account's FPS cap: {e}");
            }
        }
        None => crate::fps::end_override(),
    }
    Prepared { gen: Some(gen), window, account_id: account_id.to_string() }
}

/// After the spawn (or a failed one): starts the clock on putting the shared
/// values back, and arranges the window.
pub fn after_spawn(app: &AppHandle, prepared: &Prepared, pid: Option<u32>) {
    if let Some(pid) = pid {
        forget_manual_window(pid);
    }
    if let Some(gen) = prepared.gen {
        if let Some(a) = ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|a| a.gen == gen) {
            a.spawned = Some(Instant::now());
        }
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(if pid.is_some() { SETTLE } else { Duration::ZERO }).await;
            restore(gen);
        });
    }
    if let (Some(pid), mode) = (pid, prepared.window) {
        if mode != WindowMode::Normal {
            arrange_window(app.clone(), pid, mode, prepared.account_id.clone());
        }
    }
}

/// The game-area size a "small" window gets: the Auto layout size when one
/// was chosen, else 640 x 360.
fn small_size() -> (i64, i64) {
    let cfg = crate::settings::snapshot().get("layoutConfig").cloned().unwrap_or(Value::Null);
    let get = |k: &str| cfg.get(k).and_then(Value::as_f64).map(|n| n.round() as i64);
    match (get("w"), get("h")) {
        (Some(w), Some(h)) if (160..=1280).contains(&w) && (90..=720).contains(&h) => (w, h),
        _ => (640, 360),
    }
}

/// Waits for the client's window, then shrinks or minimizes it. Minimizing
/// waits a few seconds first so the client finishes starting up in the open.
fn arrange_window(app: AppHandle, pid: u32, mode: WindowMode, account_id: String) {
    tauri::async_runtime::spawn(async move {
        let cmd = match mode {
            WindowMode::Minimized => format!("winmode|{pid}|min"),
            WindowMode::Hidden => format!("winmode|{pid}|hide"),
            WindowMode::Small => {
                let (w, h) = small_size();
                format!("winmode|{pid}|small|{w}|{h}")
            }
            WindowMode::Normal => return,
        };
        if matches!(mode, WindowMode::Minimized | WindowMode::Hidden) {
            tokio::time::sleep(Duration::from_secs(8)).await;
        }
        let started = Instant::now();
        let mut done = false;
        while started.elapsed() < Duration::from_secs(90) {
            let st = app.state::<AppState>();
            if !st.account_pids.lock().unwrap().values().any(|p| *p == pid) {
                return; // closed or killed meanwhile
            }
            if manually_changed(pid) {
                return; // the user took over this window
            }
            if let Ok(r) = crate::helper::call(&app, &st, &cmd, crate::helper::DEFAULT_TIMEOUT).await {
                if r.trim() == "1" {
                    done = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        if done && mode == WindowMode::Hidden {
            use tauri::Emitter;
            let _ = app.emit("roblox:hidden", json!({ "id": account_id, "hidden": true }));
        }
        // Roblox sets its own window size once the game has loaded, and can
        // bring its window back up then; do it once more after that.
        if done && matches!(mode, WindowMode::Small | WindowMode::Hidden) {
            tokio::time::sleep(Duration::from_secs(12)).await;
            let st = app.state::<AppState>();
            if st.account_pids.lock().unwrap().values().any(|p| *p == pid) && !manually_changed(pid) {
                let _ = crate::helper::call(&app, &st, &cmd, crate::helper::DEFAULT_TIMEOUT).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_written_the_way_roblox_reads_them() {
        assert_eq!(coerce("FFlagDebugSkyGray", &json!(true)).as_deref(), Some("True"));
        assert_eq!(coerce("FFlagDebugSkyGray", &json!("false")).as_deref(), Some("False"));
        assert_eq!(coerce("FFlagDebugSkyGray", &json!(1)), None);
        assert_eq!(coerce("FIntFRMMaxGrassDistance", &json!(0)).as_deref(), Some("0"));
        assert_eq!(coerce("FIntFRMMaxGrassDistance", &json!(" 250 ")).as_deref(), Some("250"));
        assert_eq!(coerce("FIntFRMMaxGrassDistance", &json!(1.5)), None);
        assert_eq!(coerce("FIntFRMMaxGrassDistance", &json!(1e12)), None);
        assert_eq!(coerce("DFIntTaskSchedulerTargetFps", &json!(240)), None, "not on the allowlist");
    }

    #[test]
    fn only_allowlisted_custom_flags_survive() {
        let list = json!([
            { "name": "FFlagDebugSkyGray", "value": true },
            { "name": "DFIntTaskSchedulerTargetFps", "value": 999 },
            { "name": "DFIntDebugFRMQualityLevelOverride", "value": 1 },
            { "name": "FIntDebugForceMSAASamples", "value": 1 },
            { "name": "DFIntCSGLevelOfDetailSwitchingDistance", "value": "x" },
            { "name": "DFIntCSGLevelOfDetailSwitchingDistance", "value": 0, "accounts": ["a", "b"] },
            { "name": "DFIntCSGLevelOfDetailSwitchingDistance", "value": 5, "accounts": [] },
        ]);
        let got = parse_entries(Some(&list));
        assert_eq!(got, vec![
            Entry { name: "FFlagDebugSkyGray".into(), value: "True".into(), accounts: None },
            Entry { name: "DFIntCSGLevelOfDetailSwitchingDistance".into(), value: "0".into(), accounts: Some(vec!["a".into(), "b".into()]) },
            Entry { name: "DFIntCSGLevelOfDetailSwitchingDistance".into(), value: "5".into(), accounts: None },
        ]);
    }

    #[test]
    fn an_account_gets_its_own_flags_fps_graphics_and_window() {
        let list = parse_entries(Some(&json!([
            { "name": "FFlagDebugSkyGray", "value": true },
            { "name": "DFIntCSGLevelOfDetailSwitchingDistance", "value": 0, "accounts": ["alt"] },
        ])));
        let alt = json!({ "id": "alt", "launchFps": 15, "launchGfx": 1, "launchWindow": "minimized" });
        let (fps, gfx, extra, window) = account_values(Some(&alt), &list, "alt", &Value::Null, false);
        assert_eq!((fps, gfx, window), (Some(15), Some(1), WindowMode::Minimized));
        assert_eq!(extra.get("DFIntCSGLevelOfDetailSwitchingDistance"), Some(&json!("0")));
        assert_eq!(extra.get(GFX_FLAG), Some(&json!("1")));
        assert!(extra.get("FFlagDebugSkyGray").is_none(), "shared flags live in the file already");

        let hid = json!({ "id": "h", "launchWindow": "hidden" });
        assert_eq!(account_values(Some(&hid), &list, "h", &Value::Null, false).3, WindowMode::Hidden);

        let main = json!({ "id": "main", "launchFps": 0, "launchGfx": 99 });
        let (fps, gfx, extra, window) = account_values(Some(&main), &list, "main", &Value::Null, false);
        assert_eq!((fps, gfx, window), (None, None, WindowMode::Normal));
        assert!(extra.is_empty());
    }

    #[test]
    fn an_account_can_differ_from_advanced_graphics_in_settings() {
        let on = json!({ "on": true, "msaa": 4, "texture": 2, "grass": false });
        let off = json!({ "on": false, "msaa": 2 });
        let wants_on = json!({ "id": "a", "launchAdvGfx": "on" });
        let wants_off = json!({ "id": "a", "launchAdvGfx": "off" });
        let follows = json!({ "id": "a" });

        // Settings off, this account on: the flags come in for its launch.
        let (_, _, extra, _) = account_values(Some(&wants_on), &[], "a", &off, false);
        assert_eq!(extra.get("FIntDebugForceMSAASamples"), Some(&json!("2")));
        assert_eq!(extra.get("DFIntTextureQualityOverride"), Some(&json!("0")));
        assert_eq!(extra.get("FIntFRMMaxGrassDistance"), Some(&json!("0")), "grass hidden by default");

        // Settings on, this account off: the flags are taken out for its launch.
        let (_, _, extra, _) = account_values(Some(&wants_off), &[], "a", &on, false);
        assert_eq!(extra.len(), ADV_FLAGS.len());
        assert!(extra.values().all(Value::is_null));

        // Same as Settings either way: nothing of its own.
        for cfg in [&on, &off] {
            assert!(account_values(Some(&follows), &[], "a", cfg, false).2.is_empty());
        }
        assert!(account_values(Some(&wants_on), &[], "a", &on, false).2.is_empty());
        assert!(account_values(Some(&wants_off), &[], "a", &off, false).2.is_empty());

        // Options are read from Settings, with the grass switch respected.
        let (_, _, extra, _) = account_values(Some(&wants_on), &[], "a", &json!({ "on": false, "msaa": 8, "texture": 3, "grass": false }), false);
        assert_eq!(extra.get("FIntDebugForceMSAASamples"), Some(&json!("8")));
        assert_eq!(extra.get("DFIntTextureQualityOverride"), Some(&json!("3")));
        assert!(extra.get("FIntFRMMaxGrassDistance").is_none());
    }

    #[test]
    fn a_window_you_changed_by_hand_is_left_alone() {
        // Unique numbers, since the note is process-wide and tests share it.
        let (a, b) = (4_000_000_001u32, 4_000_000_002u32);
        assert!(!manually_changed(a));
        note_manual_window(a);
        assert!(manually_changed(a), "set after showing or hiding by hand");
        assert!(!manually_changed(b), "other clients are not affected");
        forget_manual_window(a);
        assert!(!manually_changed(a), "a new client reusing the process id starts clean");
    }

    #[test]
    fn default_window_follows_hide_windows_in_settings() {
        let default = json!({ "id": "a" });
        let empty = json!({ "id": "a", "launchWindow": "" });
        let normal = json!({ "id": "a", "launchWindow": "normal" });
        let small = json!({ "id": "a", "launchWindow": "small" });
        let win = |a: &Value, hide| account_values(Some(a), &[], "a", &Value::Null, hide).3;
        assert_eq!(win(&default, true), WindowMode::Hidden);
        assert_eq!(win(&empty, true), WindowMode::Hidden);
        assert_eq!(win(&default, false), WindowMode::Normal);
        assert_eq!(win(&normal, true), WindowMode::Normal, "an explicit Normal stays visible");
        assert_eq!(win(&small, true), WindowMode::Small);
        assert!(follows_window_default(&default) && follows_window_default(&empty));
        assert!(!follows_window_default(&normal) && !follows_window_default(&small));
    }
}
