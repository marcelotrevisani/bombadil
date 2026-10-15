//! Showing a directory in the platform's file manager -- spec §8's "Open
//! folder".
//!
//! Non-destructive and tiny, but it follows the same two rules the terminal
//! module established, for the same reasons:
//!
//! * The target [`Os`] is a parameter, never a `cfg!`. That is what lets the
//!   macOS and Windows argv be covered by tests running on Linux.
//! * The launch goes through [`CommandRunner::spawn`], never `run`.
//!   `SystemCommandRunner::run` calls `output()`, which waits for the child to
//!   exit -- a file manager lives until the user closes its window, so `run`
//!   would hold the calling thread for that whole time. Nothing here reads the
//!   outcome anyway: `explorer.exe` famously exits non-zero even when it did
//!   open the window, so its status would be worse than useless.

use super::terminal::Os;
use crate::error::CommandError;
use crate::process::{CommandRunner, CommandSpec};
use std::path::Path;

/// Open `dir` in the file manager `os` provides.
///
/// The spawned process deliberately inherits the ambient environment --
/// `clear_env` stays false and nothing composed is attached. Two reasons, and
/// they point the same way: a desktop file manager needs the session
/// variables that reach Bombadil (`DISPLAY`, `WAYLAND_DISPLAY`,
/// `DBUS_SESSION_BUS_ADDRESS`) and simply fails to launch without them, and
/// there is no reason for a file manager to receive a project's environment
/// -- which would mean handing keychain secrets and index passwords to a
/// process that has no use for either.
pub fn open(os: Os, dir: &Path, runner: &dyn CommandRunner) -> Result<(), CommandError> {
    open_target(os, dir.to_string_lossy().into_owned(), runner)
}

/// Open `url` in the default browser. The same opener a folder goes through:
/// each of the three hands a URL to the browser the user chose.
pub fn open_url(os: Os, url: &str, runner: &dyn CommandRunner) -> Result<(), CommandError> {
    open_target(os, url.to_string(), runner)
}

fn open_target(os: Os, target: String, runner: &dyn CommandRunner) -> Result<(), CommandError> {
    let program = match os {
        Os::Linux => "xdg-open",
        Os::MacOs => "open",
        Os::Windows => "explorer",
    };
    runner.spawn(&CommandSpec::new(program).arg(target))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{CommandOutcome, FakeCommandRunner};

    fn opened(os: Os, program: &str) -> Vec<CommandSpec> {
        let runner = FakeCommandRunner::new();
        runner.expect(
            program,
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        open(os, Path::new("/proj/my-api"), &runner).expect("opening must succeed");
        runner.calls()
    }

    #[test]
    fn linux_opens_the_directory_with_xdg_open() {
        let calls = opened(Os::Linux, "xdg-open");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].program.to_string_lossy(), "xdg-open");
        assert_eq!(calls[0].args, vec!["/proj/my-api"]);
    }

    #[test]
    fn macos_opens_the_directory_with_open() {
        // Runs on Linux CI: `Os` is a parameter precisely so this is
        // exercised everywhere rather than compiled away on two of three
        // platforms.
        let calls = opened(Os::MacOs, "open");
        assert_eq!(calls[0].program.to_string_lossy(), "open");
        assert_eq!(calls[0].args, vec!["/proj/my-api"]);
    }

    #[test]
    fn windows_opens_the_directory_with_explorer() {
        let calls = opened(Os::Windows, "explorer");
        assert_eq!(calls[0].program.to_string_lossy(), "explorer");
        assert_eq!(calls[0].args, vec!["/proj/my-api"]);
    }

    #[test]
    fn every_os_gets_a_different_command() {
        // The mutation this exists against: one hardcoded program that
        // happens to be right on whichever platform CI runs. Three separate
        // per-OS tests can all be satisfied by a single wrong constant only
        // if it is right three times, which this makes explicit.
        let programs: Vec<String> = [
            (Os::Linux, "xdg-open"),
            (Os::MacOs, "open"),
            (Os::Windows, "explorer"),
        ]
        .into_iter()
        .map(|(os, program)| {
            opened(os, program)[0]
                .program
                .to_string_lossy()
                .into_owned()
        })
        .collect();
        let unique: std::collections::BTreeSet<&String> = programs.iter().collect();
        assert_eq!(unique.len(), 3, "got {programs:?}");
    }

    #[test]
    fn the_file_manager_inherits_the_session_environment_rather_than_a_composed_one() {
        // `clear_env` would strip DISPLAY / WAYLAND_DISPLAY /
        // DBUS_SESSION_BUS_ADDRESS and the file manager would not start at
        // all; attaching a composed environment would hand keychain secrets
        // and index passwords to a process with no use for them.
        let calls = opened(Os::Linux, "xdg-open");
        assert!(
            !calls[0].clear_env,
            "the file manager needs the ambient session environment"
        );
        assert!(
            calls[0].env.is_empty(),
            "no composed environment may be attached; got {:?}",
            calls[0].env.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn opening_a_folder_does_not_wait_for_the_file_manager_to_exit() {
        // `run` calls `output()`, which waits for the child; a file manager
        // lives until the user closes its window, so `run` would park the job
        // thread for that whole time. The default `spawn` delegates to `run`,
        // which is why this asserts on the counter rather than on `calls`.
        let runner = FakeCommandRunner::new();
        runner.expect(
            "xdg-open",
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        open(Os::Linux, Path::new("/proj/my-api"), &runner).unwrap();

        assert_eq!(
            runner.blocking_calls(),
            0,
            "the file manager must be spawned, not waited on"
        );
    }

    #[test]
    fn a_file_manager_that_will_not_launch_is_reported_rather_than_ignored() {
        // Nothing programmed on the fake -- the spawn fails. A headless or
        // minimal system with no xdg-open is a real case, and silently doing
        // nothing looks like a dead button.
        let runner = FakeCommandRunner::new();
        let err = open(Os::Linux, Path::new("/proj/my-api"), &runner).unwrap_err();
        assert!(err.to_string().contains("xdg-open"), "got {err}");
    }

    #[test]
    fn a_url_goes_to_the_same_opener_unchanged() {
        let runner = FakeCommandRunner::new();
        runner.expect(
            "xdg-open",
            &["https://github.com/sponsors/x"],
            CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        open_url(Os::Linux, "https://github.com/sponsors/x", &runner).unwrap();
        assert_eq!(
            runner.blocking_calls(),
            0,
            "a browser must not be waited on"
        );
    }
}
