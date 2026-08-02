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
use crate::validate::index_credential_var;
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

/// Resolve one variable into `into`, returning the value when it came from
/// the keychain so the caller can track it for redaction.
fn resolve_var(
    var: &EnvVar,
    key: SecretKey,
    store: &dyn SecretStore,
    into: &mut BTreeMap<String, String>,
) -> Result<Option<String>, EnvError> {
    if !var.secret {
        into.insert(var.key.clone(), var.value.clone());
        return Ok(None);
    }

    // A secret with nothing stored is actively *removed* rather than set
    // empty: an empty value looks set to the child and fails somewhere far
    // from the cause. Removing rather than skipping matters because the layers
    // accumulate into one map -- skipping would let an earlier layer's value
    // survive, so a user who has `AWS_SECRET_ACCESS_KEY` exported in the shell
    // that launched Bombadil and has marked it secret but not yet filled it in
    // would silently get the ambient value while believing Bombadil supplied
    // it.
    let stored = store.get(&key).map_err(|source| EnvError::Secret {
        message: source.to_string(),
    })?;
    match stored {
        Some(value) => {
            into.insert(var.key.clone(), value.clone());
            Ok(Some(value))
        }
        None => {
            into.remove(&var.key);
            Ok(None)
        }
    }
}

/// Replace every occurrence of a known secret value with `***`.
///
/// `compose` is the only place that knows which values it fetched from the
/// keychain, so it is the only place that can redact them out of an error
/// before it is shown to a user. A pre-activate script traced with `set -x`
/// writes its commands to stderr with variables already expanded, so a
/// failing script's stderr can otherwise carry a secret straight into
/// [`EnvError::ScriptFailed`] verbatim.
///
/// No minimum length is enforced on a secret before it is treated as a
/// redaction needle (empty strings are the only exception, skipped below).
/// A short secret risks over-redacting unrelated stderr that happens to
/// contain the same short string, but a length floor would risk leaving a
/// short secret unredacted outright -- a guaranteed leak traded for
/// readability. Over-redaction fails safe; that is the trade this function
/// deliberately takes.
fn redact(mut text: String, secrets: &[String]) -> String {
    for secret in secrets {
        // An empty needle would match (and get replaced) everywhere.
        if secret.is_empty() {
            continue;
        }
        text = text.replace(secret.as_str(), "***");
    }
    text
}

/// Redact every collected secret out of an [`EnvError`] before it propagates,
/// covering the two variants that can carry a script's raw stderr or spawn
/// message.
fn redact_env_error(err: EnvError, secrets: &[String]) -> EnvError {
    match err {
        EnvError::ScriptFailed {
            script,
            status,
            stderr,
        } => EnvError::ScriptFailed {
            script,
            status,
            stderr: redact(stderr, secrets),
        },
        EnvError::ScriptSpawn { script, message } => EnvError::ScriptSpawn {
            script,
            message: redact(message, secrets),
        },
        other => other,
    }
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
///
/// Secret redaction is applied only around the `apply` call for each
/// pre-activate script -- a future text-carrying `EnvError` raised elsewhere
/// in this function would bypass it. It also only covers secrets resolved
/// *before* the failing script ran: a value a pre-activate script introduces
/// itself (e.g. one it exports for a later script) is not in the redaction
/// set, so a later script's traced stderr could still expose it.
pub fn compose(
    inputs: &EnvInputs,
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
) -> Result<BTreeMap<String, String>, EnvError> {
    // 1. parent
    let mut env = inputs.parent.clone();

    // Values fetched from the keychain, collected as they resolve so a
    // failing pre-activate script's error can be scrubbed of them before it
    // is shown to a user.
    let mut secrets: Vec<String> = Vec::new();

    // 2. global variables
    for var in &inputs.config.global_env {
        if let Some(value) = resolve_var(
            var,
            SecretKey::GlobalEnv {
                key: var.key.clone(),
            },
            store,
            &mut env,
        )? {
            secrets.push(value);
        }
    }

    // 3 and 4. project variables, secrets resolved from the keychain
    for var in &inputs.project.env {
        if let Some(value) = resolve_var(
            var,
            SecretKey::ProjectEnv {
                project_id: inputs.project.id,
                key: var.key.clone(),
            },
            store,
            &mut env,
        )? {
            secrets.push(value);
        }
    }

    // 5. indexes this project selected
    let selected: Vec<&crate::model::Index> = inputs
        .config
        .indexes
        .iter()
        .filter(|i| inputs.project.index_names.contains(&i.name))
        .collect();
    inject(&selected, store, &mut env)?;
    for index in &selected {
        if let Some(password) = env.get(&index_credential_var(&index.name, "PASSWORD")) {
            secrets.push(password.clone());
        }
    }

    // 6. pre-activate scripts, in order
    for script in inputs
        .config
        .global_scripts
        .iter()
        .chain(&inputs.project.scripts)
    {
        env = apply(script, runner, &env).map_err(|err| redact_env_error(err, &secrets))?;
    }

    // 7. activation, last so a script cannot undo it. UV_PROJECT_ENVIRONMENT
    //    and VIRTUAL_ENV name the same venv and are set together: if a script
    //    could redirect one but not the other, uv would operate on one venv
    //    while the shell showed another.
    let bin = venv_bin_dir(&inputs.venv_path);
    let bin = bin.to_string_lossy().into_owned();
    // Windows variable names are case-insensitive and `std::env::vars()`
    // yields the canonical spelling `Path`. Looking up "PATH" exactly would
    // miss it, then insert a *second* key -- and since the child's environment
    // is written in `BTreeMap` order and `Command` keys its env table
    // case-insensitively on Windows, whichever spelling is written last wins,
    // leaving the venv off `PATH` entirely.
    let path_key = env
        .keys()
        .find(|k| k.eq_ignore_ascii_case("PATH"))
        .cloned()
        .unwrap_or_else(|| "PATH".to_string());
    let path = match env.get(&path_key) {
        Some(existing) if !existing.is_empty() => {
            format!("{bin}{}{existing}", if cfg!(windows) { ";" } else { ":" })
        }
        _ => bin.clone(),
    };
    // Picking one spelling is not enough when the map holds several -- a
    // config naming a variable `PATH` alongside the Windows parent's `Path`
    // gives two, and `find` takes the `BTreeMap`-sorted first ("PATH" <
    // "Path"), leaving the other behind to be written afterwards with its
    // stale value. Exactly one PATH-ish key may survive.
    env.retain(|k, _| k == &path_key || !k.eq_ignore_ascii_case("PATH"));
    env.insert(path_key, path);
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

    /// The program `dump_command` builds on this platform. Hardcoding "sh"
    /// made every one of these tests fail on Windows, where the spec names
    /// `powershell` and nothing matched the programmed rule.
    fn shell() -> &'static str {
        if cfg!(windows) { "powershell" } else { "sh" }
    }

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
                // Seeded so "a secret with nothing stored" is tested against a
                // parent that actually carries the variable. Without it the
                // test passes whether or not the layer removes anything.
                ("TOKEN".to_string(), "ambient".to_string()),
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
    fn a_secret_with_nothing_stored_is_removed_not_left_to_the_parent() {
        // An empty value looks set to the child process and fails in a way
        // that points nowhere near the real cause -- but "omitted" is not
        // enough either. The layers accumulate into one map, so merely
        // skipping the insert would pass the parent's ambient TOKEN straight
        // through while the user believes Bombadil supplied it.
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
    fn a_global_secret_is_read_from_the_global_keychain_key() {
        // Mis-keying this (e.g. reusing ProjectEnv) would leave every other
        // compose test green while global secrets silently never resolved.
        let config = Config {
            global_env: vec![EnvVar {
                key: "GLOBAL_TOKEN".into(),
                value: String::new(),
                secret: true,
            }],
            ..Config::default()
        };
        let store = InMemorySecretStore::new();
        store
            .set(
                &SecretKey::GlobalEnv {
                    key: "GLOBAL_TOKEN".into(),
                },
                "from-global-keychain",
            )
            .unwrap();
        let runner = FakeCommandRunner::new();

        let got = compose(&inputs(config, project()), &store, &runner).unwrap();

        assert_eq!(
            got.get("GLOBAL_TOKEN").map(String::as_str),
            Some("from-global-keychain")
        );
    }

    #[test]
    fn a_global_script_runs_too_not_just_the_projects() {
        // Dropping the `.chain()` in compose would leave every other test
        // green while global pre-activate scripts silently never ran.
        let config = Config {
            global_scripts: vec![Script {
                path: PathBuf::from("/g/global.sh"),
                args: vec![],
                enabled: true,
            }],
            ..Config::default()
        };
        let runner = FakeCommandRunner::new();
        runner.expect(
            shell(),
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: "PATH=/usr/bin\0FROM_GLOBAL_SCRIPT=set\0__BOMBADIL_DUMP_OK=1\0".into(),
                stderr: String::new(),
            },
        );
        let store = InMemorySecretStore::new();

        let got = compose(&inputs(config, project()), &store, &runner).unwrap();

        assert_eq!(
            got.get("FROM_GLOBAL_SCRIPT").map(String::as_str),
            Some("set")
        );
        assert_eq!(
            runner.calls().len(),
            1,
            "the global script must be run once"
        );
    }

    #[test]
    fn the_venv_is_prepended_to_a_windows_style_path_key() {
        // Windows variable names are case-insensitive and `std::env::vars()`
        // yields `Path`. An exact "PATH" lookup misses it and inserts a second
        // key; the child's environment is written in BTreeMap order, so
        // "PATH" (the venv) would be overwritten by "Path" (the parent's) and
        // the venv would not be on PATH at all.
        let mut ins = inputs(Config::default(), project());
        ins.parent = BTreeMap::from([("Path".to_string(), "/usr/bin".to_string())]);

        let got = compose_with(ins);

        let path_keys: Vec<&String> = got
            .keys()
            .filter(|k| k.eq_ignore_ascii_case("PATH"))
            .collect();
        assert_eq!(path_keys.len(), 1, "got {path_keys:?}");
        let path = &got[path_keys[0]];
        assert!(path.starts_with("/envs/my-api"), "got {path}");
        assert!(
            path.contains("/usr/bin"),
            "the inherited PATH must survive; got {path}"
        );
    }

    #[test]
    fn only_one_path_key_survives_when_the_parent_holds_two_spellings() {
        // A config naming a variable `PATH` alongside the Windows parent's
        // `Path` puts both in the map. `find` walks BTreeMap order and "PATH"
        // sorts before "Path", so the stale `Path` would survive untouched --
        // and `Command` keys its env table case-insensitively on Windows, so
        // whichever is written last wins. Written last is "Path".
        let mut ins = inputs(Config::default(), project());
        ins.parent = BTreeMap::from([
            ("PATH".to_string(), "/from/upper".to_string()),
            ("Path".to_string(), "/from/mixed".to_string()),
        ]);

        let got = compose_with(ins);

        let path_keys: Vec<&String> = got
            .keys()
            .filter(|k| k.eq_ignore_ascii_case("PATH"))
            .collect();
        assert_eq!(path_keys.len(), 1, "got {path_keys:?}");
        assert!(got[path_keys[0]].starts_with("/envs/my-api"), "got {got:?}");
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
            shell(),
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
                    "__BOMBADIL_DUMP_OK=1\0",
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

    #[test]
    fn redact_replaces_every_occurrence_of_a_secret_in_one_string() {
        let out = redact(
            "leaked once: hunter2, and again: hunter2".to_string(),
            &["hunter2".to_string()],
        );
        assert_eq!(out, "leaked once: ***, and again: ***");
    }

    #[test]
    fn a_failing_pre_activate_script_never_leaks_a_secret_in_its_error() {
        // A script traced with `set -x` writes its commands to stderr with
        // variables already expanded, so a failing traced script can put a
        // keychain secret straight into EnvError::ScriptFailed verbatim --
        // exactly the kind of failure report the GUI shows the user.
        let project = Project {
            env: vec![EnvVar {
                key: "TOKEN".into(),
                value: String::new(),
                secret: true,
            }],
            scripts: vec![Script {
                path: PathBuf::from("/p/pre.sh"),
                args: vec![],
                enabled: true,
            }],
            ..project()
        };
        let store = InMemorySecretStore::new();
        store
            .set(
                &SecretKey::ProjectEnv {
                    project_id: Uuid::from_u128(7),
                    key: "TOKEN".into(),
                },
                "s3cr3t-value",
            )
            .unwrap();
        let runner = FakeCommandRunner::new();
        runner.expect(
            shell(),
            &[],
            CommandOutcome {
                status: Some(1),
                stdout: String::new(),
                stderr: "+ curl -H 'Authorization: Bearer s3cr3t-value'".into(),
            },
        );

        let err = compose(&inputs(Config::default(), project), &store, &runner).unwrap_err();

        let rendered = err.to_string();
        assert!(rendered.contains("***"), "got {rendered}");
        assert!(!rendered.contains("s3cr3t-value"), "got {rendered}");
    }

    #[test]
    fn a_failing_pre_activate_script_never_leaks_an_index_password_in_its_error() {
        // The redaction set is built from project/global env secrets AND from
        // the index passwords compose reads back after inject -- deleting
        // just that read-back would leave the project-secret leak test above
        // green while a private index's password still leaked.
        use crate::model::{Index, IndexAuth, IndexKind};

        let idx = Index {
            name: "priv".into(),
            url: "https://priv.example/simple".into(),
            kind: IndexKind::Extra,
            default_for_new_projects: false,
            auth: IndexAuth::Basic {
                username: "alice".into(),
            },
        };
        let config = Config {
            indexes: vec![idx],
            ..Config::default()
        };
        let project = Project {
            index_names: vec!["priv".into()],
            scripts: vec![Script {
                path: PathBuf::from("/p/pre.sh"),
                args: vec![],
                enabled: true,
            }],
            ..project()
        };
        let store = InMemorySecretStore::new();
        store
            .set(
                &SecretKey::Index {
                    name: "priv".into(),
                },
                "index-password-value",
            )
            .unwrap();
        let runner = FakeCommandRunner::new();
        runner.expect(
            shell(),
            &[],
            CommandOutcome {
                status: Some(1),
                stdout: String::new(),
                stderr: "+ curl -u alice:index-password-value https://priv.example/simple".into(),
            },
        );

        let err = compose(&inputs(config, project), &store, &runner).unwrap_err();

        let rendered = err.to_string();
        assert!(rendered.contains("***"), "got {rendered}");
        assert!(!rendered.contains("index-password-value"), "got {rendered}");
    }

    #[test]
    fn a_spawn_failure_never_leaks_a_secret_in_its_error() {
        // Covers the ScriptSpawn arm of redact_env_error -- the tests above
        // only exercise ScriptFailed. An unprogrammed FakeCommandRunner call
        // reports the exact command line it could not match, and a script
        // argument is interpolated straight into that command line, so this
        // reaches ScriptSpawn with the secret in its `message` without a real
        // spawn failure. The secret goes in `args`, not `path`: `path` also
        // becomes EnvError::ScriptSpawn's `script` field, which redaction
        // does not touch, and putting it there would leak through that field
        // instead of testing the one this test targets.
        let project = Project {
            env: vec![EnvVar {
                key: "TOKEN".into(),
                value: String::new(),
                secret: true,
            }],
            scripts: vec![Script {
                path: PathBuf::from("/p/pre.sh"),
                args: vec!["s3cr3t-spawn-value".to_string()],
                enabled: true,
            }],
            ..project()
        };
        let store = InMemorySecretStore::new();
        store
            .set(
                &SecretKey::ProjectEnv {
                    project_id: Uuid::from_u128(7),
                    key: "TOKEN".into(),
                },
                "s3cr3t-spawn-value",
            )
            .unwrap();
        // No outcome programmed: the runner fails every call, and its
        // CommandError::Unexpected message embeds the whole command line --
        // including the secret-bearing script path -- so this reaches
        // ScriptSpawn with the secret in its message without spawning
        // anything real.
        let runner = FakeCommandRunner::new();

        let err = compose(&inputs(Config::default(), project), &store, &runner).unwrap_err();

        let rendered = err.to_string();
        assert!(rendered.contains("***"), "got {rendered}");
        assert!(!rendered.contains("s3cr3t-spawn-value"), "got {rendered}");
    }

    #[test]
    fn compose_and_uv_command_project_spec_agree_on_uv_project_environment() {
        // Plan 2c hands compose's output to a uv command with clear_env =
        // true, discarding project_spec's own UV_PROJECT_ENVIRONMENT --
        // which is safe only because both derive it from the same venv_path
        // the same way. Nothing else pins that the two stay in sync.
        let venv_path = PathBuf::from("/envs/my-api");

        let mut ins = inputs(Config::default(), project());
        ins.venv_path = venv_path.clone();
        let composed = compose_with(ins);

        // Constructed directly rather than resolved, so this test touches
        // neither a real uv binary nor the filesystem.
        let uv = crate::uv::ResolvedUv {
            path: PathBuf::from("/data/uv/uv"),
            version: crate::uv::UvVersion {
                major: 0,
                minor: 12,
                patch: 1,
            },
            origin: crate::uv::UvOrigin::Bundled,
            newer_than_bundled: false,
        };
        let ctx = crate::uv::command::ProjectContext {
            project_dir: PathBuf::from("/proj/my-api"),
            venv_path,
        };
        let spec = crate::uv::command::sync(&uv, &ctx);

        assert!(
            composed.contains_key("UV_PROJECT_ENVIRONMENT"),
            "got {composed:?}"
        );
        assert_eq!(
            composed.get("UV_PROJECT_ENVIRONMENT"),
            spec.env.get("UV_PROJECT_ENVIRONMENT"),
            "compose and uv::command::project_spec must derive the same value from venv_path"
        );
    }
}
