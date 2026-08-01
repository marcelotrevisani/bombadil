pub mod atomic;
pub mod paths;

use crate::error::StoreError;
use crate::model::{CURRENT_SCHEMA_VERSION, Config};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Only enough of the file to decide whether we are allowed to understand it.
#[derive(Deserialize)]
struct VersionProbe {
    schema_version: u32,
}

/// The outcome of reading the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded {
    /// Parsed cleanly at a version we support.
    Ok(Config),
    /// No file existed yet; these are defaults.
    Fresh(Config),
    /// Written by a newer build. Usable for display, but must not be written back.
    ReadOnly { config: Config, found: u32 },
    /// Could not be parsed. Defaults are supplied and the file is left alone.
    Unreadable { config: Config, message: String },
}

impl Loaded {
    pub fn config(&self) -> &Config {
        match self {
            Loaded::Ok(c)
            | Loaded::Fresh(c)
            | Loaded::ReadOnly { config: c, .. }
            | Loaded::Unreadable { config: c, .. } => c,
        }
    }
}

/// Why a store may (or may not) be written to. Carries the specific cause so
/// `save` can report it truthfully instead of guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Writability {
    Writable,
    /// The file declares a schema version newer than this build supports.
    TooNew {
        found: u32,
    },
    /// The file could not be read or parsed at all.
    Unreadable {
        message: String,
    },
}

/// Owns the config file path and whether writing to it is permitted.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
    writability: Writability,
}

impl Store {
    fn new(path: PathBuf, writability: Writability) -> Self {
        Self { path, writability }
    }

    /// Read the file at `path`, returning both the store and what was found.
    ///
    /// Never fails: an unreadable or too-new file yields defaults plus a
    /// non-writable store, so the application can always start.
    pub fn open(path: PathBuf) -> (Self, Loaded) {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let store = Self::new(path, Writability::Writable);
                return (store, Loaded::Fresh(Config::default()));
            }
            Err(e) => {
                let message = e.to_string();
                let store = Self::new(
                    path,
                    Writability::Unreadable {
                        message: message.clone(),
                    },
                );
                return (
                    store,
                    Loaded::Unreadable {
                        config: Config::default(),
                        message,
                    },
                );
            }
        };

        // Check the version before trusting the rest of the file: a newer build
        // may have written fields whose meaning we would otherwise guess at.
        let found = match toml::from_str::<VersionProbe>(&text) {
            Ok(probe) => probe.schema_version,
            Err(e) => {
                let message = e.to_string();
                let store = Self::new(
                    path,
                    Writability::Unreadable {
                        message: message.clone(),
                    },
                );
                return (
                    store,
                    Loaded::Unreadable {
                        config: Config::default(),
                        message,
                    },
                );
            }
        };

        match toml::from_str::<Config>(&text) {
            Ok(config) if found > CURRENT_SCHEMA_VERSION => {
                let store = Self::new(path, Writability::TooNew { found });
                (store, Loaded::ReadOnly { config, found })
            }
            Ok(config) => {
                let store = Self::new(path, Writability::Writable);
                (store, Loaded::Ok(config))
            }
            // Too new to parse at all. The version already explains the
            // situation better than the parse error would, so it is discarded —
            // but the file still must not be overwritten.
            Err(_) if found > CURRENT_SCHEMA_VERSION => {
                let store = Self::new(path, Writability::TooNew { found });
                (
                    store,
                    Loaded::ReadOnly {
                        config: Config::default(),
                        found,
                    },
                )
            }
            Err(e) => {
                let message = e.to_string();
                let store = Self::new(
                    path,
                    Writability::Unreadable {
                        message: message.clone(),
                    },
                );
                (
                    store,
                    Loaded::Unreadable {
                        config: Config::default(),
                        message,
                    },
                )
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_writable(&self) -> bool {
        matches!(self.writability, Writability::Writable)
    }

    /// Persist `config`, or refuse if this store was opened on a file we must
    /// not modify.
    pub fn save(&self, config: &Config) -> Result<(), StoreError> {
        match &self.writability {
            Writability::Writable => {
                let text = toml::to_string_pretty(config)
                    .map_err(|source| StoreError::Serialize { source })?;
                atomic::write_atomic(&self.path, &text)
            }
            Writability::TooNew { found } => Err(StoreError::ReadOnly {
                path: self.path.clone(),
                found: *found,
                supported: CURRENT_SCHEMA_VERSION,
            }),
            Writability::Unreadable { message } => Err(StoreError::RefusedUnreadable {
                path: self.path.clone(),
                message: message.clone(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CURRENT_SCHEMA_VERSION, Config, DefaultVenvLocation, Settings};
    use std::path::PathBuf;

    fn temp_config_path(dir: &tempfile::TempDir) -> PathBuf {
        paths::config_file_in(dir.path())
    }

    #[test]
    fn a_missing_file_loads_as_fresh_defaults_and_is_writable() {
        let dir = tempfile::tempdir().unwrap();
        let (store, loaded) = Store::open(temp_config_path(&dir));

        assert!(matches!(loaded, Loaded::Fresh(_)));
        assert_eq!(loaded.config(), &Config::default());
        assert!(store.is_writable());
    }

    #[test]
    fn a_saved_config_loads_back_identically() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_config_path(&dir);

        let config = Config {
            settings: Settings {
                default_venv_location: DefaultVenvLocation::Central {
                    path: PathBuf::from("/home/t/.venvs"),
                },
                ..Settings::default()
            },
            ..Config::default()
        };

        let (store, _) = Store::open(path.clone());
        store.save(&config).unwrap();

        let (_, loaded) = Store::open(path);
        assert!(matches!(loaded, Loaded::Ok(_)));
        assert_eq!(loaded.config(), &config);
    }

    #[test]
    fn a_newer_schema_version_loads_read_only_and_refuses_to_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_config_path(&dir);
        let future = CURRENT_SCHEMA_VERSION + 7;
        std::fs::write(&path, format!("schema_version = {future}\n")).unwrap();

        let (store, loaded) = Store::open(path.clone());

        match loaded {
            Loaded::ReadOnly { found, .. } => assert_eq!(found, future),
            other => panic!("expected ReadOnly, got {other:?}"),
        }
        assert!(!store.is_writable());

        let err = store.save(&Config::default()).unwrap_err();
        match err {
            StoreError::ReadOnly { found, .. } => assert_eq!(found, future),
            other => panic!("expected ReadOnly, got {other:?}"),
        }

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("schema_version = {future}\n"),
            "a newer config must never be modified"
        );
    }

    #[test]
    fn an_unparseable_file_yields_defaults_and_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_config_path(&dir);
        let garbage = "this is not = = valid toml [[[\n";
        std::fs::write(&path, garbage).unwrap();

        let (store, loaded) = Store::open(path.clone());

        match &loaded {
            Loaded::Unreadable { message, .. } => assert!(!message.is_empty()),
            other => panic!("expected Unreadable, got {other:?}"),
        }
        assert_eq!(loaded.config(), &Config::default());
        assert!(!store.is_writable());

        let err = store.save(&Config::default()).unwrap_err();
        match err {
            StoreError::RefusedUnreadable { message, .. } => assert!(!message.is_empty()),
            other => panic!("expected RefusedUnreadable, got {other:?}"),
        }

        assert_eq!(std::fs::read_to_string(&path).unwrap(), garbage);
    }
}
