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
use crate::theme;
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

/// What the detail header's one-press terminal button should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenPlan {
    /// Open this terminal now.
    Open(TerminalChoice),
    /// Nothing is configured, so find out what is installed and ask again.
    Detect,
    /// Nothing configured and nothing installed. There is no terminal to
    /// open, and the caller says so rather than spawning something arbitrary.
    Nothing,
}

/// Decides [`OpenPlan`] from what is configured and, once it is known, what
/// is installed.
///
/// `detected` is `None` before any probe has run and `Some` afterwards --
/// which is what separates "we have not looked yet" from "we looked and found
/// nothing", two states an empty slice alone cannot tell apart.
///
/// A configured default wins whatever it is, without probing at all: the user
/// said so, substituting another terminal because theirs is not on the
/// detected list would be the application overruling a setting, and seven
/// `which` calls the answer does not depend on are seven the user waits
/// through before their window appears.
///
/// A function rather than a `match` inside `update`, and the reason is a test
/// that lied: both branches of that `match` returned a `Task`, and no
/// assertion available to a test could tell one from the other -- so "opens
/// without probing" passed just as happily against an implementation that
/// always probed.
pub fn one_press_plan(
    default: Option<&TerminalChoice>,
    detected: Option<&[TerminalKind]>,
) -> OpenPlan {
    if let Some(choice) = default {
        return OpenPlan::Open(choice.clone());
    }
    match detected {
        None => OpenPlan::Detect,
        Some([]) => OpenPlan::Nothing,
        Some(kinds) => OpenPlan::Open(TerminalChoice::Detected(kinds[0])),
    }
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

/// The pending "syncing will delete this environment" confirmation.
///
/// `project_id`, not an index, for the reason [`ConfirmRecreate`]'s own doc
/// gives at length: an index is only meaningful against the list as it stood
/// when the confirmation opened.
///
/// `venv_path`, `from` and `to` are captured when it opens rather than
/// re-derived by `view`: the sentence the user agrees to and the directory
/// that gets deleted must be the same one.
pub struct ConfirmPinChange {
    pub project_id: Uuid,
    pub label: String,
    pub venv_path: PathBuf,
    /// The version the environment is on now.
    pub from: String,
    /// The version it is pinned to.
    pub to: String,
}

/// What the user agrees to before a pinned Sync deletes an environment.
///
/// Worded like [`recreate_confirmation`] because it is the same action: uv
/// does not migrate an environment to another interpreter, it removes the
/// directory and builds a new one. Verified against uv 0.12.1, which prints
/// `Removed virtual environment at: .venv`.
pub fn pin_change_confirmation(label: &str, venv_path: &Path, from: &str, to: &str) -> String {
    format!(
        "{label} is pinned to Python {to}, but its environment is on {from}.\n\n\
         Syncing will delete {} and build a new one on {to}. Anything installed \
         in it that is not in the lock file will be lost.\n\n\
         If the rebuild fails -- no network, a lock file that no longer resolves -- \
         the old environment has already been deleted and cannot be restored.",
        venv_path.display()
    )
}

/// Renders it. Deliberately dumb, like [`confirm_view`]: the sentence worth
/// testing is [`pin_change_confirmation`]'s.
pub fn confirm_pin_view<'a>(confirm: &ConfirmPinChange) -> iced::Element<'a, Message> {
    iced::widget::container(
        iced::widget::column![
            iced::widget::text(pin_change_confirmation(
                &confirm.label,
                &confirm.venv_path,
                &confirm.from,
                &confirm.to,
            ))
            .size(theme::BODY),
            iced::widget::row![
                iced::widget::button(iced::widget::text("Delete and rebuild").size(theme::BODY))
                    .on_press(Message::PinChangeConfirmed)
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_danger),
                iced::widget::button(iced::widget::text("Cancel").size(theme::BODY))
                    .on_press(Message::PinChangeCancelled)
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_quiet),
            ]
            .spacing(theme::SPACE_2),
        ]
        .spacing(theme::SPACE_3),
    )
    .padding(theme::SPACE_3)
    .style(theme::panel)
    .into()
}

/// The pending "this environment will stop existing" confirmation.
///
/// Removing an environment is the same act as Recreate venv -- a directory
/// full of installed packages stops being there -- so it asks the same way,
/// naming the directory rather than asking "are you sure?".
pub struct ConfirmEnvironmentRemoval {
    pub project_id: Uuid,
    pub label: String,
    pub location: bombadil_core::model::VenvLocation,
    pub venv_path: PathBuf,
}

/// What the user agrees to before an environment is removed.
///
/// Says plainly that the directory is *not* deleted from disk. Bombadil keeps
/// an account of environments it does not own, and removing one from that
/// account is not the same as destroying it -- claiming otherwise would be
/// the more alarming lie of the two.
pub fn environment_removal_confirmation(label: &str, venv_path: &Path) -> String {
    format!(
        "Remove this environment from {label}?\n\n\
         {} stays on disk -- Bombadil stops keeping an account of it, and will \
         not sync it or open a terminal on it again. Delete the directory \
         yourself if you want the space back.",
        venv_path.display()
    )
}

/// Renders it. Deliberately dumb, like the other two confirmations: the
/// sentence worth testing is `environment_removal_confirmation`'s.
pub fn confirm_environment_removal_view<'a>(
    confirm: &ConfirmEnvironmentRemoval,
) -> iced::Element<'a, Message> {
    iced::widget::container(
        iced::widget::column![
            iced::widget::text(environment_removal_confirmation(
                &confirm.label,
                &confirm.venv_path,
            ))
            .size(theme::BODY),
            iced::widget::row![
                iced::widget::button(iced::widget::text("Remove").size(theme::BODY))
                    .on_press(Message::EnvironmentRemovalConfirmed)
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_danger),
                iced::widget::button(iced::widget::text("Cancel").size(theme::BODY))
                    .on_press(Message::EnvironmentRemovalCancelled)
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_quiet),
            ]
            .spacing(theme::SPACE_2),
        ]
        .spacing(theme::SPACE_3),
    )
    .padding(theme::SPACE_3)
    .style(theme::panel)
    .into()
}

/// Renders the pending Recreate venv confirmation. Deliberately dumb, like
/// `view`: the sentence worth testing is `recreate_confirmation`'s.
///
/// The confirmation sentence is one prose block, not split into a mono span
/// for the venv path -- splitting it would mean `view` re-deriving the path's
/// position in the sentence independently of the string
/// `recreate_confirmation` returns and this module's own tests prove, which
/// is exactly the "two derivations that could drift" risk `ConfirmRecreate`'s
/// own doc warns against for `venv_path` itself.
///
/// Recreate venv is the only action in the application that destroys a
/// user's work (`venv_path::delete`'s only call site is
/// `Message::RecreateVenvConfirmed`, gated on this confirmation), so its
/// confirm button is the only control anywhere styled with `boot`. `INK`
/// text on it, not the usual `PARCHMENT`, because `boot`'s yellow is far
/// too light for `PARCHMENT` to read against. Cancel gets no style of its
/// own -- an unstyled button already paints from the theme's primary colour
/// (`jacket`), the same reasoning `detail::view` gives for the Sync button.
pub fn confirm_view<'a>(confirm: &ConfirmRecreate) -> iced::Element<'a, Message> {
    iced::widget::container(
        iced::widget::column![
            iced::widget::text(recreate_confirmation(&confirm.label, &confirm.venv_path))
                .size(theme::BODY),
            iced::widget::row![
                iced::widget::button(iced::widget::text("Delete and recreate").size(theme::BODY))
                    .on_press(Message::RecreateVenvConfirmed)
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_danger),
                iced::widget::button(iced::widget::text("Cancel").size(theme::BODY))
                    .on_press(Message::RecreateVenvCancelled)
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_quiet),
            ]
            .spacing(theme::SPACE_2),
        ]
        .spacing(theme::SPACE_3),
    )
    .padding(theme::SPACE_3)
    .style(theme::panel)
    .into()
}

/// Renders the open context menu. Deliberately dumb: all the logic worth
/// testing lives in `ordered`, `activation_warning` and `submenu`.
pub fn view<'a>(
    menu: &ContextMenu,
    label: &str,
    default: Option<&TerminalChoice>,
    waiting: Option<&str>,
) -> iced::Element<'a, Message> {
    let index = menu.project_index;
    let enabled = waiting.is_none();

    let mut column = iced::widget::column![
        // Which project this menu acts on. It is opened by a right-press that
        // deliberately does not move the selection, so without this the menu
        // is a list of destructive actions with no subject named.
        iced::widget::container(
            iced::widget::text(label.to_string())
                .font(theme::FONT_PROSE_SEMIBOLD)
                .size(theme::LABEL)
                .style(theme::text_muted)
        )
        .padding([theme::SPACE_1, theme::SPACE_3]),
        theme::hairline_row(),
        section_heading("Open terminal"),
    ]
    .spacing(0);

    for option in submenu(menu.terminals.clone(), default) {
        match option {
            TerminalOption::Detected(kind) => {
                column = column.push(entry(
                    terminal_name(kind),
                    // The warning is a second line in `slate` rather than a
                    // suffix on the label: appended, it made every row as
                    // wide as the longest sentence in the file and turned the
                    // menu into a paragraph.
                    Some(activation_warning(kind)),
                    Message::OpenTerminalRequested(index, TerminalChoice::Detected(kind)),
                ));
            }
            // Only offered as a live choice once the user has actually
            // configured a custom template -- an empty one is refused by
            // `terminal::argv::build` anyway, so there is nothing useful to
            // dispatch without it.
            TerminalOption::Custom => {
                if let Some(TerminalChoice::Custom { template }) = default {
                    column = column.push(entry(
                        "Custom",
                        None,
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
        .push(theme::hairline_row())
        .push(theme::wait_tip(
            entry(
                "Sync",
                None,
                enabled.then_some(Message::SyncRequested(index)),
            ),
            waiting,
        ))
        .push(entry(
            "Open folder",
            None,
            Message::OpenFolderRequested(index),
        ))
        .push(theme::hairline_row())
        // The two below the second line are the ones that take something
        // away. Grouped, not coloured: `boot` belongs to the confirm button
        // that actually destroys, and spending it on a menu entry that only
        // opens a confirmation would spend the ration twice for one action.
        //
        // Opens the confirmation, never the deletion -- see
        // `Message::RecreateVenvRequested`.
        .push(theme::wait_tip(
            entry(
                "Recreate venv",
                None,
                enabled.then_some(Message::RecreateVenvRequested(index)),
            ),
            waiting,
        ))
        .push(entry(
            "Remove from list",
            None,
            Message::RemoveProjectRequested(index),
        ))
        .into()
}

/// A heading above a group of entries. Not itself pressable.
fn section_heading<'a>(text: &'static str) -> iced::Element<'a, Message> {
    iced::widget::container(
        iced::widget::text(text)
            .size(theme::LABEL)
            .style(theme::text_muted),
    )
    .padding([theme::SPACE_1, theme::SPACE_3])
    .into()
}

/// One pressable row: a full-width bare button, left-aligned, with an
/// optional second line of detail beneath the label.
///
/// The whole menu used to be a column of default-styled buttons, each sized
/// to its own text, stacked in a centred card -- a bunch of buttons rather
/// than a menu. A menu row fills its panel, reacts on hover, and reads as one
/// of a list.
fn entry<'a>(
    label: &'a str,
    detail: Option<&'a str>,
    message: impl Into<Option<Message>>,
) -> iced::Element<'a, Message> {
    let mut content = iced::widget::column![iced::widget::text(label).size(theme::BODY)];
    if let Some(detail) = detail.filter(|d| !d.is_empty()) {
        content = content.push(
            iced::widget::text(detail.trim())
                .size(theme::LABEL)
                .style(theme::text_muted),
        );
    }

    iced::widget::button(content)
        .on_press_maybe(message.into())
        .width(iced::Length::Fill)
        .padding([theme::SPACE_1, theme::SPACE_3])
        .style(theme::button_bare(theme::Swatch::Parchment))
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
    fn a_fully_activating_terminal_carries_no_warning_line() {
        // `entry` renders the warning as a second line only when it is
        // non-empty, so an empty string here is what keeps a healthy
        // terminal's row a single line. A warning that was never empty would
        // put a line of reassurance under every entry and make the real ones
        // invisible.
        assert_eq!(activation_warning(TerminalKind::Kitty), "");
        assert!(
            activation_warning(TerminalKind::TerminalApp).contains("not activated"),
            "got {:?}",
            activation_warning(TerminalKind::TerminalApp)
        );
    }

    #[test]
    fn a_configured_default_opens_immediately_without_probing() {
        // Two things at once, and the second is the one a `Task`-shaped
        // assertion could not make: the configured terminal is chosen, *and*
        // the plan is not `Detect`. An implementation that probed anyway and
        // then honoured the default would produce the same window a moment
        // later and fail here, which is the point.
        let default = TerminalChoice::Detected(TerminalKind::Konsole);
        assert_eq!(
            one_press_plan(Some(&default), None),
            OpenPlan::Open(TerminalChoice::Detected(TerminalKind::Konsole))
        );
    }

    #[test]
    fn a_configured_default_is_not_overruled_by_what_is_installed() {
        // Konsole configured, Konsole not among the detected: still Konsole.
        // The user set it; the application does not get to substitute.
        let default = TerminalChoice::Detected(TerminalKind::Konsole);
        assert_eq!(
            one_press_plan(
                Some(&default),
                Some(&[TerminalKind::Kitty, TerminalKind::Xterm])
            ),
            OpenPlan::Open(TerminalChoice::Detected(TerminalKind::Konsole))
        );
    }

    #[test]
    fn nothing_configured_means_look_before_opening() {
        assert_eq!(one_press_plan(None, None), OpenPlan::Detect);
    }

    #[test]
    fn with_nothing_configured_the_first_detected_is_used() {
        assert_eq!(
            one_press_plan(None, Some(&[TerminalKind::Kitty, TerminalKind::Xterm])),
            OpenPlan::Open(TerminalChoice::Detected(TerminalKind::Kitty))
        );
    }

    #[test]
    fn having_looked_and_found_nothing_is_not_the_same_as_not_having_looked() {
        // The whole reason `detected` is an `Option<&[_]>`. Collapsed to a
        // bare slice, "no terminals installed" and "no probe has run yet"
        // are the same empty list -- and the button would probe forever.
        assert_eq!(one_press_plan(None, Some(&[])), OpenPlan::Nothing);
        assert_ne!(one_press_plan(None, Some(&[])), one_press_plan(None, None));
    }

    #[test]
    fn the_removal_confirmation_says_the_directory_is_not_deleted() {
        // Bombadil keeps an account of environments it does not own. Claiming
        // to delete one would be the more alarming lie of the two, and a user
        // who believed it would go looking for a directory that is still
        // there.
        let text = environment_removal_confirmation("api", Path::new("/envs/api-311"));
        assert!(text.contains("/envs/api-311"), "got {text}");
        assert!(
            text.contains("stays on disk"),
            "the user must be told the directory survives; got {text}"
        );
        assert!(text.contains("api"), "got {text}");
    }

    #[test]
    fn the_pin_change_confirmation_names_both_versions_and_the_directory() {
        // "Are you sure?" is not a confirmation. The user has to see which
        // directory stops existing and what it is being rebuilt as -- and
        // that it is *deleted*, not migrated, because uv does not migrate.
        let text = pin_change_confirmation(
            "api",
            Path::new("/home/t/.venvs/api-1f2e3d"),
            "3.13.14",
            "3.12",
        );
        assert!(text.contains("/home/t/.venvs/api-1f2e3d"), "got {text}");
        assert!(text.contains("3.13.14"), "got {text}");
        assert!(text.contains("3.12"), "got {text}");
        assert!(
            text.contains("deleted"),
            "the user must be told the environment is deleted, not rebuilt; got {text}"
        );
        assert!(
            text.contains("lock file will be lost"),
            "the cost the user cannot see -- anything not in the lock file -- \
             has to be stated; got {text}"
        );
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
