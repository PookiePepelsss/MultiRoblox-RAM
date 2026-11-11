// Locating RobloxNative.exe.
use crate::paths::app_data_dir;
use crate::state::AppState;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

// Tauri's resource_dir() returns a \\?\-prefixed path on Windows, which not
// every consumer of such a path handles, so it is stripped first.
fn strip_verbatim_prefix(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => p.to_path_buf(),
    }
}

fn bundled_native_exe_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().resource_dir().ok().map(|d| {
        strip_verbatim_prefix(&d)
            .join("resources")
            .join("RobloxNative.exe")
    })
}

// Baked in at compile time so the exe works standalone without its sibling
// resources/ folder; extracted to the app data dir on first use.
const EMBEDDED_NATIVE_EXE: &[u8] = include_bytes!("../../resources/RobloxNative.exe");

static EMBEDDED_NATIVE_SHA: once_cell::sync::Lazy<[u8; 32]> = once_cell::sync::Lazy::new(|| {
    use sha2::Digest;
    sha2::Sha256::digest(EMBEDDED_NATIVE_EXE).into()
});

// Content-hashed, not length-compared. The helper speaks a line protocol, so
// an older extracted copy isn't merely stale; it doesn't understand "daemon"
// at all, prints usage to stderr and exits immediately, which the supervisor
// would then see as a crash and restart forever. A same-length build is
// unlikely but entirely possible, and the failure mode is an endless stream
// of spawned processes, so this compares what's actually in the file.
fn ensure_embedded_native_exe() -> Option<PathBuf> {
    let out = app_data_dir().join("RobloxNative.exe");
    let needs_write = match std::fs::read(&out) {
        Ok(bytes) => {
            use sha2::Digest;
            let on_disk: [u8; 32] = sha2::Sha256::digest(&bytes).into();
            on_disk != *EMBEDDED_NATIVE_SHA
        }
        Err(_) => true,
    };
    if needs_write {
        std::fs::create_dir_all(out.parent()?).ok()?;
        // A running helper holds this file open and the write fails; callers
        // sweep strays before extracting, so that's already handled.
        if let Err(e) = std::fs::write(&out, EMBEDDED_NATIVE_EXE) {
            eprintln!("[helper] could not refresh RobloxNative.exe: {}", e);
            // Only fall through to the existing copy if there is one; better
            // a stale helper than none, and the hash check above will retry
            // on the next run.
            if !out.exists() {
                return None;
            }
        }
    }
    Some(out)
}

// Memoized for the app session: Some(Some(path)) = usable exe, Some(None) =
// resolved-to-unavailable, None = not yet resolved.
pub fn ensure_native_helper(app: &AppHandle, state: &AppState) -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let cached = state.native_helper_path.lock().unwrap().clone();
    if let Some(resolved) = cached {
        return resolved;
    }
    let resolved = resolve_native_helper(app);
    *state.native_helper_path.lock().unwrap() = Some(resolved.clone());
    resolved
}

fn resolve_native_helper(app: &AppHandle) -> Option<PathBuf> {
    // Installed builds ship RobloxNative.exe as a normal loose resource next
    // to MultiRoblox.exe (see tauri.conf.json's bundle.resources); prefer
    // that plain, installer-placed file over self-extracting the embedded
    // copy. A binary that writes another PE from its own resources to disk
    // at runtime and executes it is a classic dropper behavior pattern to
    // heuristic/ML antivirus scanners, even when (as here) it's benign; not
    // doing that at all in the common case is strictly better than doing it
    // and hoping a scanner doesn't mind. The embedded fallback below still
    // covers running the bare .exe standalone without its resources/ folder.
    if let Some(b) = bundled_native_exe_path(app) {
        if b.exists() {
            return Some(b);
        }
    }
    // Deliberately no compile-from-source fallback after this. Producing an
    // executable at runtime via a compiler (the old csc.exe path) is one of
    // the strongest heuristics antivirus engines have, because that is
    // exactly how droppers keep a payload out of a static scan. It was also
    // unreachable in practice: the extraction below only fails if the folder
    // can't be written at all, in which case csc couldn't have written there
    // either.
    ensure_embedded_native_exe()
}
