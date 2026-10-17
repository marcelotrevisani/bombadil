mod fake;
mod system;

#[cfg(any(test, feature = "test-support"))]
pub use fake::FakeCommandRunner;
pub use system::SystemCommandRunner;

use crate::error::CommandError;

use std::collections::BTreeMap;
#[cfg(any(test, feature = "test-support"))]
use std::path::Path;
use std::path::PathBuf;

/// Everything needed to launch one process.
///
/// `BTreeMap` rather than `HashMap` so the environment is ordered: tests can
/// assert on it, and failure reports render the same way twice.
///
/// `Debug` is hand-written to print `env` as a count rather than its contents:
/// it now carries the whole composed environment, including keychain secrets
/// and private-index passwords, and specs get logged.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    /// When true the child starts from an empty environment and sees only
    /// `env`. Plan 2b composes a full environment and sets this.
    pub clear_env: bool,
}

impl std::fmt::Debug for CommandSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandSpec")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("env", &format_args!("<{} variables>", self.env.len()))
            .field("clear_env", &self.clear_env)
            .finish()
    }
}

impl CommandSpec {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Start the child from an empty environment; only `env` entries are
    /// visible to it. Plan 2b uses this to compose a full environment
    /// without inheriting the caller's ambient one.
    #[must_use]
    pub fn clear_env(mut self) -> Self {
        self.clear_env = true;
        self
    }
}

/// What a finished process left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    /// `None` when the process was killed by a signal and has no exit code.
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutcome {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

/// One line of a running child's output, tagged with the pipe it came from.
///
/// `stream` delivers stdout and stderr interleaved, in arrival order, which is
/// the only order that reflects what actually happened -- uv writes its
/// progress to stderr and its results to stdout, and a transcript that showed
/// all of one and then all of the other would be a transcript of neither. The
/// tag is what lets a receiver still tell them apart afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputLine {
    Stdout(String),
    Stderr(String),
}

impl OutputLine {
    pub fn text(&self) -> &str {
        match self {
            Self::Stdout(text) | Self::Stderr(text) => text,
        }
    }
}

/// The single seam through which Bombadil launches processes.
///
/// Nothing above this trait spawns anything directly, which is what lets the
/// uv layer, the environment pipeline and the GUI be tested without processes.
pub trait CommandRunner: Send + Sync {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutcome, CommandError>;

    /// Launch and do not wait. The default delegates to `run` so fakes and
    /// existing implementations need no change.
    ///
    /// Callers that need the child's output must use `run`; this exists for
    /// the one caller that must not block -- opening a terminal emulator,
    /// which lives for as long as the user keeps the window open.
    fn spawn(&self, spec: &CommandSpec) -> Result<(), CommandError> {
        self.run(spec).map(|_| ())
    }

    /// Wait for the child, but hand each line of its output to `on_line` as
    /// it arrives instead of only after it exits. The returned outcome still
    /// carries the exit status and the whole output, so a caller that only
    /// checks `success()` needs no change.
    ///
    /// The default delegates to `run` and replays the buffered output line by
    /// line -- the same shape `spawn`'s default takes, so every existing
    /// implementation stays correct, just not incremental.
    /// `stream`, but the child is killed once `cancel` is set, for a wait
    /// the user can call off (a sign-in waiting on a browser). The default
    /// ignores `cancel`: a runner with no real child has nothing to kill.
    fn stream_cancellable(
        &self,
        spec: &CommandSpec,
        on_line: &mut dyn FnMut(OutputLine),
        _cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<CommandOutcome, CommandError> {
        self.stream(spec, on_line)
    }

    fn stream(
        &self,
        spec: &CommandSpec,
        on_line: &mut dyn FnMut(OutputLine),
    ) -> Result<CommandOutcome, CommandError> {
        let outcome = self.run(spec)?;
        for line in outcome.stdout.lines() {
            on_line(OutputLine::Stdout(line.to_string()));
        }
        for line in outcome.stderr.lines() {
            on_line(OutputLine::Stderr(line.to_string()));
        }
        Ok(outcome)
    }
}

/// Shared helper so both implementations agree on what a program "is".
#[cfg(any(test, feature = "test-support"))]
fn program_matches(program: &Path, suffix: &str) -> bool {
    program
        .file_stem()
        .map(|stem| stem.eq_ignore_ascii_case(suffix))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_exit_is_success() {
        let outcome = CommandOutcome {
            status: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(outcome.success());
    }

    #[test]
    fn a_nonzero_exit_is_not_success() {
        let outcome = CommandOutcome {
            status: Some(1),
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(!outcome.success());
    }

    #[test]
    fn a_signal_killed_process_is_not_success() {
        // No exit code at all: on Unix this is a process killed by a signal.
        // Reporting it as success would turn a crash into a silent no-op.
        let outcome = CommandOutcome {
            status: None,
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(!outcome.success());
    }

    #[test]
    fn a_secret_in_the_environment_never_appears_in_a_debug_rendering() {
        // `env` now holds the whole composed environment -- keychain secrets
        // and private-index passwords included -- and specs get logged.
        let spec = CommandSpec::new("uv")
            .arg("sync")
            .env("UV_INDEX_PRIV_PASSWORD", "hunter2");

        let rendered = format!("{spec:?}");

        assert!(!rendered.contains("hunter2"), "got {rendered}");
        assert!(
            !rendered.contains("UV_INDEX_PRIV_PASSWORD"),
            "got {rendered}"
        );
        assert!(
            rendered.contains("uv") && rendered.contains("sync"),
            "program and args carry no secrets and are worth keeping; got {rendered}"
        );
    }

    #[test]
    fn the_fake_returns_the_outcome_matching_program_and_args() {
        let fake = FakeCommandRunner::new();
        fake.expect(
            "uv",
            &["sync"],
            CommandOutcome {
                status: Some(0),
                stdout: "synced".into(),
                stderr: String::new(),
            },
        );

        let spec = CommandSpec::new("/opt/bin/uv").arg("sync").arg("--frozen");
        let got = fake.run(&spec).unwrap();

        assert_eq!(got.stdout, "synced");
    }

    #[test]
    fn the_fake_records_every_call_for_assertion() {
        let fake = FakeCommandRunner::new();
        fake.expect(
            "uv",
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let spec = CommandSpec::new("/opt/bin/uv")
            .arg("add")
            .arg("requests")
            .env("UV_PROJECT_ENVIRONMENT", "/envs/x");
        fake.run(&spec).unwrap();

        let calls = fake.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args, vec!["add", "requests"]);
        assert_eq!(
            calls[0]
                .env
                .get("UV_PROJECT_ENVIRONMENT")
                .map(String::as_str),
            Some("/envs/x")
        );
    }

    #[test]
    fn the_fake_tells_a_blocking_run_apart_from_a_spawn() {
        // Callers that must not wait on their child -- a terminal emulator, a
        // file manager -- are only provable if the fake distinguishes the two,
        // because the default `spawn` delegates to `run`. Without this test
        // `blocking_calls` could return a constant 0 and every assertion built
        // on it would pass while meaning nothing.
        let fake = FakeCommandRunner::new();
        fake.expect(
            "xdg-open",
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        let spec = CommandSpec::new("xdg-open");

        fake.spawn(&spec).unwrap();
        assert_eq!(fake.blocking_calls(), 0, "spawn must not count as blocking");
        assert_eq!(fake.calls().len(), 1, "a spawn is still a recorded call");

        fake.run(&spec).unwrap();
        assert_eq!(fake.blocking_calls(), 1, "run waits for the child");
        assert_eq!(fake.calls().len(), 2);
    }

    #[test]
    fn the_fake_tells_a_stream_apart_from_a_run() {
        // The counterpart of the test above, for the same reason: the default
        // `CommandRunner::stream` delegates to `run`, so a caller switched
        // back to the buffered path would look identical to a test asserting
        // only on `calls()`. Without this counter "the drawer streams" is
        // unprovable, which is exactly how "spawn, never run" went unproven
        // for two plans.
        let fake = FakeCommandRunner::new();
        fake.expect(
            "uv",
            &["sync"],
            CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        let spec = CommandSpec::new("uv").arg("sync");

        fake.stream(&spec, &mut |_| {}).unwrap();
        assert_eq!(fake.streaming_calls(), 1);
        assert_eq!(
            fake.blocking_calls(),
            0,
            "a stream must not count as a buffered run"
        );

        fake.run(&spec).unwrap();
        assert_eq!(fake.blocking_calls(), 1);
        assert_eq!(
            fake.streaming_calls(),
            1,
            "a run must not count as a stream"
        );
        assert_eq!(fake.calls().len(), 2, "both are still recorded calls");
    }

    #[test]
    fn the_fake_delivers_the_programmed_output_through_the_callback() {
        // A `stream` that returned the outcome but never called `on_line`
        // would satisfy every assertion about exit status while delivering
        // nothing to the drawer at all.
        let fake = FakeCommandRunner::new();
        fake.expect(
            "uv",
            &["sync"],
            CommandOutcome {
                status: Some(0),
                stdout: "resolved 3 packages\ninstalled 3 packages".into(),
                stderr: "warning: no interpreter pin".into(),
            },
        );

        let mut seen = Vec::new();
        let outcome = fake
            .stream(&CommandSpec::new("uv").arg("sync"), &mut |line| {
                seen.push(line)
            })
            .unwrap();

        assert_eq!(
            seen,
            vec![
                OutputLine::Stdout("resolved 3 packages".into()),
                OutputLine::Stdout("installed 3 packages".into()),
                OutputLine::Stderr("warning: no interpreter pin".into()),
            ]
        );
        assert!(outcome.success(), "the exit status still comes back");
    }

    #[test]
    fn the_default_stream_replays_a_runners_whole_output_and_keeps_its_status() {
        // The default exists so an implementation that has not overridden
        // `stream` stays correct -- non-incremental, but complete. A default
        // that dropped stderr, or lost the exit code, would break every
        // caller that switched to `stream`.
        struct RunOnly;
        impl CommandRunner for RunOnly {
            fn run(&self, _spec: &CommandSpec) -> Result<CommandOutcome, CommandError> {
                Ok(CommandOutcome {
                    status: Some(2),
                    stdout: "out-1\nout-2".into(),
                    stderr: "err-1".into(),
                })
            }
        }

        let mut seen = Vec::new();
        let outcome = RunOnly
            .stream(&CommandSpec::new("uv"), &mut |line| {
                seen.push(line.text().to_string())
            })
            .unwrap();

        assert_eq!(seen, vec!["out-1", "out-2", "err-1"]);
        assert_eq!(
            outcome.status,
            Some(2),
            "the exit status must survive the default `stream`"
        );
    }

    #[test]
    fn an_unexpected_call_is_an_explicit_failure_not_a_default() {
        // A fake that silently returns success for an unprogrammed call turns
        // "we never ran this" into a passing test.
        let fake = FakeCommandRunner::new();
        let spec = CommandSpec::new("/opt/bin/uv").arg("sync");
        let err = fake.run(&spec).unwrap_err();
        assert!(format!("{err}").contains("uv"), "got {err}");
    }
}
