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
//! - **The interpreter itself**: written to disk before the launch, by
//!   [`configure_interpreter`]. There is no CLI flag for it. PyCharm records
//!   a project's SDK in `.idea/misc.xml` *by name*, and that name has to
//!   resolve in a `jdk.table.xml` under the IDE's own versioned config
//!   directory. PyCharm holds that file in memory and rewrites it wholesale
//!   when it exits, so while an IDE is running only an entry that already
//!   names the venv is reused. Otherwise the step reports what to set by
//!   hand, and the launch goes ahead. A new entry carries empty classpath
//!   roots, and PyCharm fills them in itself on first use.

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
            home.join("Applications/PyCharm.app/Contents/MacOS/pycharm"),
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

// --- interpreter ---

/// `JetBrains` under this user's config directory, where every IDE keeps a
/// versioned folder (`PyCharm2026.2`, ...). Reads the environment, so it is
/// called at the GUI's boundary, never from a test.
pub fn jetbrains_config_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.config_dir().join("JetBrains"))
}

/// Whether a PyCharm is running right now. Only `Some(1)` from `pgrep` --
/// "nothing matched" -- counts as not running: a probe that failed must not
/// license a write the running IDE would throw away on exit.
// ponytail: matches the process name only; a java-launched PyCharm (old pycharm.sh) isn't detected.
pub fn is_running(os: Os, runner: &dyn CommandRunner) -> bool {
    match os {
        Os::Windows => {
            let spec =
                CommandSpec::new("tasklist").args(["/FI", "IMAGENAME eq pycharm64.exe", "/NH"]);
            runner
                .run(&spec)
                .map(|o| o.stdout.contains("pycharm64.exe"))
                .unwrap_or(true)
        }
        _ => {
            let spec = CommandSpec::new("pgrep").args(["-x", "pycharm"]);
            runner
                .run(&spec)
                .map(|o| o.status != Some(1))
                .unwrap_or(true)
        }
    }
}

/// Point the project's PyCharm interpreter at the environment in `inputs`:
/// register the venv's python in `jdk.table.xml` (reusing an entry that
/// already names it) and select it by name in `.idea/misc.xml`.
///
/// The module doc's caveat still holds for a *running* IDE -- it rewrites
/// `jdk.table.xml` from memory on exit -- so with `running` set only an
/// existing entry is reused. `misc.xml` is per project and read on open, so
/// it is always safe to write before the launch.
///
/// The error is a user-facing sentence ending in what to do by hand.
pub fn configure_interpreter(
    inputs: &EnvInputs,
    jetbrains: &Path,
    home: &Path,
    running: bool,
) -> Result<(), String> {
    let exe = if cfg!(windows) {
        "python.exe"
    } else {
        "python"
    };
    let python = crate::envspec::venv_bin_dir(&inputs.venv_path).join(exe);
    let manual = format!("set the interpreter to {} by hand", python.display());

    let config = latest_pycharm_dir(jetbrains).ok_or_else(|| {
        format!(
            "no PyCharm settings under {}; {manual}",
            jetbrains.display()
        )
    })?;
    let table = config.join("options").join("jdk.table.xml");

    let sdk = if running {
        let content = std::fs::read_to_string(&table).unwrap_or_default();
        find_sdk(&content, &python, home)
            .ok_or_else(|| format!("PyCharm is already running; {manual}"))?
    } else {
        let base = format!("Bombadil ({})", inputs.project.label);
        ensure_sdk(&table, &base, &python, home).map_err(|e| format!("{e}; {manual}"))?
    };

    let misc = project_dir(inputs).join(".idea").join("misc.xml");
    set_project_sdk(&misc, &sdk).map_err(|e| format!("{e}; {manual}"))
}

/// The newest `PyCharm<version>` / `PyCharmCE<version>` folder.
fn latest_pycharm_dir(jetbrains: &Path) -> Option<PathBuf> {
    std::fs::read_dir(jetbrains)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let version = name
                .strip_prefix("PyCharm")?
                .trim_start_matches(char::is_alphabetic);
            let parts: Vec<u32> = version
                .split('.')
                .map(|p| p.parse().ok())
                .collect::<Option<_>>()?;
            Some((parts, entry.path()))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, path)| path)
}

const EMPTY_TABLE: &str =
    "<application>\n  <component name=\"ProjectJdkTable\">\n  </component>\n</application>\n";

/// The value of the first `<tag value="..." />` in `block`.
fn tag_value<'a>(block: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag} value=\"");
    let start = block.find(&open)? + open.len();
    let len = block[start..].find('"')?;
    Some(&block[start..start + len])
}

fn jdk_blocks(content: &str) -> impl Iterator<Item = &str> {
    content
        .split("<jdk ")
        .skip(1)
        .map(|b| b.split("</jdk>").next().unwrap_or(b))
}

/// The name of an existing SDK whose interpreter is `python`.
fn find_sdk(content: &str, python: &Path, home: &Path) -> Option<String> {
    // PyCharm writes '/' on Windows too, and abbreviates the home directory.
    let norm = |p: &str| p.replace('\\', "/");
    let python = norm(&python.to_string_lossy());
    let home = norm(&home.to_string_lossy());
    jdk_blocks(content).find_map(|b| {
        let path = norm(&xml_unescape(tag_value(b, "homePath")?)).replace("$USER_HOME$", &home);
        (path == python).then(|| tag_value(b, "name").map(xml_unescape))?
    })
}

/// The SDK name for `python`, adding an entry to the table if there is none.
fn ensure_sdk(table: &Path, base: &str, python: &Path, home: &Path) -> Result<String, String> {
    let content = match std::fs::read_to_string(table) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => EMPTY_TABLE.to_string(),
        Err(e) => return Err(format!("cannot read {}: {e}", table.display())),
    };
    if let Some(name) = find_sdk(&content, python, home) {
        return Ok(name);
    }

    // SDK names are unique; PyCharm drops a duplicate on load.
    let taken: Vec<String> = jdk_blocks(&content)
        .filter_map(|b| tag_value(b, "name").map(xml_unescape))
        .collect();
    let mut name = base.to_string();
    let mut n = 2;
    while taken.contains(&name) {
        name = format!("{base} {n}");
        n += 1;
    }

    let bad = || format!("unrecognised format in {}", table.display());
    let component = content.find("name=\"ProjectJdkTable\"").ok_or_else(bad)?;
    let close = component + content[component..].find("</component>").ok_or_else(bad)?;
    // At the start of the closing tag's line, so the indentation stays tidy.
    let at = content[..close].rfind('\n').map_or(close, |i| i + 1);

    let entry = format!(
        r#"    <jdk version="2">
      <name value="{name}" />
      <type value="Python SDK" />
      <homePath value="{path}" />
      <roots>
        <classPath>
          <root type="composite" />
        </classPath>
        <sourcePath>
          <root type="composite" />
        </sourcePath>
      </roots>
      <additional>
        <setting name="FLAVOR_ID" value="VirtualEnvSdkFlavor" />
        <setting name="FLAVOR_DATA" value="{{}}" />
      </additional>
    </jdk>
"#,
        name = xml_escape(&name),
        path = xml_escape(&python.to_string_lossy()),
    );

    let mut updated = content;
    updated.insert_str(at, &entry);
    if let Some(dir) = table.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(table, updated).map_err(|e| format!("cannot write {}: {e}", table.display()))?;
    Ok(name)
}

/// Create or patch `misc.xml` so the project uses the SDK named `sdk`,
/// leaving every other component in it alone.
fn set_project_sdk(misc: &Path, sdk: &str) -> Result<(), String> {
    let line = format!(
        r#"<component name="ProjectRootManager" version="2" project-jdk-name="{}" project-jdk-type="Python SDK" />"#,
        xml_escape(sdk)
    );

    // `.idea` comes from the repository, so either it or `misc.xml` may be a
    // committed symlink; writing through one would rewrite a file outside
    // the project. Refused rather than followed.
    // ponytail: checked then written, not atomic -- a race needs a local
    // attacker already able to write the project.
    for path in [misc.parent(), Some(misc)].into_iter().flatten() {
        if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!("{} is a symlink", path.display()));
        }
    }

    let updated = match std::fs::read_to_string(misc) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<project version=\"4\">\n  {line}\n</project>\n"
        ),
        Err(e) => return Err(format!("cannot read {}: {e}", misc.display())),
        Ok(content) => {
            let bad = || format!("unrecognised format in {}", misc.display());
            if let Some(start) = content.find("<component name=\"ProjectRootManager\"") {
                let gt = start + content[start..].find('>').ok_or_else(bad)?;
                let end = if content[..gt].ends_with('/') {
                    gt + 1
                } else {
                    gt + content[gt..].find("</component>").ok_or_else(bad)? + "</component>".len()
                };
                format!("{}{line}{}", &content[..start], &content[end..])
            } else {
                let close = content.rfind("</project>").ok_or_else(bad)?;
                format!("{}  {line}\n{}", &content[..close], &content[close..])
            }
        }
    };

    if let Some(dir) = misc.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(misc, updated).map_err(|e| format!("cannot write {}: {e}", misc.display()))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
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

    // --- interpreter ---

    const HOME: &str = "/home/u";

    /// A JetBrains dir holding one PyCharm config, and a project whose venv
    /// sits beside its manifest -- both in a scratch directory.
    fn scratch() -> (tempfile::TempDir, PathBuf, EnvInputs) {
        let dir = tempfile::tempdir().unwrap();
        let jetbrains = dir.path().join("JetBrains");
        std::fs::create_dir_all(jetbrains.join("PyCharm2026.2")).unwrap();
        let mut inputs = inputs();
        inputs.project.pyproject_path = dir.path().join("api/pyproject.toml");
        inputs.venv_path = dir.path().join("api/.venv");
        (dir, jetbrains, inputs)
    }

    #[test]
    fn the_interpreter_is_registered_and_selected_for_the_project() {
        let (dir, jetbrains, inputs) = scratch();
        configure_interpreter(&inputs, &jetbrains, Path::new(HOME), false).unwrap();

        let table = std::fs::read_to_string(jetbrains.join("PyCharm2026.2/options/jdk.table.xml"))
            .expect("the SDK must be registered");
        assert!(
            table.contains(r#"<name value="Bombadil (api)" />"#),
            "{table}"
        );
        let misc = std::fs::read_to_string(dir.path().join("api/.idea/misc.xml"))
            .expect("the project must select it");
        assert!(
            misc.contains(r#"project-jdk-name="Bombadil (api)""#),
            "{misc}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_idea_file_is_never_written_through() {
        // A cloned repo can commit `.idea/misc.xml` (or `.idea`) as a symlink
        // to another file of the user's; following it would rewrite that
        // file instead.
        let (dir, jetbrains, inputs) = scratch();
        let victim = dir.path().join("victim.xml");
        let original = "<project version=\"4\">\n</project>\n";
        std::fs::write(&victim, original).unwrap();
        std::fs::create_dir_all(dir.path().join("api/.idea")).unwrap();
        std::os::unix::fs::symlink(&victim, dir.path().join("api/.idea/misc.xml")).unwrap();

        let err = configure_interpreter(&inputs, &jetbrains, Path::new(HOME), false)
            .expect_err("a symlinked misc.xml is refused");
        assert!(err.contains("by hand"), "{err}");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), original);

        // The same through a symlinked `.idea` directory.
        let (dir, jetbrains, inputs) = scratch();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(dir.path().join("api")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.path().join("api/.idea")).unwrap();
        configure_interpreter(&inputs, &jetbrains, Path::new(HOME), false)
            .expect_err("a symlinked .idea is refused");
        assert!(!elsewhere.join("misc.xml").exists());
    }

    #[test]
    fn a_running_pycharm_table_is_never_written() {
        // It would be overwritten from memory when the IDE exits, leaving
        // misc.xml naming an SDK that no longer exists.
        let (dir, jetbrains, inputs) = scratch();
        let err = configure_interpreter(&inputs, &jetbrains, Path::new(HOME), true)
            .expect_err("nothing to reuse while PyCharm is running");

        assert!(
            err.contains("by hand"),
            "the user must be told what to do: {err}"
        );
        assert!(
            !jetbrains
                .join("PyCharm2026.2/options/jdk.table.xml")
                .exists()
        );
        assert!(!dir.path().join("api/.idea/misc.xml").exists());
    }

    #[test]
    fn no_pycharm_settings_is_a_warning_naming_where_it_looked() {
        let (_dir, jetbrains, inputs) = scratch();
        std::fs::remove_dir(jetbrains.join("PyCharm2026.2")).unwrap();
        let err = configure_interpreter(&inputs, &jetbrains, Path::new(HOME), false).unwrap_err();
        assert!(err.contains("JetBrains"), "{err}");
    }

    #[test]
    fn an_existing_sdk_is_reused_not_duplicated() {
        let dir = tempfile::tempdir().unwrap();
        let table = dir.path().join("options/jdk.table.xml");
        let py = Path::new("/venvs/a & b/bin/python");

        assert_eq!(
            ensure_sdk(&table, "Bombadil (a)", py, Path::new(HOME)).unwrap(),
            "Bombadil (a)"
        );
        let content = std::fs::read_to_string(&table).unwrap();
        assert!(content.contains(r#"<homePath value="/venvs/a &amp; b/bin/python" />"#));

        assert_eq!(
            ensure_sdk(&table, "other", py, Path::new(HOME)).unwrap(),
            "Bombadil (a)"
        );
        assert_eq!(std::fs::read_to_string(&table).unwrap(), content);
    }

    #[test]
    fn user_home_macro_matches_and_taken_names_get_a_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let table = dir.path().join("jdk.table.xml");
        std::fs::write(
            &table,
            r#"<application>
  <component name="ProjectJdkTable">
    <jdk version="2">
      <name value="uv (api)" />
      <homePath value="$USER_HOME$/api/.venv/bin/python" />
    </jdk>
    <jdk version="2">
      <name value="Bombadil (x)" />
      <homePath value="/elsewhere/bin/python" />
    </jdk>
  </component>
</application>
"#,
        )
        .unwrap();

        let existing = Path::new("/home/u/api/.venv/bin/python");
        assert_eq!(
            ensure_sdk(&table, "n", existing, Path::new(HOME)).unwrap(),
            "uv (api)"
        );

        let new = Path::new("/venvs/x/bin/python");
        assert_eq!(
            ensure_sdk(&table, "Bombadil (x)", new, Path::new(HOME)).unwrap(),
            "Bombadil (x) 2"
        );
        assert_eq!(
            std::fs::read_to_string(&table)
                .unwrap()
                .matches("<jdk ")
                .count(),
            3
        );
    }

    #[test]
    fn windows_separators_still_match() {
        let table = r#"<jdk version="2"><name value="w" /><homePath value="$USER_HOME$/p/.venv/Scripts/python.exe" /></jdk>"#;
        let py = Path::new(r"C:\Users\u\p\.venv\Scripts\python.exe");
        assert_eq!(
            find_sdk(table, py, Path::new(r"C:\Users\u")).as_deref(),
            Some("w")
        );
    }

    #[test]
    fn an_unrecognised_table_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let table = dir.path().join("jdk.table.xml");
        std::fs::write(&table, "garbage").unwrap();
        assert!(ensure_sdk(&table, "n", Path::new("/p"), Path::new(HOME)).is_err());
        assert_eq!(std::fs::read_to_string(&table).unwrap(), "garbage");
    }

    #[test]
    fn misc_xml_keeps_its_other_components() {
        let dir = tempfile::tempdir().unwrap();
        let misc = dir.path().join("misc.xml");
        std::fs::write(
            &misc,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<project version="4">
  <component name="Black">
    <option name="sdkName" value="old" />
  </component>
  <component name="ProjectRootManager" version="2" project-jdk-name="old" project-jdk-type="Python SDK" />
</project>
"#,
        )
        .unwrap();
        set_project_sdk(&misc, "new").unwrap();
        let content = std::fs::read_to_string(&misc).unwrap();
        assert!(content.contains(r#"project-jdk-name="new""#));
        assert!(!content.contains(r#"project-jdk-name="old""#));
        assert!(content.contains(r#"<component name="Black">"#));
        assert_eq!(content.matches("ProjectRootManager").count(), 1);
    }

    #[test]
    fn misc_xml_without_the_component_gets_one() {
        let dir = tempfile::tempdir().unwrap();
        let misc = dir.path().join("misc.xml");
        std::fs::write(&misc, "<project version=\"4\">\n</project>\n").unwrap();
        set_project_sdk(&misc, "new").unwrap();
        let content = std::fs::read_to_string(&misc).unwrap();
        assert!(content.contains("ProjectRootManager"));
        assert!(content.ends_with("</project>\n"));
    }

    #[test]
    fn the_newest_pycharm_settings_win() {
        let dir = tempfile::tempdir().unwrap();
        for d in [
            "PyCharm2025.3",
            "PyCharm2026.2",
            "PyCharmCE2026.1",
            "PyCharm2026.10x",
            "CLion2027.1",
        ] {
            std::fs::create_dir(dir.path().join(d)).unwrap();
        }
        let got = latest_pycharm_dir(dir.path()).unwrap();
        assert_eq!(got.file_name().unwrap(), "PyCharm2026.2");
    }

    #[test]
    fn a_failed_running_probe_assumes_running() {
        // Assuming "not running" would license a write the IDE discards.
        let runner = FakeCommandRunner::new();
        assert!(is_running(Os::Linux, &runner));
        runner.expect(
            "pgrep",
            &["-x", "pycharm"],
            CommandOutcome {
                status: Some(1),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        assert!(!is_running(Os::Linux, &runner));
    }
}
