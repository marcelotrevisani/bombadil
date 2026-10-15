# Layout and Hierarchy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Summary card with drift, resizable and collapsible sidebar, remembered window size and tab order, empty states.

**Architecture:** One persisted `Layout` in core `Settings`. GUI state for drags lives on `App` (`sidebar_drag`, `tab_drag`, `window_size`); a drag-only `listen_with` subscription exists only while a drag runs. All decisions are pure functions next to the view that uses them.

**Tech Stack:** Rust, iced 0.14 (`mouse_area::interaction/on_enter/on_release`, `event::listen_with`, `window::Event::Resized`), serde/toml.

**Spec:** `docs/superpowers/specs/2026-10-15-layout-hierarchy-design.md`

## Global Constraints

- No commits (user's standing instruction); each task ends with the verification command.
- Verification: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings -A clippy::chunks_exact_to_as_chunks && cargo test --workspace`.
- Sidebar width clamp 180–480, default 240; window floor 640×420; rail 44 px; handle 6 px.
- Existing `config.toml` without `[settings.layout]` must load unchanged.
- Views untested by convention; Lucide icons from `lucide-static@1.49.0`.

## Review Focus

1. A drag released outside the window or over another widget must still end (no stuck `sidebar_drag`/`tab_drag`) — the drag subscription maps any left release.
2. A press on a tab without movement must still select it and persist nothing.
3. Quit via tray and via window close both save the window size.
4. A corrupt layout (width 5, window 10×10, unknown tab keys) must yield a usable window.
5. The context menu anchors at the live sidebar width, and at the rail when collapsed.

---

### Task 1: Core `Layout`
- Files: `crates/bombadil-core/src/model/settings.rs`, `model/mod.rs` re-export if Settings types are re-exported there, `tests/fixtures/full.toml`.
- Tests first: `layout_defaults_when_absent` (Settings from toml without layout → width 240, not collapsed, window None, order empty); golden round-trip fixture gains `[settings.layout]` with all four fields.
- Implement struct per spec §1, `Default` with width 240, `Settings.layout` `#[serde(default)]`.

### Task 2: Resizable sidebar
- Files: `app.rs` (messages `SidebarDragStarted`, `SidebarDragged(f32)`, `SidebarDragEnded`; `App.sidebar_drag`; `clamp_sidebar`; `dragging(app) -> bool`), `lib.rs` (`drag_event` subscription while `dragging`), `shell.rs` (`anchored_layer` takes the width), `theme.rs` (keep `SIDEBAR_WIDTH` as default only).
- Tests first: `clamp_sidebar_bounds`; `dragging_the_handle_sets_width_and_persists_once_on_release` (use the persist test double already in app tests — find how `ThemeChoiceSelected` persistence is asserted and mirror it).
- View: handle = `mouse_area(container(Space).width(6).height(Fill).style(hairline look)).interaction(ResizingHorizontally).on_press(SidebarDragStarted)`.

### Task 3: Collapsible sidebar
- Files: `shortcuts.rs` (`Shortcut::ToggleSidebar`, Ctrl+B, table now 6), `app.rs` (`SidebarToggled`, `shortcut_message` arm, rail view), `icons.rs` (`Icon::PanelLeft`), `sidebar.rs` (`rail` view).
- Tests first: Ctrl+B maps; `ToggleSidebar` refused under an overlay; `SidebarToggled` flips and persists.

### Task 4: Remembered window size
- Files: `lib.rs` (`window_settings(&Config)`, `Resized` mapping), `app.rs` (`WindowResized(Size)`, `App.window_size`, quit path copies + persists, `initial_size(&Layout) -> Option<Size>`).
- Tests first: `initial_size` None/clamped/kept; quitting saves the size (find the quit handler for `Tray(Quit)`; persistence must happen before `iced::exit()`).

### Task 5: Reorderable tabs
- Files: `app.rs` (`Tab::key`, `tab_order`, `move_tab`, `TabDragStarted/Over/Ended`, `App.tab_drag`, tab bar uses order), `lib.rs` (drag subscription also ends tab drag).
- Tests first: `tab_order` unknown/duplicate/missing; `move_tab` forward/backward/self; drag over reorders live; press+release without over persists nothing and selects the tab.

### Task 6: Summary card
- Files: `detail.rs` (`worst`, `deps_summary`, `members_summary`, tiles view; header struct gains what tiles need), `app.rs` (pass deps/members/entry state into header).
- Tests first: each summary over state mixes; `worst` ordering.

### Task 7: Empty states
- Files: `app.rs` (no-projects card, none-selected text), `deps.rs` (no-environment state when `VenvStatus::Absent`).
- View-only except the decision `deps::show_no_environment(venv_absent: bool) ` is trivial — no test (state it). Then full verification + smoke run.
