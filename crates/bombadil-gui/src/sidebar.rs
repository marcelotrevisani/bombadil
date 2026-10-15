//! The project sidebar: one entry per registered project, marked with the
//! state glyph shared by every pane in the application (`theme::state_glyph`).
//!
//! Spec §8: registered-without-a-venv is a normal state, not an error -- a
//! user adds a project first and creates the environment afterwards -- so a
//! project without a venv is still listed, marked `Absent` rather than
//! treated as a problem.
//!
//! This is the first consumer of `state_glyph`, replacing the ad-hoc
//! filled/hollow marker the sidebar used to draw for itself. That marker
//! only had two states, so a venv that exists but whose Python does not
//! satisfy the project's `requires-python` rendered exactly like a healthy
//! one. `state` below is what tells those apart, mapping to `theme::State`'s
//! third state, `Drifted`.
//!
//! `theme::GLYPH_COLUMN_WIDTH` was not retrofitted onto this row when it was
//! written -- every other pane's glyph column (dependency, member and index
//! rows) is fixed-width, but the sidebar's own glyph sized itself to its
//! content, so the label following it did not start at the offset the rest
//! of the application already trained the eye to expect. Fixed alongside
//! Task 5.

use bombadil_core::model::{Config, PythonPin, VenvLocation};
use std::path::PathBuf;

use crate::app::Message;
use crate::interpreter;
use crate::theme::{self, State};

/// One environment beneath a project in the tree.
///
/// Identified by `location`, which is also how `Message::VenvProbed` finds the
/// row its result belongs to: a project can have several environments in
/// flight at once, and an index would be meaningless by the time a probe
/// lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentRow {
    pub location: VenvLocation,
    /// Where it resolves to. Shown in the tree, and what the user recognises
    /// as the environment.
    pub path: PathBuf,
    /// From `pyvenv.cfg`, once the probe lands. `None` means the directory is
    /// not there -- or has not been looked at yet.
    pub python_version: Option<String>,
    /// This environment's own pin.
    pub pin: PythonPin,
    /// Whether Sync, Open terminal and Recreate act on this one.
    pub active: bool,
}

/// One project in the sidebar, with its environments beneath it.
pub struct Entry {
    /// The project this row is for. Carried because the environment controls
    /// beneath it act by id, not by position -- the list is edited while the
    /// application is running.
    pub id: uuid::Uuid,
    pub label: String,
    /// The project's `requires-python`, re-read alongside the venv probe
    /// (`app::probe_every_venv`) rather than stored on `Project` -- see that
    /// module's doc for why. `None` covers both "not probed yet" and "the
    /// manifest declares none"; `state` treats both the same way
    /// `interpreter::version_satisfies` does, as nothing to check against
    /// rather than a mismatch.
    pub requires_python: Option<String>,
    /// Every environment this project has, in config order.
    pub environments: Vec<EnvironmentRow>,
    /// Whether the tree shows them. Persisted on the project, so a project
    /// you collapse stays collapsed.
    pub expanded: bool,
    /// The project directory's git branch and status, once probed. `None`
    /// before then and for a project that is not in a repository.
    pub git: Option<bombadil_core::git::GitStatus>,
}

/// Builds one `Entry` per project, in config order.
///
/// Every entry starts hollow: `app::boot`'s background probes fill in real
/// venv status and `requires-python` asynchronously through
/// `Message::VenvProbed`. Probing here inline would block the render thread
/// on a `pyvenv.cfg`/`pyproject.toml` read, which is exactly what those
/// background probes exist to avoid.
pub fn entries(config: &Config) -> Vec<Entry> {
    config
        .projects
        .iter()
        .map(|project| Entry {
            id: project.id,
            label: project.label.clone(),
            requires_python: None,
            environments: project
                .environments
                .iter()
                .map(|environment| EnvironmentRow {
                    location: environment.location.clone(),
                    // Resolved here rather than left blank: the path is what
                    // the user recognises an environment by, and a row that
                    // said nothing until a probe landed would be a row you
                    // could not read. A location that cannot resolve shows
                    // empty, which is what `venv_path::resolve` refusing it
                    // means.
                    path: bombadil_core::venv_path::resolve(project, environment, &config.settings)
                        .unwrap_or_default(),
                    python_version: None,
                    pin: environment.python.clone(),
                    active: environment.location == project.active,
                })
                .collect(),
            expanded: project.expanded,
            git: None,
        })
        .collect()
}

/// The one glyph a project shows for all of its environments.
///
/// Drift beats absent beats present, because drift is the only one of the
/// three that needs the user: an environment that disagrees with its pin will
/// be deleted and rebuilt by the next Sync. A project with no environments at
/// all reads as absent -- there is nothing there, which is what absent means.
///
/// One glyph cannot say more than "the worst of these", and the tree is where
/// the detail lives.
pub fn rolled_up(rows: &[EnvironmentRow], requires_python: Option<&str>) -> State {
    if rows.is_empty() {
        return State::Absent;
    }
    // Every row is looked at, not just up to the first non-present one: an
    // early return on `Absent` would hide a drifted environment further down
    // the list, which is precisely the one the user needs to see.
    let mut any_absent = false;
    for row in rows {
        match row_state(row, requires_python) {
            State::Drifted => return State::Drifted,
            State::Absent => any_absent = true,
            State::Present => {}
        }
    }
    if any_absent {
        State::Absent
    } else {
        State::Present
    }
}

/// The message an environment row sends when pressed.
///
/// Returns a `Message`, not an `Option<Message>`, and that is the point: it
/// used to be `on_press_maybe((!active).then_some(...))`, so the row that was
/// already the project's default had no press handler. A project with one
/// environment has it default always, so its row was permanently dead -- and
/// because this press is also what *selects the project*, that project could
/// not be reached by clicking its environment at all.
///
/// A type that cannot express "not pressable" is a better guarantee than a
/// test, and this bug had no test: it lived entirely in `view`, which this
/// crate does not test by convention.
pub fn environment_press(project: usize, row: &EnvironmentRow) -> Message {
    Message::EnvironmentActivated(project, row.location.clone())
}

/// The indices of `entries` the sidebar shows under `filter`: those whose
/// label or project folder contains it (case-insensitively), and always the
/// selected one, so the project the detail pane shows never vanishes from
/// the list. Only what is shown changes; messages keep real indices.
pub fn visible(
    entries: &[Entry],
    dirs: &[std::path::PathBuf],
    filter: &str,
    selected: Option<usize>,
) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(i, entry)| {
            selected == Some(*i)
                || crate::deps::name_matches(&entry.label, filter)
                // The folder's own name, not its whole path: every project
                // shares its parents (the home folder, "projects"), so
                // matching them would make short queries match everything.
                || dirs.get(*i).and_then(|d| d.file_name()).is_some_and(|name| {
                    crate::deps::name_matches(&name.to_string_lossy(), filter)
                })
        })
        .map(|(i, _)| i)
        .collect()
}

/// What to say when an environment's own pin is not what it runs: the pin
/// was changed and nothing has rebuilt it yet. `None` when unpinned, not yet
/// probed, or agreeing.
pub fn pin_note(row: &EnvironmentRow) -> Option<String> {
    let (Some(version), PythonPin::Version(pinned)) = (&row.python_version, &row.pin) else {
        return None;
    };
    (!interpreter::satisfies_pin(version, &row.pin))
        .then(|| format!("pinned {pinned}: sync to apply"))
}

/// The glyph for one environment row.
pub fn row_state(row: &EnvironmentRow, requires_python: Option<&str>) -> State {
    match &row.python_version {
        None => State::Absent,
        Some(version) => {
            // Two independent reasons to drift, and the environment has to
            // clear both: the manifest's `requires-python`, and this
            // environment's own pin.
            let agrees = interpreter::version_satisfies(version, requires_python)
                && interpreter::satisfies_pin(version, &row.pin);
            if agrees {
                State::Present
            } else {
                State::Drifted
            }
        }
    }
}

/// Maps one sidebar entry to the glyph state it should render.
///
/// - No venv: [`State::Absent`].
/// - A venv whose Python satisfies the project's `requires-python`:
///   [`State::Present`].
/// - A venv whose Python does not: [`State::Drifted`] -- present, but not
///   agreeing, which is the whole reason the glyph has three states rather
///   than two (see `theme::State`'s own doc).
///
/// Reuses `interpreter::version_satisfies` for the check rather than a
/// second comparator, including its fallback direction: a `requires-python`
/// this parser cannot read is treated as satisfied, not as a mismatch, so a
/// working environment is never marked drifted just because its manifest's
/// constraint used syntax outside the subset that parser handles.
pub fn state(entry: &Entry) -> State {
    rolled_up(&entry.environments, entry.requires_python.as_deref())
}

/// Renders the entry list. Deliberately dumb: all the logic worth testing
/// lives in `entries` and `state` above -- this crate's convention is that
/// `view` itself carries no test, since it returns an opaque `Element` no
/// test can drive.
///
/// The selected row gets a `jacket` background and its label switches to the
/// semibold prose face; every row's label is prose (Atkinson) at body size,
/// and beneath it the Python version renders in the mono face (Plex) at data
/// size in `slate` -- or the words "no env" in the *prose* face, since "no
/// env" is words for the user to read, not a value to compare character by
/// character. That is the rule this task makes hold for the first time: a
/// version string is data, prose is not, no exceptions for how short it is.
///
/// Each row is wrapped in a `mouse_area` so a right-click opens the project's
/// context menu (spec §8) without disturbing the left-click select behaviour
/// the button already provides.
/// `busy` is the project running a command and the spinner's angle: its row
/// shows the spinner where the state glyph would be. `waiting` is that
/// command's label, for the disabled Sync's tooltip.
///
/// Hovering a row (or selecting it) shows its actions -- Sync, terminal and
/// More, which opens the right-click menu -- so nothing depends on knowing
/// to right-click.
pub fn view<'a>(
    entries: &[Entry],
    selected: Option<usize>,
    busy: Option<(uuid::Uuid, f32)>,
    waiting: Option<&str>,
    shown: &[usize],
) -> iced::Element<'a, Message> {
    let mut list = iced::widget::column![].spacing(theme::SPACE_2);
    for (i, entry) in entries.iter().enumerate() {
        if !shown.contains(&i) {
            continue;
        }
        let is_selected = selected == Some(i);
        let glyph = theme::state_glyph(state(entry));

        let label_font = if is_selected {
            theme::FONT_PROSE_SEMIBOLD
        } else {
            theme::FONT_PROSE
        };

        // The active environment's version, which is what Sync and Open
        // terminal act on. The rest are in the tree below.
        let version_line: iced::Element<'_, Message> = match entry
            .environments
            .iter()
            .find(|row| row.active)
            .and_then(|row| row.python_version.as_ref())
        {
            Some(version) => iced::widget::text(version.clone())
                .font(theme::FONT_DATA)
                .size(theme::DATA)
                .style(theme::text_muted)
                .into(),
            // Words, not a version, so prose rather than the data face --
            // the rule `theme` sets and this row has always followed.
            None => iced::widget::text("no env")
                .font(theme::FONT_PROSE)
                .size(theme::DATA)
                .style(theme::text_muted)
                .into(),
        };

        // The disclosure control. A project with one environment still shows
        // it: a tree that collapsed the single case would make "how many are
        // there" a question you answer by counting rows in two layouts.
        let disclosure = iced::widget::button(
            iced::widget::text(if entry.expanded {
                "\u{25be}"
            } else {
                "\u{25b8}"
            })
            .size(theme::BODY),
        )
        .on_press(Message::ProjectExpandToggled(i))
        .padding(0.0)
        .width(theme::DISCLOSURE_WIDTH)
        .style(theme::button_bare(theme::Swatch::Slate));

        // The selection marker: a `jacket` bar down the row's leading edge,
        // and the row's fill becomes the `ink` of the pane it opens, so the
        // selected project reads as continuous with what it is showing.
        //
        // It used to be a `jacket` *fill* under parchment text, which is
        // 3.1:1 -- below WCAG AA at every size in this application. An edge
        // marker is both legible and the vocabulary the tab bar already uses
        // for the same job; see `theme`'s contrast tests.
        let marker = iced::widget::container(
            iced::widget::Space::new()
                .width(theme::SELECTED_MARKER)
                .height(iced::Length::Fill),
        )
        .height(iced::Length::Fill)
        .style(move |t| iced::widget::container::Style {
            background: is_selected.then_some(iced::Background::Color(theme::palette(t).jacket)),
            ..iced::widget::container::Style::default()
        });

        let content = iced::widget::row![
            marker,
            iced::widget::column![
                iced::widget::row![
                    match busy {
                        Some((id, angle)) if id == entry.id => iced::widget::container(
                            crate::icons::spinner(angle, 12.0, theme::Swatch::Slate)
                        )
                        .width(theme::GLYPH_COLUMN_WIDTH)
                        .into(),
                        _ => theme::tip(
                            iced::widget::text(glyph.glyph.to_string())
                                .size(theme::BODY)
                                .style(theme::tinted(glyph.swatch))
                                .width(theme::GLYPH_COLUMN_WIDTH),
                            theme::state_meaning(state(entry)),
                            iced::widget::tooltip::Position::Right,
                        ),
                    },
                    iced::widget::text(entry.label.clone())
                        .font(label_font)
                        .size(theme::BODY),
                ]
                .spacing(theme::SPACE_1),
                // Indented to start under the label, not under the glyph.
                iced::widget::row![
                    // Less the gap: this row spaces wider than the label's.
                    iced::widget::Space::new().width(theme::GLYPH_COLUMN_WIDTH - theme::SPACE_1),
                    version_line
                ]
                .push(entry.git.as_ref().map(|git| {
                    // `*` for uncommitted changes, the mark a shell
                    // prompt uses; the detail header says which.
                    let dirty = if git.is_clean() { "" } else { "*" };
                    iced::widget::text(format!("{}{dirty}", git.branch))
                        .font(theme::FONT_DATA)
                        .size(theme::DATA)
                        .style(theme::text_muted)
                        // One line, cut at the row's edge: a long branch
                        // wrapped out of the row and under its neighbour.
                        .wrapping(iced::widget::text::Wrapping::None)
                }))
                .spacing(theme::SPACE_2),
            ]
            .spacing(theme::SPACE_1),
        ]
        .spacing(theme::SPACE_2);

        // A fixed height, as environment rows have: the marker fills the
        // row's height, and with the row left to shrink the two resolved
        // each other to one squashed line that hid the version and branch.
        let row = iced::widget::button(content)
            .on_press(Message::ProjectSelected(i))
            .padding(theme::SPACE_1)
            .width(iced::Length::Fill)
            .height(theme::PROJECT_ROW_HEIGHT)
            .clip(true)
            .style(move |t, status| iced::widget::button::Style {
                background: match (is_selected, status) {
                    (true, _) => Some(iced::Background::Color(theme::palette(t).ink)),
                    (false, iced::widget::button::Status::Hovered) => {
                        Some(iced::Background::Color(theme::palette(t).ink))
                    }
                    _ => None,
                },
                text_color: theme::palette(t).parchment,
                border: iced::Border {
                    radius: theme::RADIUS.into(),
                    ..iced::Border::default()
                },
                ..iced::widget::button::Style::default()
            });
        let actions = row_actions(i, is_selected, waiting);
        let row: iced::Element<'a, Message> = if is_selected {
            iced::widget::stack![row, actions].into()
        } else {
            iced::widget::hover(row, actions)
        };
        list = list.push(
            iced::widget::mouse_area(
                iced::widget::row![disclosure, row]
                    .spacing(theme::SPACE_1)
                    .align_y(iced::Alignment::Center),
            )
            .on_right_press(Message::ContextMenuOpened(i)),
        );

        if entry.expanded {
            for environment in &entry.environments {
                list = list.push(environment_row(
                    i,
                    entry.id,
                    environment,
                    entry.environments.len() > 1,
                    entry.requires_python.as_deref(),
                ));
            }
            // The add control sits with the environments it adds to.
            list = list.push(add_environment_row(entry.id));
        }
    }
    list.into()
}

/// The collapsed sidebar: the expand button, then one state glyph per
/// project (the spinner for the busy one), its label in a tooltip. Pressing
/// a glyph selects that project, as its full row would.
pub fn rail<'a>(
    entries: &[Entry],
    selected: Option<usize>,
    busy: Option<(uuid::Uuid, f32)>,
    shown: &[usize],
) -> iced::Element<'a, Message> {
    use crate::icons::{self, Icon};
    use crate::shortcuts::{Shortcut, keys};
    let mut column = iced::widget::column![icons::icon_button(
        Icon::PanelLeft,
        format!("Expand sidebar ({})", keys(Shortcut::ToggleSidebar)),
        Some(Message::SidebarToggled),
    )]
    .spacing(theme::SPACE_1)
    .align_x(iced::Alignment::Center);
    for (i, entry) in entries.iter().enumerate() {
        if !shown.contains(&i) {
            continue;
        }
        let glyph = theme::state_glyph(state(entry));
        let mark: iced::Element<'a, Message> = match busy {
            Some((id, angle)) if id == entry.id => {
                icons::spinner(angle, 12.0, theme::Swatch::Slate)
            }
            _ => iced::widget::text(glyph.glyph.to_string())
                .size(theme::BODY)
                .style(theme::tinted(glyph.swatch))
                .into(),
        };
        column = column.push(theme::tip(
            iced::widget::button(mark)
                .on_press(Message::ProjectSelected(i))
                .padding([theme::SPACE_1, theme::SPACE_2])
                .style(theme::button_choice(selected == Some(i))),
            entry.label.clone(),
            iced::widget::tooltip::Position::Right,
        ));
    }
    iced::widget::scrollable(column)
        .height(iced::Length::Fill)
        .into()
}

/// The quiet icon buttons a project row shows on hover, on an `ink` pill
/// (the hovered row's own fill) at its right edge. Only the pill takes
/// presses; the rest of the row still selects.
fn row_actions<'a>(i: usize, selected: bool, waiting: Option<&str>) -> iced::Element<'a, Message> {
    use crate::icons::{Icon, icon_button};
    use crate::shortcuts::{Shortcut, hint};
    let sync = match waiting {
        None => icon_button(
            Icon::Refresh,
            hint("Sync", Shortcut::Sync, selected),
            Some(Message::SyncRequested(i)),
        ),
        Some(label) => icon_button(Icon::Refresh, format!("Wait for {label} to finish"), None),
    };
    let pill = iced::widget::container(iced::widget::row![
        sync,
        icon_button(
            Icon::Terminal,
            hint("Open a terminal", Shortcut::Terminal, selected),
            Some(Message::OpenTerminalHereRequested(i)),
        ),
        icon_button(
            Icon::Ellipsis,
            "More: open folder, recreate, remove",
            Some(Message::ContextMenuOpened(i)),
        ),
    ])
    .style(|t| iced::widget::container::Style {
        background: Some(iced::Background::Color(theme::palette(t).ink)),
        border: iced::Border {
            radius: theme::RADIUS.into(),
            ..iced::Border::default()
        },
        ..iced::widget::container::Style::default()
    });
    iced::widget::container(pill)
        .width(iced::Length::Fill)
        .height(iced::Length::Fill)
        .align_x(iced::alignment::Horizontal::Right)
        // On the label's line, leaving the version and branch beneath it
        // readable while the row is selected.
        .align_y(iced::alignment::Vertical::Top)
        .into()
}

/// One environment beneath its project.
///
/// A single click makes it active, because that is not destructive: nothing
/// is deleted, the next Sync simply builds against a different directory.
/// Recreate and remove are the destructive ones, and both ask first.
fn environment_row<'a>(
    project: usize,
    project_id: uuid::Uuid,
    row: &EnvironmentRow,
    removable: bool,
    requires_python: Option<&str>,
) -> iced::Element<'a, Message> {
    let glyph = theme::state_glyph(row_state(row, requires_python));
    // The directory's own name, which is what distinguishes two environments
    // of one project -- `.venv` from `.venv-3.11`.
    let name = row
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| row.path.display().to_string());
    let version = match &row.python_version {
        Some(version) => version.clone(),
        None => "not created".to_string(),
    };
    let active = row.active;
    // "default" rather than "active", which is the user's own word for it:
    // the environment a project uses when you select it. The model calls it
    // `active`; nothing on screen has to.

    // The active marker: a `jacket` bar down the leading edge, the same shape
    // a selected project uses. It replaced a full outline around the row,
    // which drew a heavy box the width of the sidebar and read as a control
    // rather than as a state.
    let marker = iced::widget::container(
        iced::widget::Space::new()
            .width(theme::SELECTED_MARKER)
            .height(iced::Length::Fill),
    )
    .height(iced::Length::Fill)
    .style(move |t| iced::widget::container::Style {
        background: active.then_some(iced::Background::Color(theme::palette(t).jacket)),
        ..iced::widget::container::Style::default()
    });

    let content = iced::widget::row![
        marker,
        iced::widget::text(glyph.glyph.to_string())
            .size(theme::BODY)
            .style(theme::tinted(glyph.swatch))
            .width(theme::GLYPH_COLUMN_WIDTH),
        iced::widget::column![
            iced::widget::row![
                iced::widget::text(name)
                    .font(theme::FONT_DATA)
                    .size(theme::DATA)
            ]
            .push(active.then(|| theme::badge("default")))
            .spacing(theme::SPACE_2)
            .align_y(iced::Alignment::Center),
            match pin_note(row) {
                Some(note) => iced::widget::text(format!("{version} \u{b7} {note}"))
                    .font(theme::FONT_DATA)
                    .size(theme::LABEL)
                    .style(theme::text_warning),
                None => iced::widget::text(version)
                    .font(theme::FONT_DATA)
                    .size(theme::LABEL)
                    .style(theme::text_muted),
            },
        ]
        .spacing(0),
    ]
    .spacing(theme::SPACE_1)
    .height(iced::Length::Fill)
    .align_y(iced::Alignment::Center);

    // Indented past the disclosure control, so a child starts where its
    // parent's glyph does rather than to the left of it. The row itself is
    // built exactly like a project's -- same marker, same glyph column -- so
    // the two read as one list at two depths.
    let location = row.location.clone();
    let pressable = iced::widget::row![
        iced::widget::Space::new()
            .width(theme::DISCLOSURE_WIDTH + theme::SPACE_2)
            .height(theme::TREE_ROW_HEIGHT),
        iced::widget::button(content)
            .on_press(environment_press(project, row))
            .width(iced::Length::Fill)
            .height(theme::TREE_ROW_HEIGHT)
            .padding(theme::SPACE_1)
            .style(move |t, status| iced::widget::button::Style {
                background: match (active, status) {
                    (true, _) => Some(iced::Background::Color(theme::palette(t).ink)),
                    (false, iced::widget::button::Status::Hovered) => {
                        Some(iced::Background::Color(theme::palette(t).ink))
                    }
                    _ => None,
                },
                text_color: theme::palette(t).parchment,
                border: iced::Border {
                    radius: theme::RADIUS.into(),
                    ..iced::Border::default()
                },
                ..iced::widget::button::Style::default()
            }),
    ]
    .spacing(0)
    .height(theme::TREE_ROW_HEIGHT);

    // Right-press removes, the same gesture the project rows already use for
    // their own menu -- and it opens the confirmation rather than removing,
    // so an accidental right-click costs nothing. Never offered for the last
    // one: a project with no environments has nothing to sync.
    if removable {
        iced::widget::mouse_area(pressable)
            .on_right_press(Message::EnvironmentRemoveRequested(project_id, location))
            .into()
    } else {
        pressable.into()
    }
}

/// The "add an environment" control beneath a project's environments.
///
/// Indented like an environment row so it reads as belonging to the same
/// list, and labelled rather than a bare `+`: an icon-only control with no
/// icon set behind it is one the user has to guess at.
///
/// Here rather than only in Preferences because "I need a way to add more
/// virtual environments" was reported *after* that editor shipped -- a
/// surface nobody can find is a surface that does not exist.
fn add_environment_row<'a>(project_id: uuid::Uuid) -> iced::Element<'a, Message> {
    iced::widget::row![
        iced::widget::Space::new()
            .width(theme::DISCLOSURE_WIDTH + theme::SPACE_2)
            .height(theme::TREE_ROW_HEIGHT),
        iced::widget::button(
            iced::widget::text("+ add environment")
                .size(theme::DATA)
                .style(theme::text_muted),
        )
        .on_press(Message::EnvironmentAdded(project_id))
        .width(iced::Length::Fill)
        .height(theme::TREE_ROW_HEIGHT)
        .padding(theme::SPACE_1)
        .style(theme::button_bare(theme::Swatch::Slate)),
    ]
    .spacing(0)
    .height(theme::TREE_ROW_HEIGHT)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str) -> EnvironmentRow {
        EnvironmentRow {
            location: VenvLocation::Custom {
                path: PathBuf::from(path),
            },
            path: PathBuf::from(path),
            python_version: None,
            pin: PythonPin::Unpinned,
            active: false,
        }
    }

    #[test]
    fn a_pin_the_environment_misses_says_sync_applies_it() {
        assert_eq!(
            pin_note(&pinned("3.11.9", "3.12").environments[0]).as_deref(),
            Some("pinned 3.12: sync to apply")
        );
        assert_eq!(pin_note(&pinned("3.12.4", "3.12").environments[0]), None);
        assert_eq!(
            pin_note(&entry(None, None).environments[0]),
            None,
            "nothing probed, nothing to claim"
        );
        assert_eq!(
            pin_note(&entry(Some("3.11.9"), None).environments[0]),
            None,
            "unpinned"
        );
    }

    fn named(label: &str) -> Entry {
        Entry {
            label: label.into(),
            ..entry(None, None)
        }
    }

    #[test]
    fn the_project_filter_matches_label_or_folder_and_keeps_the_selection() {
        let entries = [named("api"), named("worker"), named("web")];
        let dirs = [
            std::path::PathBuf::from("/code/api"),
            std::path::PathBuf::from("/code/jobs"),
            std::path::PathBuf::from("/code/site"),
        ];
        assert_eq!(visible(&entries, &dirs, "", None), [0, 1, 2]);
        assert_eq!(visible(&entries, &dirs, "WO", None), [1]);
        assert_eq!(visible(&entries, &dirs, "jobs", None), [1], "by folder too");
        assert_eq!(
            visible(&entries, &dirs, "code", None),
            Vec::<usize>::new(),
            "a parent every project shares narrows nothing, so it does not match"
        );
        assert_eq!(
            visible(&entries, &dirs, "web", Some(0)),
            [0, 2],
            "the selected project never vanishes"
        );
    }

    fn entry(python_version: Option<&str>, requires_python: Option<&str>) -> Entry {
        Entry {
            id: uuid::Uuid::nil(),
            label: "api".into(),
            requires_python: requires_python.map(String::from),
            environments: vec![EnvironmentRow {
                python_version: python_version.map(String::from),
                active: true,
                ..row("/p/api/.venv")
            }],
            expanded: true,
            git: None,
        }
    }

    /// An entry pinned to `pin`, with no `requires-python` in play -- so a
    /// test about the pin is not quietly also a test about the manifest.
    fn pinned(python_version: &str, pin: &str) -> Entry {
        Entry {
            environments: vec![EnvironmentRow {
                python_version: Some(python_version.into()),
                pin: PythonPin::Version(pin.into()),
                active: true,
                ..row("/p/api/.venv")
            }],
            ..entry(Some(python_version), None)
        }
    }

    #[test]
    fn every_environment_row_is_pressable_including_the_default_one() {
        // The press does two things -- makes this environment the default,
        // and selects its project -- and only the first is a no-op when it is
        // already the default. Gating on that made a one-environment
        // project's row dead, and with it the only way to reach that project
        // by clicking its environment.
        //
        // `environment_press` returns a `Message` rather than an `Option`, so
        // this is really asserting the type still says so.
        let default_row = EnvironmentRow {
            active: true,
            ..row("/p/api/.venv")
        };
        let other = EnvironmentRow {
            active: false,
            ..row("/p/api/.venv-311")
        };

        for row in [&default_row, &other] {
            let message = environment_press(0, row);
            assert!(
                matches!(message, Message::EnvironmentActivated(0, ref location)
                    if *location == row.location),
                "every row must send its own activation; got {message:?}"
            );
        }
    }

    #[test]
    fn the_projects_glyph_rolls_up_its_environments() {
        // One glyph cannot say more than "the worst of these", and the tree is
        // where the detail lives. Drift beats absent beats present, because
        // drift is the only one of the three that needs the user: an
        // environment that disagrees with its pin will be deleted and rebuilt
        // by the next Sync.
        let present = EnvironmentRow {
            python_version: Some("3.12".into()),
            pin: PythonPin::Version("3.12".into()),
            ..row("/p/api/.venv")
        };
        let absent = EnvironmentRow {
            python_version: None,
            ..row("/p/api/.venv-311")
        };
        let drifted = EnvironmentRow {
            python_version: Some("3.11".into()),
            pin: PythonPin::Version("3.12".into()),
            ..row("/p/api/.venv-x")
        };

        assert_eq!(
            rolled_up(std::slice::from_ref(&present), None),
            State::Present
        );
        assert_eq!(
            rolled_up(&[present.clone(), absent.clone()], None),
            State::Absent
        );
        assert_eq!(
            rolled_up(&[present, absent, drifted], None),
            State::Drifted,
            "drift outranks absent: an environment that disagrees with its pin \
             is the one that needs the user"
        );
    }

    #[test]
    fn a_project_with_no_environments_reads_as_absent() {
        // Reachable only between removing the last environment and adding
        // another. There is nothing there, which is what absent means.
        assert_eq!(rolled_up(&[], None), State::Absent);
    }

    #[test]
    fn each_environment_row_is_judged_against_its_own_pin() {
        // Two environments of one project carry different interpreters on
        // purpose. Judging both against one pin would mark whichever is not
        // the active one as drifted, forever.
        let ok = EnvironmentRow {
            python_version: Some("3.11".into()),
            pin: PythonPin::Version("3.11".into()),
            ..row("/p/api/.venv-311")
        };
        let also_ok = EnvironmentRow {
            python_version: Some("3.12".into()),
            pin: PythonPin::Version("3.12".into()),
            ..row("/p/api/.venv-312")
        };

        assert_eq!(row_state(&ok, None), State::Present);
        assert_eq!(row_state(&also_ok, None), State::Present);
        assert_eq!(rolled_up(&[ok, also_ok], None), State::Present);
    }

    #[test]
    fn a_venv_off_its_pin_reads_as_drift() {
        assert_eq!(state(&pinned("3.11.9", "3.12")), State::Drifted);
    }

    #[test]
    fn a_venv_on_its_pin_is_present_even_when_it_records_fewer_segments() {
        // The destructive-loop guard, at the level the user actually sees:
        // `pyvenv.cfg` records `3.12` for a venv built with `-p 3.12`, and
        // reading that as drift would make every Sync delete and rebuild it.
        assert_eq!(state(&pinned("3.12", "3.12.13")), State::Present);
    }

    #[test]
    fn an_unpinned_project_still_reports_drift_against_requires_python() {
        // The pin is an addition to the existing check, not a replacement.
        assert_eq!(state(&entry(Some("3.9.1"), Some(">=3.11"))), State::Drifted);
    }

    #[test]
    fn a_venv_on_its_pin_but_off_requires_python_still_drifts() {
        // Both checks have to hold. Satisfying the pin must not excuse a
        // manifest the environment cannot actually run.
        let entry = Entry {
            environments: vec![EnvironmentRow {
                python_version: Some("3.9.1".into()),
                pin: PythonPin::Version("3.9".into()),
                active: true,
                ..row("/p/api/.venv")
            }],
            ..entry(Some("3.9.1"), Some(">=3.11"))
        };
        assert_eq!(state(&entry), State::Drifted);
    }

    #[test]
    fn every_entry_starts_hollow_and_still_lists_the_project() {
        // Registered-without-a-venv is a normal state, not an error: the user
        // adds the project first and creates the environment afterwards.
        // Real versions arrive later through `Message::VenvProbed`, not from
        // `entries` itself.
        let entries = entries(&config_with(&["api"]));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].requires_python, None);
        assert!(
            entries[0]
                .environments
                .iter()
                .all(|row| row.python_version.is_none()),
            "no environment may claim a version before its probe lands"
        );
    }

    #[test]
    fn entries_follow_config_order_so_the_list_does_not_reshuffle() {
        let entries = entries(&config_with(&["web", "api"]));
        let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, vec!["web", "api"]);
    }

    #[test]
    fn no_venv_is_absent() {
        assert_eq!(state(&entry(None, Some(">=3.11"))), State::Absent);
    }

    #[test]
    fn a_venv_satisfying_requires_python_is_present() {
        assert_eq!(
            state(&entry(Some("3.12.4"), Some(">=3.11"))),
            State::Present
        );
    }

    #[test]
    fn a_venv_older_than_requires_python_is_drifted() {
        // This is the case the old filled/hollow marker could not represent:
        // the venv is there, but it is the wrong one.
        assert_eq!(
            state(&entry(Some("3.9.18"), Some(">=3.11"))),
            State::Drifted
        );
    }

    #[test]
    fn no_requirement_is_present_regardless_of_the_venv_version() {
        // Nothing to check against is not evidence of drift.
        assert_eq!(state(&entry(Some("3.9.18"), None)), State::Present);
    }

    #[test]
    fn an_unparseable_requirement_falls_back_to_present_not_drifted() {
        // The fallback direction matters: a requirement this parser cannot
        // read must never read as "does not satisfy". Marking a working
        // environment drifted on a parse failure would be a false alarm.
        //
        // The version must *violate* the requirement it cannot parse.
        // `("3.9.18", "<3.13")` -- what this asserted first -- discriminates
        // nothing: 3.9.18 satisfies `<3.13`, so it reads Present whether the
        // fallback fired or `<` were implemented tomorrow. `<3.5` can only
        // read Present because the parse failed.
        assert_eq!(state(&entry(Some("3.9.18"), Some("<3.5"))), State::Present);
        assert_eq!(
            state(&entry(Some("3.9.18"), Some("not a specifier"))),
            State::Present
        );
    }

    fn config_with(labels: &[&str]) -> bombadil_core::model::Config {
        bombadil_core::model::Config {
            projects: labels.iter().map(|label| project(label)).collect(),
            ..bombadil_core::model::Config::default()
        }
    }

    fn project(label: &str) -> bombadil_core::model::Project {
        bombadil_core::model::Project {
            id: uuid::Uuid::new_v4(),
            label: label.into(),
            pyproject_path: std::path::PathBuf::from(format!("/p/{label}/pyproject.toml")),
            environments: vec![bombadil_core::model::Environment {
                location: bombadil_core::model::VenvLocation::Alongside,
                python: bombadil_core::model::PythonPin::Unpinned,
            }],
            active: bombadil_core::model::VenvLocation::Alongside,
            ..bombadil_core::model::Project::default()
        }
    }
}
