//! The add-project dialog: pick a `pyproject.toml`, show what it declares,
//! and choose where its virtual environment lives.
//!
//! Spec §8: a file picker for the manifest, showing the parsed project name
//! and `requires-python` once chosen, and a three-way venv control --
//! settings default, alongside the manifest, or a chosen folder -- with the
//! resolved path always displayed.

use crate::app::Message;
use crate::interpreter;
use bombadil_core::error::VenvPathError;
use bombadil_core::model::{Project, Settings, VenvLocation};
use bombadil_core::uv::results::Interpreter;
use bombadil_core::{pyproject, venv_path};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Where the new project's virtual environment will live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VenvChoice {
    /// Follow `Settings::default_venv_location`.
    SettingsDefault,
    /// `<project dir>/.venv`.
    Alongside,
    /// A directory the user picked explicitly.
    Chosen(PathBuf),
}

/// The state of the add-project dialog for one chosen manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub pyproject_path: PathBuf,
    pub name: Option<String>,
    pub requires_python: Option<String>,
    pub venv_choice: VenvChoice,
    /// The interpreters uv can see, fetched asynchronously by `app.rs` once
    /// the manifest is loaded (`uv python list`). Starts empty -- before the
    /// fetch lands, [`interpreter_status`] shows that as "checking" rather
    /// than "none found", which would be indistinguishable from a genuinely
    /// empty machine and would show the install offer prematurely.
    pub interpreters: Vec<Interpreter>,
}

/// Builds a `Draft` from the chosen manifest's path and text.
///
/// Takes the text, not just the path, so this stays pure and testable
/// without touching the filesystem -- reading the file is `app.rs`'s job,
/// inside `job::run`.
///
/// A parse failure yields `None` fields rather than propagating the error:
/// the user already picked this file, and telling them nothing *and*
/// forgetting which file they picked is worse than showing an empty name. A
/// manifest with no `[project]` table (a uv workspace root) parses cleanly
/// to the same `None` name, which is why it is still addable.
pub fn draft_from(path: &Path, text: &str) -> Draft {
    let parsed = pyproject::parse(text).ok();
    Draft {
        pyproject_path: path.to_path_buf(),
        name: parsed.as_ref().and_then(|p| p.name.clone()),
        requires_python: parsed.as_ref().and_then(|p| p.requires_python.clone()),
        venv_choice: VenvChoice::SettingsDefault,
        interpreters: Vec::new(),
    }
}

/// The interpreter line for the dialog. Spec §8: pre-selected to satisfy
/// `requires-python`, with an offer to install one when none qualifies.
///
/// Three states, not two: before `draft.interpreters` is populated this
/// must read as "still looking," not "nothing found" -- the two look
/// identical to `interpreter::preselect` (`None` either way), but only one
/// of them should ever prompt the user to install something.
fn interpreter_status(draft: &Draft) -> String {
    match interpreter::preselect(&draft.interpreters, draft.requires_python.as_deref()) {
        Some(interpreter) => format!("python: {}", interpreter.version),
        None if draft.interpreters.is_empty() => {
            "python: (checking available interpreters...)".to_string()
        }
        None => "python: no compatible interpreter found".to_string(),
    }
}

/// Whether the dialog should offer to install a suitable interpreter: a
/// list has actually been fetched, and nothing in it satisfies
/// `requires_python`. Never true while `interpreters` is still empty from
/// not having been fetched yet -- see `interpreter_status`.
fn needs_install_offer(draft: &Draft) -> bool {
    !draft.interpreters.is_empty()
        && interpreter::preselect(&draft.interpreters, draft.requires_python.as_deref()).is_none()
}

/// Builds the `Project` a draft describes. Used both for resolving its venv
/// path for display (id is irrelevant to that call) and for confirming the
/// dialog, where the id must be real and unique -- one function rather than
/// two, since generating a `Uuid::new_v4()` for a display-only resolve costs
/// nothing.
pub fn to_project(draft: &Draft) -> Project {
    Project {
        id: Uuid::new_v4(),
        label: draft.name.clone().unwrap_or_default(),
        pyproject_path: draft.pyproject_path.clone(),
        index_names: Vec::new(),
        venv: match &draft.venv_choice {
            VenvChoice::SettingsDefault => VenvLocation::Default,
            VenvChoice::Alongside => VenvLocation::Alongside,
            VenvChoice::Chosen(path) => VenvLocation::Custom { path: path.clone() },
        },
        env: Vec::new(),
        scripts: Vec::new(),
        members: Default::default(),
    }
}

/// The venv path the dialog would resolve to right now, for display.
///
/// Reuses `venv_path::resolve` rather than re-deriving its central-store
/// hashing and alongside logic here -- the same resolution a real `Project`
/// gets once this draft is confirmed. A fresh call per render (see
/// `app::view`) is what makes this track `venv_choice` as the user changes
/// it, the same trick `banner::problems` uses to stay live.
pub fn resolved_venv_path(draft: &Draft, settings: &Settings) -> Result<PathBuf, VenvPathError> {
    venv_path::resolve(&to_project(draft), settings)
}

/// Formats the resolved path for display, or the reason it could not be
/// resolved -- only possible when the manifest path has no parent directory.
pub fn resolved_venv_path_display(draft: &Draft, settings: &Settings) -> String {
    match resolved_venv_path(draft, settings) {
        Ok(path) => path.display().to_string(),
        Err(err) => err.to_string(),
    }
}

/// What confirming the dialog will do to the chosen venv location.
/// Spec §8: when the folder already holds a virtual environment the dialog
/// states it will be adopted, and shows its Python version; otherwise a
/// fresh one is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VenvOutcome {
    Adopt { python_version: String },
    Create,
}

/// Decides `VenvOutcome` for `venv_path`. `probe` is a parameter rather than
/// `venv_path::probe` called directly here, so this stays pure and testable
/// without a real filesystem -- `venv_path` may not exist at all on the
/// machine running a test. The real caller (`app::run_confirm_add_project`)
/// passes `venv_path::probe` itself.
pub fn venv_outcome(venv_path: &Path, probe: impl Fn(&Path) -> Option<String>) -> VenvOutcome {
    match probe(venv_path) {
        Some(python_version) => VenvOutcome::Adopt { python_version },
        None => VenvOutcome::Create,
    }
}

/// Renders the dialog for an already-chosen manifest. Deliberately dumb: all
/// the logic worth testing lives in `draft_from` and `resolved_venv_path`.
/// `resolved` is computed by the caller (`app::view`) so it is recomputed
/// fresh on every render rather than something `view` would have to know how
/// to derive.
pub fn view<'a>(draft: &Draft, resolved: &str) -> iced::Element<'a, Message> {
    let name = draft
        .name
        .clone()
        .unwrap_or_else(|| "(no [project] table)".to_string());
    let requires_python = draft
        .requires_python
        .clone()
        .unwrap_or_else(|| "(none)".to_string());

    let venv_row = iced::widget::row![
        iced::widget::button(iced::widget::text("Settings default")).on_press(
            Message::AddProjectVenvChoiceSelected(VenvChoice::SettingsDefault)
        ),
        iced::widget::button(iced::widget::text("Alongside manifest"))
            .on_press(Message::AddProjectVenvChoiceSelected(VenvChoice::Alongside)),
        iced::widget::button(iced::widget::text("Choose folder..."))
            .on_press(Message::AddProjectPickVenvFolderRequested),
    ]
    .spacing(8);

    let mut column = iced::widget::column![
        iced::widget::text(draft.pyproject_path.display().to_string()),
        iced::widget::text(format!("name: {name}")),
        iced::widget::text(format!("requires-python: {requires_python}")),
        iced::widget::text(interpreter_status(draft)),
    ]
    .spacing(4);

    if needs_install_offer(draft) {
        column = column.push(
            iced::widget::button(iced::widget::text("Install compatible interpreter"))
                .on_press(Message::AddProjectInstallInterpreterRequested),
        );
    }

    let actions = iced::widget::row![
        iced::widget::button(iced::widget::text("Add project"))
            .on_press(Message::AddProjectConfirmed),
        iced::widget::button(iced::widget::text("Cancel")).on_press(Message::AddProjectCancelled),
    ]
    .spacing(8);

    column
        .push(venv_row)
        .push(iced::widget::text(format!("venv: {resolved}")))
        .push(actions)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bombadil_core::model::DefaultVenvLocation;

    const MANIFEST: &str = r#"
[project]
name = "my-api"
version = "0.1.0"
requires-python = ">=3.11"
"#;

    #[test]
    fn a_chosen_manifest_shows_its_name_and_python_requirement() {
        let draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        assert_eq!(draft.name.as_deref(), Some("my-api"));
        assert_eq!(draft.requires_python.as_deref(), Some(">=3.11"));
    }

    #[test]
    fn a_manifest_without_a_name_is_still_addable() {
        // A workspace root often has no [project] table at all. Refusing it
        // would make the monorepo case unaddable through the UI.
        let draft = draft_from(
            Path::new("/p/root/pyproject.toml"),
            "[tool.uv.workspace]\nmembers = []\n",
        );
        assert_eq!(draft.name, None);
        assert_eq!(draft.pyproject_path, Path::new("/p/root/pyproject.toml"));
    }

    #[test]
    fn unparseable_toml_does_not_lose_the_chosen_path() {
        // The user picked a file; telling them nothing and forgetting which
        // one they picked is the worst of both.
        let draft = draft_from(Path::new("/p/bad/pyproject.toml"), "this is not toml {{{");
        assert_eq!(draft.pyproject_path, Path::new("/p/bad/pyproject.toml"));
        assert_eq!(draft.name, None);
    }

    #[test]
    fn the_default_venv_choice_is_the_settings_default() {
        let draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        assert_eq!(draft.venv_choice, VenvChoice::SettingsDefault);
    }

    fn settings(default_venv_location: DefaultVenvLocation) -> Settings {
        Settings {
            default_venv_location,
            ..Settings::default()
        }
    }

    #[test]
    fn settings_default_follows_the_alongside_setting() {
        let draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        let path = resolved_venv_path(&draft, &settings(DefaultVenvLocation::Alongside)).unwrap();
        assert_eq!(path, PathBuf::from("/p/my-api/.venv"));
    }

    #[test]
    fn settings_default_follows_the_central_setting() {
        let draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        let path = resolved_venv_path(
            &draft,
            &settings(DefaultVenvLocation::Central {
                path: PathBuf::from("/home/t/.venvs"),
            }),
        )
        .unwrap();
        assert!(
            path.starts_with("/home/t/.venvs"),
            "got {path:?}; the central setting must be honoured"
        );
    }

    #[test]
    fn alongside_ignores_the_settings_default() {
        // The point of the three-way control: choosing "alongside" explicitly
        // must win over a central `default_venv_location`, not just mirror it.
        let mut draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        draft.venv_choice = VenvChoice::Alongside;
        let path = resolved_venv_path(
            &draft,
            &settings(DefaultVenvLocation::Central {
                path: PathBuf::from("/home/t/.venvs"),
            }),
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("/p/my-api/.venv"));
    }

    #[test]
    fn a_chosen_folder_is_used_verbatim() {
        let mut draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        draft.venv_choice = VenvChoice::Chosen(PathBuf::from("/mnt/fast/envs/api"));
        let path = resolved_venv_path(&draft, &settings(DefaultVenvLocation::Alongside)).unwrap();
        assert_eq!(path, PathBuf::from("/mnt/fast/envs/api"));
    }

    #[test]
    fn the_resolved_path_updates_as_the_choice_changes() {
        let mut draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        let settings = settings(DefaultVenvLocation::Alongside);

        let before = resolved_venv_path(&draft, &settings).unwrap();
        draft.venv_choice = VenvChoice::Chosen(PathBuf::from("/mnt/fast/envs/api"));
        let after = resolved_venv_path(&draft, &settings).unwrap();

        assert_ne!(
            before, after,
            "changing venv_choice must change the resolved path"
        );
        assert_eq!(after, PathBuf::from("/mnt/fast/envs/api"));
    }

    fn interp(version: &str) -> Interpreter {
        Interpreter {
            key: format!("cpython-{version}"),
            version: version.to_string(),
            path: Some(PathBuf::from(format!("/usr/bin/python{version}"))),
            implementation: "cpython".to_string(),
        }
    }

    #[test]
    fn the_default_interpreter_list_is_empty_until_fetched() {
        let draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        assert!(draft.interpreters.is_empty());
    }

    #[test]
    fn before_the_fetch_lands_the_status_reads_as_checking_not_missing() {
        // interpreters is empty both before the fetch and when the fetch
        // genuinely finds nothing -- interpreter_status must not conflate
        // "still looking" with "none found," which would show the install
        // offer prematurely.
        let draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        assert!(interpreter_status(&draft).contains("checking"));
        assert!(!needs_install_offer(&draft));
    }

    #[test]
    fn a_satisfying_interpreter_is_named_in_the_status_line() {
        let mut draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        draft.interpreters = vec![interp("3.10.13"), interp("3.11.9")];
        assert_eq!(interpreter_status(&draft), "python: 3.11.9");
        assert!(!needs_install_offer(&draft));
    }

    #[test]
    fn nothing_satisfying_offers_to_install_instead_of_naming_the_wrong_one() {
        let mut draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        draft.interpreters = vec![interp("3.9.18")];
        assert_eq!(
            interpreter_status(&draft),
            "python: no compatible interpreter found"
        );
        assert!(needs_install_offer(&draft));
    }

    #[test]
    fn a_folder_holding_a_venv_is_reported_as_adoptable_with_its_version() {
        // Criterion 1. The mutation this earns its place against: treating an
        // existing venv as empty and silently recreating it, which destroys
        // the user's environment. `probe` never touches a real path -- it is
        // a fake closure, so this proves the *wiring*, not the filesystem
        // read `venv_path::probe` itself already covers.
        let outcome = venv_outcome(Path::new("/whatever/.venv"), |_| Some("3.12.4".to_string()));
        assert_eq!(
            outcome,
            VenvOutcome::Adopt {
                python_version: "3.12.4".to_string()
            }
        );
    }

    #[test]
    fn an_empty_folder_is_reported_as_a_creation_not_an_adoption() {
        // Criterion 2: the opposite mistake -- claiming to adopt something
        // that is not there.
        let outcome = venv_outcome(Path::new("/whatever/.venv"), |_| None);
        assert_eq!(outcome, VenvOutcome::Create);
    }

    #[test]
    fn to_project_gives_every_draft_a_real_unique_id() {
        // Confirming the dialog needs a real id -- `venv_path::resolve`
        // itself never reads it, so a nil id would pass every existing
        // `resolved_venv_path` test and still be wrong for the config the
        // project is about to be appended to.
        let draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        let a = to_project(&draft);
        let b = to_project(&draft);
        assert_ne!(a.id, Uuid::nil());
        assert_ne!(
            a.id, b.id,
            "two confirmations of the same draft must not collide"
        );
    }
}
