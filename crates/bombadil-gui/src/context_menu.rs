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
//! Spec §8's other two entries, Open folder and Recreate venv, are here too.
//! Both spawn or delete through core (`folder::open`, `venv_path::delete`)
//! and are dispatched from `app.rs`. Recreate venv is the one action on this
//! menu that destroys something, so the button does not perform it: it opens
//! the confirmation `recreate_confirmation` writes, and only
//! `Message::RecreateVenvConfirmed` reaches the delete.

use crate::app::Message;
use bombadil_core::model::TerminalChoice;
use bombadil_core::terminal::{ActivationSupport, TerminalKind};
use std::path::{Path, PathBuf};
use uuid::Uuid;

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

/// The pending Recreate venv confirmation (spec §8). Held while the user
/// decides; nothing is deleted until `Message::RecreateVenvConfirmed`
/// arrives. Same "starts as `None`, exists only while it is on screen" shape
/// as `ContextMenu` itself.
///
/// `venv_path` is resolved once, when the confirmation opens, and carried
/// here rather than re-resolved by `view`: the text the user agreed to and
/// the path that gets deleted must be the same string, not two derivations
/// that could drift.
///
/// `project_id`, not a `project_index`: an index is only meaningful against
/// the list as it stood when the confirmation opened, and nothing here is
/// modal. Right-click B and pick Recreate venv, then right-click A and pick
/// Remove from list, then press Confirm -- with an index this deleted C's
/// environment, because every project after A had shifted down one.
/// `venv_path::delete`'s refusals all held; it was handed the wrong project.
/// A `Uuid` makes that class unrepresentable rather than needing each
/// removal path to remember one more `= None`.
pub struct ConfirmRecreate {
    pub project_id: Uuid,
    pub label: String,
    pub venv_path: PathBuf,
}

/// What the confirmation says before anything is deleted.
///
/// Recreating is delete-then-sync, and only the delete is certain to work.
/// Both facts the user is agreeing to -- *which* directory disappears, and
/// that a failed rebuild leaves them with nothing -- have to be on screen
/// before they agree, so this states them rather than asking "are you sure?".
pub fn recreate_confirmation(label: &str, venv_path: &Path) -> String {
    format!(
        "Recreate the environment for {label}?\n\n\
         {} will be deleted, then rebuilt with uv sync.\n\n\
         If the rebuild fails -- no network, a lock file that no longer resolves -- \
         the old environment has already been deleted and cannot be restored.",
        venv_path.display()
    )
}

/// Renders the pending Recreate venv confirmation. Deliberately dumb, like
/// `view`: the sentence worth testing is `recreate_confirmation`'s.
pub fn confirm_view<'a>(confirm: &ConfirmRecreate) -> iced::Element<'a, Message> {
    iced::widget::column![
        iced::widget::text(recreate_confirmation(&confirm.label, &confirm.venv_path)),
        iced::widget::button(iced::widget::text("Delete and recreate"))
            .on_press(Message::RecreateVenvConfirmed),
        iced::widget::button(iced::widget::text("Cancel")).on_press(Message::RecreateVenvCancelled),
    ]
    .spacing(8)
    .into()
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
            iced::widget::button(iced::widget::text("Open folder"))
                .on_press(Message::OpenFolderRequested(index)),
        )
        // Opens the confirmation, never the deletion -- see
        // `Message::RecreateVenvRequested`.
        .push(
            iced::widget::button(iced::widget::text("Recreate venv"))
                .on_press(Message::RecreateVenvRequested(index)),
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

    #[test]
    fn the_recreate_confirmation_names_the_exact_directory_that_will_be_deleted() {
        // "Are you sure?" is not a confirmation of anything. The user has to
        // be able to see, before pressing yes, which directory is about to
        // stop existing -- a central-store venv is a hashed folder name they
        // have never seen, and the project label alone does not identify it.
        let text = recreate_confirmation("api", Path::new("/home/t/.venvs/api-1f2e3d"));

        assert!(
            text.contains("/home/t/.venvs/api-1f2e3d"),
            "the resolved venv path must be stated verbatim; got {text}"
        );
        assert!(text.contains("api"), "got {text}");
    }

    #[test]
    fn the_recreate_confirmation_states_that_a_failed_rebuild_leaves_nothing() {
        // Recreating is delete-then-sync, and the sync is the half that can
        // fail (no network, an unresolvable lock file). The user is agreeing
        // to that risk, so it has to be on screen before they agree.
        let text = recreate_confirmation("api", Path::new("/p/api/.venv"));

        assert!(
            text.contains("deleted"),
            "the destructive half must be named; got {text}"
        );
        assert!(
            text.to_lowercase().contains("cannot be restored"),
            "a failed rebuild leaving nothing must be stated; got {text}"
        );
    }
}
