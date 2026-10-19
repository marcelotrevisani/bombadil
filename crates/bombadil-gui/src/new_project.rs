//! "New project…": a name, a place, App or Library, then `uv init`. The
//! existing Add project dialog takes it from there.

use crate::app::Message;
use crate::theme;
use bombadil_core::model::{Config, Project};
use bombadil_core::process::CommandRunner;
pub use bombadil_core::uv::command::InitKind;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Draft {
    pub name: String,
    pub location: PathBuf,
    pub kind: InitKind,
    /// `None` lets uv choose.
    pub python: Option<String>,
    pub git: bool,
    /// What went wrong with the last Create, shown under the form.
    pub error: Option<String>,
}

/// Why `name` cannot be a project name (PEP 508), or `None`.
pub fn name_error(name: &str) -> Option<&'static str> {
    let ok_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    if name.is_empty() {
        return Some("Give the project a name.");
    }
    if !name.chars().all(ok_char) {
        return Some("Use letters, digits, '-', '_' or '.' only.");
    }
    let edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    if !edge(name.chars().next()) || !edge(name.chars().last()) {
        return Some("Start and end with a letter or digit.");
    }
    None
}

/// Beside the most recently added project, else the home folder.
pub fn default_location(projects: &[Project], home: &Path) -> PathBuf {
    projects
        .last()
        .and_then(|p| p.pyproject_path.parent()?.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.to_path_buf())
}

/// The blocking half of Create: checks the place, runs `uv init`, and
/// returns the new `pyproject.toml`. Nothing is created when it refuses.
pub fn init_blocking(
    draft: &Draft,
    config: &Config,
    runner: &dyn CommandRunner,
) -> Result<PathBuf, String> {
    if !draft.location.is_dir() {
        return Err(format!("{} doesn't exist.", draft.location.display()));
    }
    let target = draft.location.join(&draft.name);
    let has_files = std::fs::read_dir(&target).is_ok_and(|mut d| d.next().is_some());
    if has_files {
        return Err(format!("{} already has files in it.", target.display()));
    }
    let uv = crate::app::resolve_uv_for(config, runner)?;
    let spec = bombadil_core::uv::command::init(
        &uv,
        &draft.location,
        &draft.name,
        draft.kind,
        draft.python.as_deref(),
        draft.git,
    );
    let outcome = runner.run(&spec).map_err(|e| e.to_string())?;
    if !outcome.success() {
        let words = format!("{}{}", outcome.stderr, outcome.stdout);
        return Err(words.trim().to_string());
    }
    Ok(target.join("pyproject.toml"))
}

/// The "New project" dialog. Deliberately dumb.
pub fn view<'a>(
    draft: &'a Draft,
    pythons: &'a [String],
    waiting: bool,
) -> iced::Element<'a, Message> {
    use iced::widget::{button, checkbox, column, pick_list, row, text, text_input};
    let muted = |t: String| text(t).size(theme::BODY).style(theme::text_muted);
    let choice = |label: &'static str, kind: InitKind| {
        button(text(label).size(theme::BODY))
            .on_press(Message::NewProjectKind(kind))
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(theme::button_choice(draft.kind == kind))
    };
    const UV_CHOOSES: &str = "Let uv choose";
    let mut options = vec![UV_CHOOSES.to_string()];
    options.extend(pythons.iter().cloned());
    let chosen = draft
        .python
        .clone()
        .unwrap_or_else(|| UV_CHOOSES.to_string());
    let name_problem = (!draft.name.is_empty())
        .then(|| name_error(&draft.name))
        .flatten();
    let ready = !waiting && name_error(&draft.name).is_none();

    let mut form = column![
        text("New project")
            .font(theme::FONT_PROSE_SEMIBOLD)
            .size(theme::DISPLAY),
        text("Name").size(theme::LABEL),
        text_input("my-project", &draft.name)
            .on_input(Message::NewProjectName)
            .on_submit_maybe(ready.then_some(Message::NewProjectCreate))
            .style(theme::text_field),
    ]
    .spacing(theme::SPACE_2);
    if let Some(problem) = name_problem {
        form = form.push(text(problem).size(theme::BODY).style(theme::text_warning));
    }
    form = form
        .push(text("Location").size(theme::LABEL))
        .push(
            row![
                text(draft.location.display().to_string())
                    .font(theme::FONT_DATA)
                    .size(theme::DATA)
                    .width(iced::Length::Fill),
                button(text("Browse\u{2026}").size(theme::BODY))
                    .on_press(Message::NewProjectBrowse)
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_secondary),
            ]
            .spacing(theme::SPACE_2)
            .align_y(iced::Alignment::Center),
        )
        .push(muted(format!(
            "Creates {}",
            draft.location.join(&draft.name).display()
        )))
        .push(text("Kind").size(theme::LABEL))
        .push(
            row![
                choice("App", InitKind::App),
                choice("Library", InitKind::Lib)
            ]
            .spacing(theme::SPACE_1),
        )
        .push(muted(
            match draft.kind {
                InitKind::App => "App: something you run. It gets a Run button.",
                InitKind::Lib => "Library: something other code imports.",
            }
            .into(),
        ))
        .push(text("Python").size(theme::LABEL))
        .push(pick_list(options, Some(chosen), |v: String| {
            Message::NewProjectPython((v != UV_CHOOSES).then_some(v))
        }))
        .push(
            checkbox(draft.git)
                .label("Start a Git repository")
                .on_toggle(Message::NewProjectGit),
        );
    if let Some(error) = &draft.error {
        form = form.push(
            text(error.clone())
                .size(theme::BODY)
                .style(theme::text_warning),
        );
    }
    form = form.push(
        row![
            button(
                text(if waiting {
                    "Creating\u{2026}"
                } else {
                    "Create"
                })
                .size(theme::BODY)
            )
            .on_press_maybe(ready.then_some(Message::NewProjectCreate))
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(theme::button_primary),
            button(text("Cancel").size(theme::BODY))
                .on_press_maybe((!waiting).then_some(Message::NewProjectCancelled))
                .padding([theme::SPACE_1, theme::SPACE_3])
                .style(theme::button_quiet),
        ]
        .spacing(theme::SPACE_2),
    );
    // `shell::overlay_layer` draws the card around it.
    form.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{fake_uv_settings, version_probe};
    use bombadil_core::process::{CommandOutcome, FakeCommandRunner};

    #[test]
    fn names_follow_python_package_rules() {
        assert_eq!(name_error("weather-api"), None);
        assert_eq!(name_error("a"), None);
        assert_eq!(name_error("x.y_z-1"), None);
        assert!(name_error("").is_some());
        assert!(name_error("-start").is_some());
        assert!(name_error("end-").is_some());
        assert!(name_error("has space").is_some());
        assert!(name_error("ünï").is_some());
    }

    #[test]
    fn the_default_location_is_beside_the_last_project() {
        let home = Path::new("/h");
        assert_eq!(default_location(&[], home), home);
        let p = Project {
            pyproject_path: Path::new("/h")
                .join("code")
                .join("api")
                .join("pyproject.toml"),
            ..Default::default()
        };
        assert_eq!(default_location(&[p], home), Path::new("/h").join("code"));
    }

    fn draft(location: &Path) -> Draft {
        Draft {
            name: "demo".into(),
            location: location.to_path_buf(),
            kind: InitKind::App,
            python: None,
            git: true,
            error: None,
        }
    }

    fn config() -> Config {
        Config {
            settings: fake_uv_settings(),
            ..Config::default()
        }
    }

    fn outcome(status: i32, stderr: &str) -> CommandOutcome {
        CommandOutcome {
            status: Some(status),
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn a_folder_with_files_is_refused_before_uv_runs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("demo")).unwrap();
        std::fs::write(dir.path().join("demo").join("keep.txt"), "x").unwrap();
        let runner = FakeCommandRunner::new();
        let err = init_blocking(&draft(dir.path()), &config(), &runner).unwrap_err();
        assert!(err.contains("already has files"), "{err}");
        assert!(runner.calls().is_empty(), "uv never ran");
    }

    #[test]
    fn a_missing_location_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let runner = FakeCommandRunner::new();
        let err = init_blocking(&draft(&dir.path().join("nope")), &config(), &runner).unwrap_err();
        assert!(err.contains("doesn't exist"), "{err}");
    }

    #[test]
    fn a_failed_init_returns_uvs_words() {
        let dir = tempfile::tempdir().unwrap();
        let runner = FakeCommandRunner::new();
        version_probe(&runner);
        runner.expect("uv", &["init"], outcome(2, "error: boom\n"));
        let err = init_blocking(&draft(dir.path()), &config(), &runner).unwrap_err();
        assert!(err.contains("boom"), "{err}");
    }

    #[test]
    fn a_good_init_hands_back_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let runner = FakeCommandRunner::new();
        version_probe(&runner);
        runner.expect("uv", &["init"], outcome(0, ""));
        let path = init_blocking(&draft(dir.path()), &config(), &runner).unwrap();
        assert_eq!(path, dir.path().join("demo").join("pyproject.toml"));
        let init = runner
            .calls()
            .into_iter()
            .find(|c| c.args.first().map(String::as_str) == Some("init"))
            .unwrap();
        assert!(init.args.contains(&"--app".to_string()));
    }
}
