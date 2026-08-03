# Preferences (core) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A project can override the global uv source, terminal and Python version; the Python version is a pin that Sync asserts and the sidebar reports drift against — without ever deleting an environment the user did not agree to lose.

**Architecture:** `Override<T>` (`Inherit | Set(T)`) is the one way a project says "not the global value". Global holds `T`, a project holds `Override<T>`, and `resolve::effective` is the single place the two are combined — so sync, the terminal launcher, the drift check and (later) the Preferences UI cannot each derive inheritance differently. This plan is the headless half of `docs/superpowers/specs/2026-08-03-preferences-design.md`; the Preferences modal is a separate plan.

**Tech Stack:** Rust edition 2024, `rust-version = "1.88"`, serde, `bombadil-core` (no iced) and `bombadil-gui`.

## Global Constraints

- **No new dependency.** Everything here uses crates already in `Cargo.lock`.
- **`bombadil-core` must not depend on `iced`.**
- **Every new config field is `#[serde(default)]`**, so no `SCHEMA_VERSION` bump and a config written by an older build still loads.
- **A `Set(T)` variant needs `#[serde(tag = "kind", content = "value")]`** — adjacent tagging, not internal. `TerminalChoice` already documents why: a newtype variant wrapping a unit enum serialises as a bare string, which internal tagging cannot carry. `#[serde(tag = "kind")]` alone compiles and then fails at runtime.
- **Never compare a `display()`ed path to a string literal** — see `bombadil-gui/src/main.rs`'s module docs. Two Windows CI failures came from this.
- **No test may open a window, spawn a real process, touch the real keychain, or reach the network** — except the feature-gated `uv_integration` file, which may.
- Clippy lints denied in CI: `derivable_impls`, `field_reassign_with_default`. `just ci` must pass.

## Verified against a real uv 0.12.1 before this plan was written

Do not re-derive these; they are why several tasks are shaped as they are.

- `uv sync -p X` on a venv at another version **deletes it** (`Removed virtual environment at: .venv`) and builds a new one. Anything outside the lockfile is gone.
- The same pin re-run is idempotent — no removal.
- A pin *more precise* than `pyvenv.cfg` records (`-p 3.12.13` against `version_info = 3.12`) does **not** remove it. uv compares the real interpreter, not the file.
- `pyvenv.cfg` records **what was requested**: `-p 3.12` wrote `version_info = 3.12`; a system interpreter wrote `3.13.14`.
- An unsatisfiable pin exits **2** with actionable stderr and leaves the venv alone.

---

### Task 1: `Project::default()`, so adding a field is not a 64-site edit

**Files:**
- Modify: `crates/bombadil-core/src/model/project.rs`
- Modify: every file holding a `Project { .. }` literal (64 of them across 19 files; `grep -rn "Project {" --include="*.rs" crates`)

**Interfaces:**
- Produces: `impl Default for Project` (derived) and `impl Default for VenvLocation` (derived), so later tasks add fields without touching a single call site.

> A pure refactor with no behaviour change, deliberately separate from the task that adds fields. Done together, a reviewer could not tell a mechanical edit of 64 literals apart from a behavioural change hidden among them.

- [ ] **Step 1: Give `VenvLocation` a derived `Default`**

`Project`'s derive needs it. In `crates/bombadil-core/src/model/project.rs`:

```rust
/// Where a project's virtual environment lives.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VenvLocation {
    /// Follow `Settings::default_venv_location`.
    #[default]
    Default,
    /// `<project dir>/.venv`.
    Alongside,
    /// A directory the user picked explicitly.
    Custom { path: PathBuf },
}
```

`#[derive(Default)]` with `#[default]`, never a hand-written `impl`: clippy's `derivable_impls` is denied in CI.

- [ ] **Step 2: Derive `Default` on `Project`**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Project {
```

`Uuid` already implements `Default` (the nil UUID), which is exactly right for a default-constructed project: it is a test scaffold, not a project anyone registered.

- [ ] **Step 3: Verify it compiles before touching any call site**

Run: `cargo check -p bombadil-core --offline`
Expected: PASS. Nothing consumes the new impls yet.

- [ ] **Step 4: Convert every `Project { .. }` literal to use it**

For each of the 64 sites, drop the fields that are already their default and end the literal with `..Project::default()`. A typical test helper becomes:

```rust
fn project(label: &str) -> Project {
    Project {
        id: uuid::Uuid::new_v4(),
        label: label.into(),
        pyproject_path: PathBuf::from(format!("/p/{label}/pyproject.toml")),
        ..Project::default()
    }
}
```

Keep any field a site sets to a non-default value. Do **not** collapse a site that deliberately sets `venv: VenvLocation::Alongside` — `Alongside` is not the default, `Default` is.

- [ ] **Step 5: Run the whole suite**

Run: `cargo test --workspace --locked`
Expected: PASS, same count as before this task. A refactor that changes a test count changed behaviour.

- [ ] **Step 6: Commit**

```bash
git add crates/
git commit -m "refactor(core): Project::default(), so a new field is not a 64-site edit"
```

---

### Task 2: `Override<T>`, `PythonPin`, and the three new project fields

**Files:**
- Create: `crates/bombadil-core/src/model/inherit.rs`
- Modify: `crates/bombadil-core/src/model/mod.rs`, `crates/bombadil-core/src/model/settings.rs`, `crates/bombadil-core/src/model/project.rs`

**Interfaces:**
- Produces:
  - `pub enum Override<T> { Inherit, Set(T) }` with `fn as_set(&self) -> Option<&T>` and `fn resolve<'a>(&'a self, global: &'a T) -> &'a T`
  - `pub enum PythonPin { Unpinned, Version(String) }`
  - `Settings::python: PythonPin`
  - `Project::python: Override<PythonPin>`, `Project::uv_source: Override<UvSource>`, `Project::terminals: BTreeMap<Os, TerminalChoice>`

- [ ] **Step 1: Write the failing tests**

Create `crates/bombadil-core/src/model/inherit.rs` with only its test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherit_yields_the_global_value() {
        let global = "global".to_string();
        assert_eq!(Override::Inherit.resolve(&global), "global");
    }

    #[test]
    fn a_set_override_replaces_the_global_value() {
        let global = "global".to_string();
        let over = Override::Set("mine".to_string());
        assert_eq!(over.resolve(&global), "mine");
    }

    #[test]
    fn a_field_missing_from_an_older_config_reads_as_inherit() {
        // Every new field is `#[serde(default)]`, so a config written before
        // this existed must still load -- and must load as "follow the
        // global", never as an override of something the user never set.
        #[derive(serde::Deserialize)]
        struct Holder {
            #[serde(default)]
            value: Override<String>,
        }
        let holder: Holder = toml::from_str("").unwrap();
        assert_eq!(holder.value, Override::Inherit);
    }

    #[test]
    fn an_override_round_trips_through_toml() {
        // Adjacent tagging, not internal: `Set` is a newtype variant, and
        // internal tagging compiles and then fails at runtime for exactly
        // that shape -- the mistake `TerminalChoice` already documents.
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Holder {
            value: Override<String>,
        }
        let holder = Holder {
            value: Override::Set("mine".to_string()),
        };
        let text = toml::to_string(&holder).unwrap();
        assert_eq!(toml::from_str::<Holder>(&text).unwrap(), holder);
    }

    #[test]
    fn an_unpinned_python_is_not_the_same_as_inheriting_one() {
        // The three states a project needs. Without `Unpinned` as a value in
        // its own right, a project cannot say "no pin" against a global that
        // has one -- `Inherit` would hand it the global's pin right back.
        let global = PythonPin::Version("3.12".to_string());
        assert_eq!(
            Override::Set(PythonPin::Unpinned).resolve(&global),
            &PythonPin::Unpinned
        );
        assert_eq!(Override::<PythonPin>::Inherit.resolve(&global), &global);
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p bombadil-core --offline inherit`
Expected: FAIL to compile — `Override` and `PythonPin` are not defined.

- [ ] **Step 3: Write the implementation**

At the top of `crates/bombadil-core/src/model/inherit.rs`:

```rust
//! How a project says "not the global value".
//!
//! One shape for every setting that has a global default and a per-project
//! override, so a reader learns it once. The alternative -- an `Option<T>`
//! per field, meaning "inherit" by convention -- reads as "absent" and gives
//! a setting whose own value is optional nowhere to put that.

use serde::{Deserialize, Serialize};

/// A project's answer for one setting: follow the global value, or replace it.
///
/// `Default` is `Inherit`, which is what makes every new field
/// `#[serde(default)]`-able: a config written before the field existed loads
/// as "follow the global", never as an override the user never set.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Override<T> {
    #[default]
    Inherit,
    Set(T),
}

impl<T> Override<T> {
    /// The overriding value, if this is one.
    pub fn as_set(&self) -> Option<&T> {
        match self {
            Self::Set(value) => Some(value),
            Self::Inherit => None,
        }
    }

    /// This override applied to `global`.
    ///
    /// Callers go through `resolve::effective` rather than calling this
    /// directly wherever more than one setting is involved -- one call site
    /// per setting is how the answers drift apart.
    pub fn resolve<'a>(&'a self, global: &'a T) -> &'a T {
        self.as_set().unwrap_or(global)
    }
}

/// Which interpreter a project's environment is built with.
///
/// Three states are needed, not two, and `Override<PythonPin>` is how: a
/// project can inherit the global pin, set its own, or explicitly carry no
/// pin at all (`Set(Unpinned)`) against a global that has one.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PythonPin {
    /// Let uv choose, subject to the manifest's `requires-python`.
    #[default]
    Unpinned,
    /// Build with this version. May be as coarse as `3.12` or as precise as
    /// `3.12.13` -- see `interpreter::satisfies_pin` for how that is compared.
    Version(String),
}
```

- [ ] **Step 4: Register the module and re-export**

In `crates/bombadil-core/src/model/mod.rs`, alongside the existing `mod`/`pub use` lines:

```rust
mod inherit;
pub use inherit::{Override, PythonPin};
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p bombadil-core --offline inherit`
Expected: PASS, 5 tests.

- [ ] **Step 6: Add the fields**

In `crates/bombadil-core/src/model/settings.rs`, add to `Settings`:

```rust
    /// The interpreter new environments are built with, and the one Sync
    /// asserts. `Unpinned` leaves the choice to uv and the manifest's
    /// `requires-python`, which is what happened before this existed.
    #[serde(default)]
    pub python: super::PythonPin,
```

In `crates/bombadil-core/src/model/project.rs`, add to `Project`:

```rust
    /// This project's interpreter pin, or the global one.
    #[serde(default)]
    pub python: super::Override<super::PythonPin>,
    /// Which uv runs this project, or the global choice. A project pinned to
    /// an older uv is the case this exists for.
    #[serde(default)]
    pub uv_source: super::Override<super::UvSource>,
    /// Which terminal this project opens in, per OS. Empty means "follow
    /// `Config::terminal_defaults`".
    ///
    /// A map rather than an `Override<TerminalChoice>`, mirroring the global
    /// field: the terminals that exist differ per platform, so a project
    /// override of iTerm2 must not make the project unopenable on Linux.
    #[serde(default)]
    pub terminals: std::collections::BTreeMap<crate::terminal::Os, super::TerminalChoice>,
```

- [ ] **Step 7: Run the whole suite**

Run: `cargo test --workspace --locked`
Expected: PASS. No call site needed changing — that is what Task 1 bought.

- [ ] **Step 8: Commit**

```bash
git add crates/
git commit -m "feat(core): Override<T>, PythonPin, and the per-project overrides"
```

---

### Task 3: `resolve::effective`, the one place inheritance is applied

**Files:**
- Create: `crates/bombadil-core/src/resolve.rs`
- Modify: `crates/bombadil-core/src/lib.rs`

**Interfaces:**
- Consumes: `Override`, `PythonPin` from Task 2.
- Produces: `pub struct Effective { pub python: PythonPin, pub uv_source: UvSource, pub terminal: Option<TerminalChoice> }` and `pub fn effective(config: &Config, project: &Project, os: Os) -> Effective`.

> `os` is a parameter, not `Os::host()` called inside: the terminal answer differs per platform and a test must be able to ask for one it is not running on. Every other module here takes `Os` the same way.

- [ ] **Step 1: Write the failing tests**

Create `crates/bombadil-core/src/resolve.rs` with its test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Config, Project, Settings, TerminalChoice, UvSource};
    use crate::terminal::{Os, TerminalKind};

    fn config_with(settings: Settings, project: Project) -> Config {
        Config {
            settings,
            projects: vec![project],
            ..Config::default()
        }
    }

    #[test]
    fn a_project_that_overrides_nothing_gets_every_global_value() {
        let settings = Settings {
            python: PythonPin::Version("3.12".into()),
            uv_source: UvSource::Bundled,
            ..Settings::default()
        };
        let project = Project::default();
        let mut config = config_with(settings, project.clone());
        config
            .terminal_defaults
            .insert(Os::Linux, TerminalChoice::Detected(TerminalKind::Kitty));

        let got = effective(&config, &project, Os::Linux);

        assert_eq!(got.python, PythonPin::Version("3.12".into()));
        assert_eq!(got.uv_source, UvSource::Bundled);
        assert_eq!(
            got.terminal,
            Some(TerminalChoice::Detected(TerminalKind::Kitty))
        );
    }

    #[test]
    fn a_project_override_replaces_the_global_value() {
        let settings = Settings {
            python: PythonPin::Version("3.12".into()),
            uv_source: UvSource::Bundled,
            ..Settings::default()
        };
        let project = Project {
            python: Override::Set(PythonPin::Version("3.11".into())),
            uv_source: Override::Set(UvSource::FromPath),
            ..Project::default()
        };
        let config = config_with(settings, project.clone());

        let got = effective(&config, &project, Os::Linux);

        assert_eq!(got.python, PythonPin::Version("3.11".into()));
        assert_eq!(got.uv_source, UvSource::FromPath);
    }

    #[test]
    fn a_project_can_unpin_against_a_global_pin() {
        // The reason `PythonPin::Unpinned` is a value and not the absence of
        // one. `Override::Inherit` would hand the global pin straight back.
        let settings = Settings {
            python: PythonPin::Version("3.12".into()),
            ..Settings::default()
        };
        let project = Project {
            python: Override::Set(PythonPin::Unpinned),
            ..Project::default()
        };
        let config = config_with(settings, project.clone());

        assert_eq!(effective(&config, &project, Os::Linux).python, PythonPin::Unpinned);
    }

    #[test]
    fn a_terminal_override_applies_only_to_the_os_it_names() {
        // The whole reason this is a map and not an `Override`. A project
        // opened in iTerm2 on macOS must still open on Linux.
        let project = Project {
            terminals: std::collections::BTreeMap::from([(
                Os::MacOs,
                TerminalChoice::Detected(TerminalKind::ITerm2),
            )]),
            ..Project::default()
        };
        let mut config = config_with(Settings::default(), project.clone());
        config
            .terminal_defaults
            .insert(Os::Linux, TerminalChoice::Detected(TerminalKind::Kitty));

        assert_eq!(
            effective(&config, &project, Os::MacOs).terminal,
            Some(TerminalChoice::Detected(TerminalKind::ITerm2))
        );
        assert_eq!(
            effective(&config, &project, Os::Linux).terminal,
            Some(TerminalChoice::Detected(TerminalKind::Kitty)),
            "an override for another OS must not take this one's default away"
        );
    }

    #[test]
    fn nothing_configured_anywhere_leaves_the_terminal_unanswered() {
        let project = Project::default();
        let config = config_with(Settings::default(), project.clone());
        assert_eq!(effective(&config, &project, Os::Linux).terminal, None);
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p bombadil-core --offline resolve`
Expected: FAIL to compile — `effective` is not defined.

- [ ] **Step 3: Write the implementation**

At the top of `crates/bombadil-core/src/resolve.rs`:

```rust
//! The single answer to "what applies to this project".
//!
//! Every setting here has a global value and a per-project override, and
//! combining the two is exactly the kind of small rule that gets re-derived
//! at each call site and then drifts. Sync, the terminal launcher, the drift
//! check and the Preferences UI all ask here instead.
//!
//! Not every setting is in `Effective`: environment variables and indexes
//! *merge* rather than override -- a project's variables layer over the
//! global ones by key -- and `envspec::compose` already owns that. Only the
//! settings where a project replaces one value with another live here.

use crate::model::{Config, Override, Project, PythonPin, TerminalChoice, UvSource};
use crate::terminal::Os;

/// What applies to one project, after its overrides are resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    pub python: PythonPin,
    pub uv_source: UvSource,
    /// `None` when neither the project nor the global config names one --
    /// the caller decides what to do about that, and the GUI's one-press
    /// button detects rather than guessing.
    pub terminal: Option<TerminalChoice>,
}

/// Resolves `project`'s settings against `config`'s globals for `os`.
///
/// `os` is a parameter rather than `Os::host()` read inside, so a test can
/// ask about a platform it is not running on -- the same shape
/// `terminal::detect` and `terminal::open` already take.
pub fn effective(config: &Config, project: &Project, os: Os) -> Effective {
    Effective {
        python: project.python.resolve(&config.settings.python).clone(),
        uv_source: project.uv_source.resolve(&config.settings.uv_source).clone(),
        terminal: project
            .terminals
            .get(&os)
            .or_else(|| config.terminal_defaults.get(&os))
            .cloned(),
    }
}
```

`Override` is imported so the module's own doc links resolve, and because
`resolve` is the method being called on the field.

- [ ] **Step 4: Register the module**

In `crates/bombadil-core/src/lib.rs`, alongside the existing `pub mod` lines:

```rust
pub mod resolve;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p bombadil-core --offline resolve`
Expected: PASS, 5 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/
git commit -m "feat(core): resolve::effective, the one place inheritance is applied"
```

---

### Task 4: A pin is satisfied by common segments, never by string equality

**Files:**
- Modify: `crates/bombadil-gui/src/interpreter.rs`
- Modify: `crates/bombadil-gui/src/sidebar.rs`

**Interfaces:**
- Consumes: `PythonPin` from Task 2.
- Produces: `pub fn satisfies_pin(version: &str, pin: &PythonPin) -> bool` in `interpreter`; `sidebar::Entry` gains `pub pin: PythonPin` and `sidebar::state` reports `State::Drifted` for a venv that does not satisfy it.

> **This is the most important task in the plan.** `pyvenv.cfg` records what was *requested*, not a full version: `-p 3.12` writes `version_info = 3.12`. A pin of `3.12.13` against that is a match, and string equality calls it a mismatch — which does not produce a wrong glyph, it produces a destructive loop. The sidebar would show permanent drift and every Sync (Task 5) would delete and recreate a working environment.

- [ ] **Step 1: Write the failing tests**

In `crates/bombadil-gui/src/interpreter.rs`'s `mod tests`:

```rust
    #[test]
    fn a_pin_is_satisfied_by_a_version_that_only_records_fewer_segments() {
        // `pyvenv.cfg` records what was *requested*: `uv sync -p 3.12` writes
        // `version_info = 3.12`, not `3.12.13`. Verified against uv 0.12.1.
        //
        // String equality here is not a cosmetic bug. It would mark this venv
        // drifted forever, and every Sync would delete and recreate a working
        // environment -- a destructive loop out of a string comparison.
        assert!(satisfies_pin(
            "3.12",
            &PythonPin::Version("3.12.13".to_string())
        ));
    }

    #[test]
    fn a_pin_is_satisfied_by_a_version_that_records_more_segments() {
        // The other direction, for a venv built from a system interpreter:
        // uv wrote `version_info = 3.13.14` there.
        assert!(satisfies_pin(
            "3.13.14",
            &PythonPin::Version("3.13".to_string())
        ));
    }

    #[test]
    fn a_pin_is_not_satisfied_by_a_different_minor_version() {
        assert!(!satisfies_pin(
            "3.13.14",
            &PythonPin::Version("3.12".to_string())
        ));
        assert!(!satisfies_pin(
            "3.12.13",
            &PythonPin::Version("3.12.14".to_string())
        ));
    }

    #[test]
    fn an_unpinned_project_is_satisfied_by_anything() {
        assert!(satisfies_pin("3.9.1", &PythonPin::Unpinned));
        assert!(satisfies_pin("", &PythonPin::Unpinned));
    }

    #[test]
    fn a_version_that_parses_to_nothing_does_not_satisfy_a_pin() {
        // An unreadable `pyvenv.cfg` value must not read as agreement. It is
        // not drift-by-mismatch either, but the caller has a venv it cannot
        // vouch for, and saying "this matches" would be a claim from nothing.
        assert!(!satisfies_pin(
            "not-a-version",
            &PythonPin::Version("3.12".to_string())
        ));
    }
```

In `crates/bombadil-gui/src/sidebar.rs`'s `mod tests`:

```rust
    #[test]
    fn a_venv_off_its_pin_reads_as_drift() {
        let entry = Entry {
            python_version: Some("3.11.9".to_string()),
            pin: PythonPin::Version("3.12".to_string()),
            ..entry_for("api")
        };
        assert_eq!(state(&entry), State::Drifted);
    }

    #[test]
    fn a_venv_on_its_pin_is_present_even_when_it_records_fewer_segments() {
        // The destructive-loop guard, at the level the user actually sees.
        let entry = Entry {
            python_version: Some("3.12".to_string()),
            pin: PythonPin::Version("3.12.13".to_string()),
            ..entry_for("api")
        };
        assert_eq!(state(&entry), State::Present);
    }

    #[test]
    fn an_unpinned_project_still_reports_drift_against_requires_python() {
        // The pin is an addition to the existing check, not a replacement.
        let entry = Entry {
            python_version: Some("3.9.1".to_string()),
            requires_python: Some(">=3.11".to_string()),
            pin: PythonPin::Unpinned,
            ..entry_for("api")
        };
        assert_eq!(state(&entry), State::Drifted);
    }
```

Add the helper `entry_for` next to the existing test helpers in that module:

```rust
    fn entry_for(label: &str) -> Entry {
        Entry {
            label: label.to_string(),
            venv_exists: true,
            python_version: None,
            requires_python: None,
            pin: PythonPin::Unpinned,
        }
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p bombadil-gui --offline satisfies_pin` then `cargo test -p bombadil-gui --offline sidebar`
Expected: FAIL to compile — `satisfies_pin` is not defined and `Entry` has no `pin`.

- [ ] **Step 3: Implement `satisfies_pin`**

In `crates/bombadil-gui/src/interpreter.rs`:

```rust
/// Whether a venv's recorded Python `version` satisfies `pin`.
///
/// Compares only the release segments **both** versions carry, which is not a
/// convenience: `pyvenv.cfg` records what was requested, so a venv built with
/// `uv sync -p 3.12` records `version_info = 3.12` while one built from a
/// system interpreter records `3.13.14`. A pin of `3.12.13` against a
/// recorded `3.12` is a match that string equality calls a mismatch -- and
/// the cost of getting that wrong is not a wrong glyph. Sync asserts the pin
/// by passing `-p`, and uv *deletes and recreates* an environment whose
/// version differs, so a false mismatch destroys a working environment on
/// every press.
///
/// An unparseable version satisfies nothing: a venv this cannot read is not
/// one it can vouch for.
pub fn satisfies_pin(version: &str, pin: &PythonPin) -> bool {
    let PythonPin::Version(pinned) = pin else {
        return true;
    };
    let (a, b) = (release_segments(version), release_segments(pinned));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let shared = a.len().min(b.len());
    a[..shared] == b[..shared]
}
```

Import `PythonPin` at the top of the file:

```rust
use bombadil_core::model::PythonPin;
```

- [ ] **Step 4: Carry the pin into `Entry` and `state`**

In `crates/bombadil-gui/src/sidebar.rs`, add the field to `Entry`:

```rust
    /// The interpreter this project is pinned to, from
    /// `bombadil_core::resolve::effective`. `Unpinned` for a project that
    /// neither sets one nor inherits one, which is every project until the
    /// user sets one.
    pub pin: PythonPin,
```

Fill it in `entries`, which already has the `Config` it needs:

```rust
pub fn entries(config: &Config) -> Vec<Entry> {
    config
        .projects
        .iter()
        .map(|project| Entry {
            label: project.label.clone(),
            venv_exists: false,
            python_version: None,
            requires_python: None,
            pin: bombadil_core::resolve::effective(config, project, Os::host()).python,
        })
        .collect()
}
```

and extend `state`:

```rust
pub fn state(entry: &Entry) -> State {
    match &entry.python_version {
        None => State::Absent,
        Some(version) => {
            let agrees = interpreter::version_satisfies(version, entry.requires_python.as_deref())
                && interpreter::satisfies_pin(version, &entry.pin);
            if agrees { State::Present } else { State::Drifted }
        }
    }
}
```

Add the imports `use bombadil_core::model::PythonPin;` and `use bombadil_core::terminal::Os;` at the top of `sidebar.rs`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p bombadil-gui --offline` — fix any other `Entry { .. }` literal the new field broke by adding `pin: PythonPin::Unpinned`.
Expected: PASS.

- [ ] **Step 6: Prove the tests discriminate**

Replace the body of `satisfies_pin`'s comparison with string equality:

```rust
    a == b
```

Run: `cargo test -p bombadil-gui --offline`
Expected: FAIL — `a_pin_is_satisfied_by_a_version_that_only_records_fewer_segments`, `a_pin_is_satisfied_by_a_version_that_records_more_segments` and `a_venv_on_its_pin_is_present_even_when_it_records_fewer_segments`. If any of them passes, it is not testing what it claims. Restore the implementation.

- [ ] **Step 7: Commit**

```bash
git add crates/
git commit -m "feat(gui): a pin is satisfied by common segments, never by equality"
```

---

### Task 5: Sync asserts the pin, and asks before it would delete

**Files:**
- Modify: `crates/bombadil-gui/src/app.rs`
- Modify: `crates/bombadil-gui/src/context_menu.rs`

**Interfaces:**
- Consumes: `resolve::effective` (Task 3), `interpreter::satisfies_pin` (Task 4).
- Produces: `context_menu::pin_change_confirmation(label: &str, venv_path: &Path, from: &str, to: &str) -> String`; `app::sync_would_delete(effective_pin: &PythonPin, venv_version: Option<&str>) -> bool`; `run_sync` builds `uv::command::sync_with_python` when a pin resolves.

> One task, not two. `uv sync -p X` **deletes** an environment on another version, so shipping "sync asserts the pin" without the guard ships a press that silently destroys an environment. A reviewer could not approve one half and reject the other.

- [ ] **Step 1: Write the failing tests**

In `crates/bombadil-gui/src/app.rs`'s `mod tests`:

```rust
    #[test]
    fn a_pinned_project_syncs_with_that_interpreter() {
        let project = Project {
            python: bombadil_core::model::Override::Set(PythonPin::Version("3.12".into())),
            ..project("api")
        };
        let config = Config {
            projects: vec![project.clone()],
            settings: fake_uv_settings(),
            ..Config::default()
        };
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();
        version_probe(&runner);
        runner.expect(
            "uv",
            &["sync"],
            bombadil_core::process::CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let (success, _lines) =
            collect_lines(|on_line| run_sync(&project, &config, &store, &runner, on_line));

        assert!(success);
        let sync = runner
            .calls()
            .into_iter()
            .find(|c| c.args.first().map(String::as_str) == Some("sync"))
            .expect("a sync must have run");
        assert!(
            sync.args.windows(2).any(|w| w == ["-p", "3.12"]),
            "the pin must reach uv; got {:?}",
            sync.args
        );
    }

    #[test]
    fn an_unpinned_project_syncs_with_no_interpreter_argument() {
        // The pin is opt-in. An unpinned project's command must be byte for
        // byte what it was before this feature existed.
        let project = project("api");
        let config = Config {
            projects: vec![project.clone()],
            settings: fake_uv_settings(),
            ..Config::default()
        };
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();
        version_probe(&runner);
        runner.expect(
            "uv",
            &["sync"],
            bombadil_core::process::CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let _ = collect_lines(|on_line| run_sync(&project, &config, &store, &runner, on_line));

        let sync = runner
            .calls()
            .into_iter()
            .find(|c| c.args.first().map(String::as_str) == Some("sync"))
            .expect("a sync must have run");
        assert!(
            !sync.args.iter().any(|a| a == "-p"),
            "an unpinned project must not gain an interpreter argument; got {:?}",
            sync.args
        );
    }

    #[test]
    fn syncing_a_venv_that_is_off_its_pin_would_delete_it() {
        // Verified against uv 0.12.1: `uv sync -p 3.12` on a 3.13 venv prints
        // "Removed virtual environment at: .venv". Everything outside the
        // lockfile is gone. That is the same class of action as Recreate
        // venv, which this application has always guarded.
        assert!(sync_would_delete(
            &PythonPin::Version("3.12".into()),
            Some("3.13.14")
        ));
    }

    #[test]
    fn syncing_a_venv_that_is_on_its_pin_deletes_nothing() {
        assert!(!sync_would_delete(
            &PythonPin::Version("3.12.13".into()),
            Some("3.12")
        ));
    }

    #[test]
    fn syncing_without_a_pin_or_without_a_venv_deletes_nothing() {
        // No pin: uv is passed no `-p` and removes nothing. No venv: there is
        // nothing to lose, and this is the ordinary "create it" path that
        // must never be gated behind a confirmation.
        assert!(!sync_would_delete(&PythonPin::Unpinned, Some("3.13.14")));
        assert!(!sync_would_delete(&PythonPin::Version("3.12".into()), None));
    }
```

In `crates/bombadil-gui/src/context_menu.rs`'s `mod tests`:

```rust
    #[test]
    fn the_pin_change_confirmation_names_both_versions_and_the_directory() {
        // "Are you sure?" is not a confirmation. The user has to see which
        // directory stops existing and what it is being rebuilt as.
        let text = pin_change_confirmation(
            "api",
            Path::new("/home/t/.venvs/api-1f2e3d"),
            "3.13.14",
            "3.12",
        );
        assert!(text.contains("/home/t/.venvs/api-1f2e3d"), "got {text}");
        assert!(text.contains("3.13.14"), "got {text}");
        assert!(text.contains("3.12"), "got {text}");
        assert!(
            text.contains("deleted"),
            "the user must be told the environment is deleted, not rebuilt; got {text}"
        );
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p bombadil-gui --offline`
Expected: FAIL to compile — `sync_would_delete` and `pin_change_confirmation` are not defined.

- [ ] **Step 3: Write `pin_change_confirmation`**

In `crates/bombadil-gui/src/context_menu.rs`, beside `recreate_confirmation`:

```rust
/// What the user agrees to before a pinned Sync deletes an environment.
///
/// Deliberately worded like `recreate_confirmation`, because it is the same
/// action: uv does not migrate an environment to another interpreter, it
/// removes the directory and builds a new one. Verified against uv 0.12.1.
pub fn pin_change_confirmation(label: &str, venv_path: &Path, from: &str, to: &str) -> String {
    format!(
        "{label} is pinned to Python {to}, but its environment is on {from}.\n\n\
         Syncing will delete {} and build a new one on {to}. Anything installed \
         in it that is not in the lock file will be lost.\n\n\
         If the rebuild fails -- no network, a lock file that no longer resolves -- \
         the old environment has already been deleted and cannot be restored.",
        venv_path.display()
    )
}
```

- [ ] **Step 4: Write `sync_would_delete`**

In `crates/bombadil-gui/src/app.rs`, beside `run_sync`:

```rust
/// Whether pressing Sync would destroy the existing environment.
///
/// True only when there is an environment, there is a pin, and the two
/// disagree -- because that is exactly when uv removes the directory rather
/// than reusing it. `None` for `venv_version` is the ordinary create path and
/// must never be gated behind a confirmation: there is nothing to lose, and
/// making a user confirm the creation of something that does not exist yet
/// teaches them to click through the one that matters.
fn sync_would_delete(pin: &PythonPin, venv_version: Option<&str>) -> bool {
    let Some(version) = venv_version else {
        return false;
    };
    !interpreter::satisfies_pin(version, pin)
}
```

- [ ] **Step 5: Pass the pin to uv**

In `run_sync`, replace the unconditional `uv::command::sync` with the pinned form when one resolves:

```rust
    let effective = bombadil_core::resolve::effective(config, project, Os::host());
    let spec = match &effective.python {
        bombadil_core::model::PythonPin::Version(version) => {
            uv::command::sync_with_python(&resolved_uv, &ctx, version)
        }
        bombadil_core::model::PythonPin::Unpinned => uv::command::sync(&resolved_uv, &ctx),
    };
    compose_and_stream(spec, project, config, &venv_path, store, runner, on_line)
```

- [ ] **Step 6: Gate the Sync press on the confirmation**

In `update`'s `Message::SyncRequested(i)` arm, before dispatching, ask the already-probed `app.entries` cache for the venv's version and open the confirmation instead when the sync would delete:

```rust
                let pin = bombadil_core::resolve::effective(&app.config, &project, Os::host())
                    .python;
                let venv_version = app
                    .entries
                    .get(i)
                    .and_then(|entry| entry.python_version.clone());
                if sync_would_delete(&pin, venv_version.as_deref())
                    && let Ok(venv_path) = venv_path::resolve(&project, &app.config.settings)
                    && let (PythonPin::Version(to), Some(from)) = (&pin, &venv_version)
                {
                    app.confirm_pin_change = Some(context_menu::ConfirmPinChange {
                        project_id: project.id,
                        label: project.label.clone(),
                        venv_path,
                        from: from.clone(),
                        to: to.clone(),
                    });
                    return Task::none();
                }
```

`app.rs` needs `use bombadil_core::model::PythonPin;` at the top for the
pattern above; every other `bombadil_core` type in this task is already
written fully qualified.

In `crates/bombadil-gui/src/context_menu.rs`, beside `ConfirmRecreate`:

```rust
/// The pending "syncing will delete this environment" confirmation.
///
/// `project_id`, not an index, for the reason `ConfirmRecreate`'s own doc
/// gives at length: an index is only meaningful against the list as it stood
/// when the confirmation opened, and nothing here is modal.
///
/// `venv_path`, `from` and `to` are captured when it opens rather than
/// re-derived by `view`: the sentence the user agrees to and the directory
/// that gets deleted must be the same one.
pub struct ConfirmPinChange {
    pub project_id: Uuid,
    pub label: String,
    pub venv_path: PathBuf,
    /// The version the environment is on now.
    pub from: String,
    /// The version it is pinned to.
    pub to: String,
}

/// Renders it. Deliberately dumb, like `confirm_view`: the sentence worth
/// testing is `pin_change_confirmation`'s.
pub fn confirm_pin_view<'a>(confirm: &ConfirmPinChange) -> iced::Element<'a, Message> {
    iced::widget::container(
        iced::widget::column![
            iced::widget::text(pin_change_confirmation(
                &confirm.label,
                &confirm.venv_path,
                &confirm.from,
                &confirm.to,
            ))
            .size(theme::BODY),
            iced::widget::row![
                iced::widget::button(iced::widget::text("Delete and rebuild").size(theme::BODY))
                    .on_press(Message::PinChangeConfirmed)
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_danger),
                iced::widget::button(iced::widget::text("Cancel").size(theme::BODY))
                    .on_press(Message::PinChangeCancelled)
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_quiet),
            ]
            .spacing(theme::SPACE_2),
        ]
        .spacing(theme::SPACE_3),
    )
    .padding(theme::SPACE_3)
    .style(theme::panel)
    .into()
}
```

In `crates/bombadil-gui/src/app.rs`, add the field to `App` beside
`confirm_recreate`:

```rust
    /// The pending "syncing will delete this environment" confirmation.
    /// `Message::PinChangeConfirmed` is the only message that reaches a
    /// pinned sync against an existing environment.
    pub confirm_pin_change: Option<context_menu::ConfirmPinChange>,
```

initialise it to `None` in `App::new` alongside `confirm_recreate: None`, add
the two messages to `Message`:

```rust
    /// The pin-change confirmation was accepted: run the sync that deletes
    /// and rebuilds.
    PinChangeConfirmed,
    /// It was declined. Nothing runs and nothing is deleted.
    PinChangeCancelled,
```

and the two arms to `update`:

```rust
        Message::PinChangeCancelled => {
            app.confirm_pin_change = None;
        }
        Message::PinChangeConfirmed => {
            // `take` unconditionally: whether or not work is dispatched, the
            // user has answered and the dialog is done. Re-resolved by id,
            // not by an index, for the reason `ConfirmRecreate` documents.
            if let Some(confirm) = app.confirm_pin_change.take()
                && app.syncing.is_none()
                && let Some(project) = app
                    .config
                    .projects
                    .iter()
                    .find(|p| p.id == confirm.project_id)
                    .cloned()
            {
                let config = app.config.clone();
                app.syncing = Some(project.id.to_string());
                app.drawer = drawer::Drawer {
                    status: format!("syncing {}...", project.label),
                    ..drawer::Drawer::default()
                };
                return Task::stream(sync_project(project, config, runner));
            }
        }
```

and render it as a layer in `view`, beside the other overlays:

```rust
    let confirm_pin_view: iced::Element<'_, Message> = match &app.confirm_pin_change {
        Some(confirm) => context_menu::confirm_pin_view(confirm),
        None => iced::widget::text("").into(),
    };
```

```rust
    if app.confirm_pin_change.is_some() {
        layers = layers.push(shell::overlay_layer(confirm_pin_view));
    }
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p bombadil-gui --offline`
Expected: PASS.

- [ ] **Step 8: Prove the guard discriminates**

Change `sync_would_delete` to `false` unconditionally.
Run: `cargo test -p bombadil-gui --offline`
Expected: FAIL — `syncing_a_venv_that_is_off_its_pin_would_delete_it`. Restore it.

- [ ] **Step 9: Run the whole check set**

Run: `just ci`
Expected: `all clean`.

- [ ] **Step 10: Commit**

```bash
git add crates/
git commit -m "feat(gui): sync asserts the pin, and asks before it would delete"
```

---

## Done when

- `just ci` passes on Linux, macOS and Windows.
- A config written before this plan loads unchanged, with every project inheriting.
- An unpinned project's `uv sync` argv is byte for byte what it was before.
- A pin of `3.12.13` against a venv recording `3.12` is **not** drift, and the mutation proving that test discriminates has been run.
- No press of Sync deletes an environment without a confirmation naming the directory and both versions.
- An unsatisfiable pin still shows uv's own stderr in the drawer. This needs no new code — uv exits 2 and `compose_and_stream` already streams both pipes — but confirm it by hand once, since a pin is the first setting that can make uv refuse for a reason the user caused.

## What this does not do

- **No Preferences UI.** Nothing in this plan lets a user set a pin — that is the second plan. Until it lands, every project inherits `Unpinned` and behaviour is unchanged, which is what makes this plan safe to merge on its own.
- **No per-project index primary.** `Project::primary_index` is not added here; it has no consumer until the Preferences UI exists, and a field nothing reads is a field nothing tests.
- **No migration of `VenvLocation::Default`** onto `Override<T>`. It already means inherit.
