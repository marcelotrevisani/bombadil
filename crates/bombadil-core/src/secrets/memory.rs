use super::{SecretKey, SecretStore};
use crate::error::SecretError;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// An in-memory store, used by tests and as the session-only fallback when the
/// machine has no usable keychain.
///
/// `Debug` is hand-written to print only how many secrets are held: the
/// derived one would render values, and failure reports render `Debug`.
#[derive(Default)]
pub struct InMemorySecretStore {
    entries: Mutex<BTreeMap<String, String>>,
}

impl std::fmt::Debug for InMemorySecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.entries_guard().len();
        write!(f, "InMemorySecretStore({count} secrets)")
    }
}

impl InMemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get access to the entries map, recovering from poison if needed.
    ///
    /// A poisoned lock means another thread panicked while holding it. The
    /// map has no invariant a panic could leave half-applied, so taking the
    /// data back is sound — and far better than a store that panics for the
    /// rest of the session, since this is the fallback that must keep working
    /// when the machine has no keychain.
    fn entries_guard(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, String>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, key: &SecretKey) -> Result<Option<String>, SecretError> {
        Ok(self.entries_guard().get(&key.account()).cloned())
    }

    fn set(&self, key: &SecretKey, value: &str) -> Result<(), SecretError> {
        self.entries_guard()
            .insert(key.account(), value.to_string());
        Ok(())
    }

    fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        self.entries_guard().remove(&key.account());
        Ok(())
    }

    fn is_available(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn recovery_from_poisoned_lock() {
        let store = Arc::new(InMemorySecretStore::new());
        let key = SecretKey::GlobalEnv { key: "TEST".into() };
        store.set(&key, "value1").unwrap();

        // Spawn a thread that poisons the lock by panicking while holding it.
        let store_clone = Arc::clone(&store);
        let poison_thread = thread::spawn(move || {
            let _guard = store_clone.entries.lock().unwrap();
            panic!("intentional poison");
        });

        // Let the poison happen and join.
        let _ = poison_thread.join();

        // The store should recover and continue working even though the lock is
        // poisoned: the map itself has no invariant a panic could corrupt, so
        // taking the data back is safe.
        assert_eq!(store.get(&key).unwrap().as_deref(), Some("value1"));
        store.set(&key, "value2").unwrap();
        assert_eq!(store.get(&key).unwrap().as_deref(), Some("value2"));
    }
}
