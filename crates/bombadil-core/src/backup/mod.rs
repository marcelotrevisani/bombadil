//! Moving a setup to another computer: one `.bombadil` file holding the
//! config and, encrypted with a passphrase, the keychain secrets it uses.

pub mod crypto;
pub mod import;
pub mod mapping;

use crate::error::SecretError;
use crate::model::{CURRENT_SCHEMA_VERSION, Config, IndexAuth};
use crate::secrets::{SecretKey, SecretStore};
use crypto::{Kdf, Sealed};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("this file was made by a newer Bombadil")]
    TooNew,
    #[error("this isn't a Bombadil export: {0}")]
    Parse(String),
    #[error("the passphrase is wrong, or the file was changed")]
    WrongPassphrase,
    #[error("this setup has {0} secrets; enter a passphrase to export them")]
    NeedsPassphrase(usize),
    #[error(transparent)]
    Keychain(#[from] SecretError),
    #[error("{0}")]
    Io(String),
}

pub const FORMAT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub format: u32,
    pub exported_from: crate::terminal::Os,
    pub home: PathBuf,
    pub config: Config,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets: Option<Sealed>,
}

/// Account name to secret value. `Debug` never shows a value.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secrets(pub BTreeMap<String, String>);

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.keys()).finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exported {
    pub text: String,
    pub secrets: usize,
    /// Accounts the config names but the keychain does not hold.
    pub missing: Vec<String>,
}

/// Every keychain entry `config` can name, plus the GitHub token.
pub fn secret_keys(config: &Config) -> Vec<SecretKey> {
    let indexes = config
        .indexes
        .iter()
        .filter(|i| i.auth != IndexAuth::None)
        .map(|i| SecretKey::Index {
            name: i.name.clone(),
        });
    let global = config
        .global_env
        .iter()
        .filter(|v| v.secret)
        .map(|v| SecretKey::GlobalEnv { key: v.key.clone() });
    let projects = config.projects.iter().flat_map(|p| {
        p.env
            .iter()
            .filter(|v| v.secret)
            .map(|v| SecretKey::ProjectEnv {
                project_id: p.id,
                key: v.key.clone(),
            })
    });
    indexes
        .chain(global)
        .chain(projects)
        .chain([SecretKey::GitHubToken])
        .collect()
}

/// `config` without the settings that only make sense on this machine.
pub fn portable(config: &Config) -> Config {
    let mut c = config.clone();
    let local = crate::model::Layout::default();
    c.settings.layout.window = None;
    c.settings.layout.sidebar_width = local.sidebar_width;
    c.settings.layout.sidebar_collapsed = local.sidebar_collapsed;
    c.settings.pycharm = None;
    c.settings.github_account = None;
    c
}

pub fn export(
    config: &Config,
    store: &dyn SecretStore,
    passphrase: &str,
    kdf: Kdf,
    home: &Path,
) -> Result<Exported, BackupError> {
    let mut found = BTreeMap::new();
    let mut missing = Vec::new();
    // No keychain on this machine: the config still goes, and every secret
    // it names is reported as not found.
    let available = store.is_available();
    for key in secret_keys(config) {
        let value = if available { store.get(&key)? } else { None };
        match value {
            Some(value) => {
                found.insert(key.account(), value);
            }
            // No token is the usual case, not a gap worth reporting.
            None if key == SecretKey::GitHubToken => {}
            None => missing.push(key.account()),
        }
    }
    if !found.is_empty() && passphrase.is_empty() {
        return Err(BackupError::NeedsPassphrase(found.len()));
    }
    let secrets = (!found.is_empty()).then(|| {
        let plain = toml::to_string(&found).expect("a string map always serializes");
        crypto::seal(plain.as_bytes(), passphrase, kdf)
    });
    let bundle = Bundle {
        format: FORMAT,
        exported_from: crate::terminal::Os::host(),
        home: home.to_path_buf(),
        config: portable(config),
        secrets,
    };
    let text = toml::to_string_pretty(&bundle).map_err(|e| BackupError::Parse(e.to_string()))?;
    Ok(Exported {
        text,
        secrets: found.len(),
        missing,
    })
}

#[derive(Deserialize)]
struct Probe {
    format: u32,
    config: Option<SchemaProbe>,
}

#[derive(Deserialize)]
struct SchemaProbe {
    schema_version: u32,
}

pub fn read(text: &str) -> Result<Bundle, BackupError> {
    let probe: Probe = toml::from_str(text).map_err(|e| BackupError::Parse(e.to_string()))?;
    let schema = probe.config.map_or(0, |c| c.schema_version);
    if probe.format > FORMAT || schema > CURRENT_SCHEMA_VERSION {
        return Err(BackupError::TooNew);
    }
    let mut bundle: Bundle = toml::from_str(text).map_err(|e| BackupError::Parse(e.to_string()))?;
    bundle.config = crate::store::migrate(bundle.config);
    Ok(bundle)
}

pub fn unlock(bundle: &Bundle, passphrase: &str) -> Result<Secrets, BackupError> {
    let Some(sealed) = &bundle.secrets else {
        return Ok(Secrets::default());
    };
    let plain = crypto::open(sealed, passphrase)?;
    // Fixed words, never the parser's: its errors quote the offending
    // line, which here is a secret.
    let unreadable = || BackupError::Parse("the secrets inside are unreadable".into());
    let text = String::from_utf8(plain).map_err(|_| unreadable())?;
    let map = toml::from_str(&text).map_err(|_| unreadable())?;
    Ok(Secrets(map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EnvVar, Index, IndexAuth, Project};
    use crate::secrets::InMemorySecretStore;
    use std::path::Path;

    fn setup() -> (Config, InMemorySecretStore) {
        let mut config = Config::default();
        let id = uuid::Uuid::new_v4();
        config.projects.push(Project {
            id,
            label: "api".into(),
            pyproject_path: "/home/a/code/api/pyproject.toml".into(),
            env: vec![EnvVar {
                key: "TOKEN".into(),
                value: String::new(),
                secret: true,
            }],
            ..Default::default()
        });
        config.indexes.push(Index {
            name: "corp".into(),
            url: "https://corp/simple".into(),
            kind: crate::model::IndexKind::Extra,
            default_for_new_projects: false,
            auth: IndexAuth::Token,
        });
        config.settings.layout.window = Some((900, 700));
        config.settings.pycharm = Some("/opt/pycharm".into());
        config.settings.github_account = Some("alice".into());
        let store = InMemorySecretStore::new();
        store
            .set(
                &SecretKey::Index {
                    name: "corp".into(),
                },
                "idx-pw",
            )
            .unwrap();
        store
            .set(
                &SecretKey::ProjectEnv {
                    project_id: id,
                    key: "TOKEN".into(),
                },
                "env-pw",
            )
            .unwrap();
        store.set(&SecretKey::GitHubToken, "gh-pw").unwrap();
        (config, store)
    }

    #[test]
    fn a_round_trip_restores_the_config_and_every_secret() {
        let (config, store) = setup();
        let out = export(
            &config,
            &store,
            "correct horse",
            Kdf::FAST,
            Path::new("/home/a"),
        )
        .unwrap();
        assert_eq!(out.secrets, 3);
        assert!(out.missing.is_empty());
        for secret in ["idx-pw", "env-pw", "gh-pw"] {
            assert!(!out.text.contains(secret), "{secret} in clear");
        }
        let bundle = read(&out.text).unwrap();
        assert_eq!(bundle.config.projects, config.projects);
        assert_eq!(bundle.home, Path::new("/home/a"));
        let secrets = unlock(&bundle, "correct horse").unwrap();
        assert_eq!(
            secrets.0.get("index/corp").map(String::as_str),
            Some("idx-pw")
        );
        assert_eq!(
            secrets.0.get("github/token").map(String::as_str),
            Some("gh-pw")
        );
        assert_eq!(secrets.0.len(), 3);
    }

    #[test]
    fn machine_local_settings_stay_behind() {
        let (config, store) = setup();
        let out = export(
            &config,
            &store,
            "correct horse",
            Kdf::FAST,
            Path::new("/home/a"),
        )
        .unwrap();
        let s = read(&out.text).unwrap().config.settings;
        assert_eq!(s.layout.window, None);
        assert_eq!(s.pycharm, None);
        assert_eq!(s.github_account, None);
    }

    #[test]
    fn a_wrong_passphrase_unlocks_nothing() {
        let (config, store) = setup();
        let out = export(
            &config,
            &store,
            "correct horse",
            Kdf::FAST,
            Path::new("/home/a"),
        )
        .unwrap();
        let bundle = read(&out.text).unwrap();
        assert!(matches!(
            unlock(&bundle, "nope nope"),
            Err(BackupError::WrongPassphrase)
        ));
    }

    #[test]
    fn a_secret_missing_from_the_keychain_is_reported_not_fatal() {
        let (config, store) = setup();
        store
            .delete(&SecretKey::Index {
                name: "corp".into(),
            })
            .unwrap();
        let out = export(
            &config,
            &store,
            "correct horse",
            Kdf::FAST,
            Path::new("/home/a"),
        )
        .unwrap();
        assert_eq!(out.secrets, 2);
        assert_eq!(out.missing, ["index/corp"]);
    }

    #[test]
    fn no_secrets_means_no_secrets_table_and_no_passphrase() {
        let config = Config::default();
        let out = export(
            &config,
            &InMemorySecretStore::new(),
            "",
            Kdf::FAST,
            Path::new("/h"),
        )
        .unwrap();
        assert!(!out.text.contains("[secrets]"));
        let bundle = read(&out.text).unwrap();
        assert_eq!(unlock(&bundle, "").unwrap(), Secrets::default());
    }

    #[test]
    fn secrets_without_a_passphrase_are_refused() {
        let (config, store) = setup();
        assert!(matches!(
            export(&config, &store, "", Kdf::FAST, Path::new("/home/a")),
            Err(BackupError::NeedsPassphrase(3))
        ));
    }

    #[test]
    fn a_newer_format_is_refused() {
        assert!(matches!(read("format = 2\n"), Err(BackupError::TooNew)));
    }

    #[test]
    fn a_newer_config_schema_is_refused() {
        let text = format!(
            "format = 1\nexported_from = \"linux\"\nhome = \"/h\"\n\n[config]\nschema_version = {}\n",
            crate::model::CURRENT_SCHEMA_VERSION + 1
        );
        assert!(matches!(read(&text), Err(BackupError::TooNew)));
    }

    #[test]
    fn an_old_config_inside_is_migrated() {
        let text = "format = 1\nexported_from = \"linux\"\nhome = \"/h\"\n\n[config]\nschema_version = 1\n\n[[config.projects]]\nid = \"11111111-1111-1111-1111-111111111111\"\nlabel = \"api\"\npyproject_path = \"/h/api/pyproject.toml\"\n";
        let bundle = read(text).unwrap();
        assert_eq!(
            bundle.config.schema_version,
            crate::model::CURRENT_SCHEMA_VERSION
        );
        assert_eq!(bundle.config.projects[0].environments.len(), 1);
    }

    #[test]
    fn not_a_bundle_is_a_parse_error() {
        assert!(matches!(read("hello"), Err(BackupError::Parse(_))));
    }

    /// A machine with no keychain at all: every call fails.
    struct NoKeychain;

    impl SecretStore for NoKeychain {
        fn get(&self, key: &SecretKey) -> Result<Option<String>, crate::error::SecretError> {
            Err(crate::error::SecretError::Backend {
                account: key.account(),
                message: "no Secret Service".into(),
            })
        }
        fn set(&self, _: &SecretKey, _: &str) -> Result<(), crate::error::SecretError> {
            unreachable!()
        }
        fn delete(&self, _: &SecretKey) -> Result<(), crate::error::SecretError> {
            unreachable!()
        }
        fn is_available(&self) -> bool {
            false
        }
    }

    #[test]
    fn no_keychain_still_exports_the_config() {
        let (config, _) = setup();
        let out = export(&config, &NoKeychain, "", Kdf::FAST, Path::new("/home/a")).unwrap();
        assert_eq!(out.secrets, 0);
        assert_eq!(out.missing.len(), 2, "{:?}", out.missing);
        assert_eq!(read(&out.text).unwrap().config.projects, config.projects);
    }

    #[test]
    fn a_bad_secret_table_never_quotes_its_text() {
        let out = export(
            &Config::default(),
            &InMemorySecretStore::new(),
            "",
            Kdf::FAST,
            Path::new("/h"),
        )
        .unwrap();
        let mut bundle = read(&out.text).unwrap();
        bundle.secrets = Some(crypto::seal(
            b"index/corp = s3cret-unquoted",
            "correct horse",
            Kdf::FAST,
        ));
        let err = unlock(&bundle, "correct horse").unwrap_err().to_string();
        assert!(!err.contains("s3cret"), "{err}");
    }

    #[test]
    fn secrets_never_show_in_debug() {
        let s = Secrets(BTreeMap::from([(
            "index/corp".to_string(),
            "idx-pw".to_string(),
        )]));
        assert!(!format!("{s:?}").contains("idx-pw"));
    }
}
