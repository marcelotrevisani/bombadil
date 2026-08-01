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
                // Stamp the current version rather than trusting whatever was
                // loaded: a v1 file re-saved after a v2 build gains v2-shaped
                // data, and must not go back out still labelled v1, or an
                // older build would parse and silently truncate it. Clone so
                // the caller's `&Config` is never mutated.
                let config = Config {
                    schema_version: CURRENT_SCHEMA_VERSION,
                    ..config.clone()
                };
                let text = toml::to_string_pretty(&config)
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
    use crate::model::{
        CURRENT_SCHEMA_VERSION, Config, DefaultVenvLocation, EnvVar, Index, IndexAuth, IndexKind,
        Project, Script, Settings, UvSource, VenvLocation,
    };
    use std::path::PathBuf;
    use uuid::Uuid;

    /// Hand-authored, not serde-generated (see the file's own header comment):
    /// a generated fixture would just re-encode whatever bug is already in the
    /// serializer. Exercises every field of `Config`, including a `Central`
    /// default venv location, a `Custom` uv source, one `Default` and one
    /// `Extra` index, `Basic` and `None` auth, global and per-project env vars
    /// and scripts, and a project with a `Custom` venv location.
    const GOLDEN_FIXTURE: &str = include_str!("../../tests/fixtures/full.toml");

    #[test]
    fn deserializes_the_golden_fixture_field_by_field() {
        let got: Config = toml::from_str(GOLDEN_FIXTURE).expect("golden fixture must parse");

        let expected = Config {
            schema_version: 1,
            settings: Settings {
                uv_source: UvSource::Custom {
                    path: PathBuf::from("/opt/uv/bin/uv"),
                },
                default_venv_location: DefaultVenvLocation::Central {
                    path: PathBuf::from("/home/t/.venvs"),
                },
            },
            indexes: vec![
                Index {
                    name: "priv".into(),
                    url: "https://nexus.corp/simple".into(),
                    kind: IndexKind::Default,
                    default_for_new_projects: true,
                    auth: IndexAuth::Basic {
                        username: "alice".into(),
                    },
                },
                Index {
                    name: "extra".into(),
                    url: "https://pypi.org/simple".into(),
                    kind: IndexKind::Extra,
                    default_for_new_projects: false,
                    auth: IndexAuth::None,
                },
            ],
            global_env: vec![EnvVar {
                key: "HTTP_PROXY".into(),
                value: "http://proxy:8080".into(),
                secret: false,
            }],
            global_scripts: vec![Script {
                path: PathBuf::from("/etc/bombadil/pre.sh"),
                args: vec!["--quiet".into()],
                enabled: true,
            }],
            projects: vec![Project {
                id: Uuid::nil(),
                label: "my-api".into(),
                pyproject_path: PathBuf::from("/proj/my-api/pyproject.toml"),
                index_names: vec!["priv".into(), "extra".into()],
                venv: VenvLocation::Custom {
                    path: PathBuf::from("/mnt/envs/my-api"),
                },
                env: vec![EnvVar {
                    key: "DATABASE_PASSWORD".into(),
                    value: String::new(),
                    secret: true,
                }],
                scripts: vec![Script {
                    path: PathBuf::from("./scripts/decrypt.sh"),
                    args: vec![],
                    enabled: false,
                }],
            }],
        };

        // Field-by-field, not just `assert_eq!(got, expected)` on the whole
        // struct: a whole-struct assertion failure only says "not equal" and
        // hides which field renamed or changed shape.
        assert_eq!(got.schema_version, expected.schema_version);
        assert_eq!(got.settings, expected.settings);
        assert_eq!(got.indexes, expected.indexes);
        assert_eq!(got.global_env, expected.global_env);
        assert_eq!(got.global_scripts, expected.global_scripts);
        assert_eq!(got.projects, expected.projects);
        assert_eq!(got, expected);
    }

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
    fn save_always_stamps_the_current_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_config_path(&dir);
        // A file already on disk claiming an old version number, distinct
        // from whatever CURRENT_SCHEMA_VERSION happens to be today.
        std::fs::write(&path, "schema_version = 1\n").unwrap();

        let (store, loaded) = Store::open(path.clone());
        let config = loaded.config().clone();
        store.save(&config).unwrap();

        let (_, reloaded) = Store::open(path);
        assert_eq!(reloaded.config().schema_version, CURRENT_SCHEMA_VERSION);
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
