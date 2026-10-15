# Layout and hierarchy: summary card, sidebar, empty states, remembered layout

Sub-project 4 of 5 in the UX/UI overhaul. Builds on sub-project 1 (palette,
icons, roles), 2 (busy label) and 3 (shortcuts table, `tip`).

## Intent

**Said by the user:** the app should be easier to use and more dynamic. For
this part: "go, add the out of scope as well", so collapsing the sidebar,
remembering the window size and reordering tabs are in.

**Agreed during design:**

- The detail header gets a summary card that shows drift, which today only
  the sidebar glyph shows.
- The sidebar is resizable by dragging, and its width is remembered.
- Empty states say what to do next instead of being blank.
- The sidebar can collapse to a narrow rail (Ctrl+B).
- The window reopens at its last size.
- Tabs can be reordered by dragging; the order is remembered.

**Success:**

- Opening a project with a drifted dependency shows it in the header without
  opening a tab.
- Width, collapsed state, window size and tab order survive a restart.
- A fresh install shows a card with one obvious next step.

## 1. Persisted layout

Core gains, in `model::settings`:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    #[serde(default = "Layout::default_sidebar_width")]
    pub sidebar_width: f32,        // default 240
    #[serde(default)]
    pub sidebar_collapsed: bool,
    #[serde(default)]
    pub window: Option<(f32, f32)>, // last inner size, None = iced's default
    #[serde(default)]
    pub tab_order: Vec<String>,    // tab keys; empty = built-in order
}
```

`Settings.layout: Layout` with `#[serde(default)]`, so existing
`config.toml` files load unchanged. The golden fixture gains a `[settings.layout]`
table. Writes go through the existing settings-persist path.

## 2. Header summary card

Under the title row (label left; PyCharm, Terminal, Sync right), one `bark`
card (`theme::surface`, `RADIUS`) with up to four tiles in a row. A tile is a
state glyph (with its `state_meaning` tooltip), a label in `LABEL` muted, and
one value line.

| Tile | Value | Glyph | Press |
|---|---|---|---|
| Environment | `Python 3.12.4`, `no environment`, or `3.11 does not satisfy >=3.12` | `sidebar::state(entry)` | — |
| Dependencies | `42 installed · 3 drifted · 1 missing` (zero counts omitted; `loading…` before the fetch lands; `unavailable` on error) | worst row state | `TabSelected(Dependencies)` |
| Members | `4 of 5 installed` | Present if all, Drifted if a removal is pending, else Absent | `TabSelected(Members)`; tile absent for non-workspaces |
| Git | `detail::git_summary` without the upstream name | Present when clean, Drifted when dirty | — ; tile absent without a repo |

"Worst" orders Drifted > Absent > Present.

Pure, tested functions in `detail.rs`:
`deps_summary(&DepsTab) -> (State, String)`,
`members_summary(&[MemberRow]) -> Option<(State, String)>`,
`worst(impl IntoIterator<Item = State>) -> State`.

The project path and environment path move under the card as `slate`
`DATA` lines (reference, not status). The git line leaves the header
(it is the Git tile).

## 3. Resizable sidebar

- The hairline between sidebar and detail becomes a 6 px handle:
  `mouse_area(...).interaction(mouse::Interaction::ResizingHorizontally)
  .on_press(Message::SidebarDragStarted)`.
- `App.sidebar_drag: bool`. While true, the subscription batch adds
  `event::listen_with(drag_event)`, mapping `CursorMoved { position }` to
  `SidebarDragged(position.x)` and `ButtonReleased(Left)` to
  `SidebarDragEnded`. Not subscribed otherwise.
- `SidebarDragged(x)` sets `layout.sidebar_width = clamp_sidebar(x)`,
  `clamp_sidebar(x) = x.clamp(180.0, 480.0)`, in memory only.
- `SidebarDragEnded` clears `sidebar_drag` and persists settings once.
- Everything that used `theme::SIDEBAR_WIDTH` reads the live width
  (sidebar container, the context menu's anchor in `shell::anchored_layer`).
  `SIDEBAR_WIDTH` stays as the default's value.

## 4. Collapsible sidebar

- Sidebar header gains a quiet icon button (`Icon::PanelLeft`, Lucide
  `panel-left`) "Collapse sidebar (Ctrl+B)"; Ctrl+B is a sixth `Shortcut`,
  `ToggleSidebar`, allowed when no overlay is open.
- Collapsed: a 44 px rail. Top: the same button, "Expand sidebar (Ctrl+B)".
  Then one button per project: its state glyph (or the spinner if busy),
  tooltip = label, press = `ProjectSelected(i)`, the selected one marked with
  the `jacket` edge. No handle while collapsed.
- `SidebarToggled` flips `layout.sidebar_collapsed` and persists.

## 5. Remembered window size

- `window_settings(&Config)` sets `size` from `layout.window` when present,
  each side clamped to at least 640×420 so a bad value cannot make the window
  unusable.
- The window-events subscription also maps `Resized(size)` to
  `WindowResized(size)`, which stores it in `App.window_size` only (no disk
  write per pixel).
- On quit (the close request and the tray's Quit, both `Tray(Quit)`), the
  size is copied to `layout.window` and persisted before exiting.

## 6. Reorderable tabs

- `Tab::key(self) -> &'static str` ("deps", "members", "env", "scripts",
  "indexes") and `tab_order(&[String]) -> Vec<Tab>`: the saved keys in
  order, unknown keys dropped, duplicates dropped, missing tabs appended in
  built-in order. The tab bar renders `tab_order(...)`.
- Drag: each tab is a `mouse_area` with `on_press(TabDragStarted(tab))`,
  `on_enter(TabDragOver(tab))`, `on_release(TabDragEnded)`.
  `App.tab_drag: Option<Tab>`. `TabDragOver(t)` while dragging another tab
  moves the dragged one to `t`'s position (live). `TabDragEnded` clears the
  drag, and if the order changed, writes it to `layout.tab_order` and
  persists. A press without movement still selects the tab, as today.
- `move_tab(order, from, to) -> Vec<Tab>` is pure and tested.
- The release can land outside any tab: the drag-time subscription from §3
  (generalised to "any drag") also maps `ButtonReleased(Left)` to
  `TabDragEnded`.

## 7. Empty states

- **No projects:** the detail area shows a centred `panel` card: title
  "No projects yet", one sentence ("Bombadil keeps track of the uv
  environments of your Python projects. Add one by choosing its
  pyproject.toml."), and a primary **Add a project** button with the
  `tip` naming Ctrl+N. The sidebar's "None yet" stays.
- **Projects, none selected:** centred muted "Select a project on the left".
- **Dependencies tab, no environment:** instead of the error text, "No
  environment yet. Sync creates it from pyproject.toml." and a secondary
  Sync button (disabled with `wait_tip` while busy). Shown when the
  header's `VenvStatus` is `Absent` (the probe already knows), not by
  parsing uv's error text, which uv is free to reword.

## Testing

Pure parts only; views untested by convention.

- `Layout` defaults; a `config.toml` without `[settings.layout]` loads with
  width 240, not collapsed, no window, empty order; round-trip of a full one.
- `clamp_sidebar` at both bounds; drag start → move → end sets the width and
  persists once (end), not on move.
- `SidebarToggled` flips and persists; Ctrl+B maps to `ToggleSidebar` and is
  refused under an overlay.
- `window_size` clamping in `window_settings`'s pure helper
  `initial_size(&Layout) -> Option<Size>`; quit copies `window_size` into
  `layout.window` and persists.
- `tab_order` with unknown, duplicate and missing keys; `move_tab` forward,
  backward, onto itself; a drag that ends where it began persists nothing.
- `deps_summary`, `members_summary`, `worst` over each state mix.
