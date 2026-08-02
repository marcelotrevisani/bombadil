use super::{SECRET_PROBE_ACCOUNT, SERVICE, SecretKey, SecretStore};
use crate::error::SecretError;

/// The real OS keychain: Keychain on macOS, Credential Manager on Windows,
/// Secret Service on Linux.
///
/// Verified against keyring 4.1.6, which needs no backend feature flags.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringSecretStore;

impl KeyringSecretStore {
    pub fn new() -> Self {
        Self
    }

    fn entry(key: &SecretKey) -> Result<keyring::Entry, SecretError> {
        let account = key.account();
        keyring::Entry::new(SERVICE, &account).map_err(|e| SecretError::Backend {
            account,
            message: e.to_string(),
        })
    }
}

impl SecretStore for KeyringSecretStore {
    fn get(&self, key: &SecretKey) -> Result<Option<String>, SecretError> {
        match Self::entry(key)?.get_password() {
            Ok(value) => Ok(Some(value)),
            // Not set yet is ordinary, not a failure.
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Backend {
                account: key.account(),
                message: e.to_string(),
            }),
        }
    }

    fn set(&self, key: &SecretKey, value: &str) -> Result<(), SecretError> {
        Self::entry(key)?
            .set_password(value)
            .map_err(|e| SecretError::Backend {
                account: key.account(),
                message: e.to_string(),
            })
    }

    fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Backend {
                account: key.account(),
                message: e.to_string(),
            }),
        }
    }

    /// Probe by asking for a namespaced account that is never written.
    ///
    /// A `NoEntry` answer proves the backend responded, which is what we care
    /// about; only a hard error means unusable. Headless Linux and WSL often
    /// have no Secret Service, and the caller offers session-only entry
    /// instead of failing later at spawn time with something cryptic.
    fn is_available(&self) -> bool {
        match keyring::Entry::new(SERVICE, SECRET_PROBE_ACCOUNT) {
            Ok(entry) => !matches!(
                entry.get_password(),
                Err(keyring::Error::PlatformFailure(_))
            ),
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_can_be_probed_without_panicking() {
        // The only unit test permitted against the real keychain: that asking
        // whether it exists is safe. CI machines have no keychain, and a
        // developer's real credential store must never be read or written by
        // `cargo test`. Round-trip coverage lives in the integration test.
        let store = KeyringSecretStore::new();
        let _: bool = store.is_available();
    }

    #[test]
    fn the_store_reveals_nothing_in_debug() {
        let rendered = format!("{:?}", KeyringSecretStore::new());
        assert!(!rendered.is_empty());
        assert!(
            !rendered.to_lowercase().contains("password"),
            "got {rendered}"
        );
    }
}
