#![cfg(any(test, feature = "test-support"))]

use super::{
    CommandError, CommandOutcome, CommandRunner, CommandSpec, OutputLine, program_matches,
};
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
    streaming: Mutex<usize>,
    cancellable: Mutex<usize>,
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

    /// How many calls arrived through `stream` -- which delivers output as it
    /// is produced -- rather than through `run`, which delivers it only after
    /// the child exits.
    ///
    /// Exists for the same reason `blocking_calls` does, and learned from the
    /// same mistake: `CommandRunner::stream`'s default delegates to `run`, so
    /// without this counter no test could tell a caller that streams from one
    /// that buffers, and the whole point of streaming would be unprovable.
    pub fn streaming_calls(&self) -> usize {
        *self.streaming.lock().unwrap()
    }

    /// How many calls arrived through `stream_cancellable`: a run the user
    /// can Stop, as opposed to a sync that cannot be called off.
    pub fn cancellable_calls(&self) -> usize {
        *self.cancellable.lock().unwrap()
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

    /// Hands the programmed outcome to `on_line` a line at a time, stdout
    /// then stderr -- a fake has no real child to interleave with, and the
    /// only thing a test can meaningfully assert is that every line arrived
    /// through the callback rather than only in the returned outcome.
    ///
    /// Counted separately from `run` and deliberately not counted as
    /// blocking: that is what makes "this caller streams" an assertion rather
    /// than a hope.
    fn stream(
        &self,
        spec: &CommandSpec,
        on_line: &mut dyn FnMut(OutputLine),
    ) -> Result<CommandOutcome, CommandError> {
        *self.streaming.lock().unwrap() += 1;
        let outcome = self.record(spec)?;
        for line in outcome.stdout.lines() {
            on_line(OutputLine::Stdout(line.to_string()));
        }
        for line in outcome.stderr.lines() {
            on_line(OutputLine::Stderr(line.to_string()));
        }
        Ok(outcome)
    }

    fn stream_cancellable(
        &self,
        spec: &CommandSpec,
        on_line: &mut dyn FnMut(OutputLine),
        _cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<CommandOutcome, CommandError> {
        *self.cancellable.lock().unwrap() += 1;
        self.stream(spec, on_line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellable_streams_are_counted_apart() {
        let fake = FakeCommandRunner::new();
        fake.expect(
            "uv",
            &["run"],
            CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        let spec = CommandSpec::new("uv").arg("run");
        fake.stream_cancellable(
            &spec,
            &mut |_| {},
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(fake.cancellable_calls(), 1);
        fake.stream(&spec, &mut |_| {}).unwrap();
        assert_eq!(
            fake.cancellable_calls(),
            1,
            "a plain stream is not cancellable"
        );
    }
}
