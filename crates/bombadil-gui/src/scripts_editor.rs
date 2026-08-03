//! The Scripts tab: global pre-activate scripts, and (once a project is
//! selected) that project's own scripts alongside them.
//!
//! Spec §6: pre-activate scripts run in order, after the project's variables
//! and before activation. `envspec::compose` runs every global script, in
//! order, then every project script, in order (`inputs.config.global_scripts
//! .iter().chain(&inputs.project.scripts)`, `envspec/mod.rs`) -- concatenation
//! of two sequences, not one flat list. A script that exits non-zero aborts
//! the whole composition rather than applying partially (`envspec::script::apply`).
//!
//! # Why reordering is scoped, not a single flat list
//!
//! Criterion 1 asks for display order to match run order, and for reordering
//! to persist. A single list sorted by some combined index would let the UI
//! *appear* to offer moving a project script ahead of a global one -- which
//! `compose` can never honour, since every global script has already run by
//! the time the first project script starts. [`move_up`] and [`move_down`]
//! only ever see one scope's `&mut [Script]` at a time (the same slice
//! [`scripts_mut`] resolves), so there is no operation in this module that
//! could move a script across the global/project boundary even if the view
//! tried to offer one.
//!
//! [`execution_order`] makes the concatenation explicit and checkable against
//! `compose`'s own chain, rather than trusting that two side-by-side sections
//! in `view` happen to read in the right order.

use crate::app::Message;
use crate::theme;
use bombadil_core::model::{Config, Script};
use std::path::PathBuf;

/// Which list of scripts a row edits: `Config::global_scripts`, or one
/// project's `Project::scripts` addressed by its position in
/// `App::config.projects`.
/// Mirrors `env_editor::EnvScope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptScope {
    Global,
    Project(usize),
}

/// The `&mut Vec<Script>` a scope addresses. `None` only for a `Project`
/// scope whose index no longer exists -- the same stale-index tolerance
/// `env_editor::vars_mut` extends.
pub fn scripts_mut(config: &mut Config, scope: ScriptScope) -> Option<&mut Vec<Script>> {
    match scope {
        ScriptScope::Global => Some(&mut config.global_scripts),
        ScriptScope::Project(i) => config.projects.get_mut(i).map(|p| &mut p.scripts),
    }
}

/// Appends a script at `path`, enabled by default and with no args -- the
/// starting state a user picking a file expects: it will run, unmodified,
/// the next time this scope's scripts do.
pub fn add_script(scripts: &mut Vec<Script>, path: PathBuf) {
    scripts.push(Script {
        path,
        args: Vec::new(),
        enabled: true,
    });
}

pub fn remove_script(scripts: &mut Vec<Script>, index: usize) {
    if index < scripts.len() {
        scripts.remove(index);
    }
}

/// Flips a script's `enabled` flag in place (criterion 2). Never removes the
/// row -- a disabled script is still shown, still reorderable, and still
/// there to re-enable; `envspec::script::apply` is what actually skips
/// running it (`if !script.enabled { return Ok(base.clone()); }`).
pub fn toggle_enabled(scripts: &mut [Script], index: usize) {
    if let Some(script) = scripts.get_mut(index) {
        script.enabled = !script.enabled;
    }
}

/// Moves `scripts[index]` one position earlier within its own scope
/// (criterion 1). A no-op at the first position or an out-of-range index --
/// nothing to move earlier into or nothing there at all.
pub fn move_up(scripts: &mut [Script], index: usize) {
    if index != 0 && index < scripts.len() {
        scripts.swap(index - 1, index);
    }
}

/// Moves `scripts[index]` one position later within its own scope.
/// A no-op at the last position or an out-of-range index.
pub fn move_down(scripts: &mut [Script], index: usize) {
    if index + 1 < scripts.len() {
        scripts.swap(index, index + 1);
    }
}

/// The order scripts actually run in (spec §6): every global script, then
/// every project script, each in its own stored order. Mirrors
/// `envspec::compose`'s own chain (`global_scripts.iter().chain(&project
/// .scripts)`) exactly, so a caller can check display order against this
/// rather than against a re-derivation of what `compose` does.
pub fn execution_order<'a>(global: &'a [Script], project: &'a [Script]) -> Vec<&'a Script> {
    global.iter().chain(project.iter()).collect()
}

/// The sentence the tab states (criterion 4): a user seeing a project script
/// run after a global one with the same effect needs this to make sense of
/// the order, not just see two labelled sections and guess.
pub const ORDER_NOTE: &str = "Global scripts run first, in the order shown; project scripts run after them, in their own order.";

/// Renders one scope's rows. Deliberately dumb, like every other `view` in
/// this crate: all the logic worth testing lives in the functions above.
///
/// A script's path is data -- read character by character, the same as any
/// other path in this application -- so it renders in Plex; "enabled" and the
/// buttons are prose. Each row sits on a `bark` surface, the same treatment
/// every row in every pane gets.
fn rows<'a>(scope: ScriptScope, scripts: &[Script]) -> iced::Element<'a, Message> {
    let mut column = iced::widget::column![].spacing(theme::SPACE_2);
    let last_index = scripts.len().saturating_sub(1);
    for (index, script) in scripts.iter().enumerate() {
        let path_text = iced::widget::text(script.path.display().to_string())
            .font(theme::FONT_DATA)
            .size(theme::DATA);

        let enabled_toggle = iced::widget::checkbox(script.enabled)
            .label("enabled")
            .on_toggle(move |_| Message::ScriptEnabledToggled(scope, index));

        let move_up_button = iced::widget::button(iced::widget::text("up"))
            .on_press_maybe((index != 0).then_some(Message::ScriptMovedUp(scope, index)));
        let move_down_button = iced::widget::button(iced::widget::text("down")).on_press_maybe(
            (index != last_index).then_some(Message::ScriptMovedDown(scope, index)),
        );

        let remove = iced::widget::button(iced::widget::text("remove"))
            .on_press(Message::ScriptRemoved(scope, index));

        let row = iced::widget::row![
            path_text,
            enabled_toggle,
            move_up_button,
            move_down_button,
            remove
        ]
        .spacing(theme::SPACE_2);

        column = column.push(iced::widget::container(row).padding(theme::SPACE_1).style(
            |_theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(theme::BARK)),
                ..iced::widget::container::Style::default()
            },
        ));
    }
    column.into()
}

/// Renders the whole tab: global scripts (which run first), then -- once a
/// project is selected -- that project's own scripts, which run after them.
/// The global scripts alone, for the Preferences Scripts section. The
/// per-project half stays in the Scripts tab, which the user asked to keep:
/// a pre-activate hook is a project's own behaviour, and only the global list
/// is a setting.
pub fn global_view<'a>(config: &Config) -> iced::Element<'a, Message> {
    iced::widget::column![
        iced::widget::text(ORDER_NOTE)
            .size(theme::BODY)
            .color(theme::SLATE),
        rows(ScriptScope::Global, &config.global_scripts),
        iced::widget::button(iced::widget::text("Add global script").size(theme::BODY))
            .on_press(Message::ScriptAddRequested(ScriptScope::Global))
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(theme::button_quiet),
    ]
    .spacing(theme::SPACE_3)
    .into()
}

pub fn view<'a>(config: &Config, selected_project: Option<usize>) -> iced::Element<'a, Message> {
    let mut column = iced::widget::column![
        iced::widget::text("Scripts")
            .font(theme::FONT_PROSE_SEMIBOLD)
            .size(theme::TITLE),
        iced::widget::text(ORDER_NOTE)
            .size(theme::BODY)
            .color(theme::SLATE),
        iced::widget::text("Global (runs first)")
            .font(theme::FONT_PROSE)
            .size(theme::LABEL)
            .color(theme::SLATE),
        rows(ScriptScope::Global, &config.global_scripts),
        iced::widget::button(iced::widget::text("Add global script"))
            .on_press(Message::ScriptAddRequested(ScriptScope::Global)),
    ]
    .spacing(theme::SPACE_3);

    if let Some(i) = selected_project
        && let Some(project) = config.projects.get(i)
    {
        column = column.push(
            iced::widget::text("Project (runs after global)")
                .font(theme::FONT_PROSE)
                .size(theme::LABEL)
                .color(theme::SLATE),
        );
        column = column.push(rows(ScriptScope::Project(i), &project.scripts));
        column = column.push(
            iced::widget::button(iced::widget::text("Add project script"))
                .on_press(Message::ScriptAddRequested(ScriptScope::Project(i))),
        );

        // The concrete order this project's own run would follow, spelling
        // out `ORDER_NOTE`'s claim as an actual count rather than leaving the
        // user to add up two sections themselves.
        let total = execution_order(&config.global_scripts, &project.scripts).len();
        column = column.push(
            iced::widget::text(format!(
                "{total} script(s) will run for this project, in the order above"
            ))
            .size(theme::BODY)
            .color(theme::SLATE),
        );
    }

    column.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bombadil_core::model::Project;
    use uuid::Uuid;

    fn script(path: &str, enabled: bool) -> Script {
        Script {
            path: PathBuf::from(path),
            args: Vec::new(),
            enabled,
        }
    }

    fn project_with_scripts(scripts: Vec<Script>) -> Project {
        Project {
            id: Uuid::from_u128(1),
            label: "api".into(),
            pyproject_path: PathBuf::from("/p/api/pyproject.toml"),
            environments: vec![bombadil_core::model::Environment {
                location: bombadil_core::model::VenvLocation::Alongside,
                python: bombadil_core::model::PythonPin::Unpinned,
            }],
            active: bombadil_core::model::VenvLocation::Alongside,
            scripts,
            ..Project::default()
        }
    }

    // --- scripts_mut scope wiring ---

    #[test]
    fn global_scope_addresses_the_global_list() {
        let mut config = Config {
            global_scripts: vec![script("/etc/pre.sh", true)],
            ..Config::default()
        };
        let scripts = scripts_mut(&mut config, ScriptScope::Global).expect("global always exists");
        assert_eq!(scripts.len(), 1);
        assert_eq!(scripts[0].path, PathBuf::from("/etc/pre.sh"));
    }

    #[test]
    fn project_scope_addresses_that_projects_own_list_not_global() {
        let mut config = Config {
            global_scripts: vec![script("/etc/global.sh", true)],
            projects: vec![project_with_scripts(vec![script("/p/api/local.sh", true)])],
            ..Config::default()
        };
        let scripts = scripts_mut(&mut config, ScriptScope::Project(0)).expect("project 0 exists");
        assert_eq!(scripts.len(), 1);
        assert_eq!(
            scripts[0].path,
            PathBuf::from("/p/api/local.sh"),
            "must address the project's own scripts, not global_scripts"
        );
    }

    #[test]
    fn a_stale_project_index_addresses_nothing() {
        let mut config = Config::default();
        assert!(scripts_mut(&mut config, ScriptScope::Project(0)).is_none());
    }

    // --- add / remove ---

    #[test]
    fn add_script_appends_an_enabled_row_with_no_args() {
        let mut scripts = Vec::new();
        add_script(&mut scripts, PathBuf::from("/p/api/setup.sh"));
        assert_eq!(scripts.len(), 1);
        assert_eq!(scripts[0].path, PathBuf::from("/p/api/setup.sh"));
        assert!(scripts[0].args.is_empty());
        assert!(scripts[0].enabled, "a newly added script must run");
    }

    #[test]
    fn remove_script_drops_the_row_at_index() {
        let mut scripts = vec![script("/a.sh", true), script("/b.sh", true)];
        remove_script(&mut scripts, 0);
        assert_eq!(scripts, vec![script("/b.sh", true)]);
    }

    #[test]
    fn remove_script_out_of_range_is_a_no_op() {
        let mut scripts = vec![script("/a.sh", true)];
        remove_script(&mut scripts, 5);
        assert_eq!(scripts.len(), 1);
    }

    // --- criterion 2: disable is not delete ---

    #[test]
    fn toggling_enabled_flips_the_flag_without_removing_the_script() {
        let mut scripts = vec![script("/a.sh", true)];

        toggle_enabled(&mut scripts, 0);

        assert_eq!(
            scripts.len(),
            1,
            "a disabled script must still be in the list, not removed"
        );
        assert!(!scripts[0].enabled);
        assert_eq!(
            scripts[0].path,
            PathBuf::from("/a.sh"),
            "the same script must still be there, not replaced"
        );
    }

    #[test]
    fn toggling_enabled_twice_returns_to_the_original_state() {
        let mut scripts = vec![script("/a.sh", true)];
        toggle_enabled(&mut scripts, 0);
        toggle_enabled(&mut scripts, 0);
        assert!(scripts[0].enabled);
        assert_eq!(scripts.len(), 1);
    }

    #[test]
    fn toggling_enabled_out_of_range_is_a_no_op() {
        let mut scripts = vec![script("/a.sh", true)];
        toggle_enabled(&mut scripts, 5);
        assert_eq!(scripts.len(), 1);
        assert!(scripts[0].enabled);
    }

    // --- criterion 1: reordering within a scope ---

    #[test]
    fn move_up_swaps_with_the_previous_row() {
        let mut scripts = vec![script("/a.sh", true), script("/b.sh", true)];
        move_up(&mut scripts, 1);
        assert_eq!(
            scripts,
            vec![script("/b.sh", true), script("/a.sh", true)],
            "got {scripts:?}"
        );
    }

    #[test]
    fn move_up_at_the_first_row_is_a_no_op() {
        let mut scripts = vec![script("/a.sh", true), script("/b.sh", true)];
        move_up(&mut scripts, 0);
        assert_eq!(scripts, vec![script("/a.sh", true), script("/b.sh", true)]);
    }

    #[test]
    fn move_down_swaps_with_the_next_row() {
        let mut scripts = vec![script("/a.sh", true), script("/b.sh", true)];
        move_down(&mut scripts, 0);
        assert_eq!(
            scripts,
            vec![script("/b.sh", true), script("/a.sh", true)],
            "got {scripts:?}"
        );
    }

    #[test]
    fn move_down_at_the_last_row_is_a_no_op() {
        let mut scripts = vec![script("/a.sh", true), script("/b.sh", true)];
        move_down(&mut scripts, 1);
        assert_eq!(scripts, vec![script("/a.sh", true), script("/b.sh", true)]);
    }

    #[test]
    fn move_up_out_of_range_is_a_no_op() {
        let mut scripts = vec![script("/a.sh", true)];
        move_up(&mut scripts, 5);
        assert_eq!(scripts.len(), 1);
    }

    #[test]
    fn move_down_out_of_range_is_a_no_op() {
        let mut scripts = vec![script("/a.sh", true)];
        move_down(&mut scripts, 5);
        assert_eq!(scripts.len(), 1);
    }

    // --- criterion 1: display order matches execution order, across scopes ---

    #[test]
    fn execution_order_is_every_global_script_then_every_project_script() {
        // The concatenation `envspec::compose` performs
        // (`global_scripts.iter().chain(&project.scripts)`), proven directly
        // against this function rather than against a re-derivation of it.
        let global = vec![script("/g1.sh", true), script("/g2.sh", true)];
        let project = vec![script("/p1.sh", true), script("/p2.sh", true)];

        let order = execution_order(&global, &project);

        let paths: Vec<_> = order.iter().map(|s| s.path.to_str().unwrap()).collect();
        assert_eq!(
            paths,
            vec!["/g1.sh", "/g2.sh", "/p1.sh", "/p2.sh"],
            "every global script must run before any project script"
        );
    }

    #[test]
    fn execution_order_preserves_each_scopes_own_order() {
        // Catches an implementation that sorts (e.g. alphabetically) instead
        // of preserving stored order within a scope.
        let global = vec![script("/z-global.sh", true), script("/a-global.sh", true)];
        let project: Vec<Script> = vec![];

        let order = execution_order(&global, &project);

        let paths: Vec<_> = order.iter().map(|s| s.path.to_str().unwrap()).collect();
        assert_eq!(paths, vec!["/z-global.sh", "/a-global.sh"]);
    }

    #[test]
    fn a_disabled_script_still_appears_in_execution_order() {
        // Criterion 2 and criterion 1 intersect here: `execution_order` is a
        // display-order function, and `envspec::script::apply` -- not this
        // module -- is what skips running a disabled script. Filtering it out
        // here would make the tab lie about what is in the list.
        let global = vec![script("/enabled.sh", true), script("/disabled.sh", false)];

        let order = execution_order(&global, &[]);

        assert_eq!(order.len(), 2, "got {order:?}");
        assert!(!order[1].enabled);
    }

    // --- criterion 4: the tab's own statement of order ---

    #[test]
    fn the_order_note_says_global_scripts_run_first() {
        assert!(ORDER_NOTE.to_lowercase().contains("global"));
        assert!(ORDER_NOTE.to_lowercase().contains("first"));
        assert!(ORDER_NOTE.to_lowercase().contains("project"));
    }
}
