# Preferences (UI) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Help > Preferences opens one surface where every setting is reachable, globally or for one project, with each project field showing whether it inherits or overrides.

**Architecture:** A modal with a scope switch across the top and a section list down the left. The three editors it absorbs (`settings_editor`, `env_editor`, `index_editor`) already separate their mutation logic from their `view`, and every one of those `pub fn`s keeps its existing tests — this plan moves and re-slices views, and adds the small amount of genuinely new logic that scope switching and inherit/override need.

**Tech Stack:** Rust edition 2024, iced 0.14, `bombadil-core` (no iced).

## Global Constraints

- **No new dependency.**
- **`view` is untested by convention** (see `bombadil-gui/src/main.rs` module docs). Most of this plan is `view`, so most of it ships without mutation proofs. Every task below states which part of it *is* testable, and that part gets tests. Do not invent assertions about `Element`s to make the ratio look better.
- **A secret must never enter a `Message`**, which derives `Debug`.
- **`boot` is rationed** — see `theme`'s module doc. A Preferences field is not an alarm.
- **Never compare a `display()`ed path to a string literal**, and never assume a line ending. Three CI failures on this project came from a platform assumption hiding in an assertion.
- Clippy lints denied in CI: `derivable_impls`, `field_reassign_with_default`. `just ci` must pass.

## What already exists

Plan A (merged, PR #20) delivered the model and the resolver:

- `Override<T>` = `Inherit | Set(T)`, `PythonPin = Unpinned | Version`
- `Settings::python`; `Project::{python, uv_source, terminals}`
- `resolve::effective(config, project, os) -> Effective { python, uv_source, terminal }`
- `interpreter::satisfies_pin`, sidebar drift against the pin
- `run_sync` passes `-p` when a pin resolves
- `sync_would_delete` + `ConfirmPinChange` gate the Sync press

**Nothing in the UI can set a pin yet.** This plan is what makes the destructive
path reachable, so Task 4 is where the guard gets exercised end to end for the
first time.

## Decisions from the spec

- One Preferences screen, scope switch (Global | each project), section list on the left.
- Env vars, Indexes and Settings tabs are absorbed and removed. **Scripts keeps its own tab** (the user asked for this explicitly). Dependencies and Members stay — readouts, not settings.
- Indexes stay globally defined; a project ticks which apply.
- Lists merge, scalars override. The UI says which each section is rather than pretending both behave alike.

---

### Task 1: Scope and Section, and the state that survives a project disappearing

**Files:**
- Create: `crates/bombadil-gui/src/preferences.rs`
- Modify: `crates/bombadil-gui/src/main.rs` (register the module)

**Interfaces:**
- Produces:
  - `pub enum Scope { Global, Project(Uuid) }`
  - `pub enum Section { Environments, Python, Uv, Terminal, EnvVars, Indexes }` with `ALL` and `fn label(self) -> &'static str`
  - `pub struct State { pub scope: Scope, pub section: Section }` with `Default`
  - `pub fn resolve_scope(scope: Scope, config: &Config) -> Scope`

> `Scope::Project` carries a `Uuid`, not an index, for the reason `ConfirmRecreate` documents at length: an index is only meaningful against the list as it stood when it was captured. Preferences can be open while a project is removed from the sidebar behind it.

- [ ] **Step 1: Write the failing tests**

```rust
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
            label: "api".into(),
            ..Project::default()
        };
        let second = Project {
            id: uuid::Uuid::from_u128(2),
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
        let scope = Scope::Project(uuid::Uuid::from_u128(7));
        assert_eq!(resolve_scope(scope, &config_with(vec![])), Scope::Global);
    }

    #[test]
    fn global_always_resolves_to_itself() {
        assert_eq!(resolve_scope(Scope::Global, &config_with(vec![])), Scope::Global);
    }

    #[test]
    fn every_section_has_a_distinct_label() {
        // The section list is the only way to reach five of the six, so two
        // sharing a label makes one unreachable in practice.
        let labels: Vec<&str> = Section::ALL.iter().map(|s| s.label()).collect();
        for (i, label) in labels.iter().enumerate() {
            assert!(!label.trim().is_empty(), "{:?} has no label", Section::ALL[i]);
            assert!(
                !labels[i + 1..].contains(label),
                "two sections are both called {label}"
            );
        }
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p bombadil-gui --offline preferences`
Expected: FAIL to compile — nothing in this module is defined yet.

- [ ] **Step 3: Write the implementation**

```rust
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
//! "Inherit from global" control. Environment variables and indexes are
//! lists that *combine* -- a project's variables layer over the global ones
//! by key, and a project ticks which of the global indexes apply. Those
//! sections say so rather than offering an inherit control that would mean
//! something different from the one three rows above it.

use bombadil_core::model::Config;
use uuid::Uuid;

/// Whose settings are on screen.
///
/// `Project` carries a `Uuid`, not an index, for the reason
/// `context_menu::ConfirmRecreate` documents: an index is only meaningful
/// against the list as it stood when it was captured, and Preferences can be
/// open while projects are added or removed behind it.
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
    /// Every section, in the order the list renders them: the three that
    /// decide how an environment is built first, then the two lists.
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
/// "not open".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    pub scope: Scope,
    pub section: Section,
}

impl Default for State {
    fn default() -> Self {
        // Global, because it is the only scope guaranteed to exist: opening
        // on a project would need there to be one.
        Self {
            scope: Scope::Global,
            section: Section::ALL[0],
        }
    }
}

/// The scope to actually render, given the config as it stands now.
///
/// A project scope whose project has been removed falls back to `Global`.
/// Without this the next render shows a project that no longer exists, and an
/// edit made in it would be written into whichever project took its place --
/// the exact class of bug `ConfirmRecreate` carries a `Uuid` to avoid.
pub fn resolve_scope(scope: Scope, config: &Config) -> Scope {
    match scope {
        Scope::Global => Scope::Global,
        Scope::Project(id) if config.projects.iter().any(|p| p.id == id) => scope,
        Scope::Project(_) => Scope::Global,
    }
}
```

`impl Default for State` is written by hand rather than derived: `Scope` has no
meaningful `Default` of its own, and deriving one on it would invite a second,
different answer to "which scope is the default". Clippy's `derivable_impls`
does not fire, because this is not what a derive would produce.

- [ ] **Step 4: Register the module**

In `crates/bombadil-gui/src/main.rs`, alongside the other `mod` lines:

```rust
mod preferences;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p bombadil-gui --offline preferences`
Expected: PASS, 5 tests.

- [ ] **Step 6: Prove the fallback discriminates**

Change `resolve_scope`'s third arm to `Scope::Project(id) => Scope::Project(id)`.
Run: `cargo test -p bombadil-gui --offline preferences`
Expected: FAIL — `a_scope_whose_project_is_gone_falls_back_to_global`. Restore it.

- [ ] **Step 7: Commit**

```bash
git add crates/
git commit -m "feat(gui): Preferences scope and section, with a scope that survives removal"
```

---

### Task 2: Open it from the menu, and switch scope and section

**Files:**
- Modify: `crates/bombadil-gui/src/shell.rs`, `crates/bombadil-gui/src/app.rs`, `crates/bombadil-gui/src/preferences.rs`

**Interfaces:**
- Consumes: `Scope`, `Section`, `State`, `resolve_scope` (Task 1).
- Produces: `App::preferences: Option<preferences::State>`; `Message::{PreferencesOpened, PreferencesClosed, PreferencesScopeSelected(Scope), PreferencesSectionSelected(Section)}`; `preferences::view(state, config, ...) -> Element`.

- [ ] **Step 1: Write the failing tests**

In `crates/bombadil-gui/src/app.rs`'s `mod tests`:

```rust
    #[test]
    fn the_help_menu_opens_preferences_on_global() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let _ = update(&mut app, Message::MenuToggled, unused_runner());
        let _ = update(&mut app, Message::PreferencesOpened, unused_runner());

        let state = app.preferences.expect("Preferences must open");
        assert_eq!(state.scope, preferences::Scope::Global);
        assert!(
            !app.menu_open,
            "the menu must close behind what it opened, as `ModalOpened` does"
        );
    }

    #[test]
    fn switching_scope_keeps_the_section_you_were_reading() {
        // Scope and section are independent axes. Resetting the section on a
        // scope change would throw the user back to the top of the list every
        // time they compared a project against the global value -- which is
        // the main thing this screen is for.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let id = app.config.projects[0].id;

        let _ = update(&mut app, Message::PreferencesOpened, unused_runner());
        let _ = update(
            &mut app,
            Message::PreferencesSectionSelected(preferences::Section::Python),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::PreferencesScopeSelected(preferences::Scope::Project(id)),
            unused_runner(),
        );

        let state = app.preferences.expect("still open");
        assert_eq!(state.scope, preferences::Scope::Project(id));
        assert_eq!(state.section, preferences::Section::Python);
    }

    #[test]
    fn closing_preferences_keeps_every_edit_made_in_it() {
        // There is no OK/Cancel here: every control writes straight through to
        // `Config` and persists, the same as the tabs it replaces. Closing is
        // not an undo, and must never read as one.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let _ = update(&mut app, Message::PreferencesOpened, unused_runner());
        let _ = update(
            &mut app,
            Message::UvSourceSelected(settings_editor::UvSourceChoice::Bundled),
            unused_runner(),
        );
        let _ = update(&mut app, Message::PreferencesClosed, unused_runner());

        assert!(app.preferences.is_none());
        assert_eq!(app.config.settings.uv_source, UvSource::Bundled);
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p bombadil-gui --offline preferences`
Expected: FAIL to compile — the messages and the `preferences` field do not exist.

- [ ] **Step 3: Add the state and messages**

In `App`, beside `modal`:

```rust
    /// What Preferences is showing, or `None` when it is closed.
    ///
    /// Separate from `modal`: that one is a stack of read-only cards with a
    /// Close button, and this is a working surface whose controls write
    /// through to `Config`. Sharing the field would mean one Close doing two
    /// different things.
    pub preferences: Option<preferences::State>,
```

initialise it to `None` in `App::new`, and add the messages:

```rust
    /// Help > Preferences.
    PreferencesOpened,
    PreferencesClosed,
    /// The scope switch: whose settings to show.
    PreferencesScopeSelected(preferences::Scope),
    /// The section list on the left.
    PreferencesSectionSelected(preferences::Section),
```

and the arms:

```rust
        Message::PreferencesOpened => {
            app.preferences = Some(preferences::State::default());
            app.menu_open = false;
        }
        Message::PreferencesClosed => {
            // No undo: every control in there has already written through to
            // `Config` and persisted, the same as the tabs it replaces.
            app.preferences = None;
        }
        Message::PreferencesScopeSelected(scope) => {
            if let Some(state) = &mut app.preferences {
                // The section is deliberately untouched: the two axes are
                // independent, and comparing one project against the global
                // value is the main thing this screen is for.
                state.scope = scope;
            }
        }
        Message::PreferencesSectionSelected(section) => {
            if let Some(state) = &mut app.preferences {
                state.section = section;
            }
        }
```

- [ ] **Step 4: Add the menu entry**

In `crates/bombadil-gui/src/shell.rs`, the Help menu currently holds only
modals. Preferences is not a modal, so `MENU_ENTRIES` cannot carry it. Replace
the entry list with one that names its own message:

```rust
/// Every entry in the Help menu, in the order it is shown.
///
/// The message, not a `Modal`: Preferences is a working surface rather than a
/// read-only card, so it is not one. One list, so the menu and the tests that
/// hold it to still offering the licences cannot disagree.
pub fn menu_entries() -> [(&'static str, Message); 3] {
    [
        ("Preferences", Message::PreferencesOpened),
        ("About Bombadil", Message::ModalOpened(Modal::About)),
        ("Bundled licences", Message::ModalOpened(Modal::Licences)),
    ]
}
```

and update `menu_layer` to iterate it, and `shell`'s own tests to use it:
`the_menu_offers_the_bundled_licences` asserts an entry whose message is
`Message::ModalOpened(Modal::Licences)`;
`every_menu_entry_is_labelled_and_opens_a_distinct_modal` becomes
`every_menu_entry_is_labelled_and_does_something_distinct`, comparing the
`format!("{message:?}")` of each entry.

- [ ] **Step 5: Render it as a layer**

In `app::view`, beside the other layers:

```rust
    if let Some(state) = app.preferences {
        let scope = preferences::resolve_scope(state.scope, &app.config);
        layers = layers.push(preferences::view(
            state.section,
            scope,
            &app.config,
            &app.env_value_drafts,
            &app.index_credential_drafts,
            &app.uv_custom_path_draft,
            app.uv_custom_validation.as_ref(),
        ));
    }
```

`resolve_scope` is applied *here*, at render, rather than stored: recomputing
it every frame is the same "recompute, don't cache" discipline
`banner::problems` and `add_project::resolved_venv_path_display` already
follow, and it is what makes a removed project take effect immediately rather
than at the next open.

- [ ] **Step 6: Write `preferences::view`**

A `shell::modal_layer`-shaped card: title row with the scope switch and a
Close button, then a row of the section list and the chosen section's panel.
Each section's panel delegates to the existing editors for now — Task 3
splits them by scope, and until then they render as they always have:

```rust
pub fn view<'a>(
    section: Section,
    scope: Scope,
    config: &Config,
    env_drafts: &crate::env_editor::ValueDrafts,
    credential_drafts: &[String],
    uv_custom_path_draft: &str,
    uv_custom_validation: Option<&Result<String, String>>,
) -> iced::Element<'a, Message> {
    let selected_project = match scope {
        Scope::Global => None,
        Scope::Project(id) => config.projects.iter().position(|p| p.id == id),
    };

    let panel: iced::Element<'a, Message> = match section {
        Section::EnvVars => crate::env_editor::view(config, selected_project, env_drafts),
        Section::Indexes => {
            crate::index_editor::view(config, selected_project, credential_drafts)
        }
        Section::Environments | Section::Uv => crate::settings_editor::view(
            &config.settings,
            &config.projects,
            uv_custom_path_draft,
            uv_custom_validation,
        ),
        // Task 4 fills these in; until then they say so rather than
        // rendering an empty panel that looks broken.
        Section::Python | Section::Terminal => iced::widget::text("Coming in the next task")
            .size(theme::BODY)
            .color(theme::SLATE)
            .into(),
    };

    // ... scope switch, section list, card, scrim -- see `shell::modal_layer`
    // for the layering, and `shell::dismissable` for why `opaque` goes around
    // the card and `mouse_area` outside it.
}
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p bombadil-gui --offline`
Expected: PASS.

- [ ] **Step 8: Prove the scope switch does not reset the section**

Change `PreferencesScopeSelected`'s arm to also set
`state.section = Section::default();`.
Run: `cargo test -p bombadil-gui --offline preferences`
Expected: FAIL — `switching_scope_keeps_the_section_you_were_reading`. Restore it.

- [ ] **Step 9: Commit**

```bash
git add crates/
git commit -m "feat(gui): open Preferences from the menu, switch scope and section"
```

---

### Task 3: Split the absorbed editors by scope

**Files:**
- Modify: `crates/bombadil-gui/src/env_editor.rs`, `crates/bombadil-gui/src/index_editor.rs`, `crates/bombadil-gui/src/settings_editor.rs`, `crates/bombadil-gui/src/preferences.rs`

**Interfaces:**
- Produces: `env_editor::{global_view, project_view}`, `index_editor::{global_view, project_view}`, `settings_editor::global_view`. Every existing `pub fn` that mutates keeps its name, signature and tests.

> Each of these `view`s currently renders both scopes stacked in one pane, which is exactly what the scope switch replaces. **Pure view movement**: no mutation logic changes, so the whole of each module's existing test suite must still pass untouched. If a test needs editing, something moved that should not have.

- [ ] **Step 1: Split `env_editor::view`**

Cut the existing function at its "Global variables" / project boundary into:

```rust
/// The global variables half. Every project sees these unless it sets its own
/// variable with the same key.
pub fn global_view<'a>(config: &Config, drafts: &ValueDrafts) -> iced::Element<'a, Message>

/// One project's own variables, and which global ones they shadow.
///
/// `overrides` is what makes the shadowing visible; it already exists and is
/// already tested, and this is the view that was hiding it below a fold.
pub fn project_view<'a>(
    config: &Config,
    project: usize,
    drafts: &ValueDrafts,
) -> iced::Element<'a, Message>
```

Delete the old `view`. Do not touch `vars_mut`, `add_var`, `remove_var`,
`set_key`, `set_value`, `commit_value`, `mark_secret`, `unmark_secret`,
`overrides`, or any of their tests.

- [ ] **Step 2: Split `index_editor::view` the same way**

```rust
/// Every configured index: name, URL, kind, auth. Defined once, globally.
pub fn global_view<'a>(config: &Config, credential_drafts: &[String]) -> iced::Element<'a, Message>

/// Which of the global indexes this project uses.
pub fn project_view<'a>(config: &Config, project: usize) -> iced::Element<'a, Message>
```

- [ ] **Step 3: Rename `settings_editor::view` to `global_view`**

It has no project half — `default_venv_location` and `uv_source` are global,
and the per-project equivalents are `Project::venv` and `Project::uv_source`,
which Task 4 renders. Signature is otherwise unchanged.

- [ ] **Step 4: Point `preferences::view` at the halves**

```rust
    let panel: iced::Element<'a, Message> = match (section, selected_project) {
        (Section::EnvVars, None) => env_editor::global_view(config, env_drafts),
        (Section::EnvVars, Some(i)) => env_editor::project_view(config, i, env_drafts),
        (Section::Indexes, None) => index_editor::global_view(config, credential_drafts),
        (Section::Indexes, Some(i)) => index_editor::project_view(config, i),
        // ...
    };
```

- [ ] **Step 5: Run the whole suite**

Run: `cargo test --workspace --locked`
Expected: PASS, with the **same count** as before this task. A view-only split
that changes the test count moved logic, not just markup.

- [ ] **Step 6: Commit**

```bash
git add crates/
git commit -m "refactor(gui): split the absorbed editors by scope"
```

---

### Task 4: The Python, uv and Terminal sections, and the guard they make reachable

**Files:**
- Modify: `crates/bombadil-gui/src/preferences.rs`, `crates/bombadil-gui/src/app.rs`

**Interfaces:**
- Produces: `Message::{PreferencesPythonPinChanged(Scope, String), PreferencesPythonUnpinned(Scope), PreferencesInheritToggled(Scope, Section), PreferencesUvSourceOverridden(Scope, UvSource), PreferencesTerminalOverridden(Scope, TerminalChoice)}`; `preferences::inherited_label(section, config, project) -> String`.

> **This task makes Plan A's destructive path reachable for the first time.** Until now nothing could set a pin, so `sync_would_delete` and `ConfirmPinChange` have never fired from a real press. The last test below is the one that matters: setting a pin and pressing Sync must reach the confirmation, not the sync.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn setting_a_global_pin_writes_it_through_and_persists() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let persist = Arc::new(FakeConfigPersist::default());
        app.persist = persist.clone();

        let _ = update(&mut app, Message::PreferencesOpened, unused_runner());
        let _ = update(
            &mut app,
            Message::PreferencesPythonPinChanged(preferences::Scope::Global, "3.12".into()),
            unused_runner(),
        );

        assert_eq!(
            app.config.settings.python,
            PythonPin::Version("3.12".into())
        );
        assert_eq!(persist.saved().len(), 1, "a setting change must survive a restart");
    }

    #[test]
    fn a_project_pin_overrides_the_global_one_without_touching_it() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            settings: Settings {
                python: PythonPin::Version("3.12".into()),
                ..Settings::default()
            },
            ..Config::default()
        });
        let id = app.config.projects[0].id;

        let _ = update(&mut app, Message::PreferencesOpened, unused_runner());
        let _ = update(
            &mut app,
            Message::PreferencesPythonPinChanged(preferences::Scope::Project(id), "3.11".into()),
            unused_runner(),
        );

        assert_eq!(
            app.config.projects[0].python,
            Override::Set(PythonPin::Version("3.11".into()))
        );
        assert_eq!(
            app.config.settings.python,
            PythonPin::Version("3.12".into()),
            "a project override must not write into the global value"
        );
    }

    #[test]
    fn toggling_inherit_back_on_discards_the_override_not_the_global() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            settings: Settings {
                python: PythonPin::Version("3.12".into()),
                ..Settings::default()
            },
            ..Config::default()
        });
        let id = app.config.projects[0].id;
        app.config.projects[0].python = Override::Set(PythonPin::Version("3.11".into()));

        let _ = update(&mut app, Message::PreferencesOpened, unused_runner());
        let _ = update(
            &mut app,
            Message::PreferencesInheritToggled(
                preferences::Scope::Project(id),
                preferences::Section::Python,
            ),
            unused_runner(),
        );

        assert_eq!(app.config.projects[0].python, Override::Inherit);
        assert_eq!(app.config.settings.python, PythonPin::Version("3.12".into()));
    }

    #[test]
    fn an_edit_in_a_scope_whose_project_is_gone_writes_nothing() {
        // Preferences can be open while the project it is showing is removed.
        // The edit must land nowhere rather than in whichever project took its
        // place -- the same class of bug `ConfirmRecreate` carries a `Uuid` for.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let gone = uuid::Uuid::from_u128(99);

        let _ = update(&mut app, Message::PreferencesOpened, unused_runner());
        let _ = update(
            &mut app,
            Message::PreferencesPythonPinChanged(preferences::Scope::Project(gone), "3.11".into()),
            unused_runner(),
        );

        assert_eq!(app.config.projects[0].python, Override::Inherit);
        assert_eq!(app.config.settings.python, PythonPin::Unpinned);
    }

    #[test]
    fn a_pin_set_here_reaches_the_confirmation_when_sync_would_delete() {
        // The end-to-end proof that Plan A's guard is wired to something a
        // user can actually reach. Until this task nothing could set a pin, so
        // `sync_would_delete` had never fired from a real press.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let id = app.config.projects[0].id;
        app.entries[0].python_version = Some("3.13.14".to_string());

        let _ = update(&mut app, Message::PreferencesOpened, unused_runner());
        let _ = update(
            &mut app,
            Message::PreferencesPythonPinChanged(preferences::Scope::Project(id), "3.12".into()),
            unused_runner(),
        );
        let task = update(&mut app, Message::SyncRequested(0), unused_runner());

        assert_eq!(task.units(), 0, "nothing may run before the user answers");
        let confirm = app
            .confirm_pin_change
            .as_ref()
            .expect("a pin set in Preferences must reach the same guard");
        assert_eq!(confirm.from, "3.13.14");
        assert_eq!(confirm.to, "3.12");
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p bombadil-gui --offline preferences`
Expected: FAIL to compile — the messages do not exist.

- [ ] **Step 3: Write the update arms**

Every arm resolves the scope against the *current* config and does nothing when
it does not resolve, then persists through the same helper the other editors
use:

```rust
        Message::PreferencesPythonPinChanged(scope, version) => {
            // An empty box is not a pin. Typing and clearing must not leave a
            // `Version("")` that uv would refuse with a confusing error.
            let pin = if version.trim().is_empty() {
                PythonPin::Unpinned
            } else {
                PythonPin::Version(version.trim().to_string())
            };
            match preferences::resolve_scope(scope, &app.config) {
                preferences::Scope::Global => app.config.settings.python = pin,
                preferences::Scope::Project(id) => {
                    let Some(project) = app.config.projects.iter_mut().find(|p| p.id == id) else {
                        return Task::none();
                    };
                    project.python = Override::Set(pin);
                }
            }
            return persist_env_edit(app);
        }
```

`PreferencesInheritToggled` sets the field back to `Override::Inherit` for a
project scope and is a no-op for `Global`, which has nothing to inherit from.
`PreferencesUvSourceOverridden` and `PreferencesTerminalOverridden` follow the
same shape.

Note: `resolve_scope` answers `Global` for a project that is gone, so the
guard above is belt and braces — matched explicitly rather than relying on
that, because an edit silently landing on the global settings would be worse
than one landing nowhere.

- [ ] **Step 4: Write the three section panels**

Each project-scope field is an "Inherit from global" checkbox above the
control, and the inherited value is shown beside it in `slate` when
inheriting, so the user can see what they are about to replace:

```rust
/// What a project would get if it inherited this section, as words.
///
/// Shown beside the inherit control. A checkbox saying "Inherit from global"
/// with no indication of what that *is* makes the user close Preferences,
/// switch scope, read the value, and come back.
pub fn inherited_label(section: Section, config: &Config, project: &Project) -> String
```

The Python panel's control is a text input (uv accepts `3.12` and `3.12.13`
alike; see `interpreter::satisfies_pin`) plus an "Unpin" button. The uv panel
reuses `settings_editor::UvSourceChoice`. The Terminal panel offers the kinds
`terminal::detect` found, keyed by the host OS.

**The Python panel must state what changing a pin does**, in the panel, not
only in the confirmation: uv deletes and rebuilds an environment whose version
differs. The confirmation is the last line of defence, not the only warning.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p bombadil-gui --offline`
Expected: PASS.

- [ ] **Step 6: Prove the scope guard discriminates**

Change `PreferencesPythonPinChanged`'s project arm to write to
`app.config.settings.python` instead.
Run: `cargo test -p bombadil-gui --offline preferences`
Expected: FAIL — `a_project_pin_overrides_the_global_one_without_touching_it`
and `an_edit_in_a_scope_whose_project_is_gone_writes_nothing`. Restore it.

- [ ] **Step 7: Commit**

```bash
git add crates/
git commit -m "feat(gui): the Python, uv and Terminal sections"
```

---

### Task 5: Remove the absorbed tabs

**Files:**
- Modify: `crates/bombadil-gui/src/app.rs`

- [ ] **Step 1: Cut the three variants**

`Tab` becomes:

```rust
pub enum Tab {
    #[default]
    Dependencies,
    Members,
    Scripts,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Dependencies, Tab::Members, Tab::Scripts];
```

Delete `Tab::EnvVars`, `Tab::Indexes` and `Tab::Settings`, their `label` arms,
and their arms in `view`'s `pane` match. Scripts stays.

- [ ] **Step 2: Fix what the compiler names**

Any test asserting on the removed variants moves to the equivalent Preferences
message, or is deleted if it was only asserting the tab existed.

- [ ] **Step 3: Run the whole check set**

Run: `just ci`
Expected: `all clean`.

- [ ] **Step 4: Confirm by hand, in the running application**

Run: `cargo run -p bombadil-gui`

Check, and report what you saw rather than that you ran it:
1. Help > Preferences opens on Global.
2. The scope switch lists every project; switching keeps the section.
3. Every section renders in both scopes.
4. A project field shows what it inherits before you override it.
5. Setting a pin that differs from an existing venv, then pressing Sync,
   reaches the confirmation and **not** the sync.
6. Cancelling it deletes nothing.

- [ ] **Step 5: Commit**

```bash
git add crates/
git commit -m "feat(gui): remove the tabs Preferences absorbed"
```

---

## Done when

- `just ci` passes on Linux, macOS and Windows.
- Every setting is reachable from Preferences in both scopes.
- A project field shows its inherited value before it is overridden.
- A pin set in Preferences reaches `ConfirmPinChange` on the next Sync, proven
  by a test, and confirmed by hand once in the running application.
- The Env vars, Indexes and Settings tabs are gone; Dependencies, Members and
  Scripts remain.

## What this does not do

- **No per-project index primary.** `Project::primary_index` was deliberately
  not added in Plan A and is not added here; it needs its own decision about
  what "primary" means when two projects disagree.
- **No search across settings.** Six sections do not need one.
- **No import or export of preferences.**
