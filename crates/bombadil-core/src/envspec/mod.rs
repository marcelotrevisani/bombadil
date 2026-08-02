mod dump;
pub mod index;
pub mod script;

pub use dump::parse_dump;
pub use index::inject;
pub use script::{apply, dump_command};

use crate::error::EnvError;
use crate::model::{Config, EnvVar, Project};
use crate::process::CommandRunner;
use crate::secrets::{SecretKey, SecretStore};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Everything needed to compose a child process's environment.
///
/// Owned rather than borrowed: the GUI builds one per launch, and borrowing
/// from the live config would tie it to a lock held across a subprocess
/// spawn.
pub struct EnvInputs {
    /// The environment Bombadil itself is running in.
    pub parent: BTreeMap<String, String>,
    pub config: Config,
    pub project: Project,
    /// Already resolved by `venv_path::resolve`.
    pub venv_path: PathBuf,
}

/// The directory holding a venv's executables, which differs by platform.
fn venv_bin_dir(venv: &std::path::Path) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts")
    } else {
        venv.join("bin")
    }
}

fn resolve_var(
    var: &EnvVar,
    key: SecretKey,
    store: &dyn SecretStore,
    into: &mut BTreeMap<String, String>,
) -> Result<(), EnvError> {
    if !var.secret {
        into.insert(var.key.clone(), var.value.clone());
        return Ok(());
    }

    // A secret with nothing stored is omitted rather than set empty: an empty
    // value looks set to the child and fails somewhere far from the cause.
    let stored = store.get(&key).map_err(|source| EnvError::Secret {
        message: source.to_string(),
    })?;
    if let Some(value) = stored {
        into.insert(var.key.clone(), value);
    }
    Ok(())
}

/// Compose the environment a child process runs in.
///
/// Layers, each overriding the previous, per spec section 6 (amended):
/// parent environment; global variables; project variables; secrets;
/// index configuration; pre-activate scripts; activation.
///
/// Scripts run second to last so they can see the project's configuration but
/// cannot undo activation, which is applied afterwards and wins.
/// `UV_PROJECT_ENVIRONMENT` and `VIRTUAL_ENV` name the same venv and are set
/// together in the final layer: if a script could redirect one but not the
/// other, uv would operate on one venv while the shell showed another --
/// silent and near-undiagnosable.
///
/// Known limitation: `PYTHONHOME` is only removed at the final (activation)
/// layer, so a pre-activate script still sees whatever stale value the
/// parent process had. This is deliberate -- they are *pre*-activate
/// scripts, and a non-activated environment is exactly what that name
/// promises -- but a script that shells out to `python` will still hit it.
pub fn compose(
    inputs: &EnvInputs,
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
) -> Result<BTreeMap<String, String>, EnvError> {
    // 1. parent
    let mut env = inputs.parent.clone();

    // 2. global variables
    for var in &inputs.config.global_env {
        resolve_var(
            var,
            SecretKey::GlobalEnv {
                key: var.key.clone(),
            },
            store,
            &mut env,
        )?;
    }

    // 3 and 4. project variables, secrets resolved from the keychain
    for var in &inputs.project.env {
        resolve_var(
            var,
            SecretKey::ProjectEnv {
                project_id: inputs.project.id,
                key: var.key.clone(),
            },
            store,
            &mut env,
        )?;
    }

    // 5. indexes this project selected
    let selected: Vec<&crate::model::Index> = inputs
        .config
        .indexes
        .iter()
        .filter(|i| inputs.project.index_names.contains(&i.name))
        .collect();
    inject(&selected, store, &mut env).map_err(|source| EnvError::Secret {
        message: source.to_string(),
    })?;

    // 6. pre-activate scripts, in order
    for script in inputs
        .config
        .global_scripts
        .iter()
        .chain(&inputs.project.scripts)
    {
        env = apply(script, runner, &env)?;
    }

    // 7. activation, last so a script cannot undo it. UV_PROJECT_ENVIRONMENT
    //    and VIRTUAL_ENV name the same venv and are set together: if a script
    //    could redirect one but not the other, uv would operate on one venv
    //    while the shell showed another.
    let bin = venv_bin_dir(&inputs.venv_path);
    let bin = bin.to_string_lossy().into_owned();
    let path = match env.get("PATH") {
        Some(existing) if !existing.is_empty() => {
            format!("{bin}{}{existing}", if cfg!(windows) { ";" } else { ":" })
        }
        _ => bin.clone(),
    };
    env.insert("PATH".to_string(), path);
    let venv = inputs.venv_path.to_string_lossy().into_owned();
    env.insert("UV_PROJECT_ENVIRONMENT".to_string(), venv.clone());
    env.insert("VIRTUAL_ENV".to_string(), venv);
    // A stale PYTHONHOME silently redirects the stdlib search.
    env.remove("PYTHONHOME");

    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Config, EnvVar, Project, Script, VenvLocation};
    use crate::process::{CommandOutcome, FakeCommandRunner};
    use crate::secrets::InMemorySecretStore;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn project() -> Project {
        Project {
            id: Uuid::from_u128(7),
            label: "my-api".into(),
            pyproject_path: PathBuf::from("/proj/my-api/pyproject.toml"),
            index_names: vec![],
            venv: VenvLocation::Alongside,
            env: vec![],
            scripts: vec![],
            members: Default::default(),
        }
    }

    fn inputs(config: Config, project: Project) -> EnvInputs {
        EnvInputs {
            parent: BTreeMap::from([
                ("PATH".to_string(), "/usr/bin".to_string()),
                ("HOME".to_string(), "/home/t".to_string()),
                ("PYTHONHOME".to_string(), "/stale".to_string()),
            ]),
            config,
            project,
            venv_path: PathBuf::from("/envs/my-api"),
        }
    }

    fn compose_with(inputs: EnvInputs) -> BTreeMap<String, String> {
        let store = InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();
        compose(&inputs, &store, &runner).unwrap()
    }

    #[test]
    fn the_parent_environment_is_inherited() {
        let got = compose_with(inputs(Config::default(), project()));
        assert_eq!(got.get("HOME").map(String::as_str), Some("/home/t"));
    }

    #[test]
    fn a_project_variable_overrides_a_global_one() {
        let config = Config {
            global_env: vec![EnvVar {
                key: "SHARED".into(),
                value: "global".into(),
                secret: false,
            }],
            ..Config::default()
        };
        let project = Project {
            env: vec![EnvVar {
                key: "SHARED".into(),
                value: "project".into(),
                secret: false,
            }],
            ..project()
        };

        let got = compose_with(inputs(config, project));

        assert_eq!(got.get("SHARED").map(String::as_str), Some("project"));
    }

    #[test]
    fn a_secret_variable_takes_its_value_from_the_keychain() {
        let project = Project {
            env: vec![EnvVar {
                key: "TOKEN".into(),
                value: String::new(),
                secret: true,
            }],
            ..project()
        };
        let store = InMemorySecretStore::new();
        store
            .set(
                &crate::secrets::SecretKey::ProjectEnv {
                    project_id: Uuid::from_u128(7),
                    key: "TOKEN".into(),
                },
                "from-keychain",
            )
            .unwrap();
        let runner = FakeCommandRunner::new();

        let got = compose(&inputs(Config::default(), project), &store, &runner).unwrap();

        assert_eq!(got.get("TOKEN").map(String::as_str), Some("from-keychain"));
    }

    #[test]
    fn a_secret_with_nothing_stored_is_omitted_rather_than_empty() {
        // An empty value looks set to the child process and fails in a way
        // that points nowhere near the real cause.
        let project = Project {
            env: vec![EnvVar {
                key: "TOKEN".into(),
                value: String::new(),
                secret: true,
            }],
            ..project()
        };

        let got = compose_with(inputs(Config::default(), project));

        assert!(!got.contains_key("TOKEN"), "got {got:?}");
    }

    #[test]
    fn activation_sets_virtual_env_and_prepends_path() {
        // Verified against a real venv: VIRTUAL_ENV plus a prepended PATH is
        // exactly what `source activate` does, and works in every shell.
        let got = compose_with(inputs(Config::default(), project()));

        assert_eq!(
            got.get("VIRTUAL_ENV").map(String::as_str),
            Some("/envs/my-api")
        );
        let path = got.get("PATH").expect("PATH");
        assert!(path.starts_with("/envs/my-api"), "got {path}");
        assert!(
            path.contains("/usr/bin"),
            "the inherited PATH must survive; got {path}"
        );
    }

    #[test]
    fn pythonhome_is_removed() {
        // A stale PYTHONHOME silently redirects the stdlib search and fails in
        // a way that looks nothing like its cause.
        let got = compose_with(inputs(Config::default(), project()));
        assert!(!got.contains_key("PYTHONHOME"), "got {got:?}");
    }

    #[test]
    fn uv_project_environment_points_at_the_venv() {
        let got = compose_with(inputs(Config::default(), project()));
        assert_eq!(
            got.get("UV_PROJECT_ENVIRONMENT").map(String::as_str),
            Some("/envs/my-api")
        );
    }

    #[test]
    fn a_project_only_gets_credentials_for_indexes_it_selected() {
        // A regression that drops or inverts the index_names filter would
        // inject every configured index's credentials into every project's
        // environment -- one project's private-registry password leaking
        // into another project's child processes.
        use crate::model::{Index, IndexAuth, IndexKind};
        use crate::validate::index_credential_var;

        let mine = Index {
            name: "mine".into(),
            url: "https://mine.example/simple".into(),
            kind: IndexKind::Extra,
            default_for_new_projects: false,
            auth: IndexAuth::Basic {
                username: "alice".into(),
            },
        };
        let theirs = Index {
            name: "theirs".into(),
            url: "https://theirs.example/simple".into(),
            kind: IndexKind::Extra,
            default_for_new_projects: false,
            auth: IndexAuth::Basic {
                username: "bob".into(),
            },
        };
        let config = Config {
            indexes: vec![mine.clone(), theirs.clone()],
            ..Config::default()
        };
        let project = Project {
            index_names: vec!["mine".into()],
            ..project()
        };
        let store = InMemorySecretStore::new();
        store
            .set(
                &crate::secrets::SecretKey::Index {
                    name: "mine".into(),
                },
                "my-secret",
            )
            .unwrap();
        store
            .set(
                &crate::secrets::SecretKey::Index {
                    name: "theirs".into(),
                },
                "their-secret",
            )
            .unwrap();
        let runner = FakeCommandRunner::new();

        let got = compose(&inputs(config, project), &store, &runner).unwrap();

        assert_eq!(
            got.get(&index_credential_var("mine", "USERNAME"))
                .map(String::as_str),
            Some("alice")
        );
        assert_eq!(
            got.get(&index_credential_var("mine", "PASSWORD"))
                .map(String::as_str),
            Some("my-secret")
        );
        assert!(
            !got.contains_key(&index_credential_var("theirs", "USERNAME")),
            "got {got:?}"
        );
        assert!(
            !got.contains_key(&index_credential_var("theirs", "PASSWORD")),
            "got {got:?}"
        );
    }

    #[test]
    fn a_script_runs_after_the_variables_and_before_activation() {
        // Order matters: the script must see the project's configuration, and
        // must not be able to undo activation.
        let project = Project {
            env: vec![EnvVar {
                key: "SEEN_BY_SCRIPT".into(),
                value: "yes".into(),
                secret: false,
            }],
            scripts: vec![Script {
                path: PathBuf::from("/p/pre.sh"),
                args: vec![],
                enabled: true,
            }],
            ..project()
        };

        let runner = FakeCommandRunner::new();
        // The script sees the whole environment up to this point (a real
        // `env -0` dump contains everything, not just what the script
        // changed), echoes back its own export, and tries to clobber both
        // VIRTUAL_ENV and UV_PROJECT_ENVIRONMENT -- which activation must
        // overwrite afterwards, since they name the same venv.
        runner.expect(
            "sh",
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: concat!(
                    "PATH=/usr/bin\0",
                    "HOME=/home/t\0",
                    "PYTHONHOME=/stale\0",
                    "SEEN_BY_SCRIPT=yes\0",
                    "UV_PROJECT_ENVIRONMENT=/wrong-from-script\0",
                    "FROM_SCRIPT=set\0",
                    "VIRTUAL_ENV=/wrong\0",
                )
                .to_string(),
                stderr: String::new(),
            },
        );
        let store = InMemorySecretStore::new();

        let got = compose(&inputs(Config::default(), project), &store, &runner).unwrap();

        assert_eq!(got.get("FROM_SCRIPT").map(String::as_str), Some("set"));
        assert_eq!(
            got.get("VIRTUAL_ENV").map(String::as_str),
            Some("/envs/my-api"),
            "activation runs after scripts and must win"
        );
        assert_eq!(
            got.get("UV_PROJECT_ENVIRONMENT").map(String::as_str),
            Some("/envs/my-api"),
            "a script must not be able to redirect which venv uv operates on"
        );
        assert_eq!(got.get("HOME").map(String::as_str), Some("/home/t"));
        assert!(!got.contains_key("PYTHONHOME"), "got {got:?}");
    }
}
