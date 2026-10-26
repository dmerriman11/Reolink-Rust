//! Camera passwords live in the OS credential store (Windows Credential Manager, macOS Keychain, Secret Service on
//! Linux), not in devices.json. Each camera has a random `secret` id that names its entry.
//! A password the store won't take (e.g. no Secret Service running on Linux) stays in devices.json, so the camera
//! still connects; it moves to the store at the next save that works.

use crate::device::DeviceConfig;
use std::collections::HashMap;
use std::sync::Mutex;

const SERVICE: &str = "Reolink-rs";

/// Secret id -> the password last written to (or read from) the store, so saves only write what changed.
static STORED: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn entry(id: &str) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, id)
}

/// A new random id for a camera's store entry.
pub fn new_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    // RandomState is seeded from the OS RNG; two draws give 128 bits.
    let a = std::collections::hash_map::RandomState::new().build_hasher().finish();
    let b = std::collections::hash_map::RandomState::new().build_hasher().finish();
    format!("{a:016x}{b:016x}")
}

fn remember(id: &str, password: &str) {
    STORED.lock().unwrap().get_or_insert_default().insert(id.into(), password.into());
}

/// Fill in `cfg.password` from the store, giving the camera an id first if it has none. A password already in the
/// config (devices.json from before the store, or one the store refused) is kept, and moved by `for_disk`.
pub fn load(cfg: &mut DeviceConfig) {
    if cfg.secret.is_empty() {
        cfg.secret = new_id();
    }
    if !cfg.password.is_empty() {
        return;
    }
    match entry(&cfg.secret).and_then(|e| e.get_password()) {
        Ok(p) => {
            remember(&cfg.secret, &p);
            cfg.password = p;
        }
        Err(keyring::Error::NoEntry) => {} // no password set (factory default login)
        Err(e) => log::error!("{}: can't read its password from the credential store: {e}", cfg.name),
    }
}

/// `cfg` as written to devices.json: its password goes to the store and out of the file, unless the store refuses.
pub fn for_disk(cfg: &DeviceConfig) -> DeviceConfig {
    let mut out = cfg.clone();
    if cfg.password.is_empty() || cfg.secret.is_empty() {
        return out;
    }
    let stored = STORED.lock().unwrap().as_ref().and_then(|m| m.get(&cfg.secret).cloned());
    if stored.as_deref() != Some(cfg.password.as_str()) {
        if let Err(e) = entry(&cfg.secret).and_then(|e| e.set_password(&cfg.password)) {
            log::error!("{}: credential store refused its password ({e}); keeping it in devices.json", cfg.name);
            return out;
        }
        remember(&cfg.secret, &cfg.password);
    }
    out.password.clear();
    out
}

/// Remove a camera's password from the store (the camera is gone for good).
pub fn delete(id: &str) {
    if id.is_empty() {
        return;
    }
    STORED.lock().unwrap().get_or_insert_default().remove(id);
    match entry(id).and_then(|e| e.delete_credential()) {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(e) => log::error!("can't delete a password from the credential store: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uses the real credential store (Windows Credential Manager here) under a throwaway id.
    #[test]
    fn passwords_go_to_the_store_and_out_of_the_file() {
        let mut cfg = DeviceConfig { name: "Test".into(), password: "hunter2".into(), ..Default::default() };
        load(&mut cfg); // an old config: gets an id, keeps its password until saved
        assert_eq!(cfg.secret.len(), 32);
        assert_eq!(cfg.password, "hunter2");

        let disk = for_disk(&cfg);
        assert!(disk.password.is_empty(), "password left in devices.json");
        assert_eq!(disk.secret, cfg.secret);
        assert!(!serde_json::to_string(&disk).unwrap().contains("hunter2"));

        let mut again = DeviceConfig { password: String::new(), ..disk };
        STORED.lock().unwrap().take(); // a fresh start reads the store itself
        load(&mut again);
        assert_eq!(again.password, "hunter2");

        delete(&again.secret);
        let mut gone = DeviceConfig { password: String::new(), ..again };
        load(&mut gone);
        assert!(gone.password.is_empty());
    }
}
