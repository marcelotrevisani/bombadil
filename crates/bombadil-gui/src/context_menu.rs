//! The project context menu: spec §8 -- right-clicking a project offers Open
//! terminal (a submenu of terminals actually detected on this machine, the
//! configured default first, plus a Custom entry), Sync, and Remove from
//! list.
//!
//! Detection (`terminal::detect::available`) and opening
//! (`terminal::open::open`) both take a `CommandRunner` and must run through
//! `job::run` / `CommandRunner::spawn` -- that wiring lives in `app.rs`
//! (`detect_terminals`, `open_terminal`). Everything in this file is pure:
//! ordering the detected list, labelling a kind by how much of the composed
//! environment it actually delivers, and the dumb `view` that renders it.
//!
//! Spec §8 also lists Open folder and Recreate venv on this menu. Neither is
//! built here: this branch has no interface for opening a file manager, and
//! `bombadil-core` has no function that deletes a venv at all (confirmed by
//! grep -- the only `fs::remove_*` call in the whole crate is inside
//! `venv_path`'s own test fixture teardown). Recreate venv would have to
//! invent destructive filesystem code with no test coverage backing it;
//! noted here rather than built speculatively.

use crate::app::Message;
use bombadil_core::model::TerminalChoice;
use bombadil_core::terminal::{ActivationSupport, TerminalKind};

/// One entry in the "Open terminal" submenu: a detected kind, or the
/// always-present Custom entry (spec §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalOption {
    Detected(TerminalKind),
    Custom,
}

/// Puts the configured default terminal first among the detected kinds,
/// leaving the rest in `available`'s order. Criterion 2: an arbitrary order
/// ignores the user's setting.
///
/// A `Custom` default has nothing to move to the front here -- it is not a
/// `TerminalKind` at all, so `available`'s order is left untouched and the
/// always-present Custom entry (see `submenu`) is where that choice actually
/// surfaces. A default that was never detected (uninstalled since it was
/// configured) is likewise left alone -- there is nothing to move it to.
pub fn ordered(
    mut available: Vec<TerminalKind>,
    default: Option<&TerminalChoice>,
) -> Vec<TerminalKind> {
    if let Some(TerminalChoice::Detected(default_kind)) = default
        && let Some(pos) = available.iter().position(|k| k == default_kind)
    {
        let kind = available.remove(pos);
        available.insert(0, kind);
    }
    available
}

/// The full "Open terminal" submenu: every detected kind, ordered by
/// `ordered`, plus the Custom entry spec §8 always offers.
pub fn submenu(
    available: Vec<TerminalKind>,
    default: Option<&TerminalChoice>,
) -> Vec<TerminalOption> {
    let mut entries: Vec<TerminalOption> = ordered(available, default)
        .into_iter()
        .map(TerminalOption::Detected)
        .collect();
    entries.push(TerminalOption::Custom);
    entries
}

/// The warning suffix a degraded terminal's menu label carries. `Full` gets
/// none -- the empty string -- so a caller can append it unconditionally
/// without an extra branch.
///
/// Criterion 3: `PathOnly` and `None` must read as different problems, not
/// the same one. A `PathOnly` terminal still receives the composed
/// environment and only fails to re-assert the venv's position on `PATH`
/// after the user's shell rc runs; a `None` terminal receives no composed
/// environment at all -- the venv is not on `PATH` in any position. A macOS
/// user picking Terminal.app needs to see that it is the worse of the two,
/// not a warning indistinguishable from PowerShell's.
pub fn activation_warning(kind: TerminalKind) -> &'static str {
    match kind.activation_support() {
        ActivationSupport::Full => "",
        ActivationSupport::PathOnly => {
            " (environment on PATH, not re-asserted after your shell config)"
        }
        ActivationSupport::None => " (not activated -- no environment reaches this shell)",
    }
}

/// A human-readable name for the submenu row, independent of the probe
/// binary name `detect` matches on.
pub fn terminal_name(kind: TerminalKind) -> &'static str {
    use TerminalKind::*;
    match kind {
        WindowsTerminal => "Windows Terminal",
        PowerShell => "PowerShell",
        Cmd => "Command Prompt",
        GitBash => "Git Bash",
        Wsl => "WSL",
        TerminalApp => "Terminal.app",
        ITerm2 => "iTerm2",
        GnomeTerminal => "GNOME Terminal",
        Konsole => "Konsole",
        Kitty => "kitty",
        Alacritty => "Alacritty",
        WezTerm => "WezTerm",
        Foot => "foot",
        Xterm => "xterm",
    }
}

/// The label the submenu row shows for a detected kind: its name plus
/// `activation_warning`, so a degraded terminal reads as degraded right in
/// the menu, not only after the user has already opened it and found an
/// unactivated shell.
pub fn menu_label(kind: TerminalKind) -> String {
    format!("{}{}", terminal_name(kind), activation_warning(kind))
}

/// The context menu's live state while it is open. `terminals` starts empty
/// -- `app::update`'s `ContextMenuTerminalsDetected` arm fills it in once the
/// background probe (criterion 1) lands, the same "starts hollow, filled in
/// asynchronously" shape `sidebar::Entry` uses for venv status.
pub struct ContextMenu {
    pub project_index: usize,
    pub terminals: Vec<TerminalKind>,
}

/// Renders the open context menu. Deliberately dumb: all the logic worth
/// testing lives in `ordered`, `activation_warning` and `submenu`.
pub fn view<'a>(
    menu: &ContextMenu,
    default: Option<&TerminalChoice>,
) -> iced::Element<'a, Message> {
    let index = menu.project_index;
    let mut column = iced::widget::column![iced::widget::text("Open terminal")].spacing(4);

    for option in submenu(menu.terminals.clone(), default) {
        match option {
            TerminalOption::Detected(kind) => {
                column = column.push(
                    iced::widget::button(iced::widget::text(menu_label(kind))).on_press(
                        Message::OpenTerminalRequested(index, TerminalChoice::Detected(kind)),
                    ),
                );
            }
            // Only offered as a live choice once the user has actually
            // configured a custom template -- an empty one is refused by
            // `terminal::argv::build` anyway, so there is nothing useful to
            // dispatch without it.
            TerminalOption::Custom => {
                if let Some(TerminalChoice::Custom { template }) = default {
                    column =
                        column.push(iced::widget::button(iced::widget::text("Custom")).on_press(
                            Message::OpenTerminalRequested(
                                index,
                                TerminalChoice::Custom {
                                    template: template.clone(),
                                },
                            ),
                        ));
                }
            }
        }
    }

    column
        .push(
            iced::widget::button(iced::widget::text("Sync"))
                .on_press(Message::SyncRequested(index)),
        )
        .push(
            iced::widget::button(iced::widget::text("Remove from list"))
                .on_press(Message::RemoveProjectRequested(index)),
        )
        .push(
            iced::widget::button(iced::widget::text("Close")).on_press(Message::ContextMenuClosed),
        )
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configured_default_terminal_is_moved_to_the_front() {
        let available = vec![
            TerminalKind::GnomeTerminal,
            TerminalKind::Kitty,
            TerminalKind::Xterm,
        ];
        let default = TerminalChoice::Detected(TerminalKind::Xterm);

        let got = ordered(available, Some(&default));

        assert_eq!(got.first(), Some(&TerminalKind::Xterm), "got {got:?}");
        assert_eq!(
            got.len(),
            3,
            "reordering must not drop or duplicate an entry; got {got:?}"
        );
        assert!(got.contains(&TerminalKind::GnomeTerminal));
        assert!(got.contains(&TerminalKind::Kitty));
    }

    #[test]
    fn no_configured_default_leaves_detection_order_untouched() {
        let available = vec![TerminalKind::GnomeTerminal, TerminalKind::Kitty];
        let got = ordered(available.clone(), None);
        assert_eq!(got, available);
    }

    #[test]
    fn a_default_that_was_not_detected_changes_nothing() {
        // Configured, then uninstalled -- must not panic or silently invent
        // an entry for a terminal that is not actually there.
        let available = vec![TerminalKind::GnomeTerminal, TerminalKind::Kitty];
        let default = TerminalChoice::Detected(TerminalKind::Xterm);
        let got = ordered(available.clone(), Some(&default));
        assert_eq!(got, available);
    }

    #[test]
    fn a_custom_default_does_not_touch_the_detected_order() {
        let available = vec![TerminalKind::GnomeTerminal, TerminalKind::Kitty];
        let default = TerminalChoice::Custom {
            template: "myterm --cd {cwd} -e {shell}".into(),
        };
        let got = ordered(available.clone(), Some(&default));
        assert_eq!(got, available);
    }

    #[test]
    fn the_submenu_always_carries_a_custom_entry_last() {
        let available = vec![TerminalKind::Kitty];
        let entries = submenu(available, None);
        assert_eq!(
            entries,
            vec![
                TerminalOption::Detected(TerminalKind::Kitty),
                TerminalOption::Custom
            ]
        );
    }

    #[test]
    fn full_activation_kinds_carry_no_warning() {
        assert_eq!(activation_warning(TerminalKind::Kitty), "");
        assert_eq!(activation_warning(TerminalKind::WindowsTerminal), "");
    }

    #[test]
    fn path_only_and_none_are_labelled_with_different_warnings() {
        // The exact bug criterion 3 exists to catch: lumping PathOnly and
        // None together would make a macOS user picking Terminal.app
        // indistinguishable from one picking PowerShell -- only one of them
        // gets zero composed environment at all.
        let path_only = activation_warning(TerminalKind::PowerShell);
        let none = activation_warning(TerminalKind::TerminalApp);

        assert_ne!(path_only, "", "PathOnly must carry SOME warning");
        assert_ne!(none, "", "None must carry SOME warning");
        assert_ne!(
            path_only, none,
            "PathOnly and None must read as different problems, not the same one"
        );
    }

    #[test]
    fn every_path_only_kind_shares_the_same_warning_text() {
        // Internally consistent: PowerShell, Cmd and GitBash are all
        // PathOnly, and a user should not have to learn three different
        // phrasings for the same failure mode.
        let cases = [
            TerminalKind::PowerShell,
            TerminalKind::Cmd,
            TerminalKind::GitBash,
        ];
        let first = activation_warning(cases[0]);
        for kind in cases {
            assert_eq!(activation_warning(kind), first, "{kind:?}");
        }
    }

    #[test]
    fn every_none_kind_shares_the_same_warning_text() {
        let cases = [
            TerminalKind::TerminalApp,
            TerminalKind::ITerm2,
            TerminalKind::Wsl,
        ];
        let first = activation_warning(cases[0]);
        for kind in cases {
            assert_eq!(activation_warning(kind), first, "{kind:?}");
        }
    }

    #[test]
    fn the_menu_label_names_the_terminal_and_appends_its_warning() {
        assert_eq!(menu_label(TerminalKind::Kitty), "kitty");
        let label = menu_label(TerminalKind::TerminalApp);
        assert!(label.starts_with("Terminal.app"), "got {label}");
        assert!(label.contains("not activated"), "got {label}");
    }
}
