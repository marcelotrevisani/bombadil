# Close to tray, and a tray menu of projects

## Intent

**Said by the user:** "when clicking to close the app, I would like it to
minimize it to the systray (support mac, windows and linux), and in the
systray I would like a menu where the user can see the projects and their
environments and has option to open a terminal or pycharm. If the user
double click on the icon in the systray it should open the app."

**Agreed during design:**

- The X hides to the tray by default, behind a Preferences switch
  ("Closing the window keeps Bombadil in the tray"). With no tray running,
  the X still quits: a hidden window with no icon cannot be brought back.
- The tray menu lists every project; each environment offers Open terminal
  and Open in PyCharm, acting on **that** environment without making it
  active.
- Opening the window: double-click on Windows; the host's activate on Linux
  (a single click on GNOME and KDE; StatusNotifierItem has no double-click);
  the menu's "Show Bombadil" on macOS, where a menu-bar item's click opens
  its menu.

**Out of scope:** running Sync or other uv commands from the tray; tray
notifications; per-project tray icons.

**Success:**

- Closing the window leaves the icon; the menu still opens a terminal for any
  environment; Show brings the window back with its state intact.
- The menu matches the projects and environments within one update of a
  change (add, remove, rename, new environment, a probed Python version).
- Linux is verified by running it; macOS and Windows are compiled in CI and
  stated as unrun, as the existing tray is.

## 1. Close routing

- Core `Settings.close_to_tray: bool`, `#[serde(default = "yes")]` → `true`.
- `lib.rs`: `close_requests()` maps to `Message::CloseRequested`.
- `update`: `CloseRequested` → `Tray(Hide)` when `close_to_tray`, else
  `Tray(Quit)`. `window_action` already turns `Hide` into `Close` when no
  tray is running; that guard is the safety net and keeps its test.
- Preferences → Appearance gains the switch (a `button_choice` pair "Keep in
  tray" / "Quit"), persisted like the theme.
- The module doc in `tray.rs` ("The close button is not a minimise button")
  is rewritten to say what is true now and why the switch exists.
- Saving the window size: hiding remembers nothing new; the tray's Quit
  still goes through `remember_window_size`. A hide also records the size in
  memory first, since a hidden window reports no further resizes.

## 2. The menu model

Pure and tested, in `tray.rs`:

```rust
pub struct TrayProject { pub id: Uuid, pub label: String, pub environments: Vec<TrayEnvironment> }
pub struct TrayEnvironment { pub location: VenvLocation, pub label: String, pub active: bool }
pub fn menu_model(config: &Config, entries: &[sidebar::Entry]) -> Vec<TrayProject>
```

- Environment label: the location as the sidebar names it, then
  `· Python 3.12.4` when probed, then ` (active)` for the active one.
- `App.tray_menu: Vec<TrayProject>` holds the last model sent.
- After every `update`, the wrapper in `lib.rs` computes the model and, if it
  differs from `tray_menu`, stores it and hands it to the tray
  (`tray::Tray::set_menu`). Cheap: a few dozen strings.
- The `Tray` handle moves from a local in `run()` into an `Rc` captured by
  that update closure (iced 0.14's `UpdateFn` has no `Send` bound; checked).
  Still held for the whole run, so the icon still lives as long as the app.

## 3. Menu commands

`TrayCommand` gains:

```rust
OpenTerminal { project: Uuid, environment: VenvLocation },
OpenPycharm { project: Uuid, environment: VenvLocation },
```

- Ids and locations, never positions: a menu built before a project was
  removed must not open the wrong one.
- `update` resolves the id to an index; an unknown id or location is
  ignored. It clones the project, sets the clone's `active` to the chosen
  location, and runs the same open path as `OpenTerminalHereRequested` /
  `OpenPycharmRequested` with the clone. The saved config is untouched.
- Results reach the drawer and toasts like the window's own buttons; a
  hidden window shows them when it comes back.

Menu layout, top to bottom: **Show Bombadil**; separator; one submenu per
project (label). In it: one submenu per environment holding
**Open terminal** and **Open in PyCharm**; or, for a project with exactly one
environment, those two items directly. A project with no environments shows
a disabled "No environment". Separator; **Quit**. "Hide" leaves the menu: the
X does that now.

## 4. Platforms

- **Linux (`ksni`):** `Menu` gains `projects: Vec<TrayProject>`; `menu()`
  builds `SubMenu`s from it; `set_menu` calls `Handle::update`, which is
  thread-safe. `activate` stays "Show".
- **macOS / Windows (`tray-icon`):** `set_menu` rebuilds a `muda` `Menu` and
  calls `TrayIcon::set_menu`. `tray-icon` objects must be touched on the
  main thread; iced runs `update` (and so the wrapper) there. Menu item ids
  map to commands through a `HashMap<MenuId, TrayCommand>` shared with the
  event thread behind a `Mutex`, replaced on each rebuild.
  Windows: `TrayIconEvent::DoubleClick` → Show, via
  `TrayIconEvent::receiver()` on the same thread that reads menu events.
- **Compiled, not run** on macOS and Windows: stated in the module doc, as
  today.

## Testing

- `menu_model`: one environment, several, the active mark, probed and
  unprobed versions, a project with none; it follows `config` order.
- `CloseRequested` → hide with the switch on, quit with it off; with no tray
  `window_action(Hide, false)` is `Close` (existing test).
- A config without `close_to_tray` loads as `true`; the switch persists.
- Tray `OpenTerminal` for a non-active environment builds its command against
  that environment's path, and leaves `config` unchanged; an unknown project
  id or location is ignored.
- The Linux `menu()` item tree from a model (labels and nesting), since
  `ksni` menus are plain data.
