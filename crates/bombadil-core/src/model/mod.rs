mod env_var;
mod index;
mod member;
mod project;
mod script;
mod settings;

pub use env_var::EnvVar;
pub use index::{Index, IndexAuth, IndexKind};
pub use member::{MemberOrigin, MemberRef, MemberSelection};
pub use project::{Project, VenvLocation};
pub use script::Script;
pub use settings::{DefaultVenvLocation, Settings, UvSource};

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Which terminal to open, per operating system.
///
/// Adjacently tagged, not internally tagged: `Detected` is a newtype variant
/// wrapping a unit enum, which serialises as a bare string, and serde's
/// internally-tagged representation cannot carry that. `#[serde(tag = "kind")]`
/// alone compiles and then fails at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum TerminalChoice {
    Detected(crate::terminal::TerminalKind),
    /// A command template with `{cwd}` and `{shell}` placeholders, for a
    /// terminal Bombadil does not recognise. Spec section 8.
    Custom {
        template: String,
    },
}

/// Bumped only when a change cannot be expressed by adding `#[serde(default)]`
/// fields. A config declaring a higher version opens read-only.
///
/// Same-version forward compatibility is NOT preserved: `Store::save`
/// serializes only the fields `Config` declares today, so an unknown field
/// written by a *different build at this same version* is silently dropped
/// on the next save, not retained. Two builds at version 1 with different
/// field sets will silently lose each other's settings on round-trip. Plan 2
/// must decide explicitly — retain unknown fields, or bump the version on
/// every field addition — rather than inherit this hole unexamined.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// The entire persisted state of the application.
///
/// Field order is not load-bearing for parsing — `toml` 1.1.4 reorders
/// scalars before tables automatically on serialization. `schema_version` is
/// declared first purely for human readability of the on-disk file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub schema_version: u32,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub indexes: Vec<Index>,
    #[serde(default)]
    pub global_env: Vec<EnvVar>,
    #[serde(default)]
    pub global_scripts: Vec<Script>,
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub terminal_defaults: BTreeMap<crate::terminal::Os, TerminalChoice>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            settings: Settings::default(),
            indexes: Vec::new(),
            global_env: Vec::new(),
            global_scripts: Vec::new(),
            projects: Vec::new(),
            terminal_defaults: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fully_populated() -> Config {
        Config {
            schema_version: CURRENT_SCHEMA_VERSION,
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
                // Every field named on purpose: this fixture exists to prove
                // a whole `Config` round-trips through TOML, and a
                // `..Project::default()` tail would let a field added later
                // slip past the very test that would have caught it.
                id: uuid::Uuid::nil(),
                members: MemberSelection::default(),
            }],
            terminal_defaults: BTreeMap::from([
                (
                    crate::terminal::Os::Linux,
                    TerminalChoice::Detected(crate::terminal::TerminalKind::Kitty),
                ),
                (
                    crate::terminal::Os::Windows,
                    TerminalChoice::Custom {
                        template: "myterm --cd {cwd} -e {shell}".into(),
                    },
                ),
            ]),
        }
    }

    #[test]
    fn fully_populated_config_round_trips_through_toml() {
        let original = fully_populated();
        let text = toml::to_string_pretty(&original).expect("serialize");
        let back: Config = toml::from_str(&text).expect("deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn default_config_round_trips() {
        let original = Config::default();
        let text = toml::to_string_pretty(&original).expect("serialize");
        let back: Config = toml::from_str(&text).expect("deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn a_minimal_file_fills_in_every_default() {
        let back: Config = toml::from_str("schema_version = 1\n").expect("deserialize");
        assert_eq!(back, Config::default());
    }

    #[test]
    fn a_project_written_before_the_optional_tables_existed_still_loads() {
        // The claim behind "no schema bump": a config written by an older build,
        // carrying none of the optional per-project tables, must still parse and
        // fill in defaults. Proving this on a bare MemberSelection would only
        // show the field type has a default, which is a weaker claim.
        let text = r#"
schema_version = 1

[[projects]]
id = "00000000-0000-0000-0000-000000000000"
label = "legacy"
pyproject_path = "/proj/legacy/pyproject.toml"

[projects.venv]
kind = "alongside"
"#;

        let config: Config = toml::from_str(text).expect("a pre-feature project must still parse");
        let project = &config.projects[0];

        assert_eq!(project.label, "legacy");
        assert_eq!(project.members, MemberSelection::default());
        assert!(
            project.members.all_discovered,
            "default selection takes everything"
        );
        assert!(project.index_names.is_empty());
        assert!(project.env.is_empty());
        assert!(project.scripts.is_empty());
    }

    #[test]
    fn a_secret_env_var_persists_an_empty_value() {
        // Struct-update syntax rather than `let mut c = Config::default(); c.x = ..`:
        // clippy's field_reassign_with_default rejects the latter, and CI denies warnings.
        let config = Config {
            global_env: vec![EnvVar {
                key: "DATABASE_PASSWORD".into(),
                // What a caller must store for a secret: the real value lives
                // in the keychain, added in plan 2.
                value: String::new(),
                secret: true,
            }],
            ..Config::default()
        };

        let text = toml::to_string_pretty(&config).expect("serialize");

        assert!(text.contains("secret = true"), "got:\n{text}");
        assert!(text.contains("value = \"\""), "got:\n{text}");

        let back: Config = toml::from_str(&text).expect("deserialize");
        assert_eq!(back.global_env[0].value, "");
        assert!(back.global_env[0].secret);
    }
}
