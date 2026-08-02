use serde::{Deserialize, Serialize};

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
}

impl TerminalKind {
    pub fn os(self) -> Os {
        use TerminalKind::*;
        match self {
            WindowsTerminal | PowerShell | Cmd | GitBash | Wsl => Os::Windows,
            TerminalApp | ITerm2 => Os::MacOs,
            GnomeTerminal | Konsole | Kitty | Alacritty | WezTerm | Foot | Xterm => Os::Linux,
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
        for kind in TerminalKind::for_os(Os::Linux) {
            assert_eq!(kind.os(), Os::Linux, "{kind:?} is not a Linux terminal");
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
        assert_eq!(TerminalKind::for_os(Os::Linux).len(), 7);
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
