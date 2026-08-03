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

use bombadil_core::model::{Config, PythonPin};

use crate::app::Message;
use crate::interpreter;
use crate::theme::{self, State};

/// One row in the sidebar.
pub struct Entry {
    pub label: String,
    pub venv_exists: bool,
    pub python_version: Option<String>,
    /// The project's `requires-python`, re-read alongside the venv probe
    /// (`app::probe_every_venv`) rather than stored on `Project` -- see that
    /// module's doc for why. `None` covers both "not probed yet" and "the
    /// manifest declares none"; `state` treats both the same way
    /// `interpreter::version_satisfies` does, as nothing to check against
    /// rather than a mismatch.
    pub requires_python: Option<String>,
    /// The interpreter this project is pinned to, from
    /// `bombadil_core::resolve::effective`. `Unpinned` until someone sets
    /// one, which is every project today.
    pub pin: PythonPin,
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
            label: project.label.clone(),
            venv_exists: false,
            python_version: None,
            requires_python: None,
            pin: project
                .active_environment()
                .map(|environment| environment.python.clone())
                .unwrap_or_default(),
        })
        .collect()
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
    match &entry.python_version {
        None => State::Absent,
        Some(version) => {
            // Two independent reasons to drift, and the venv has to clear
            // both: the manifest's own `requires-python`, and the pin the
            // user set. Either alone would let the other's mismatch pass.
            let agrees = interpreter::version_satisfies(version, entry.requires_python.as_deref())
                && interpreter::satisfies_pin(version, &entry.pin);
            if agrees {
                State::Present
            } else {
                State::Drifted
            }
        }
    }
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
pub fn view<'a>(entries: &[Entry], selected: Option<usize>) -> iced::Element<'a, Message> {
    let mut list = iced::widget::column![].spacing(theme::SPACE_2);
    for (i, entry) in entries.iter().enumerate() {
        let is_selected = selected == Some(i);
        let glyph = theme::state_glyph(state(entry));

        let label_font = if is_selected {
            theme::FONT_PROSE_SEMIBOLD
        } else {
            theme::FONT_PROSE
        };

        let version_line: iced::Element<'_, Message> = match &entry.python_version {
            Some(version) => iced::widget::text(version.clone())
                .font(theme::FONT_DATA)
                .size(theme::DATA)
                .color(theme::SLATE)
                .into(),
            None => iced::widget::text("no env")
                .font(theme::FONT_PROSE)
                .size(theme::DATA)
                .color(theme::SLATE)
                .into(),
        };

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
        .style(move |_theme| iced::widget::container::Style {
            background: is_selected.then_some(iced::Background::Color(theme::JACKET)),
            ..iced::widget::container::Style::default()
        });

        let content = iced::widget::row![
            marker,
            iced::widget::column![
                iced::widget::row![
                    iced::widget::text(glyph.glyph.to_string())
                        .size(theme::BODY)
                        .color(glyph.colour)
                        .width(theme::GLYPH_COLUMN_WIDTH),
                    iced::widget::text(entry.label.clone())
                        .font(label_font)
                        .size(theme::BODY),
                ]
                .spacing(theme::SPACE_1),
                version_line,
            ]
            .spacing(theme::SPACE_1),
        ]
        .spacing(theme::SPACE_2);

        let row = iced::widget::button(content)
            .on_press(Message::ProjectSelected(i))
            .padding(theme::SPACE_1)
            .width(iced::Length::Fill)
            .style(move |_theme, status| iced::widget::button::Style {
                background: match (is_selected, status) {
                    (true, _) => Some(iced::Background::Color(theme::INK)),
                    (false, iced::widget::button::Status::Hovered) => {
                        Some(iced::Background::Color(theme::INK))
                    }
                    _ => None,
                },
                text_color: theme::PARCHMENT,
                border: iced::Border {
                    radius: theme::RADIUS.into(),
                    ..iced::Border::default()
                },
                ..iced::widget::button::Style::default()
            });
        list =
            list.push(iced::widget::mouse_area(row).on_right_press(Message::ContextMenuOpened(i)));
    }
    list.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(python_version: Option<&str>, requires_python: Option<&str>) -> Entry {
        Entry {
            label: "api".into(),
            venv_exists: python_version.is_some(),
            python_version: python_version.map(String::from),
            requires_python: requires_python.map(String::from),
            pin: PythonPin::Unpinned,
        }
    }

    /// An entry pinned to `pin`, with no `requires-python` in play -- so a
    /// test about the pin is not quietly also a test about the manifest.
    fn pinned(python_version: &str, pin: &str) -> Entry {
        Entry {
            pin: PythonPin::Version(pin.into()),
            ..entry(Some(python_version), None)
        }
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
            pin: PythonPin::Version("3.9".into()),
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
        assert!(!entries[0].venv_exists);
        assert_eq!(entries[0].python_version, None);
        assert_eq!(entries[0].requires_python, None);
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
