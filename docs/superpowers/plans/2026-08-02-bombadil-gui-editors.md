# Bombadil GUI Editors Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** See and edit what a project actually contains — its dependencies, environment variables, pre-activate scripts, private indexes, workspace members, and Bombadil's own settings.

**Architecture:** Extends `bombadil-gui`. Everything that spawns a process or touches the filesystem goes through the existing `job::run` seam. Editors mutate `Config` in memory and persist through the `ConfigPersist` trait. Secrets never enter `Config` — they go to the keychain via `SecretStore`, and the config holds only the fact that a value is secret.

**Tech Stack:** Rust edition 2024, `iced` 0.14, `rfd` 0.17, `bombadil-core`, `bombadil-uv-bin`.

## Scope

Plan 3c of three, and the last. **3a** delivered the shell; **3b** the project lifecycle. This adds the four tabs from spec §8, the settings surface, and the monorepo member checklist.

## Global Constraints

- Rust edition 2024, `rust-version = "1.88"`. **No new dependencies.**
- **`bombadil-core` must not depend on `iced` or `rfd`.**
- **No test may open a window, open a file dialog, spawn a real process, open a real terminal, touch the real OS keychain, reach the network, or read the filesystem.** Take probes, runners, stores and persisters as parameters — this crate does so throughout.
- **Secrets must never enter `config.toml`, a log line, a `Debug` render, or a failure report.** `Message` derives `Debug`; redact on the job thread via `terminal::open::secret_values`.
- **Never block the render thread.** Everything spawning a process goes through `job::run`; terminal opening uses `CommandRunner::spawn`.
- **Never write `pyproject.toml` or `uv.lock` directly** — uv owns both. Dependency edits go through `uv add`/`uv remove`.
- **Never edit `[tool.uv.workspace] members`** — it is a shared file.
- `view` is dumb and untested; logic worth testing lives in free functions.
- Clippy lints denied in CI: `derivable_impls`, `field_reassign_with_default`.

## Facts Already Established — do not re-derive

From merged plans, verified against real tools:

- **`uv tree --format json` emits a `"preview"` schema**, so `parse_tree` returns `TreeOutcome::Parsed(DependencyGraph)` **or** `UnsupportedSchema { found }`. That second variant exists so the UI can show **direct dependencies only** rather than guessing at a shape uv changed under us. Honouring it is a requirement, not a nicety.
- **`uv pip list --format json` reports `editable_project_location`** for workspace members and `uv add --editable` siblings alike. It is the join key for the member checklist — **join on path, not name**: the path is exact and survives members with no `[project].name`, which cannot be passed to `--package` at all.
- **`uv pip list` ignores `UV_PROJECT_ENVIRONMENT`** and needs an explicit `--python <venv>`; `uv::command::pip_list` already does this.
- **Plain `uv sync` in a workspace installs no members** — `--all-packages` is required, and re-syncing with a smaller `--package` set removes the deselected ones. `members::effective` encodes the precedence rule.
- **uv writes paths verbatim into `[tool.uv.sources]`**, so `members::path_for_manifest` computes a relative path to keep absolute machine paths out of a shared manifest.

## A Note On Task Detail

Every task here gives **acceptance criteria rather than literal test code**, and each must carry a **mutation proof** — break the behaviour the criterion names, confirm the test fails, restore. **Thirteen tests on this project have been found passing for the wrong reason**, every one asserting something true of both the correct and the broken behaviour. Two were found by sweeping rather than by failing, so the reviews must check that assertions discriminate, not merely run.

## Debt This Plan Must Pay

Carried from 3b's ledger:

1. **Recreate venv needs a core primitive that does not exist.** `bombadil-core` has no delete-venv function — the only `fs::remove_*` in the crate is test teardown. Task 7 adds it **in core, with its own tests**, before any GUI code calls it. Building a destructive operation inside a GUI task is how an environment gets deleted by accident.
2. **Open folder** is non-destructive and small. Task 7 also.
3. **`run_open_terminal` writes its init file into the real temp dir**, against the no-filesystem test constraint. `open` already takes a `scratch_dir`; `run_open_terminal` just needs to accept one. Task 7.

---

### Task 1: The Dependencies tab

**Files:** create `crates/bombadil-gui/src/deps.rs`; modify `app.rs`

> Spec §8: rows grouped by `main`, each dependency group, and each optional-dependencies extra. Columns: package, declared constraint, installed version. Direct dependencies expand to reveal their transitive tree.

**Criteria — each needs a mutation proof. The parenthetical is the bug it must catch.**

1. **Declared constraints and installed versions are joined by package name**, with a package declared but not installed showing its constraint and no version. (Catches: dropping declared-but-uninstalled rows, hiding exactly the packages a user needs to sync.)
2. **A package installed but not declared is still shown**, marked as such — it is a transitive dependency or a leftover. (Catches: pretending the environment matches the manifest when it does not.)
3. **Grouping keeps `main`, named groups and extras distinct**, and a package appearing in two groups appears under both. (Catches: flattening, which loses the information the tab exists to show.)
4. **`TreeOutcome::UnsupportedSchema` shows direct dependencies only, and says why.** (Catches: guessing at an unknown uv schema, or silently showing an empty tree.)
5. **Fetching the tree and the installed list goes through `job::run`, never inline.** (Catches: freezing the window — `uv tree` on a large project is slow.)

---

### Task 2: The Env vars tab

**Files:** create `crates/bombadil-gui/src/env_editor.rs`; modify `app.rs`

> Spec §5: global variables apply to everything Bombadil launches; project variables override them. A variable marked secret has its value in the OS keychain, never in `config.toml`.

**Criteria — each needs a mutation proof.**

1. **A project variable overriding a global one is shown as overriding**, with both values visible. (Catches: showing only the winner, so a user cannot tell why a global has no effect.)
2. **Marking a variable secret writes the value to the `SecretStore` and stores no value in `Config`.** Assert the `Config` field is empty **and** the store holds the value. (Catches: the exact leak the whole secrets plan exists to prevent — a secret in a file the user might commit.)
3. **Unmarking a variable secret deletes the keychain entry.** (Catches: an orphaned secret surviving in the keychain after the user believes it removed.)
4. **A secret's value is never rendered**, in the editor or anywhere else. (Catches: a UI that displays what it stored.)
5. **Editing persists through `ConfigPersist`.** (Catches: in-memory-only edits that vanish on restart.)

---

### Task 3: The Scripts tab

**Files:** create `crates/bombadil-gui/src/scripts_editor.rs`; modify `app.rs`

> Spec §6: pre-activate scripts run in order, after the project's variables and before activation. A script that exits non-zero aborts the whole composition.

**Criteria — each needs a mutation proof.**

1. **Scripts render in execution order, and reordering persists.** (Catches: display order diverging from run order, which makes the tab actively misleading.)
2. **A disabled script is shown as disabled and is not removed.** (Catches: conflating disable with delete.)
3. **Adding a script uses the file picker through `AsyncFileDialog`**, never the blocking form. (Catches: freezing the render thread.)
4. **Global and project scripts are distinguished**, and the tab states that global ones run first. (Catches: a user unable to explain the order they observe.)

---

### Task 4: The Indexes tab

**Files:** create `crates/bombadil-gui/src/index_editor.rs`; modify `app.rs`

> Spec §7: index credentials reach uv purely through environment variables — **nothing is written to `pyproject.toml`**.

**Criteria — each needs a mutation proof.**

1. **An index credential is stored in the `SecretStore` under `SecretKey::Index`, never in `Config`.** (Catches: a registry password in a committed file.)
2. **Two index names normalising to the same credential variable are rejected with a message naming both.** `validate` already detects this and `inject` refuses it; the editor must surface it **before** the user saves. (Catches: discovering at sync time that one registry received another's password.)
3. **A project's selected indexes are a subset of the configured ones**, and deselecting removes only the association, not the index. (Catches: deleting shared configuration from a per-project screen.)
4. **The credential is never rendered.** (Catches: displaying what was stored.)

---

### Task 5: The monorepo member checklist

**Files:** create `crates/bombadil-gui/src/members_editor.rs`; modify `app.rs`

> Spec §5a: detect multiple `pyproject.toml`, let the user choose which are installed in editable mode, **all selected by default**, plus arbitrary outside projects.

**Criteria — each needs a mutation proof.**

1. **Installed-editable state is joined on PATH, not name.** `editable_project_location` is a path; a member with no `[project].name` has no name to join on and cannot be passed to `--package`. (Catches: a join that silently drops nameless members — the case the monorepo plan explicitly supports.)
2. **All discovered members are selected by default.** (Catches: an empty default, which for a workspace means `uv sync` installs nothing.)
3. **`members::effective` decides what is selected**, not a second copy of the precedence rule in the GUI. (Catches: the drift this project has already been bitten by — a rule implemented twice.)
4. **Deselecting a member and syncing removes it**, and the UI says so before the user confirms. (Catches: silent removal, verified real: re-syncing with a smaller `--package` set removes the deselected members.)
5. **A member excluded by `[tool.uv.workspace] exclude` is not offered.** Passing it to `--package` fails and **aborts the entire sync**. (Catches: one excluded member breaking every sync.)

---

### Task 6: Settings

**Files:** create `crates/bombadil-gui/src/settings_editor.rs`; modify `app.rs`

> Spec §5: `uv_source` (bundled, from `PATH`, or a specific binary) and `default_venv_location`.

**Criteria — each needs a mutation proof.**

1. **Changing `uv_source` persists and takes effect on the next resolve**, without restarting. (Catches: a setting read once at boot.)
2. **Choosing a specific uv binary validates it before saving**, through `job::run`, and reports a version mismatch against `MIN_UV_VERSION`. (Catches: saving a path that fails later at spawn time with something cryptic.)
3. **Changing `default_venv_location` changes the resolved path shown for projects using the default**, and does not move any existing venv. (Catches: a settings change silently relocating environments.)

---

### Task 7: The carried debts

**Files:** modify `crates/bombadil-core/src/venv_path.rs`, `crates/bombadil-gui/src/app.rs`, `context_menu.rs`

**Three items, in this order.**

**7a — a delete-venv primitive in core, with its own tests.** This is destructive, so it belongs in core where it can be tested properly, not improvised in a GUI task.

- It must refuse a path that is not a virtual environment. Use the same `pyvenv.cfg` test `venv_path::probe` uses — uv writes `version_info`, the stdlib writes `version`, and `probe` already handles both.
- It must refuse a path outside the resolved venv location for that project.
- Test both refusals with `tempfile`, and test that a real venv-shaped directory is removed.

**Criteria mutation proofs are mandatory here.** A test that passes for the wrong reason on a delete primitive destroys a user's environment.

**7b — Recreate venv in the context menu**, wired to 7a through `job::run`, with a confirmation step. It must state what will be deleted before doing it.

**7c — Open folder**, non-destructive, via `CommandRunner::spawn` (`xdg-open`/`open`/`explorer`) inside `job::run`. Follow how `open_terminal` does it.

**7d — `run_open_terminal` must take a `scratch_dir`** rather than writing its init file into the real temp dir. `terminal::open::open` already accepts one; only the GUI caller needs threading. This closes a standing violation of the no-filesystem test constraint.

---

## Done when

- `cargo test --workspace --locked` passes on Linux, macOS and Windows.
- `cargo clippy --all-targets --all-features --locked -- -D warnings` and `cargo deny check` are clean.
- The `msrv` job at 1.88.0 passes.
- No test opens a window or file dialog, spawns a process, touches the keychain, reaches the network, or reads the filesystem outside `tempfile`.
- No secret appears in `Config`, a `Debug` render, or any rendered text.
- Every core call that spawns a process goes through `job::run`.

## What remains after this plan

- **Streaming uv output.** Spec §8 says the drawer "streams live uv output", but `CommandRunner::run` is buffered until exit. This needs a streaming runner in core — a plan of its own, not a patch.
- **Spec §9's "Copy failure report"** action, and a user-facing drawer expand control.
- **`view` is untested by convention**, and one mutation proved the cost: deleting a render line passed every test. A pure "what lines does this show" function per module would close it.
- **Two Windows `TODO(windows)` items in core**: Git Bash detection is likely inverted, and a Windows venv path is unusable inside a Git Bash `PATH`.
- **The local `x86_64-pc-windows-gnu` cross-check no longer runs** — `bombadil-uv-bin` pins the `msvc` triple, so its build script panics first. Windows CI is the only Windows signal.
