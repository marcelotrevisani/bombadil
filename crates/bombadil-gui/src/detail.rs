//! The detail pane header: identifies the selected project, its path, venv
//! status and Python version, and offers the Sync action.
//!
//! Spec §8: the header carries the project name, its path, venv status,
//! Python version, and a Sync action. A project registered without a venv is
//! a normal state (see `sidebar`) -- creating the environment is how the user
//! recovers from it, so the Sync action is never gated on the venv already
//! existing. `view` does not branch on `venv_status` to decide whether to
//! show the Sync button; it is always rendered, which is what keeps this
//! criterion true by construction rather than by a check someone could
//! accidentally remove.

use crate::app::Message;
use crate::theme;
use bombadil_core::model::Project;
use std::path::{Path, PathBuf};

/// Whether the project's virtual environment exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenvStatus {
    Present,
    Absent,
}

/// Everything the detail pane header shows for the selected project.
pub struct Header {
    pub label: String,
    /// The project's directory -- what a user recognises -- never the
    /// `pyproject.toml` file inside it.
    pub path: PathBuf,
    pub venv_status: VenvStatus,
    pub python_version: Option<String>,
    /// Where the *active* environment resolves to.
    ///
    /// Shown beneath the project's own path, because a project can have
    /// several environments now and the header describes the one Sync and
    /// Open terminal act on -- which the project path alone cannot say. `None`
    /// for a project with no environments at all.
    pub venv_path: Option<PathBuf>,
}

/// Builds the header for `project`.
///
/// `venv_probe` is a parameter rather than a filesystem call, so this is
/// testable without touching disk. The real caller in `app::view` answers
/// from `App`'s already-probed `entries` cache rather than touching disk
/// again on every frame.
///
/// `venv_path` likewise comes from the caller: resolving it needs `Settings`,
/// which this function deliberately does not take -- one resolver, called
/// where the config already is.
pub fn header(
    project: &Project,
    venv_path: Option<PathBuf>,
    venv_probe: &dyn Fn(&Project) -> Option<String>,
) -> Header {
    let python_version = venv_probe(project);
    let venv_status = if python_version.is_some() {
        VenvStatus::Present
    } else {
        VenvStatus::Absent
    };

    Header {
        label: project.label.clone(),
        path: project_dir(project),
        venv_status,
        python_version,
        venv_path,
    }
}

/// The project's directory: `pyproject_path`'s parent, or the path itself
/// when it has none (a bare filename with no directory component) -- the
/// same fallback `venv_path::resolve` uses for the same edge case.
fn project_dir(project: &Project) -> PathBuf {
    project
        .pyproject_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| project.pyproject_path.clone())
}

/// Renders the header. Deliberately dumb: all the logic worth testing lives
/// in `header` -- this crate's convention is that `view` itself carries no
/// test, since it returns an opaque `Element` no test can drive. The Sync
/// button is unconditional -- it does not read `venv_status` to decide
/// whether to render at all.
///
/// Two rows, both right-anchored: the project label (prose, `DISPLAY` --
/// the largest thing on screen) sits across from the Python version, and
/// the path beneath it sits across from Sync. The project label and Sync's
/// own label are the only words here, so both stay in the prose face at
/// their default colour; the path and the version are read character by
/// character, so both are Plex at `DATA` in `slate` -- the same rule
/// `sidebar` already applies to a project's path and version, held here too.
///
/// Sync is `theme::button_primary` -- the one filled control in this view,
/// and the reason the tokens live in `theme` rather than here: an unstyled
/// iced button also paints from the palette's primary, but it puts parchment
/// on it, which measures 3.1:1. `button_primary` uses ink text for 5.4:1.
///
/// `index` is the project's position in `App::config.projects`, carried into
/// `Message::SyncRequested` so one message shape serves this button and the
/// context menu's Sync entry -- see that message's own doc for why resolving
/// against the selection instead is a bug.
pub fn view<'a>(header: &Header, index: usize) -> iced::Element<'a, Message> {
    // How a missing version renders is a decision, but it stays inline here
    // rather than becoming a tested function, matching `sidebar::view`'s own
    // precedent for the identical choice (its version line, Some vs the
    // words "no env"): both are a single match producing an `Element`, not
    // a mapping to a domain state, so there is nothing here for a mutation
    // test to prove that `header`'s own tests do not already cover.
    let version: iced::Element<'_, Message> = match (header.venv_status, &header.python_version) {
        (VenvStatus::Present, Some(version)) => iced::widget::text(version.clone())
            .font(theme::FONT_DATA)
            .size(theme::DATA)
            .into(),
        (VenvStatus::Present, None) => iced::widget::text("environment ready")
            .size(theme::DATA)
            .color(theme::SLATE)
            .into(),
        (VenvStatus::Absent, _) => iced::widget::text("no environment")
            .size(theme::DATA)
            .color(theme::SLATE)
            .into(),
    };

    let title_row = iced::widget::row![
        iced::widget::text(header.label.clone())
            .font(theme::FONT_PROSE_SEMIBOLD)
            .size(theme::DISPLAY),
        iced::widget::Space::new().width(iced::Length::Fill),
        version,
    ];

    let sync = iced::widget::button(iced::widget::text("Sync").size(theme::BODY))
        .on_press(Message::SyncRequested(index))
        .padding([theme::SPACE_1, theme::SPACE_3])
        .style(theme::button_primary);

    // A shell prompt, set in the data face -- the same `>_` a terminal shows.
    // Drawn from the two typefaces already shipped rather than adding an icon
    // asset for one control, and deliberately not one of `theme::state_glyph`'s
    // shapes: those are the state vocabulary, and this is an action.
    //
    // A tooltip, not a bare glyph: an icon-only control with no icon set
    // behind it is something the user has to guess at, and this one opens a
    // process.
    let terminal = iced::widget::tooltip(
        iced::widget::button(
            iced::widget::text(">_")
                .font(theme::FONT_DATA)
                .size(theme::BODY),
        )
        .on_press(Message::OpenTerminalHereRequested(index))
        .padding([theme::SPACE_1, theme::SPACE_2])
        .style(theme::button_quiet),
        iced::widget::container(
            iced::widget::text("Open a terminal here, with the environment activated")
                .size(theme::LABEL),
        )
        .padding(theme::SPACE_1)
        .style(theme::panel),
        iced::widget::tooltip::Position::Left,
    );

    // "PC", set in the same data face as the terminal's `>_` beside it, for
    // the same reason: two typefaces already ship, an icon set does not, and
    // a JetBrains mark is not ours to draw. The tooltip carries the meaning,
    // and says what the launch actually does -- the interpreter is the part a
    // user would otherwise assume was handled (see `bombadil_core::ide`'s
    // module doc for why it cannot be).
    let pycharm = iced::widget::tooltip(
        iced::widget::button(
            iced::widget::text("PC")
                .font(theme::FONT_DATA)
                .size(theme::BODY),
        )
        .on_press(Message::OpenPycharmRequested(index))
        .padding([theme::SPACE_1, theme::SPACE_2])
        .style(theme::button_quiet),
        iced::widget::container(
            iced::widget::text(
                "Open in PyCharm, in this folder, with the environment's variables and PATH",
            )
            .size(theme::LABEL),
        )
        .padding(theme::SPACE_1)
        .style(theme::panel),
        iced::widget::tooltip::Position::Left,
    );

    let path_row = iced::widget::row![
        iced::widget::text(header.path.display().to_string())
            .font(theme::FONT_DATA)
            .size(theme::DATA)
            .color(theme::SLATE),
        iced::widget::Space::new().width(iced::Length::Fill),
        pycharm,
        terminal,
        sync,
    ]
    .spacing(theme::SPACE_2)
    .align_y(iced::Alignment::Center);

    // The active environment's path, beneath the project's own. A project can
    // have several environments, so "which one do Sync and Open terminal act
    // on" is a question the project path cannot answer -- and it is the one
    // the two buttons on the row above make urgent.
    let venv_row: iced::Element<'_, Message> = match &header.venv_path {
        Some(path) => theme::labeled_value("environment", path.display().to_string()),
        None => iced::widget::text("no environment configured")
            .size(theme::DATA)
            .color(theme::SLATE)
            .into(),
    };

    iced::widget::column![title_row, path_row, venv_row]
        .spacing(theme::SPACE_2)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_carries_the_active_environments_path() {
        // A project can have several environments, so "which one do Sync and
        // Open terminal act on" is a question the project's own path cannot
        // answer -- and the two buttons sitting on that row make it urgent.
        let path = PathBuf::from("/home/me/envs/api-312");
        let header = header(&project("api"), Some(path.clone()), &|_| {
            Some("3.12.1".into())
        });

        assert_eq!(header.venv_path, Some(path));
        assert_ne!(
            header.venv_path.as_ref(),
            Some(&header.path),
            "the environment path is not the project path -- showing one twice \
             would say nothing"
        );
    }

    #[test]
    fn a_project_with_no_environments_carries_no_path() {
        // Reachable between removing the last environment and adding another.
        // `None` rather than the project directory, which would claim an
        // environment lives somewhere it does not.
        let header = header(&project("api"), None, &|_| None);
        assert_eq!(header.venv_path, None);
    }

    #[test]
    fn a_project_with_a_venv_shows_its_python_version() {
        let header = header(&project("api"), None, &|_| Some("3.12.1".into()));
        assert_eq!(header.python_version.as_deref(), Some("3.12.1"));
        assert_eq!(header.venv_status, VenvStatus::Present);
    }

    #[test]
    fn a_project_without_a_venv_reports_no_environment() {
        // Registered-without-a-venv is a normal state (see `sidebar`);
        // whether Sync is still offered for it is `update`'s concern, not
        // `header`'s -- covered in `app.rs` where the Sync message is
        // dispatched independently of venv state.
        let header = header(&project("api"), None, &|_| None);
        assert_eq!(header.python_version, None);
        assert_eq!(header.venv_status, VenvStatus::Absent);
    }

    #[test]
    fn the_displayed_path_is_the_project_directory_not_the_manifest_file() {
        // A user recognises the directory they added, not the manifest file
        // inside it.
        let header = header(&project("api"), None, &|_| None);
        assert_eq!(header.path, PathBuf::from("/p/api"));
        assert_ne!(header.path, PathBuf::from("/p/api/pyproject.toml"));
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
