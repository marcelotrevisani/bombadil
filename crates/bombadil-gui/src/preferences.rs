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
use bombadil_core::model::{Config, PythonPin, TerminalChoice, UvSource, VenvLocation};
use bombadil_core::terminal::Os;
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
    /// Light, dark, or the desktop's. First: it is the setting people look
    /// for in a Settings menu before anything Bombadil-specific.
    #[default]
    Appearance,
    Environments,
    Python,
    Uv,
    Terminal,
    Pycharm,
    Scripts,
    EnvVars,
    Indexes,
}

impl Section {
    /// Every section, in the order the list renders them: the four that decide
    /// how an environment is built and opened first, then the two lists.
    pub const ALL: [Section; 9] = [
        Section::Appearance,
        Section::Environments,
        Section::Python,
        Section::Uv,
        Section::Terminal,
        Section::Pycharm,
        Section::Scripts,
        Section::EnvVars,
        Section::Indexes,
    ];

    /// The Settings menu's wording, which has room to name the thing a user
    /// is actually looking for. The section list inside Preferences uses the
    /// shorter [`label`](Self::label).
    pub fn menu_label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Environments => "Virtual environments",
            Section::Python => "Python version",
            Section::Uv => "uv",
            Section::Terminal => "Terminal",
            Section::Pycharm => "PyCharm",
            Section::Scripts => "Scripts",
            Section::EnvVars => "Environment variables",
            Section::Indexes => "Package indexes and auth",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Environments => "Environments",
            Section::Python => "Python",
            Section::Uv => "uv",
            Section::Terminal => "Terminal",
            Section::Pycharm => "PyCharm",
            Section::Scripts => "Scripts",
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

/// What a typed version box means.
///
/// An empty or whitespace-only box is not a pin: typing a version and then
/// clearing it must leave `Unpinned`, not `Version("")`, which uv would
/// refuse with an error about an interpreter nobody asked for.
pub fn pin_from_input(input: &str) -> PythonPin {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        PythonPin::Unpinned
    } else {
        PythonPin::Version(trimmed.to_string())
    }
}

/// What a project would get if it inherited `section`, in words.
///
/// Shown beside the inherit control. A checkbox saying "Inherit from global"
/// with no indication of what that *is* makes the user close Preferences,
/// switch scope, read the value, and come back.
pub fn inherited_label(section: Section, config: &Config, os: Os) -> String {
    match section {
        Section::Python => match &config.settings.python {
            PythonPin::Unpinned => "no pin -- uv chooses".to_string(),
            PythonPin::Version(version) => version.clone(),
        },
        Section::Uv => match &config.settings.uv_source {
            UvSource::Auto => "auto".to_string(),
            UvSource::Bundled => "bundled".to_string(),
            UvSource::FromPath => "from PATH".to_string(),
            UvSource::Custom { path } => path.display().to_string(),
        },
        Section::Terminal => match config.terminal_defaults.get(&os) {
            Some(TerminalChoice::Detected(kind)) => {
                crate::context_menu::terminal_name(*kind).to_string()
            }
            Some(TerminalChoice::Custom { .. }) => "a custom command".to_string(),
            None => "none set".to_string(),
        },
        // These merge rather than override, so "what you would inherit" is
        // not a single value and the panel says so in prose instead. Appearance
        // and PyCharm are global only.
        Section::Appearance
        | Section::Pycharm
        | Section::Environments
        | Section::Scripts
        | Section::EnvVars
        | Section::Indexes => String::new(),
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
    pycharm_detected: Option<&Option<std::path::PathBuf>>,
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
                    theme::Swatch::Parchment
                } else {
                    theme::Swatch::Slate
                })),
        );
    }

    // --- the chosen section ----------------------------------------------
    let panel: iced::Element<'a, Message> = match (section, selected_project) {
        (Section::EnvVars, None) => env_editor::global_view(config, env_drafts),
        (Section::EnvVars, Some(i)) => env_editor::project_view(config, i, env_drafts),
        (Section::Indexes, None) => index_editor::global_view(config, credential_drafts),
        (Section::Indexes, Some(i)) => index_editor::project_view(config, i),
        (Section::Environments, Some(i)) => environments_panel(config, i),
        (Section::Environments, None) => settings_editor::global_view(
            &config.settings,
            &config.projects,
            uv_custom_path_draft,
            uv_custom_validation,
        ),
        (Section::Uv, None) => settings_editor::global_view(
            &config.settings,
            &config.projects,
            uv_custom_path_draft,
            uv_custom_validation,
        ),
        (Section::Uv, Some(i)) => uv_panel(config, scope, i),
        (Section::Appearance, _) => appearance_panel(config, selected_project.is_some()),
        (Section::Scripts, _) => settings_scripts_panel(config, selected_project),
        (Section::Python, _) => python_panel(config, scope, selected_project),
        (Section::Terminal, _) => terminal_panel(config, scope, selected_project),
        (Section::Pycharm, _) => {
            pycharm_panel(config, pycharm_detected, selected_project.is_some())
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
        .style(theme::fill(theme::Swatch::Scrim));

    iced::widget::opaque(iced::widget::mouse_area(positioner).on_press(Message::PreferencesClosed))
}

/// The Environments section, project scope: where this project's virtual
/// environment lives.
///
/// No "Inherit from global" control, unlike Python, uv and Terminal:
/// `VenvLocation::Default` *is* the inherit state and predates
/// `Override<T>`, so the choice already has three options rather than a
/// checkbox plus two.
fn environments_panel<'a>(config: &Config, project_index: usize) -> iced::Element<'a, Message> {
    let Some(project) = config.projects.get(project_index) else {
        return iced::widget::text("").into();
    };
    let id = project.id;
    let removable = project.environments.len() > 1;

    let mut column = iced::widget::column![
        iced::widget::text(
            "Every virtual environment this project has. Sync, Open terminal and Recreate \
             act on the default one, and selecting the project shows it; the sidebar lists \
             them all."
        )
        .size(theme::BODY)
        .style(theme::text_muted),
    ]
    .spacing(theme::SPACE_3);

    for environment in &project.environments {
        let location = environment.location.clone();
        let active = environment.location == project.active;
        let resolved = bombadil_core::venv_path::resolve(project, environment, &config.settings)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|err| err.to_string());
        let pin = match &environment.python {
            PythonPin::Version(version) => version.clone(),
            PythonPin::Unpinned => String::new(),
        };

        let mut card = iced::widget::column![
            iced::widget::row![
                iced::widget::button(
                    // The user's word: this is the environment the project
                    // uses when you select it.
                    iced::widget::text(if active {
                        "default for this project"
                    } else {
                        "make default"
                    })
                    .size(theme::BODY)
                )
                .on_press_maybe((!active).then_some(Message::EnvironmentActivated(
                    project_index,
                    location.clone()
                )))
                .padding([theme::SPACE_1, theme::SPACE_3])
                .style(theme::button_choice(active)),
                iced::widget::Space::new().width(iced::Length::Fill),
                // Never offered for the last one: a project with no
                // environments has nothing to sync, and the state is only
                // reachable by removing them all one at a time.
                iced::widget::button(iced::widget::text("Remove").size(theme::BODY))
                    .on_press_maybe(
                        removable
                            .then_some(Message::EnvironmentRemoveRequested(id, location.clone()))
                    )
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_quiet),
            ]
            .align_y(iced::Alignment::Center),
            theme::labeled_value("path", resolved),
        ]
        .spacing(theme::SPACE_2);

        // Where it lives. `Default` follows the global setting, which is what
        // that variant has always meant -- now per environment rather than
        // per project.
        let place = |label: &'static str, to: VenvLocation, current: bool| {
            let from = location.clone();
            iced::widget::button(iced::widget::text(label).size(theme::BODY))
                .on_press(Message::EnvironmentLocationSelected(id, from, to))
                .padding([theme::SPACE_1, theme::SPACE_2])
                .style(theme::button_choice(current))
        };
        let for_picker = location.clone();
        card = card.push(
            iced::widget::row![
                iced::widget::text("where")
                    .size(theme::LABEL)
                    .style(theme::text_muted)
                    .width(theme::INDEX_LABEL_WIDTH),
                place(
                    "follow global location",
                    VenvLocation::Default,
                    environment.location == VenvLocation::Default
                ),
                place(
                    "alongside",
                    VenvLocation::Alongside,
                    environment.location == VenvLocation::Alongside
                ),
                iced::widget::button(iced::widget::text("folder...").size(theme::BODY))
                    .on_press(Message::EnvironmentPickFolderRequested(id, for_picker))
                    .padding([theme::SPACE_1, theme::SPACE_2])
                    .style(theme::button_choice(matches!(
                        environment.location,
                        VenvLocation::Custom { .. }
                    ))),
            ]
            .spacing(theme::SPACE_1)
            .align_y(iced::Alignment::Center),
        );

        let for_input = location.clone();
        card = card.push(
            iced::widget::row![
                iced::widget::text("python")
                    .size(theme::LABEL)
                    .style(theme::text_muted)
                    .width(theme::INDEX_LABEL_WIDTH),
                iced::widget::text_input("3.12, or empty to let uv choose", &pin)
                    .style(crate::theme::text_field)
                    .on_input(move |version| Message::EnvironmentPinChanged(
                        id,
                        for_input.clone(),
                        version
                    ))
                    .font(theme::FONT_DATA)
                    .size(theme::DATA)
                    .padding(theme::SPACE_1),
            ]
            .spacing(theme::SPACE_2)
            .align_y(iced::Alignment::Center),
        );

        column = column.push(
            iced::widget::container(card)
                .padding(theme::SPACE_3)
                .style(theme::panel),
        );
    }

    column
        .push(
            iced::widget::button(iced::widget::text("Add environment").size(theme::BODY))
                .on_press(Message::EnvironmentAdded(id))
                .padding([theme::SPACE_1, theme::SPACE_3])
                .style(theme::button_quiet),
        )
        .push(
            iced::widget::text(
                "Changing an environment's python deletes and rebuilds it on the next Sync, \
                 which asks first. Removing one here leaves the directory on disk.",
            )
            .size(theme::LABEL)
            .style(theme::text_muted),
        )
        .into()
}

/// The Scripts section. Global only: a project's own pre-activate hooks stay
/// in its Scripts tab, because they are that project's behaviour rather than
/// a setting, and the user asked for the tab to stay.
fn settings_scripts_panel<'a>(
    config: &Config,
    project_index: Option<usize>,
) -> iced::Element<'a, Message> {
    if project_index.is_some() {
        return iced::widget::column![
            iced::widget::text(
                "A project's own pre-activate scripts live in its Scripts tab, next to \
                 Dependencies and Members -- they are that project's behaviour rather than \
                 a setting. Switch scope to Global to edit the scripts every project runs."
            )
            .size(theme::BODY)
            .style(theme::text_muted),
        ]
        .into();
    }
    crate::scripts_editor::global_view(config)
}

/// The Python section: the pin, and what changing it costs.
fn python_panel<'a>(
    config: &Config,
    scope: Scope,
    project_index: Option<usize>,
) -> iced::Element<'a, Message> {
    let project = project_index.and_then(|i| config.projects.get(i));
    // The pin lives on the active environment now. Task 5 turns this panel
    // into a per-environment editor; until then it edits the active one.
    let inheriting = false;
    let effective = match project.and_then(|p| p.active_environment()) {
        Some(environment) => environment.python.clone(),
        None => config.settings.python.clone(),
    };
    let current = match &effective {
        PythonPin::Version(version) => version.clone(),
        PythonPin::Unpinned => String::new(),
    };

    let mut column = iced::widget::column![
        iced::widget::text(
            "The interpreter Sync builds this environment with. Leave it empty to let uv \
             choose, subject to the manifest's requires-python."
        )
        .size(theme::BODY)
        .style(theme::text_muted),
    ]
    .spacing(theme::SPACE_3);

    if project.is_some() {
        column = column.push(inherit_row(Section::Python, config, scope, inheriting));
    }

    column = column.push(
        iced::widget::text_input("3.12, or 3.12.13", &current)
            .style(crate::theme::text_field)
            .on_input(move |version| Message::PreferencesPythonPinChanged(scope, version))
            .font(theme::FONT_DATA)
            .size(theme::DATA)
            .padding(theme::SPACE_2),
    );

    // Stated here, not only in the confirmation. uv does not migrate an
    // environment to another interpreter -- it deletes the directory and
    // builds a new one -- and the confirmation is the last line of defence,
    // not the only warning.
    column
        .push(
            iced::widget::text(
                "Changing this deletes the existing environment and rebuilds it. Anything \
                 installed in it that is not in the lock file is lost. Sync asks first.",
            )
            .size(theme::LABEL)
            .style(theme::text_muted),
        )
        .into()
}

/// The uv section, project scope: which uv runs this project.
///
/// Only `Auto`, `Bundled` and `FromPath`. A custom binary needs a path and a
/// version probe before it can be trusted, and that machinery
/// (`uv_custom_path_draft`, `SettingsUvCustomValidated`) is global. A project
/// that needs a specific binary sets it globally and inherits it; offering a
/// second, unvalidated path box here would be a way to configure a uv that
/// has never been run.
fn uv_panel<'a>(config: &Config, scope: Scope, project_index: usize) -> iced::Element<'a, Message> {
    let Some(project) = config.projects.get(project_index) else {
        return iced::widget::text("").into();
    };
    let inheriting = project.uv_source == bombadil_core::model::Override::Inherit;
    let effective = project
        .uv_source
        .resolve(&config.settings.uv_source)
        .clone();

    let mut column = iced::widget::column![
        iced::widget::text("Which uv runs this project's commands.")
            .size(theme::BODY)
            .style(theme::text_muted),
        inherit_row(Section::Uv, config, scope, inheriting),
    ]
    .spacing(theme::SPACE_2);

    for (label, source) in [
        ("auto", UvSource::Auto),
        ("bundled", UvSource::Bundled),
        ("from PATH", UvSource::FromPath),
    ] {
        let selected = !inheriting && effective == source;
        column = column.push(
            iced::widget::button(iced::widget::text(label).size(theme::BODY))
                .on_press(Message::PreferencesUvSourceOverridden(scope, source))
                .padding([theme::SPACE_1, theme::SPACE_3])
                .style(theme::button_choice(selected)),
        );
    }

    column
        .push(
            iced::widget::text(
                "A specific uv binary is configured globally, in this section's Global \
                 scope, because it is probed for its version before it is trusted.",
            )
            .size(theme::LABEL)
            .style(theme::text_muted),
        )
        .into()
}

/// The Terminal section: which terminal this scope opens, on this OS.
fn terminal_panel<'a>(
    config: &Config,
    scope: Scope,
    project_index: Option<usize>,
) -> iced::Element<'a, Message> {
    let os = Os::host();
    let project = project_index.and_then(|i| config.projects.get(i));
    let inheriting = project.is_some_and(|p| !p.terminals.contains_key(&os));
    let current = match project {
        Some(project) => project
            .terminals
            .get(&os)
            .or_else(|| config.terminal_defaults.get(&os)),
        None => config.terminal_defaults.get(&os),
    }
    .cloned();

    let mut column = iced::widget::column![
        iced::widget::text(
            "Which terminal the Open terminal action launches. Set per operating system, \
             because the terminals that exist differ between them."
        )
        .size(theme::BODY)
        .style(theme::text_muted),
    ]
    .spacing(theme::SPACE_2);

    if project.is_some() {
        column = column.push(inherit_row(Section::Terminal, config, scope, inheriting));
    }

    for kind in bombadil_core::terminal::TerminalKind::for_os(os) {
        let selected = current == Some(TerminalChoice::Detected(kind));
        let warning = crate::context_menu::activation_warning(kind);
        let mut label = iced::widget::column![
            iced::widget::text(crate::context_menu::terminal_name(kind)).size(theme::BODY),
        ];
        if !warning.is_empty() {
            label = label.push(
                iced::widget::text(warning.trim())
                    .size(theme::LABEL)
                    .style(theme::text_muted),
            );
        }
        column = column.push(
            iced::widget::button(label)
                .on_press(Message::PreferencesTerminalOverridden(
                    scope,
                    TerminalChoice::Detected(kind),
                ))
                .width(iced::Length::Fill)
                .padding([theme::SPACE_1, theme::SPACE_2])
                .style(theme::button_choice(selected)),
        );
    }

    column
        .push(
            iced::widget::text(
                "Listed for this machine's OS. A terminal that is not installed reports so \
                 when opened rather than being hidden here.",
            )
            .size(theme::LABEL)
            .style(theme::text_muted),
        )
        .into()
}

/// The "Inherit from global" control, with the value it would inherit beside
/// it -- otherwise the user has to switch scope, read it, and come back.
fn inherit_row<'a>(
    section: Section,
    config: &Config,
    scope: Scope,
    inheriting: bool,
) -> iced::Element<'a, Message> {
    let inherited = inherited_label(section, config, Os::host());
    iced::widget::row![
        iced::widget::checkbox(inheriting)
            .label("Inherit from global")
            .text_size(theme::BODY)
            .on_toggle_maybe(
                (!inheriting)
                    .then_some(move |_| Message::PreferencesInheritToggled(scope, section))
            ),
        iced::widget::text(format!("({inherited})"))
            .size(theme::DATA)
            .style(theme::text_muted),
    ]
    .spacing(theme::SPACE_2)
    .align_y(iced::Alignment::Center)
    .into()
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

/// Light, dark, or the desktop's. Global only: one window, one appearance.
/// `for_project`: Preferences is open on a project, so say this is not one
/// of that project's settings.
fn appearance_panel<'a>(config: &Config, for_project: bool) -> iced::Element<'a, Message> {
    use bombadil_core::model::ThemeChoice;
    let current = config.settings.theme;
    let choice = |label: &'static str, value: ThemeChoice| {
        iced::widget::button(iced::widget::text(label).size(theme::BODY))
            .on_press(Message::ThemeChoiceSelected(value))
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(theme::button_choice(current == value))
    };
    iced::widget::column![
        iced::widget::text("Appearance").size(theme::TITLE),
        iced::widget::row![
            choice("System", ThemeChoice::System),
            choice("Light", ThemeChoice::Light),
            choice("Dark", ThemeChoice::Dark),
        ]
        .spacing(theme::SPACE_1),
        iced::widget::text("System follows your desktop's light or dark setting as it changes.")
            .size(theme::DATA)
            .style(theme::text_muted),
        iced::widget::text("Closing the window").size(theme::TITLE),
        iced::widget::row![
            iced::widget::button(
                iced::widget::text("Keeps Bombadil in the tray").size(theme::BODY)
            )
            .on_press(Message::CloseToTraySelected(true))
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(theme::button_choice(config.settings.close_to_tray)),
            iced::widget::button(iced::widget::text("Quits").size(theme::BODY))
                .on_press(Message::CloseToTraySelected(false))
                .padding([theme::SPACE_1, theme::SPACE_3])
                .style(theme::button_choice(!config.settings.close_to_tray)),
        ]
        .spacing(theme::SPACE_1),
        iced::widget::text(
            "Without a tray (some desktops have none), closing always quits: \
             a hidden window would have no way back."
        )
        .size(theme::DATA)
        .style(theme::text_muted),
    ]
    .push(for_project.then(|| {
        iced::widget::text("These apply to every project, not just this one.")
            .size(theme::DATA)
            .style(theme::text_muted)
    }))
    .spacing(theme::SPACE_3)
    .into()
}

/// Which PyCharm "Open in PyCharm" launches: detected each time, or one
/// chosen by hand. One setting for every project, like Appearance.
fn pycharm_panel<'a>(
    config: &Config,
    detected: Option<&Option<std::path::PathBuf>>,
    for_project: bool,
) -> iced::Element<'a, Message> {
    let chosen = config.settings.pycharm.clone();
    let choice = |label: &'static str, selected: bool, message: Message| {
        iced::widget::button(iced::widget::text(label).size(theme::BODY))
            .on_press(message)
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(theme::button_choice(selected))
    };
    let path = |text: String| {
        iced::widget::text(text)
            .font(theme::FONT_DATA)
            .size(theme::DATA)
    };
    let found = match detected {
        None => iced::widget::text("Looking for PyCharm\u{2026}")
            .size(theme::DATA)
            .style(theme::text_muted)
            .into(),
        Some(Some(found)) => iced::Element::from(
            iced::widget::column![
                iced::widget::text("Detected")
                    .size(theme::LABEL)
                    .style(theme::text_muted),
                path(found.display().to_string()),
            ]
            .spacing(theme::SPACE_1),
        ),
        Some(None) => iced::widget::text(
            "Not found in the usual install locations or on PATH. Choose it below.",
        )
        .size(theme::DATA)
        .style(theme::text_warning)
        .into(),
    };
    iced::widget::column![
        iced::widget::text("PyCharm").size(theme::TITLE),
        iced::widget::row![
            choice(
                "Detect automatically",
                chosen.is_none(),
                Message::PycharmAutomatic
            ),
            choice(
                "Choose executable\u{2026}",
                chosen.is_some(),
                Message::PycharmBrowse
            ),
        ]
        .spacing(theme::SPACE_1),
    ]
    .push(chosen.map(|chosen| {
        iced::widget::column![
            iced::widget::text("Using")
                .size(theme::LABEL)
                .style(theme::text_muted),
            path(chosen.display().to_string()),
        ]
        .spacing(theme::SPACE_1)
    }))
    .push(found)
    .push(
        iced::widget::text(
            "Open in PyCharm launches this, on the project folder, with the selected \
             environment as its interpreter. The Toolbox script, the PyCharm app, or \
             pycharm.sh / pycharm64.exe all work.",
        )
        .size(theme::DATA)
        .style(theme::text_muted),
    )
    .push(for_project.then(|| {
        iced::widget::text("This applies to every project, not just this one.")
            .size(theme::DATA)
            .style(theme::text_muted)
    }))
    .spacing(theme::SPACE_3)
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
    fn appearance_is_global_only_and_first() {
        assert_eq!(Section::ALL[0], Section::Appearance);
        // Nothing to inherit: there is no per-project appearance.
        assert_eq!(
            inherited_label(Section::Appearance, &Config::default(), Os::Linux),
            String::new()
        );
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
