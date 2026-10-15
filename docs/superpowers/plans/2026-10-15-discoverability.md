# Discoverability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keyboard shortcuts, a shortcuts card, hover row actions and tooltips, so what Bombadil can do is visible.

**Architecture:** A new `shortcuts.rs` owns the key table and the pure key mapping; `app.rs` owns the gating (`shortcut_message`) because it reads `App`. One `event::listen_with` subscription feeds `Message::Shortcut`. Views gain a shared `theme::tip` and iced's stateless `hover` widget; no new App state.

**Tech Stack:** Rust, iced 0.14 (`event::listen_with`, `keyboard`, `widget::hover`, `widget::tooltip`), Lucide 1.49.0 path data.

**Spec:** `docs/superpowers/specs/2026-10-15-discoverability-design.md`

## Global Constraints

- Do not commit (the user's standing instruction). Each task ends with the verification command instead of a commit.
- Verification: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings -A clippy::chunks_exact_to_as_chunks && cargo test --workspace`.
- `view` code is untested by convention; logic lives in tested free functions.
- Icons copied verbatim from `lucide-static@1.49.0` (pencil, ellipsis — markup below).
- "Ctrl" is `Modifiers::command()`; labels say `⌘` under `cfg(target_os = "macos")`.

## Review Focus

1. A shortcut typed into a focused text field (e.g. Ctrl+Enter in the version input, F5 in Preferences) must not fire behind it — captured events are dropped, except Esc.
2. Esc with several layers must close only the top one (`modal` over `preferences`, a confirm over the context menu).
3. Ctrl+Enter/F5 while a command runs must not start a second uv command (gated by `can_start`, and `SyncRequested` also refuses).
4. A shortcut naming the selected project after that project was removed (`selected` stale) must do nothing, not panic — handlers already guard `i < len`; `shortcut_message` must not index.
5. The hover pill must not swallow a click on the row beneath its empty area — the pill is only as wide as its buttons.

---

### Task 1: `shortcuts.rs` — the key table and key mapping

**Files:**
- Create: `crates/bombadil-gui/src/shortcuts.rs`
- Modify: `crates/bombadil-gui/src/lib.rs` (add `pub mod shortcuts;`)

**Interfaces:**
- Produces: `pub enum Shortcut { AddProject, Sync, Terminal, Refresh, Escape }` (Debug, Clone, Copy, PartialEq, Eq); `pub const MOD: &str`; `pub const SHORTCUTS: [(Shortcut, &str, &str); 5]`; `pub fn keys(s: Shortcut) -> &'static str`; `pub fn shortcut(key: &Key, modifiers: Modifiers, status: Status) -> Option<Shortcut>`.

- [ ] **Step 1: Write failing tests** in `shortcuts.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use iced::event::Status;
    use iced::keyboard::{Key, Modifiers, key::Named};

    fn ch(c: &str) -> Key { Key::Character(c.into()) }

    #[test]
    fn each_key_maps_to_its_shortcut() {
        let cmd = Modifiers::COMMAND;
        assert_eq!(shortcut(&ch("n"), cmd, Status::Ignored), Some(Shortcut::AddProject));
        assert_eq!(shortcut(&ch("N"), cmd | Modifiers::SHIFT, Status::Ignored), None);
        assert_eq!(shortcut(&Key::Named(Named::Enter), cmd, Status::Ignored), Some(Shortcut::Sync));
        assert_eq!(shortcut(&ch("t"), cmd, Status::Ignored), Some(Shortcut::Terminal));
        assert_eq!(shortcut(&Key::Named(Named::F5), Modifiers::empty(), Status::Ignored), Some(Shortcut::Refresh));
        assert_eq!(shortcut(&Key::Named(Named::Escape), Modifiers::empty(), Status::Ignored), Some(Shortcut::Escape));
    }

    #[test]
    fn letters_need_the_command_modifier() {
        assert_eq!(shortcut(&ch("n"), Modifiers::empty(), Status::Ignored), None);
        assert_eq!(shortcut(&Key::Named(Named::Enter), Modifiers::empty(), Status::Ignored), None);
    }

    #[test]
    fn a_key_a_field_took_is_not_a_shortcut_except_escape() {
        let cmd = Modifiers::COMMAND;
        assert_eq!(shortcut(&Key::Named(Named::Enter), cmd, Status::Captured), None);
        assert_eq!(shortcut(&Key::Named(Named::F5), Modifiers::empty(), Status::Captured), None);
        assert_eq!(
            shortcut(&Key::Named(Named::Escape), Modifiers::empty(), Status::Captured),
            Some(Shortcut::Escape)
        );
    }

    #[test]
    fn the_table_lists_every_shortcut_once() {
        for s in [Shortcut::AddProject, Shortcut::Sync, Shortcut::Terminal, Shortcut::Refresh, Shortcut::Escape] {
            assert_eq!(SHORTCUTS.iter().filter(|(t, _, _)| *t == s).count(), 1, "{s:?}");
        }
        assert_eq!(keys(Shortcut::Sync), format!("{MOD}+Enter"));
    }
}
```

`Modifiers::COMMAND` is `CTRL` on Linux/Windows and `LOGO` on macOS, so the test holds on every CI platform.

- [ ] **Step 2: Run** `cargo test -p bombadil-gui shortcuts` — Expected: compile FAIL (items missing).

- [ ] **Step 3: Implement**

```rust
//! Bombadil's keyboard shortcuts: which keys, and what each is called.
//! Whether one may run right now is `app::shortcut_message`'s call, since
//! that reads the application state.

use iced::event::Status;
use iced::keyboard::{Key, Modifiers, key::Named};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut { AddProject, Sync, Terminal, Refresh, Escape }

/// The modifier as the platform names it.
pub const MOD: &str = if cfg!(target_os = "macos") { "\u{2318}" } else { "Ctrl" };

macro_rules! with_mod { ($k:literal) => {
    if cfg!(target_os = "macos") { concat!("\u{2318}+", $k) } else { concat!("Ctrl+", $k) }
}; }

/// The one table: the shortcuts card and every tooltip read it.
pub const SHORTCUTS: [(Shortcut, &str, &str); 5] = [
    (Shortcut::AddProject, with_mod!("N"), "Add a project"),
    (Shortcut::Sync, with_mod!("Enter"), "Sync the selected project"),
    (Shortcut::Terminal, with_mod!("T"), "Open a terminal for the selected project"),
    (Shortcut::Refresh, "F5", "Re-read the selected project from disk"),
    (Shortcut::Escape, "Esc", "Close whatever is on top"),
];

pub fn keys(which: Shortcut) -> &'static str {
    SHORTCUTS.iter().find(|(s, _, _)| *s == which).map_or("", |(_, k, _)| k)
}

/// The shortcut `key` is, if any. A key a widget already handled (typing in
/// a field) is the widget's -- except Esc, which a focused field takes only
/// to unfocus itself; passing it on closes the edit in one press, not two.
pub fn shortcut(key: &Key, modifiers: Modifiers, status: Status) -> Option<Shortcut> {
    if let Key::Named(Named::Escape) = key {
        return Some(Shortcut::Escape);
    }
    if status == Status::Captured {
        return None;
    }
    let only_command = modifiers.command() && !modifiers.shift() && !modifiers.alt();
    match key {
        Key::Named(Named::F5) if modifiers.is_empty() => Some(Shortcut::Refresh),
        Key::Named(Named::Enter) if only_command => Some(Shortcut::Sync),
        Key::Character(c) if only_command && c.as_str() == "n" => Some(Shortcut::AddProject),
        Key::Character(c) if only_command && c.as_str() == "t" => Some(Shortcut::Terminal),
        _ => None,
    }
}
```

- [ ] **Step 4: Run** `cargo test -p bombadil-gui shortcuts` — Expected: 4 passed. (Clippy dead-code on unused pub items is not an issue: they are `pub` in a lib.)

---

### Task 2: Gating, Esc order, `RefreshRequested`, the subscription

**Files:**
- Modify: `crates/bombadil-gui/src/app.rs` (Message variants, `update` arms, `shortcut_message`, `overlay_open`, tests)
- Modify: `crates/bombadil-gui/src/lib.rs` (subscription + `key_event`)

**Interfaces:**
- Consumes: `shortcuts::{Shortcut, shortcut}`.
- Produces: `Message::Shortcut(Shortcut)`, `Message::RefreshRequested`, `pub fn shortcut_message(app: &App, s: Shortcut) -> Option<Message>`.

- [ ] **Step 1: Failing tests** (in `app.rs` tests):

```rust
#[test]
fn shortcuts_name_the_selected_project_when_allowed() {
    use crate::shortcuts::Shortcut;
    let mut app = App::new(Config { projects: vec![project("api")], ..Config::default() });
    assert!(shortcut_message(&app, Shortcut::Sync).is_none(), "nothing selected");
    assert!(shortcut_message(&app, Shortcut::Terminal).is_none());
    assert!(shortcut_message(&app, Shortcut::Refresh).is_none());
    assert!(matches!(shortcut_message(&app, Shortcut::AddProject), Some(Message::AddProjectPickManifestRequested)));
    app.selected = Some(0);
    assert!(matches!(shortcut_message(&app, Shortcut::Sync), Some(Message::SyncRequested(0))));
    assert!(matches!(shortcut_message(&app, Shortcut::Terminal), Some(Message::OpenTerminalHereRequested(0))));
    assert!(matches!(shortcut_message(&app, Shortcut::Refresh), Some(Message::RefreshRequested)));
    app.selected = Some(5);
    assert!(shortcut_message(&app, Shortcut::Sync).is_none(), "a stale selection names nothing");
}

#[test]
fn shortcuts_that_start_uv_wait_while_busy() {
    use crate::shortcuts::Shortcut;
    let mut app = App::new(Config { projects: vec![project("api")], ..Config::default() });
    app.selected = Some(0);
    app.busy = Some(crate::busy::Busy::new(uuid::Uuid::nil(), crate::busy::Action::Sync));
    for s in [Shortcut::AddProject, Shortcut::Sync, Shortcut::Refresh] {
        assert!(shortcut_message(&app, s).is_none(), "{s:?}");
    }
    assert!(shortcut_message(&app, Shortcut::Terminal).is_some(), "a terminal starts no uv command");
}

#[test]
fn shortcuts_do_nothing_under_an_overlay() {
    use crate::shortcuts::Shortcut;
    let mut app = App::new(Config { projects: vec![project("api")], ..Config::default() });
    app.selected = Some(0);
    app.open_menu = Some(shell::TopMenu::Help);
    for s in [Shortcut::AddProject, Shortcut::Sync, Shortcut::Terminal, Shortcut::Refresh] {
        assert!(shortcut_message(&app, s).is_none(), "{s:?}");
    }
}

#[test]
fn escape_closes_the_top_layer_only() {
    use crate::shortcuts::Shortcut;
    let mut app = App::new(Config::default());
    assert!(shortcut_message(&app, Shortcut::Escape).is_none());
    app.open_menu = Some(shell::TopMenu::Help);
    app.modal = Some(shell::Modal::About);
    assert!(matches!(shortcut_message(&app, Shortcut::Escape), Some(Message::ModalClosed)));
    app.modal = None;
    assert!(matches!(shortcut_message(&app, Shortcut::Escape), Some(Message::MenuDismissed)));
}

#[test]
fn escape_cancels_a_version_edit() {
    use crate::shortcuts::Shortcut;
    let mut app = App::new(Config::default());
    app.dep_edit = Some(deps::DepEdit { /* fields as the type defines them */ ..test_dep_edit() });
    assert!(matches!(shortcut_message(&app, Shortcut::Escape), Some(Message::DepVersionEditCancelled)));
}

#[test]
fn a_shortcut_runs_what_its_button_runs() {
    let mut app = App::new(Config { projects: vec![project("api")], ..Config::default() });
    app.modal = Some(shell::Modal::About);
    let _ = update(&mut app, Message::Shortcut(crate::shortcuts::Shortcut::Escape), Arc::new(FakeCommandRunner::new()));
    assert_eq!(app.modal, None);
}

#[test]
fn refresh_rereads_the_selected_project() {
    let mut app = App::new(Config { projects: vec![project("api")], ..Config::default() });
    let none = update(&mut app, Message::RefreshRequested, Arc::new(FakeCommandRunner::new()));
    assert_eq!(none.units(), 0);
    app.selected = Some(0);
    let task = update(&mut app, Message::RefreshRequested, Arc::new(FakeCommandRunner::new()));
    assert!(task.units() > 0);
}
```

(`test_dep_edit()`: construct `DepEdit` literally with whatever fields `deps::DepEdit` has — read the struct; no new helper if a literal fits.)

- [ ] **Step 2: Run** `cargo test -p bombadil-gui shortcut escape refresh_rereads` — Expected: compile FAIL.

- [ ] **Step 3: Implement** in `app.rs`:

```rust
// Message enum:
/// A keyboard shortcut, already mapped from its key.
Shortcut(crate::shortcuts::Shortcut),
/// F5: re-read the selected project, as a window focus does, undebounced.
RefreshRequested,

// update():
Message::Shortcut(s) => {
    if let Some(message) = shortcut_message(app, s) {
        return update(app, message, runner);
    }
}
Message::RefreshRequested => {
    if let Some(i) = app.selected.filter(|i| *i < app.config.projects.len()) {
        return Task::batch([reprobe_project(app, i, runner), probe_every_venv(app)]);
    }
}

/// The close message for the top layer, in `view`'s stacking order, top first.
fn escape_message(app: &App) -> Option<Message> {
    if app.modal.is_some() { return Some(Message::ModalClosed); }
    if app.preferences.is_some() { return Some(Message::PreferencesClosed); }
    if app.open_menu.is_some() { return Some(Message::MenuDismissed); }
    if app.confirm_member_sync.is_some() { return Some(Message::MembersSyncCancelled); }
    if app.confirm_environment_removal.is_some() { return Some(Message::EnvironmentRemovalCancelled); }
    if app.confirm_pin_change.is_some() { return Some(Message::PinChangeCancelled); }
    if app.confirm_recreate.is_some() { return Some(Message::RecreateVenvCancelled); }
    if app.context_menu.is_some() { return Some(Message::ContextMenuClosed); }
    if app.add_project.is_some() { return Some(Message::AddProjectCancelled); }
    if app.dep_edit.is_some() { return Some(Message::DepVersionEditCancelled); }
    None
}

/// What `s` does right now, if anything: the gating the spec's table sets.
pub fn shortcut_message(app: &App, s: crate::shortcuts::Shortcut) -> Option<Message> {
    use crate::shortcuts::Shortcut;
    if s == Shortcut::Escape {
        return escape_message(app);
    }
    // Everything Esc would close is something the other keys must not act behind,
    // except the version edit, which is inline.
    let overlay = escape_message(app).is_some_and(|m| !matches!(m, Message::DepVersionEditCancelled));
    if overlay {
        return None;
    }
    let selected = app.selected.filter(|i| *i < app.config.projects.len());
    match s {
        Shortcut::AddProject => can_start(app).then_some(Message::AddProjectPickManifestRequested),
        Shortcut::Sync => selected.filter(|_| can_start(app)).map(Message::SyncRequested),
        Shortcut::Terminal => selected.map(Message::OpenTerminalHereRequested),
        Shortcut::Refresh => selected.filter(|_| can_start(app)).map(|_| Message::RefreshRequested),
        Shortcut::Escape => None,
    }
}
```

Check the names of the confirm cancel messages against the enum (`MembersSyncCancelled`, `EnvironmentRemovalCancelled`, `PinChangeCancelled`, `RecreateVenvCancelled`, `ContextMenuClosed`, `AddProjectCancelled`, `DepVersionEditCancelled`) — verified present at spec time.

In `lib.rs`, add to the batch: `iced::event::listen_with(key_event),` and:

```rust
fn key_event(event: iced::Event, status: iced::event::Status, _id: iced::window::Id) -> Option<app::Message> {
    match event {
        iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, modifiers, .. }) => {
            shortcuts::shortcut(&key, modifiers, status).map(app::Message::Shortcut)
        }
        _ => None,
    }
}
```

- [ ] **Step 4: Run** the verification command — Expected: all green.

---

### Task 3: Tooltips, icons, the shortcuts card, state meanings

**Files:**
- Modify: `theme.rs` (`tip`, `state_meaning`), `icons.rs` (Pencil, Ellipsis, `icon_button` via `tip`, `ALL` → 13), `shell.rs` (`Modal::Shortcuts`, Help entry, body), `detail.rs` (keys in tooltips), `app.rs` (sidebar Add tooltip), `deps.rs`, `members_editor.rs`, `context_menu.rs` (wait tooltips).

**Interfaces:**
- Produces: `pub fn tip<'a, M: 'a>(content: impl Into<Element<'a, M>>, text: impl Into<String>, position: tooltip::Position) -> Element<'a, M>`; `pub fn wait_tip<'a, M: 'a>(content, waiting: Option<&str>) -> Element<'a, M>` (wraps in "Wait for {label} to finish" only when `Some`); `pub fn state_meaning(state: State) -> &'static str`; `Icon::Pencil`, `Icon::Ellipsis`; `Modal::Shortcuts`.
- Ruling to make: the views' `enabled: bool` becomes `waiting: Option<&str>` (`None` = enabled), so the label reaches the tooltip with one parameter.

Lucide markup (verbatim from lucide-static@1.49.0):
- pencil: `<path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z"/><path d="m15 5 4 4"/>`
- ellipsis: `<circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/>`

- [ ] **Step 1: Failing tests:**
  - `theme`: `every_state_has_a_meaning` — for each `State` variant (read the enum), `state_meaning` is non-empty and all are distinct.
  - `shell`: `help_lists_the_keyboard_shortcuts` — `TopMenu::Help.entries()` contains `Message::ModalOpened(Modal::Shortcuts)`; extend the existing all-modals-have-titles loop with `Modal::Shortcuts`.
  - `icons`: existing parse-every-icon test covers Pencil/Ellipsis once `ALL` grows.
- [ ] **Step 2: Run** — Expected: compile FAIL.
- [ ] **Step 3: Implement**: `tip` (body of today's `icon_button` tooltip), `icon_button` calls it; `wait_tip`; `state_meaning`; icons; `Modal::Shortcuts` title "Keyboard shortcuts", body = `SHORTCUTS` rows (keys in `FONT_DATA`, fixed-width column, description in prose); Help entry "Keyboard shortcuts" first. Tooltip texts: header terminal `"Open a terminal here, with the environment activated (Ctrl+T)"` built with `format!("… ({})", shortcuts::keys(Shortcut::Terminal))` — `icon_button`'s tip parameter becomes `impl Into<String>`; header Sync idle gains a tip "Sync ({keys})"; sidebar "Add" gets `tip(…, "Add a project (Ctrl+N)")` and becomes disabled with `wait_tip` while busy. Swap `enabled` → `waiting` in deps toolbar/version cell/members/context menu, wrapping each uv-starting button in `wait_tip`. Sidebar glyph wrapped in `tip(glyph, state_meaning(state(entry)), Right)`.
- [ ] **Step 4: Run** verification — Expected: green.

---

### Task 4: Hover row actions

**Files:**
- Modify: `sidebar.rs` (`view` signature gains `waiting: Option<&str>`; hover pill), `deps.rs` (pencil on version cell), `app.rs` (pass `waiting`).

- [ ] **Step 1:** No new pure logic: this task is view-only, untested by convention (state it in the ledger).
- [ ] **Step 2: Implement** sidebar: build `actions` = `row![sync, terminal, more]` of `icons::icon_button` (Sync gets `None` + `wait_tip` while waiting), inside a `container(...).style(theme::panel-like bark pill)`, right-aligned with `container(actions).width(Fill).align_x(Right)`. Row element = `if is_selected { stack![row, actions] } else { hover(row, actions) }`. More → `Message::ContextMenuOpened(i)`, Terminal → `OpenTerminalHereRequested(i)`, Sync → `SyncRequested(i)`.
  deps: non-editing version cell becomes `hover(mouse_area(version).on_double_click(...), pencil)` where pencil = right-aligned `icon_button(Icon::Pencil, "Change version (or double-click)", waiting.is_none().then(|| DepVersionEditStarted{..}))`; when waiting, no pencil (plain cell).
- [ ] **Step 3: Run** verification — Expected: green. Smoke-run `timeout 8 cargo run -p bombadil` — Expected: exit 124, no panic.
