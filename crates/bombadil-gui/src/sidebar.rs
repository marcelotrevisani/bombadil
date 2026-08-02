//! The project sidebar: one entry per registered project, marked filled when
//! its virtual environment exists and hollow when it does not.
//!
//! Spec §8: registered-without-a-venv is a normal state, not an error -- a
//! user adds a project first and creates the environment afterwards -- so a
//! project without a venv is still listed, just marked hollow.

use bombadil_core::model::Config;
use bombadil_core::venv_path;
use std::path::Path;

use crate::app::Message;

/// One row in the sidebar.
pub struct Entry {
    pub label: String,
    pub venv_exists: bool,
    pub python_version: Option<String>,
}

/// Builds one `Entry` per project, in config order.
///
/// `venv_probe` is a parameter rather than a filesystem call so this runs
/// without touching disk in tests; the caller supplies a real probe.
///
/// A project whose venv path cannot be resolved (`venv_path::resolve` fails
/// on a `pyproject.toml` with no parent) is listed hollow rather than
/// dropped -- dropping it would make it unrecoverable through the UI, since
/// the user could never select it again to fix or remove it.
pub fn entries(config: &Config, venv_probe: &dyn Fn(&Path) -> Option<String>) -> Vec<Entry> {
    config
        .projects
        .iter()
        .map(|project| {
            let python_version = venv_path::resolve(project, &config.settings)
                .ok()
                .and_then(|path| venv_probe(&path));
            Entry {
                label: project.label.clone(),
                venv_exists: python_version.is_some(),
                python_version,
            }
        })
        .collect()
}

/// Renders the entry list. Deliberately dumb: all the logic worth testing
/// lives in `entries`. `selected` is accepted for parity with the other
/// `view` functions in this crate; highlighting the selected row is left to
/// styling, not covered by a test.
pub fn view<'a>(entries: &[Entry], _selected: Option<usize>) -> iced::Element<'a, Message> {
    let mut list = iced::widget::column![].spacing(4);
    for (i, entry) in entries.iter().enumerate() {
        let marker = if entry.venv_exists {
            "\u{25cf}"
        } else {
            "\u{25cb}"
        };
        let label = match &entry.python_version {
            Some(version) => format!("{marker} {} ({version})", entry.label),
            None => format!("{marker} {}", entry.label),
        };
        list = list.push(
            iced::widget::button(iced::widget::text(label))
                .on_press(Message::ProjectSelected(i))
                .width(iced::Length::Fill),
        );
    }
    list.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_with_a_venv_is_marked_filled() {
        let entries = entries(&config_with(&["api"]), &|_| Some("3.12.1".into()));
        assert_eq!(entries.len(), 1);
        assert!(entries[0].venv_exists);
        assert_eq!(entries[0].python_version.as_deref(), Some("3.12.1"));
    }

    #[test]
    fn a_project_without_a_venv_is_marked_hollow_and_still_listed() {
        // Registered-without-a-venv is a normal state, not an error: the user
        // adds the project first and creates the environment afterwards.
        let entries = entries(&config_with(&["api"]), &|_| None);
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].venv_exists);
        assert_eq!(entries[0].python_version, None);
    }

    #[test]
    fn entries_follow_config_order_so_the_list_does_not_reshuffle() {
        let entries = entries(&config_with(&["web", "api"]), &|_| None);
        let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, vec!["web", "api"]);
    }

    #[test]
    fn a_project_whose_venv_path_cannot_be_resolved_is_listed_as_hollow() {
        // venv_path::resolve fails on a pyproject path with no parent. Dropping
        // the project from the list would make it unrecoverable through the UI.
        let mut config = config_with(&["api"]);
        config.projects[0].pyproject_path = std::path::PathBuf::from("pyproject.toml");
        let entries = entries(&config, &|_| Some("3.12.1".into()));
        assert_eq!(entries.len(), 1, "the project must still be listed");
        assert!(!entries[0].venv_exists);
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
            index_names: vec![],
            venv: bombadil_core::model::VenvLocation::Alongside,
            env: vec![],
            scripts: vec![],
            members: Default::default(),
        }
    }
}
