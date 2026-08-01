mod fake;
mod system;

#[cfg(any(test, feature = "test-support"))]
pub use fake::FakeCommandRunner;
pub use system::SystemCommandRunner;

use std::collections::BTreeMap;
#[cfg(any(test, feature = "test-support"))]
use std::path::Path;
use std::path::PathBuf;

/// Everything needed to launch one process.
///
/// `BTreeMap` rather than `HashMap` so the environment is ordered: tests can
/// assert on it, and failure reports render the same way twice.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    /// When true the child starts from an empty environment and sees only
    /// `env`. Plan 2b composes a full environment and sets this.
    pub clear_env: bool,
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

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("could not run {program}: {source}")]
    Spawn {
        program: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("no fake outcome was programmed for {program} {args:?}")]
    Unexpected { program: PathBuf, args: Vec<String> },
}

/// The single seam through which Bombadil launches processes.
///
/// Nothing above this trait spawns anything directly, which is what lets the
/// uv layer, the environment pipeline and the GUI be tested without processes.
pub trait CommandRunner: Send + Sync {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutcome, CommandError>;
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
    fn an_unexpected_call_is_an_explicit_failure_not_a_default() {
        // A fake that silently returns success for an unprogrammed call turns
        // "we never ran this" into a passing test.
        let fake = FakeCommandRunner::new();
        let spec = CommandSpec::new("/opt/bin/uv").arg("sync");
        let err = fake.run(&spec).unwrap_err();
        assert!(format!("{err}").contains("uv"), "got {err}");
    }
}
