//! Laying an imported config onto this machine's, and the secrets that come
//! with it.

use super::{BackupError, Secrets};
use crate::model::Config;
use crate::secrets::{SecretKey, SecretStore};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Merge,
    Replace,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub projects: usize,
    pub indexes: usize,
    /// Projects, indexes, env vars and scripts already here.
    pub skipped: usize,
    pub secrets: usize,
    /// Labels of imported projects whose `pyproject.toml` isn't here.
    pub not_found: Vec<String>,
    /// Why the secrets could not be written, when the config was.
    pub secrets_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Planned {
    pub config: Config,
    pub secrets: Secrets,
    pub report: Report,
}

pub fn key_for(account: &str) -> Option<SecretKey> {
    if account == "github/token" {
        return Some(SecretKey::GitHubToken);
    }
    if let Some(name) = account.strip_prefix("index/") {
        return Some(SecretKey::Index { name: name.into() });
    }
    let rest = account.strip_prefix("env/")?;
    let (scope, key) = rest.split_once('/')?;
    if scope == "global" {
        return Some(SecretKey::GlobalEnv { key: key.into() });
    }
    let project_id = scope.parse().ok()?;
    Some(SecretKey::ProjectEnv {
        project_id,
        key: key.into(),
    })
}

pub fn plan(
    current: &Config,
    incoming: Config,
    secrets: Secrets,
    mode: Mode,
    exists: &dyn Fn(&Path) -> bool,
) -> Planned {
    let mut report = Report::default();
    let (config, secrets) = match mode {
        Mode::Replace => {
            let mut config = incoming;
            let here = &current.settings;
            config.settings.layout.window = here.layout.window;
            config.settings.layout.sidebar_width = here.layout.sidebar_width;
            config.settings.layout.sidebar_collapsed = here.layout.sidebar_collapsed;
            config.settings.pycharm = here.pycharm.clone();
            config.settings.github_account = here.github_account.clone();
            report.projects = config.projects.len();
            report.indexes = config.indexes.len();
            (config, secrets)
        }
        Mode::Merge => {
            let mut config = current.clone();
            let mut kept = Secrets::default();
            let mut carry = |account: String, secrets: &Secrets| {
                if let Some(v) = secrets.0.get(&account) {
                    kept.0.insert(account, v.clone());
                }
            };
            for project in incoming.projects {
                let here = config
                    .projects
                    .iter()
                    .any(|p| p.id == project.id || p.pyproject_path == project.pyproject_path);
                if here {
                    report.skipped += 1;
                    continue;
                }
                // Every secret filed under this project, whatever its env
                // list says now.
                let prefix = format!("env/{}/", project.id);
                for account in secrets.0.keys().filter(|a| a.starts_with(&prefix)) {
                    carry(account.clone(), &secrets);
                }
                report.projects += 1;
                config.projects.push(project);
            }
            for index in incoming.indexes {
                if config.indexes.iter().any(|i| i.name == index.name) {
                    report.skipped += 1;
                    continue;
                }
                carry(format!("index/{}", index.name), &secrets);
                report.indexes += 1;
                config.indexes.push(index);
            }
            for var in incoming.global_env {
                if config.global_env.iter().any(|v| v.key == var.key) {
                    report.skipped += 1;
                    continue;
                }
                if var.secret {
                    carry(format!("env/global/{}", var.key), &secrets);
                }
                config.global_env.push(var);
            }
            for script in incoming.global_scripts {
                if config.global_scripts.iter().any(|s| s.path == script.path) {
                    report.skipped += 1;
                    continue;
                }
                config.global_scripts.push(script);
            }
            // `write_secrets` in Merge never overwrites, so a token already
            // here stays.
            carry("github/token".into(), &secrets);
            (config, kept)
        }
    };
    report.secrets = secrets.0.len();
    let imported: Vec<_> = match mode {
        Mode::Replace => config.projects.iter().collect(),
        Mode::Merge => config.projects[current.projects.len()..].iter().collect(),
    };
    report.not_found = imported
        .into_iter()
        .filter(|p| !exists(&p.pyproject_path))
        .map(|p| p.label.clone())
        .collect();
    Planned {
        config,
        secrets,
        report,
    }
}

/// Writes each secret to the keychain. Merge skips one already set here.
/// Returns how many were written. An account name this build doesn't know
/// is skipped.
pub fn write_secrets(
    secrets: &Secrets,
    store: &dyn SecretStore,
    mode: Mode,
) -> Result<usize, BackupError> {
    let mut written = 0;
    for (account, value) in &secrets.0 {
        let Some(key) = key_for(account) else {
            continue;
        };
        if mode == Mode::Merge && store.get(&key)?.is_some() {
            continue;
        }
        store.set(&key, value)?;
        written += 1;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EnvVar, Index, IndexAuth, Project};
    use crate::secrets::InMemorySecretStore;

    fn project(label: &str) -> Project {
        Project {
            id: uuid::Uuid::new_v4(),
            label: label.into(),
            pyproject_path: format!("/p/{label}/pyproject.toml").into(),
            ..Default::default()
        }
    }

    fn index(name: &str) -> Index {
        Index {
            name: name.into(),
            url: "https://x/simple".into(),
            kind: crate::model::IndexKind::Extra,
            default_for_new_projects: false,
            auth: IndexAuth::Token,
        }
    }

    fn secrets(pairs: &[(&str, &str)]) -> Secrets {
        Secrets(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn merge_skips_a_project_already_here_by_id_or_path() {
        let here = project("api");
        let mut same_path = project("web");
        let current = Config {
            projects: vec![here.clone(), same_path.clone()],
            ..Default::default()
        };
        same_path.id = uuid::Uuid::new_v4();
        let new = project("cli");
        let incoming = Config {
            projects: vec![here.clone(), same_path, new.clone()],
            ..Default::default()
        };
        let p = plan(&current, incoming, Secrets::default(), Mode::Merge, &|_| {
            true
        });
        assert_eq!(p.config.projects.len(), 3);
        assert_eq!(p.config.projects[2].id, new.id);
        assert_eq!((p.report.projects, p.report.skipped), (1, 2));
    }

    #[test]
    fn merge_skips_an_index_env_var_and_script_by_name() {
        let current = Config {
            indexes: vec![index("corp")],
            global_env: vec![EnvVar {
                key: "A".into(),
                value: "here".into(),
                secret: false,
            }],
            ..Default::default()
        };
        let incoming = Config {
            indexes: vec![index("corp"), index("other")],
            global_env: vec![
                EnvVar {
                    key: "A".into(),
                    value: "there".into(),
                    secret: false,
                },
                EnvVar {
                    key: "B".into(),
                    value: "b".into(),
                    secret: false,
                },
            ],
            ..Default::default()
        };
        let p = plan(&current, incoming, Secrets::default(), Mode::Merge, &|_| {
            true
        });
        assert_eq!(p.config.indexes.len(), 2);
        assert_eq!(p.config.global_env[0].value, "here");
        assert_eq!(p.config.global_env.len(), 2);
        assert_eq!(p.report.indexes, 1);
    }

    #[test]
    fn merge_keeps_this_machines_settings() {
        let mut current = Config::default();
        current.settings.theme = crate::model::ThemeChoice::Dark;
        let mut incoming = Config::default();
        incoming.settings.theme = crate::model::ThemeChoice::Light;
        let p = plan(&current, incoming, Secrets::default(), Mode::Merge, &|_| {
            true
        });
        assert_eq!(p.config.settings.theme, crate::model::ThemeChoice::Dark);
    }

    #[test]
    fn a_skipped_projects_secrets_are_not_carried() {
        let here = project("api");
        let new = project("cli");
        let current = Config {
            projects: vec![here.clone()],
            indexes: vec![index("corp")],
            ..Default::default()
        };
        let incoming = Config {
            projects: vec![here.clone(), new.clone()],
            indexes: vec![index("corp")],
            ..Default::default()
        };
        let s = secrets(&[
            (&format!("env/{}/T", here.id), "skip-me"),
            (&format!("env/{}/T", new.id), "keep-me"),
            ("index/corp", "skip-me-too"),
            ("github/token", "gh"),
        ]);
        let p = plan(&current, incoming, s, Mode::Merge, &|_| true);
        assert!(p.secrets.0.contains_key(&format!("env/{}/T", new.id)));
        assert!(p.secrets.0.contains_key("github/token"));
        assert!(!p.secrets.0.contains_key(&format!("env/{}/T", here.id)));
        assert!(!p.secrets.0.contains_key("index/corp"));
        assert_eq!(p.report.secrets, 2);
    }

    #[test]
    fn replace_takes_the_file_but_keeps_machine_local_settings() {
        let mut current = Config {
            projects: vec![project("old")],
            ..Default::default()
        };
        current.settings.layout.window = Some((800, 600));
        current.settings.pycharm = Some("/opt/pc".into());
        let mut incoming = Config {
            projects: vec![project("new")],
            ..Default::default()
        };
        incoming.settings.theme = crate::model::ThemeChoice::Light;
        let p = plan(
            &current,
            incoming,
            secrets(&[("index/x", "v")]),
            Mode::Replace,
            &|_| true,
        );
        assert_eq!(p.config.projects.len(), 1);
        assert_eq!(p.config.projects[0].label, "new");
        assert_eq!(p.config.settings.theme, crate::model::ThemeChoice::Light);
        assert_eq!(p.config.settings.layout.window, Some((800, 600)));
        assert_eq!(
            p.config.settings.pycharm.as_deref(),
            Some(std::path::Path::new("/opt/pc"))
        );
        assert_eq!(p.report.secrets, 1);
    }

    #[test]
    fn projects_whose_folder_is_missing_are_reported() {
        let incoming = Config {
            projects: vec![project("gone"), project("here")],
            ..Default::default()
        };
        let p = plan(
            &Config::default(),
            incoming,
            Secrets::default(),
            Mode::Merge,
            &|path| path.to_string_lossy().contains("here"),
        );
        assert_eq!(p.report.not_found, ["gone"]);
        assert_eq!(p.config.projects.len(), 2, "still imported");
    }

    #[test]
    fn merge_never_overwrites_a_secret_already_here() {
        let store = InMemorySecretStore::new();
        store.set(&SecretKey::GitHubToken, "mine").unwrap();
        let n = write_secrets(
            &secrets(&[("github/token", "theirs"), ("index/x", "v")]),
            &store,
            Mode::Merge,
        )
        .unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            store.get(&SecretKey::GitHubToken).unwrap().as_deref(),
            Some("mine")
        );
        assert_eq!(
            store
                .get(&SecretKey::Index { name: "x".into() })
                .unwrap()
                .as_deref(),
            Some("v")
        );
    }

    #[test]
    fn replace_overwrites() {
        let store = InMemorySecretStore::new();
        store.set(&SecretKey::GitHubToken, "mine").unwrap();
        write_secrets(
            &secrets(&[("github/token", "theirs")]),
            &store,
            Mode::Replace,
        )
        .unwrap();
        assert_eq!(
            store.get(&SecretKey::GitHubToken).unwrap().as_deref(),
            Some("theirs")
        );
    }

    #[test]
    fn account_names_read_back_into_keys() {
        let id = uuid::Uuid::new_v4();
        for key in [
            SecretKey::Index {
                name: "corp".into(),
            },
            SecretKey::ProjectEnv {
                project_id: id,
                key: "T".into(),
            },
            SecretKey::GlobalEnv { key: "G".into() },
            SecretKey::GitHubToken,
        ] {
            assert_eq!(key_for(&key.account()), Some(key));
        }
        assert_eq!(key_for("nonsense"), None);
    }
}
