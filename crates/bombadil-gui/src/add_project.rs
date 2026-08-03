//! The add-project dialog: pick a `pyproject.toml`, show what it declares,
//! and choose where its virtual environment lives.
//!
//! Spec §8: a file picker for the manifest, showing the parsed project name
//! and `requires-python` once chosen, and a three-way venv control --
//! settings default, alongside the manifest, or a chosen folder -- with the
//! resolved path always displayed.

use crate::app::Message;
use crate::interpreter;
use crate::theme;
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

/// The state of the asynchronous `uv python list` fetch behind the dialog's
/// interpreter line.
///
/// Three states rather than a bare `Vec`: "not fetched yet", "fetched, and
/// here is what uv sees" and "the fetch could not produce a list at all" are
/// three different things to tell the user, and a producer that can only
/// return a `Vec` collapses the third into the first -- which renders as
/// "(checking available interpreters...)" forever, with no error, no retry
/// and the install button suppressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Interpreters {
    /// The fetch is still in flight.
    Pending,
    /// The fetch succeeded. An empty list is a real answer here: uv ran and
    /// saw no interpreters, which is what the install offer exists for.
    Loaded(Vec<Interpreter>),
    /// The fetch failed: no usable uv, a non-zero exit, output this build
    /// cannot parse, a timeout, or a panicking job. Carries uv's own text
    /// wherever there is any.
    Failed(String),
}

impl Interpreters {
    /// The interpreters there are to choose from -- empty while pending and
    /// after a failure, since neither state knows of any.
    pub fn available(&self) -> &[Interpreter] {
        match self {
            Self::Loaded(interpreters) => interpreters,
            Self::Pending | Self::Failed(_) => &[],
        }
    }
}

/// The state of the add-project dialog for one chosen manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub pyproject_path: PathBuf,
    pub name: Option<String>,
    pub requires_python: Option<String>,
    pub venv_choice: VenvChoice,
    /// What `uv python list` has said so far, fetched asynchronously by
    /// `app.rs` once the manifest is loaded. See [`Interpreters`] for why
    /// this is not a plain `Vec`.
    pub interpreters: Interpreters,
    /// Whether confirming would adopt an environment already sitting at the
    /// chosen location or create a fresh one (spec §8). `None` until the
    /// probe for the current `venv_choice` lands, and reset to `None` every
    /// time that choice changes -- a statement about the previous folder is
    /// worse than no statement at all.
    ///
    /// Filled in asynchronously by `app.rs` through `job::run`, the same
    /// seam `interpreters` uses: deciding this means reading `pyvenv.cfg`,
    /// which the render thread must never do.
    pub venv_outcome: Option<VenvOutcome>,
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
        interpreters: Interpreters::Pending,
        venv_outcome: None,
    }
}

/// The interpreter line for the dialog. Spec §8: pre-selected to satisfy
/// `requires-python`, with an offer to install one when none qualifies.
///
/// Four states, not two: "still looking", "the lookup failed", "found one"
/// and "found none" all read differently, and only one of them should ever
/// prompt the user to install something. `interpreter::preselect` answers
/// `None` for the first three alike, which is why the distinction has to
/// live in [`Interpreters`] rather than be inferred from an empty list.
fn interpreter_status(draft: &Draft) -> String {
    match &draft.interpreters {
        Interpreters::Pending => "python: (checking available interpreters...)".to_string(),
        Interpreters::Failed(why) => format!("python: could not list interpreters -- {why}"),
        Interpreters::Loaded(interpreters) => {
            match interpreter::preselect(interpreters, draft.requires_python.as_deref()) {
                Some(interpreter) => format!("python: {}", interpreter.version),
                None => "python: no compatible interpreter found".to_string(),
            }
        }
    }
}

/// Whether the dialog should offer to install a suitable interpreter: uv
/// actually answered, and nothing it reported satisfies `requires_python`.
/// Never true while the fetch is still in flight or has failed -- in neither
/// case is "no compatible interpreter" known to be true, and the failed case
/// would install through the very uv that just could not be run.
fn needs_install_offer(draft: &Draft) -> bool {
    match &draft.interpreters {
        Interpreters::Loaded(interpreters) => {
            interpreter::preselect(interpreters, draft.requires_python.as_deref()).is_none()
        }
        Interpreters::Pending | Interpreters::Failed(_) => false,
    }
}

/// The label for a manifest with no `[project]` table: the project
/// directory's own name, which is what the user picked and recognises.
///
/// The nameless workspace root is deliberately supported (see `draft_from`),
/// so `unwrap_or_default()` is not a "cannot happen" fallback -- it renders
/// as a bare sidebar marker with nothing after it and a blank detail header.
/// Empty only for a manifest path with no directory component at all, which
/// `venv_path::resolve` already refuses for its own reasons.
fn fallback_label(pyproject_path: &Path) -> String {
    pyproject_path
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Builds the `Project` a draft describes. Used both for resolving its venv
/// path for display (id is irrelevant to that call) and for confirming the
/// dialog, where the id must be real and unique -- one function rather than
/// two, since generating a `Uuid::new_v4()` for a display-only resolve costs
/// nothing.
pub fn to_project(draft: &Draft) -> Project {
    Project {
        id: Uuid::new_v4(),
        label: draft
            .name
            .clone()
            .unwrap_or_else(|| fallback_label(&draft.pyproject_path)),
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

/// The adopt-or-create sentence the dialog shows under the resolved path.
/// `None` while the probe for the current choice has not landed -- saying
/// nothing yet beats saying something about the previous folder.
///
/// The adoption line names the version *and* the consequence: an adopted
/// venv keeps whatever Python it already has, so the interpreter selected
/// above it is not used. Without that, pointing the dialog at a stale 3.9
/// venv under `requires-python = ">=3.12"` adopts it silently, and the
/// detail header afterwards just reads "Python 3.9" with nothing to explain
/// why the chosen interpreter was ignored.
fn venv_outcome_status(draft: &Draft) -> Option<String> {
    Some(match draft.venv_outcome.as_ref()? {
        VenvOutcome::Adopt { python_version } => format!(
            "this folder already holds a virtual environment (python {python_version}) -- \
             it will be adopted as it is, and the interpreter selected above will not be used"
        ),
        VenvOutcome::Create => "a new virtual environment will be created here".to_string(),
    })
}

/// Renders the dialog for an already-chosen manifest. Deliberately dumb: all
/// the logic worth testing lives in `draft_from` and `resolved_venv_path`.
/// `resolved` is computed by the caller (`app::view`) so it is recomputed
/// fresh on every render rather than something `view` would have to know how
/// to derive.
///
/// This is the first screen a new user reaches, so it follows the same rules
/// as every pane behind it: a semibold `TITLE` heading, the manifest path in
/// Plex (it is a path), and the name, `requires-python` and resolved venv
/// path as [`theme::labeled_value`] pairs -- prose label, Plex value -- since
/// all three are values the user compares character by character. The
/// interpreter and adopt-or-create lines are sentences, so they stay prose.
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
    .spacing(theme::SPACE_2);

    let mut column = iced::widget::column![
        iced::widget::text("Add project")
            .font(theme::FONT_PROSE_SEMIBOLD)
            .size(theme::TITLE),
        iced::widget::text(draft.pyproject_path.display().to_string())
            .font(theme::FONT_DATA)
            .size(theme::DATA)
            .color(theme::SLATE),
        theme::labeled_value("name:", name),
        theme::labeled_value("requires-python:", requires_python),
        iced::widget::text(interpreter_status(draft))
            .size(theme::BODY)
            .color(theme::SLATE),
    ]
    .spacing(theme::SPACE_1);

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
    .spacing(theme::SPACE_2);

    column = column
        .push(venv_row)
        .push(theme::labeled_value("venv:", resolved.to_string()));

    if let Some(outcome) = venv_outcome_status(draft) {
        column = column.push(
            iced::widget::text(outcome)
                .size(theme::BODY)
                .color(theme::SLATE),
        );
    }

    column.push(actions).into()
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
    fn the_interpreter_list_starts_pending_rather_than_empty() {
        let draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        assert_eq!(draft.interpreters, Interpreters::Pending);
        assert!(draft.interpreters.available().is_empty());
    }

    #[test]
    fn before_the_fetch_lands_the_status_reads_as_checking_not_missing() {
        // Pending and Loaded(empty) both have nothing to preselect --
        // interpreter_status must not conflate "still looking" with "none
        // found," which would show the install offer prematurely.
        let draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        assert!(interpreter_status(&draft).contains("checking"));
        assert!(!needs_install_offer(&draft));
    }

    #[test]
    fn a_failed_fetch_says_what_went_wrong_instead_of_checking_forever() {
        // The dead end this state exists for: with a producer that could
        // only return a `Vec`, an unresolvable uv arrived as `vec![]` and
        // the dialog read "(checking available interpreters...)" for the
        // rest of its life -- no error, no retry, and the install button
        // suppressed because an empty list also means "not fetched yet".
        let mut draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        draft.interpreters =
            Interpreters::Failed("no uv binary is available at /nope/uv".to_string());

        let status = interpreter_status(&draft);
        assert!(
            !status.contains("checking"),
            "a failed fetch must not keep claiming to be in progress; got {status}"
        );
        assert!(
            status.contains("/nope/uv"),
            "the failure's own text must survive to the dialog; got {status}"
        );
        assert!(
            !needs_install_offer(&draft),
            "installing through the very uv that could not be run is not an offer worth making"
        );
    }

    #[test]
    fn a_satisfying_interpreter_is_named_in_the_status_line() {
        let mut draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        draft.interpreters = Interpreters::Loaded(vec![interp("3.10.13"), interp("3.11.9")]);
        assert_eq!(interpreter_status(&draft), "python: 3.11.9");
        assert!(!needs_install_offer(&draft));
    }

    #[test]
    fn nothing_satisfying_offers_to_install_instead_of_naming_the_wrong_one() {
        let mut draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        draft.interpreters = Interpreters::Loaded(vec![interp("3.9.18")]);
        assert_eq!(
            interpreter_status(&draft),
            "python: no compatible interpreter found"
        );
        assert!(needs_install_offer(&draft));
    }

    #[test]
    fn a_machine_uv_finds_nothing_on_is_offered_the_install_button() {
        // `Loaded(vec![])` is a real answer -- uv ran and saw nothing -- and
        // is exactly the case the install offer exists for. Only `Pending`
        // and `Failed` must suppress it.
        let mut draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        draft.interpreters = Interpreters::Loaded(Vec::new());
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
    fn a_manifest_with_no_project_table_is_labelled_by_its_directory() {
        // The nameless workspace root is deliberately supported, so an empty
        // label is not a "cannot happen" case: it renders as a bare sidebar
        // marker with nothing after it and a blank detail header.
        let draft = draft_from(
            Path::new("/p/monorepo-root/pyproject.toml"),
            "[tool.uv.workspace]\nmembers = []\n",
        );

        let project = to_project(&draft);

        assert_eq!(project.label, "monorepo-root");
    }

    #[test]
    fn a_declared_name_still_wins_over_the_directory() {
        // The fallback must not start overriding what the manifest says --
        // a project whose name differs from its folder is entirely normal.
        let draft = draft_from(Path::new("/p/some-folder/pyproject.toml"), MANIFEST);
        assert_eq!(to_project(&draft).label, "my-api");
    }

    #[test]
    fn a_chosen_folder_holding_a_venv_states_the_adoption_and_its_version() {
        // Spec §8, and the whole point of probing before the user confirms:
        // `venv_outcome` existed and was tested, but its only caller ran on
        // the job thread *after* confirmation, so the dialog never said a
        // word. Pointing it at a stale 3.9 venv under
        // `requires-python = ">=3.12"` adopted that 3.9 silently, and the
        // header afterwards just read "Python 3.9".
        let mut draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        draft.venv_outcome = Some(VenvOutcome::Adopt {
            python_version: "3.9.18".to_string(),
        });

        let status = venv_outcome_status(&draft).expect("a probed draft must say what will happen");

        assert!(
            status.contains("adopted"),
            "the adoption must be stated outright; got {status}"
        );
        assert!(
            status.contains("3.9.18"),
            "the version of what is being adopted must be named; got {status}"
        );
        assert!(
            status.contains("interpreter selected above will not be used"),
            "the consequence of adopting is that the chosen interpreter is ignored; got {status}"
        );
    }

    #[test]
    fn an_empty_chosen_folder_does_not_claim_an_adoption() {
        let mut draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        draft.venv_outcome = Some(VenvOutcome::Create);

        let status = venv_outcome_status(&draft).expect("a probed draft must say what will happen");

        assert!(
            !status.contains("adopt"),
            "claiming to adopt something that is not there is the opposite mistake; got {status}"
        );
        assert!(status.contains("created"), "got {status}");
    }

    #[test]
    fn nothing_is_stated_until_the_probe_for_the_current_choice_lands() {
        // A statement about the folder the user just moved away from is
        // worse than no statement at all.
        let draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        assert_eq!(draft.venv_outcome, None);
        assert_eq!(venv_outcome_status(&draft), None);
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
