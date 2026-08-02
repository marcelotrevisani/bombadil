# Bombadil GUI Project Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a project to Bombadil, create or adopt its virtual environment, and open a terminal in it.

**Architecture:** Extends `bombadil-gui`. Everything that spawns a process or touches the filesystem goes through the existing `job::run` seam. The file dialog uses `rfd`'s **async** API, because its blocking form would freeze the render thread. `bombadil-uv-bin` finally joins the GUI so venv creation works without uv on `PATH`.

**Tech Stack:** Rust edition 2024, `iced` 0.14, `rfd` 0.17, `bombadil-core`, `bombadil-uv-bin`.

## Scope

Plan 3b of three. **3a** delivered the shell — window, sidebar, detail header, output drawer, `job` seam, `validate()` banner. **3c** adds the four tab editors, settings, and the monorepo member checklist.

## Global Constraints

- Rust edition 2024, `rust-version = "1.88"`.
- **`bombadil-core` must not depend on `iced` or `rfd`.**
- **The only new dependencies are `rfd` and `bombadil-uv-bin`.** Anything else is an escalation.
- **No test may open a window, open a file dialog, spawn a real process, touch the real OS keychain, reach the network, or read the filesystem.**
- **Secrets must never enter a log line, a `Debug` render, or a failure report.** Redact on the job thread, before a `Message` exists — `Message` derives `Debug`.
- **Never block the render thread.** Everything that spawns a process or blocks on I/O goes through `job::run`.
- `view` is deliberately dumb and untested; logic worth testing lives in `update` or free functions.
- Clippy lints denied in CI: `derivable_impls`, `field_reassign_with_default`.

## Facts Verified Empirically Before This Plan

Checked by compiling real spikes against the real crates. Do not re-derive.

- **`rfd` resolves to 0.17.2** and adds only ~30 crates. **`cargo deny check licenses` passes against the existing policy** — no licence decision is needed, unlike iced.
- **`rfd::AsyncFileDialog::pick_file()` returns a future that `Task::perform` drives**, and compiles against iced 0.14 with no extra runtime. The blocking `rfd::FileDialog` must not be used: a modal dialog on the render thread freezes the window.
- **Core already provides everything the lifecycle needs** — `uv::command::{python_list, python_install, sync_with_python}`, `uv::results::{parse_python_list, Interpreter}`, `venv_path::{resolve, probe}`. No new core command is required.
- **uv writes `version_info` in `pyvenv.cfg`, not `version`** — `venv_path::probe` already handles both. This is how an existing venv is detected for adoption.

## A Note On Task Detail

Tasks 1–3 give literal test code. **Tasks 4–5 give acceptance criteria instead**, and their implementers write the tests to those criteria. This is deliberate, and it shifts risk onto review: **twelve tests on this project have been found passing for the wrong reason**, every one asserting something true of both the correct and the broken behaviour. So each of Tasks 4–5 must carry a **mutation proof** — break the behaviour the criterion names, confirm the test fails, restore — and each task review must check the assertion discriminates rather than merely runs.

## Debt This Plan Must Pay

Three items carried from plan 3a, all recorded in its ledger:

1. **`bombadil-uv-bin` is not a GUI dependency**, so Sync fails with no uv on `PATH` — the exact case the embedded binary exists for. Task 1.
2. **`TerminalKind::activation_support()` has no consumer.** Six of fourteen terminals degrade and three deliver no composed environment at all; without a warning a macOS user gets a terminal that looks activated and is not. Task 5.
3. **`sidebar::entries`' `venv_probe` parameter is production-dead** — the only real caller passes `|_| None`, and versions arrive via `VenvProbed`. Leftover scaffolding. Task 1 removes it.

---

### Task 1: Dependencies, the bundled uv, and the attribution surface

**Files:**
- Modify: `crates/bombadil-gui/Cargo.toml`, `crates/bombadil-gui/src/app.rs`, `crates/bombadil-gui/src/sidebar.rs`
- Create: `crates/bombadil-gui/src/about.rs`

**Interfaces:**
- Produces: `about::attribution() -> &'static str`, surfaced in `view`.

> **Why the About surface is not optional.** `bombadil-uv-bin` embeds a real uv binary, which is Apache-2.0. §4 of that licence obliges us to carry its notices in a distributed work. `crates/bombadil-uv-bin/src/lib.rs` says so directly. Adding the dependency without a surface that shows the notice is a licence violation, not a polish item.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_attribution_names_uv_and_its_licence() {
        // Apache-2.0 section 4 obliges a distributed work to carry the notice.
        // Embedding uv without this is a licence violation, not a missing nicety.
        let text = attribution();
        assert!(text.contains("uv"), "got {text}");
        assert!(text.contains("Apache-2.0") || text.contains("Apache License"), "got {text}");
    }

    #[test]
    fn the_attribution_names_the_bundled_version() {
        // A notice that does not say which version was shipped is not much of a
        // notice, and the version is already a compile-time constant.
        let text = attribution();
        assert!(
            text.contains(bombadil_uv_bin::BUNDLED_UV_VERSION),
            "must name the bundled version; got {text}"
        );
    }
}
```

Check `bombadil-uv-bin`'s real exported constant name before writing this — it may be `BUNDLED_UV_VERSION` or similar. Follow the crate, not this snippet.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p bombadil-gui about`
Expected: FAIL — `cannot find function attribution`.

- [ ] **Step 3: Implement**

Add `bombadil-uv-bin` and `rfd = "0.17"` to `crates/bombadil-gui/Cargo.toml`. Write `about.rs`, and render its text somewhere reachable in `view` — a footer line is enough for now; the point is that it ships, not that it is pretty.

Then replace `run_sync`'s stub `bundled` closure with the real one, so `UvSource::Auto` falls back to the embedded binary when `locate("uv")` finds nothing. Read how `uv::resolve` expects the closure to behave before wiring it.

Finally, remove `sidebar::entries`' production-dead `venv_probe` parameter and the `venv_path::resolve` call inside it, updating its tests. Versions arrive via `Message::VenvProbed`; the parameter is scaffolding from a superseded design.

- [ ] **Step 4: Verify and commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --workspace --locked && cargo deny check
git add crates/bombadil-gui Cargo.toml Cargo.lock
git commit -m "feat(gui): bundle uv, carry its attribution, drop dead scaffolding

Sync previously failed on any machine without uv on PATH -- the exact case
the embedded binary exists for. Embedding it obliges us under Apache-2.0
section 4 to carry uv's notice, so the About surface ships with it rather
than after it."
```

**Report the effect on build time and binary size** — the embedded blob is ~35 MB and now lands in every `cargo test -p bombadil-gui` build. If it is bad enough to hurt the CI loop, say so; that is a real cost worth knowing before 3c adds more.

---

### Task 2: The add-project dialog

**Files:**
- Create: `crates/bombadil-gui/src/add_project.rs`
- Modify: `crates/bombadil-gui/src/app.rs`

**Interfaces:**
- Consumes: `bombadil_core::pyproject`, `rfd::AsyncFileDialog`.
- Produces: `add_project::Draft { pyproject_path, name, requires_python, venv_choice }`, `add_project::draft_from(path, text) -> Draft`, `add_project::view(draft)`

> Spec §8: a file picker for the `pyproject.toml`, showing the parsed project name and `requires-python` once chosen, and a three-way venv control — settings default, alongside the manifest, or a chosen folder — **with the resolved path always displayed**.
>
> **Verified:** `rfd::AsyncFileDialog::pick_file()` returns a future `Task::perform` drives. The blocking `rfd::FileDialog` must never be used.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const MANIFEST: &str = r#"
[project]
name = "my-api"
version = "0.1.0"
requires-python = ">=3.11"
"#;

    #[test]
    fn a_chosen_manifest_shows_its_name_and_python_requirement() {
        let draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        assert_eq!(draft.name.as_deref(), Some("my-api"));
        assert_eq!(draft.requires_python.as_deref(), Some(">=3.11"));
    }

    #[test]
    fn a_manifest_without_a_name_is_still_addable() {
        // A workspace root often has no [project] table at all. Refusing it
        // would make the monorepo case unaddable through the UI.
        let draft = draft_from(Path::new("/p/root/pyproject.toml"), "[tool.uv.workspace]\nmembers = []\n");
        assert_eq!(draft.name, None);
        assert_eq!(draft.pyproject_path, Path::new("/p/root/pyproject.toml"));
    }

    #[test]
    fn unparseable_toml_does_not_lose_the_chosen_path() {
        // The user picked a file; telling them nothing and forgetting which
        // one they picked is the worst of both.
        let draft = draft_from(Path::new("/p/bad/pyproject.toml"), "this is not toml {{{");
        assert_eq!(draft.pyproject_path, Path::new("/p/bad/pyproject.toml"));
        assert_eq!(draft.name, None);
    }

    #[test]
    fn the_default_venv_choice_is_the_settings_default() {
        let draft = draft_from(Path::new("/p/my-api/pyproject.toml"), MANIFEST);
        assert_eq!(draft.venv_choice, VenvChoice::SettingsDefault);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p bombadil-gui add_project`
Expected: FAIL — `cannot find function draft_from`.

- [ ] **Step 3: Implement**

`draft_from` takes the path and the manifest **text** — not the path alone — so tests need no filesystem. Reading the file happens in `app.rs` inside `job::run`, and the dialog itself is `Task::perform` over `AsyncFileDialog`. Parse with `bombadil_core::pyproject`; a parse failure yields a draft with `None` fields rather than an error, because the user still needs to see which file they picked.

`VenvChoice` is `SettingsDefault | Alongside | Chosen(PathBuf)`. The resolved path is computed with `venv_path::resolve` for display and must update as the choice changes.

- [ ] **Step 4: Verify and commit** — as in Task 1.

---

### Task 3: Interpreter selection

**Files:**
- Create: `crates/bombadil-gui/src/interpreter.rs`
- Modify: `crates/bombadil-gui/src/add_project.rs`, `crates/bombadil-gui/src/app.rs`

**Interfaces:**
- Consumes: `uv::command::{python_list, python_install}`, `uv::results::{parse_python_list, Interpreter}`.
- Produces: `interpreter::satisfying(interpreters, requires_python) -> Vec<&Interpreter>`, `interpreter::preselect(interpreters, requires_python) -> Option<&Interpreter>`

> Spec §8: an interpreter selector **pre-selected to satisfy `requires-python`**, offering to install a suitable interpreter through uv when none is available.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bombadil_core::uv::results::Interpreter;

    fn interp(version: &str) -> Interpreter { /* follow the real struct */ }

    #[test]
    fn only_interpreters_satisfying_the_requirement_are_offered() {
        let all = vec![interp("3.10.13"), interp("3.11.9"), interp("3.12.4")];
        let got = satisfying(&all, Some(">=3.11"));
        let versions: Vec<&str> = got.iter().map(|i| i.version.as_str()).collect();
        assert_eq!(versions, vec!["3.11.9", "3.12.4"]);
    }

    #[test]
    fn no_requirement_offers_everything() {
        // A manifest without requires-python constrains nothing; filtering to
        // an empty list would make the project uncreatable.
        let all = vec![interp("3.10.13"), interp("3.12.4")];
        assert_eq!(satisfying(&all, None).len(), 2);
    }

    #[test]
    fn the_preselection_is_the_newest_satisfying_interpreter() {
        let all = vec![interp("3.11.9"), interp("3.12.4"), interp("3.10.13")];
        assert_eq!(preselect(&all, Some(">=3.11")).map(|i| i.version.as_str()), Some("3.12.4"));
    }

    #[test]
    fn nothing_satisfying_preselects_nothing_rather_than_the_wrong_one() {
        // Silently preselecting an interpreter that violates requires-python
        // produces a venv that fails later, far from the cause.
        let all = vec![interp("3.9.18")];
        assert_eq!(preselect(&all, Some(">=3.11")), None);
    }
}
```

**Check what `Interpreter` actually contains** before writing the fixture — follow the struct, not this snippet. If it carries no plain version string, adapt and say so.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p bombadil-gui interpreter`
Expected: FAIL — `cannot find function satisfying`.

- [ ] **Step 3: Implement**

Version comparison must handle `>=`, `>`, `==`, `~=` and bare versions. **Do not add a semver dependency** — Python versions are not semver, and `requires-python` uses PEP 440 syntax. A conservative parser handling the common operators is correct here; anything it cannot parse must fall back to offering **everything** rather than nothing, because an over-strict filter makes a project uncreatable while an over-permissive one merely lets uv reject it later with a clear message. Document that choice where it is made.

- [ ] **Step 4: Verify and commit** — as in Task 1.

---

### Task 4: Creating or adopting the virtual environment

**Files:**
- Modify: `crates/bombadil-gui/src/app.rs`, `crates/bombadil-gui/src/add_project.rs`

> Spec §8: when the chosen folder **already holds a virtual environment**, the dialog states it will be adopted and shows its Python version.

**Criteria — write a real test for each, with a mutation proof. The parenthetical is the bug it must catch.**

1. **A folder containing a venv is reported as adoptable, with its version.** Use `venv_path::probe`, which reads `pyvenv.cfg` and handles uv's `version_info` key as well as the stdlib's `version`. (Catches: treating an existing venv as empty and silently recreating it.)
2. **An empty folder is reported as a creation, not an adoption.** (Catches: claiming to adopt something that is not there.)
3. **Confirming the dialog appends the project to the config and persists it.** (Catches: adding to the in-memory list only, so the project vanishes on restart.)
4. **Creation dispatches `sync_with_python` through `job::run` with the composed environment and `clear_env = true`,** never inline. (Catches: the freeze the job seam exists to prevent, and the uncomposed-environment defect plan 3a shipped and had to fix.)
5. **A project whose venv already exists is added without re-running sync.** (Catches: adoption silently rebuilding an environment the user asked to keep.)

For criterion 4, reuse the shape 3a settled on: give `update` a `FakeCommandRunner` and assert no call happens synchronously, plus `Task::units() > 0`. `terminal::open::secret_values` is `pub` — use it for redaction on the job thread rather than writing another copy.

---

### Task 5: The context menu

**Files:**
- Create: `crates/bombadil-gui/src/context_menu.rs`
- Modify: `crates/bombadil-gui/src/app.rs`

> Spec §8: right-clicking a project offers **Open terminal** — a submenu of terminals actually detected on this machine, configured default first, plus a Custom entry — and Open folder, Sync, Recreate venv, and Remove from list.

**Criteria — write a real test for each, with a mutation proof.**

1. **Only detected terminals are offered.** `terminal::detect::available` takes a `CommandRunner`, so tests stay hermetic. Detection must run through `job::run`: it fires up to seven probes at startup, and a `which` walking a stale network mount is precisely what that timeout exists for. (Catches: offering a terminal that is not installed, which fails at the worst moment.)
2. **The configured default appears first.** (Catches: an arbitrary order that ignores the user's setting.)
3. **A terminal that cannot deliver activation is labelled.** `TerminalKind::activation_support()` returns `Full`, `PathOnly` or `None`. **Six of fourteen degrade and three deliver no composed environment at all** — a macOS user picking Terminal.app gets a shell that looks activated and is not. The label must distinguish the two degraded kinds, not lump them together. (Catches: a silently unactivated terminal, the exact failure plan 2c existed to prevent.)
4. **Remove from list deletes the project from the config and persists**, and does not touch the venv on disk. (Catches: deleting a user's environment when they asked only to stop tracking it.)
5. **Opening a terminal dispatches through `job::run`** and never blocks. (Catches: freezing the window for the lifetime of the terminal — `SystemCommandRunner::spawn` exists for this and must be the path taken, not `run`.)

---

## Done when

- `cargo test --workspace --locked` passes on Linux, macOS and Windows.
- `cargo clippy --all-targets --all-features --locked -- -D warnings` and `cargo deny check` are clean.
- The `msrv` job at 1.88.0 passes.
- No test opens a window, opens a file dialog, spawns a real process, touches the keychain, or reads the filesystem.
- Every core call that spawns a process goes through `job::run`.
- The About surface carries uv's Apache-2.0 notice and the bundled version.

## What plan 3c picks up

The Dependencies, Env vars, Scripts and Indexes tabs, settings, and the monorepo member checklist. It must join installed-editable state on **path**, not name — the path is exact and survives members with no `[project].name`.

Two items remain open and are not this plan's to fix:

- **The drawer does not stream.** `CommandRunner::run` is buffered until exit, so spec §8's "streams live uv output" is unfulfilled. That needs a core change — a streaming runner — and belongs in a plan of its own, not a patch.
- **Two Windows `TODO(windows)` items in `bombadil-core`:** Git Bash detection is likely inverted (WSL's `bash.exe` is on `PATH`, Git for Windows' is not), and a Windows venv path is unusable inside a Git Bash `PATH` because the drive-letter colon is that shell's separator.
