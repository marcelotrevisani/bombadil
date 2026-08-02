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
        let count = self.entries.lock().map(|e| e.len()).unwrap_or(0);
        write!(f, "InMemorySecretStore({count} secrets)")
    }
}

impl InMemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, key: &SecretKey) -> Result<Option<String>, SecretError> {
        Ok(self.entries.lock().unwrap().get(&key.account()).cloned())
    }

    fn set(&self, key: &SecretKey, value: &str) -> Result<(), SecretError> {
        self.entries
            .lock()
            .unwrap()
            .insert(key.account(), value.to_string());
        Ok(())
    }

    fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        self.entries.lock().unwrap().remove(&key.account());
        Ok(())
    }

    fn is_available(&self) -> bool {
        true
    }
}
