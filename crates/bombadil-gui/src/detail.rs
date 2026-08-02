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
}

/// Builds the header for `project`.
///
/// `venv_probe` is a parameter rather than a filesystem call, so this is
/// testable without touching disk -- the same pattern `sidebar::entries`
/// uses. Unlike `sidebar::entries`, `header` has no `Settings` to resolve a
/// venv path with, so the probe is asked about the project directly; the
/// real caller in `app::view` answers from `App`'s already-probed
/// `entries` cache rather than touching disk again on every frame.
pub fn header(project: &Project, venv_probe: &dyn Fn(&Project) -> Option<String>) -> Header {
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
/// in `header`. The Sync button is unconditional -- it does not read
/// `venv_status` at all.
pub fn view<'a>(header: &Header) -> iced::Element<'a, Message> {
    let status = match header.venv_status {
        VenvStatus::Present => match &header.python_version {
            Some(version) => format!("Python {version}"),
            None => "environment ready".to_string(),
        },
        VenvStatus::Absent => "no environment".to_string(),
    };

    iced::widget::column![
        iced::widget::text(header.label.clone()).size(20),
        iced::widget::text(header.path.display().to_string()),
        iced::widget::text(status),
        iced::widget::button(iced::widget::text("Sync")).on_press(Message::SyncRequested),
    ]
    .spacing(4)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_with_a_venv_shows_its_python_version() {
        let header = header(&project("api"), &|_| Some("3.12.1".into()));
        assert_eq!(header.python_version.as_deref(), Some("3.12.1"));
        assert_eq!(header.venv_status, VenvStatus::Present);
    }

    #[test]
    fn a_project_without_a_venv_reports_no_environment() {
        // Registered-without-a-venv is a normal state (see `sidebar`);
        // whether Sync is still offered for it is `update`'s concern, not
        // `header`'s -- covered in `app.rs` where the Sync message is
        // dispatched independently of venv state.
        let header = header(&project("api"), &|_| None);
        assert_eq!(header.python_version, None);
        assert_eq!(header.venv_status, VenvStatus::Absent);
    }

    #[test]
    fn the_displayed_path_is_the_project_directory_not_the_manifest_file() {
        // A user recognises the directory they added, not the manifest file
        // inside it.
        let header = header(&project("api"), &|_| None);
        assert_eq!(header.path, PathBuf::from("/p/api"));
        assert_ne!(header.path, PathBuf::from("/p/api/pyproject.toml"));
    }

    fn project(label: &str) -> bombadil_core::model::Project {
        bombadil_core::model::Project {
            id: uuid::Uuid::new_v4(),
            label: label.into(),
            pyproject_path: std::path::PathBuf::from(format!("/p/{label}/pyproject.toml")),
            index_names: vec![],
            venv: bombadil_core::model::VenvLocation::Alongside,
            env: vec![],
            scripts: vec![],
            members: Default::default(),
        }
    }
}
