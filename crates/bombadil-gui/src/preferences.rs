//! The Preferences surface: every setting, globally or for one project.
//!
//! One screen with two axes -- a scope across the top, a section down the
//! left -- because the six settings are the same six whichever scope you are
//! in. Splitting them across a global dialog and per-project tabs meant
//! learning each setting twice and never seeing what a project inherited.
//!
//! # Scalars override, lists merge
//!
//! Python, uv and the terminal are single values, so a project either
//! inherits the global one or replaces it: `Override<T>`, rendered as an
//! "Inherit from global" control. Environment variables and indexes are lists
//! that *combine* -- a project's variables layer over the global ones by key,
//! and a project ticks which of the global indexes apply. Those sections say
//! so rather than offering an inherit control that would mean something
//! different from the one three rows above it.

use crate::app::Message;
use crate::theme;
use crate::{env_editor, index_editor, settings_editor};
use bombadil_core::model::Config;
use uuid::Uuid;

/// Whose settings are on screen.
///
/// `Project` carries a `Uuid`, not an index, for the reason
/// `context_menu::ConfirmRecreate` documents at length: an index is only
/// meaningful against the list as it stood when it was captured, and
/// Preferences can be open while projects are added or removed behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Project(Uuid),
}

/// Which group of settings is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    Environments,
    Python,
    Uv,
    Terminal,
    EnvVars,
    Indexes,
}

impl Section {
    /// Every section, in the order the list renders them: the four that decide
    /// how an environment is built and opened first, then the two lists.
    pub const ALL: [Section; 6] = [
        Section::Environments,
        Section::Python,
        Section::Uv,
        Section::Terminal,
        Section::EnvVars,
        Section::Indexes,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Environments => "Environments",
            Section::Python => "Python",
            Section::Uv => "uv",
            Section::Terminal => "Terminal",
            Section::EnvVars => "Env vars",
            Section::Indexes => "Indexes",
        }
    }
}

/// What Preferences is showing. `App` holds an `Option<State>`; `None` is
/// "not open", which is why there is no closed variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    pub scope: Scope,
    pub section: Section,
}

impl Default for State {
    /// Global, because it is the only scope guaranteed to exist: opening on a
    /// project would need there to be one.
    ///
    /// Hand-written rather than derived, and clippy's `derivable_impls` does
    /// not fire: `Scope` has no `Default` of its own, and giving it one would
    /// invite a second, different answer to "which scope is the default".
    fn default() -> Self {
        Self {
            scope: Scope::Global,
            section: Section::ALL[0],
        }
    }
}

/// The scope to actually render, given the config as it stands now.
///
/// A project scope whose project has been removed falls back to `Global`.
/// Without this the next render shows a project that no longer exists -- and
/// an edit made in it would be written into whichever project took its place,
/// the exact class of bug `ConfirmRecreate` carries a `Uuid` to avoid.
pub fn resolve_scope(scope: Scope, config: &Config) -> Scope {
    match scope {
        Scope::Global => Scope::Global,
        Scope::Project(id) if config.projects.iter().any(|project| project.id == id) => scope,
        Scope::Project(_) => Scope::Global,
    }
}

/// Renders Preferences: the scope switch across the top, the section list
/// down the left, the chosen section's fields on the right.
///
/// Layered like `shell::modal_layer` -- `opaque` around the card so presses on
/// it stop there, `mouse_area` outside so presses anywhere else close it. Get
/// that nesting backwards and nothing ever dismisses; see `shell::dismissable`
/// for the bug that taught it.
///
/// `scope` has already been through [`resolve_scope`] by the caller, so a
/// project named here is one that still exists.
#[allow(clippy::too_many_arguments)]
pub fn view<'a>(
    section: Section,
    scope: Scope,
    config: &Config,
    env_drafts: &env_editor::ValueDrafts,
    credential_drafts: &[String],
    uv_custom_path_draft: &str,
    uv_custom_validation: Option<&Result<String, String>>,
) -> iced::Element<'a, Message> {
    let selected_project = match scope {
        Scope::Global => None,
        Scope::Project(id) => config.projects.iter().position(|p| p.id == id),
    };

    // --- scope switch -----------------------------------------------------
    let mut scopes = iced::widget::row![scope_button("Global", Scope::Global, scope)]
        .spacing(theme::SPACE_1)
        .align_y(iced::Alignment::Center);
    for project in &config.projects {
        scopes = scopes.push(scope_button(
            project.label.clone(),
            Scope::Project(project.id),
            scope,
        ));
    }

    // --- section list -----------------------------------------------------
    let mut sections = iced::widget::column![].spacing(0);
    for entry in Section::ALL {
        let selected = entry == section;
        sections = sections.push(
            iced::widget::button(iced::widget::text(entry.label()).size(theme::BODY))
                .on_press_maybe((!selected).then_some(Message::PreferencesSectionSelected(entry)))
                .width(iced::Length::Fill)
                .padding([theme::SPACE_1, theme::SPACE_2])
                .style(theme::button_bare(if selected {
                    theme::PARCHMENT
                } else {
                    theme::SLATE
                })),
        );
    }

    // --- the chosen section ----------------------------------------------
    let panel: iced::Element<'a, Message> = match (section, selected_project) {
        (Section::EnvVars, _) => env_editor::view(config, selected_project, env_drafts),
        (Section::Indexes, _) => index_editor::view(config, selected_project, credential_drafts),
        (Section::Environments | Section::Uv, _) => settings_editor::view(
            &config.settings,
            &config.projects,
            uv_custom_path_draft,
            uv_custom_validation,
        ),
        // Filled in by the next task. Said out loud rather than rendered as an
        // empty panel, which reads as broken rather than unfinished.
        (Section::Python | Section::Terminal, _) => {
            iced::widget::text("Not built yet -- the next task fills this in.")
                .size(theme::BODY)
                .color(theme::SLATE)
                .into()
        }
    };

    let card = iced::widget::container(
        iced::widget::column![
            iced::widget::row![
                iced::widget::text("Preferences")
                    .font(theme::FONT_PROSE_SEMIBOLD)
                    .size(theme::DISPLAY),
                iced::widget::Space::new().width(iced::Length::Fill),
                iced::widget::button(iced::widget::text("Close").size(theme::BODY))
                    .on_press(Message::PreferencesClosed)
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_quiet),
            ]
            .align_y(iced::Alignment::Center),
            iced::widget::scrollable(scopes).width(iced::Length::Fill),
            theme::hairline_row(),
            iced::widget::row![
                iced::widget::container(sections).width(theme::PREFS_SECTION_WIDTH),
                theme::hairline_column(),
                iced::widget::container(iced::widget::scrollable(panel).height(iced::Length::Fill))
                    .width(iced::Length::Fill)
                    .padding([0.0, theme::SPACE_3]),
            ]
            .height(theme::PREFS_BODY_HEIGHT)
            .spacing(theme::SPACE_2),
        ]
        .spacing(theme::SPACE_3),
    )
    .width(theme::PREFS_WIDTH)
    .padding(theme::SPACE_4)
    .style(theme::panel);

    let positioner = iced::widget::container(iced::widget::center(iced::widget::opaque(card)))
        .width(iced::Length::Fill)
        .height(iced::Length::Fill)
        .style(|_theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(theme::SCRIM)),
            ..iced::widget::container::Style::default()
        });

    iced::widget::opaque(iced::widget::mouse_area(positioner).on_press(Message::PreferencesClosed))
}

/// One option of the scope switch. The current one is not pressable, the same
/// "no press needed, no press possible" treatment the tab bar gives its own
/// selected entry.
fn scope_button<'a>(
    label: impl Into<String>,
    scope: Scope,
    current: Scope,
) -> iced::Element<'a, Message> {
    let selected = scope == current;
    iced::widget::button(iced::widget::text(label.into()).size(theme::BODY))
        .on_press_maybe((!selected).then_some(Message::PreferencesScopeSelected(scope)))
        .padding([theme::SPACE_1, theme::SPACE_3])
        .style(theme::button_choice(selected))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bombadil_core::model::Project;

    fn config_with(projects: Vec<Project>) -> Config {
        Config {
            projects,
            ..Config::default()
        }
    }

    #[test]
    fn preferences_open_on_global_and_the_first_section() {
        // Global is the only scope guaranteed to exist. Opening on a project
        // would need one, and there may be none.
        let state = State::default();
        assert_eq!(state.scope, Scope::Global);
        assert_eq!(state.section, Section::ALL[0]);
    }

    #[test]
    fn a_project_scope_survives_the_list_being_reordered() {
        // The whole reason this is a `Uuid`. Adding a project ahead of the
        // selected one shifts every index; the scope must still name the same
        // project.
        let first = Project {
            id: Uuid::from_u128(1),
            label: "api".into(),
            ..Project::default()
        };
        let second = Project {
            id: Uuid::from_u128(2),
            label: "web".into(),
            ..Project::default()
        };
        let scope = Scope::Project(second.id);

        let config = config_with(vec![second.clone(), first.clone()]);
        assert_eq!(resolve_scope(scope, &config), scope);

        let reordered = config_with(vec![first, second]);
        assert_eq!(resolve_scope(scope, &reordered), scope);
    }

    #[test]
    fn a_scope_whose_project_is_gone_falls_back_to_global() {
        // Preferences can be open while the project it is showing is removed
        // from the sidebar behind it. Falling back is what keeps the next
        // render from showing a project that no longer exists -- or worse,
        // writing an edit into whichever project took its place.
        let scope = Scope::Project(Uuid::from_u128(7));
        assert_eq!(resolve_scope(scope, &config_with(vec![])), Scope::Global);
    }

    #[test]
    fn global_always_resolves_to_itself() {
        assert_eq!(
            resolve_scope(Scope::Global, &config_with(vec![])),
            Scope::Global
        );
    }

    #[test]
    fn every_section_has_a_distinct_label() {
        // The section list is the only way to reach five of the six, so two
        // sharing a label makes one unreachable in practice.
        let labels: Vec<&str> = Section::ALL.iter().map(|section| section.label()).collect();
        for (i, label) in labels.iter().enumerate() {
            assert!(
                !label.trim().is_empty(),
                "{:?} has no label",
                Section::ALL[i]
            );
            assert!(
                !labels[i + 1..].contains(label),
                "two sections are both called {label}"
            );
        }
    }
}
