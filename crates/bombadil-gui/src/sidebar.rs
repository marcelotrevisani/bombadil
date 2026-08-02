//! The project sidebar: one entry per registered project, marked filled when
//! its virtual environment exists and hollow when it does not.
//!
//! Spec §8: registered-without-a-venv is a normal state, not an error -- a
//! user adds a project first and creates the environment afterwards -- so a
//! project without a venv is still listed, just marked hollow.

use bombadil_core::model::Config;

use crate::app::Message;

/// One row in the sidebar.
pub struct Entry {
    pub label: String,
    pub venv_exists: bool,
    pub python_version: Option<String>,
}

/// Builds one `Entry` per project, in config order.
///
/// Every entry starts hollow: `app::boot`'s background probes fill in real
/// venv status asynchronously through `Message::VenvProbed`. Probing here
/// inline would block the render thread on a `pyvenv.cfg` read, which is
/// exactly what those background probes exist to avoid.
pub fn entries(config: &Config) -> Vec<Entry> {
    config
        .projects
        .iter()
        .map(|project| Entry {
            label: project.label.clone(),
            venv_exists: false,
            python_version: None,
        })
        .collect()
}

/// Renders the entry list. Deliberately dumb: all the logic worth testing
/// lives in `entries`. `selected` is accepted for parity with the other
/// `view` functions in this crate; highlighting the selected row is left to
/// styling, not covered by a test.
///
/// Each row is wrapped in a `mouse_area` so a right-click opens the project's
/// context menu (spec §8) without disturbing the left-click select behaviour
/// the button already provides.
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
        let row = iced::widget::button(iced::widget::text(label))
            .on_press(Message::ProjectSelected(i))
            .width(iced::Length::Fill);
        list =
            list.push(iced::widget::mouse_area(row).on_right_press(Message::ContextMenuOpened(i)));
    }
    list.into()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn entries_follow_config_order_so_the_list_does_not_reshuffle() {
        let entries = entries(&config_with(&["web", "api"]));
        let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, vec!["web", "api"]);
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
