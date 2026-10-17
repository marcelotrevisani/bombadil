mod keyring_store;
mod memory;

pub use keyring_store::KeyringSecretStore;
pub use memory::InMemorySecretStore;

use crate::error::SecretError;
use uuid::Uuid;

/// The keychain service every Bombadil secret is stored under.
pub const SERVICE: &str = "bombadil";

/// An account name never written to, used only to probe whether the keychain
/// answers at all.
pub const SECRET_PROBE_ACCOUNT: &str = "availability-probe";

/// Which secret, addressed the way spec section 5 names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretKey {
    /// Credentials for a named package index.
    Index { name: String },
    /// A secret environment variable belonging to one project.
    ProjectEnv { project_id: Uuid, key: String },
    /// A secret environment variable applying to every project.
    GlobalEnv { key: String },
    /// The personal access token used when `gh` is unavailable.
    GitHubToken,
}

impl SecretKey {
    /// The account name under [`SERVICE`].
    ///
    /// These strings are what actually lands in the OS keychain: changing one
    /// orphans every secret already stored under the old name, with no error
    /// and no way to find it again from inside the app.
    pub fn account(&self) -> String {
        match self {
            SecretKey::Index { name } => format!("index/{name}"),
            SecretKey::ProjectEnv { project_id, key } => format!("env/{project_id}/{key}"),
            SecretKey::GlobalEnv { key } => format!("env/global/{key}"),
            SecretKey::GitHubToken => "github/token".to_string(),
        }
    }
}

/// Where secret material lives.
///
/// Behind a trait so everything above it is testable without a keychain —
/// which matters because headless Linux and WSL frequently have no Secret
/// Service at all.
pub trait SecretStore: Send + Sync {
    /// `Ok(None)` means "not set", which is an ordinary state rather than a
    /// failure. An `Err` means the store itself could not be consulted.
    fn get(&self, key: &SecretKey) -> Result<Option<String>, SecretError>;

    fn set(&self, key: &SecretKey, value: &str) -> Result<(), SecretError>;

    /// Deleting a secret that is not there succeeds.
    fn delete(&self, key: &SecretKey) -> Result<(), SecretError>;

    /// Whether this store can be used at all. False on a machine with no
    /// usable keychain, which the caller surfaces rather than failing later at
    /// spawn time with something cryptic.
    fn is_available(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn the_github_token_has_a_fixed_account() {
        assert_eq!(SecretKey::GitHubToken.account(), "github/token");
    }

    #[test]
    fn account_names_match_the_three_documented_shapes() {
        // Spec section 5. These strings are what actually lands in the OS
        // keychain, so changing them orphans every stored secret.
        assert_eq!(
            SecretKey::Index {
                name: "priv".into()
            }
            .account(),
            "index/priv"
        );
        assert_eq!(
            SecretKey::GlobalEnv {
                key: "TOKEN".into()
            }
            .account(),
            "env/global/TOKEN"
        );
        assert_eq!(
            SecretKey::ProjectEnv {
                project_id: Uuid::nil(),
                key: "DATABASE_PASSWORD".into(),
            }
            .account(),
            "env/00000000-0000-0000-0000-000000000000/DATABASE_PASSWORD"
        );
    }

    #[test]
    fn a_missing_secret_is_none_not_an_error() {
        // "Not set yet" is an ordinary state, not a failure. Making callers
        // distinguish it from a broken keychain by matching errors guarantees
        // someone eventually gets it wrong.
        let store = InMemorySecretStore::new();
        let key = SecretKey::GlobalEnv {
            key: "ABSENT".into(),
        };
        assert_eq!(store.get(&key).unwrap(), None);
    }

    #[test]
    fn a_stored_secret_round_trips() {
        let store = InMemorySecretStore::new();
        let key = SecretKey::Index {
            name: "priv".into(),
        };
        store.set(&key, "hunter2").unwrap();
        assert_eq!(store.get(&key).unwrap().as_deref(), Some("hunter2"));
    }

    #[test]
    fn deleting_removes_it_and_deleting_again_is_not_an_error() {
        let store = InMemorySecretStore::new();
        let key = SecretKey::Index {
            name: "priv".into(),
        };
        store.set(&key, "hunter2").unwrap();
        store.delete(&key).unwrap();
        assert_eq!(store.get(&key).unwrap(), None);
        store
            .delete(&key)
            .expect("deleting an absent secret is a no-op");
    }

    #[test]
    fn different_projects_do_not_share_a_secret() {
        // The project id is in the account name precisely so two projects with
        // the same variable name keep separate values.
        let store = InMemorySecretStore::new();
        let a = SecretKey::ProjectEnv {
            project_id: Uuid::from_u128(1),
            key: "TOKEN".into(),
        };
        let b = SecretKey::ProjectEnv {
            project_id: Uuid::from_u128(2),
            key: "TOKEN".into(),
        };
        store.set(&a, "alpha").unwrap();
        store.set(&b, "beta").unwrap();
        assert_eq!(store.get(&a).unwrap().as_deref(), Some("alpha"));
        assert_eq!(store.get(&b).unwrap().as_deref(), Some("beta"));
    }

    #[test]
    fn the_in_memory_store_reports_itself_available() {
        assert!(InMemorySecretStore::new().is_available());
    }

    #[test]
    fn a_secret_value_never_appears_in_a_debug_rendering() {
        // Failure reports and logs render Debug. A secret leaking through it
        // would defeat the entire point of the keychain.
        let store = InMemorySecretStore::new();
        store
            .set(
                &SecretKey::Index {
                    name: "priv".into(),
                },
                "hunter2",
            )
            .unwrap();
        let rendered = format!("{store:?}");
        assert!(!rendered.contains("hunter2"), "got {rendered}");
    }
}
