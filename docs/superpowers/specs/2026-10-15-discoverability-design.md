# Discoverability: shortcuts, hover row actions, tooltips

Sub-project 3 of 5 in the UX/UI overhaul. Builds on sub-project 1's icons
and button roles and sub-project 2's busy state (`can_start`, `Busy::label`).

## Intent

**Said by the user:** the app should be easier to use.

**Agreed during design:**

- What the app can do should be visible where people look: right-click
  menus, double-click to edit a version, and shortcuts are invisible today.
- Five shortcuts: Ctrl+N, Ctrl+Enter, Ctrl+T, F5, Esc (⌘ for Ctrl on macOS).
- Shortcuts are named in tooltips and listed under Help → Keyboard shortcuts.
- Sidebar rows show their actions on hover; the version cell shows a pencil.

**Out of scope:** user-remappable keys, arrow-key sidebar navigation, per-tab
shortcuts, empty states (sub-project 4).

**Success:**

- Every shortcut works from anywhere it is allowed and does nothing where it
  is not (under a dialog, while busy, with nothing selected).
- Esc closes whatever is on top, one layer per press.
- A user who never right-clicks can still reach every context-menu action.
- Every button disabled while a command runs says what it is waiting for.

## 1. Shortcuts

```rust
pub enum Shortcut { AddProject, Sync, Terminal, Refresh, Escape }
```

| Shortcut | Keys | Message | Allowed when |
|---|---|---|---|
| AddProject | Ctrl+N | `AddProjectPickManifestRequested` | no overlay open, `can_start` |
| Sync | Ctrl+Enter | `SyncRequested(selected)` | no overlay, `can_start`, a project selected |
| Terminal | Ctrl+T | `OpenTerminalHereRequested(selected)` | no overlay, a project selected |
| Refresh | F5 | `RefreshRequested` (new) | no overlay, `can_start`, a project selected |
| Escape | Esc | the top layer's close message | something to close |

"Ctrl" is `Modifiers::command()`: Ctrl on Linux/Windows, ⌘ on macOS.
"Overlay" is any of: `modal`, `preferences`, `open_menu`, the four confirm
dialogs, `context_menu`, `add_project`.

- **Input:** `iced::event::listen_with(key_event)` in the subscription batch.
  `key_event` is a plain `fn(Event, Status, window::Id) -> Option<Message>`
  that calls `shortcut(&Key, Modifiers, Status) -> Option<Shortcut>` and
  wraps it as `Message::Shortcut(Shortcut)`.
- **Captured keys:** `shortcut` returns `None` for a captured event, except
  Esc. A focused `text_input` captures Esc to unfocus itself; letting Esc
  through means one press closes the version edit, not two. Every other key
  typed into a field stays the field's.
- **Gating:** `shortcut_message(&App, Shortcut) -> Option<Message>` applies
  the table. `update` handles `Message::Shortcut(s)` by recursing into
  `update` with the result, so shortcut and button run the same code.
- **`RefreshRequested`:** runs `reprobe_project` for the selected project,
  the same re-read as the focus refresh, with no debounce (it was asked for).

### Esc order

Top of the stack first, matching the layer order in `app::view`:

1. `modal` → `ModalClosed`
2. `preferences` → `PreferencesClosed`
3. `open_menu` → `MenuDismissed`
4. `confirm_member_sync` → `MembersSyncCancelled`
5. `confirm_environment_removal` → `EnvironmentRemovalCancelled`
6. `confirm_pin_change` → `PinChangeCancelled`
7. `confirm_recreate` → `RecreateVenvCancelled`
8. `context_menu` → `ContextMenuClosed`
9. `add_project` → `AddProjectCancelled`
10. `dep_edit` → `DepVersionEditCancelled`

Toasts are not closed by Esc: they are not in the way of anything.

## 2. Naming the shortcuts

- `pub const SHORTCUTS: [(Shortcut, &str /* keys */, &str /* what */); 5]`
  is the one table. Keys use `MOD` (`"Ctrl"`, or `"⌘"` under
  `cfg(target_os = "macos")`).
- **Tooltips** append the keys: header Sync "Sync (Ctrl+Enter)", header
  terminal "… (Ctrl+T)", sidebar Add project "Add project (Ctrl+N)".
- **Help → Keyboard shortcuts** opens `Modal::Shortcuts`, a card rendering
  `SHORTCUTS` as two columns, keys in the data face.

## 3. Hover row actions

- **Sidebar project row:** wrapped in `iced::widget::hover(row, actions)`.
  `actions` is right-aligned quiet icon buttons on a `bark` pill: Sync
  (`Icon::Refresh`), Terminal (`Icon::Terminal`), More (`Icon::Ellipsis`,
  Lucide `ellipsis`), each with a tooltip. More sends
  `ContextMenuOpened(i)`, the same menu a right-click opens.
- **The selected row** shows `actions` always (stacked, not hovered), so the
  actions do not depend on a pointer that hovers.
- Sync is disabled while busy, with the "Wait for …" tooltip below.
- The pill covers the end of a long label while hovered; the label is still
  in full in the detail header.
- **Dependencies version cell:** `hover(cell, pencil)`, a quiet
  `Icon::Pencil` at the cell's right edge that sends the same message as the
  double-click. Tooltip: "Change version (or double-click)". Not shown while
  busy, nor on rows that cannot be edited.

## 4. Tooltips

- `theme::tip(content, text, position) -> Element` is the one tooltip helper:
  a `panel`-styled container, `LABEL` size. `icons::icon_button` uses it.
- **Disabled while busy:** every uv-starting button (deps toolbar Sync and
  Recreate, Members Sync, context-menu Sync and Recreate, Add project, the
  sidebar hover Sync) shows "Wait for <busy label> to finish", the header
  Sync's wording.
- **State glyphs:** each sidebar glyph gets a tooltip with its meaning, from
  a `fn state_meaning(State) -> &'static str` beside `theme::state_glyph`.

## Testing

Unit tests on the pure parts; views stay untested by convention.

- `shortcut`: each of the five keys maps; Ctrl+N without Ctrl does not;
  a captured Ctrl+Enter is `None`; a captured Esc is still `Escape`.
- `shortcut_message`: each shortcut's message when allowed; `None` while
  busy (AddProject, Sync, Refresh), with nothing selected (Sync, Terminal,
  Refresh), and under an overlay (all but Escape).
- Esc: with two layers open, the message closes the upper one; with nothing
  open, `None`.
- `RefreshRequested` with a selection returns a task; without, does nothing.
- `SHORTCUTS` lists every `Shortcut` exactly once.
- `state_meaning` covers every `State`.
