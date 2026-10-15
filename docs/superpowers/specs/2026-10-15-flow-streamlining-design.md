# Flow streamlining: upgrades, filters, cues

Sub-project 5 of 5 in the UX/UI overhaul. Builds on 2 (busy, toasts),
3 (shortcuts, hover actions, `tip`) and 4 (summary card).

## Intent

**Said by the user:** after the flow audit, "Add 3,4,5,7,7,8,9,10" — the
audit rows: upgrade packages, filter packages, filter projects, Python pin
cue, unsaved env-var cue, sync after outside edits, open actions.

Then: "Add packages from a folder as a feature as well", clarified as
**both** a local package as a dependency and adding projects from a folder
(audit #6).

**Out of scope (left out of the user's list):** adding a package from an
index by name, and removing dependencies (audit #1, #2).

**Success:**

- Upgrading one package or all of them is one action plus, for all, one
  confirmation, with the busy spinner and a toast.
- A long package list or project list narrows as you type.
- A pin that the environment does not satisfy says so, next to a Sync.
- A typed, unsaved env-var value never looks saved.

## 1. Upgrades (#3)

- `uv::command::sync_upgrade(uv, ctx, package: Option<&str>)`:
  `uv sync --upgrade-package <name>` for one, `uv sync --upgrade` for all.
  Verified flags (`-P, --upgrade-package`; `-U, --upgrade`).
- `busy::Action::Upgrade(Option<String>)`: label "Upgrading fastapi…" /
  "Upgrading all packages…"; done "Upgraded fastapi" / "Upgraded all
  packages"; failed "Could not upgrade …".
- Row action: a quiet `Icon::ArrowUp` (Lucide `arrow-up`) in the version
  cell's hover, beside the pencil, tooltip "Upgrade <name> within its
  constraint". `Message::DepUpgradeRequested(name)`; refused unless
  `can_start`.
- Toolbar: secondary **Upgrade all** (`Icon::ArrowUp`), opening a confirm
  ("Upgrade every package to the newest versions the constraints allow?
  This rewrites uv.lock.") → `DepUpgradeAllConfirmed` / `…Cancelled`;
  Esc cancels (added to `escape_message` above `confirm_recreate`).
- Runs through the same streaming path as Sync; the finish refreshes deps.

## 2. Package filter (#4)

- `App.dep_filter: String`; toolbar `text_input` "Filter packages", id
  `DEP_FILTER`. `Shortcut::Find` (Ctrl+F) focuses it when the Dependencies
  tab shows (switches to it otherwise); Esc with a non-empty filter and
  nothing else open clears it (after `dep_edit` in the Esc order).
- Pure, tested: `deps::matches(name, filter) -> bool` (case-insensitive
  substring; empty matches all); `deps::filter_all(rows, filter)`;
  `deps::filter_tree(tab, filter) -> DepsTab` keeping direct dependencies
  that match or have a matching descendant in the graph.
- Cleared on project change.

## 3. Project filter (#5)

- `App.project_filter: String`; a `text_input` "Filter projects" above the
  sidebar list (hidden while collapsed).
- Pure, tested: `sidebar::visible(entries, config, filter, selected) ->
  Vec<usize>` — indices whose label or project directory contains the text
  (case-insensitive), plus `selected` always. Views render only these
  indices; messages keep carrying real indices.
- The rail uses the same `visible`.

## 4. Python pin cue (#7)

- Pure, tested: `sidebar::pin_unmet(pin: &PythonPin, version: Option<&str>)
  -> bool` — true when the pin names a version (`PythonPin::Version(v)`) and
  the probed version does not start with `v` as whole components
  (`3.12` matches `3.12.4`, not `3.1`); false when unpinned or not probed.
- Sidebar environment row: "Sync to apply" in `text_warning` and a quiet
  Sync icon button, when `pin_unmet`.
- Environment tile: value becomes "Python 3.11.9, pinned 3.12: sync to
  apply", state Drifted, press `SyncRequested(i)`.

## 5. Unsaved env-var values (#8)

- Pure, tested: `env_editor::unsaved(drafts, vars) -> Vec<usize>` — rows
  whose draft differs from the saved value (secret rows: any non-empty
  draft).
- Row: an amber "unsaved" label; the save button becomes primary.
- The section heading appends "· N unsaved".
- `PreferencesClosed` with unsaved drafts keeps them (as today) and pushes a
  failure-kind toast "N environment variable values are not saved" with no
  Show output.

## 6. Sync where the drift shows (#9)

- The Dependencies tile, when its state is Absent or Drifted and nothing is
  busy, shows a small primary **Sync** button inside the tile
  (`SyncRequested(i)`). The tile's own press still opens the tab.

## 7. Open folder in the header (#10)

- Header gains `icon_button(Icon::Folder, "Open the project folder",
  OpenFolderRequested(i))` before PyCharm. `Icon::Folder` is Lucide
  `folder`.

## 8. A local package as a dependency

- Dependencies toolbar: secondary **Add local package…** → folder picker.
- The folder must hold a `pyproject.toml` (or `setup.py`); otherwise a
  failure toast "<folder> is not a Python package" and nothing runs.
- A small dialog (`add_local::Draft`): the folder, its package name read
  from `[project].name` when present, a group choice (main, each existing
  dependency group, each extra) and **Editable** (on by default). Add /
  Cancel; Esc cancels.
- `uv::command::add_path(uv, ctx, path, editable, group)`:
  `uv add [--editable] <path> [--group g | --optional e]`.
- The path is passed relative to the project directory
  (`relative_to(folder, project_dir)`, pure, tested: shared prefix then
  `..` steps; absolute when there is no shared prefix, e.g. another drive),
  so `[tool.uv.sources]` stays portable across machines.
- `busy::Action::AddLocal(String)`: "Adding <name>…", "Added <name>",
  "Could not add <name>". Finish refreshes deps.

## 9. Add a project from a folder

- The sidebar's **Add** (and Ctrl+N, and the empty state) opens a **folder**
  picker instead of a `pyproject.toml` file picker.
- Pure, tested: `add_project::manifests_in(dir, read_dir) -> Vec<PathBuf>` —
  `dir/pyproject.toml` if present; otherwise each immediate subfolder's
  `pyproject.toml`, sorted. Projects already registered are excluded.
  - One → the existing add dialog, as today.
  - Several → a chooser listing them (relative names); picking one opens
    the add dialog. (One at a time: the dialog's interpreter and venv
    choices are per project.)
  - None → failure toast "No pyproject.toml in <folder> or its
    subfolders".
- **Drop:** `window::Event::FileDropped(path)` → the same function, for a
  folder or a `pyproject.toml` file. Works on Windows, macOS and X11;
  **winit 0.30 has no drop support on Wayland**, where the picker is the
  way in. The empty state says "Choose a project folder" and does not
  promise dropping, since it cannot tell whether drops will arrive.

## Testing

- `sync_upgrade` argv for one and all.
- `Action::Upgrade` label/done/failed; `DepUpgradeRequested` sets busy and
  refuses while busy; Upgrade-all confirm → busy; cancel and Esc clear it.
- `matches`, `filter_all`, `filter_tree` (match in a descendant keeps the
  parent; no match → empty); Ctrl+F maps and focuses; Esc clears a filter.
- `visible`: label match, path match, selected kept, empty filter = all.
- `pin_unmet` cases above.
- `unsaved` for plain and secret rows; closing Preferences with unsaved
  drafts pushes the toast.
- `add_path` argv: editable/non-editable, group, extra.
- `relative_to`: sibling (`../lib`), child (`libs/x`), no shared prefix.
- `manifests_in`: root manifest wins; subfolders sorted; registered ones
  excluded; none → empty.
- A non-package folder refuses with a toast; `AddLocal` busy/labels.
