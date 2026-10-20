use serde::{Deserialize, Serialize};

pub mod activation;
pub mod argv;
pub mod detect;
pub mod open;

pub use crate::model::TerminalChoice;

/// The operating system a terminal command is being built for.
///
/// This is a parameter rather than a `cfg!` everywhere it appears, so that
/// Windows and macOS argv construction is covered by tests that run on Linux.
/// Per spec section 10 that is a deliberate testability requirement, not a
/// stylistic choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Os {
    Linux,
    MacOs,
    Windows,
}

impl Os {
    /// The OS this build is running on.
    pub fn host() -> Self {
        if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }
}

/// A terminal emulator Bombadil knows how to launch. Spec section 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalKind {
    // Windows
    WindowsTerminal,
    PowerShell,
    Cmd,
    GitBash,
    Wsl,
    // macOS
    TerminalApp,
    ITerm2,
    // Linux
    GnomeTerminal,
    Konsole,
    Kitty,
    Alacritty,
    WezTerm,
    Foot,
    Xterm,
    Ptyxis,
    Ghostty,
    Terminator,
}

/// How much of the composed environment actually reaches the shell.
///
/// Three buckets, not two: a kind can fail to re-assert the venv's position
/// on `PATH` (bad), or fail to receive the composed environment at all
/// (worse -- the venv is not on `PATH` in any position). The GUI has no way
/// to warn the user correctly without telling those apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationSupport {
    /// Everything arrives and the re-assertion runs.
    Full,
    /// The composed environment arrives, but nothing re-asserts it after the
    /// user's shell rc runs.
    PathOnly,
    /// The emulator is launched in a way that does not carry an environment at
    /// all, so none of the composition reaches the shell.
    None,
}

impl TerminalKind {
    pub fn os(self) -> Os {
        use TerminalKind::*;
        match self {
            WindowsTerminal | PowerShell | Cmd | GitBash | Wsl => Os::Windows,
            TerminalApp | ITerm2 => Os::MacOs,
            GnomeTerminal | Konsole | Kitty | Alacritty | WezTerm | Foot | Xterm | Ptyxis
            | Ghostty | Terminator => Os::Linux,
        }
    }

    /// How much of the composed environment this kind actually delivers.
    ///
    /// Exhaustive on purpose -- no wildcard arm -- so adding a
    /// [`TerminalKind`] is a compile error until someone decides which bucket
    /// it belongs in, rather than silently defaulting to the optimistic one.
    pub fn activation_support(self) -> ActivationSupport {
        use TerminalKind::*;
        match self {
            // The emulator is handed our `shell` and `shell_args`, and the
            // spawn carries the composed environment: both halves arrive.
            GnomeTerminal | Konsole | Kitty | Alacritty | WezTerm | Foot | Xterm | Ptyxis
            | Ghostty | Terminator | WindowsTerminal => ActivationSupport::Full,

            // Launched as an ordinary child process, so `spec.env` reaches
            // them -- but none has a flag to hand it a specific shell binary
            // plus arguments, so the generated init file never runs.
            // `Cmd` and `PowerShell` are near-fine in practice: the composed
            // PATH already leads with the venv and neither has an rc that
            // re-prepends. `GitBash` runs `bash --login -i`, whose rc
            // reproduces the measured demotion.
            PowerShell | Cmd | GitBash => ActivationSupport::PathOnly,

            // `open -a Terminal <cwd>` / `open -a iTerm <cwd>` hand the
            // request to LaunchServices, which does not propagate the calling
            // process's environment -- that is why `launchctl setenv` exists.
            // `spec.env` and `clear_env` are delivered to the `open` binary
            // and discarded; only `cwd` survives, because it is an argument.
            // `wsl.exe` is the same story across the WSL boundary: nothing
            // crosses except via WSLENV, and a `C:\...\Scripts` path would be
            // meaningless inside the distro anyway.
            TerminalApp | ITerm2 | Wsl => ActivationSupport::None,
        }
    }

    /// Every terminal this OS could have, in the order they are offered.
    pub fn for_os(os: Os) -> Vec<TerminalKind> {
        use TerminalKind::*;
        match os {
            Os::Windows => vec![WindowsTerminal, PowerShell, Cmd, GitBash, Wsl],
            Os::MacOs => vec![TerminalApp, ITerm2],
            Os::Linux => vec![
                GnomeTerminal,
                Konsole,
                Kitty,
                Alacritty,
                WezTerm,
                Foot,
                Xterm,
                Ptyxis,
                Ghostty,
                Terminator,
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_knows_which_os_it_belongs_to() {
        assert_eq!(TerminalKind::GnomeTerminal.os(), Os::Linux);
        assert_eq!(TerminalKind::ITerm2.os(), Os::MacOs);
        assert_eq!(TerminalKind::WindowsTerminal.os(), Os::Windows);
    }

    #[test]
    fn kinds_for_an_os_are_only_that_oss_kinds() {
        // Counts alone would not catch a variant swapped between two lists,
        // which would offer a terminal on a platform where its argv cannot run.
        for os in [Os::Linux, Os::MacOs, Os::Windows] {
            for kind in TerminalKind::for_os(os) {
                assert_eq!(kind.os(), os, "{kind:?} is listed under {os:?}");
            }
        }
        assert!(TerminalKind::for_os(Os::MacOs).contains(&TerminalKind::TerminalApp));
        assert!(!TerminalKind::for_os(Os::MacOs).contains(&TerminalKind::Konsole));
    }

    #[test]
    fn the_spec_list_is_complete_for_every_os() {
        // Spec section 8 names these exactly. A missing entry means a terminal
        // the user has is never offered, which reads as "Bombadil does not
        // support my setup" rather than as a bug.
        assert_eq!(TerminalKind::for_os(Os::Windows).len(), 5);
        assert_eq!(TerminalKind::for_os(Os::MacOs).len(), 2);
        assert_eq!(TerminalKind::for_os(Os::Linux).len(), 10);
    }

    #[test]
    fn every_kind_is_pinned_to_the_activation_bucket_it_was_measured_in() {
        // All seventeen, spelled out. The exhaustive match in
        // `activation_support` makes a NEW variant a compile error, but
        // nothing stops someone quietly moving an existing kind between
        // buckets -- and the GUI's warning is only correct if these are.
        // Each bucket is a materially different message to the user:
        // Full = activated; PathOnly = on PATH but possibly not first;
        // None = not activated at all.
        use ActivationSupport::*;
        use TerminalKind::*;
        let expected = [
            (GnomeTerminal, Full),
            (Konsole, Full),
            (Kitty, Full),
            (Alacritty, Full),
            (WezTerm, Full),
            (Foot, Full),
            (Xterm, Full),
            (Ptyxis, Full),
            (Ghostty, Full),
            (Terminator, Full),
            (WindowsTerminal, Full),
            (PowerShell, PathOnly),
            (Cmd, PathOnly),
            (GitBash, PathOnly),
            (TerminalApp, None),
            (ITerm2, None),
            (Wsl, None),
        ];

        for (kind, want) in expected {
            assert_eq!(kind.activation_support(), want, "{kind:?}");
        }

        // And every kind that exists is actually listed above, so a new
        // variant cannot slip past this test unmeasured.
        let listed: Vec<TerminalKind> = expected.iter().map(|(k, _)| *k).collect();
        for os in [Os::Linux, Os::MacOs, Os::Windows] {
            for kind in TerminalKind::for_os(os) {
                assert!(listed.contains(&kind), "{kind:?} is not pinned");
            }
        }
    }

    #[test]
    fn a_terminal_choice_round_trips_through_toml() {
        let choice = TerminalChoice::Detected(TerminalKind::Kitty);
        let text = toml::to_string(&Wrapper {
            choice: choice.clone(),
        })
        .unwrap();
        let back: Wrapper = toml::from_str(&text).unwrap();
        assert_eq!(back.choice, choice);
    }

    #[test]
    fn a_custom_choice_round_trips_through_toml() {
        let choice = TerminalChoice::Custom {
            template: "myterm --cd {cwd} -e {shell}".into(),
        };
        let text = toml::to_string(&Wrapper {
            choice: choice.clone(),
        })
        .unwrap();
        let back: Wrapper = toml::from_str(&text).unwrap();
        assert_eq!(back.choice, choice);
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Wrapper {
        choice: TerminalChoice,
    }
}
