// Port of main.js's key-session logic (getEncryptionKey/getLegacyKey/
// encryptField/decryptField/passphraseMode). Ciphertext tags (gs:/gcm:/cbc:/
// safe:) are unchanged so records written by the Electron build keep
// decrypting here. New writes use "safe2:" (plain DPAPI, no Local State
// dependency) instead of Electron's Chromium-flavoured "safe:"; see
// crypto.rs for why.
use crate::crypto;
use crate::paths::local_state_path;
use crate::settings::{self, Settings};
use crate::state::AppState;
use rand::RngCore;
use serde_json::Value;
use tauri::Manager;

const VERIFY_TOKEN: &str = "multiroblox-verify-v1";
/// The generator API key, encrypted like the cookies. Backend-owned: never
/// handed to the frontend through settings_load, nor accepted through settings_save.
pub const GEN_KEY_SETTING: &str = "genApiKeyEnc";

pub fn load_gen_key(state: &AppState) -> String {
    settings::get_str(GEN_KEY_SETTING)
        .and_then(|ct| decrypt_field(state, &ct))
        .unwrap_or_default()
}

pub fn save_gen_key(state: &AppState, key: &str) -> Result<(), String> {
    let sealed = if key.is_empty() {
        None
    } else {
        Some(encrypt_field(state, key)?)
    };
    settings::update(|s| {
        match sealed {
            Some(ct) => s.insert(GEN_KEY_SETTING.into(), Value::String(ct)),
            None => s.remove(GEN_KEY_SETTING),
        };
    })
}

fn non_empty(s: &Settings, key: &str) -> bool {
    s.get(key)
        .and_then(Value::as_str)
        .is_some_and(|v| !v.is_empty())
}

pub fn passphrase_mode() -> bool {
    let s = settings::snapshot();
    let has_key = s
        .get("customKey")
        .and_then(Value::as_str)
        .is_some_and(|v| !v.trim().is_empty());
    non_empty(&s, "keyVerifier") || non_empty(&s, "customKeyEnc") || has_key
}

fn current_salt() -> String {
    settings::get_str("kdfSalt").unwrap_or_else(|| crypto::LEGACY_SALT.to_string())
}

fn verifier_for_key(key: &[u8; 32]) -> String {
    crypto::encrypt_gcm(VERIFY_TOKEN, key, "gs")
}

/// Derives the passphrase key and checks it against the stored verifier.
/// Returns the key on success so the caller can cache it instead of paying
/// for a second ~340ms scrypt derive on the first decrypt.
pub fn verify_pass(pass: &str) -> Option<[u8; 32]> {
    let verifier = settings::get_str("keyVerifier")?;
    let key = crypto::derive_scrypt_key(pass, &current_salt());
    (crypto::decrypt_gcm(&verifier, &key, "gs").as_deref() == Some(VERIFY_TOKEN)).then_some(key)
}

fn parse_device_key(s: &Settings) -> Option<[u8; 32]> {
    let bytes = hex::decode(s.get("_deviceKey")?.as_str()?).ok()?;
    bytes.try_into().ok()
}

fn get_or_create_device_key() -> [u8; 32] {
    if let Some(k) = parse_device_key(&settings::snapshot()) {
        return k;
    }
    let mut fresh = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut fresh);
    // Check-and-insert as one update: two first-time callers racing would
    // otherwise each mint a key and the loser's would be cached in memory
    // while the winner's reached disk.
    let stored = settings::update(|s| {
        if let Some(k) = parse_device_key(s) {
            return k;
        }
        s.insert("_deviceKey".into(), Value::String(hex::encode(fresh)));
        fresh
    });
    stored.unwrap_or_else(|e| {
        eprintln!("[encryption] {e}");
        fresh
    })
}

fn safe_storage_ready() -> bool {
    cfg!(windows)
}

pub fn get_encryption_key(state: &AppState) -> Option<[u8; 32]> {
    if let Some(k) = *state.cached_key.lock().unwrap() {
        return Some(k);
    }
    let session_pass = state.session_pass.lock().unwrap().clone();
    let k = match session_pass {
        Some(pass) => crypto::derive_scrypt_key(&pass, &current_salt()),
        None if !passphrase_mode() => get_or_create_device_key(),
        None => return None, // locked
    };
    *state.cached_key.lock().unwrap() = Some(k);
    Some(k)
}

pub fn get_legacy_key(state: &AppState) -> Option<[u8; 32]> {
    if let Some(k) = *state.cached_legacy_key.lock().unwrap() {
        return Some(k);
    }
    let session_pass = state.session_pass.lock().unwrap().clone();
    let k = match session_pass {
        Some(pass) => crypto::derive_legacy_key(&pass),
        None if !passphrase_mode() => get_or_create_device_key(),
        None => return None,
    };
    *state.cached_legacy_key.lock().unwrap() = Some(k);
    Some(k)
}

/// Forgets every derived key, and with them every cookie decrypted under
/// one (see storage::invalidate_decrypted).
pub fn invalidate_key_cache(state: &AppState) {
    *state.cached_key.lock().unwrap() = None;
    *state.cached_legacy_key.lock().unwrap() = None;
    crate::storage::invalidate_decrypted();
}

/// Unlocks passphrase mode for this session. False on a wrong passphrase.
pub fn unlock(state: &AppState, pass: &str) -> bool {
    if pass.trim().is_empty() {
        return false;
    }
    // enc_set_key stores the passphrase trimmed, so " abc " was saved as
    // "abc" and could never be unlocked as typed. The exact text is still
    // tried first for anything set some other way.
    let (pass, key) = match verify_pass(pass) {
        Some(key) => (pass, key),
        None => match verify_pass(pass.trim()) {
            Some(key) if pass.trim() != pass => (pass.trim(), key),
            _ => return false,
        },
    };
    *state.session_pass.lock().unwrap() = Some(pass.to_string());
    invalidate_key_cache(state);
    *state.cached_key.lock().unwrap() = Some(key);
    true
}

/// Sets, changes or (with an empty `pass`) removes the passphrase, and
/// re-encrypts every stored secret to match.
///
/// The salt, verifier and setup flag land in one settings write, so there is
/// no state in which the salt has rotated but the verifier still belongs to
/// the old one. If the accounts then can't be written, the settings and
/// session key are put back, so the key on record always matches the
/// ciphertext on disk.
pub fn set_passphrase(state: &AppState, pass: &str) -> Result<(), String> {
    const TOUCHED: [&str; 5] = [
        "customKey",
        "customKeyEnc",
        "keyVerifier",
        "kdfSalt",
        "encSetupDone",
    ];
    let new_pass = (!pass.is_empty()).then(|| pass.to_string());
    // Derived before taking any lock: scrypt is the slow part (~340ms).
    let new_secret = new_pass.as_ref().map(|p| {
        let mut bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut bytes);
        let salt = hex::encode(bytes);
        let key = crypto::derive_scrypt_key(p, &salt);
        (salt, key)
    });
    // Read under the old key; written back under the new one below.
    let gen_key = load_gen_key(state);

    let rekeyed = crate::storage::ACCOUNTS.rekey(state, &crate::storage::GENHISTORY, || {
        let previous = settings::update(|s| {
            let previous: Vec<(&str, Option<Value>)> =
                TOUCHED.iter().map(|k| (*k, s.get(*k).cloned())).collect();
            s.remove("customKey");
            s.remove("customKeyEnc");
            match &new_secret {
                Some((salt, key)) => {
                    s.insert("kdfSalt".into(), Value::String(salt.clone()));
                    s.insert("keyVerifier".into(), Value::String(verifier_for_key(key)));
                }
                None => {
                    s.remove("keyVerifier");
                }
            }
            s.insert("encSetupDone".into(), Value::Bool(true));
            previous
        })?;
        let previous_pass = std::mem::replace(&mut *state.session_pass.lock().unwrap(), new_pass);
        invalidate_key_cache(state);
        if let Some((_, key)) = &new_secret {
            *state.cached_key.lock().unwrap() = Some(*key);
        }
        Ok(move || {
            let restored = settings::update(|s| {
                for (k, v) in previous {
                    match v {
                        Some(v) => s.insert(k.to_string(), v),
                        None => s.remove(k),
                    };
                }
            });
            if let Err(e) = restored {
                eprintln!("[encryption] could not roll back key settings: {e}");
            }
            *state.session_pass.lock().unwrap() = previous_pass;
            invalidate_key_cache(state);
        })
    });
    if rekeyed.is_ok() && !gen_key.is_empty() {
        // Only costs re-typing the key if it fails, so it doesn't fail the change.
        if let Err(e) = save_gen_key(state, &gen_key) {
            eprintln!("[encryption] could not re-encrypt the generator API key: {e}");
        }
    }
    rekeyed
}

/// Pre-derives the unlocked passphrase key off the async runtime's blocking
/// pool so the first decrypt hits the cache instead of blocking on a
/// ~340ms scrypt derive. No-op when locked or machine-bound.
pub fn prewarm_key(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        if state.cached_key.lock().unwrap().is_some() {
            return;
        }
        let Some(pass) = state.session_pass.lock().unwrap().clone() else {
            return;
        };
        let salt = current_salt();
        let key = tokio::task::spawn_blocking(move || crypto::derive_scrypt_key(&pass, &salt))
            .await
            .ok();
        if let Some(key) = key {
            let mut cached = state.cached_key.lock().unwrap();
            if cached.is_none() {
                *cached = Some(key);
            }
        }
    });
}

pub fn is_encrypted(v: &str) -> bool {
    v.starts_with("safe2:")
        || v.starts_with("safe:")
        || v.starts_with("gs:")
        || v.starts_with("gcm:")
        || v.starts_with("cbc:")
}

pub fn encrypt_field(state: &AppState, plaintext: &str) -> Result<String, String> {
    if state.session_pass.lock().unwrap().is_some() {
        let key = get_encryption_key(state).ok_or("locked")?;
        return Ok(crypto::encrypt_gcm(plaintext, &key, "gs"));
    }
    if passphrase_mode() {
        return Err("locked".into());
    }
    if safe_storage_ready() {
        if let Some(enc) = crypto::encrypt_safe2(plaintext) {
            return Ok(enc);
        }
    }
    let key = get_encryption_key(state).ok_or("locked")?;
    Ok(crypto::encrypt_gcm(plaintext, &key, "gs"))
}

pub fn decrypt_field(state: &AppState, ct: &str) -> Option<String> {
    if ct.is_empty() {
        return None;
    }
    if ct.starts_with("safe2:") {
        return crypto::decrypt_safe2(ct);
    }
    if let Some(body) = ct.strip_prefix("safe:") {
        return crypto::decrypt_electron_safe_storage(body, &local_state_path());
    }
    if ct.starts_with("gs:") {
        return crypto::decrypt_gcm(ct, &get_encryption_key(state)?, "gs");
    }
    if ct.starts_with("gcm:") {
        return crypto::decrypt_gcm(ct, &get_legacy_key(state)?, "gcm");
    }
    if ct.starts_with("cbc:") {
        return crypto::decrypt_cbc(ct, &get_legacy_key(state)?);
    }
    Some(ct.to_string())
}

/// Runs once at startup: migrate legacy customKey/customKeyEnc formats to the
/// verifier model. Unlike the old Electron build's per-boot session cache,
/// this app asks for the passphrase on every launch (writeSessionKey was
/// already disabled in the source main.js).
pub fn init_encryption(state: &AppState) {
    let s = settings::snapshot();
    if s.get("keyVerifier").and_then(Value::as_str).is_some() {
        return;
    }
    let mut legacy: Option<String> = None;
    if let Some(enc) = s.get("customKeyEnc").and_then(Value::as_str) {
        if !enc.is_empty() {
            // customKeyEnc was written via Electron's safeStorage.encryptString directly
            // (same Chromium os_crypt scheme as the account "safe:" cookies, just without
            // the tag prefix); reuse the same Local-State-backed reader, not raw DPAPI.
            legacy = crypto::decrypt_electron_safe_storage(enc, &local_state_path());
        }
    }
    if legacy.is_none() {
        if let Some(k) = s.get("customKey").and_then(Value::as_str) {
            if !k.trim().is_empty() {
                legacy = Some(k.trim().to_string());
            }
        }
    }
    let Some(pass) = legacy else {
        return;
    };
    let key = crypto::derive_scrypt_key(&pass, &current_salt());
    let saved = settings::update(|s| {
        s.remove("customKey");
        s.remove("customKeyEnc");
        s.insert("keyVerifier".into(), Value::String(verifier_for_key(&key)));
    });
    if let Err(e) = saved {
        eprintln!("[encryption] {e}");
    }
    *state.session_pass.lock().unwrap() = Some(pass);
    *state.cached_key.lock().unwrap() = Some(key);
}
