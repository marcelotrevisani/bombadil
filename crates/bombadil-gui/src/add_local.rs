//! Adding a local package -- a folder on disk -- to a project's dependencies:
//! `uv add [--editable] <folder>`. The folder is written into
//! `[tool.uv.sources]` relative to the project (`members::path_for_manifest`),
//! so the manifest still works on a teammate's machine.

use crate::app::Message;
use crate::deps::GroupKind;
use crate::theme;
use std::path::{Path, PathBuf};

/// The open "Add local package" dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    /// The project it adds to, by id.
    pub project: uuid::Uuid,
    pub folder: PathBuf,
    /// `[project].name`, when the folder's manifest declares one.
    pub name: Option<String>,
    /// Where it can go: main, then the project's groups and extras.
    pub groups: Vec<GroupKind>,
    pub group: GroupKind,
    pub editable: bool,
}

impl Draft {
    /// The package's name, or its folder's when the manifest names none.
    pub fn label(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            self.folder
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.folder.display().to_string())
        })
    }
}

/// Whether `folder` is a Python package, and its name. `read` returns a
/// file's text, or `None` when it is not there -- the filesystem stays at the
/// caller's edge.
pub fn inspect(
    folder: &Path,
    read: &dyn Fn(&Path) -> Option<String>,
) -> Result<Option<String>, String> {
    let manifest = folder.join("pyproject.toml");
    if let Some(text) = read(&manifest) {
        return Ok(crate::add_project::draft_from(&manifest, &text).name);
    }
    if read(&folder.join("setup.py")).is_some() {
        return Ok(None);
    }
    let name = folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.display().to_string());
    Err(format!(
        "{name} is not a Python package: it has no pyproject.toml or setup.py"
    ))
}

/// The dialog: what is being added, where to, and whether editable.
pub fn view<'a>(draft: &Draft, to: &str, waiting: Option<&str>) -> iced::Element<'a, Message> {
    let group_label = |kind: &GroupKind| match kind {
        GroupKind::Main => "main".to_string(),
        GroupKind::Group(name) => format!("group: {name}"),
        GroupKind::Extra(name) => format!("extra: {name}"),
    };
    let mut groups = iced::widget::row![].spacing(theme::SPACE_1);
    for kind in &draft.groups {
        groups = groups.push(
            iced::widget::button(iced::widget::text(group_label(kind)).size(theme::BODY))
                .on_press(Message::AddLocalGroupSelected(kind.clone()))
                .padding([theme::SPACE_1, theme::SPACE_2])
                .style(theme::button_choice(*kind == draft.group)),
        );
    }
    iced::widget::container(
        iced::widget::column![
            iced::widget::text(format!("Add {} to {to}", draft.label())).size(theme::TITLE),
            theme::labeled_value("folder", draft.folder.display().to_string()),
            groups,
            iced::widget::checkbox(draft.editable)
                .label("Editable: changes in the folder apply without reinstalling")
                .on_toggle(Message::AddLocalEditableToggled),
            iced::widget::row![
                iced::widget::Space::new().width(iced::Length::Fill),
                iced::widget::button(iced::widget::text("Cancel").size(theme::BODY))
                    .on_press(Message::AddLocalCancelled)
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_quiet),
                theme::wait_tip(
                    iced::widget::button(iced::widget::text("Add").size(theme::BODY))
                        .on_press_maybe(waiting.is_none().then_some(Message::AddLocalConfirmed))
                        .padding([theme::SPACE_1, theme::SPACE_3])
                        .style(theme::button_primary),
                    waiting,
                ),
            ]
            .spacing(theme::SPACE_2),
        ]
        .spacing(theme::SPACE_3),
    )
    .width(theme::MODAL_WIDTH)
    .padding(theme::SPACE_4)
    .style(theme::panel)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn files(names: &'static [(&'static str, &'static str)]) -> impl Fn(&Path) -> Option<String> {
        move |path: &Path| {
            names
                .iter()
                .find(|(n, _)| path.file_name().is_some_and(|f| f == *n))
                .map(|(_, text)| text.to_string())
        }
    }

    #[test]
    fn a_folder_with_a_manifest_names_its_package() {
        let read = files(&[("pyproject.toml", "[project]\nname = \"mylib\"\n")]);
        assert_eq!(
            inspect(Path::new("/code/mylib"), &read),
            Ok(Some("mylib".to_string()))
        );
    }

    #[test]
    fn a_setup_py_package_is_accepted_without_a_name() {
        let read = files(&[("setup.py", "from setuptools import setup\n")]);
        assert_eq!(inspect(Path::new("/code/old"), &read), Ok(None));
    }

    #[test]
    fn a_folder_that_is_not_a_package_is_refused() {
        let read = files(&[]);
        let err = inspect(Path::new("/code/notes"), &read).unwrap_err();
        assert!(
            err.contains("notes") && err.contains("not a Python package"),
            "{err}"
        );
    }

    #[test]
    fn the_label_falls_back_to_the_folder_name() {
        let draft = Draft {
            project: uuid::Uuid::nil(),
            folder: PathBuf::from("/code/old"),
            name: None,
            groups: vec![crate::deps::GroupKind::Main],
            group: crate::deps::GroupKind::Main,
            editable: true,
        };
        assert_eq!(draft.label(), "old");
    }
}
