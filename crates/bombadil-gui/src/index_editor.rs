//! The Indexes tab: named package indexes, defined once globally
//! (`Config::indexes`) and opted into per project (`Project::index_names`).
//!
//! Spec §7: index credentials reach uv purely through environment variables
//! -- nothing is ever written to `pyproject.toml`. Verified against uv 0.12.1
//! with a header-capturing server: a named index and its credentials work
//! against a manifest that mentions no index at all, and uv sends Basic auth
//! preemptively (`envspec::index::inject`'s own doc comment carries the same
//! citation). This editor's job is to get a credential from the textbox to
//! the keychain -- never to `Config`, and never through this crate's own
//! `Debug`-deriving `Message` in a form that could print it.
//!
//! # Where `env_editor` follows this module, and where it still differs
//!
//! `env_editor` now holds an in-progress value in `App::env_value_drafts` for
//! the same reason this module holds a credential in
//! `App::index_credential_drafts`: typed text that might be about to become a
//! secret must not be written to `config.toml` while the user is still typing
//! it. Both commit on an explicit action rather than per keystroke.
//!
//! The remaining difference is what a commit means. An `EnvVar`'s value has a
//! genuine plaintext phase -- `HTTP_PROXY` is not a secret and belongs in
//! `Config` -- so committing one writes it there. `IndexAuth` has no
//! equivalent field: its variants are `None`, `Basic { username }` and
//! `Token`, so there is no "plaintext password" case, ever. Every character
//! typed into the credential textbox is already the credential, from the first
//! keystroke, and the only commit it has is the keychain.
//!
//! Two consequences follow:
//!
//! * The typed-so-far text lives in `App`'s own ephemeral scratch state
//!   (`App::index_credential_drafts`, one slot per `config.indexes` row),
//!   never in `Config` -- so there is nothing for a config save to write out
//!   and nothing for `Debug`-printing `Config` (it derives `Debug`) to leak.
//! * The `on_input` closure still has to hand that text to `update` through
//!   a `Message`, the same as any other iced controlled widget, and
//!   `Message` derives `Debug`. [`CredentialInput`] is a newtype with a
//!   hand-written `Debug` that never looks at its own field -- the same
//!   trick `InMemorySecretStore` already uses in
//!   `bombadil_core::secrets::memory` ("the derived one would render values,
//!   and failure reports render `Debug`"). Wrapping the field, not skipping
//!   `Debug` on the whole `Message` enum, is what keeps every *other*
//!   variant's derive doing its normal job.
//!
//! # Criterion 2: surfaced before the user saves, not discovered at sync time
//!
//! `validate::validate` already detects two index names that normalize to
//! the same uv credential variable, and `envspec::index::inject` already
//! refuses to write credentials for either once it notices -- at the point
//! of harm, mid-sync. [`collision_message`] is a thin filter over
//! `validate::validate`'s own output, not a second copy of the normalisation
//! rule (`validate::index_credential_var`'s own doc names that drift as the
//! exact bug this project has already been bitten by). The credential-save
//! button in `view` is disabled whenever this returns `Some` for the row's
//! current name, so the write this collision would corrupt never happens in
//! the first place -- the user sees the same sentence `validate` would give
//! them, before a keychain write, not after a failed sync.
//!
//! # Criterion 3: a project's selection is a subset, and deselecting keeps
//! the index
//!
//! `Project::index_names` only ever holds names the project has opted into;
//! [`toggle_selected`] adds or removes a name from that `Vec<String>` alone
//! and never touches `Config::indexes`, the same "removes only the
//! association" split `RemoveProjectRequested`'s own doc draws between
//! unregistering a project and destroying its venv. [`selected_are_subset`]
//! is the directly-testable form of the invariant this is supposed to
//! uphold.
//!
//! # Criterion 4: the credential is never rendered
//!
//! There is no code path here that reads a credential back out of the
//! `SecretStore` to redisplay it -- `view` never takes one at all. The
//! credential textbox binds to `App::index_credential_drafts` (what the user
//! has typed *this session*, cleared the instant it is handed to the
//! keychain job), rendered through `.secure(true)` so even that never shows
//! as plaintext on screen. [`CredentialInput`]'s hand-written `Debug` is the
//! other half: nothing that flows through `Message` can print it either.

use crate::app::Message;
use crate::theme;
use bombadil_core::model::{Config, Index, IndexAuth, IndexKind};
use bombadil_core::secrets::SecretKey;
use bombadil_core::validate;

/// The text shown in place of a saved credential -- criterion 4. Fixed,
/// regardless of whether this index already has a credential stored: `view`
/// never calls `SecretStore::get`, so there is no "real" state to vary this
/// on, and no path by which one could sneak in later without adding such a
/// call.
pub const CREDENTIAL_PLACEHOLDER: &str = "credential (write-only; never shown once saved)";

/// A credential character as it travels from the textbox's `on_input` into
/// `update` -- never the real text in `Debug`. See the module doc for why
/// this exists at all rather than a bare `String` field.
#[derive(Clone)]
pub struct CredentialInput(pub String);

impl std::fmt::Debug for CredentialInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CredentialInput(<redacted>)")
    }
}

/// Which shape of `IndexAuth` the auth selector currently offers. Carries no
/// `username` or secret of its own -- those are edited separately
/// (`IndexUsernameChanged`, the credential textbox) -- so switching choices
/// here can never smuggle either through a `Message`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthChoice {
    None,
    Basic,
    Token,
}

/// Appends a blank extra index with no auth -- the least surprising starting
/// point: it authenticates nothing and does not silently replace PyPI.
pub fn add_index(indexes: &mut Vec<Index>) {
    indexes.push(Index {
        name: String::new(),
        url: String::new(),
        kind: IndexKind::Extra,
        default_for_new_projects: false,
        auth: IndexAuth::None,
    });
}

/// Removes `indexes[i]` only -- callers are responsible for keeping any
/// parallel per-row state (`App::index_credential_drafts`) in step; this
/// module never assumes such state exists.
///
/// That includes the keychain: this has no `SecretStore`, so `app::update`
/// resolves the `SecretKey::Index` account *before* calling this and
/// dispatches the delete itself. Without it the credential stayed in the
/// user's keychain forever, with nothing left in the app able to name it.
pub fn remove_index(indexes: &mut Vec<Index>, i: usize) {
    if i < indexes.len() {
        indexes.remove(i);
    }
}

/// Renames `indexes[i]` only. The credential keyed on the old name is moved
/// by `app::update`, which resolves both accounts around this call -- see
/// `remove_index` for why that split lives there rather than here.
pub fn set_name(indexes: &mut [Index], i: usize, name: String) {
    if let Some(index) = indexes.get_mut(i) {
        index.name = name;
    }
}

pub fn set_url(indexes: &mut [Index], i: usize, url: String) {
    if let Some(index) = indexes.get_mut(i) {
        index.url = url;
    }
}

pub fn set_kind(indexes: &mut [Index], i: usize, kind: IndexKind) {
    if let Some(index) = indexes.get_mut(i) {
        index.kind = kind;
    }
}

pub fn toggle_default_for_new_projects(indexes: &mut [Index], i: usize) {
    if let Some(index) = indexes.get_mut(i) {
        index.default_for_new_projects = !index.default_for_new_projects;
    }
}

/// Switches which `IndexAuth` variant a row carries. Re-selecting the choice
/// a row already has is a no-op on its username -- pressing "basic" again
/// while already `Basic` must not wipe what was typed -- but switching away
/// to `Token`/`None` and back starts the username blank again: neither of
/// those variants has anywhere to hold it in the meantime, the same "nothing
/// to restore" restraint `env_editor::unmark_secret` documents for not
/// reading a value back out of the keychain either.
pub fn set_auth_choice(indexes: &mut [Index], i: usize, choice: AuthChoice) {
    let Some(index) = indexes.get_mut(i) else {
        return;
    };
    let existing_username = match &index.auth {
        IndexAuth::Basic { username } => username.clone(),
        _ => String::new(),
    };
    index.auth = match choice {
        AuthChoice::None => IndexAuth::None,
        AuthChoice::Basic => IndexAuth::Basic {
            username: existing_username,
        },
        AuthChoice::Token => IndexAuth::Token,
    };
}

/// A no-op unless the row is currently `IndexAuth::Basic` -- there is no
/// username field to write for `None` or `Token`.
pub fn set_username(indexes: &mut [Index], i: usize, username: String) {
    if let Some(index) = indexes.get_mut(i)
        && let IndexAuth::Basic { username: u } = &mut index.auth
    {
        *u = username;
    }
}

/// Criterion 2: the collision message `validate::validate` already produces
/// for `name`, if any -- reused rather than re-derived. `None` for a name
/// with no collision, including an empty name or one that does not (yet)
/// belong to any index in `config.indexes`.
///
/// Matches on the two message shapes `validate::validate` can produce for a
/// credential-variable collision (two distinct names that normalize the
/// same, or the exact same name entered twice) -- both share `"become"` and
/// name `name` (or one of `name`'s exact duplicates) in their text; every
/// other problem `validate::validate` reports for an unrelated reason (a
/// bad URL, an unknown reference, ...) does not.
pub fn collision_message(config: &Config, name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    validate::validate(config)
        .into_iter()
        .map(|p| p.message)
        .find(|m| m.contains("become") && m.contains(name))
}

/// What `IndexCredentialSaveRequested` needs to hand to the keychain job for
/// `config.indexes[i]`: the account it must be stored under (criterion 1)
/// and the plaintext to store there -- or `None` when there is nothing to
/// save (an out-of-range row, an empty draft) or nothing that may safely be
/// saved yet (criterion 2: `collision_message` is `Some` for this row's
/// current name). Checked here, not only by `view` disabling the button, so
/// `update` cannot dispatch a colliding write no matter what constructs the
/// message.
///
/// A pure function rather than inlined in `update`, so the exact
/// `(SecretKey, String)` pair a real press would send to the keychain is
/// itself directly testable -- proving `update`'s wiring reaches it is a
/// separate, thinner claim (`app.rs`'s own tests).
pub fn credential_save(
    config: &Config,
    drafts: &[String],
    i: usize,
) -> Option<(SecretKey, String)> {
    let index = config.indexes.get(i)?;
    if collision_message(config, &index.name).is_some() {
        return None;
    }
    let plain_value = drafts.get(i)?;
    if plain_value.is_empty() {
        return None;
    }
    Some((
        SecretKey::Index {
            name: index.name.clone(),
        },
        plain_value.clone(),
    ))
}

/// Criterion 3, the mutating half: adds `name` to a project's selection if
/// absent, removes it if present. Only ever touches `index_names` -- there is
/// no `&mut Config` in this signature for it to reach `Config::indexes`
/// through even by mistake.
pub fn toggle_selected(index_names: &mut Vec<String>, name: &str) {
    if let Some(pos) = index_names.iter().position(|n| n == name) {
        index_names.remove(pos);
    } else {
        index_names.push(name.to_string());
    }
}

/// Criterion 3, the invariant: every name a project has selected still names
/// a configured index. Proven directly against whatever state
/// [`toggle_selected`] produces, rather than trusting that a passing render
/// implies it.
pub fn selected_are_subset(project_index_names: &[String], indexes: &[Index]) -> bool {
    project_index_names
        .iter()
        .all(|name| indexes.iter().any(|index| &index.name == name))
}

/// Maps one index's row, in a project's own selection checklist, to the
/// glyph state it should render: [`theme::State::Present`] when the project
/// has selected it, [`theme::State::Absent`] otherwise. Only two states
/// apply here -- an index is either opted into or not, with nothing
/// analogous to "installed but outside the constraint" to give a third.
pub fn state(project_index_names: &[String], name: &str) -> theme::State {
    if project_index_names.iter().any(|n| n == name) {
        theme::State::Present
    } else {
        theme::State::Absent
    }
}

/// Renders one auth-choice button, disabled (via `on_press_maybe(None)`) when
/// it already names the row's current choice -- the same "no press needed,
/// no press possible" treatment `scripts_editor`'s move buttons give the
/// first/last row.
fn auth_button<'a>(
    label: &'static str,
    choice: AuthChoice,
    current: AuthChoice,
    i: usize,
) -> iced::Element<'a, Message> {
    iced::widget::button(iced::widget::text(label).size(theme::BODY))
        .on_press_maybe((choice != current).then_some(Message::IndexAuthChoiceChanged(i, choice)))
        .padding([theme::SPACE_1, theme::SPACE_2])
        .style(theme::button_choice(choice == current))
        .into()
}

/// One labelled field of an index card: the label in a fixed column, the
/// control beside it.
///
/// The fixed column is what makes the fields line up down the card, and the
/// labels are why they are readable at all: this used to be one horizontal
/// strip of five unlabelled controls, and inside the Preferences panel every
/// text input in it was squeezed to nothing. The name and URL boxes were not
/// merely small, they had no width left to render.
fn field<'a>(
    label: &'static str,
    control: iced::Element<'a, Message>,
) -> iced::Element<'a, Message> {
    iced::widget::row![
        iced::widget::text(label)
            .size(theme::LABEL)
            .style(theme::text_muted)
            .width(theme::INDEX_LABEL_WIDTH),
        control,
    ]
    .spacing(theme::SPACE_2)
    .align_y(iced::Alignment::Center)
    .into()
}

/// Renders one index as a card of labelled fields, plus -- if its name
/// collides with another's credential variable -- the message criterion 2
/// requires, naming both, directly under it.
fn index_row<'a>(
    i: usize,
    index: &Index,
    config: &Config,
    draft: &str,
) -> iced::Element<'a, Message> {
    // Enter commits, the same as the card's save button below: typing itself
    // neither persists nor moves the keychain credential -- see
    // `app::commit_index_edit`. Both fields are data -- a name and a URL are
    // compared character by character -- so both render in Plex.
    let name_input = iced::widget::text_input("internal", &index.name)
        .style(crate::theme::text_field)
        .font(theme::FONT_DATA)
        .size(theme::DATA)
        .padding(theme::SPACE_1)
        .on_input(move |v| Message::IndexNameChanged(i, v))
        .on_submit(Message::IndexEditCommitted(i));
    let url_input = iced::widget::text_input("https://nexus.example/simple", &index.url)
        .style(crate::theme::text_field)
        .font(theme::FONT_DATA)
        .size(theme::DATA)
        .padding(theme::SPACE_1)
        .on_input(move |v| Message::IndexUrlChanged(i, v))
        .on_submit(Message::IndexEditCommitted(i));

    // Two options shown as two, not one button whose label explains what the
    // *other* press would do. "default index (replaces PyPI) -- click for
    // extra" was 47 characters of control, and it was the single widest thing
    // in the row.
    let kind_choice = iced::widget::row![
        kind_button("replaces PyPI", IndexKind::Default, index.kind, i),
        kind_button("supplements PyPI", IndexKind::Extra, index.kind, i),
    ]
    .spacing(theme::SPACE_1);

    let current_choice = match &index.auth {
        IndexAuth::None => AuthChoice::None,
        IndexAuth::Basic { .. } => AuthChoice::Basic,
        IndexAuth::Token => AuthChoice::Token,
    };
    let auth_buttons = iced::widget::row![
        auth_button("none", AuthChoice::None, current_choice, i),
        auth_button("basic", AuthChoice::Basic, current_choice, i),
        auth_button("token", AuthChoice::Token, current_choice, i),
    ]
    .spacing(theme::SPACE_1);

    let collision = collision_message(config, &index.name);

    let mut card = iced::widget::column![
        field("name", name_input.into()),
        field("url", url_input.into()),
        field("kind", kind_choice.into()),
        field(
            "",
            iced::widget::checkbox(index.default_for_new_projects)
                .label("use for new projects")
                .text_size(theme::BODY)
                .on_toggle(move |_| Message::IndexDefaultToggled(i))
                .into()
        ),
        field("auth", auth_buttons.into()),
    ]
    .spacing(theme::SPACE_2);

    if let IndexAuth::Basic { username } = &index.auth {
        card = card.push(field(
            "username",
            iced::widget::text_input("alice", username)
                .style(crate::theme::text_field)
                .font(theme::FONT_DATA)
                .size(theme::DATA)
                .padding(theme::SPACE_1)
                .on_input(move |v| Message::IndexUsernameChanged(i, v))
                .on_submit(Message::IndexEditCommitted(i))
                .into(),
        ));
    }

    if !matches!(index.auth, IndexAuth::None) {
        // Criterion 4: bound to the ephemeral draft, never to anything read
        // back from the keychain, and rendered `.secure(true)` so even the
        // in-flight text is masked on screen.
        let credential_input = iced::widget::text_input(CREDENTIAL_PLACEHOLDER, draft)
            .style(crate::theme::text_field)
            .font(theme::FONT_DATA)
            .size(theme::DATA)
            .padding(theme::SPACE_1)
            .secure(true)
            .on_input(move |v| Message::IndexCredentialChanged(i, CredentialInput(v)));
        let can_save = collision.is_none() && !draft.is_empty();
        card = card.push(field(
            match index.auth {
                IndexAuth::Token => "token",
                _ => "password",
            },
            iced::widget::row![
                credential_input,
                iced::widget::button(iced::widget::text("save to keychain").size(theme::BODY))
                    .on_press_maybe(can_save.then_some(Message::IndexCredentialSaveRequested(i)))
                    .padding([theme::SPACE_1, theme::SPACE_2])
                    .style(theme::button_quiet),
            ]
            .spacing(theme::SPACE_2)
            .into(),
        ));
    }

    if let Some(message) = collision {
        // Not one of `BOOT`'s two rationed cases (the drifted glyph, a
        // destructive confirm) -- this is a save-blocking validation message,
        // not a third glyph state, so it stays in the default text colour
        // rather than borrowing the rationed one.
        card = card.push(iced::widget::text(message).size(theme::BODY));
    }

    // The typed fields (name, URL, username) reach `config.toml` through this
    // press or through Enter, never per keystroke.
    card = card.push(
        iced::widget::row![
            iced::widget::Space::new().width(iced::Length::Fill),
            iced::widget::button(iced::widget::text("save").size(theme::BODY))
                .on_press(Message::IndexEditCommitted(i))
                .padding([theme::SPACE_1, theme::SPACE_3])
                .style(theme::button_primary),
            iced::widget::button(iced::widget::text("remove").size(theme::BODY))
                .on_press(Message::IndexRemoved(i))
                .padding([theme::SPACE_1, theme::SPACE_3])
                .style(theme::button_quiet),
        ]
        .spacing(theme::SPACE_2),
    );

    iced::widget::container(card)
        .padding(theme::SPACE_3)
        .style(theme::panel)
        .into()
}

/// One of the two index kinds, shown as a choice rather than as a toggle
/// whose label describes the press you are not making.
fn kind_button<'a>(
    label: &'static str,
    kind: IndexKind,
    current: IndexKind,
    i: usize,
) -> iced::Element<'a, Message> {
    iced::widget::button(iced::widget::text(label).size(theme::BODY))
        .on_press_maybe((kind != current).then_some(Message::IndexKindChanged(i, kind)))
        .padding([theme::SPACE_1, theme::SPACE_2])
        .style(theme::button_choice(kind == current))
        .into()
}

/// Every configured index: name, URL, kind, auth. Defined once, globally --
/// so one index means one keychain credential, whichever projects use it.
///
/// Split from [`project_view`] because Preferences shows one scope at a time.
pub fn global_view<'a>(
    config: &Config,
    credential_drafts: &[String],
) -> iced::Element<'a, Message> {
    let mut column = iced::widget::column![
        iced::widget::text(
            "Defined once and used by any project. Credentials live in the OS keychain, \
             keyed by the index name."
        )
        .size(theme::BODY)
        .style(theme::text_muted),
    ]
    .spacing(theme::SPACE_3);

    for (i, index) in config.indexes.iter().enumerate() {
        let draft = credential_drafts.get(i).map(String::as_str).unwrap_or("");
        column = column.push(index_row(i, index, config, draft));
    }

    column
        .push(
            iced::widget::button(iced::widget::text("Add index").size(theme::BODY))
                .on_press(Message::IndexAdded)
                .padding([theme::SPACE_1, theme::SPACE_3])
                .style(theme::button_quiet),
        )
        .into()
}

/// Which of the global indexes this project uses.
///
/// A selection, not a definition: adding an index is a global act, and this
/// scope only ticks the ones that apply here.
pub fn project_view<'a>(config: &Config, project_index: usize) -> iced::Element<'a, Message> {
    let Some(project) = config.projects.get(project_index) else {
        return iced::widget::text("").into();
    };

    let mut column = iced::widget::column![
        iced::widget::text("Which of the indexes above this project resolves against.")
            .size(theme::BODY)
            .style(theme::text_muted),
    ]
    .spacing(theme::SPACE_2);

    for index in &config.indexes {
        let glyph = theme::state_glyph(state(&project.index_names, &index.name));
        let selected = project.index_names.contains(&index.name);
        let name = index.name.clone();
        // The index name is the checkbox label -- data, the same as any other
        // name compared character by character, so it renders in Plex rather
        // than the checkbox's default prose font.
        let checkbox = iced::widget::checkbox(selected)
            .label(index.name.clone())
            .font(theme::FONT_DATA)
            .text_size(theme::DATA)
            .on_toggle(move |_| Message::ProjectIndexToggled(project_index, name.clone()));
        let line = iced::widget::row![
            iced::widget::text(glyph.glyph.to_string())
                .size(theme::BODY)
                .style(theme::tinted(glyph.swatch))
                .width(theme::GLYPH_COLUMN_WIDTH),
            checkbox,
        ]
        .spacing(theme::SPACE_1);
        column = column.push(
            iced::widget::container(line)
                .padding(theme::SPACE_1)
                .style(theme::surface),
        );
    }

    // Criterion 3's invariant, stated rather than only silently relied on:
    // `banner::view` already reports a stale reference by name
    // (`validate::validate`'s "references index ... which is not defined"),
    // so this never fires in practice; it exists as the directly-testable
    // guard this scope's selection state has to keep satisfying, not a second
    // copy of that user-facing message.
    if !selected_are_subset(&project.index_names, &config.indexes) {
        column = column.push(
            iced::widget::text("this project selects an index that no longer exists")
                .size(theme::BODY),
        );
    }

    column.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bombadil_core::model::Project;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn index(name: &str, kind: IndexKind, auth: IndexAuth) -> Index {
        Index {
            name: name.into(),
            url: "https://example.com/simple".into(),
            kind,
            default_for_new_projects: false,
            auth,
        }
    }

    fn config_with(indexes: Vec<Index>) -> Config {
        Config {
            indexes,
            ..Config::default()
        }
    }

    fn project_with_indexes(index_names: Vec<String>) -> Project {
        Project {
            id: Uuid::from_u128(1),
            label: "api".into(),
            pyproject_path: PathBuf::from("/p/api/pyproject.toml"),
            index_names,
            environments: vec![bombadil_core::model::Environment {
                location: bombadil_core::model::VenvLocation::Alongside,
                python: bombadil_core::model::PythonPin::Unpinned,
            }],
            active: bombadil_core::model::VenvLocation::Alongside,
            ..Project::default()
        }
    }

    // --- criterion 1: proven in app.rs against `run_secret_write`, the same
    // way `env_editor`'s own doc explains it needs a `ConfigPersist` this
    // module doesn't have. The in-memory-only half -- that nothing here ever
    // writes credential text into an `Index`/`Config` field -- is what the
    // add/set functions below are proven never to do. ---

    #[test]
    fn add_index_carries_no_auth_and_no_credential_field_to_carry_one_in() {
        let mut indexes = Vec::new();
        add_index(&mut indexes);
        assert_eq!(indexes.len(), 1);
        assert_eq!(indexes[0].auth, IndexAuth::None);
    }

    #[test]
    fn set_username_never_touches_a_non_basic_row() {
        let mut indexes = vec![index("priv", IndexKind::Extra, IndexAuth::Token)];
        set_username(&mut indexes, 0, "smuggled".to_string());
        assert_eq!(
            indexes[0].auth,
            IndexAuth::Token,
            "a Token row has no username field to write into"
        );
    }

    #[test]
    fn re_choosing_basic_while_already_basic_does_not_clear_the_username() {
        let mut indexes = vec![index(
            "priv",
            IndexKind::Extra,
            IndexAuth::Basic {
                username: "alice".into(),
            },
        )];
        set_auth_choice(&mut indexes, 0, AuthChoice::Basic);
        assert_eq!(
            indexes[0].auth,
            IndexAuth::Basic {
                username: "alice".into()
            },
            "pressing the already-selected choice must not wipe what was typed"
        );
    }

    #[test]
    fn switching_away_from_basic_and_back_starts_the_username_blank() {
        // Neither `Token` nor `None` has anywhere to hold a username in the
        // meantime -- pinned so a future change that tried to cache and
        // restore it (adding a field elsewhere) is a deliberate choice, not
        // an accident this test would silently start passing for.
        let mut indexes = vec![index(
            "priv",
            IndexKind::Extra,
            IndexAuth::Basic {
                username: "alice".into(),
            },
        )];
        set_auth_choice(&mut indexes, 0, AuthChoice::Token);
        set_auth_choice(&mut indexes, 0, AuthChoice::Basic);
        assert_eq!(
            indexes[0].auth,
            IndexAuth::Basic {
                username: String::new()
            }
        );
    }

    #[test]
    fn set_auth_choice_to_none_carries_no_username() {
        let mut indexes = vec![index(
            "priv",
            IndexKind::Extra,
            IndexAuth::Basic {
                username: "alice".into(),
            },
        )];
        set_auth_choice(&mut indexes, 0, AuthChoice::None);
        assert_eq!(indexes[0].auth, IndexAuth::None);
    }

    // --- criterion 2: the collision message is surfaced from `validate`,
    // not re-derived, and names both colliding index names ---

    #[test]
    fn a_colliding_name_yields_a_message_naming_both() {
        let config = config_with(vec![
            index("my-index", IndexKind::Extra, IndexAuth::None),
            index("my_index", IndexKind::Extra, IndexAuth::None),
        ]);

        let got = collision_message(&config, "my-index").expect("a collision must be reported");

        assert!(got.contains("my-index"), "got {got:?}");
        assert!(got.contains("my_index"), "got {got:?}");
    }

    #[test]
    fn a_non_colliding_name_yields_no_message() {
        let config = config_with(vec![
            index("priv", IndexKind::Extra, IndexAuth::None),
            index("public", IndexKind::Extra, IndexAuth::None),
        ]);
        assert_eq!(collision_message(&config, "priv"), None);
    }

    #[test]
    fn an_empty_name_never_collides() {
        // A freshly added index (`add_index`) starts with an empty name --
        // this must not be treated as colliding with anything.
        let config = config_with(vec![index("", IndexKind::Extra, IndexAuth::None)]);
        assert_eq!(collision_message(&config, ""), None);
    }

    // --- criteria 1 and 2 together: what a real "save credential" press
    // hands the keychain job ---

    #[test]
    fn credential_save_names_the_index_under_secretkey_index() {
        // Criterion 1: the account a real save writes under.
        let config = config_with(vec![index("priv", IndexKind::Extra, IndexAuth::None)]);
        let drafts = vec!["hunter2".to_string()];

        let (key, value) = credential_save(&config, &drafts, 0).expect("nothing should block this");

        assert_eq!(
            key,
            SecretKey::Index {
                name: "priv".to_string()
            }
        );
        assert_eq!(value, "hunter2");
    }

    #[test]
    fn credential_save_refuses_a_colliding_name() {
        // Criterion 2: the write this collision would corrupt never happens.
        let config = config_with(vec![
            index("my-index", IndexKind::Extra, IndexAuth::None),
            index("my_index", IndexKind::Extra, IndexAuth::None),
        ]);
        let drafts = vec!["a-secret".to_string(), "b-secret".to_string()];

        assert_eq!(credential_save(&config, &drafts, 0), None);
        assert_eq!(credential_save(&config, &drafts, 1), None);
    }

    #[test]
    fn credential_save_refuses_an_empty_draft() {
        let config = config_with(vec![index("priv", IndexKind::Extra, IndexAuth::None)]);
        let drafts = vec![String::new()];
        assert_eq!(credential_save(&config, &drafts, 0), None);
    }

    #[test]
    fn credential_save_refuses_an_out_of_range_row() {
        let config = config_with(vec![]);
        assert_eq!(credential_save(&config, &[], 0), None);
    }

    // --- criterion 3: a project's selection is a subset of the configured
    // indexes, and deselecting removes only the association ---

    #[test]
    fn toggling_an_unselected_index_selects_it() {
        let mut selected = vec!["a".to_string()];
        toggle_selected(&mut selected, "b");
        assert_eq!(selected, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn toggling_a_selected_index_deselects_it() {
        let mut selected = vec!["a".to_string(), "b".to_string()];
        toggle_selected(&mut selected, "a");
        assert_eq!(selected, vec!["b".to_string()]);
    }

    #[test]
    fn deselecting_never_touches_the_configured_indexes() {
        // The bug criterion 3 exists to catch: deleting shared configuration
        // from a per-project screen. `toggle_selected`'s signature does not
        // even take `Config::indexes`, but this pins the observable
        // behaviour a caller relies on too.
        let indexes = [index("priv", IndexKind::Extra, IndexAuth::None)];
        let mut project = project_with_indexes(vec!["priv".to_string()]);

        toggle_selected(&mut project.index_names, "priv");

        assert!(
            project.index_names.is_empty(),
            "the association must be gone"
        );
        assert_eq!(
            indexes.len(),
            1,
            "the configured index itself must still exist"
        );
        assert_eq!(indexes[0].name, "priv");
    }

    #[test]
    fn a_projects_selection_consistent_with_config_is_a_subset() {
        let indexes = vec![
            index("priv", IndexKind::Extra, IndexAuth::None),
            index("public", IndexKind::Extra, IndexAuth::None),
        ];
        let project = project_with_indexes(vec!["priv".to_string()]);
        assert!(selected_are_subset(&project.index_names, &indexes));
    }

    #[test]
    fn a_selection_naming_a_since_removed_index_is_not_a_subset() {
        let indexes = vec![index("public", IndexKind::Extra, IndexAuth::None)];
        let project = project_with_indexes(vec!["priv".to_string()]);
        assert!(!selected_are_subset(&project.index_names, &indexes));
    }

    #[test]
    fn an_empty_selection_is_always_a_subset() {
        let indexes = vec![index("priv", IndexKind::Extra, IndexAuth::None)];
        assert!(selected_are_subset(&[], &indexes));
    }

    // --- criterion 4: the credential is never rendered ---

    #[test]
    fn a_credential_input_never_prints_its_wrapped_text_in_debug() {
        let input = CredentialInput("hunter2".to_string());
        let rendered = format!("{input:?}");
        assert!(
            !rendered.contains("hunter2"),
            "a CredentialInput's Debug rendering must never carry the real text; got {rendered:?}"
        );
    }

    #[test]
    fn the_index_credential_changed_message_never_carries_its_text_in_debug() {
        // The message a real keystroke sends into `update` -- proven at the
        // `Message` level, not just the newtype's own `Debug`, in case a
        // future edit added a second field the derive could print instead.
        let msg = Message::IndexCredentialChanged(0, CredentialInput("hunter2".to_string()));
        let rendered = format!("{msg:?}");
        assert!(!rendered.contains("hunter2"), "got {rendered:?}");
    }

    #[test]
    fn the_credential_placeholder_never_contains_a_real_looking_secret() {
        // Guards the placeholder text itself against ever being edited into
        // something that looks like it discloses a stored value.
        assert!(!CREDENTIAL_PLACEHOLDER.is_empty());
        assert!(!CREDENTIAL_PLACEHOLDER.to_lowercase().contains("hunter"));
    }

    // --- state: the glyph mapping for one index's row in a project's
    // selection. Only two states apply here -- see the module doc's
    // criterion 3: a project's selection is a subset of `Config::indexes`,
    // never an installed/drifted distinction. ---

    #[test]
    fn a_selected_index_is_present() {
        assert_eq!(state(&["priv".to_string()], "priv"), theme::State::Present);
    }

    #[test]
    fn an_unselected_index_is_absent() {
        assert_eq!(state(&["public".to_string()], "priv"), theme::State::Absent);
    }
}
