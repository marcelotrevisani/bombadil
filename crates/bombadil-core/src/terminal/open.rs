//! Wiring plan 2b's `envspec::compose` to the terminal-spawning pieces built
//! by Tasks 1-4. Nothing before this file has had a production caller for
//! `compose`.
//!
//! **The composed environment carries secrets** (keychain values, private
//! index passwords). Nothing here may log a [`CommandSpec`] -- its `Debug`
//! impl already hides `env`, and that must stay the only way this data is
//! ever rendered. Any error that can carry free text is routed through
//! redaction before it reaches a caller.

use super::activation::{ShellFamily, init_file};
use super::argv;
use super::{Os, TerminalChoice};
use crate::envspec::{EnvInputs, compose};
use crate::error::EnvError;
use crate::process::CommandRunner;
use crate::secrets::SecretStore;
use crate::validate::index_credential_var;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The directory holding a venv's executables, which differs by platform.
///
/// Duplicated from the private `envspec::venv_bin_dir` rather than shared
/// across modules for one two-line call site.
fn venv_bin_dir(venv: &Path) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts")
    } else {
        venv.join("bin")
    }
}

/// Where `pyproject.toml` lives -- spec section 8 opens the terminal here,
/// not in the venv. Falls back to the venv's own directory only if the
/// configured path is rootless, which `venv_path::resolve` already refuses
/// to produce a venv for, so this is not expected to be reached in practice.
fn project_dir(inputs: &EnvInputs) -> PathBuf {
    inputs
        .project
        .pyproject_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| inputs.venv_path.clone())
}

/// Substitute the init file's `{file}`/`{dir}` placeholders into a batch of
/// strings.
fn substitute<'a>(values: impl Iterator<Item = &'a String>, file: &str, dir: &str) -> Vec<String> {
    values
        .map(|v| v.replace("{file}", file).replace("{dir}", dir))
        .collect()
}

/// Every secret value that ended up in the composed environment, gathered by
/// re-reading which keys were marked secret -- not by re-touching the
/// keychain. `compose` already resolved these once; this just tells them
/// apart from the ordinary values sitting alongside them in `env`, mirroring
/// how `compose` builds its own redaction set.
fn secret_values(inputs: &EnvInputs, env: &BTreeMap<String, String>) -> Vec<String> {
    let mut secrets = Vec::new();
    for var in inputs.config.global_env.iter().chain(&inputs.project.env) {
        if var.secret
            && let Some(value) = env.get(&var.key)
        {
            secrets.push(value.clone());
        }
    }
    for index in inputs
        .config
        .indexes
        .iter()
        .filter(|i| inputs.project.index_names.contains(&i.name))
    {
        if let Some(value) = env.get(&index_credential_var(&index.name, "PASSWORD")) {
            secrets.push(value.clone());
        }
    }
    secrets
}

/// Replace every occurrence of a known secret value with `***`. Duplicated
/// from the private `envspec::redact` for the same reason `venv_bin_dir` is:
/// one small call site here, kept out of another task's file.
fn redact(mut text: String, secrets: &[String]) -> String {
    for secret in secrets {
        if secret.is_empty() {
            continue;
        }
        text = text.replace(secret.as_str(), "***");
    }
    text
}

/// Open a terminal running the composed environment for `inputs`.
///
/// Composes the environment, derives the shell family from `shell`, writes a
/// re-assertion init file into `scratch_dir` when the family is known, builds
/// the terminal's argv, and spawns it with `clear_env = true` so the
/// composition is authoritative rather than layered on top of whatever
/// launched Bombadil. Returns as soon as the terminal is launched -- an
/// interactive terminal exits when the user closes it, so nothing here waits
/// on the child, and a successful spawn says nothing about whether the
/// terminal actually opened.
///
/// `shell` is a parameter rather than read from `$SHELL`: doing that here
/// would make every test depend on the machine running it. The GUI passes
/// `$SHELL`.
pub fn open(
    choice: &TerminalChoice,
    os: Os,
    inputs: &EnvInputs,
    shell: &str,
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
    scratch_dir: &Path,
) -> Result<(), EnvError> {
    let env = compose(inputs, store, runner)?;

    let family = ShellFamily::detect(shell);
    let bin = venv_bin_dir(&inputs.venv_path);
    let init = init_file(family, &bin);

    let (shell_args, extra_env) = match &init {
        Some(f) => {
            // Every shell family's relative_path is a fixed name
            // (bombadil-bashrc, .zshrc, ...), so joining it straight under
            // scratch_dir would let a second `open` for a DIFFERENT project
            // overwrite the first project's init file before its shell has
            // read it -- the first terminal then activates the wrong
            // project's environment. Namespacing by project id keeps
            // concurrent opens for different projects from colliding; a
            // second open for the *same* project id still overwrites, which
            // is fine, since it is the same venv path either way.
            let dir = scratch_dir.join(inputs.project.id.to_string());
            std::fs::create_dir_all(&dir).map_err(|source| EnvError::TerminalInitFile {
                path: dir.clone(),
                message: source.to_string(),
            })?;
            let file_path = dir.join(&f.relative_path);
            std::fs::write(&file_path, &f.contents).map_err(|source| {
                EnvError::TerminalInitFile {
                    path: file_path.clone(),
                    message: source.to_string(),
                }
            })?;

            let file_str = file_path.to_string_lossy().into_owned();
            let dir_str = dir.to_string_lossy().into_owned();

            let shell_args = substitute(f.shell_args.iter(), &file_str, &dir_str);
            let extra_env: BTreeMap<String, String> = f
                .env
                .keys()
                .cloned()
                .zip(substitute(f.env.values(), &file_str, &dir_str))
                .collect();
            (shell_args, extra_env)
        }
        None => (Vec::new(), BTreeMap::new()),
    };

    let cwd = project_dir(inputs);
    let mut spec = argv::build(choice, os, &cwd, shell, &shell_args)?;

    let mut full_env = env.clone();
    full_env.extend(extra_env);
    spec.env = full_env;
    spec.clear_env = true;

    let argv_line = format!(
        "{}{}{}",
        spec.program.display(),
        if spec.args.is_empty() { "" } else { " " },
        spec.args.join(" ")
    );

    // No live path puts a secret into `spec.args` today, but a custom
    // terminal template is exactly the kind of thing that could later
    // interpolate an environment value into argv -- redact both fields with
    // the same set, not just `message`.
    let secrets = secret_values(inputs, &env);
    // `spawn`, never `run`: `run` waits for the child, and a terminal
    // emulator lives until the user closes its window. Only gnome-terminal
    // and konsole self-daemonize; kitty, alacritty, foot, xterm, wezterm's
    // first window and every Windows console kind would hold the GUI hostage
    // for the terminal's whole lifetime. Nothing here reads the outcome
    // anyway.
    runner
        .spawn(&spec)
        .map_err(|source| EnvError::TerminalSpawn {
            argv: redact(argv_line, &secrets),
            message: redact(source.to_string(), &secrets),
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::envspec::EnvInputs;
    use crate::model::{Config, EnvVar, Project, VenvLocation};
    use crate::process::FakeCommandRunner;
    use crate::secrets::InMemorySecretStore;
    use crate::terminal::TerminalKind;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use uuid::Uuid;

    /// The program `envspec::script::dump_command` builds on this platform.
    /// Duplicated from `envspec::script`'s own test-only `shell()` helper
    /// rather than exposed from there, to keep that file unchanged for other
    /// tasks.
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

    fn inputs(project: Project) -> EnvInputs {
        EnvInputs {
            parent: BTreeMap::from([("PATH".to_string(), "/usr/bin".to_string())]),
            config: Config::default(),
            project,
            venv_path: PathBuf::from("/envs/my-api"),
        }
    }

    fn opened(shell: &str, project: Project) -> Vec<crate::process::CommandSpec> {
        let store = InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();
        // The terminal-emulator spawn itself: an unprogrammed FakeCommandRunner
        // reports every call as a failure, which is exactly what test 5 below
        // exercises deliberately (Kitty, left unprogrammed). These tests are
        // about the composed CommandSpec's contents, not about spawn failure,
        // so gnome-terminal is programmed to succeed here.
        runner.expect(
            "gnome-terminal",
            &[],
            crate::process::CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        let tmp = tempfile::tempdir().unwrap();
        open(
            &TerminalChoice::Detected(TerminalKind::GnomeTerminal),
            Os::Linux,
            &inputs(project),
            shell,
            &store,
            &runner,
            tmp.path(),
        )
        .expect("opening must succeed");
        runner.calls()
    }

    #[test]
    fn the_terminal_receives_the_composed_environment_and_nothing_ambient() {
        // clear_env is what makes the composition authoritative; without it the
        // shell that launched Bombadil leaks in underneath.
        let p = Project {
            env: vec![EnvVar {
                key: "MARKER".into(),
                value: "set".into(),
                secret: false,
            }],
            ..project()
        };
        let calls = opened("/bin/bash", p);
        assert_eq!(calls.len(), 1);
        assert!(calls[0].clear_env, "the composition must be authoritative");
        assert_eq!(calls[0].env.get("MARKER").map(String::as_str), Some("set"));
        assert_eq!(
            calls[0].env.get("VIRTUAL_ENV").map(String::as_str),
            Some("/envs/my-api")
        );
    }

    #[test]
    fn the_working_directory_is_the_projects_directory_not_the_venvs() {
        // Spec section 8: the terminal opens where pyproject.toml lives.
        let calls = opened("/bin/bash", project());
        assert_eq!(calls[0].cwd, Some(PathBuf::from("/proj/my-api")));
    }

    #[test]
    fn a_known_shell_gets_the_reassertion_arguments() {
        // Task 2's --rcfile arguments are useless if they are dropped here:
        // activation would silently degrade to whatever the user's rc leaves.
        let calls = opened("/bin/bash", project());
        let args = calls[0].args.join(" ");
        assert!(args.contains("--rcfile"), "got {args}");
        assert!(args.contains("bombadil-bashrc"), "got {args}");
    }

    #[test]
    fn an_unknown_shell_still_opens_a_terminal() {
        // Losing the re-assertion is acceptable; refusing to open is not.
        let calls = opened("/usr/bin/nu", project());
        assert_eq!(calls.len(), 1);
        let args = calls[0].args.join(" ");
        assert!(!args.contains("--rcfile"), "got {args}");
        assert!(args.contains("/usr/bin/nu"), "got {args}");
    }

    #[test]
    fn a_terminal_that_has_disappeared_reports_the_exact_argv_attempted() {
        // Spec section 9. We cannot pass --wait, so the exit status tells us
        // nothing; a missing emulator surfaces as a spawn failure, and the
        // user needs the argv to see what was actually tried.
        let store = InMemorySecretStore::new();
        let runner = FakeCommandRunner::new(); // nothing programmed -> spawn fails
        let tmp = tempfile::tempdir().unwrap();
        let err = open(
            &TerminalChoice::Detected(TerminalKind::Kitty),
            Os::Linux,
            &inputs(project()),
            "/bin/bash",
            &store,
            &runner,
            tmp.path(),
        )
        .unwrap_err();

        let rendered = err.to_string();
        assert!(
            rendered.contains("kitty"),
            "must name the program; got {rendered}"
        );
        assert!(
            rendered.contains("--directory") && rendered.contains("/proj/my-api"),
            "must name the argv, not just the program; got {rendered}"
        );
    }

    #[test]
    fn a_secret_never_appears_in_an_error_from_opening() {
        // Same constraint as compose: a failing pre-activate script must not
        // put a keychain value into a user-visible error.
        let store = InMemorySecretStore::new();
        store
            .set(
                &crate::secrets::SecretKey::ProjectEnv {
                    project_id: Uuid::from_u128(7),
                    key: "TOKEN".into(),
                },
                "s3cr3t-open-value",
            )
            .unwrap();
        let p = Project {
            env: vec![EnvVar {
                key: "TOKEN".into(),
                value: String::new(),
                secret: true,
            }],
            scripts: vec![crate::model::Script {
                path: PathBuf::from("/p/pre.sh"),
                args: vec![],
                enabled: true,
            }],
            ..project()
        };
        let runner = FakeCommandRunner::new();
        runner.expect(
            shell(),
            &[],
            crate::process::CommandOutcome {
                status: Some(1),
                stdout: String::new(),
                stderr: "+ curl -H 'Authorization: Bearer s3cr3t-open-value'".into(),
            },
        );
        let tmp = tempfile::tempdir().unwrap();
        let err = open(
            &TerminalChoice::Detected(TerminalKind::GnomeTerminal),
            Os::Linux,
            &inputs(p),
            "/bin/bash",
            &store,
            &runner,
            tmp.path(),
        )
        .unwrap_err();

        let rendered = err.to_string();
        assert!(!rendered.contains("s3cr3t-open-value"), "got {rendered}");
        assert!(rendered.contains("***"), "got {rendered}");
    }

    fn success_outcome() -> crate::process::CommandOutcome {
        crate::process::CommandOutcome {
            status: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn extract_flag_value(spec: &crate::process::CommandSpec, flag: &str) -> String {
        let idx = spec
            .args
            .iter()
            .position(|a| a == flag)
            .unwrap_or_else(|| panic!("{flag} not found in {:?}", spec.args));
        spec.args[idx + 1].clone()
    }

    #[test]
    fn two_projects_do_not_share_an_init_file_even_under_the_same_scratch_dir() {
        // A fixed filename per shell family (bombadil-bashrc, .zshrc, ...)
        // joined straight under scratch_dir would let a second `open` for a
        // DIFFERENT project overwrite the first project's init file before
        // its shell has read it -- the first terminal would then activate
        // the wrong project's environment. Same class of silent
        // wrong-environment bug this whole plan exists to prevent.
        let store = InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();
        runner.expect("gnome-terminal", &[], success_outcome());
        let tmp = tempfile::tempdir().unwrap();

        let ins_a = EnvInputs {
            parent: BTreeMap::from([("PATH".to_string(), "/usr/bin".to_string())]),
            config: Config::default(),
            project: Project {
                id: Uuid::from_u128(1),
                ..project()
            },
            venv_path: PathBuf::from("/envs/project-a"),
        };
        let ins_b = EnvInputs {
            parent: BTreeMap::from([("PATH".to_string(), "/usr/bin".to_string())]),
            config: Config::default(),
            project: Project {
                id: Uuid::from_u128(2),
                ..project()
            },
            venv_path: PathBuf::from("/envs/project-b"),
        };

        for ins in [&ins_a, &ins_b] {
            open(
                &TerminalChoice::Detected(TerminalKind::GnomeTerminal),
                Os::Linux,
                ins,
                "/bin/bash",
                &store,
                &runner,
                tmp.path(),
            )
            .expect("opening must succeed");
        }

        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        let path_a = extract_flag_value(&calls[0], "--rcfile");
        let path_b = extract_flag_value(&calls[1], "--rcfile");
        assert_ne!(
            path_a, path_b,
            "two different projects must not share an init file path"
        );

        let contents_a = std::fs::read_to_string(&path_a).unwrap();
        let contents_b = std::fs::read_to_string(&path_b).unwrap();
        assert!(contents_a.contains("/envs/project-a"), "got {contents_a}");
        assert!(!contents_a.contains("/envs/project-b"), "got {contents_a}");
        assert!(contents_b.contains("/envs/project-b"), "got {contents_b}");
        assert!(!contents_b.contains("/envs/project-a"), "got {contents_b}");
    }

    #[test]
    fn zsh_gets_zdotdir_pointing_at_a_real_directory_not_the_placeholder() {
        // {dir} is never exercised by the bash-only tests above; a
        // regression isolated to the {dir} substitution would leave zsh
        // users with a literal "{dir}" ZDOTDIR, which sources nothing --
        // silently losing activation rather than failing loudly.
        let store = InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();
        runner.expect("gnome-terminal", &[], success_outcome());
        let tmp = tempfile::tempdir().unwrap();
        let ins = inputs(project());

        open(
            &TerminalChoice::Detected(TerminalKind::GnomeTerminal),
            Os::Linux,
            &ins,
            "/bin/zsh",
            &store,
            &runner,
            tmp.path(),
        )
        .expect("opening must succeed");

        let calls = runner.calls();
        let zdotdir = calls[0].env.get("ZDOTDIR").expect("ZDOTDIR must be set");
        assert!(!zdotdir.contains("{dir}"), "got {zdotdir}");
        let expected = tmp.path().join(ins.project.id.to_string());
        assert_eq!(PathBuf::from(zdotdir), expected);
    }

    #[test]
    fn posix_sh_gets_env_pointing_at_a_real_file_not_the_placeholder() {
        let store = InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();
        runner.expect("gnome-terminal", &[], success_outcome());
        let tmp = tempfile::tempdir().unwrap();
        let ins = inputs(project());

        open(
            &TerminalChoice::Detected(TerminalKind::GnomeTerminal),
            Os::Linux,
            &ins,
            "/bin/sh",
            &store,
            &runner,
            tmp.path(),
        )
        .expect("opening must succeed");

        let calls = runner.calls();
        let env_var = calls[0].env.get("ENV").expect("ENV must be set");
        assert!(!env_var.contains("{file}"), "got {env_var}");
        let expected = tmp
            .path()
            .join(ins.project.id.to_string())
            .join("bombadil-env.sh");
        assert_eq!(PathBuf::from(env_var), expected);
    }

    #[test]
    fn fish_substitutes_file_in_the_middle_of_an_argument() {
        // fish's shell_args carries "{file}" embedded inside "source {file}",
        // not as a standalone argument -- an equality-based substitution
        // would silently leave it untouched.
        let store = InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();
        runner.expect("gnome-terminal", &[], success_outcome());
        let tmp = tempfile::tempdir().unwrap();
        let ins = inputs(project());

        open(
            &TerminalChoice::Detected(TerminalKind::GnomeTerminal),
            Os::Linux,
            &ins,
            "/usr/local/bin/fish",
            &store,
            &runner,
            tmp.path(),
        )
        .expect("opening must succeed");

        let calls = runner.calls();
        let args = calls[0].args.join(" ");
        assert!(!args.contains("{file}"), "got {args}");
        let expected_file = tmp
            .path()
            .join(ins.project.id.to_string())
            .join("bombadil.fish");
        assert!(
            args.contains(&format!("source {}", expected_file.display())),
            "got {args}"
        );
    }
}
