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

/// Picks the build to launch. The newest build wins; RobloxPlayerBeta's
/// own mtime tracks the last real client update, which a bootstrapper's mod
/// overlay never touches, so it reliably points at the install actually in
/// use. A root chosen in Settings narrows both searches to that root, and is
/// ignored (rather than failing every launch) if it holds no usable build.
pub fn pick_install() -> Option<Install> {
    let all = roots();
    let scoped: Vec<Root> = match selected_root(&all) {
        Some(r) if !installs_in(&r).is_empty() => vec![r],
        _ => all,
    };
    let installs: Vec<Install> = scoped.iter().flat_map(installs_in).collect();
    installs.into_iter().max_by_key(|i| i.modified)
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
