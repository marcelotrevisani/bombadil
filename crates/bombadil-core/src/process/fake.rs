#![cfg(any(test, feature = "test-support"))]

use super::{CommandError, CommandOutcome, CommandRunner, CommandSpec, program_matches};
use std::sync::Mutex;

struct Rule {
    program_suffix: String,
    args_contains: Vec<String>,
    outcome: CommandOutcome,
}

/// A `CommandRunner` that returns programmed outcomes and records every call.
///
/// An unprogrammed call is an error rather than a default success: a fake that
/// quietly succeeds turns "this code never ran the command" into a green test.
#[derive(Default)]
pub struct FakeCommandRunner {
    rules: Mutex<Vec<Rule>>,
    calls: Mutex<Vec<CommandSpec>>,
    blocking: Mutex<usize>,
}

impl FakeCommandRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Program an outcome for any call whose program file stem matches
    /// `program_suffix` and whose args contain every entry of `args_contains`.
    pub fn expect(&self, program_suffix: &str, args_contains: &[&str], outcome: CommandOutcome) {
        self.rules.lock().unwrap().push(Rule {
            program_suffix: program_suffix.to_string(),
            args_contains: args_contains.iter().map(|s| s.to_string()).collect(),
            outcome,
        });
    }

    /// Every spec passed to `run` or `spawn`, in order.
    pub fn calls(&self) -> Vec<CommandSpec> {
        self.calls.lock().unwrap().clone()
    }

    /// How many calls arrived through `run` -- which waits for the child --
    /// rather than through `spawn`, which does not.
    ///
    /// The default `CommandRunner::spawn` delegates to `run`, so without this
    /// counter the two are indistinguishable to a test, and a caller that
    /// must not block on its child (a terminal emulator, a file manager:
    /// both live until the user closes the window) could be switched to `run`
    /// with every assertion still passing.
    pub fn blocking_calls(&self) -> usize {
        *self.blocking.lock().unwrap()
    }

    /// Record the call and answer it from the programmed rules. Shared by
    /// `run` and `spawn` so both are recorded identically; only the blocking
    /// counter tells them apart.
    fn record(&self, spec: &CommandSpec) -> Result<CommandOutcome, CommandError> {
        self.calls.lock().unwrap().push(spec.clone());

        let rules = self.rules.lock().unwrap();
        rules
            .iter()
            .find(|rule| {
                program_matches(&spec.program, &rule.program_suffix)
                    && rule
                        .args_contains
                        .iter()
                        .all(|want| spec.args.contains(want))
            })
            .map(|rule| rule.outcome.clone())
            .ok_or_else(|| CommandError::Unexpected {
                program: spec.program.clone(),
                args: spec.args.clone(),
            })
    }
}

impl CommandRunner for FakeCommandRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutcome, CommandError> {
        *self.blocking.lock().unwrap() += 1;
        self.record(spec)
    }

    fn spawn(&self, spec: &CommandSpec) -> Result<(), CommandError> {
        self.record(spec).map(|_| ())
    }
}
