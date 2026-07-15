// The Electron build stored its userData at %APPDATA%\multiroblox (Electron
// derives that folder name from package.json's "name" field). Tauri's own
// app_data_dir() would resolve to a DIFFERENT folder (keyed off the bundle
// identifier), which would silently orphan every existing user's saved
// accounts/settings on upgrade. Hardcoding the exact legacy path is what
// makes this migration lossless.
use std::path::PathBuf;

pub fn app_data_dir() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    let dir = PathBuf::from(base).join("multiroblox");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn settings_path() -> PathBuf {
    app_data_dir().join("settings.json")
}
pub fn accounts_path() -> PathBuf {
    app_data_dir().join("accounts.json")
}
pub fn packages_path() -> PathBuf {
    app_data_dir().join("packages.json")
}
pub fn genhistory_path() -> PathBuf {
    app_data_dir().join("genhistory.json")
}
pub fn local_state_path() -> PathBuf {
    app_data_dir().join("Local State")
}
