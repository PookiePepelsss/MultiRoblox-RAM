// accounts.json, packages.json and genhistory.json.
//
// Accounts are decrypted once and then served from memory. Decrypting is a
// DPAPI call (or an AES pass) per cookie, and accounts are looked up on every
// launch, kill and closed-instance tick, so re-reading and re-decrypting the
// whole file each time was most of the cost of those paths. The cache is
// dropped whenever the key could have changed (unlock, passphrase change,
// wipe) via a global epoch, so it can never serve plaintext produced under a
// key that is no longer current.
//
// Every write is a read-modify-write under one lock per file. Before this,
// two commands landing together (a launch stamping lastUsed while the UI
// saved an edit) each loaded, changed and saved their own copy, and the
// second save silently erased the first.
use crate::encryption::is_encrypted;
use crate::jsonfile;
use crate::paths::{accounts_path, genhistory_path, packages_path};
use crate::state::AppState;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Field-level encryption, abstracted so the store logic can be tested
/// without a running app (AppState needs a Tauri handle).
pub trait FieldCipher {
    fn seal(&self, plaintext: &str) -> Result<String, String>;
    fn open(&self, ciphertext: &str) -> Option<String>;
}

impl FieldCipher for AppState {
    fn seal(&self, plaintext: &str) -> Result<String, String> {
        crate::encryption::encrypt_field(self, plaintext)
    }
    fn open(&self, ciphertext: &str) -> Option<String> {
        crate::encryption::decrypt_field(self, ciphertext)
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

static KEY_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Marks every cached decryption stale. Called on any change to the session
/// key (unlock, passphrase change, wipe). Lock-free on purpose: it runs from
/// inside the passphrase change, which already holds the accounts lock.
pub fn invalidate_decrypted() {
    KEY_EPOCH.fetch_add(1, Ordering::SeqCst);
}

const ACCOUNTS_UNREADABLE: &str =
    "accounts.json could not be read, so saving was stopped to avoid overwriting it. \
     A copy is at accounts.corrupt-backup.json - fix or remove accounts.json, then restart.";

pub fn account_id(a: &Value) -> Option<&str> {
    a.get("id").and_then(Value::as_str)
}

fn str_field<'a>(a: &'a Value, key: &str) -> Option<&'a str> {
    a.get(key).and_then(Value::as_str)
}

/// The handful of account fields the backend itself reads. The record stays
/// a free-form JSON object everywhere else, since it carries whatever fields
/// the frontend chooses to store.
#[derive(Debug, Clone, Default)]
pub struct AccountInfo {
    pub username: Option<String>,
    pub user_id: Option<String>,
    pub cookie: String,
    pub game_target: String,
}

impl AccountInfo {
    pub fn from_value(a: &Value) -> Self {
        Self {
            username: str_field(a, "username").map(str::to_owned),
            user_id: str_field(a, "userId").map(str::to_owned),
            cookie: str_field(a, "cookie").unwrap_or("").to_owned(),
            game_target: str_field(a, "gameTarget").unwrap_or("").to_owned(),
        }
    }

    /// The name to show in logs: the username when known, else `fallback`
    /// (normally the account id).
    pub fn display<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.username.as_deref().unwrap_or(fallback)
    }
}

struct Decrypted {
    epoch: u64,
    list: Vec<Value>,
    /// Ciphertext of cookies that failed to decrypt, by account id. They are
    /// handed to the UI as "" (flagged `_cookieInvalid`), and before this the
    /// next save of any account wrote that "" back, destroying a cookie that
    /// was only unreadable under the current key. Now the original bytes go
    /// back to disk untouched until the account gets a new cookie.
    undecryptable: HashMap<String, String>,
    /// plaintext cookie -> the ciphertext it was read from or last written
    /// as, so a save only pays for encrypting cookies that actually changed.
    sealed: HashMap<String, String>,
    /// Set when a write stored something whose decrypted form isn't known
    /// here; the next access re-reads instead of trusting memory.
    stale: bool,
}

pub struct AccountStore {
    path: fn() -> PathBuf,
    slot: Mutex<Option<Decrypted>>,
}

pub static ACCOUNTS: AccountStore = AccountStore::new(accounts_path);

/// Encrypts one account record for disk. Non-object entries pass through
/// untouched (indexing into them used to panic, which with panic=abort took
/// the whole app down over one malformed line in accounts.json).
fn seal_account<C: FieldCipher>(
    cipher: &C,
    a: &Value,
    undecryptable: &HashMap<String, String>,
    known: &HashMap<String, String>,
    sealed: &mut HashMap<String, String>,
) -> Result<(Value, bool), String> {
    let mut out = a.clone();
    let mut passthrough = false;
    let id = account_id(a).map(str::to_owned);
    if let Some(obj) = out.as_object_mut() {
        // Derived at load time from whether decryption worked; persisting it
        // made a cookie that later decrypted fine look dead forever.
        obj.remove("_cookieInvalid");
        let cookie = obj
            .get("cookie")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if cookie.is_empty() {
            if let Some(ct) = id.as_deref().and_then(|id| undecryptable.get(id)) {
                obj.insert("cookie".into(), Value::String(ct.clone()));
            }
        } else if is_encrypted(&cookie) {
            passthrough = true;
        } else {
            let ct = match known.get(&cookie) {
                Some(ct) => ct.clone(),
                None => cipher.seal(&cookie)?,
            };
            sealed.insert(cookie, ct.clone());
            obj.insert("cookie".into(), Value::String(ct));
        }
        obj.insert("_enc".into(), Value::Bool(true));
    }
    Ok((out, passthrough))
}

fn write_json_array(path: &Path, v: &[Value]) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(v).unwrap_or_else(|_| "[]".into());
    jsonfile::write_atomic(path, &json)
}

impl AccountStore {
    pub const fn new(path: fn() -> PathBuf) -> Self {
        Self {
            path,
            slot: Mutex::new(None),
        }
    }

    fn read<C: FieldCipher>(&self, cipher: &C, epoch: u64) -> Decrypted {
        let raw = jsonfile::read_array(&(self.path)());
        let mut d = Decrypted {
            epoch,
            list: Vec::with_capacity(raw.len()),
            undecryptable: HashMap::new(),
            sealed: HashMap::new(),
            stale: false,
        };
        for mut a in raw {
            let id = account_id(&a).map(str::to_owned);
            if let Some(obj) = a.as_object_mut() {
                let ct = obj
                    .get("cookie")
                    .and_then(Value::as_str)
                    .filter(|c| !c.is_empty())
                    .map(str::to_owned);
                if let Some(ct) = ct {
                    match cipher.open(&ct) {
                        Some(plain) => {
                            if is_encrypted(&ct) && !plain.is_empty() {
                                d.sealed.insert(plain.clone(), ct);
                            }
                            obj.remove("_cookieInvalid");
                            obj.insert("cookie".into(), Value::String(plain));
                        }
                        // Decrypt failure means the cookie is unusable; flag
                        // it immediately instead of waiting on the renderer's
                        // async validation to find out.
                        None => {
                            if let Some(id) = id {
                                d.undecryptable.insert(id, ct);
                            }
                            obj.insert("_cookieInvalid".into(), Value::Bool(true));
                            obj.insert("cookie".into(), Value::String(String::new()));
                        }
                    }
                }
            }
            d.list.push(a);
        }
        d
    }

    /// Runs `f` against the decrypted accounts, loading them first if the
    /// cache is empty or from an older key epoch. The lock is held for the
    /// duration, so `f` must not call back into the store.
    fn with<C: FieldCipher, R>(&self, cipher: &C, f: impl FnOnce(&mut Decrypted) -> R) -> R {
        let mut slot = lock(&self.slot);
        let epoch = KEY_EPOCH.load(Ordering::SeqCst);
        if slot.as_ref().is_none_or(|d| d.epoch != epoch || d.stale) {
            *slot = None;
            let fresh = self.read(cipher, epoch);
            // A file that exists but didn't parse reads as empty. Serve that
            // without caching it, so fixing the file needs no restart and the
            // refusal in persist() keeps re-checking the real state.
            if jsonfile::is_unreadable(&(self.path)()) {
                let mut fresh = fresh;
                return f(&mut fresh);
            }
            *slot = Some(fresh);
        }
        let d = slot.get_or_insert_with(|| self.read(cipher, epoch));
        f(d)
    }

    pub fn load<C: FieldCipher>(&self, cipher: &C) -> Vec<Value> {
        self.with(cipher, |d| d.list.clone())
    }

    pub fn find<C: FieldCipher>(&self, cipher: &C, id: &str) -> Option<Value> {
        self.with(cipher, |d| {
            d.list.iter().find(|a| account_id(a) == Some(id)).cloned()
        })
    }

    /// Read-modify-write of the account list under the store's lock. `f`
    /// works on a copy; nothing is written if it leaves the list unchanged,
    /// and on a failed write memory keeps matching the disk.
    pub fn update<C: FieldCipher, R>(
        &self,
        cipher: &C,
        f: impl FnOnce(&mut Vec<Value>) -> R,
    ) -> Result<R, String> {
        self.with(cipher, |d| {
            let mut next = d.list.clone();
            let result = f(&mut next);
            if next != d.list {
                self.persist(cipher, d, next)?;
            }
            Ok(result)
        })
    }

    fn persist<C: FieldCipher>(
        &self,
        cipher: &C,
        d: &mut Decrypted,
        mut next: Vec<Value>,
    ) -> Result<(), String> {
        // The file is there but didn't parse, so what we just loaded is an
        // empty list rather than the real accounts. Writing that back would
        // destroy the only copy; refuse, and point at the backup instead.
        let path = (self.path)();
        if jsonfile::is_unreadable(&path) {
            return Err(ACCOUNTS_UNREADABLE.into());
        }
        let mut sealed = HashMap::with_capacity(next.len());
        let mut out = Vec::with_capacity(next.len());
        let mut passthrough = false;
        for a in &next {
            let (sealed_a, pt) = seal_account(cipher, a, &d.undecryptable, &d.sealed, &mut sealed)?;
            passthrough |= pt;
            out.push(sealed_a);
        }
        write_json_array(&path, &out).map_err(|e| e.to_string())?;

        // Bring memory in line with what a fresh load of that file returns.
        d.undecryptable.retain(|id, _| {
            next.iter().any(|a| {
                account_id(a) == Some(id.as_str())
                    && str_field(a, "cookie").unwrap_or("").is_empty()
            })
        });
        for a in &mut next {
            let id = account_id(a).map(str::to_owned);
            if let Some(obj) = a.as_object_mut() {
                obj.insert("_enc".into(), Value::Bool(true));
                if id.is_some_and(|id| d.undecryptable.contains_key(&id)) {
                    obj.insert("_cookieInvalid".into(), Value::Bool(true));
                } else {
                    obj.remove("_cookieInvalid");
                }
            }
        }
        d.list = next;
        d.sealed = sealed;
        d.stale = passthrough;
        Ok(())
    }

    /// Re-encrypts every account cookie (and the generated-account history)
    /// under whatever key `switch` installs. Both stores stay locked from the
    /// first read to the last write, so no save can slip in under the old key
    /// halfway through. Refuses up front if any cookie doesn't decrypt under
    /// the current key, because re-encrypting it would turn "unreadable for
    /// now" into "gone". If the accounts can't be written, `switch`'s undo
    /// runs so the key on record still matches the ciphertext on disk.
    pub fn rekey<C: FieldCipher, U: FnOnce()>(
        &self,
        cipher: &C,
        history: &HistoryStore,
        switch: impl FnOnce() -> Result<U, String>,
    ) -> Result<(), String> {
        let mut slot = lock(&self.slot);
        let _history = lock(&history.file.lock);

        let path = (self.path)();
        let raw = jsonfile::read_array(&path);
        if jsonfile::is_unreadable(&path) {
            // Drop the cache too: it was built before this failed read, and
            // keeping it would keep every later save refused (the flag is only
            // cleared by a fresh read) until the app restarted.
            *slot = None;
            return Err(ACCOUNTS_UNREADABLE.into());
        }
        let mut plain = Vec::with_capacity(raw.len());
        for mut a in raw {
            if let Some(obj) = a.as_object_mut() {
                let ct = obj
                    .get("cookie")
                    .and_then(Value::as_str)
                    .filter(|c| !c.is_empty())
                    .map(str::to_owned);
                if let Some(ct) = ct {
                    match cipher.open(&ct).filter(|p| !p.is_empty()) {
                        Some(p) => {
                            obj.insert("cookie".into(), Value::String(p));
                        }
                        None => return Err("decrypt failed".into()),
                    }
                }
            }
            plain.push(a);
        }

        let history_path = (history.file.path)();
        let history_raw = jsonfile::read_array(&history_path);
        let history_ok = !jsonfile::is_unreadable(&history_path);
        // Entries that don't decrypt keep their ciphertext as-is: unreadable
        // under the new key too, but not destroyed.
        let history_plain: Vec<Value> = history_raw
            .into_iter()
            .map(|mut e| {
                for field in HISTORY_SECRET_FIELDS {
                    let opened = str_field(&e, field)
                        .filter(|v| !v.is_empty())
                        .and_then(|v| cipher.open(v));
                    if let (Some(p), Some(obj)) = (opened, e.as_object_mut()) {
                        obj.insert(field.into(), Value::String(p));
                    }
                }
                e
            })
            .collect();

        let undo = switch()?;
        // The memo maps plaintext to old-key ciphertext; reusing it now would
        // write cookies the new key can't read.
        *slot = None;

        let none = HashMap::new();
        let mut scratch = HashMap::new();
        let sealed: Result<Vec<Value>, String> = plain
            .iter()
            .map(|a| seal_account(cipher, a, &none, &none, &mut scratch).map(|(v, _)| v))
            .collect();
        let written =
            sealed.and_then(|out| write_json_array(&path, &out).map_err(|e| e.to_string()));
        if let Err(e) = written {
            undo();
            return Err(e);
        }

        if history_ok {
            match seal_history(cipher, history_plain) {
                Ok(out) => {
                    if let Err(e) = write_json_array(&history_path, &out) {
                        eprintln!("[storage] genhistory re-encryption failed: {e}");
                    }
                }
                Err(e) => eprintln!("[storage] genhistory re-encryption failed: {e}"),
            }
        }
        Ok(())
    }

    /// Drops the cached plaintext. Holds the store's lock while `f` runs, so
    /// no save lands in the middle of it.
    fn reset_with<T>(&self, f: impl FnOnce() -> T) -> T {
        let mut slot = lock(&self.slot);
        let out = f();
        *slot = None;
        out
    }
}

/// One JSON-array file with its writes serialised.
pub struct ListFile {
    path: fn() -> PathBuf,
    lock: Mutex<()>,
}

impl ListFile {
    pub const fn new(path: fn() -> PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn read(&self) -> Vec<Value> {
        let _g = lock(&self.lock);
        jsonfile::read_array(&(self.path)())
    }

    pub fn write(&self, v: &[Value]) -> Result<(), String> {
        let _g = lock(&self.lock);
        write_json_array(&(self.path)(), v).map_err(|e| e.to_string())
    }
}

pub static PACKAGES: ListFile = ListFile::new(packages_path);

const HISTORY_SECRET_FIELDS: [&str; 2] = ["password", "cookie"];
const HISTORY_CAP: usize = 500;

fn seal_history<C: FieldCipher>(cipher: &C, list: Vec<Value>) -> Result<Vec<Value>, String> {
    let mut out = Vec::with_capacity(list.len().min(HISTORY_CAP));
    for mut e in list.into_iter().take(HISTORY_CAP) {
        for field in HISTORY_SECRET_FIELDS {
            let plain = str_field(&e, field)
                .filter(|v| !v.is_empty() && !is_encrypted(v))
                .map(str::to_owned);
            if let (Some(plain), Some(obj)) = (plain, e.as_object_mut()) {
                obj.insert(field.into(), Value::String(cipher.seal(&plain)?));
            }
        }
        out.push(e);
    }
    Ok(out)
}

/// Generated-account history: the password and cookie of each entry are
/// encrypted at rest, the same way account cookies are.
pub struct HistoryStore {
    file: ListFile,
}

pub static GENHISTORY: HistoryStore = HistoryStore {
    file: ListFile::new(genhistory_path),
};

impl HistoryStore {
    pub fn read<C: FieldCipher>(&self, cipher: &C) -> Vec<Value> {
        self.file
            .read()
            .into_iter()
            .map(|mut e| {
                for field in HISTORY_SECRET_FIELDS {
                    let opened = str_field(&e, field)
                        .filter(|v| !v.is_empty())
                        .map(|v| cipher.open(v).unwrap_or_default());
                    if let (Some(p), Some(obj)) = (opened, e.as_object_mut()) {
                        obj.insert(field.into(), Value::String(p));
                    }
                }
                e
            })
            .collect()
    }

    pub fn write<C: FieldCipher>(&self, cipher: &C, list: Vec<Value>) -> Result<(), String> {
        // Encrypted before taking the file lock; DPAPI on 500 entries is the
        // slow part and doesn't need to block a concurrent read.
        let out = seal_history(cipher, list)?;
        self.file.write(&out)
    }

    pub fn clear(&self) -> Result<(), String> {
        self.file.write(&[])
    }
}

// ---- convenience wrappers over the app's stores ----

pub fn load_accounts(state: &AppState) -> Vec<Value> {
    ACCOUNTS.load(state)
}

pub fn account_info(state: &AppState, id: &str) -> Option<AccountInfo> {
    ACCOUNTS
        .find(state, id)
        .map(|a| AccountInfo::from_value(&a))
}

pub fn update_accounts<R>(
    state: &AppState,
    f: impl FnOnce(&mut Vec<Value>) -> R,
) -> Result<R, String> {
    ACCOUNTS.update(state, f)
}

// One-time upgrade of legacy (gcm:/cbc:) cookies to DPAPI storage. Skipped
// entirely in passphrase mode, and aborts without writing if any cookie
// fails to decrypt.
pub fn migrate_account_encryption_to_keychain(state: &AppState) {
    if crate::encryption::passphrase_mode() {
        return;
    }
    let needs = jsonfile::read_array(&accounts_path()).iter().any(|a| {
        str_field(a, "cookie").is_some_and(|c| c.starts_with("gcm:") || c.starts_with("cbc:"))
    });
    if !needs {
        return;
    }
    match ACCOUNTS.rekey(state, &GENHISTORY, || Ok(|| {})) {
        Ok(()) => println!("[migrate] upgraded account encryption to DPAPI"),
        Err(e) => eprintln!("[migrate] {e}; leaving accounts untouched"),
    }
}

/// Deletes the app-data folder with every store locked, then resets all
/// in-memory copies so nothing written before the wipe can resurface.
pub fn wipe_app_data(dir: &Path) -> std::io::Result<()> {
    ACCOUNTS.reset_with(|| {
        let _history = lock(&GENHISTORY.file.lock);
        let _packages = lock(&PACKAGES.lock);
        let out = crate::settings::reset_with(|| std::fs::remove_dir_all(dir));
        jsonfile::forget_unreadable();
        invalidate_decrypted();
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// "enc:<plain>" with a key tag, so tests can tell which key sealed what.
    struct Fake {
        key: Cell<char>,
        seals: Cell<usize>,
    }
    impl Fake {
        fn new() -> Self {
            Self {
                key: Cell::new('a'),
                seals: Cell::new(0),
            }
        }
    }
    impl FieldCipher for Fake {
        fn seal(&self, p: &str) -> Result<String, String> {
            self.seals.set(self.seals.get() + 1);
            // "gs:" so is_encrypted recognises it.
            Ok(format!("gs:{}:{}", self.key.get(), p))
        }
        fn open(&self, c: &str) -> Option<String> {
            let rest = c.strip_prefix("gs:")?;
            let (k, p) = rest.split_once(':')?;
            (k.chars().next()? == self.key.get()).then(|| p.to_string())
        }
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join("mr-storage-tests").join(format!(
            "{}-{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn disk(path: &Path) -> Vec<Value> {
        jsonfile::read_array(path)
    }

    macro_rules! store {
        ($name:ident, $file:literal) => {{
            fn path() -> PathBuf {
                dir_cached(stringify!($name)).join($file)
            }
            AccountStore::new(path)
        }};
    }

    // The path fn can't capture, so the directory is created once per name.
    fn dir_cached(name: &str) -> PathBuf {
        static MADE: Mutex<Option<HashMap<String, PathBuf>>> = Mutex::new(None);
        let mut m = lock(&MADE);
        m.get_or_insert_with(HashMap::new)
            .entry(name.to_string())
            .or_insert_with(|| dir(name))
            .clone()
    }

    #[test]
    fn round_trip_encrypts_on_disk_and_decrypts_in_memory() {
        let s = store!(roundtrip, "accounts.json");
        let c = Fake::new();
        s.update(&c, |l| {
            l.push(serde_json::json!({"id":"1","cookie":"secret"}))
        })
        .unwrap();
        let on_disk = disk(&(s.path)());
        assert_eq!(on_disk[0]["cookie"], "gs:a:secret");
        assert_eq!(on_disk[0]["_enc"], true);
        assert_eq!(s.load(&c)[0]["cookie"], "secret");
    }

    #[test]
    fn unchanged_cookies_are_not_re_encrypted() {
        let s = store!(memo, "accounts.json");
        let c = Fake::new();
        s.update(&c, |l| {
            l.push(serde_json::json!({"id":"1","cookie":"x"}));
            l.push(serde_json::json!({"id":"2","cookie":"y"}));
        })
        .unwrap();
        assert_eq!(c.seals.get(), 2);
        s.update(&c, |l| l[0]["note"] = "hi".into()).unwrap();
        assert_eq!(c.seals.get(), 2, "only changed cookies should be sealed");
        // No-op updates don't touch the disk at all.
        s.update(&c, |_| ()).unwrap();
        assert_eq!(c.seals.get(), 2);
    }

    #[test]
    fn undecryptable_cookie_survives_a_save() {
        let s = store!(keep_ct, "accounts.json");
        jsonfile::write_atomic(
            &(s.path)(),
            r#"[{"id":"1","cookie":"gs:z:old"},{"id":"2","cookie":"gs:a:ok"}]"#,
        )
        .unwrap();
        let c = Fake::new();
        let loaded = s.load(&c);
        assert_eq!(loaded[0]["cookie"], "");
        assert_eq!(loaded[0]["_cookieInvalid"], true);
        assert!(loaded[1].get("_cookieInvalid").is_none());

        s.update(&c, |l| l[1]["note"] = "edited".into()).unwrap();
        let on_disk = disk(&(s.path)());
        assert_eq!(
            on_disk[0]["cookie"], "gs:z:old",
            "ciphertext must not be replaced by \"\""
        );
        assert!(
            on_disk[0].get("_cookieInvalid").is_none(),
            "derived flag is not persisted"
        );
        assert_eq!(s.load(&c)[0]["_cookieInvalid"], true);

        // A new cookie replaces it and clears the flag.
        s.update(&c, |l| l[0]["cookie"] = "fresh".into()).unwrap();
        assert_eq!(disk(&(s.path)())[0]["cookie"], "gs:a:fresh");
        assert!(s.load(&c)[0].get("_cookieInvalid").is_none());
    }

    #[test]
    fn corrupt_file_blocks_writes() {
        let s = store!(corrupt, "accounts.json");
        std::fs::write((s.path)(), "[{\"id\":").unwrap();
        let c = Fake::new();
        assert!(s.load(&c).is_empty());
        let err = s
            .update(&c, |l| l.push(serde_json::json!({"id":"n"})))
            .unwrap_err();
        assert!(err.contains("could not be read"));
        assert_eq!(std::fs::read_to_string((s.path)()).unwrap(), "[{\"id\":");
    }

    #[test]
    fn non_object_entries_do_not_panic() {
        let s = store!(junk, "accounts.json");
        jsonfile::write_atomic(&(s.path)(), r#"["junk", 3, {"id":"1","cookie":"gs:a:c"}]"#)
            .unwrap();
        let c = Fake::new();
        s.update(&c, |l| l[2]["note"] = "x".into()).unwrap();
        let on_disk = disk(&(s.path)());
        assert_eq!(on_disk[0], "junk");
        assert_eq!(on_disk[2]["cookie"], "gs:a:c");
    }

    #[test]
    fn rekey_moves_accounts_and_history_to_the_new_key() {
        let s = store!(rekey, "accounts.json");
        fn hist_path() -> PathBuf {
            dir_cached("rekey").join("genhistory.json")
        }
        let h = HistoryStore {
            file: ListFile::new(hist_path),
        };
        let c = Fake::new();
        s.update(&c, |l| l.push(serde_json::json!({"id":"1","cookie":"ck"})))
            .unwrap();
        h.write(&c, vec![serde_json::json!({"password":"pw","cookie":""})])
            .unwrap();
        s.load(&c); // warm the memo with key-a ciphertext

        s.rekey(&c, &h, || {
            c.key.set('b');
            Ok(|| {})
        })
        .unwrap();
        assert_eq!(disk(&(s.path)())[0]["cookie"], "gs:b:ck");
        assert_eq!(disk(&hist_path())[0]["password"], "gs:b:pw");
        assert_eq!(s.load(&c)[0]["cookie"], "ck");
    }

    #[test]
    fn rekey_refuses_when_a_cookie_does_not_decrypt() {
        let s = store!(rekey_refuse, "accounts.json");
        fn hist_path() -> PathBuf {
            dir_cached("rekey_refuse").join("genhistory.json")
        }
        let h = HistoryStore {
            file: ListFile::new(hist_path),
        };
        jsonfile::write_atomic(&(s.path)(), r#"[{"id":"1","cookie":"gs:z:x"}]"#).unwrap();
        let c = Fake::new();
        let switched = Cell::new(false);
        let r = s.rekey(&c, &h, || {
            switched.set(true);
            Ok(|| {})
        });
        assert_eq!(r.unwrap_err(), "decrypt failed");
        assert!(
            !switched.get(),
            "the key must not change when the data can't follow"
        );
    }
}
