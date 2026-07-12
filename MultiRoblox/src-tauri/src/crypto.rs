// Port of the Electron app's field-level encryption (see the old main.js
// SALT/encryptGCM/decryptField block). Ciphertext format tags are unchanged
// so every account/genhistory record written by the Electron build keeps
// decrypting correctly after migrating to this app -- nothing gets re-keyed
// except through the same explicit passphrase-change path as before.
use aes::Aes256;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use pbkdf2::pbkdf2_hmac;
use rand::RngCore;
use sha2::Sha512;

pub const LEGACY_SALT: &str = "multiroblox-v1-salt-2025";
const ITERATIONS: u32 = 210_000;
const KEY_LEN: usize = 32;
// N=2^16, r=8, p=1 -- mirrors SCRYPT_PARAMS in the old main.js.
const SCRYPT_LOG_N: u8 = 16;
const SCRYPT_R: u32 = 8;
const SCRYPT_P: u32 = 1;

pub fn derive_scrypt_key(pass: &str, salt: &str) -> [u8; KEY_LEN] {
    let params = scrypt::Params::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P, KEY_LEN).expect("valid scrypt params");
    let mut out = [0u8; KEY_LEN];
    scrypt::scrypt(pass.as_bytes(), salt.as_bytes(), &params, &mut out).expect("scrypt derive");
    out
}

pub fn derive_legacy_key(pass: &str) -> [u8; KEY_LEN] {
    let mut out = [0u8; KEY_LEN];
    pbkdf2_hmac::<Sha512>(pass.as_bytes(), LEGACY_SALT.as_bytes(), ITERATIONS, &mut out);
    out
}

/// `tag:iv_b64:authtag_b64:data_b64` -- matches encryptGCM in the old main.js
/// (Node's cipher.getAuthTag() is appended separately from the ciphertext;
/// the `aes-gcm` crate instead appends the 16-byte tag to the ciphertext, so
/// we split/rejoin to keep the on-disk format identical).
pub fn encrypt_gcm(plaintext: &str, key: &[u8; KEY_LEN], tag: &str) -> String {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut iv = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut iv);
    let nonce = Nonce::from_slice(&iv);
    let ct = cipher.encrypt(nonce, plaintext.as_bytes()).expect("aes-gcm encrypt");
    let (data, authtag) = ct.split_at(ct.len() - 16);
    format!("{}:{}:{}:{}", tag, B64.encode(iv), B64.encode(authtag), B64.encode(data))
}

pub fn decrypt_gcm(ct: &str, key: &[u8; KEY_LEN], tag: &str) -> Option<String> {
    let prefix = format!("{}:", tag);
    let rest = ct.strip_prefix(&prefix)?;
    let parts: Vec<&str> = rest.split(':').collect();
    if parts.len() < 3 {
        return None;
    }
    let iv = B64.decode(parts[0]).ok()?;
    let authtag = B64.decode(parts[1]).ok()?;
    let data = B64.decode(parts[2]).ok()?;
    if iv.len() != 12 {
        return None;
    }
    let mut combined = data;
    combined.extend_from_slice(&authtag);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(&iv);
    let pt = cipher.decrypt(nonce, combined.as_ref()).ok()?;
    String::from_utf8(pt).ok()
}

/// Legacy unauthenticated CBC reader -- read-only, ported from decryptCBC.
/// Never produced by new writes; kept so cbc: records from very old builds
/// still decrypt and migrate forward on next save.
pub fn decrypt_cbc(ct: &str, key: &[u8; KEY_LEN]) -> Option<String> {
    let rest = ct.strip_prefix("cbc:")?;
    let parts: Vec<&str> = rest.split(':').collect();
    if parts.len() < 2 {
        return None;
    }
    let iv = B64.decode(parts[0]).ok()?;
    let mut data = B64.decode(parts[1]).ok()?;
    type Aes256CbcDec = cbc::Decryptor<Aes256>;
    let cipher = Aes256CbcDec::new_from_slices(key, &iv).ok()?;
    let pt = cipher.decrypt_padded_mut::<Pkcs7>(&mut data).ok()?;
    String::from_utf8(pt.to_vec()).ok()
}

// ---- DPAPI (Windows) --------------------------------------------------
// Direct CryptProtectData/CryptUnprotectData passthrough, used for this
// app's OWN "safe2:" ciphertext (new writes, no separate key file needed --
// DPAPI ties the blob to the logged-in Windows user by itself).
#[cfg(windows)]
pub mod dpapi {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData};

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        unsafe {
            let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
            let mut output = CRYPT_INTEGER_BLOB::default();
            CryptProtectData(&input, windows::core::PCWSTR::null(), None, None, None, 1 /* CRYPTPROTECT_UI_FORBIDDEN */, &mut output).ok()?;
            let out = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
            let _ = LocalFree(HLOCAL(output.pbData as *mut _));
            Some(out)
        }
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        unsafe {
            let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
            let mut output = CRYPT_INTEGER_BLOB::default();
            CryptUnprotectData(&input, None, None, None, None, 1, &mut output).ok()?;
            let out = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
            let _ = LocalFree(HLOCAL(output.pbData as *mut _));
            Some(out)
        }
    }
}

#[cfg(not(windows))]
pub mod dpapi {
    pub fn protect(_data: &[u8]) -> Option<Vec<u8>> { None }
    pub fn unprotect(_data: &[u8]) -> Option<Vec<u8>> { None }
}

pub fn encrypt_safe2(plaintext: &str) -> Option<String> {
    let blob = dpapi::protect(plaintext.as_bytes())?;
    Some(format!("safe2:{}", B64.encode(blob)))
}
pub fn decrypt_safe2(ct: &str) -> Option<String> {
    let rest = ct.strip_prefix("safe2:")?;
    let blob = B64.decode(rest).ok()?;
    let pt = dpapi::unprotect(&blob)?;
    String::from_utf8(pt).ok()
}

// ---- Legacy Electron `safeStorage` reader (Windows) --------------------
// Electron's safeStorage.encryptString on Windows is Chromium's OSCrypt:
// a single AES-256-GCM key is generated once, DPAPI-wrapped, and cached in
// the app's "Local State" JSON file (os_crypt.encrypted_key, base64, with a
// "DPAPI" ASCII prefix before the DPAPI blob). Each encrypted string is then
// "v10" + 12-byte nonce + ciphertext+tag, base64-encoded. This reader exists
// ONLY to keep decrypting accounts saved by the old Electron build; new
// writes use encrypt_safe2 (plain DPAPI, no key file) instead.
pub fn decrypt_electron_safe_storage(ct_b64: &str, local_state_path: &std::path::Path) -> Option<String> {
    let raw = base64_maybe(ct_b64)?;
    let local_state = std::fs::read_to_string(local_state_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&local_state).ok()?;
    let enc_key_b64 = json.get("os_crypt")?.get("encrypted_key")?.as_str()?;
    let enc_key = B64.decode(enc_key_b64).ok()?;
    let dpapi_blob = enc_key.strip_prefix(b"DPAPI")?;
    let aes_key = dpapi::unprotect(dpapi_blob)?;
    if aes_key.len() != 32 {
        return None;
    }
    let body = raw.strip_prefix(b"v10").or_else(|| raw.strip_prefix(b"v11"))?;
    if body.len() < 12 + 16 {
        return None;
    }
    let (nonce_bytes, ct_and_tag) = body.split_at(12);
    let mut key_arr = [0u8; 32];
    key_arr.copy_from_slice(&aes_key);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_arr));
    let nonce = Nonce::from_slice(nonce_bytes);
    let pt = cipher.decrypt(nonce, ct_and_tag).ok()?;
    String::from_utf8(pt).ok()
}

fn base64_maybe(s: &str) -> Option<Vec<u8>> {
    B64.decode(s).ok()
}
