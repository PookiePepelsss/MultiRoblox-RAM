// settings.json, held in memory.
//
// Settings are read on hot paths (passphrase_mode runs on every encrypt and
// decrypt, every launch, every watch tick that closes an account), so the
// file is parsed once and served from a cached snapshot afterwards. Every
// change goes through `update`, which serialises read-modify-write under one
// lock and writes through to disk before the new snapshot becomes visible:
// two settings saves racing each other can no longer drop one another's keys.
//
// Unparseable settings are backed up rather than silently ignored: dropping
// keyVerifier/kdfSalt makes the app forget a passphrase was ever set and offer
// to create a new one, which rotates the salt and orphans every gs:-encrypted
// cookie. Writes stay allowed here (unlike accounts); blocking them would
// leave enc_set_key re-encrypting accounts under a key whose verifier never
// reached disk, which is a worse outcome than a reset config.
use crate::paths::settings_path;
use serde_json::{Map, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

pub type Settings = Map<String, Value>;

struct Store {
    path: fn() -> PathBuf,
    cache: RwLock<Option<Arc<Settings>>>,
    /// Held for the whole read-modify-write of an update, so concurrent
    /// updates apply one after another instead of each starting from the
    /// same stale snapshot. Readers never take it, so a slow disk write
    /// doesn't stall the hot read path.
    write: Mutex<()>,
}

static STORE: Store = Store::new(settings_path);

impl Store {
    const fn new(path: fn() -> PathBuf) -> Self {
        Self {
            path,
            cache: RwLock::new(None),
            write: Mutex::new(()),
        }
    }

    fn write_lock(&self) -> MutexGuard<'_, ()> {
        self.write.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn snapshot(&self) -> Arc<Settings> {
        if let Some(s) = self
            .cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            return s.clone();
        }
        let path = (self.path)();
        let loaded = Arc::new(crate::jsonfile::read_object(&path));
        // A file that failed to read is served as empty but not cached, so
        // the next call tries again instead of pinning an empty config for
        // the rest of the session.
        if crate::jsonfile::is_unreadable(&path) {
            return loaded;
        }
        let mut cache = self.cache.write().unwrap_or_else(PoisonError::into_inner);
        // Only fill an empty slot: an update may have landed while this read
        // was in flight, and its snapshot is newer than what we just parsed.
        cache.get_or_insert(loaded).clone()
    }

    fn update<R>(&self, f: impl FnOnce(&mut Settings) -> R) -> Result<R, String> {
        let _w = self.write_lock();
        let current = self.snapshot();
        // The file exists but couldn't even be opened (antivirus or a sync
        // client holding it): `current` is an empty stand-in, and writing it
        // back with one key changed would wipe keyVerifier and kdfSalt, which
        // makes every passphrase-encrypted cookie unrecoverable. A corrupt
        // but readable file is different: it is backed up, and saving over
        // it stays allowed (see load comment above).
        if crate::jsonfile::read_failed(&(self.path)()) {
            return Err("settings.json could not be read right now, so nothing was saved over it. Try again in a moment.".into());
        }
        let mut next = (*current).clone();
        let result = f(&mut next);
        if next == *current {
            return Ok(result);
        }
        let json = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
        crate::jsonfile::write_atomic(&(self.path)(), &json)
            .map_err(|e| format!("could not save settings: {e}"))?;
        // Published only after the write succeeded, so memory never claims a
        // state that a restart would not come back to.
        *self.cache.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(next));
        Ok(result)
    }

    fn reset_with<T>(&self, wipe: impl FnOnce() -> T) -> T {
        let _w = self.write_lock();
        let out = wipe();
        *self.cache.write().unwrap_or_else(PoisonError::into_inner) = None;
        out
    }
}

/// The current settings. Cheap: a refcount bump, not a parse.
pub fn snapshot() -> Arc<Settings> {
    STORE.snapshot()
}

/// Applies `f` to the settings and writes the result through to disk, all
/// under one lock. `f` must not call `update` itself (it would deadlock);
/// reading via `snapshot` inside it is fine. Nothing is written when `f`
/// leaves the settings unchanged. On a failed write the in-memory settings
/// stay as they were, matching what is on disk.
pub fn update<R>(f: impl FnOnce(&mut Settings) -> R) -> Result<R, String> {
    STORE.update(f)
}

/// Runs `wipe` (which deletes the app-data folder) with updates blocked,
/// then drops the cache so the next read starts from the now-empty disk.
pub fn reset_with<T>(wipe: impl FnOnce() -> T) -> T {
    STORE.reset_with(wipe)
}

pub fn get_bool(key: &str, default: bool) -> bool {
    snapshot()
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

pub fn get_str(key: &str) -> Option<String> {
    snapshot()
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub fn get_i64(key: &str) -> Option<i64> {
    snapshot().get(key).and_then(Value::as_i64)
}

pub fn get_f64(key: &str) -> Option<f64> {
    snapshot().get(key).and_then(Value::as_f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path() -> PathBuf {
        std::env::temp_dir()
            .join("mr-settings-tests")
            .join(format!("{}", std::process::id()))
            .join("settings.json")
    }

    // One test, run in sequence: the store is keyed on a fixed path.
    #[test]
    fn caches_writes_through_and_resets() {
        let store = Store::new(test_path);
        let _ = std::fs::remove_dir_all(test_path().parent().unwrap());

        assert!(store.snapshot().is_empty(), "missing file reads as empty");

        store
            .update(|s| s.insert("a".into(), Value::Bool(true)))
            .unwrap();
        let on_disk = crate::jsonfile::read_object(&test_path());
        assert_eq!(on_disk.get("a"), Some(&Value::Bool(true)), "write-through");

        // Served from memory: an edit behind the cache's back is not seen.
        crate::jsonfile::write_atomic(&test_path(), r#"{"a":false}"#).unwrap();
        assert_eq!(store.snapshot().get("a"), Some(&Value::Bool(true)));

        // Updates build on the cached state and keep other keys.
        store
            .update(|s| s.insert("b".into(), Value::from(2)))
            .unwrap();
        let s = store.snapshot();
        assert_eq!(s.get("a"), Some(&Value::Bool(true)));
        assert_eq!(s.get("b"), Some(&Value::from(2)));

        // A reset re-reads the disk.
        store.reset_with(|| std::fs::remove_file(test_path()).unwrap());
        assert!(store.snapshot().is_empty());
    }

    #[test]
    fn concurrent_updates_do_not_lose_keys() {
        fn path() -> PathBuf {
            std::env::temp_dir()
                .join("mr-settings-tests")
                .join(format!("{}-concurrent", std::process::id()))
                .join("settings.json")
        }
        static S: Store = Store::new(path);
        let _ = std::fs::remove_dir_all(path().parent().unwrap());
        let threads: Vec<_> = (0..8)
            .map(|i| {
                std::thread::spawn(move || {
                    S.update(|s| s.insert(format!("k{i}"), Value::from(i)))
                        .unwrap();
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let on_disk = crate::jsonfile::read_object(&path());
        assert_eq!(on_disk.len(), 8, "every update must survive: {on_disk:?}");
    }
}
