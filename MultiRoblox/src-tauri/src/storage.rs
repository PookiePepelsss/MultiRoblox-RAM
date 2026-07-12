use crate::encryption::{decrypt_field, encrypt_field, is_encrypted};
use crate::paths::{accounts_path, genhistory_path, packages_path};
use crate::state::AppState;
use serde_json::Value;

fn read_json_array(path: &std::path::Path) -> Vec<Value> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}
pub fn write_json_array(path: &std::path::Path, v: &[Value]) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(v).unwrap_or_else(|_| "[]".into());
    std::fs::write(path, json)
}

pub fn load_accounts_raw() -> Vec<Value> {
    read_json_array(&accounts_path())
}

// Decrypt failure is itself proof a cookie is unusable -- flag it immediately
// (_cookieInvalid) instead of waiting on the renderer's async network
// validation to discover the same thing (matches the fix already shipped in
// the Electron build's main.js this session).
pub fn decrypt_account(state: &AppState, mut a: Value) -> Value {
    if let Some(cookie) = a.get("cookie").and_then(|v| v.as_str()) {
        if !cookie.is_empty() {
            let dec = decrypt_field(state, cookie);
            if dec.is_none() {
                a["_cookieInvalid"] = Value::Bool(true);
            }
            a["cookie"] = Value::String(dec.unwrap_or_default());
        }
    }
    a
}
fn encrypt_account(state: &AppState, mut a: Value) -> Result<Value, String> {
    if let Some(cookie) = a.get("cookie").and_then(|v| v.as_str()) {
        if !cookie.is_empty() && !is_encrypted(cookie) {
            let enc = encrypt_field(state, cookie)?;
            a["cookie"] = Value::String(enc);
        }
    }
    a["_enc"] = Value::Bool(true);
    Ok(a)
}

pub fn load_accounts(state: &AppState) -> Vec<Value> {
    read_json_array(&accounts_path()).into_iter().map(|a| decrypt_account(state, a)).collect()
}

pub fn save_accounts(state: &AppState, accounts: Vec<Value>) -> Result<(), String> {
    let mut out = Vec::with_capacity(accounts.len());
    for a in accounts {
        out.push(encrypt_account(state, a)?);
    }
    write_json_array(&accounts_path(), &out).map_err(|e| e.to_string())
}

/// One-time, best-effort upgrade: re-encrypt any legacy device-key (gcm:) or
/// unauthenticated (cbc:) cookies to DPAPI storage (safe2:). Only runs when no
/// passphrase is set. Aborts untouched if any non-empty cookie fails to
/// decrypt, so a bad read can never wipe data.
pub fn migrate_account_encryption_to_keychain(state: &AppState) {
    if crate::encryption::passphrase_mode() {
        return; // passphrase user: never touch (avoids wrong-key writes)
    }
    let raw = load_accounts_raw();
    let needs = raw.iter().any(|a| a.get("cookie").and_then(|v| v.as_str()).map(|c| c.starts_with("gcm:") || c.starts_with("cbc:")).unwrap_or(false));
    if !needs {
        return;
    }
    let plain: Vec<Value> = raw.iter().cloned().map(|a| decrypt_account(state, a)).collect();
    for (orig, dec) in raw.iter().zip(plain.iter()) {
        let had = orig.get("cookie").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
        let empty = dec.get("cookie").and_then(|v| v.as_str()).map(|s| s.is_empty()).unwrap_or(true);
        if had && empty {
            eprintln!("[migrate] decrypt failed; leaving accounts untouched");
            return;
        }
    }
    if save_accounts(state, plain).is_ok() {
        println!("[migrate] upgraded account encryption to DPAPI");
    }
}

pub fn load_packages() -> Vec<Value> {
    read_json_array(&packages_path())
}
pub fn save_packages(packages: &[Value]) -> Result<(), String> {
    write_json_array(&packages_path(), packages).map_err(|e| e.to_string())
}

fn decrypt_gen_entry(state: &AppState, mut e: Value) -> Value {
    for field in ["password", "cookie"] {
        if let Some(v) = e.get(field).and_then(|v| v.as_str()) {
            if !v.is_empty() {
                e[field] = Value::String(decrypt_field(state, v).unwrap_or_default());
            }
        }
    }
    e
}
fn encrypt_gen_entry(state: &AppState, mut e: Value) -> Result<Value, String> {
    for field in ["password", "cookie"] {
        if let Some(v) = e.get(field).and_then(|v| v.as_str()) {
            if !v.is_empty() && !is_encrypted(v) {
                let enc = encrypt_field(state, v)?;
                e[field] = Value::String(enc);
            }
        }
    }
    Ok(e)
}

pub fn read_genhistory(state: &AppState) -> Vec<Value> {
    read_json_array(&genhistory_path()).into_iter().map(|e| decrypt_gen_entry(state, e)).collect()
}
pub fn write_genhistory(state: &AppState, list: Vec<Value>) -> Result<(), String> {
    let capped: Vec<Value> = list.into_iter().take(500).collect();
    let mut out = Vec::with_capacity(capped.len());
    for e in capped {
        out.push(encrypt_gen_entry(state, e)?);
    }
    write_json_array(&genhistory_path(), &out).map_err(|e| e.to_string())
}
