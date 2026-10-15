# Feedback and Liveness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the one running uv command visible (spinner, disabled buttons, live status), report every finish as a toast, and refresh the selected project when the window regains focus.

**Architecture:** A typed `busy::Busy` replaces `App.syncing`/`App.dep_install`. Toasts live in `App.toasts`. `iced::window::frames()` drives a `Tick` only while busy or a toast shows; `iced::window::events()` delivers `Focused`. Decisions are pure functions in `busy.rs`, tested; views read them.

**Tech Stack:** Rust 2024, iced 0.14 (`svg` feature already on), Lucide icons from `lucide-static` 1.49.0.

**Spec:** `docs/superpowers/specs/2026-10-15-feedback-liveness-design.md`

## Global Constraints

- **Do not commit.** End each task with the verification command; leave changes in the working tree.
- One uv command at a time. Never allow a second to start while `busy` is `Some`.
- Success toast auto-dismisses after **4 s**; failure toasts never expire. At most **3** toasts, newest first.
- Focus refresh debounce: **2 s**. Skipped while busy.
- Spinner: one turn per second.
- Frames subscribed only when `busy.is_some() || !toasts.is_empty()`.
- `view` stays untested by convention; logic in tested free functions.
- Verification command for every task: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings -A clippy::chunks_exact_to_as_chunks && cargo test --workspace`.

## Review Focus

1. **A command that never reaches its finish message** (job panic/timeout). Expected: `busy` still clears and a failure toast appears. The finish paths already funnel timeouts into `SyncFinished`/`AddProjectConfirmFinished`; Task 2 adds `a_timed_out_sync_releases_busy_and_says_so`.
2. **Project removed while its command runs.** Expected: the label degrades to the action alone, finish still clears. Task 1 `label_without_a_project_name`.
3. **Focus storm** (alt-tab repeatedly). Expected: at most one re-probe per 2 s. Task 6 `should_refresh_on_focus` cases.
4. **Toast flood** (many quick finishes). Expected: never more than 3. Task 3 `toasts_are_capped_at_three`.
5. **Failure toast disappearing before it is read.** Expected: never expires. Task 4 `tick_never_expires_a_failure`.

---

### Task 1: The `busy` module

**Files:**
- Create: `crates/bombadil-gui/src/busy.rs`
- Modify: `crates/bombadil-gui/src/lib.rs` (`mod busy;`)

**Interfaces:**
- Produces:
  - `pub enum Action { Sync, InstallVersion(String), AddProject, PinChange, Recreate, MembersSync }` (`Debug, Clone, PartialEq, Eq`)
  - `pub struct Busy { pub project_id: uuid::Uuid, pub action: Action, pub started: std::time::Instant }` (`Debug, Clone`) with `pub fn new(project_id, action) -> Self` (started = `Instant::now()`)
  - `pub fn label(&self, project_label: Option<&str>, now: Instant) -> String`
  - `impl Action { pub fn done(&self, project_label: Option<&str>) -> String; pub fn failed(&self, project_label: Option<&str>) -> String }`

- [ ] **Step 1: Write the failing tests** (in `busy.rs`, `#[cfg(test)] mod tests`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn busy(action: Action) -> Busy {
        Busy::new(uuid::Uuid::nil(), action)
    }

    #[test]
    fn label_names_the_action_and_project_and_counts_seconds() {
        let b = busy(Action::Sync);
        assert_eq!(b.label(Some("api"), b.started), "Syncing api…");
        assert_eq!(
            b.label(Some("api"), b.started + Duration::from_millis(12_400)),
            "Syncing api… 12s"
        );
    }

    #[test]
    fn every_action_has_its_own_wording() {
        let now_of = |a: Action| {
            let b = busy(a);
            b.label(Some("api"), b.started)
        };
        assert_eq!(now_of(Action::InstallVersion("fastapi==0.110".into())), "Installing fastapi==0.110…");
        assert_eq!(now_of(Action::AddProject), "Setting up api…");
        assert_eq!(now_of(Action::PinChange), "Rebuilding api…");
        assert_eq!(now_of(Action::Recreate), "Recreating api…");
        assert_eq!(now_of(Action::MembersSync), "Syncing members of api…");
    }

    #[test]
    fn label_without_a_project_name() {
        // The project was removed while its command ran.
        let b = busy(Action::Sync);
        assert_eq!(b.label(None, b.started), "Syncing…");
    }

    #[test]
    fn finish_sentences() {
        assert_eq!(Action::Sync.done(Some("api")), "Synced api");
        assert_eq!(Action::Sync.failed(Some("api")), "Could not sync api");
        let install = Action::InstallVersion("fastapi==0.110".into());
        assert_eq!(install.done(Some("api")), "Installed fastapi==0.110");
        assert_eq!(install.failed(None), "Could not install fastapi==0.110");
        assert_eq!(Action::Recreate.done(Some("api")), "Recreated api");
        assert_eq!(Action::AddProject.done(Some("api")), "Added api");
        assert_eq!(Action::PinChange.failed(Some("api")), "Could not rebuild api");
        assert_eq!(Action::MembersSync.done(Some("api")), "Synced members of api");
    }
}
```

- [ ] **Step 2: Run** `cargo test -p bombadil-gui busy::` — Expected: compile errors (`Busy`, `Action` not found). Add `mod busy;` to `lib.rs` first so the module is compiled.

- [ ] **Step 3: Implement** above the tests:

```rust
//! The one uv command that may run at a time, and the words for it.
//!
//! Pure on purpose: `app::update` decides, views render, and everything worth
//! a test is here.

use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Sync,
    /// The `name==version` requirement being installed.
    InstallVersion(String),
    /// The first sync of a project just added.
    AddProject,
    /// A sync that rebuilds the venv on a different interpreter.
    PinChange,
    Recreate,
    MembersSync,
}

#[derive(Debug, Clone)]
pub struct Busy {
    pub project_id: uuid::Uuid,
    pub action: Action,
    pub started: Instant,
}

impl Busy {
    pub fn new(project_id: uuid::Uuid, action: Action) -> Self {
        Self { project_id, action, started: Instant::now() }
    }

    /// "Syncing api… 12s". Seconds appear from the first whole second.
    pub fn label(&self, project_label: Option<&str>, now: Instant) -> String {
        let secs = now.saturating_duration_since(self.started).as_secs();
        let base = self.action.present(project_label);
        if secs == 0 { format!("{base}…") } else { format!("{base}… {secs}s") }
    }
}

/// `" api"` or nothing, so a missing project degrades the sentence instead of
/// breaking it.
fn named(project_label: Option<&str>) -> String {
    project_label.map(|l| format!(" {l}")).unwrap_or_default()
}

impl Action {
    fn present(&self, p: Option<&str>) -> String {
        let n = named(p);
        match self {
            Action::Sync => format!("Syncing{n}"),
            Action::InstallVersion(req) => format!("Installing {req}"),
            Action::AddProject => format!("Setting up{n}"),
            Action::PinChange => format!("Rebuilding{n}"),
            Action::Recreate => format!("Recreating{n}"),
            Action::MembersSync => match p {
                Some(l) => format!("Syncing members of {l}"),
                None => "Syncing members".to_string(),
            },
        }
    }

    pub fn done(&self, p: Option<&str>) -> String {
        let n = named(p);
        match self {
            Action::Sync => format!("Synced{n}"),
            Action::InstallVersion(req) => format!("Installed {req}"),
            Action::AddProject => format!("Added{n}"),
            Action::PinChange => format!("Rebuilt{n}"),
            Action::Recreate => format!("Recreated{n}"),
            Action::MembersSync => match p {
                Some(l) => format!("Synced members of {l}"),
                None => "Synced members".to_string(),
            },
        }
    }

    pub fn failed(&self, p: Option<&str>) -> String {
        let n = named(p);
        match self {
            Action::Sync => format!("Could not sync{n}"),
            Action::InstallVersion(req) => format!("Could not install {req}"),
            Action::AddProject => format!("Could not set up{n}"),
            Action::PinChange => format!("Could not rebuild{n}"),
            Action::Recreate => format!("Could not recreate{n}"),
            Action::MembersSync => match p {
                Some(l) => format!("Could not sync members of {l}"),
                None => "Could not sync members".to_string(),
            },
        }
    }
}
```

- [ ] **Step 4: Run** `cargo test -p bombadil-gui busy::` — Expected: PASS. (Clippy will flag dead code until Task 2; that gate runs at Task 2's end.)

---

### Task 2: `App.busy` replaces `syncing` and `dep_install`

**Files:**
- Modify: `crates/bombadil-gui/src/app.rs`

**Interfaces:**
- Consumes: `busy::{Busy, Action}` (Task 1).
- Produces: `App.busy: Option<busy::Busy>`; `pub fn can_start(app: &App) -> bool` (= `app.busy.is_none()`); `pub fn project_label(app: &App, id: uuid::Uuid) -> Option<&str>`. `App.syncing` and `App.dep_install` no longer exist.

- [ ] **Step 1: Write the failing tests** in `app.rs` tests:

```rust
    fn busy_project(app: &App) -> Option<String> {
        app.busy.as_ref().map(|b| b.project_id.to_string())
    }

    #[test]
    fn a_running_command_blocks_a_second_one() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let fake = Arc::new(FakeCommandRunner::new());
        let _ = update(&mut app, Message::SyncRequested(0), fake.clone());
        assert!(!can_start(&app));
        assert_eq!(app.busy.as_ref().map(|b| b.action.clone()), Some(crate::busy::Action::Sync));
        let second = update(&mut app, Message::SyncRequested(0), fake.clone());
        assert_eq!(second.units(), 0, "a second sync must not be dispatched");
    }

    #[test]
    fn a_timed_out_sync_releases_busy_and_says_so() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.busy = Some(crate::busy::Busy::new(app.config.projects[0].id, crate::busy::Action::Sync));
        let _ = update(
            &mut app,
            Message::SyncFinished { success: false, lines: vec!["sync timed out".into()] },
            unused_runner(),
        );
        assert!(can_start(&app));
        assert!(app.drawer.status.contains("Could not sync api"), "{}", app.drawer.status);
    }
```

- [ ] **Step 2: Run** `cargo test -p bombadil-gui a_running_command` — Expected: compile errors (`busy` field, `can_start`).

- [ ] **Step 3: Implement.**
- Struct: replace `pub syncing: Option<String>` (keep its doc, reworded to "the one uv command running, if any") with `pub busy: Option<crate::busy::Busy>`; delete `dep_install`. In `App::new`: `busy: None`, delete `dep_install: None`.
- Every guard: `app.syncing.is_none()` → `can_start(app)`; `app.syncing.is_some()` → `!can_start(app)`; `app.syncing = None` → `app.busy = None`.
- Every claim, with its action:
  - `SyncRequested`: `app.busy = Some(Busy::new(project.id, Action::Sync));`
  - `DepVersionSubmitted`: `Action::InstallVersion(requirement.clone())`; delete the `app.dep_install = ...` line.
  - `AddProjectConfirmed`: `Action::AddProject`
  - `PinChangeConfirmed`: `Action::PinChange`
  - `RecreateVenvConfirmed`: `Action::Recreate`
  - `dispatch_members_sync`: `Action::MembersSync`
- Add near `sync_target`:

```rust
/// Whether a new uv command may start. The one rule every uv-starting
/// handler and button follows.
pub fn can_start(app: &App) -> bool {
    app.busy.is_none()
}

/// The label of the project with `id`, if it still exists.
pub fn project_label(app: &App, id: uuid::Uuid) -> Option<&str> {
    app.config.projects.iter().find(|p| p.id == id).map(|p| p.label.as_str())
}
```

- `SyncFinished`: replace the `match (app.dep_install.take(), success)` status block with:

```rust
            let finished = app.busy.take();
            let status = match &finished {
                Some(b) => {
                    let name = project_label(app, b.project_id);
                    if success { b.action.done(name) } else { b.action.failed(name) }
                }
                None if success => "sync succeeded".to_string(),
                None => "sync failed".to_string(),
            };
```

  and remove the earlier `app.busy = None;` line at the top of the arm (the `take()` does it). Keep `drawer::set_idle(&mut app.drawer, status)`. Keep, for now, the `app.drawer.expanded = true` on a failed `InstallVersion` (Task 3 replaces it with a toast): `if !success && matches!(finished.as_ref().map(|b| &b.action), Some(Action::InstallVersion(_))) { app.drawer.expanded = true; }`.
- `AddProjectConfirmFinished`: `app.busy = None;` stays as the release.
- Tests: `app.syncing.as_deref()` → `busy_project(&app).as_deref()`; `app.syncing.is_none()/is_some()` → `app.busy.is_none()/is_some()`; the `dep_install` test (`a_version_uv_cannot_install_says_which_and_opens_the_drawer`) sets `app.busy = Some(Busy::new(Uuid::nil(), Action::InstallVersion("fastapi==0.1.0".into())))` instead, asserts `app.busy.is_none()` after, and its status assertion becomes `contains("Could not install fastapi==0.1.0")`.
- Any test asserting the old status strings `"sync succeeded"`/`"sync failed"` after a claimed sync now expects the action sentence (`"Synced api"` / `"Could not sync api"`); update the expectation and keep the test's intent.

- [ ] **Step 4: Run** `cargo test -p bombadil-gui` — Expected: PASS.
- [ ] **Step 5: Full verification.** Expected: green (Task 1's dead-code warnings now gone).

---

### Task 3: Toasts

**Files:**
- Modify: `crates/bombadil-gui/src/busy.rs` (toast model), `crates/bombadil-gui/src/app.rs` (state, messages, push on finish), `crates/bombadil-gui/src/icons.rs` (`Alert`, `X`)

**Interfaces:**
- Consumes: Task 2's finish handlers.
- Produces:
  - `busy.rs`: `pub enum ToastKind { Success, Failure }`; `pub struct Toast { pub kind: ToastKind, pub text: String, pub expires: Option<Instant> }`; `pub const SUCCESS_TOAST: Duration = 4 s`; `pub const MAX_TOASTS: usize = 3`; `pub fn push(toasts: &mut Vec<Toast>, kind: ToastKind, text: String, now: Instant)`.
  - `app.rs`: `App.toasts: Vec<busy::Toast>`; `Message::ToastDismissed(usize)`, `Message::ShowOutput`.
  - `icons.rs`: `Icon::Alert`, `Icon::X`.

- [ ] **Step 1: Failing tests** in `busy.rs`:

```rust
    #[test]
    fn a_success_toast_expires_and_a_failure_does_not() {
        let now = Instant::now();
        let mut t = Vec::new();
        push(&mut t, ToastKind::Success, "Synced api".into(), now);
        push(&mut t, ToastKind::Failure, "Could not sync api".into(), now);
        assert_eq!(t[0].kind, ToastKind::Failure, "newest first");
        assert_eq!(t[0].expires, None);
        assert_eq!(t[1].expires, Some(now + SUCCESS_TOAST));
    }

    #[test]
    fn toasts_are_capped_at_three() {
        let now = Instant::now();
        let mut t = Vec::new();
        for i in 0..5 {
            push(&mut t, ToastKind::Success, format!("{i}"), now);
        }
        assert_eq!(t.len(), MAX_TOASTS);
        assert_eq!(t[0].text, "4", "the oldest are the ones dropped");
    }
```

and in `app.rs`:

```rust
    #[test]
    fn a_finished_command_raises_one_toast_and_no_longer_forces_the_drawer_open() {
        let mut app = App::new(Config { projects: vec![project("api")], ..Config::default() });
        app.drawer.expanded = false;
        app.busy = Some(crate::busy::Busy::new(
            app.config.projects[0].id,
            crate::busy::Action::InstallVersion("fastapi==0.1.0".into()),
        ));
        let _ = update(
            &mut app,
            Message::SyncFinished { success: false, lines: vec!["No solution found".into()] },
            unused_runner(),
        );
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(app.toasts[0].kind, crate::busy::ToastKind::Failure);
        assert_eq!(app.toasts[0].text, "Could not install fastapi==0.1.0");
        assert!(!app.drawer.expanded, "Show output opens it, the failure does not");
        let _ = update(&mut app, Message::ShowOutput, unused_runner());
        assert!(app.drawer.expanded);
        let _ = update(&mut app, Message::ToastDismissed(0), unused_runner());
        assert!(app.toasts.is_empty());
    }
```

Also update the earlier test `a_version_uv_cannot_install_says_which_and_opens_the_drawer`: rename to `a_version_uv_cannot_install_says_which` and drop its `drawer.expanded` assertion (superseded by the test above).

- [ ] **Step 2: Run** `cargo test -p bombadil-gui toast` — Expected: compile errors.

- [ ] **Step 3: Implement.** In `busy.rs`:

```rust
use std::time::Duration;

/// How long a success stays on screen. Failures stay until dismissed.
pub const SUCCESS_TOAST: Duration = Duration::from_secs(4);
pub const MAX_TOASTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind { Success, Failure }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    /// `None` until dismissed: a failure must not vanish unread.
    pub expires: Option<Instant>,
}

/// Newest first; the oldest beyond [`MAX_TOASTS`] is dropped.
pub fn push(toasts: &mut Vec<Toast>, kind: ToastKind, text: String, now: Instant) {
    let expires = (kind == ToastKind::Success).then(|| now + SUCCESS_TOAST);
    toasts.insert(0, Toast { kind, text, expires });
    toasts.truncate(MAX_TOASTS);
}
```

In `app.rs`: `pub toasts: Vec<crate::busy::Toast>` (init `Vec::new()`); messages `ToastDismissed(usize)` (`if i < app.toasts.len() { app.toasts.remove(i); }`) and `ShowOutput` (`app.drawer.expanded = true;`). In `SyncFinished`, after computing `status`, when `finished.is_some()`: `busy::push(&mut app.toasts, if success { Success } else { Failure }, status.clone(), Instant::now())`; delete the forced `app.drawer.expanded = true` for `InstallVersion` added in Task 2. In `AddProjectConfirmFinished`, push `Action::AddProject.done/failed(name)` the same way (the project is the one at `index`).

In `icons.rs` add `Alert` and `X` to `Icon`, to `ALL` (now 11), with bodies from `lucide-static` 1.49.0:
- `Alert`: `<path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4"/><path d="M12 17h.01"/>`
- `X`: `<path d="M18 6 6 18"/><path d="m6 6 12 12"/>`
(They are used by Task 5's toast view; until then clippy reports them unused — the gate for this task is the test run; Task 5 runs the full gate.)

- [ ] **Step 4: Run** `cargo test -p bombadil-gui` — Expected: PASS.

---

### Task 4: Tick — spinner and toast expiry

**Files:**
- Modify: `crates/bombadil-gui/src/busy.rs` (`expire`, `spin`), `crates/bombadil-gui/src/app.rs` (`Message::Tick`, `App.spinner`, `fn animating`), `crates/bombadil-gui/src/lib.rs` (subscription)

**Interfaces:**
- Produces: `busy::expire(toasts: &mut Vec<Toast>, now: Instant)`; `busy::spin(started: Instant, now: Instant) -> f32` (radians, one turn per second); `Message::Tick(Instant)`; `App.now: Instant` (last tick, used by views for labels and the spinner); `pub fn animating(app: &App) -> bool`.

- [ ] **Step 1: Failing tests** in `busy.rs`:

```rust
    #[test]
    fn tick_expires_successes_only() {
        let now = Instant::now();
        let mut t = Vec::new();
        push(&mut t, ToastKind::Success, "ok".into(), now);
        push(&mut t, ToastKind::Failure, "bad".into(), now);
        expire(&mut t, now + SUCCESS_TOAST + Duration::from_millis(1));
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].kind, ToastKind::Failure);
    }

    #[test]
    fn tick_never_expires_a_failure() {
        let now = Instant::now();
        let mut t = Vec::new();
        push(&mut t, ToastKind::Failure, "bad".into(), now);
        expire(&mut t, now + Duration::from_secs(3600));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn the_spinner_turns_once_a_second() {
        let start = Instant::now();
        assert_eq!(spin(start, start), 0.0);
        let quarter = spin(start, start + Duration::from_millis(250));
        assert!((quarter - std::f32::consts::FRAC_PI_2).abs() < 1e-3, "{quarter}");
    }
```

and in `app.rs`:

```rust
    #[test]
    fn frames_are_wanted_only_while_something_moves() {
        let mut app = App::new(Config::default());
        assert!(!animating(&app));
        app.busy = Some(crate::busy::Busy::new(uuid::Uuid::nil(), crate::busy::Action::Sync));
        assert!(animating(&app));
        app.busy = None;
        crate::busy::push(&mut app.toasts, crate::busy::ToastKind::Success, "x".into(), std::time::Instant::now());
        assert!(animating(&app));
    }
```

- [ ] **Step 2: Run** `cargo test -p bombadil-gui tick` — Expected: compile errors.

- [ ] **Step 3: Implement.** `busy.rs`:

```rust
/// Drops every toast whose time has passed. Failures have none.
pub fn expire(toasts: &mut Vec<Toast>, now: Instant) {
    toasts.retain(|t| t.expires.is_none_or(|at| now < at));
}

/// The spinner's angle: one full turn per second since `started`.
pub fn spin(started: Instant, now: Instant) -> f32 {
    now.saturating_duration_since(started).as_secs_f32().fract() * std::f32::consts::TAU
}
```

`app.rs`: `pub now: std::time::Instant` (init `Instant::now()`); `Message::Tick(std::time::Instant)` → `app.now = now; busy::expire(&mut app.toasts, now);`;

```rust
/// Whether anything on screen moves: the spinner or a toast's countdown.
/// The frame subscription exists only while this is true.
pub fn animating(app: &App) -> bool {
    app.busy.is_some() || !app.toasts.is_empty()
}
```

`lib.rs` `.subscription(|app: &app::App| ...)`: change the closure parameter from `_` to `state`, and add to the batch:

```rust
            if app::animating(state) {
                iced::window::frames().map(app::Message::Tick)
            } else {
                iced::Subscription::none()
            },
```

- [ ] **Step 4: Run** `cargo test -p bombadil-gui` — Expected: PASS.

---

### Task 5: Busy views and the toast layer

**Files:**
- Modify: `detail.rs` (header Sync), `deps.rs` (toolbar, version cell), `members_editor.rs` (Sync members), `context_menu.rs` (Sync, Recreate entries), `sidebar.rs` (busy row glyph), `drawer.rs` (live label), `app.rs` (view wiring, toast layer), `icons.rs` (`spinner`)

**Interfaces:**
- Consumes: `can_start`, `project_label`, `App.now`, `busy::spin`, `busy::Toast`, `Icon::{Refresh, Alert, X, Check}`.
- Produces: `icons::spinner<'a, M: 'a>(angle: f32, size: f32, swatch: Swatch) -> Element<'a, M>` (`Icon::Refresh` with `.rotation(iced::Radians(angle))`); a `Busy view` input passed to views as `busy: Option<(&busy::Busy, f32)>` where `f32` is the spinner angle, or `can_start: bool` where only enabling matters.

This task is view code: no new unit tests (repo convention). Its gate is the full verification plus a run of the app.

- [ ] **Step 1: `icons::spinner`.**

```rust
/// The refresh icon turned by `angle` radians: the busy indicator.
pub fn spinner<'a, M: 'a>(angle: f32, size: f32, swatch: Swatch) -> iced::Element<'a, M> {
    svg(svg::Handle::from_memory(document(Icon::Refresh).into_bytes()))
        .width(size)
        .height(size)
        .rotation(iced::Radians(angle))
        .style(move |theme, _status| svg::Style {
            color: Some(theme::palette(theme).get(swatch)),
        })
        .into()
}
```

- [ ] **Step 2: Header (`detail.rs`).** `detail::view(header, index)` gains `busy: Option<(String, f32, bool)>` = `(label, angle, is_this_project)`. Sync button:
  - `None` → as today (Refresh icon + "Sync", `.on_press(SyncRequested(index))`).
  - `Some((label, angle, true))` → `spinner(angle, 14.0, Swatch::Slate)` + the label text, no `on_press` (disabled style).
  - `Some((label, _, false))` → normal icon + "Sync", no `on_press`, wrapped in a tooltip `format!("Wait for {label} to finish")`.
  In `app::view` build it from `app.busy`: `label = busy.label(project_label(app, busy.project_id), app.now)`, `angle = busy::spin(busy.started, app.now)`, `is_this = busy.project_id == project.id`.
- [ ] **Step 3: Disabled elsewhere** — pass `can_start(app)` as `enabled: bool`:
  - `deps::view(..., enabled)`: toolbar Sync / Recreate use `.on_press_maybe(enabled.then_some(...))`; the version cell's `text_input` uses `.on_submit_maybe(enabled.then_some(DepVersionSubmitted))`.
  - `members_editor::view(tab, i, enabled)`: Sync members `.on_press_maybe(enabled.then_some(...))`.
  - `context_menu::view(menu, label, default, enabled)`: `entry` gains `message: Option<Message>` (`.on_press_maybe`); Sync and Recreate pass `enabled.then_some(...)`, the rest `Some(...)`.
- [ ] **Step 4: Sidebar.** `sidebar::view(entries, selected, busy: Option<(uuid::Uuid, f32)>)`: for the entry whose `id` matches, render `icons::spinner(angle, 12.0, Swatch::Slate)` in the glyph column instead of the state glyph.
- [ ] **Step 5: Drawer.** `drawer::view(drawer, busy_label: Option<String>)`: when `Some`, show it (in `Swatch::Slate`) instead of `drawer.status`.
- [ ] **Step 6: Toast layer** in `app::view`, pushed after the dialogs and before `preferences`/menus:

```rust
    if !app.toasts.is_empty() {
        let mut list = iced::widget::column![].spacing(theme::SPACE_2).align_x(iced::Alignment::End);
        for (i, toast) in app.toasts.iter().enumerate() {
            let (icon, swatch) = match toast.kind {
                busy::ToastKind::Success => (icons::Icon::Check, theme::Swatch::Moss),
                busy::ToastKind::Failure => (icons::Icon::Alert, theme::Swatch::Boot),
            };
            let mut row = iced::widget::row![
                icons::icon(icon, 16.0, swatch),
                iced::widget::text(toast.text.clone()).size(theme::BODY),
            ]
            .spacing(theme::SPACE_2)
            .align_y(iced::Alignment::Center);
            if toast.kind == busy::ToastKind::Failure {
                row = row.push(
                    iced::widget::button(iced::widget::text("Show output").size(theme::LABEL))
                        .on_press(Message::ShowOutput)
                        .padding([theme::SPACE_1, theme::SPACE_2])
                        .style(theme::button_secondary),
                );
            }
            row = row.push(icons::icon_button(icons::Icon::X, "Dismiss", Some(Message::ToastDismissed(i))));
            list = list.push(
                iced::widget::container(row)
                    .padding([theme::SPACE_2, theme::SPACE_3])
                    .style(theme::panel),
            );
        }
        layers = layers.push(
            iced::widget::container(list)
                .width(iced::Length::Fill)
                .height(iced::Length::Fill)
                .align_x(iced::Alignment::End)
                .align_y(iced::Alignment::End)
                .padding(theme::SPACE_3),
        );
    }
```

  The toast container is not `opaque`: presses outside the toasts reach the app beneath. Check that iced `stack` passes events through a non-opaque transparent container; if a press on the empty area is swallowed, wrap only the `list` (not the full-size container) with `iced::widget::opaque`, and keep the outer container non-interactive.
- [ ] **Step 7: Full verification**, then `timeout 8 ./target/debug/bombadil` to confirm it starts cleanly, and say in the summary that busy/toast rendering was not seen by eye.

---

### Task 6: Refresh on focus

**Files:**
- Modify: `crates/bombadil-gui/src/busy.rs` (`FOCUS_DEBOUNCE`), `crates/bombadil-gui/src/app.rs` (`should_refresh_on_focus`, `Message::WindowFocused`, `App.last_focus_refresh`, handler), `crates/bombadil-gui/src/lib.rs` (subscription)

**Interfaces:**
- Produces: `busy::FOCUS_DEBOUNCE: Duration = 2 s`; `pub fn should_refresh_on_focus(app: &App, now: Instant) -> bool`; `Message::WindowFocused`.

- [ ] **Step 1: Failing tests** in `app.rs`:

```rust
    #[test]
    fn focus_refreshes_only_when_idle_selected_and_not_too_soon() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let mut app = App::new(Config { projects: vec![project("api")], ..Config::default() });
        app.selected = Some(0);
        assert!(should_refresh_on_focus(&app, now));

        app.last_focus_refresh = Some(now);
        assert!(!should_refresh_on_focus(&app, now + Duration::from_millis(1999)));
        assert!(should_refresh_on_focus(&app, now + Duration::from_secs(2)));

        app.busy = Some(crate::busy::Busy::new(app.config.projects[0].id, crate::busy::Action::Sync));
        assert!(!should_refresh_on_focus(&app, now + Duration::from_secs(10)));

        app.busy = None;
        app.selected = None;
        assert!(!should_refresh_on_focus(&app, now + Duration::from_secs(10)));
    }

    #[test]
    fn focusing_the_window_reprobes_the_selected_project() {
        let mut app = App::new(Config { projects: vec![project("api")], ..Config::default() });
        app.selected = Some(0);
        let task = update(&mut app, Message::WindowFocused, Arc::new(FakeCommandRunner::new()));
        assert!(task.units() > 0, "git, venv and deps must be re-read");
        assert!(app.last_focus_refresh.is_some());
        let again = update(&mut app, Message::WindowFocused, Arc::new(FakeCommandRunner::new()));
        assert_eq!(again.units(), 0, "a second focus inside the debounce does nothing");
    }
```

- [ ] **Step 2: Run** `cargo test -p bombadil-gui focus` — Expected: compile errors.

- [ ] **Step 3: Implement.** `busy.rs`: `pub const FOCUS_DEBOUNCE: Duration = Duration::from_secs(2);`. `app.rs`: `pub last_focus_refresh: Option<std::time::Instant>` (init `None`);

```rust
/// Whether coming back to the window should re-read the selected project:
/// idle, something selected, and not within [`busy::FOCUS_DEBOUNCE`] of the
/// last time -- alt-tabbing must not stack up `git` and `uv` calls.
pub fn should_refresh_on_focus(app: &App, now: std::time::Instant) -> bool {
    can_start(app)
        && app.selected.is_some()
        && app
            .last_focus_refresh
            .is_none_or(|at| now.saturating_duration_since(at) >= busy::FOCUS_DEBOUNCE)
}
```

`Message::WindowFocused` handler:

```rust
        Message::WindowFocused => {
            let now = std::time::Instant::now();
            if should_refresh_on_focus(app, now)
                && let Some(i) = app.selected
            {
                app.last_focus_refresh = Some(now);
                // The same re-read a selection does: git, deps, members.
                return Task::batch([
                    update(app, Message::ProjectSelected(i), runner),
                    probe_every_venv(app),
                ]);
            }
        }
```

(If `ProjectSelected` clears state that should survive a focus — it clears `dep_edit` — ledger a ruling: an open version edit is closed by refocusing. If that is unacceptable, factor the three tasks `ProjectSelected` returns into `fn reprobe_selected(app, i, runner) -> Task<Message>` and call it from both.)

`lib.rs` subscription batch adds:

```rust
            iced::window::events().filter_map(|(_id, event)| match event {
                iced::window::Event::Focused => Some(app::Message::WindowFocused),
                _ => None,
            }),
```

(`filter_map` on `Subscription` requires a non-capturing fn; if the closure form is rejected, use a named `fn focused(event: (iced::window::Id, iced::window::Event)) -> Option<app::Message>`.)

- [ ] **Step 4: Run** `cargo test -p bombadil-gui` — Expected: PASS.
- [ ] **Step 5: Full verification.**

---

## Self-review notes

- Spec §1 → Tasks 1–2; §2 → Task 5; §3 → Tasks 3, 5; §4 → Task 4; §5 → Task 6; §6 → Task 1 (`label_without_a_project_name`), Task 2 (`a_timed_out_sync_releases_busy_and_says_so`); Testing → per task.
- Names cross-checked across tasks: `Busy::new`, `Action::*`, `label`, `done`, `failed`, `can_start`, `project_label`, `push`, `expire`, `spin`, `animating`, `Tick`, `ToastDismissed`, `ShowOutput`, `should_refresh_on_focus`, `WindowFocused`, `FOCUS_DEBOUNCE`, `icons::spinner`.
- `App.now` is added in Task 4 because views need a clock for labels and the spinner; the spec's `App.spinner: f32` is replaced by deriving the angle from `busy.started` and `App.now` (`busy::spin`), so there is no second piece of state to keep in step.
