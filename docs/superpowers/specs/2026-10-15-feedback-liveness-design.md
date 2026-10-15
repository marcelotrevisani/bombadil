# Feedback and liveness: busy state, toasts, refresh on focus

Sub-project 2 of 5 in the UX/UI overhaul. Builds on sub-project 1's button
roles (the disabled state) and icons (`Icon::Refresh`, `Icon::Check`).

## Intent

**Said by the user:** the app should feel more dynamic. Pressing something
should visibly do something; the state on screen should not go stale.

**Agreed during design:**

- One uv command at a time, as today. It becomes visible.
- Refresh trigger: **window regains focus** (not a timer, not file watching).
- Animation via `iced::window::frames()`, subscribed only while something
  animates. No new runtime dependency (`smol`/`tokio` rejected).

**Out of scope:** concurrent uv commands; cancelling a running command;
periodic or file-watch refresh.

**Success:**

- While a command runs, its button shows a rotating icon and the label;
  every other uv-starting button is disabled. Pressing twice is impossible.
- Every finished command produces a toast that says what happened.
- After committing in a terminal and switching back, the Git line and
  dependency list are current without clicking anything.
- When nothing is busy and no toast is showing, the app requests no frames.

## Today

`app.syncing: Option<String>` holds the running project's id. It is set by six
handlers (`SyncRequested`, `DepVersionSubmitted`, `AddProjectConfirmed`,
`PinChangeConfirmed`, `RecreateVenvConfirmed`, `dispatch_members_sync`) and
read about 29 times, but no view reads it: buttons stay clickable and a second
press is silently ignored. `SyncFinished` clears it. `dep_install` (added
earlier this session) carries the requirement for the finish message and is
folded into the new `Action`.

## 1. State

```rust
pub struct Busy {
    pub project_id: Uuid,
    pub action: Action,
    pub started: Instant,
}

pub enum Action {
    Sync,
    InstallVersion(String), // the `name==version` requirement
    AddProject,
    PinChange,
    Recreate,
    MembersSync,
}
```

- `App.busy: Option<Busy>` replaces `App.syncing` and `App.dep_install`.
- `Busy::label(&self, project_label: &str, now: Instant) -> String`, e.g.
  `"Syncing api… 12s"`, `"Installing fastapi==0.110… 3s"`. Under 1 s shows no
  seconds.
- `Action::done(&self) -> String` / `Action::failed(&self) -> String`: the
  toast sentences ("Synced api", "Could not install fastapi==0.110").
- `fn can_start(app: &App) -> bool` is `app.busy.is_none()`; every
  uv-starting button uses it, so the rule lives in one place.
- `App.spinner: f32` is the current rotation in radians, advanced on `Tick`.

## 2. Busy rendering

- **The button that started the command** (header Sync, deps toolbar Sync or
  Recreate, the version cell, Members "Sync members") shows `Icon::Refresh`
  rotated by `app.spinner` and the action's present-tense label, and is
  disabled.
- **Every other uv-starting button** in every project is disabled.
  `icon_button`/role buttons render their disabled state; its tooltip reads
  "Wait for <label> to finish".
- **The busy project's sidebar row** shows the rotating icon in place of its
  state glyph.
- **The drawer's collapsed line** shows `Busy::label` while busy.

## 3. Toasts

```rust
pub struct Toast {
    pub kind: ToastKind, // Success | Failure
    pub text: String,
    pub expires: Option<Instant>, // None = until dismissed
}
```

- `App.toasts: Vec<Toast>`, newest first, capped at 3 (the oldest is dropped).
- **Success:** `Icon::Check` in moss, text, auto-dismiss after **4 s**.
- **Failure:** an alert in boot, text, a **Show output** button that sets
  `drawer.expanded = true`, and no expiry.
- Every toast has a dismiss (✕) button: `Message::ToastDismissed(index)`.
- Rendered as a layer in the bottom-right corner, above content and below
  modals, using the existing layer stacking in `app::view`.
- `SyncFinished` pushes the toast for `busy.action`. The forced
  `drawer.expanded = true` on a failed version install is removed; Show output
  replaces it.

Icons this needs beyond sub-project 1: `Icon::Alert` (Lucide
`triangle-alert`) and `Icon::X` (Lucide `x`), from the same pinned
`lucide-static` 1.49.0.

## 4. Animation and expiry

- `Message::Tick(Instant)`: advances `spinner`, drops toasts whose `expires`
  has passed.
- `lib.rs` subscription adds `iced::window::frames().map(Message::Tick)` only
  when `app.busy.is_some() || !app.toasts.is_empty()`.
- Spinner speed: one turn per second.

## 5. Refresh on focus

- Subscribe to `iced::window::events()` (yields `(window::Id, window::Event)`)
  and map `window::Event::Focused` to `Message::WindowFocused`; the handler
  reads `Instant::now()` for the debounce. Other events map to nothing.
- Handler: if nothing is busy, a project is selected, and the last focus
  refresh was at least **2 s** ago, re-run the selected project's Git, venv
  and deps probes through the functions `ProjectSelected` already uses.
  `App.last_focus_refresh: Option<Instant>` records it.
- Skipped while busy: the command's own finish refreshes everything.

## 6. Errors and edge cases

- Timeout or panic in a command still ends in `SyncFinished`, which clears
  `busy` and pushes a failure toast.
- A project removed while busy: `busy.project_id` no longer resolves. The
  label falls back to the action alone ("Syncing…"), and the finish clears
  `busy` regardless.
- A `Tick` arriving when nothing animates is a no-op.

## Testing

Unit tests on the pure parts (views stay untested by convention):

- `Busy::label` for each action, with and without elapsed seconds.
- `can_start` false while busy, true after `SyncFinished`.
- Each of the six handlers sets `busy` with the right `Action`, and refuses to
  start while busy.
- `SyncFinished` pushes one toast of the right kind and text, and clears
  `busy`.
- Toast cap of 3; `Tick` past `expires` removes a success toast and never a
  failure toast.
- The focus decision is a pure `fn should_refresh_on_focus(app, now) -> bool`
  tested directly: true when idle with a selection and no refresh in the last
  2 s; false while busy, with nothing selected, or within 2 s.
- Existing tests reading `syncing`/`dep_install` move to `busy`.
