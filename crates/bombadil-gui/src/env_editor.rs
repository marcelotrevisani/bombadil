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
//! `Debug`-deriving `Message` -- or `config.toml`
//!
//! The value textbox for a row is only ever shown -- and only ever produces
//! `Message::EnvVarValueChanged` -- while that row is *not* marked secret
//! (see `rows` below, which renders a fixed placeholder instead of a textbox
//! once `var.secret` is true). So every value that message ever carries is,
//! by construction, not a secret *yet*: it is ordinary plaintext the user is
//! still deciding whether to protect, the same as typing `HTTP_PROXY`.
//!
//! "Not a secret yet" is not the same as "safe to write to disk", which is
//! what this editor used to do. `EnvVarValueChanged` wrote straight into
//! `Config` and dispatched a persist, so a PyPI token typed into that box
//! reached `config.toml` once per character, and only the later
//! `EnvVarSecretToggled` blanked it -- by which time the store's atomic
//! rename had already released every intermediate copy to free blocks.
//!
//! So the in-progress value lives in [`ValueDrafts`] -- `App`-level scratch
//! state that no `ConfigPersist` can see -- exactly as `index_editor`'s
//! credential does, and reaches `Config` only when the user commits the row
//! (`Message::EnvVarValueCommitted`, from Enter or the row's save button).
//! Ticking "secret" first is what the workflow is *for*, and on that path the
//! plaintext goes from the draft to the keychain having never been in
//! `Config` at all.
//!
//! Checking "secret" does not carry the typed text in its own message at
//! all: `Message::EnvVarSecretToggled` names only the row (`scope`, `index`).
//! `app::update` takes the plaintext from this row's draft, or -- for a value
//! the user had already committed -- straight out of `app.config` through
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
//! The *key* box is read-only for a secret row for a different reason. A
//! secret's keychain account is named after the variable (`SecretKey::GlobalEnv`,
//! `SecretKey::ProjectEnv`), and nothing in this module has a `SecretStore` to
//! move an entry with -- so a rename would leave `envspec::compose` looking
//! under a name nothing was ever stored under. An unresolvable secret is
//! dropped, so the variable would simply vanish from the environment, with no
//! message and no banner. Unmark secret, rename, mark secret again is the path
//! that works. See [`set_key`].
//!
//! What a row displays is centralised in [`display_value`], which every
//! rendering path here goes through (a row's own value, and a project row's
//! overridden-global value) -- a secret is redacted there regardless of what
//! happens to be sitting in `var.value`, rather than relying on every call
//! site remembering to check `secret` itself.

use crate::app::Message;
use crate::icons::{self, Icon};
use crate::theme;
use bombadil_core::model::{Config, EnvVar};
use bombadil_core::secrets::SecretKey;
use std::collections::HashMap;
use uuid::Uuid;

/// How many `slate` dots a secret's value renders as -- never the value,
/// and never a placeholder that could be mistaken for one, per criterion 4.
///
/// Fixed, regardless of the real value's length (which is empty in `Config`
/// anyway by the time a row is secret -- see `mark_secret`) or emptiness, so
/// an empty secret and any other secret render identically. A run of dots
/// whose length tracked the real value would leak exactly the shape of
/// information a secret exists to hide; text like `"(secret)"` is still a
/// value-shaped placeholder someone glancing at the screen could mistake for
/// what was actually typed a moment before.
const SECRET_DOT_COUNT: usize = 8;

/// The dots a secret's value renders as. Callers colour it `theme::text_muted` --
/// this returns only the text, since some call sites (the value textbox's
/// stand-in) render it alone and others (an override row's sentence) render
/// it inline with prose that must not share its colour.
fn secret_dots() -> String {
    "\u{2022}".repeat(SECRET_DOT_COUNT)
}

/// Which list of variables a row edits: `Config::global_env`, or one
/// project's `Project::env` addressed by its position in
/// `App::config.projects`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnvScope {
    Global,
    Project(usize),
}

/// What the user has typed into each value textbox but not yet committed --
/// keyed by the row it belongs to, and held by `App`, never by `Config`. See
/// the module doc for why it cannot live in `EnvVar::value` while it is being
/// typed.
///
/// An absent entry means "this row has nothing uncommitted", which is the
/// state every row is in when the tab opens and the state a row returns to the
/// moment it is committed ([`commit_value`]) or moved to the keychain
/// (`app::update`'s `EnvVarSecretToggled` arm takes the draft out).
pub type ValueDrafts = HashMap<(EnvScope, usize), String>;

/// The `&mut Vec<EnvVar>` a scope addresses. `None` only for a `Project`
/// scope whose index no longer exists -- the same stale-index tolerance
/// `app::update` already extends to every other indexed message.
pub fn vars_mut(config: &mut Config, scope: EnvScope) -> Option<&mut Vec<EnvVar>> {
    match scope {
        EnvScope::Global => Some(&mut config.global_env),
        EnvScope::Project(i) => config.projects.get_mut(i).map(|p| &mut p.env),
    }
}

/// [`vars_mut`]'s read-only half, for a caller that needs to look a row up
/// without announcing an edit it is not making -- `app::update` reads a row's
/// key to build its `SecretKey` before deciding whether anything changes at
/// all.
pub fn vars(config: &Config, scope: EnvScope) -> Option<&Vec<EnvVar>> {
    match scope {
        EnvScope::Global => Some(&config.global_env),
        EnvScope::Project(i) => config.projects.get(i).map(|p| &p.env),
    }
}

/// The keychain account a scope's variable named `key` lives under -- spec
/// §5's two account shapes, `SecretKey::GlobalEnv` and `SecretKey::ProjectEnv`.
/// `project_id` is the project's own id, not its index, so it must be
/// resolved by the caller (`app::update`, which reads it from
/// `App::config.projects`); `None` for a `Global` scope is fine, since that
/// branch never reads it.
/// `None` for a project scope without its id: nothing to address.
pub fn secret_key(scope: EnvScope, project_id: Option<Uuid>, key: &str) -> Option<SecretKey> {
    match scope {
        EnvScope::Global => Some(SecretKey::GlobalEnv {
            key: key.to_string(),
        }),
        EnvScope::Project(_) => Some(SecretKey::ProjectEnv {
            project_id: project_id?,
            key: key.to_string(),
        }),
    }
}

/// What to show for a variable's value. A secret's real text is never
/// returned here, regardless of what happens to be sitting in `var.value` --
/// the one place criterion 4 is enforced, so every renderer that goes through
/// it inherits the guarantee rather than having to reimplement the check.
pub fn display_value(var: &EnvVar) -> String {
    if var.secret {
        secret_dots()
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

/// Only ever reached for a row that is not secret: `rows` renders the key box
/// read-only once `var.secret` is true, so `Message::EnvVarKeyChanged` can
/// never be produced for a secret row.
///
/// The guard is repeated here for the same reason `set_value`'s is. A secret's
/// keychain account is named after the variable (`SecretKey::GlobalEnv`,
/// `SecretKey::ProjectEnv`), and this function has no `SecretStore` to move
/// the entry with -- so a rename that got through would leave
/// `envspec::compose` looking under a name nothing was stored under, and an
/// unresolvable secret is dropped: the variable would vanish from the
/// environment entirely, with no message and no banner. Unmark secret, rename,
/// mark secret again is the path that works.
pub fn set_key(vars: &mut [EnvVar], index: usize, key: String) {
    if let Some(var) = vars.get_mut(index)
        && !var.secret
    {
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

/// What a row's value textbox shows: the uncommitted draft when the user has
/// typed one, otherwise what `Config` currently holds. Never called for a
/// secret row -- `rows` renders no textbox at all for one -- so this cannot
/// become a path back to a secret's real text; [`display_value`] stays the
/// only thing a secret row renders through.
pub fn value_box_text<'a>(
    drafts: &'a ValueDrafts,
    scope: EnvScope,
    index: usize,
    var: &'a EnvVar,
) -> &'a str {
    drafts
        .get(&(scope, index))
        .map(String::as_str)
        .unwrap_or(&var.value)
}

/// Whether this row has typing that has not reached `Config` yet -- what the
/// row's save button is enabled by, so an uncommitted value is visible as
/// uncommitted rather than silently waiting to be lost.
pub fn is_uncommitted(drafts: &ValueDrafts, scope: EnvScope, index: usize, var: &EnvVar) -> bool {
    matches!(drafts.get(&(scope, index)), Some(draft) if *draft != var.value)
}

/// The rows of `scope` with a value typed and not saved.
pub fn unsaved(drafts: &ValueDrafts, scope: EnvScope, vars: &[EnvVar]) -> Vec<usize> {
    vars.iter()
        .enumerate()
        .filter(|(i, var)| !var.secret && is_uncommitted(drafts, scope, *i, var))
        .map(|(i, _)| i)
        .collect()
}

/// Every unsaved value, in any scope that still exists.
pub fn unsaved_count(config: &Config, drafts: &ValueDrafts) -> usize {
    drafts
        .keys()
        .filter(|(scope, index)| {
            vars(config, *scope)
                .and_then(|vars| vars.get(*index))
                .is_some_and(|var| !var.secret && is_uncommitted(drafts, *scope, *index, var))
        })
        .count()
}

/// Moves this row's draft into `Config` (plaintext, deliberately: the user
/// committed a value they did not mark secret). Returns whether anything was
/// committed, so `app::update` can skip the persist when there was nothing to
/// save.
///
/// The draft is taken either way -- a draft for a secret row cannot be
/// committed (see [`set_value`]) and must not be left sitting in `App` state
/// afterwards either.
pub fn commit_value(
    vars: &mut [EnvVar],
    drafts: &mut ValueDrafts,
    scope: EnvScope,
    index: usize,
) -> bool {
    let Some(value) = drafts.remove(&(scope, index)) else {
        return false;
    };
    let committed = matches!(vars.get(index), Some(var) if !var.secret);
    set_value(vars, index, value);
    committed
}

/// Keeps `drafts` addressing the same rows after `removed` is deleted from
/// `scope`: that row's own draft goes with it, and every later row in the same
/// scope shifts down one.
///
/// Without this a removal silently moves every later draft onto its
/// neighbour's row -- the value the user is part-way through typing would
/// commit into a different variable.
pub fn drop_draft_row(drafts: &mut ValueDrafts, scope: EnvScope, removed: usize) {
    let mut shifted = ValueDrafts::new();
    drafts.retain(|(s, i), value| {
        if *s != scope || *i < removed {
            return true;
        }
        if *i > removed {
            shifted.insert((*s, *i - 1), std::mem::take(value));
        }
        false
    });
    drafts.extend(shifted);
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

/// Everything ticking "secret" has to decide in memory: which plaintext the
/// keychain job must write, with `Config` left blank either way.
///
/// The row's uncommitted draft wins over what `Config` holds -- it is what the
/// textbox was showing, and on the workflow this tab is built around (type the
/// value, then tick secret) it is the *only* copy, since typing never reaches
/// `Config`. An empty draft falls back to a value the user committed earlier;
/// a row with neither yields an empty string, which is what marking an empty
/// row secret has always stored.
///
/// `None` for an out-of-range index or a row already secret -- [`mark_secret`]'s
/// own two nothing-to-do cases. The draft is dropped regardless: a row that
/// just became secret must not leave the plaintext sitting in `App` state.
///
/// A function here rather than inlined in `app::update` for the reason
/// `index_editor::credential_save` gives: the exact plaintext a real press
/// sends to the keychain is then directly testable, without driving an opaque
/// `Task`.
pub fn take_secret_value(
    vars: &mut [EnvVar],
    drafts: &mut ValueDrafts,
    scope: EnvScope,
    index: usize,
) -> Option<String> {
    let drafted = drafts.remove(&(scope, index));
    let committed = mark_secret(vars, index)?;
    Some(
        drafted
            .filter(|value| !value.is_empty())
            .unwrap_or(committed),
    )
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
    /// Via [`display_value`]: a fixed run of dots (see [`SECRET_DOT_COUNT`])
    /// rather than the real text when the global itself is marked secret, so
    /// an override row can never be the leak criterion 4 exists to catch
    /// either.
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
///
/// Key and value are data -- compared character by character, per the plan's
/// own rule -- so both render in Plex; the buttons and the "secret" checkbox
/// label are prose, left in the default face. Each row sits on a `bark`
/// surface, the same treatment every row in every pane gets, so the tab reads
/// as the same kind of list as Dependencies/Members/Indexes even though it
/// carries no state glyph of its own -- an environment variable has no
/// present/absent/drifted state to report.
fn rows<'a>(scope: EnvScope, vars: &[EnvVar], drafts: &ValueDrafts) -> iced::Element<'a, Message> {
    let mut column = iced::widget::column![].spacing(theme::SPACE_2);
    for (index, var) in vars.iter().enumerate() {
        // Read-only once the row is secret: the keychain account is named
        // after the key, and nothing here can move an entry -- see `set_key`.
        let key_input = iced::widget::text_input("KEY", &var.key)
            .style(crate::theme::text_field)
            .font(theme::FONT_DATA)
            .size(theme::DATA)
            .on_input_maybe(
                (!var.secret).then_some(move |key| Message::EnvVarKeyChanged(scope, index, key)),
            );

        // The one place criterion 4 is enforced at the widget level: a
        // secret row gets no textbox bound to `var.value` at all, so there
        // is nothing for the widget itself to leak even if `display_value`
        // were somehow bypassed elsewhere. The dots render in `slate`,
        // never the value's own colour.
        let value_widget: iced::Element<'_, Message> = if var.secret {
            iced::widget::text(display_value(var))
                .font(theme::FONT_DATA)
                .size(theme::DATA)
                .style(theme::text_muted)
                .into()
        } else {
            // Bound to the draft, never straight to `Config` -- see the module
            // doc. Enter commits, the same as the save button below.
            iced::widget::text_input("value", value_box_text(drafts, scope, index, var))
                .style(crate::theme::text_field)
                .font(theme::FONT_DATA)
                .size(theme::DATA)
                .on_input(move |value| Message::EnvVarValueChanged(scope, index, value))
                .on_submit(Message::EnvVarValueCommitted(scope, index))
                .into()
        };

        // Enabled only while this row has typing `Config` has not seen, the
        // same "no press needed, no press possible" treatment `index_editor`'s
        // auth buttons give a control whose action is already the state.
        let save_value = icons::icon_button(
            Icon::Check,
            "Save value",
            (!var.secret && is_uncommitted(drafts, scope, index, var))
                .then_some(Message::EnvVarValueCommitted(scope, index)),
        );

        let secret_toggle = iced::widget::checkbox(var.secret)
            .label("secret")
            .on_toggle(move |_| Message::EnvVarSecretToggled(scope, index));

        let remove = icons::icon_button(
            Icon::Trash,
            "Remove",
            Some(Message::EnvVarRemoved(scope, index)),
        );

        // Typed but not saved must not look saved.
        let unsaved_mark = (!var.secret && is_uncommitted(drafts, scope, index, var)).then(|| {
            iced::widget::text("unsaved")
                .size(theme::LABEL)
                .style(theme::text_warning)
        });
        let row = iced::widget::row![key_input, value_widget]
            .push(unsaved_mark)
            .push(save_value)
            .push(secret_toggle)
            .push(remove)
            .spacing(theme::SPACE_2)
            .align_y(iced::Alignment::Center);

        column = column.push(
            iced::widget::container(row)
                .padding(theme::SPACE_1)
                .style(theme::fill(theme::Swatch::Bark)),
        );
    }
    column.into()
}

/// Renders the whole tab: global variables, then (if a project is selected)
/// that project's own variables plus the overrides among them (criterion 1).
/// The global variables half: every project sees these unless it sets its own
/// with the same key.
///
/// Split from [`project_view`] because Preferences shows one scope at a time.
/// The two used to be stacked in one pane, which is exactly what the scope
/// switch replaces.
pub fn global_view<'a>(config: &Config, drafts: &ValueDrafts) -> iced::Element<'a, Message> {
    iced::widget::column![
        iced::widget::text("Every project sees these, unless it sets its own with the same key.")
            .size(theme::BODY)
            .style(theme::text_muted),
        unsaved_line(drafts, EnvScope::Global, &config.global_env),
        rows(EnvScope::Global, &config.global_env, drafts),
        iced::widget::button(iced::widget::text("Add global variable").size(theme::BODY))
            .on_press(Message::EnvVarAdded(EnvScope::Global))
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(theme::button_quiet),
    ]
    .spacing(theme::SPACE_3)
    .into()
}

/// "2 unsaved -- press Enter or ✓ on each", or nothing.
fn unsaved_line<'a>(
    drafts: &ValueDrafts,
    scope: EnvScope,
    vars: &[EnvVar],
) -> iced::Element<'a, Message> {
    match unsaved(drafts, scope, vars).len() {
        0 => iced::widget::Space::new().into(),
        n => iced::widget::text(format!("{n} unsaved: press Enter or \u{2713} on each"))
            .size(theme::LABEL)
            .style(theme::text_warning)
            .into(),
    }
}

/// One project's own variables, and which global ones they shadow.
///
/// The shadowing list is [`overrides`], which already exists and is already
/// tested; this is the view that used to hide it below a fold.
pub fn project_view<'a>(
    config: &Config,
    project_index: usize,
    drafts: &ValueDrafts,
) -> iced::Element<'a, Message> {
    let Some(project) = config.projects.get(project_index) else {
        return iced::widget::text("").into();
    };

    let mut column = iced::widget::column![
        iced::widget::text("These layer over the global variables. A key set here wins.")
            .size(theme::BODY)
            .style(theme::text_muted),
        unsaved_line(drafts, EnvScope::Project(project_index), &project.env),
        rows(EnvScope::Project(project_index), &project.env, drafts),
        iced::widget::button(iced::widget::text("Add project variable").size(theme::BODY))
            .on_press(Message::EnvVarAdded(EnvScope::Project(project_index)))
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(theme::button_quiet),
    ]
    .spacing(theme::SPACE_3);

    // Every global variable, shown here too. Environment variables *merge*:
    // a project's environment is the global set with its own layered over it,
    // so a project scope that showed only its own rows would be hiding most
    // of what its commands actually run with. That is what "the environment
    // variable set is not showing" meant -- splitting the old combined pane
    // by scope quietly dropped this half.
    //
    // Read-only, because editing a global belongs in the Global scope: two
    // editable copies of one variable is two places for it to disagree.
    let shadowed: Vec<String> = overrides(&config.global_env, &project.env)
        .into_iter()
        .map(|over| over.key)
        .collect();
    if !config.global_env.is_empty() {
        column = column.push(
            iced::widget::text("Inherited from global")
                .font(theme::FONT_PROSE_SEMIBOLD)
                .size(theme::LABEL)
                .style(theme::text_muted),
        );
        for var in &config.global_env {
            let overridden = shadowed.contains(&var.key);
            let mut line = iced::widget::row![
                iced::widget::text(var.key.clone())
                    .font(theme::FONT_DATA)
                    .size(theme::DATA)
                    .style(theme::text_muted),
                iced::widget::text(display_value(var))
                    .font(theme::FONT_DATA)
                    .size(theme::DATA)
                    .style(theme::text_muted),
            ]
            .spacing(theme::SPACE_2);
            if overridden {
                line = line.push(
                    iced::widget::text("(overridden above)")
                        .size(theme::LABEL)
                        .style(theme::text_muted),
                );
            }
            column = column.push(
                iced::widget::container(line)
                    .padding(theme::SPACE_1)
                    .style(theme::surface),
            );
        }
    }

    for over in overrides(&config.global_env, &project.env) {
        // Key and the global's own value are both data; the sentence around
        // them is prose. Kept as separate `text` widgets rather than one
        // formatted string so the value -- a secret global's dots included --
        // can carry its own colour without recolouring the words around it.
        column = column.push(
            iced::widget::row![
                iced::widget::text(over.key.clone())
                    .font(theme::FONT_DATA)
                    .size(theme::DATA)
                    .style(theme::text_muted),
                iced::widget::text("overrides the global value (")
                    .size(theme::BODY)
                    .style(theme::text_muted),
                iced::widget::text(over.global_display.clone())
                    .font(theme::FONT_DATA)
                    .size(theme::DATA)
                    .style(theme::text_muted),
                iced::widget::text(")")
                    .size(theme::BODY)
                    .style(theme::text_muted),
            ]
            .spacing(theme::SPACE_1),
        );
    }

    column.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bombadil_core::model::Project;
    use std::path::PathBuf;

    #[test]
    fn unsaved_values_are_counted_wherever_they_are() {
        let config = Config {
            global_env: vec![var("A", "1", false), var("B", "2", false)],
            ..Config::default()
        };
        let mut drafts = ValueDrafts::new();
        drafts.insert((EnvScope::Global, 0), "1".into()); // same as saved
        drafts.insert((EnvScope::Global, 1), "3".into()); // changed
        assert_eq!(unsaved(&drafts, EnvScope::Global, &config.global_env), [1]);
        assert_eq!(unsaved_count(&config, &drafts), 1);
        drafts.insert((EnvScope::Project(7), 0), "x".into()); // stale scope
        assert_eq!(
            unsaved_count(&config, &drafts),
            1,
            "a gone project counts nothing"
        );
    }

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
            environments: vec![bombadil_core::model::Environment {
                location: bombadil_core::model::VenvLocation::Alongside,
                python: bombadil_core::model::PythonPin::Unpinned,
            }],
            active: bombadil_core::model::VenvLocation::Alongside,
            env,
            ..Project::default()
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
    fn marking_secret_takes_the_typed_draft_the_user_never_committed() {
        // The whole point of the draft: on this tab's own workflow -- type the
        // value, then tick "secret" -- `Config` never held the plaintext at
        // all, so this is the only copy there is to give the keychain.
        let mut vars = vec![var("TOKEN", "", false)];
        let mut drafts = ValueDrafts::new();
        drafts.insert((EnvScope::Global, 0), "hunter2".to_string());

        let taken = take_secret_value(&mut vars, &mut drafts, EnvScope::Global, 0);

        assert_eq!(taken.as_deref(), Some("hunter2"));
        assert!(vars[0].secret);
        assert_eq!(vars[0].value, "");
        assert!(
            drafts.is_empty(),
            "the plaintext must not be left in App state either; got {drafts:?}"
        );
    }

    #[test]
    fn marking_secret_falls_back_to_a_value_the_user_had_already_committed() {
        let mut vars = vec![var("TOKEN", "committed-earlier", false)];
        let mut drafts = ValueDrafts::new();

        let taken = take_secret_value(&mut vars, &mut drafts, EnvScope::Global, 0);

        assert_eq!(taken.as_deref(), Some("committed-earlier"));
        assert_eq!(vars[0].value, "");
    }

    #[test]
    fn a_draft_beats_a_stale_committed_value_when_marking_secret() {
        // Retyping over a committed value and ticking secret must protect what
        // the box is showing, not what it used to show.
        let mut vars = vec![var("TOKEN", "old", false)];
        let mut drafts = ValueDrafts::new();
        drafts.insert((EnvScope::Global, 0), "new".to_string());

        assert_eq!(
            take_secret_value(&mut vars, &mut drafts, EnvScope::Global, 0).as_deref(),
            Some("new")
        );
    }

    #[test]
    fn marking_an_already_secret_row_takes_nothing_and_leaves_no_draft() {
        let mut vars = vec![var("TOKEN", "", true)];
        let mut drafts = ValueDrafts::new();
        drafts.insert((EnvScope::Global, 0), "stray".to_string());

        assert_eq!(
            take_secret_value(&mut vars, &mut drafts, EnvScope::Global, 0),
            None
        );
        assert!(drafts.is_empty());
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

    #[test]
    fn a_secret_renders_as_dots_not_a_word_shaped_placeholder() {
        // "Never a placeholder that could be mistaken for it" rules out text
        // like "(secret)" as much as it rules out the real value -- both are
        // value-shaped strings someone glancing at the screen could take for
        // what was typed. A run of a fixed punctuation character is neither.
        let secret = var("TOKEN", "hunter2", true);
        let shown = display_value(&secret);
        assert!(
            shown.chars().all(|c| c == '\u{2022}'),
            "a secret's display must be made of dots only; got {shown:?}"
        );
    }

    #[test]
    fn an_empty_secret_renders_identically_to_any_other_secret() {
        // The decision this glyph carries: dot count is fixed, not the real
        // value's length. If it tracked length, an empty secret would render
        // as zero dots -- indistinguishable from nothing rendered at all,
        // and a non-empty one would leak its length one dot at a time.
        let empty = var("TOKEN", "", true);
        let long = var("TOKEN", "a-much-longer-secret-value", true);
        assert_eq!(display_value(&empty), display_value(&long));
        assert!(!display_value(&empty).is_empty());
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
            Some(SecretKey::GlobalEnv {
                key: "TOKEN".into()
            })
        );
    }

    #[test]
    fn the_project_secret_key_carries_the_projects_id() {
        let id = Uuid::from_u128(42);
        assert_eq!(
            secret_key(EnvScope::Project(0), Some(id), "TOKEN"),
            Some(SecretKey::ProjectEnv {
                project_id: id,
                key: "TOKEN".into(),
            })
        );
    }

    #[test]
    fn a_project_scope_without_its_id_addresses_no_secret() {
        // Was an `expect`: a caller that lost the id took the window down.
        assert_eq!(secret_key(EnvScope::Project(0), None, "TOKEN"), None);
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
    fn set_key_never_renames_a_secret_row_out_from_under_its_keychain_entry() {
        // `SecretKey::{GlobalEnv,ProjectEnv}` are keyed on the variable's
        // name. Renaming the row without moving the keychain entry leaves
        // `envspec::compose` looking under a name nothing was ever stored
        // under -- and an unresolvable secret is dropped, so the variable
        // disappears from the environment entirely, with no message and no
        // banner. `rows` renders the key box read-only for a secret row for
        // the same reason; this is the belt to that's suspenders, exactly as
        // `set_value` already has.
        let mut vars = vec![var("TOKEN", "", true)];

        set_key(&mut vars, 0, "RENAMED".to_string());

        assert_eq!(
            vars[0].key, "TOKEN",
            "a secret row's key must not move while its keychain entry stays put"
        );
    }

    #[test]
    fn set_value_updates_a_non_secret_row() {
        let mut vars = vec![var("A", "old", false)];
        set_value(&mut vars, 0, "new".to_string());
        assert_eq!(vars[0].value, "new");
    }

    // --- the value draft: typed text that has not reached `Config` ---

    #[test]
    fn a_row_with_a_draft_shows_the_draft_and_one_without_shows_config() {
        let vars = [var("A", "committed", false)];
        let mut drafts = ValueDrafts::new();
        assert_eq!(
            value_box_text(&drafts, EnvScope::Global, 0, &vars[0]),
            "committed"
        );

        drafts.insert((EnvScope::Global, 0), "typing".to_string());
        assert_eq!(
            value_box_text(&drafts, EnvScope::Global, 0, &vars[0]),
            "typing",
            "the box must show what the user typed, not what Config still holds"
        );
    }

    #[test]
    fn a_draft_for_one_scope_never_shows_in_the_other() {
        // Global row 0 and project 0's row 0 are different variables that
        // share an index -- keying drafts on the index alone would cross them.
        let vars = [var("A", "project-value", false)];
        let mut drafts = ValueDrafts::new();
        drafts.insert((EnvScope::Global, 0), "global-typing".to_string());
        assert_eq!(
            value_box_text(&drafts, EnvScope::Project(0), 0, &vars[0]),
            "project-value"
        );
    }

    #[test]
    fn committing_a_draft_moves_it_into_config_and_clears_it() {
        let mut vars = vec![var("A", "old", false)];
        let mut drafts = ValueDrafts::new();
        drafts.insert((EnvScope::Global, 0), "new".to_string());

        let committed = commit_value(&mut vars, &mut drafts, EnvScope::Global, 0);

        assert!(committed, "a real commit must report that there is work");
        assert_eq!(vars[0].value, "new");
        assert!(
            drafts.is_empty(),
            "the draft must not linger once committed; got {drafts:?}"
        );
    }

    #[test]
    fn committing_a_row_with_no_draft_reports_nothing_to_do() {
        // What keeps a stray Enter from dispatching a persist per press.
        let mut vars = vec![var("A", "old", false)];
        let mut drafts = ValueDrafts::new();
        assert!(!commit_value(&mut vars, &mut drafts, EnvScope::Global, 0));
        assert_eq!(vars[0].value, "old");
    }

    #[test]
    fn committing_never_writes_a_draft_into_a_secret_row() {
        let mut vars = vec![var("TOKEN", "", true)];
        let mut drafts = ValueDrafts::new();
        drafts.insert((EnvScope::Global, 0), "smuggled".to_string());

        let committed = commit_value(&mut vars, &mut drafts, EnvScope::Global, 0);

        assert!(!committed);
        assert_eq!(vars[0].value, "", "got {:?}", vars[0].value);
        assert!(
            drafts.is_empty(),
            "the draft must not be left sitting in App state either"
        );
    }

    #[test]
    fn a_row_is_uncommitted_only_while_its_draft_differs_from_config() {
        let vars = [var("A", "same", false)];
        let mut drafts = ValueDrafts::new();
        assert!(!is_uncommitted(&drafts, EnvScope::Global, 0, &vars[0]));

        drafts.insert((EnvScope::Global, 0), "same".to_string());
        assert!(!is_uncommitted(&drafts, EnvScope::Global, 0, &vars[0]));

        drafts.insert((EnvScope::Global, 0), "different".to_string());
        assert!(is_uncommitted(&drafts, EnvScope::Global, 0, &vars[0]));
    }

    #[test]
    fn removing_a_row_drops_its_draft_and_shifts_the_later_ones_down() {
        // The bug this exists to catch: without the shift, row 2's in-progress
        // text commits into row 1's variable after row 1 is removed.
        let mut drafts = ValueDrafts::new();
        drafts.insert((EnvScope::Global, 0), "zero".to_string());
        drafts.insert((EnvScope::Global, 1), "one".to_string());
        drafts.insert((EnvScope::Global, 2), "two".to_string());
        drafts.insert((EnvScope::Project(0), 1), "other-scope".to_string());

        drop_draft_row(&mut drafts, EnvScope::Global, 1);

        assert_eq!(
            drafts.get(&(EnvScope::Global, 0)).map(String::as_str),
            Some("zero")
        );
        assert_eq!(
            drafts.get(&(EnvScope::Global, 1)).map(String::as_str),
            Some("two"),
            "row 2's draft must follow it down to row 1; got {drafts:?}"
        );
        assert_eq!(drafts.get(&(EnvScope::Global, 2)), None);
        assert_eq!(
            drafts.get(&(EnvScope::Project(0), 1)).map(String::as_str),
            Some("other-scope"),
            "another scope's rows did not move"
        );
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
