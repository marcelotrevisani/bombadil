//! The Env vars tab: global variables editable independently of any project,
//! and (once a project is selected) that project's own variables shown
//! against the global ones they override.
//!
//! Spec §5: global variables apply to everything Bombadil launches; project
//! variables override them. A variable marked secret has its value in the OS
//! keychain and never in `config.toml` -- `EnvVar::secret` documents the same
//! rule; this is the editor that has to uphold it while the user is actively
//! typing.
//!
//! # How a secret's value reaches the keychain without ever entering a
//! `Debug`-deriving `Message`
//!
//! The value textbox for a row is only ever shown -- and only ever produces
//! `Message::EnvVarValueChanged` -- while that row is *not* marked secret
//! (see `rows` below, which renders a fixed placeholder instead of a textbox
//! once `var.secret` is true). So every value that message ever carries is,
//! by construction, not a secret: it is ordinary plaintext the user is still
//! deciding whether to protect, the same as typing `HTTP_PROXY`.
//!
//! Checking "secret" does not carry the typed text in its own message at
//! all: `Message::EnvVarSecretToggled` names only the row (`scope`, `index`).
//! `app::update` reads the plaintext straight out of `app.config` -- where
//! the user's typing already landed via `EnvVarValueChanged` -- through
//! [`mark_secret`], which returns it by ordinary function return, and moves
//! it into a `job::run` closure by ordinary Rust move semantics: the same
//! seam every other keychain call in this crate uses (`sync_project`,
//! `open_terminal`, ...). The value never touches a `Message` on that path.
//!
//! Unmarking secret does not attempt to read the real value back out of the
//! keychain to redisplay it in the now-editable textbox -- that would be
//! exactly the leak criterion 4 exists to catch. [`unmark_secret`] leaves the
//! `Config` value empty; the user retypes if they want a new one.
//!
//! What a row displays is centralised in [`display_value`], which every
//! rendering path here goes through (a row's own value, and a project row's
//! overridden-global value) -- a secret is redacted there regardless of what
//! happens to be sitting in `var.value`, rather than relying on every call
//! site remembering to check `secret` itself.

use crate::app::Message;
use bombadil_core::model::{Config, EnvVar};
use bombadil_core::secrets::SecretKey;
use uuid::Uuid;

/// The text shown in place of a secret's real value -- never rendered,
/// anywhere, per criterion 4.
const REDACTED: &str = "(secret -- stored in the keychain)";

/// Which list of variables a row edits: `Config::global_env`, or one
/// project's `Project::env` addressed by its position in `App::projects`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvScope {
    Global,
    Project(usize),
}

/// The `&mut Vec<EnvVar>` a scope addresses. `None` only for a `Project`
/// scope whose index no longer exists -- the same stale-index tolerance
/// `app::update` already extends to every other indexed message.
pub fn vars_mut(config: &mut Config, scope: EnvScope) -> Option<&mut Vec<EnvVar>> {
    match scope {
        EnvScope::Global => Some(&mut config.global_env),
        EnvScope::Project(i) => config.projects.get_mut(i).map(|p| &mut p.env),
    }
}

/// The keychain account a scope's variable named `key` lives under -- spec
/// §5's two account shapes, `SecretKey::GlobalEnv` and `SecretKey::ProjectEnv`.
/// `project_id` is the project's own id, not its index, so it must be
/// resolved by the caller (`app::update`, which alone has both `App::config`
/// and `App::projects` at hand); `None` for a `Global` scope is fine, since
/// that branch never reads it.
pub fn secret_key(scope: EnvScope, project_id: Option<Uuid>, key: &str) -> SecretKey {
    match scope {
        EnvScope::Global => SecretKey::GlobalEnv {
            key: key.to_string(),
        },
        EnvScope::Project(_) => SecretKey::ProjectEnv {
            project_id: project_id.expect("a project scope always carries a project id"),
            key: key.to_string(),
        },
    }
}

/// What to show for a variable's value. A secret's real text is never
/// returned here, regardless of what happens to be sitting in `var.value` --
/// the one place criterion 4 is enforced, so every renderer that goes through
/// it inherits the guarantee rather than having to reimplement the check.
pub fn display_value(var: &EnvVar) -> String {
    if var.secret {
        REDACTED.to_string()
    } else {
        var.value.clone()
    }
}

/// Appends a blank, non-secret row -- the starting state for every variable:
/// typed in plaintext until the user marks it secret.
pub fn add_var(vars: &mut Vec<EnvVar>) {
    vars.push(EnvVar {
        key: String::new(),
        value: String::new(),
        secret: false,
    });
}

pub fn remove_var(vars: &mut Vec<EnvVar>, index: usize) {
    if index < vars.len() {
        vars.remove(index);
    }
}

pub fn set_key(vars: &mut [EnvVar], index: usize, key: String) {
    if let Some(var) = vars.get_mut(index) {
        var.key = key;
    }
}

/// Only ever reached for a row that is not secret: `rows` renders no value
/// textbox at all once `var.secret` is true, so `Message::EnvVarValueChanged`
/// can never be produced for a secret row and this can never be called to
/// overwrite one. See the module doc.
pub fn set_value(vars: &mut [EnvVar], index: usize, value: String) {
    if let Some(var) = vars.get_mut(index)
        && !var.secret
    {
        var.value = value;
    }
}

/// The in-memory half of marking `index` secret (criterion 2): blanks the
/// `Config`-facing value and flips the flag, returning the plaintext that was
/// there so the caller can move it -- by ordinary return, never through a
/// `Message` -- into the `job::run` closure that writes it to the keychain.
///
/// `None` for an out-of-range index or a row already secret -- there is
/// nothing to mark, and nothing to write.
pub fn mark_secret(vars: &mut [EnvVar], index: usize) -> Option<String> {
    let var = vars.get_mut(index)?;
    if var.secret {
        return None;
    }
    var.secret = true;
    Some(std::mem::take(&mut var.value))
}

/// The in-memory half of unmarking `index` secret (criterion 3): just the
/// flag. The value stays empty -- criterion 4 forbids reading the real value
/// back out of the keychain to redisplay it, so there is nothing to restore
/// it to.
///
/// Returns whether there was a secret row here to unmark, so the caller
/// knows whether a keychain delete is owed.
pub fn unmark_secret(vars: &mut [EnvVar], index: usize) -> bool {
    match vars.get_mut(index) {
        Some(var) if var.secret => {
            var.secret = false;
            true
        }
        _ => false,
    }
}

/// One project variable that overrides a global variable of the same key
/// (criterion 1): carries both values so the tab can show why the global has
/// no effect here, not just which one won -- the opposite of what
/// `envspec::compose` does, which only needs the winner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Override {
    pub key: String,
    /// Via [`display_value`]: `"(secret)"`-shaped text rather than the real
    /// text when the global itself is marked secret, so an override row can
    /// never be the leak criterion 4 exists to catch either.
    pub global_display: String,
}

/// Every project variable that overrides a global one.
pub fn overrides(global: &[EnvVar], project_env: &[EnvVar]) -> Vec<Override> {
    project_env
        .iter()
        .filter_map(|pvar| {
            global.iter().find(|g| g.key == pvar.key).map(|g| Override {
                key: pvar.key.clone(),
                global_display: display_value(g),
            })
        })
        .collect()
}

/// Renders one scope's rows. Deliberately dumb, like every other `view` in
/// this crate: all the logic worth testing lives in the functions above.
fn rows<'a>(scope: EnvScope, vars: &[EnvVar]) -> iced::Element<'a, Message> {
    let mut column = iced::widget::column![].spacing(4);
    for (index, var) in vars.iter().enumerate() {
        let key_input = iced::widget::text_input("KEY", &var.key)
            .on_input(move |key| Message::EnvVarKeyChanged(scope, index, key));

        // The one place criterion 4 is enforced at the widget level: a
        // secret row gets no textbox bound to `var.value` at all, so there
        // is nothing for the widget itself to leak even if `display_value`
        // were somehow bypassed elsewhere.
        let value_widget: iced::Element<'_, Message> = if var.secret {
            iced::widget::text(display_value(var)).into()
        } else {
            iced::widget::text_input("value", &var.value)
                .on_input(move |value| Message::EnvVarValueChanged(scope, index, value))
                .into()
        };

        let secret_toggle = iced::widget::checkbox(var.secret)
            .label("secret")
            .on_toggle(move |_| Message::EnvVarSecretToggled(scope, index));

        let remove = iced::widget::button(iced::widget::text("remove"))
            .on_press(Message::EnvVarRemoved(scope, index));

        column = column
            .push(iced::widget::row![key_input, value_widget, secret_toggle, remove].spacing(6));
    }
    column.into()
}

/// Renders the whole tab: global variables, then (if a project is selected)
/// that project's own variables plus the overrides among them (criterion 1).
pub fn view<'a>(config: &Config, selected_project: Option<usize>) -> iced::Element<'a, Message> {
    let mut column = iced::widget::column![
        iced::widget::text("Global variables").size(16),
        rows(EnvScope::Global, &config.global_env),
        iced::widget::button(iced::widget::text("Add global variable"))
            .on_press(Message::EnvVarAdded(EnvScope::Global)),
    ]
    .spacing(8);

    if let Some(i) = selected_project
        && let Some(project) = config.projects.get(i)
    {
        column = column.push(iced::widget::text("Project variables").size(16));
        column = column.push(rows(EnvScope::Project(i), &project.env));
        column = column.push(
            iced::widget::button(iced::widget::text("Add project variable"))
                .on_press(Message::EnvVarAdded(EnvScope::Project(i))),
        );

        for over in overrides(&config.global_env, &project.env) {
            column = column.push(iced::widget::text(format!(
                "{} overrides the global value ({})",
                over.key, over.global_display
            )));
        }
    }

    column.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bombadil_core::model::Project;
    use std::path::PathBuf;

    fn var(key: &str, value: &str, secret: bool) -> EnvVar {
        EnvVar {
            key: key.to_string(),
            value: value.to_string(),
            secret,
        }
    }

    fn project_with_env(env: Vec<EnvVar>) -> Project {
        Project {
            id: Uuid::from_u128(1),
            label: "api".into(),
            pyproject_path: PathBuf::from("/p/api/pyproject.toml"),
            index_names: vec![],
            venv: bombadil_core::model::VenvLocation::Alongside,
            env,
            scripts: vec![],
            members: Default::default(),
        }
    }

    // --- criterion 1: a project variable overriding a global one is shown
    // as overriding, with both values visible ---

    #[test]
    fn a_project_variable_overriding_a_global_one_is_reported_with_the_globals_value() {
        let global = vec![var("SHARED", "global-value", false)];
        let project_env = vec![var("SHARED", "project-value", false)];

        let got = overrides(&global, &project_env);

        assert_eq!(got.len(), 1, "got {got:?}");
        assert_eq!(got[0].key, "SHARED");
        assert_eq!(
            got[0].global_display, "global-value",
            "the global's own value must still be visible, not just that it lost"
        );
    }

    #[test]
    fn a_project_variable_with_no_matching_global_is_not_reported_as_overriding() {
        let global = vec![var("OTHER", "g", false)];
        let project_env = vec![var("SHARED", "p", false)];

        assert!(overrides(&global, &project_env).is_empty());
    }

    #[test]
    fn an_overridden_secret_global_never_shows_its_value() {
        // The two criteria intersect here: an override row must still never
        // leak a secret global's value (criterion 4), even while proving it
        // exists and is overridden (criterion 1).
        let global = vec![var("TOKEN", "leaked-if-shown", true)];
        let project_env = vec![var("TOKEN", "project-value", false)];

        let got = overrides(&global, &project_env);

        assert_eq!(got.len(), 1);
        assert_ne!(got[0].global_display, "leaked-if-shown");
        assert!(!got[0].global_display.contains("leaked-if-shown"));
    }

    // --- criterion 2: marking secret writes to the store and blanks Config
    // (the in-memory half; the store side is proven in app.rs against
    // `run_secret_write`, which needs a `ConfigPersist` this module doesn't
    // have) ---

    #[test]
    fn marking_a_variable_secret_blanks_its_config_value_and_returns_the_plaintext() {
        let mut vars = vec![var("TOKEN", "hunter2", false)];

        let taken = mark_secret(&mut vars, 0);

        assert_eq!(taken.as_deref(), Some("hunter2"));
        assert_eq!(vars[0].value, "", "got {:?}", vars[0].value);
        assert!(vars[0].secret);
    }

    #[test]
    fn marking_an_already_secret_variable_is_a_no_op() {
        let mut vars = vec![var("TOKEN", "", true)];
        assert_eq!(mark_secret(&mut vars, 0), None);
    }

    // --- criterion 3: unmarking secret deletes the keychain entry (the
    // in-memory half) ---

    #[test]
    fn unmarking_a_secret_variable_flips_the_flag_and_reports_a_delete_is_owed() {
        let mut vars = vec![var("TOKEN", "", true)];

        let owed = unmark_secret(&mut vars, 0);

        assert!(owed, "a delete must be reported as owed");
        assert!(!vars[0].secret);
    }

    #[test]
    fn unmarking_a_non_secret_variable_reports_nothing_owed() {
        let mut vars = vec![var("TOKEN", "plain", false)];
        assert!(!unmark_secret(&mut vars, 0));
    }

    #[test]
    fn unmarking_never_restores_the_real_value() {
        // Criterion 4's other angle: even the in-memory half must not read
        // the keychain back to repopulate the textbox -- there is nothing in
        // `unmark_secret`'s signature that could, since it never touches a
        // `SecretStore`, but the value staying empty is what proves nothing
        // was smuggled in some other way.
        let mut vars = vec![var("TOKEN", "", true)];
        unmark_secret(&mut vars, 0);
        assert_eq!(vars[0].value, "");
    }

    // --- criterion 4: a secret's value is never rendered ---

    #[test]
    fn display_value_never_returns_a_secrets_real_text() {
        let secret = var("TOKEN", "should-never-render", true);
        let shown = display_value(&secret);
        assert_ne!(shown, "should-never-render");
        assert!(!shown.contains("should-never-render"), "got {shown}");
    }

    #[test]
    fn display_value_shows_a_non_secrets_real_text() {
        // The opposite mistake: redacting everything would make the editor
        // useless for ordinary variables.
        let plain = var("HTTP_PROXY", "http://proxy:8080", false);
        assert_eq!(display_value(&plain), "http://proxy:8080");
    }

    // --- vars_mut / secret_key scope wiring ---

    #[test]
    fn global_scope_addresses_the_global_list() {
        let mut config = Config {
            global_env: vec![var("A", "1", false)],
            ..Config::default()
        };
        let vars = vars_mut(&mut config, EnvScope::Global).expect("global always exists");
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].key, "A");
    }

    #[test]
    fn project_scope_addresses_that_projects_own_list_not_global() {
        let mut config = Config {
            global_env: vec![var("GLOBAL", "g", false)],
            projects: vec![project_with_env(vec![var("PROJECT", "p", false)])],
            ..Config::default()
        };
        let vars = vars_mut(&mut config, EnvScope::Project(0)).expect("project 0 exists");
        assert_eq!(vars.len(), 1);
        assert_eq!(
            vars[0].key, "PROJECT",
            "must address the project's own env, not global_env"
        );
    }

    #[test]
    fn a_stale_project_index_addresses_nothing() {
        let mut config = Config::default();
        assert!(vars_mut(&mut config, EnvScope::Project(0)).is_none());
    }

    #[test]
    fn the_global_secret_key_names_no_project() {
        assert_eq!(
            secret_key(EnvScope::Global, None, "TOKEN"),
            SecretKey::GlobalEnv {
                key: "TOKEN".into()
            }
        );
    }

    #[test]
    fn the_project_secret_key_carries_the_projects_id() {
        let id = Uuid::from_u128(42);
        assert_eq!(
            secret_key(EnvScope::Project(0), Some(id), "TOKEN"),
            SecretKey::ProjectEnv {
                project_id: id,
                key: "TOKEN".into(),
            }
        );
    }

    // --- add / remove / set_key / set_value ---

    #[test]
    fn add_var_appends_a_blank_non_secret_row() {
        let mut vars = Vec::new();
        add_var(&mut vars);
        assert_eq!(vars, vec![var("", "", false)]);
    }

    #[test]
    fn remove_var_drops_the_row_at_index() {
        let mut vars = vec![var("A", "1", false), var("B", "2", false)];
        remove_var(&mut vars, 0);
        assert_eq!(vars, vec![var("B", "2", false)]);
    }

    #[test]
    fn remove_var_out_of_range_is_a_no_op() {
        let mut vars = vec![var("A", "1", false)];
        remove_var(&mut vars, 5);
        assert_eq!(vars.len(), 1);
    }

    #[test]
    fn set_key_renames_the_row() {
        let mut vars = vec![var("OLD", "1", false)];
        set_key(&mut vars, 0, "NEW".to_string());
        assert_eq!(vars[0].key, "NEW");
    }

    #[test]
    fn set_value_updates_a_non_secret_row() {
        let mut vars = vec![var("A", "old", false)];
        set_value(&mut vars, 0, "new".to_string());
        assert_eq!(vars[0].value, "new");
    }

    #[test]
    fn set_value_never_writes_into_a_secret_row() {
        // Belt and suspenders alongside `rows` never producing
        // `EnvVarValueChanged` for a secret row in the first place: even a
        // call that should not be reachable must not overwrite one.
        let mut vars = vec![var("A", "", true)];
        set_value(&mut vars, 0, "smuggled".to_string());
        assert_eq!(vars[0].value, "", "got {:?}", vars[0].value);
    }
}
