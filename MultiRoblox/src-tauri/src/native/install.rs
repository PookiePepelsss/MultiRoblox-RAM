// Finding Roblox installs (vanilla and every bootstrapper) and starting the
// client from one.
//
// Bootstrappers are not a fixed list. Bloxstrap and its forks (Fishstrap,
// Froststrap, Voidstrap, Bubblestrap, and whatever ships next) all copy
// Bloxstrap's layout, %LOCALAPPDATA%\<Name>\Versions\version-*\ with an
// optional <Name>\Modifications\ overlay, so any folder with that shape is a
// candidate. Two more sources catch installs that live elsewhere: the
// registered roblox-player: protocol handler (whichever bootstrapper the user
// set as default, wherever it was installed) and the machine-wide vanilla
// install under Program Files.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

const PLAYER_EXE: &str = "RobloxPlayerBeta.exe";

/// Setting holding the root folder launches should use; empty means auto.
pub const INSTALL_SETTING: &str = "robloxInstall";

/// A folder that holds a Versions\ directory: vanilla Roblox or a bootstrapper.
#[derive(Debug, Clone)]
pub struct Root {
    pub name: String,
    pub path: PathBuf,
    pub vanilla: bool,
    /// The registered roblox-player: handler lives in this root.
    pub is_default: bool,
}

impl Root {
    /// Bloxstrap-family overlay that the bootstrapper copies into the version
    /// folder at each of its own launches.
    fn modifications(&self) -> Option<PathBuf> {
        (!self.vanilla).then(|| self.path.join("Modifications"))
    }
}

/// One installed player build.
#[derive(Debug, Clone)]
pub struct Install {
    pub root: Root,
    pub dir: PathBuf,
    pub exe: PathBuf,
    pub modified: SystemTime,
}

impl Install {
    pub fn version(&self) -> &str {
        self.dir.file_name().and_then(|n| n.to_str()).unwrap_or("")
    }
}

fn local_appdata() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("AppData").join("Local"))
        })
}

fn path_key(p: &Path) -> String {
    p.to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase()
}

fn is_vanilla_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("Roblox")
}

/// Reads the roblox-player: open command (per-user registration first, then
/// machine-wide), e.g. `"C:\...\Bloxstrap.exe" -player "%1"`.
#[cfg(windows)]
fn protocol_handler_command() -> Option<String> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ,
        RRF_RT_REG_SZ,
    };

    fn read(hive: HKEY, subkey: PCWSTR) -> Option<String> {
        let mut buf = vec![0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let rc = unsafe {
            RegGetValueW(
                hive,
                subkey,
                PCWSTR::null(),
                RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if rc.is_err() {
            return None;
        }
        let len = (size as usize / 2).min(buf.len());
        let s = String::from_utf16_lossy(&buf[..len]);
        let s = s.trim_end_matches('\0').trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    read(
        HKEY_CURRENT_USER,
        w!("Software\\Classes\\roblox-player\\shell\\open\\command"),
    )
    .or_else(|| {
        read(
            HKEY_LOCAL_MACHINE,
            w!("Software\\Classes\\roblox-player\\shell\\open\\command"),
        )
    })
}

#[cfg(not(windows))]
fn protocol_handler_command() -> Option<String> {
    None
}

/// The exe path at the front of a shell open command.
fn command_exe(cmd: &str) -> Option<PathBuf> {
    let cmd = cmd.trim();
    let exe = if let Some(rest) = cmd.strip_prefix('"') {
        rest.split('"').next()?
    } else {
        cmd.split_whitespace().next()?
    };
    (!exe.is_empty()).then(|| PathBuf::from(exe))
}

/// The install root a handler exe belongs to: vanilla registers the player
/// exe inside its version folder (<root>\Versions\version-x\exe); a
/// bootstrapper registers its own exe at the root.
fn root_of_handler(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let versions = dir.parent()?;
    if versions
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("Versions"))
    {
        return versions.parent().map(Path::to_path_buf);
    }
    Some(dir.to_path_buf())
}

fn scan_roots() -> Vec<Root> {
    let mut roots: Vec<Root> = Vec::new();
    let push = |path: PathBuf, roots: &mut Vec<Root>| {
        if !path.join("Versions").is_dir() {
            return;
        }
        if roots.iter().any(|r| path_key(&r.path) == path_key(&path)) {
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let vanilla = is_vanilla_name(&name);
        roots.push(Root {
            name,
            path,
            vanilla,
            is_default: false,
        });
    };

    if let Some(local) = local_appdata() {
        if let Ok(entries) = std::fs::read_dir(&local) {
            for e in entries.flatten() {
                // path().is_dir() follows junctions, which is how people move
                // an install folder to another drive; file_type() does not.
                if e.path().is_dir() {
                    push(e.path(), &mut roots);
                }
            }
        }
    }
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(pf) = std::env::var_os(var) {
            push(PathBuf::from(pf).join("Roblox"), &mut roots);
        }
    }
    let default_root = protocol_handler_command()
        .and_then(|c| command_exe(&c))
        .and_then(|exe| root_of_handler(&exe));
    if let Some(dr) = &default_root {
        push(dr.clone(), &mut roots);
        let key = path_key(dr);
        for r in &mut roots {
            r.is_default = path_key(&r.path) == key;
        }
    }
    roots
}

// Discovery lists %LOCALAPPDATA% and reads the registry; launches and the
// FFlag commands ask several times in a row, so a short cache keeps that to
// one scan per burst without hiding a newly installed bootstrapper for long.
static ROOT_CACHE: Mutex<Option<(Instant, Vec<Root>)>> = Mutex::new(None);
const ROOT_CACHE_TTL: Duration = Duration::from_secs(15);

pub fn roots() -> Vec<Root> {
    let mut cache = ROOT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, list)) = cache.as_ref() {
        if at.elapsed() < ROOT_CACHE_TTL {
            return list.clone();
        }
    }
    let list = scan_roots();
    *cache = Some((Instant::now(), list.clone()));
    list
}

// A real exe is tens of MB; anything smaller is still mid-self-update and
// spawning it triggers Roblox's own repair/reinstall prompt.
const MIN_PLAUSIBLE_EXE_BYTES: u64 = 5_000_000;

fn installs_in(root: &Root) -> Vec<Install> {
    let Ok(entries) = std::fs::read_dir(root.path.join("Versions")) else {
        return Vec::new();
    };
    let mut out: Vec<Install> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("version-"))
        .filter_map(|e| {
            let dir = e.path();
            let exe = dir.join(PLAYER_EXE);
            let meta = std::fs::metadata(&exe).ok()?;
            if meta.len() < MIN_PLAUSIBLE_EXE_BYTES {
                return None;
            }
            Some(Install {
                root: root.clone(),
                dir,
                exe,
                modified: meta.modified().ok()?,
            })
        })
        .collect();
    out.sort_by_key(|i| std::cmp::Reverse(i.modified));
    out
}

/// The root the user picked in Settings, if it still exists.
fn selected_root(all: &[Root]) -> Option<Root> {
    let want = crate::settings::get_str(INSTALL_SETTING)?;
    let want = want.trim();
    if want.is_empty() || want.eq_ignore_ascii_case("auto") {
        return None;
    }
    let key = path_key(Path::new(want));
    all.iter().find(|r| path_key(&r.path) == key).cloned()
}

// The build Roblox is currently serving ("version-<hash>"), as Roblox itself
// reports it. Launching any other build directly makes the client see it's
// out of date and run Roblox's updater, which reinstalls and closes the
// clients that are already running so it can replace their files: from the
// outside that looks like random crashes followed by a reinstall. Cached so
// launches in a burst ask once; a failed lookup is cached briefly too, so
// being offline doesn't slow every launch down.
/// What Roblox says is live: the version folder name and the version number
/// (release, changelist), e.g. ("version-4ea8e…", (747, 7471481)).
#[derive(Debug, Clone, PartialEq)]
pub struct Live {
    pub folder: String,
    pub number: Option<(u32, u32)>,
}

static LIVE_VERSION: Mutex<Option<(Instant, Option<Live>)>> = Mutex::new(None);
const LIVE_TTL: Duration = Duration::from_secs(10 * 60);
const LIVE_FAIL_TTL: Duration = Duration::from_secs(60);
const LIVE_URL: &str = "https://clientsettingscdn.roblox.com/v2/client-version/WindowsPlayer";

fn live_version_cached() -> Option<Option<Live>> {
    let cache = LIVE_VERSION.lock().unwrap_or_else(|e| e.into_inner());
    let (at, value) = cache.as_ref()?;
    let ttl = if value.is_some() { LIVE_TTL } else { LIVE_FAIL_TTL };
    (at.elapsed() < ttl).then(|| value.clone())
}

/// "0.747.0.7471481" or "0, 747, 0, 7471481" -> (747, 7471481). The second
/// part is the release and the last the changelist, which only ever grows;
/// the third part varies between channels and says nothing about age.
pub fn parse_version_number(s: &str) -> Option<(u32, u32)> {
    let parts: Vec<u32> = s
        .split(|c: char| c == '.' || c == ',')
        .map(|p| p.trim().parse::<u32>())
        .collect::<Result<_, _>>()
        .ok()?;
    (parts.len() == 4).then(|| (parts[1], parts[3]))
}

/// The live build, asking Roblox when the cache is stale or `force` is set.
pub async fn refresh_live_version(http: &reqwest::Client, force: bool) -> Option<Live> {
    if !force {
        if let Some(v) = live_version_cached() {
            return v;
        }
    }
    let fetched = async {
        let res = http
            .get(LIVE_URL)
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(4))
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?;
        let v: Value = res.json().await.ok()?;
        let folder = v.get("clientVersionUpload")?.as_str()?.trim().to_string();
        let number = v.get("version").and_then(Value::as_str).and_then(parse_version_number);
        (folder.starts_with("version-") && folder.len() <= 64).then_some(Live { folder, number })
    }
    .await;
    *LIVE_VERSION.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), fetched.clone()));
    fetched
}

/// The version number stamped into an exe ("0, 747, 0, 7471481"), cached by
/// path and file time so each build is read once.
static EXE_VERSIONS: Mutex<Vec<(PathBuf, SystemTime, Option<(u32, u32)>)>> = Mutex::new(Vec::new());

fn exe_version(install: &Install) -> Option<(u32, u32)> {
    let mut cache = EXE_VERSIONS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, _, v)) = cache.iter().find(|(p, t, _)| *p == install.exe && *t == install.modified) {
        return *v;
    }
    let v = read_file_version(&install.exe).and_then(|s| parse_version_number(&s));
    cache.retain(|(p, _, _)| *p != install.exe);
    cache.push((install.exe.clone(), install.modified, v));
    v
}

#[cfg(windows)]
fn read_file_version(exe: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};

    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let path: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None);
        if size == 0 || size > 4 * 1024 * 1024 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(path.as_ptr()), 0, size, data.as_mut_ptr().cast()).ok()?;
        // The string table is keyed by language and code page; take the first.
        let mut ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut len: u32 = 0;
        let key = wide("\\VarFileInfo\\Translation");
        if !VerQueryValueW(data.as_ptr().cast(), PCWSTR(key.as_ptr()), &mut ptr, &mut len).as_bool() || len < 4 {
            return None;
        }
        let lang = *(ptr as *const u16);
        let cp = *(ptr as *const u16).add(1);
        let key = wide(&format!("\\StringFileInfo\\{:04x}{:04x}\\FileVersion", lang, cp));
        if !VerQueryValueW(data.as_ptr().cast(), PCWSTR(key.as_ptr()), &mut ptr, &mut len).as_bool() || len == 0 {
            return None;
        }
        let chars = std::slice::from_raw_parts(ptr as *const u16, len as usize);
        let s = String::from_utf16_lossy(chars);
        Some(s.trim_end_matches('\0').trim().to_string())
    }
}

#[cfg(not(windows))]
fn read_file_version(_exe: &Path) -> Option<String> {
    None
}

/// The version folder Roblox's own launcher is registered to, when that is a
/// player build (vanilla registers the exe inside its version folder; a
/// bootstrapper registers its own exe instead, which says nothing here).
fn handler_version_dir() -> Option<PathBuf> {
    let exe = protocol_handler_command().and_then(|c| command_exe(&c))?;
    let dir = exe.parent()?;
    let in_versions = dir
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|n| n.eq_ignore_ascii_case("Versions"));
    (in_versions && exe.file_name().is_some_and(|n| n.eq_ignore_ascii_case(PLAYER_EXE)))
        .then(|| dir.to_path_buf())
}

/// The roots a launch may use: the one chosen in Settings when it holds a
/// usable build (ignored rather than failing every launch otherwise), else all.
fn scoped_roots() -> Vec<Root> {
    let all = roots();
    match selected_root(&all) {
        Some(r) if !installs_in(&r).is_empty() => vec![r],
        _ => all,
    }
}

/// Whether a build is the live one or newer: by folder name first, then by
/// the version stamped in its exe (a build from another channel, or one
/// installed after the live answer was cached, has a different folder name
/// but is not out of date).
fn is_current(install: &Install, live: &Live, version_of: &dyn Fn(&Install) -> Option<(u32, u32)>) -> bool {
    if install.version().eq_ignore_ascii_case(&live.folder) {
        return true;
    }
    match (live.number, version_of(install)) {
        (Some(want), Some(have)) => have >= want,
        _ => false,
    }
}

/// Picks the build to launch, in this order:
///   1. the live build (or a newer one) in the roots allowed by Settings;
///   2. the live build (or a newer one) anywhere, since the chosen root's
///      out-of-date build would only make Roblox reinstall itself;
///   3. the build Roblox's own launcher is registered to;
///   4. the newest build on disk.
/// The newest-file rule alone is not enough: a machine can hold several
/// builds installed minutes apart (an old channel build, a half-finished
/// update), and the newest file is not always the live one. Starting a stale
/// one makes Roblox update and reinstall, closing the clients already open.
pub fn pick_install() -> Option<Install> {
    let scoped: Vec<Install> = scoped_roots().iter().flat_map(installs_in).collect();
    let everywhere: Vec<Install> = roots().iter().flat_map(installs_in).collect();
    choose_install(
        scoped,
        everywhere,
        live_version_cached().flatten().as_ref(),
        handler_version_dir().as_deref(),
        &exe_version,
    )
}

fn choose_install(
    scoped: Vec<Install>,
    everywhere: Vec<Install>,
    live: Option<&Live>,
    handler_dir: Option<&Path>,
    version_of: &dyn Fn(&Install) -> Option<(u32, u32)>,
) -> Option<Install> {
    // Several roots can hold the same build; the newest copy of it wins.
    let newest_where = |list: &[Install], keep: &dyn Fn(&Install) -> bool| {
        list.iter().filter(|i| keep(i)).max_by_key(|i| i.modified).cloned()
    };
    if let Some(live) = live {
        for list in [&scoped, &everywhere] {
            if let Some(i) = newest_where(list, &|i| i.version().eq_ignore_ascii_case(&live.folder)) {
                return Some(i);
            }
            if let Some(i) = newest_where(list, &|i| is_current(i, live, version_of)) {
                return Some(i);
            }
        }
    }
    if let Some(dir) = handler_dir {
        let key = path_key(dir);
        if let Some(i) = newest_where(&scoped, &|i| path_key(&i.dir) == key) {
            return Some(i);
        }
    }
    newest_where(&scoped, &|_| true)
}

/// Some(live folder) when Roblox has released a build that no install on
/// this PC has yet, so a launch would only make Roblox's updater run. Only
/// said when it's certain: Roblox answered, no install anywhere is the live
/// build or newer, and the build that would launch is plain Roblox
/// (bootstrappers may be on a channel of their own and update themselves).
pub fn update_pending() -> Option<String> {
    let live = live_version_cached().flatten()?;
    let install = pick_install()?;
    if !install.root.vanilla || is_current(&install, &live, &exe_version) {
        return None;
    }
    let any_current = roots()
        .iter()
        .flat_map(installs_in)
        .any(|i| is_current(&i, &live, &exe_version));
    (!any_current).then_some(live.folder)
}

/// Where FFlags are read from: the bootstrapper's Modifications overlay (its
/// source of truth, re-applied on each of its launches), or the version folder
/// itself for vanilla.
pub fn get_fflag_path() -> Option<PathBuf> {
    fflag_paths().into_iter().next()
}

/// Every file an FFlag write should land in, primary first. For a
/// bootstrapper that is the overlay plus the active version folder: this app
/// starts RobloxPlayerBeta directly, so a change written only to the overlay
/// would not take effect until the bootstrapper itself next ran.
pub fn fflag_paths() -> Vec<PathBuf> {
    const REL: [&str; 2] = ["ClientSettings", "ClientAppSettings.json"];
    let Some(install) = pick_install() else {
        return Vec::new();
    };
    let in_version = install.dir.join(REL[0]).join(REL[1]);
    match install.root.modifications() {
        Some(mods) => vec![mods.join(REL[0]).join(REL[1]), in_version],
        None => vec![in_version],
    }
}

/// Every detected root with its builds, for the Settings picker.
pub fn list_installs() -> Value {
    let all = roots();
    let selected = selected_root(&all).map(|r| path_key(&r.path));
    let active = pick_install().map(|i| path_key(&i.root.path));
    let list: Vec<Value> = all
        .iter()
        .filter_map(|r| {
            let builds = installs_in(r);
            if builds.is_empty() {
                return None;
            }
            let key = path_key(&r.path);
            Some(json!({
                "name": r.name,
                "path": r.path.to_string_lossy(),
                "kind": if r.vanilla { "vanilla" } else { "bootstrapper" },
                "isDefault": r.is_default,
                "selected": selected.as_deref() == Some(key.as_str()),
                "active": active.as_deref() == Some(key.as_str()),
                "hasModifications": r.modifications().is_some_and(|m| m.is_dir()),
                "versions": builds.iter().map(|b| b.version().to_string()).collect::<Vec<_>>(),
                "latest": builds.first().map(|b| b.version().to_string()),
            }))
        })
        .collect();
    Value::Array(list)
}

/// Drops the discovery cache, e.g. after the Version Manager installs a build.
pub fn invalidate_cache() {
    *ROOT_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Starts the client.
pub(super) fn spawn_roblox_direct(roblox_uri: &str) -> Option<u32> {
    let install = pick_install()?;
    // std rather than tokio: the client is detached and outlives us, and
    // dropping a std Child neither kills nor waits on it. The working
    // directory is the version folder, as a bootstrapper would set it, so the
    // client resolves its content folder the same way either way.
    let mut cmd = std::process::Command::new(&install.exe);
    cmd.arg(roblox_uri)
        .current_dir(&install.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    super::process::no_window_std(&mut cmd);
    cmd.spawn().ok().map(|child| child.id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_exe_is_parsed_from_quoted_and_bare_commands() {
        assert_eq!(
            command_exe(r#""C:\Users\a\AppData\Local\Bloxstrap\Bloxstrap.exe" -player "%1""#),
            Some(PathBuf::from(
                r"C:\Users\a\AppData\Local\Bloxstrap\Bloxstrap.exe"
            ))
        );
        assert_eq!(
            command_exe(r"C:\R\RobloxPlayerBeta.exe %1"),
            Some(PathBuf::from(r"C:\R\RobloxPlayerBeta.exe"))
        );
        assert_eq!(command_exe("   "), None);
    }

    #[test]
    fn handler_root_covers_vanilla_and_bootstrappers() {
        assert_eq!(
            root_of_handler(Path::new(
                r"C:\L\Roblox\Versions\version-abc\RobloxPlayerBeta.exe"
            )),
            Some(PathBuf::from(r"C:\L\Roblox"))
        );
        assert_eq!(
            root_of_handler(Path::new(r"C:\Games\Fishstrap\Fishstrap.exe")),
            Some(PathBuf::from(r"C:\Games\Fishstrap"))
        );
    }

    fn build(root: &str, version: &str, age_secs: u64) -> Install {
        let path = PathBuf::from(root);
        let dir = path.join("Versions").join(version);
        Install {
            root: Root { name: root.into(), path, vanilla: root == "Roblox", is_default: false },
            exe: dir.join(PLAYER_EXE),
            dir,
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - age_secs),
        }
    }

    fn live(folder: &str, number: Option<(u32, u32)>) -> Live {
        Live { folder: folder.into(), number }
    }
    fn no_versions(_: &Install) -> Option<(u32, u32)> {
        None
    }
    fn pick(list: Vec<Install>, l: Option<&Live>, reg: Option<&Path>) -> Option<Install> {
        choose_install(list.clone(), list, l, reg, &no_versions)
    }

    #[test]
    fn the_live_build_wins_over_a_newer_stale_file() {
        // The real case: a stale build whose exe is a few minutes newer.
        let list = vec![build("Roblox", "version-live", 300), build("Roblox", "version-stale", 100)];
        let p = pick(list, Some(&live("version-live", None)), None).unwrap();
        assert_eq!(p.version(), "version-live");
    }

    #[test]
    fn the_registered_build_is_next_when_roblox_cant_be_asked() {
        let list = vec![build("Roblox", "version-reg", 300), build("Roblox", "version-stale", 100)];
        let reg = PathBuf::from("Roblox").join("Versions").join("version-reg");
        let p = pick(list, None, Some(&reg)).unwrap();
        assert_eq!(p.version(), "version-reg");
    }

    #[test]
    fn falls_back_to_the_newest_build() {
        let list = vec![build("Roblox", "version-old", 300), build("Bloxstrap", "version-new", 100)];
        let p = pick(list, Some(&live("version-missing", None)), None).unwrap();
        assert_eq!(p.version(), "version-new");
        assert!(pick(Vec::new(), None, None).is_none());
    }

    #[test]
    fn a_build_at_or_past_the_live_number_counts_as_current() {
        // Installed after the live answer was cached, or from another channel.
        let ver = |i: &Install| match i.version() {
            "version-newer" => Some((748, 7480001)),
            "version-older" => Some((747, 7471038)),
            _ => None,
        };
        let l = live("version-gone", Some((747, 7471481)));
        let newer = build("Roblox", "version-newer", 300);
        let older = build("Roblox", "version-older", 100);
        assert!(is_current(&newer, &l, &ver));
        assert!(!is_current(&older, &l, &ver), "same release, smaller changelist");
        let list = vec![newer, older];
        let p = choose_install(list.clone(), list, Some(&l), None, &ver).unwrap();
        assert_eq!(p.version(), "version-newer");
    }

    #[test]
    fn the_live_build_in_another_root_beats_a_stale_chosen_root() {
        let scoped = vec![build("Roblox", "version-stale", 100)];
        let everywhere = vec![build("Roblox", "version-stale", 100), build("Bloxstrap", "version-live", 300)];
        let p = choose_install(scoped, everywhere, Some(&live("version-live", None)), None, &no_versions).unwrap();
        assert_eq!(p.version(), "version-live");
    }

    /// Reads the builds installed on this machine; run by hand with --ignored.
    #[test]
    #[ignore]
    fn print_installed_builds() {
        for i in roots().iter().flat_map(installs_in) {
            println!("{} {} {:?} raw={:?} exe={}", i.root.name, i.version(), exe_version(&i), read_file_version(&i.exe), i.exe.display());
        }
        println!("pick: {:?}", pick_install().map(|i| i.version().to_string()));
    }

    #[test]
    fn version_numbers_parse_both_ways_roblox_writes_them() {
        assert_eq!(parse_version_number("0.747.0.7471481"), Some((747, 7471481)));
        assert_eq!(parse_version_number("0, 747, 662, 7471038"), Some((747, 7471038)));
        assert_eq!(parse_version_number("version-abc"), None);
        assert_eq!(parse_version_number("1.2.3"), None);
    }

    #[test]
    fn path_keys_ignore_case_and_trailing_separators() {
        assert_eq!(
            path_key(Path::new(r"C:\A\Bloxstrap\")),
            path_key(Path::new(r"c:\a\bloxstrap"))
        );
    }

    #[test]
    fn only_the_roblox_folder_counts_as_vanilla() {
        assert!(is_vanilla_name("Roblox"));
        assert!(is_vanilla_name("roblox"));
        assert!(!is_vanilla_name("Bloxstrap"));
    }
}
