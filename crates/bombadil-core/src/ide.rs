//! Opening a project in PyCharm with Bombadil's composed environment.
//!
//! **The composed environment carries secrets**, exactly as `terminal::open`
//! does. Nothing here may log a [`CommandSpec`]; every error that can carry
//! free text is redacted before it reaches a caller.
//!
//! # What can and cannot be set from out here
//!
//! Three things were asked of this: open the project folder, use the
//! project's configured virtual environment as the interpreter, and inherit
//! the environment variables. Two of them are launcher arguments. The third
//! is not, and pretending otherwise would be worse than saying so.
//!
//! - **Project folder**: an argument. `pycharm <dir>` opens that directory as
//!   the project, which is what [`open`] passes.
//! - **Environment variables**: inherited. A process started by PyCharm gets
//!   PyCharm's own environment, so composing ours into the launcher's
//!   environment is what puts them in the IDE's terminal and in every run
//!   configuration that does not override them. `VIRTUAL_ENV` and the venv's
//!   `bin` at the front of `PATH` are part of that composition, so anything
//!   the IDE spawns resolves `python` to the configured environment.
//! - **The interpreter itself**: not settable from out here. PyCharm records
//!   a project's SDK in `.idea/misc.xml` *by name*, and that name has to
//!   resolve in a `jdk.table.xml` under the IDE's own versioned config
//!   directory -- a file PyCharm holds in memory and rewrites wholesale when
//!   it exits, so anything written into it behind a running IDE's back is
//!   discarded. The entries also carry resolved classpath roots pointing at
//!   stub caches the IDE generates itself. There is no CLI flag for it: the
//!   JetBrains launcher takes a path, `diff`, `merge`, `format` and
//!   `inspect`, and nothing else.
//!
//!   In practice the gap is smaller than it sounds. PyCharm detects a venv
//!   sitting in the project directory on first open, which is where Bombadil
//!   puts one by default; for an environment kept elsewhere the user picks it
//!   once, and PyCharm remembers it per project from then on.

use crate::envspec::{EnvInputs, compose};
use crate::error::EnvError;
use crate::process::{CommandRunner, CommandSpec};
use crate::secrets::SecretStore;
use crate::terminal::Os;
use crate::terminal::open::secret_values;
use std::path::{Path, PathBuf};

/// Where a JetBrains launcher is looked for, in the order it is tried.
///
/// `home` is a parameter rather than read from the environment in here, for
/// the reason the whole `terminal` module states about [`Os`]: it is the only
/// thing that lets macOS and Windows layouts be covered by tests that run on
/// Linux CI.
///
/// The Toolbox script comes first on every platform. It is the install most
/// people have, it is the one that survives an IDE upgrade -- Toolbox
/// rewrites the script to point at the new build -- and it is regularly *not*
/// on `PATH`, which is why looking it up by name alone is not enough.
pub fn candidates(os: Os, home: &Path) -> Vec<PathBuf> {
    match os {
        Os::Linux => vec![
            home.join(".local/share/JetBrains/Toolbox/scripts/pycharm"),
            PathBuf::from("/snap/bin/pycharm-professional"),
            PathBuf::from("/snap/bin/pycharm-community"),
            PathBuf::from("/usr/local/bin/pycharm"),
            PathBuf::from("/opt/pycharm/bin/pycharm.sh"),
        ],
        Os::MacOs => vec![
            home.join("Library/Application Support/JetBrains/Toolbox/scripts/pycharm"),
            PathBuf::from("/Applications/PyCharm.app/Contents/MacOS/pycharm"),
            PathBuf::from("/Applications/PyCharm CE.app/Contents/MacOS/pycharm"),
        ],
        Os::Windows => vec![
            home.join("AppData/Local/JetBrains/Toolbox/scripts/pycharm.cmd"),
            PathBuf::from(r"C:\Program Files\JetBrains\PyCharm\bin\pycharm64.exe"),
        ],
    }
}

/// The launcher to run, or `None` when PyCharm cannot be found.
///
/// `exists` is injected so this is a pure decision over a filesystem the test
/// describes, rather than over the machine the test happens to run on.
///
/// `PATH` is consulted last, not first. A bare `pycharm` on `PATH` is usually
/// a symlink to one of the paths above anyway, and preferring the explicit
/// locations means the answer does not change with the shell Bombadil was
/// launched from.
pub fn detect(
    os: Os,
    home: &Path,
    exists: &dyn Fn(&Path) -> bool,
    runner: &dyn CommandRunner,
) -> Option<PathBuf> {
    if let Some(found) = candidates(os, home).into_iter().find(|p| exists(p)) {
        return Some(found);
    }
    on_path(os, runner)
}

/// Ask the OS whether a bare `pycharm` resolves. Never runs PyCharm itself:
/// `which`/`where` report, they do not launch -- the same rule
/// `terminal::detect` is built on, and for the same reason. Launching an IDE
/// to find out whether an IDE is installed would be unusable.
fn on_path(os: Os, runner: &dyn CommandRunner) -> Option<PathBuf> {
    let spec = match os {
        Os::Windows => CommandSpec::new("where").arg("pycharm"),
        _ => CommandSpec::new("which").arg("pycharm"),
    };
    let outcome = runner.run(&spec).ok()?;
    if !outcome.success() {
        return None;
    }
    // `where` can report several matches, one per line; take the first.
    let first = outcome.stdout.lines().next()?.trim();
    (!first.is_empty()).then(|| PathBuf::from(first))
}

/// Where `pyproject.toml` lives -- the directory PyCharm opens as the
/// project. Mirrors `terminal::open`'s own `project_dir`, including its
/// fallback for a rootless configured path that `venv_path::resolve` already
/// refuses to produce a venv for.
fn project_dir(inputs: &EnvInputs) -> PathBuf {
    inputs
        .project
        .pyproject_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| inputs.venv_path.clone())
}

/// Replace every occurrence of a known secret value with `***`. Duplicated
/// from the private `envspec::redact` for the same reason `terminal::open`
/// duplicates it: one call site, kept out of another module's file.
fn redact(mut text: String, secrets: &[String]) -> String {
    for secret in secrets {
        if secret.is_empty() {
            continue;
        }
        text = text.replace(secret.as_str(), "***");
    }
    text
}

/// Launch `launcher` on the project in `inputs`, carrying the composed
/// environment.
///
/// Returns as soon as the IDE is launched. An IDE lives until the user closes
/// it, so nothing here waits on the child, and a successful spawn says
/// nothing about whether PyCharm actually came up.
pub fn open(
    launcher: &Path,
    inputs: &EnvInputs,
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
) -> Result<(), EnvError> {
    let env = compose(inputs, store, runner)?;
    let dir = project_dir(inputs);

    let mut spec = CommandSpec::new(launcher).arg(dir.to_string_lossy().into_owned());
    spec.cwd = Some(dir.clone());
    // `clear_env`, so the composition is authoritative rather than layered on
    // top of whatever launched Bombadil. Safe for a windowed application
    // because `compose` starts from `EnvInputs::parent` -- DISPLAY,
    // WAYLAND_DISPLAY, DBUS_SESSION_BUS_ADDRESS and the rest of the session
    // are in the map it returns, not discarded by it.
    spec.env = env.clone();
    spec.clear_env = true;

    let argv_line = format!("{} {}", launcher.display(), dir.display());
    let secrets = secret_values(inputs, &env);

    // `spawn`, never `run`: `run` waits for the child, and PyCharm lives
    // until the user quits it -- waiting would hold the GUI hostage for the
    // whole editing session. The same rule, and the same reason, as
    // `terminal::open`.
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
    use crate::model::{Config, Environment, Project, VenvLocation};
    use crate::process::{CommandOutcome, FakeCommandRunner};
    use crate::secrets::InMemorySecretStore;
    use std::collections::BTreeMap;
    use uuid::Uuid;

    fn home() -> PathBuf {
        PathBuf::from("/home/user")
    }

    fn nothing_exists(_: &Path) -> bool {
        false
    }

    /// A runner that will let the PyCharm spawn through. `compose` also runs
    /// the shell dump when there are pre-activate scripts; there are none in
    /// these fixtures, so the launcher is the only call to programme.
    fn launcher_ready() -> FakeCommandRunner {
        let runner = FakeCommandRunner::new();
        runner.expect(
            "pycharm",
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        runner
    }

    #[test]
    fn the_toolbox_script_is_found_even_though_it_is_not_on_path() {
        // The install this was written against: JetBrains Toolbox generates
        // `~/.local/share/JetBrains/Toolbox/scripts/pycharm`, and does not put
        // that directory on PATH. A PATH-only probe finds nothing and the
        // button reads as broken on a machine that has PyCharm.
        let toolbox = home().join(".local/share/JetBrains/Toolbox/scripts/pycharm");
        let exists = |p: &Path| p == toolbox;
        // The runner has no `which` answer, so PATH offers nothing.
        let runner = FakeCommandRunner::new();

        assert_eq!(
            detect(Os::Linux, &home(), &exists, &runner),
            Some(toolbox),
            "the Toolbox script must be found without help from PATH"
        );
    }

    #[test]
    fn path_is_the_last_resort_not_the_first() {
        // Both are present. The explicit location must win, so the answer does
        // not change with the shell Bombadil happened to be launched from.
        let toolbox = home().join(".local/share/JetBrains/Toolbox/scripts/pycharm");
        let exists = |p: &Path| p == toolbox;
        let runner = FakeCommandRunner::new();
        runner.expect(
            "which",
            &["pycharm"],
            CommandOutcome {
                status: Some(0),
                stdout: "/usr/bin/pycharm\n".into(),
                stderr: String::new(),
            },
        );

        assert_eq!(detect(Os::Linux, &home(), &exists, &runner), Some(toolbox));
    }

    #[test]
    fn a_pycharm_only_on_path_is_still_found() {
        let runner = FakeCommandRunner::new();
        runner.expect(
            "which",
            &["pycharm"],
            CommandOutcome {
                status: Some(0),
                stdout: "/usr/bin/pycharm\n".into(),
                stderr: String::new(),
            },
        );

        assert_eq!(
            detect(Os::Linux, &home(), &nothing_exists, &runner),
            Some(PathBuf::from("/usr/bin/pycharm"))
        );
    }

    #[test]
    fn no_pycharm_anywhere_is_none_rather_than_a_guess() {
        // A guessed path spawns something that is not there and reports a
        // spawn failure, which reads as "Bombadil is broken" rather than
        // "PyCharm is not installed".
        let runner = FakeCommandRunner::new();
        assert_eq!(detect(Os::Linux, &home(), &nothing_exists, &runner), None);
    }

    #[test]
    fn a_failed_path_probe_means_absent_not_an_error() {
        let runner = FakeCommandRunner::new();
        runner.expect(
            "which",
            &["pycharm"],
            CommandOutcome {
                status: Some(1),
                stdout: String::new(),
                stderr: "not found".into(),
            },
        );
        assert_eq!(detect(Os::Linux, &home(), &nothing_exists, &runner), None);
    }

    #[test]
    fn a_successful_probe_with_no_output_is_not_an_empty_path() {
        // `which` exiting 0 while printing nothing would otherwise yield
        // `PathBuf::from("")`, which spawns the current directory.
        let runner = FakeCommandRunner::new();
        runner.expect(
            "which",
            &["pycharm"],
            CommandOutcome {
                status: Some(0),
                stdout: "  \n".into(),
                stderr: String::new(),
            },
        );
        assert_eq!(detect(Os::Linux, &home(), &nothing_exists, &runner), None);
    }

    #[test]
    fn detection_never_launches_pycharm() {
        // The rule `terminal::detect` is built on. A probe that started the
        // IDE would make the app unusable.
        for os in [Os::Linux, Os::MacOs, Os::Windows] {
            let runner = FakeCommandRunner::new();
            let _ = detect(os, &home(), &nothing_exists, &runner);
            for call in runner.calls() {
                let program = call.program.to_string_lossy().to_string();
                assert!(
                    program.contains("which") || program.contains("where"),
                    "detection must probe, not launch; got {program}"
                );
            }
        }
    }

    #[test]
    fn every_os_looks_for_the_toolbox_script_first() {
        // Toolbox is the install that survives an IDE upgrade -- it rewrites
        // the script to point at the new build, while a versioned path under
        // Program Files or /opt goes stale.
        for os in [Os::Linux, Os::MacOs, Os::Windows] {
            let first = candidates(os, &home())
                .first()
                .expect("every OS must have somewhere to look")
                .to_string_lossy()
                .to_string();
            assert!(
                first.contains("Toolbox"),
                "{os:?} looks somewhere else first: {first}"
            );
            assert!(
                first.starts_with(&home().to_string_lossy().to_string()),
                "{os:?}'s Toolbox path must be under the passed home, not a real one: {first}"
            );
        }
    }

    // --- open ---

    fn inputs() -> EnvInputs {
        let project = Project {
            id: Uuid::nil(),
            label: "api".into(),
            pyproject_path: PathBuf::from("/work/api/pyproject.toml"),
            environments: vec![Environment {
                location: VenvLocation::Alongside,
                ..Environment::default()
            }],
            ..Project::default()
        };
        EnvInputs {
            parent: BTreeMap::from([("DISPLAY".to_string(), ":0".to_string())]),
            config: Config::default(),
            project,
            venv_path: PathBuf::from("/work/api/.venv"),
        }
    }

    #[test]
    fn pycharm_is_opened_on_the_project_directory_not_the_manifest() {
        // `pycharm /work/api/pyproject.toml` opens a single file in an
        // editor window with no project at all.
        let runner = launcher_ready();
        open(
            Path::new("/opt/pycharm"),
            &inputs(),
            &InMemorySecretStore::new(),
            &runner,
        )
        .expect("a spawn with no configured failure must succeed");

        let calls = runner.calls();
        let call = calls.last().expect("PyCharm must actually be spawned");
        assert_eq!(call.program, PathBuf::from("/opt/pycharm"));
        assert_eq!(call.args, vec!["/work/api".to_string()]);
        assert_eq!(call.cwd, Some(PathBuf::from("/work/api")));
    }

    #[test]
    fn the_venv_leads_the_path_pycharm_inherits() {
        // The interpreter cannot be set from out here (see the module doc), so
        // this is what makes the configured environment the one the IDE's
        // terminal and run configurations actually resolve `python` to. If it
        // regresses, PyCharm silently uses the system interpreter.
        let runner = launcher_ready();
        open(
            Path::new("/opt/pycharm"),
            &inputs(),
            &InMemorySecretStore::new(),
            &runner,
        )
        .unwrap();

        let calls = runner.calls();
        let env = &calls.last().unwrap().env;
        assert_eq!(
            env.get("VIRTUAL_ENV").map(String::as_str),
            Some("/work/api/.venv")
        );
        let path = env.get("PATH").expect("PATH must be composed");
        let bin = if cfg!(windows) {
            r"/work/api/.venv\Scripts"
        } else {
            "/work/api/.venv/bin"
        };
        assert!(
            path.starts_with(bin),
            "the venv must lead PATH, or PyCharm resolves python elsewhere; got {path}"
        );
    }

    #[test]
    fn the_session_the_ide_needs_to_display_at_all_survives_clear_env() {
        // `clear_env` is set, so anything the windowing session needs has to
        // be in the composed map. Without DISPLAY the process starts and dies
        // with no window and no message.
        let runner = launcher_ready();
        open(
            Path::new("/opt/pycharm"),
            &inputs(),
            &InMemorySecretStore::new(),
            &runner,
        )
        .unwrap();

        let calls = runner.calls();
        let call = calls.last().unwrap();
        assert!(call.clear_env, "the composition must be authoritative");
        assert_eq!(call.env.get("DISPLAY").map(String::as_str), Some(":0"));
    }

    #[test]
    fn the_ide_is_spawned_and_never_waited_on() {
        // `run` would hold the GUI hostage for the whole editing session --
        // PyCharm exits when the user quits it, not when the project opens.
        // The default `spawn` delegates to `run`, so only a runner that counts
        // them separately can tell these apart.
        let runner = launcher_ready();
        open(
            Path::new("/opt/pycharm"),
            &inputs(),
            &InMemorySecretStore::new(),
            &runner,
        )
        .unwrap();

        assert_eq!(
            runner.blocking_calls(),
            0,
            "opening an IDE must not block on it"
        );
    }
}
