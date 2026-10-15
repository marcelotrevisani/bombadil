//! Detecting which terminal emulators are actually installed.
//!
//! Spec section 8: only terminals actually found on this machine are offered.
//! Detection must never launch the terminal it is checking for -- a probe
//! that opened a window at startup would make the app unusable -- so every
//! probe here asks the OS whether a binary or app bundle exists rather than
//! running it. `which`/`where` do exactly that for binaries; `open -Ra`
//! (note the `-R`, "register only") does the equivalent for a macOS `.app`
//! bundle without launching it.
//!
//! [`Os`] is a parameter here for the same reason it is throughout this
//! module (see its doc comment): it lets Windows and macOS detection logic
//! be exercised by tests that run on Linux CI.

use super::{Os, TerminalKind};
use crate::process::{CommandRunner, CommandSpec};

/// Every [`TerminalKind`] offered for `os` whose binary or app bundle the
/// probe actually finds, in [`TerminalKind::for_os`] order.
///
/// A probe that fails -- non-zero exit, or the runner rejecting the call
/// outright -- means "absent", never an error: offering a terminal that
/// turns out not to be there produces spec section 9's "chosen terminal has
/// disappeared" at the worst possible moment, so silence beats a false
/// positive here.
pub fn available(os: Os, runner: &dyn CommandRunner) -> Vec<TerminalKind> {
    TerminalKind::for_os(os)
        .into_iter()
        .filter(|&kind| probe(kind, os, runner))
        .collect()
}

fn probe(kind: TerminalKind, os: Os, runner: &dyn CommandRunner) -> bool {
    // A terminal that does not belong to `os` is simply not there.
    probe_spec(kind, os).is_some_and(|spec| {
        runner
            .run(&spec)
            .map(|outcome| outcome.success())
            .unwrap_or(false)
    })
}

/// The probe for `kind` on `os`, or `None` when `kind` is not one of that
/// platform's terminals.
fn probe_spec(kind: TerminalKind, os: Os) -> Option<CommandSpec> {
    use TerminalKind::*;
    match os {
        Os::Linux => Some(CommandSpec::new("which").arg(linux_binary_name(kind)?)),
        Os::Windows => Some(CommandSpec::new("where").arg(windows_binary_name(kind)?)),
        Os::MacOs => {
            let bundle = match kind {
                TerminalApp => "Terminal",
                ITerm2 => "iTerm",
                _ => return None,
            };
            // -R: register/resolve only, never launches the app.
            Some(CommandSpec::new("open").arg("-Ra").arg(bundle))
        }
    }
}

/// The binary name each Linux [`TerminalKind`] is probed for. Matches the
/// program `argv::build` spawns, so a found probe and the argv it enables
/// always agree on what "found" means.
fn linux_binary_name(kind: TerminalKind) -> Option<&'static str> {
    use TerminalKind::*;
    Some(match kind {
        GnomeTerminal => "gnome-terminal",
        Konsole => "konsole",
        Kitty => "kitty",
        Alacritty => "alacritty",
        WezTerm => "wezterm",
        Foot => "foot",
        Xterm => "xterm",
        _ => return None,
    })
}

/// The binary name each Windows [`TerminalKind`] is probed for.
fn windows_binary_name(kind: TerminalKind) -> Option<&'static str> {
    use TerminalKind::*;
    Some(match kind {
        WindowsTerminal => "wt",
        PowerShell => "powershell",
        Cmd => "cmd",
        // TODO(windows): this probe is likely inverted. `where bash` on a
        // stock Windows finds WSL's C:\Windows\System32\bash.exe -- a false
        // positive, since that is not Git Bash -- while Git for Windows'
        // own bash.exe lives under C:\Program Files\Git\bin and is normally
        // NOT on PATH, giving a false negative on the machines that actually
        // have it. Neither could be verified from the machine this was
        // written on, so the code is left alone rather than guessed at;
        // a real fix probably probes for git.exe and derives the sibling
        // bash.exe path from it.
        GitBash => "bash",
        Wsl => "wsl",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{CommandOutcome, FakeCommandRunner};

    #[test]
    fn a_terminal_from_another_platform_probes_as_absent() {
        // Was `unreachable!`: a mismatched pair panicked instead of saying
        // "not here".
        let runner = FakeCommandRunner::new();
        assert!(!probe(TerminalKind::Konsole, Os::Windows, &runner));
        assert!(!probe(TerminalKind::ITerm2, Os::Linux, &runner));
        assert!(!probe(TerminalKind::Kitty, Os::MacOs, &runner));
        assert!(
            runner.calls().is_empty(),
            "nothing is run for an impossible pair"
        );
    }

    fn found(program: &str) -> CommandOutcome {
        CommandOutcome {
            status: Some(0),
            stdout: format!("/usr/bin/{program}"),
            stderr: String::new(),
        }
    }

    #[test]
    fn only_terminals_the_probe_finds_are_offered() {
        let runner = FakeCommandRunner::new();
        // `expect` already matches on the program suffix alone when
        // `args_contains` is empty -- the probe's argv carries the terminal
        // name, so match on that instead.
        runner.expect("which", &["kitty"], found("kitty"));
        let got = available(Os::Linux, &runner);
        assert_eq!(got, vec![TerminalKind::Kitty], "got {got:?}");
    }

    #[test]
    fn nothing_installed_yields_an_empty_list_rather_than_a_default() {
        // Offering a terminal that is not there produces spec section 9's
        // "chosen terminal has disappeared" at the worst possible moment.
        let runner = FakeCommandRunner::new();
        assert!(available(Os::Linux, &runner).is_empty());
    }

    #[test]
    fn detection_order_follows_the_offered_order() {
        let runner = FakeCommandRunner::new();
        runner.expect("which", &["xterm"], found("xterm"));
        runner.expect("which", &["gnome-terminal"], found("gnome-terminal"));
        assert_eq!(
            available(Os::Linux, &runner),
            vec![TerminalKind::GnomeTerminal, TerminalKind::Xterm]
        );
    }

    #[test]
    fn a_probe_that_fails_means_absent_not_an_error() {
        let runner = FakeCommandRunner::new();
        runner.expect(
            "which",
            &["konsole"],
            CommandOutcome {
                status: Some(1),
                stdout: String::new(),
                stderr: "not found".into(),
            },
        );
        assert!(available(Os::Linux, &runner).is_empty());
    }

    #[test]
    fn detection_never_launches_the_terminal_itself() {
        // A probe that opened a window at startup would make the app unusable.
        // `open -Ra <app>` tests for the app; `open -a <app>` LAUNCHES it, and
        // the program name is "open" either way -- so the -R is the only thing
        // that distinguishes them, and it lives in the args.
        for os in [Os::Linux, Os::MacOs, Os::Windows] {
            let runner = FakeCommandRunner::new();
            let _ = available(os, &runner);
            for call in runner.calls() {
                let prog = call.program.to_string_lossy().to_string();
                assert!(
                    prog.contains("which") || prog.contains("where") || prog.contains("open"),
                    "detection must probe, not launch; got {prog}"
                );
                if prog.contains("open") {
                    assert!(
                        call.args.iter().any(|a| a == "-Ra" || a == "-R"),
                        "an `open` probe without -R launches the app; got {:?}",
                        call.args
                    );
                }
            }
        }
    }
}
