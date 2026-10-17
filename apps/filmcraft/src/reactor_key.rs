//! OS credential storage with a zeroizing cache shared by desktop workers for one app session.
//! No key or cache is exposed through UI state, preferences, or project files.

use filmcraft_ui_egui::ReactorKeyAction;
use std::sync::{
    Arc, Mutex,
    mpsc::{Receiver, channel},
};
use zeroize::Zeroizing;

const SERVICE: &str = "ai.storyteller.filmcraft.reactor";
const ACCOUNT: &str = "api-key";

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| format!("Could not open credential store: {e}"))
}

trait Store: Send + Sync {
    fn read(&self) -> Result<Option<Zeroizing<String>>, String>;
    fn save(&self, key: &str) -> Result<(), String>;
    fn remove(&self) -> Result<(), String>;
}

struct OsStore;
impl Store for OsStore {
    fn read(&self) -> Result<Option<Zeroizing<String>>, String> {
        match entry()?.get_password() {
            Ok(key) => Ok(Some(Zeroizing::new(key))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(format!("Could not read Reactor API key: {error}")),
        }
    }
    fn save(&self, key: &str) -> Result<(), String> {
        entry()?.set_password(key).map_err(|error| format!("Could not save Reactor API key: {error}"))
    }
    fn remove(&self) -> Result<(), String> {
        match entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(format!("Could not remove Reactor API key: {error}")),
        }
    }
}

/// Serialize store operations with cache updates so a delayed read cannot restore a stale key.
/// Replacement, removal, and dropping the vault zeroize its cached key.
pub struct Vault {
    cached: Mutex<Option<Zeroizing<String>>>,
    store: Arc<dyn Store>,
}
impl Default for Vault {
    fn default() -> Self {
        Self { cached: Mutex::new(None), store: Arc::new(OsStore) }
    }
}
impl Vault {
    pub(crate) fn read_key(&self) -> Result<Zeroizing<String>, String> {
        let mut cached = self.cached.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if cached.is_none() {
            *cached = self.store.read()?;
        }
        cached.as_ref().cloned().ok_or_else(|| "Add a Reactor API key in Settings first.".into())
    }
    fn apply(&self, action: ReactorKeyAction) -> Result<bool, String> {
        let mut cached = self.cached.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match action {
            ReactorKeyAction::Check => {
                if cached.is_none() {
                    *cached = self.store.read()?;
                }
                Ok(cached.is_some())
            }
            ReactorKeyAction::Save(key) => {
                let key = Zeroizing::new(key);
                if key.trim().is_empty() || key.len() > 4096 {
                    return Err("Enter a valid Reactor API key.".into());
                }
                self.store.save(&key)?;
                *cached = Some(key);
                Ok(true)
            }
            ReactorKeyAction::Remove => {
                *cached = None;
                self.store.remove()?;
                Ok(false)
            }
        }
    }
    pub fn request(self: &Arc<Self>, action: ReactorKeyAction) -> Receiver<Result<bool, String>> {
        let (tx, rx) = channel();
        let worker_tx = tx.clone();
        let vault = self.clone();
        let started = std::thread::Builder::new().name("reactor-credential-store".into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| vault.apply(action)))
                .unwrap_or_else(|_| Err("Credential store failed unexpectedly.".into()));
            let _ = worker_tx.send(result);
        });
        if let Err(error) = started {
            let _ = tx.send(Err(format!("Could not start credential-store worker: {error}")));
        }
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct FakeStore {
        value: Mutex<Option<Zeroizing<String>>>,
        reads: AtomicUsize,
        read_error: AtomicBool,
        save_error: AtomicBool,
    }
    impl Store for FakeStore {
        fn read(&self) -> Result<Option<Zeroizing<String>>, String> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            if self.read_error.load(Ordering::Relaxed) {
                return Err("authorization denied".into());
            }
            Ok(self.value.lock().unwrap().clone())
        }
        fn save(&self, key: &str) -> Result<(), String> {
            if self.save_error.load(Ordering::Relaxed) {
                return Err("store unavailable".into());
            }
            *self.value.lock().unwrap() = Some(Zeroizing::new(key.into()));
            Ok(())
        }
        fn remove(&self) -> Result<(), String> {
            *self.value.lock().unwrap() = None;
            Ok(())
        }
    }
    fn vault() -> (Vault, Arc<FakeStore>) {
        let store = Arc::new(FakeStore::default());
        (Vault { cached: Mutex::new(None), store: store.clone() }, store)
    }
    #[test]
    fn saving_avoids_store_reads_and_replacing_or_removing_invalidates_the_cache() {
        let (vault, store) = vault();
        vault.apply(ReactorKeyAction::Save("rk_fake_first".into())).unwrap();
        for _ in 0..3 {
            assert_eq!(vault.read_key().unwrap().as_str(), "rk_fake_first");
        }
        assert!(vault.apply(ReactorKeyAction::Check).unwrap());
        assert_eq!(store.reads.load(Ordering::Relaxed), 0);
        vault.apply(ReactorKeyAction::Save("rk_fake_replacement".into())).unwrap();
        assert_eq!(vault.read_key().unwrap().as_str(), "rk_fake_replacement");
        assert!(!vault.apply(ReactorKeyAction::Remove).unwrap());
        assert!(vault.cached.lock().unwrap().is_none());
        assert!(vault.read_key().is_err());
    }
    #[test]
    fn an_authorized_read_is_reused_until_the_app_session_ends() {
        let (vault, store) = vault();
        store.save("rk_fake_stored").unwrap();
        assert!(vault.apply(ReactorKeyAction::Check).unwrap());
        assert_eq!(vault.read_key().unwrap().as_str(), "rk_fake_stored");
        assert_eq!(vault.read_key().unwrap().as_str(), "rk_fake_stored");
        assert_eq!(store.reads.load(Ordering::Relaxed), 1);
        drop(vault);
        let restarted = Vault { cached: Mutex::new(None), store: store.clone() };
        restarted.read_key().unwrap();
        assert_eq!(store.reads.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn invalid_keys_do_not_replace_the_saved_credential() {
        let (vault, store) = vault();
        vault.apply(ReactorKeyAction::Save("rk_fake_good".into())).unwrap();
        for key in [String::new(), " ".into(), "k".repeat(4097)] {
            assert!(vault.apply(ReactorKeyAction::Save(key)).is_err());
        }
        assert_eq!(vault.read_key().unwrap().as_str(), "rk_fake_good");
        assert_eq!(store.reads.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn denied_reads_can_retry_and_failed_saves_leave_the_previous_key_usable() {
        let (vault, store) = vault();
        store.save("rk_fake_existing").unwrap();
        store.read_error.store(true, Ordering::Relaxed);
        assert!(vault.read_key().is_err());
        assert!(vault.cached.lock().unwrap().is_none());
        store.read_error.store(false, Ordering::Relaxed);
        assert_eq!(vault.read_key().unwrap().as_str(), "rk_fake_existing");
        assert_eq!(store.reads.load(Ordering::Relaxed), 2);
        store.save_error.store(true, Ordering::Relaxed);
        assert!(vault.apply(ReactorKeyAction::Save("rk_fake_rejected".into())).is_err());
        assert_eq!(vault.read_key().unwrap().as_str(), "rk_fake_existing");
    }
}
