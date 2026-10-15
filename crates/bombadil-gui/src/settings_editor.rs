//! The Settings tab (spec §5, Task 6): which uv binary Bombadil runs, and
//! where new virtual environments go when a project does not name its own
//! location.
//!
//! # Criterion 1: a `uv_source` change takes effect on the next resolve
//!
//! There is nothing here to get wrong in the usual way (a value read once at
//! boot and cached on `App`): `app::resolve_uv_for` takes `&Config` fresh on
//! every call site that needs a uv binary (`run_sync`,
//! `list_interpreters_blocking`, `install_interpreter_blocking`, the
//! Dependencies and Members fetches), and none of them memoize a
//! [`bombadil_core::uv::ResolvedUv`] anywhere on `App`. So the whole of
//! criterion 1 is: does choosing a source actually land in
//! `app.config.settings.uv_source`, the one field every one of those call
//! sites reads? [`apply_choice`] is that write, and `app.rs`'s own tests
//! prove a *second* resolve after the change sees it, in the same running
//! `App`, with no reconstruction in between -- the only way "cached at boot"
//! could hide.
//!
//! # Criterion 2: a custom binary is validated before it is saved
//!
//! Unlike `Auto`/`Bundled`/`FromPath`, `Custom` names a path the user just
//! typed, which can point at nothing, or at something too old. Applying it to
//! `Settings` immediately -- the way [`apply_choice`] does for the other
//! three -- would let a bad path sit in `config.toml` until the next `uv
//! sync` fails with `UvResolveError`'s own text, on a screen far from the one
//! where the mistake was made. So `Custom` never goes through
//! [`apply_choice`] at all: `app.rs`'s `SettingsUvCustomValidateRequested`
//! runs `uv::resolve` against the typed path through `job::run` first
//! (`validate_uv_source_blocking`, its own function so a test can call it
//! directly without a `Task`), and only `SettingsUvCustomValidated(Ok(_))` --
//! the landed result of a *passing* validation -- ever writes
//! `Settings::uv_source`. `uv::resolve`'s `Custom` branch already reports a
//! version below `MIN_UV_VERSION` as [`bombadil_core::error::UvResolveError::TooOld`];
//! this module adds no second copy of that check.
//!
//! # Criterion 3: `default_venv_location` changes the resolved path, never
//! the venv itself
//!
//! [`resolved_default_venv_paths`] is the read half: every project on
//! `VenvLocation::Default`, resolved fresh through
//! `bombadil_core::venv_path::resolve` -- which performs no filesystem access
//! at all (see its own doc) -- so a location change is reflected the moment
//! `view` next renders, the same "recompute, don't cache" trick
//! `add_project::resolved_venv_path_display` uses for the analogous question
//! in the add-project dialog.
//!
//! The write half is [`apply_default_venv_location`], and it is exactly as
//! narrow as it looks: a `Settings` field assignment, nothing else. Nothing
//! in this module, and nothing `app.rs` wires up for
//! `SettingsDefaultVenvLocation*`, calls `bombadil_core::venv_path::delete` or
//! touches the filesystem in any way -- the only call site for `delete` in
//! the whole crate is `RecreateVenvConfirmed`, gated on an explicit user
//! confirmation naming the path about to be removed (see `context_menu`'s own
//! module doc). A settings change cannot reach it. `app.rs`'s own test for
//! this criterion does not take that on faith: it creates a real venv on
//! disk, changes the setting, and checks the old directory is still there
//! with its `pyvenv.cfg` intact -- proof, not an assertion that nothing
//! *looks* wrong.

use crate::app::Message;
use crate::theme;
use bombadil_core::model::{DefaultVenvLocation, Project, Settings, UvSource, VenvLocation};
use bombadil_core::venv_path;
use std::path::PathBuf;

/// The three `uv_source` choices that apply immediately, with no path to
/// validate first. `Custom` is deliberately not a variant here -- see the
/// module doc's criterion 2 section for why it needs a different message
/// entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UvSourceChoice {
    Auto,
    Bundled,
    FromPath,
}

/// Criterion 1's write half: applies one of the three no-validation-needed
/// choices directly. The caller (`app::update`) persists immediately after,
/// the same as every other settings-in-place edit in this crate.
pub fn apply_choice(settings: &mut Settings, choice: UvSourceChoice) {
    settings.uv_source = match choice {
        UvSourceChoice::Auto => UvSource::Auto,
        UvSourceChoice::Bundled => UvSource::Bundled,
        UvSourceChoice::FromPath => UvSource::FromPath,
    };
}

/// Which `UvSourceChoice`, if any, `settings.uv_source` currently names --
/// `None` for `Custom`, which has no button of its own (it is shown via its
/// own path/validation state instead). Used only to decide which button
/// looks pressed; never to decide whether a message may be sent.
pub fn current_choice(settings: &Settings) -> Option<UvSourceChoice> {
    match settings.uv_source {
        UvSource::Auto => Some(UvSourceChoice::Auto),
        UvSource::Bundled => Some(UvSourceChoice::Bundled),
        UvSource::FromPath => Some(UvSourceChoice::FromPath),
        UvSource::Custom { .. } => None,
    }
}

/// Criterion 3's write half for the "alongside" choice -- the other half,
/// `Central { path }`, is written directly by `app::update` once a folder is
/// picked, since there is no separate field here to validate first (any
/// folder the picker returns is usable; unlike a uv binary, there is nothing
/// to run and nothing that can be "too old").
pub fn apply_default_venv_location(settings: &mut Settings, location: DefaultVenvLocation) {
    settings.default_venv_location = location;
}

/// Criterion 3's read half: every project on `VenvLocation::Default`, paired
/// with its resolved path under the *current* settings. `Err` carries
/// `venv_path::resolve`'s own message (a project with no directory
/// component) rather than being dropped -- silently skipping it would make a
/// broken project invisible on the one tab meant to explain where its venv
/// would go.
pub fn resolved_default_venv_paths(
    projects: &[Project],
    settings: &Settings,
) -> Vec<(String, Result<PathBuf, String>)> {
    // Every environment that follows the global default, not just one per
    // project: a project can now have several, and only some of them may be
    // on `Default`. Naming the project alone would say a change affects it
    // wholesale when it moves one of its environments.
    projects
        .iter()
        .flat_map(|project| {
            project
                .environments
                .iter()
                .filter(|environment| matches!(environment.location, VenvLocation::Default))
                .map(move |environment| {
                    (
                        project.label.clone(),
                        venv_path::resolve(project, environment, settings)
                            .map_err(|e| e.to_string()),
                    )
                })
        })
        .collect()
}

/// Renders one uv-source choice button, disabled (via `on_press_maybe(None)`)
/// when it already names the current choice -- the same "no press needed, no
/// press possible" treatment `index_editor::auth_button` gives an
/// already-selected auth kind.
fn uv_source_button<'a>(
    label: &'static str,
    choice: UvSourceChoice,
    current: Option<UvSourceChoice>,
) -> iced::Element<'a, Message> {
    iced::widget::button(iced::widget::text(label))
        .on_press_maybe(
            (current != Some(choice)).then_some(Message::SettingsUvSourceChoiceChanged(choice)),
        )
        .style(theme::button_choice(current == Some(choice)))
        .into()
}

/// Renders the whole tab.
/// The global half: which uv runs, and where new environments go.
///
/// Named `global_view` since Preferences arrived, because there is a project
/// half now -- `Project::venv` and `Project::uv_source` -- rendered by
/// `preferences` rather than here.
pub fn global_view<'a>(
    settings: &Settings,
    projects: &[Project],
    uv_custom_path_draft: &str,
    uv_custom_validation: Option<&Result<String, String>>,
) -> iced::Element<'a, Message> {
    let current = current_choice(settings);
    let uv_source_buttons = iced::widget::row![
        uv_source_button("auto", UvSourceChoice::Auto, current),
        uv_source_button("bundled", UvSourceChoice::Bundled, current),
        uv_source_button("from PATH", UvSourceChoice::FromPath, current),
    ]
    .spacing(theme::SPACE_1);

    // Three of these four are words and one is a path, so the face is picked
    // per arm rather than after the join -- see `theme::labeled_prose`.
    let current_source: iced::Element<'a, Message> = match &settings.uv_source {
        UvSource::Auto => theme::labeled_prose("current uv source:", "auto"),
        UvSource::Bundled => theme::labeled_prose("current uv source:", "bundled"),
        UvSource::FromPath => theme::labeled_prose("current uv source:", "from PATH"),
        UvSource::Custom { path } => {
            theme::labeled_value("current uv source:", path.display().to_string())
        }
    };

    let custom_path_input = iced::widget::text_input("path to a uv binary", uv_custom_path_draft)
        .style(crate::theme::text_field)
        .font(theme::FONT_DATA)
        .size(theme::DATA)
        .on_input(Message::SettingsUvCustomPathChanged);
    let validate_button = iced::widget::button(iced::widget::text("validate and use"))
        .on_press_maybe(
            (!uv_custom_path_draft.trim().is_empty())
                .then_some(Message::SettingsUvCustomValidateRequested),
        )
        .style(theme::button_secondary);

    let mut column = iced::widget::column![
        iced::widget::text("Settings")
            .font(theme::FONT_PROSE_SEMIBOLD)
            .size(theme::TITLE),
        iced::widget::text("uv binary")
            .font(theme::FONT_PROSE)
            .size(theme::LABEL)
            .style(theme::text_muted),
        current_source,
        uv_source_buttons,
        iced::widget::row![custom_path_input, validate_button].spacing(theme::SPACE_2),
    ]
    .spacing(theme::SPACE_3);

    if let Some(validation) = uv_custom_validation {
        let row: iced::Element<'a, Message> = match validation {
            Ok(version) => theme::labeled_value("validated: uv", version.clone()),
            Err(message) => iced::widget::text(format!("could not use this uv: {message}"))
                .size(theme::BODY)
                .into(),
        };
        column = column.push(row);
    }

    // Same per-arm face choice as `current_source` above: a sentence in one
    // arm, a path in the other.
    let default_venv: iced::Element<'a, Message> = match &settings.default_venv_location {
        DefaultVenvLocation::Alongside => theme::labeled_prose(
            "current default:",
            "alongside each project (<project>/.venv)",
        ),
        DefaultVenvLocation::Central { path } => {
            theme::labeled_value("current default:", path.display().to_string())
        }
    };
    let alongside_button = iced::widget::button(iced::widget::text("alongside each project"))
        .on_press_maybe(
            (!matches!(
                settings.default_venv_location,
                DefaultVenvLocation::Alongside
            ))
            .then_some(Message::SettingsDefaultVenvLocationAlongsideSelected),
        )
        .style(theme::button_choice(matches!(
            settings.default_venv_location,
            DefaultVenvLocation::Alongside
        )));
    let choose_folder_button =
        iced::widget::button(iced::widget::text("choose a central folder..."))
            .on_press(Message::SettingsDefaultVenvLocationPickFolderRequested)
            .style(theme::button_secondary);

    column = column
        .push(
            iced::widget::text("default venv location")
                .font(theme::FONT_PROSE)
                .size(theme::LABEL)
                .style(theme::text_muted),
        )
        .push(default_venv)
        .push(iced::widget::row![alongside_button, choose_folder_button].spacing(theme::SPACE_2));

    for (label, resolved) in resolved_default_venv_paths(projects, settings) {
        let value: iced::Element<'a, Message> = match resolved {
            Ok(path) => iced::widget::text(path.display().to_string())
                .font(theme::FONT_DATA)
                .size(theme::DATA)
                .style(theme::text_muted)
                .into(),
            Err(message) => iced::widget::text(message)
                .size(theme::DATA)
                .style(theme::text_muted)
                .into(),
        };
        column = column.push(
            iced::widget::row![
                iced::widget::text(format!("{label}:"))
                    .size(theme::BODY)
                    .style(theme::text_muted),
                value,
            ]
            .spacing(theme::SPACE_1),
        );
    }

    column.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bombadil_core::model::{Environment, PythonPin};
    use std::path::PathBuf;
    use uuid::Uuid;

    fn project(label: &str, venv: VenvLocation, dir: &str) -> Project {
        Project {
            id: Uuid::from_u128(1),
            label: label.into(),
            pyproject_path: PathBuf::from(dir).join("pyproject.toml"),
            environments: vec![Environment {
                location: venv.clone(),
                python: PythonPin::Unpinned,
            }],
            active: venv,
            ..Project::default()
        }
    }

    // --- criterion 1: applying a no-validation choice writes the field
    // every resolve reads ---

    #[test]
    fn apply_choice_bundled_sets_the_bundled_source() {
        let mut settings = Settings {
            uv_source: UvSource::Auto,
            ..Settings::default()
        };
        apply_choice(&mut settings, UvSourceChoice::Bundled);
        assert_eq!(settings.uv_source, UvSource::Bundled);
    }

    #[test]
    fn apply_choice_from_a_previous_custom_source_replaces_it() {
        // The mutation this earns its place against: a naive `apply_choice`
        // that only handles the "coming from Auto/Bundled/FromPath" case and
        // leaves a `Custom` source in place because it superficially looks
        // like a bespoke, deliberate configuration.
        let mut settings = Settings {
            uv_source: UvSource::Custom {
                path: PathBuf::from("/opt/uv"),
            },
            ..Settings::default()
        };
        apply_choice(&mut settings, UvSourceChoice::Auto);
        assert_eq!(settings.uv_source, UvSource::Auto);
    }

    #[test]
    fn current_choice_is_none_for_a_custom_source() {
        let settings = Settings {
            uv_source: UvSource::Custom {
                path: PathBuf::from("/opt/uv"),
            },
            ..Settings::default()
        };
        assert_eq!(current_choice(&settings), None);
    }

    #[test]
    fn current_choice_names_each_of_the_three_plain_variants() {
        for (source, want) in [
            (UvSource::Auto, UvSourceChoice::Auto),
            (UvSource::Bundled, UvSourceChoice::Bundled),
            (UvSource::FromPath, UvSourceChoice::FromPath),
        ] {
            let settings = Settings {
                uv_source: source,
                ..Settings::default()
            };
            assert_eq!(current_choice(&settings), Some(want));
        }
    }

    // --- criterion 3: the read half ---

    #[test]
    fn resolved_default_venv_paths_follows_the_alongside_setting() {
        let settings = Settings {
            default_venv_location: DefaultVenvLocation::Alongside,
            ..Settings::default()
        };
        let projects = vec![project("api", VenvLocation::Default, "/work/api")];

        let got = resolved_default_venv_paths(&projects, &settings);

        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "api");
        assert_eq!(
            got[0].1.as_ref().unwrap(),
            &PathBuf::from("/work/api/.venv")
        );
    }

    #[test]
    fn resolved_default_venv_paths_follows_the_central_setting() {
        // The whole point of criterion 3: the exact same project resolves
        // somewhere else once the setting changes, with no change to the
        // project itself.
        let projects = vec![project("api", VenvLocation::Default, "/work/api")];

        let alongside = resolved_default_venv_paths(
            &projects,
            &Settings {
                default_venv_location: DefaultVenvLocation::Alongside,
                ..Settings::default()
            },
        );
        let central = resolved_default_venv_paths(
            &projects,
            &Settings {
                default_venv_location: DefaultVenvLocation::Central {
                    path: PathBuf::from("/home/t/.venvs"),
                },
                ..Settings::default()
            },
        );

        assert_ne!(
            alongside[0].1.as_ref().unwrap(),
            central[0].1.as_ref().unwrap(),
            "changing the setting must change the resolved path"
        );
        assert!(
            central[0].1.as_ref().unwrap().starts_with("/home/t/.venvs"),
            "got {:?}",
            central[0].1
        );
    }

    #[test]
    fn resolved_default_venv_paths_ignores_a_project_with_its_own_location() {
        // Only `VenvLocation::Default` is meant to move when the setting
        // changes -- `Alongside`/`Custom` projects opted out, and the tab
        // must not claim otherwise.
        let projects = vec![
            project("api", VenvLocation::Default, "/work/api"),
            project("pinned", VenvLocation::Alongside, "/work/pinned"),
            project(
                "custom",
                VenvLocation::Custom {
                    path: PathBuf::from("/mnt/fast/custom"),
                },
                "/work/custom",
            ),
        ];
        let settings = Settings {
            default_venv_location: DefaultVenvLocation::Central {
                path: PathBuf::from("/home/t/.venvs"),
            },
            ..Settings::default()
        };

        let got = resolved_default_venv_paths(&projects, &settings);

        assert_eq!(got.len(), 1, "got {got:?}");
        assert_eq!(got[0].0, "api");
    }

    #[test]
    fn apply_default_venv_location_writes_the_field_resolve_reads() {
        let mut settings = Settings::default();
        apply_default_venv_location(
            &mut settings,
            DefaultVenvLocation::Central {
                path: PathBuf::from("/home/t/.venvs"),
            },
        );
        assert_eq!(
            settings.default_venv_location,
            DefaultVenvLocation::Central {
                path: PathBuf::from("/home/t/.venvs")
            }
        );
    }
}
