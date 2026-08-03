# Several Environments Per Project Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A project can hold several virtual environments — a 3.11 and a 3.12, say — with one active. Sync, Open terminal, Recreate and the drift glyph all act on the active one, and the sidebar shows the rest as a tree.

**Architecture:** `Project::venv: VenvLocation` becomes `Project::environments: Vec<Environment>` plus an `active: VenvLocation` naming one of them. Each `Environment` carries its own `PythonPin`, which is what makes having more than one worth doing. `SCHEMA_VERSION` goes to 2 and a version-1 config migrates in memory to exactly the environment it already had.

**Tech Stack:** Rust edition 2024, iced 0.14, `bombadil-core` (no iced).

## Global Constraints

- **No new dependency.**
- **`bombadil-core` must not depend on `iced`.**
- **`view` is untested by convention** (see `bombadil-gui/src/main.rs`). The tree ships without mutation proofs; everything behind it does not.
- **A secret must never enter a `Message`**, which derives `Debug`.
- **`boot` is rationed** — see `theme`'s module doc.
- **Never assume a path separator or a line ending in an assertion.** Three CI failures on this project came from exactly that; the most recent was a fixture split on `"\n\n"` that matched nothing under a CRLF checkout.
- Clippy denies `derivable_impls` and `field_reassign_with_default`. `just ci` must pass.

## This supersedes part of PRs #20 and #21

`Project::python: Override<PythonPin>` is **removed**. The pin moves onto each
environment. `Settings::python` survives with a changed meaning: it is the pin
a newly added environment starts with, not a value projects inherit at sync
time.

Everything else from those PRs stands — `Override<T>`, `resolve::effective`
for uv and terminal, `interpreter::satisfies_pin`, and `ConfirmPinChange`,
which matters more here because a project now has several environments to lose
one of.

---

### Task 1: The model, and a version-1 config that still loads

**Files:**
- Create: `crates/bombadil-core/src/model/environment.rs`
- Create: `crates/bombadil-core/tests/fixtures/full-v1.toml`
- Modify: `crates/bombadil-core/src/model/{mod.rs,project.rs,settings.rs}`, `crates/bombadil-core/src/store/mod.rs`

**Interfaces:**
- Produces: `Environment { location: VenvLocation, python: PythonPin }`; `Project::{environments, active, expanded}`; `migrate(config: &mut Config, from: u32)`; `CURRENT_SCHEMA_VERSION = 2`.

> The riskiest task in the plan and the reason it is first and alone: a
> migration that loses an environment loses the user's account of where their
> packages are. It gets a real version-1 file to migrate, not a hand-built
> `Config`, because a hand-built one cannot catch a field that fails to
> deserialise.

- [ ] **Step 1: Write the failing tests**

In `crates/bombadil-core/src/store/mod.rs`'s `mod tests`:

```rust
    /// A real version-1 config, from before environments were a list.
    const GOLDEN_V1: &str = include_str!("../../tests/fixtures/full-v1.toml");

    #[test]
    fn a_version_1_config_migrates_to_the_environment_it_already_had() {
        // The whole risk of this change in one test. A migration that drops
        // an environment drops the user's account of where their packages
        // are, and they would find out at the next Sync by watching uv build
        // a new one somewhere else.
        let loaded: Config = toml::from_str(GOLDEN_V1).expect("a v1 config must parse");
        let migrated = migrate(loaded);

        let project = &migrated.projects[0];
        assert_eq!(
            project.environments.len(),
            1,
            "exactly the one environment it had, no more and no fewer"
        );
        assert_eq!(
            project.environments[0].location,
            VenvLocation::Custom {
                path: PathBuf::from("/mnt/envs/my-api")
            },
            "the location must survive verbatim"
        );
        assert_eq!(
            project.active, project.environments[0].location,
            "the one environment must be the active one"
        );
        assert_eq!(migrated.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn a_version_1_project_pin_becomes_its_environments_pin() {
        // `Project::python` is removed by this change. Dropping it silently
        // would unpin an environment the user had pinned, and the next Sync
        // would build it on whatever uv chose.
        let loaded: Config = toml::from_str(GOLDEN_V1).expect("a v1 config must parse");
        let migrated = migrate(loaded);

        assert_eq!(
            migrated.projects[0].environments[0].python,
            PythonPin::Version("3.11".into()),
            "the project's pin must move onto its environment, not vanish"
        );
    }

    #[test]
    fn a_version_2_config_is_left_alone() {
        // Migration must be idempotent: `Store::open` runs it on every load,
        // including on files it wrote itself.
        let already: Config = toml::from_str(GOLDEN_FIXTURE).expect("v2 parses");
        let migrated = migrate(already.clone());
        assert_eq!(migrated, already);
    }

    #[test]
    fn a_migrated_project_starts_expanded() {
        // After migration every project has exactly one environment, and a
        // collapsed default would hide the thing this feature exists to show.
        let loaded: Config = toml::from_str(GOLDEN_V1).unwrap();
        assert!(migrate(loaded).projects[0].expanded);
    }
```

In `crates/bombadil-core/src/model/environment.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_active_environment_is_found_by_location() {
        let a = Environment {
            location: VenvLocation::Alongside,
            python: PythonPin::Unpinned,
        };
        let b = Environment {
            location: VenvLocation::Custom {
                path: std::path::PathBuf::from("/envs/b"),
            },
            python: PythonPin::Version("3.11".into()),
        };
        let project = Project {
            environments: vec![a.clone(), b.clone()],
            active: b.location.clone(),
            ..Project::default()
        };

        assert_eq!(project.active_environment(), Some(&b));
    }

    #[test]
    fn an_active_that_names_nothing_falls_back_to_the_first() {
        // `active` is a location, not an index, so removing an environment
        // can leave it naming one that is gone. Falling back keeps every
        // caller -- sync, the terminal, the drift glyph -- working against a
        // real directory rather than each inventing its own recovery.
        let a = Environment {
            location: VenvLocation::Alongside,
            python: PythonPin::Unpinned,
        };
        let project = Project {
            environments: vec![a.clone()],
            active: VenvLocation::Custom {
                path: std::path::PathBuf::from("/gone"),
            },
            ..Project::default()
        };

        assert_eq!(project.active_environment(), Some(&a));
    }

    #[test]
    fn a_project_with_no_environments_has_no_active_one() {
        // Only reachable between removing the last environment and adding
        // another. Every caller has to handle it, so it is `None` rather than
        // a synthesised default nobody chose.
        assert_eq!(Project::default().active_environment(), None);
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p bombadil-core --offline migrate` and `cargo test -p bombadil-core --offline environment`
Expected: FAIL to compile — none of `Environment`, `migrate`, `active_environment` exist.

- [ ] **Step 3: Write `Environment`**

`crates/bombadil-core/src/model/environment.rs`:

```rust
//! One virtual environment belonging to a project.
//!
//! Identified by its `location` and nothing else: two environments cannot
//! share a directory, so the directory is already a unique key, and a typed
//! name would be a second identity to keep in step with the folder it points
//! at.

use super::{PythonPin, VenvLocation};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Environment {
    #[serde(default)]
    pub location: VenvLocation,
    /// The interpreter this environment is built with, and the one Sync
    /// asserts with `-p`. `Unpinned` lets uv choose, subject to the
    /// manifest's `requires-python`.
    ///
    /// Per environment rather than per project: carrying different
    /// interpreters is the reason to have more than one.
    #[serde(default)]
    pub python: PythonPin,
}
```

- [ ] **Step 4: Change `Project`**

In `crates/bombadil-core/src/model/project.rs`, **remove** `venv` and `python`, and add:

```rust
    /// Every environment configured for this project. Never empty after a
    /// load, because migration synthesises one -- but it can be emptied by
    /// removing the last, so `active_environment` still answers `None`.
    #[serde(default)]
    pub environments: Vec<Environment>,
    /// Which environment Sync, Open terminal, Recreate and the sidebar glyph
    /// act on.
    ///
    /// The location rather than an index, for the reason `ConfirmRecreate`
    /// documents at length: an index is only meaningful against the list as
    /// it stood, and this list is edited while the application is running.
    #[serde(default)]
    pub active: VenvLocation,
    /// Whether this project's environments are shown in the sidebar tree.
    ///
    /// Persisted, so a project you collapse stays collapsed. Defaults to
    /// *expanded*: after migration every project has exactly one
    /// environment, and a collapsed default would hide the thing the tree
    /// exists to show. `#[serde(default)]` on a `bool` is `false`, which is
    /// why this names a function instead.
    #[serde(default = "expanded_by_default")]
    pub expanded: bool,
```

and beside it:

```rust
fn expanded_by_default() -> bool {
    true
}

impl Project {
    /// The environment everything acts on.
    ///
    /// Falls back to the first when `active` names a location no longer in
    /// the list -- which removing an environment can leave behind. One
    /// fallback here rather than one per caller: sync, the terminal launcher
    /// and the drift glyph must not each invent their own recovery and then
    /// disagree about which environment is current.
    pub fn active_environment(&self) -> Option<&Environment> {
        self.environments
            .iter()
            .find(|environment| environment.location == self.active)
            .or_else(|| self.environments.first())
    }
}
```

`Project`'s derived `Default` needs `expanded` to be `true`, which a derive
cannot express — so `Default` becomes a hand-written impl. Clippy's
`derivable_impls` does not fire, because this is not what a derive produces:

```rust
impl Default for Project {
    fn default() -> Self {
        Self {
            id: Uuid::nil(),
            label: String::new(),
            pyproject_path: PathBuf::new(),
            index_names: Vec::new(),
            env: Vec::new(),
            scripts: Vec::new(),
            members: MemberSelection::default(),
            uv_source: Override::Inherit,
            terminals: BTreeMap::new(),
            environments: Vec::new(),
            active: VenvLocation::Default,
            expanded: expanded_by_default(),
        }
    }
}
```

- [ ] **Step 5: Write the migration**

In `crates/bombadil-core/src/store/mod.rs`:

```rust
/// Brings a config forward to `CURRENT_SCHEMA_VERSION`.
///
/// Version 1 held one environment per project, in `Project::venv`, with the
/// pin on the project. Version 2 holds a list with the pin on each
/// environment. The two fields v1 used are read through a shim struct rather
/// than kept on `Project`: leaving them there would mean two places a
/// location could be written and two answers to which is current.
///
/// Idempotent, because `Store::open` runs it on every load including files it
/// wrote itself.
pub fn migrate(mut config: Config) -> Config {
    for project in &mut config.projects {
        if project.environments.is_empty() {
            project.environments.push(Environment {
                location: project.legacy_venv.clone().unwrap_or_default(),
                python: project.legacy_python.clone().unwrap_or_default(),
            });
            project.active = project.environments[0].location.clone();
        }
    }
    config.schema_version = CURRENT_SCHEMA_VERSION;
    config
}
```

`legacy_venv` and `legacy_python` are `#[serde(default, rename = "venv")]` and
`#[serde(default, rename = "python")]` `Option` fields on `Project`, marked
`#[serde(skip_serializing)]` so a version-2 file never carries them again.

- [ ] **Step 6: Call it from `Store::open`, and bump the version**

Set `CURRENT_SCHEMA_VERSION = 2` in `model/mod.rs`, and apply `migrate` to
every successfully parsed config in `Store::open` before it is returned —
including the `Fresh` path, so a default config is also normalised.

- [ ] **Step 7: Write the version-1 fixture**

`crates/bombadil-core/tests/fixtures/full-v1.toml`: a copy of `full.toml` as
it stood before this change — `schema_version = 1`, `[projects.venv]` with a
`custom` path of `/mnt/envs/my-api`, `[projects.python]` set to
`{ kind = "set", value = { kind = "version", value = "3.11" } }`, and no
`[[projects.environments]]`. Hand-authored, like its twin, and with a header
comment saying it must never be "fixed up" to the current schema — its whole
job is to be old.

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p bombadil-core --offline`
Expected: PASS. Fix the call sites the removed `venv`/`python` fields break;
`venv_path::resolve` is Task 2, so give it `project.active_environment()` for
now.

- [ ] **Step 9: Prove the migration discriminates**

Change `migrate` to push `Environment::default()` instead of reading
`legacy_venv`.
Run: `cargo test -p bombadil-core --offline`
Expected: FAIL — `a_version_1_config_migrates_to_the_environment_it_already_had`
and `a_version_1_project_pin_becomes_its_environments_pin`. Restore it.

- [ ] **Step 10: Commit**

```bash
git add crates/
git commit -m "feat(core): several environments per project, and a v1 migration"
```

---

### Task 2: Everything resolves against the active environment

**Files:**
- Modify: `crates/bombadil-core/src/venv_path.rs`, `crates/bombadil-core/src/resolve.rs`, `crates/bombadil-gui/src/app.rs`

**Interfaces:**
- Consumes: `Environment`, `Project::active_environment` (Task 1).
- Produces: `venv_path::resolve(project, environment, settings)`; `resolve::Effective` without `python`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn each_environment_resolves_to_its_own_directory() {
        // The point of the feature: two environments of one project must not
        // land on the same path, or "several environments" is one
        // environment with several names.
        let project = Project {
            pyproject_path: PathBuf::from("/p/api/pyproject.toml"),
            environments: vec![
                Environment {
                    location: VenvLocation::Alongside,
                    python: PythonPin::Unpinned,
                },
                Environment {
                    location: VenvLocation::Custom {
                        path: PathBuf::from("/envs/api-311"),
                    },
                    python: PythonPin::Version("3.11".into()),
                },
            ],
            ..Project::default()
        };
        let settings = Settings::default();

        let first = resolve(&project, &project.environments[0], &settings).unwrap();
        let second = resolve(&project, &project.environments[1], &settings).unwrap();

        assert_ne!(first, second);
        assert_eq!(second, PathBuf::from("/envs/api-311"));
    }

    #[test]
    fn an_environment_on_default_still_follows_the_global_setting() {
        // `VenvLocation::Default` keeps meaning "follow the global", now per
        // environment rather than per project.
        let project = Project {
            pyproject_path: PathBuf::from("/p/api/pyproject.toml"),
            environments: vec![Environment::default()],
            ..Project::default()
        };
        let settings = Settings {
            default_venv_location: DefaultVenvLocation::Central {
                path: PathBuf::from("/central"),
            },
            ..Settings::default()
        };

        let got = resolve(&project, &project.environments[0], &settings).unwrap();
        assert!(got.starts_with("/central"), "got {}", got.display());
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p bombadil-core --offline venv_path`
Expected: FAIL to compile — `resolve` takes two arguments.

- [ ] **Step 3: Change `venv_path::resolve`**

Take `environment: &Environment` and read `environment.location` where it read
`project.venv`. `project` stays a parameter: the project directory is still
what `Alongside` and the central hash are derived from.

- [ ] **Step 4: Drop `python` from `resolve::Effective`**

It is per environment now. `effective` keeps `uv_source` and `terminal`, and
its tests lose the three that were about the pin — those move to Task 3, where
the pin is read from the active environment instead.

- [ ] **Step 5: Fix every call site**

`venv_path::resolve` is called from `run_sync`, `run_members_sync`,
`run_recreate_venv`, `run_confirm_add_project`, `run_open_terminal`,
`probe_every_venv`, `preferences::environments_panel` and
`settings_editor::resolved_default_venv_paths`. Each gets
`project.active_environment()`, and each returns its existing "no environment"
error when that is `None`.

- [ ] **Step 6: Run the whole suite**

Run: `cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/
git commit -m "feat: resolve paths against a named environment, not a project"
```

---

### Task 3: Sync, terminal and drift act on the active environment

**Files:**
- Modify: `crates/bombadil-gui/src/app.rs`, `crates/bombadil-gui/src/sidebar.rs`

**Interfaces:**
- Produces: `sidebar::Entry` gaining `environments: Vec<EnvironmentRow>`; `sidebar::rolled_up(entry) -> State`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn sync_pins_from_the_active_environment_not_the_project() {
        // The pin moved onto the environment, so a project with a 3.11 and a
        // 3.12 environment must build each with its own interpreter.
        let project = Project {
            environments: vec![
                Environment {
                    location: VenvLocation::Alongside,
                    python: PythonPin::Version("3.11".into()),
                },
                Environment {
                    location: VenvLocation::Custom {
                        path: std::path::PathBuf::from("/envs/api-312"),
                    },
                    python: PythonPin::Version("3.12".into()),
                },
            ],
            active: VenvLocation::Custom {
                path: std::path::PathBuf::from("/envs/api-312"),
            },
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

        let _ = collect_lines(|on_line| run_sync(&project, &config, &store, &runner, on_line));

        let sync = runner
            .calls()
            .into_iter()
            .find(|c| c.args.first().map(String::as_str) == Some("sync"))
            .expect("a sync must have run");
        assert!(
            sync.args.windows(2).any(|w| w == ["-p", "3.12"]),
            "the ACTIVE environment's pin must reach uv, not the other one; got {:?}",
            sync.args
        );
    }
```

In `sidebar.rs`:

```rust
    #[test]
    fn the_projects_glyph_rolls_up_its_environments() {
        // One glyph cannot say more than "worst of these", and the tree is
        // there for the detail. Drift beats absent beats present, because
        // drift is the only one that needs the user.
        let present = EnvironmentRow {
            python_version: Some("3.12".into()),
            pin: PythonPin::Version("3.12".into()),
            ..row("/p/api/.venv")
        };
        let absent = EnvironmentRow {
            python_version: None,
            ..row("/p/api/.venv-311")
        };
        let drifted = EnvironmentRow {
            python_version: Some("3.11".into()),
            pin: PythonPin::Version("3.12".into()),
            ..row("/p/api/.venv-x")
        };

        assert_eq!(rolled_up(&[present.clone()]), State::Present);
        assert_eq!(rolled_up(&[present.clone(), absent.clone()]), State::Absent);
        assert_eq!(
            rolled_up(&[present, absent, drifted]),
            State::Drifted,
            "drift outranks absent: an environment that disagrees with its pin \
             is the one that needs the user"
        );
    }

    #[test]
    fn a_project_with_no_environments_reads_as_absent() {
        assert_eq!(rolled_up(&[]), State::Absent);
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p bombadil-gui --offline`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

`run_sync` reads `project.active_environment()` for both the path and the pin,
and returns its existing "no project directory" style error when there is none.
`sidebar::Entry` gains one `EnvironmentRow` per environment — location, probed
version, pin — and `rolled_up` folds them.

- [ ] **Step 4: Run and prove**

Run: `cargo test -p bombadil-gui --offline`, then change `rolled_up` to return
the *first* row's state rather than the worst.
Expected: FAIL — `the_projects_glyph_rolls_up_its_environments`. Restore it.

- [ ] **Step 5: Commit**

```bash
git add crates/
git commit -m "feat(gui): sync, terminal and drift act on the active environment"
```

---

### Task 4: The sidebar tree

**Files:** Modify `crates/bombadil-gui/src/sidebar.rs`, `crates/bombadil-gui/src/app.rs`

**Interfaces:**
- Produces: `Message::{ProjectExpandToggled(usize), EnvironmentActivated(usize, VenvLocation)}`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn collapsing_a_project_persists() {
        // The user asked for this: a project you collapse stays collapsed
        // across a restart, so it is a config field rather than view state.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        assert!(app.config.projects[0].expanded, "projects start expanded");

        let task = update(&mut app, Message::ProjectExpandToggled(0), unused_runner());

        assert!(!app.config.projects[0].expanded);
        assert!(task.units() > 0, "a persisted setting must be persisted");
    }

    #[test]
    fn activating_an_environment_moves_what_sync_acts_on() {
        let target = VenvLocation::Custom {
            path: std::path::PathBuf::from("/envs/api-311"),
        };
        let mut app = App::new(Config {
            projects: vec![Project {
                environments: vec![
                    Environment {
                        location: VenvLocation::Alongside,
                        python: PythonPin::Unpinned,
                    },
                    Environment {
                        location: target.clone(),
                        python: PythonPin::Version("3.11".into()),
                    },
                ],
                active: VenvLocation::Alongside,
                ..project("api")
            }],
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::EnvironmentActivated(0, target.clone()),
            unused_runner(),
        );

        assert_eq!(app.config.projects[0].active, target);
        assert_eq!(
            app.config.projects[0]
                .active_environment()
                .map(|e| e.python.clone()),
            Some(PythonPin::Version("3.11".into()))
        );
    }

    #[test]
    fn activating_an_environment_a_project_does_not_have_changes_nothing() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let before = app.config.projects[0].active.clone();

        let _ = update(
            &mut app,
            Message::EnvironmentActivated(
                0,
                VenvLocation::Custom {
                    path: std::path::PathBuf::from("/not/ours"),
                },
            ),
            unused_runner(),
        );

        assert_eq!(app.config.projects[0].active, before);
    }
```

- [ ] **Step 2–4: Run, implement, re-run**

`sidebar::view` renders a disclosure control per project and, while expanded,
one row per environment: the state glyph, the directory's last component in
Plex, its pin, and the active marker. Activating re-probes and re-fetches the
tabs, the same set `refresh_after_sync` uses — the active environment is what
those tabs describe.

- [ ] **Step 5: Prove the guard**

Drop the "does this project have it" check in `EnvironmentActivated`.
Expected: FAIL — `activating_an_environment_a_project_does_not_have_changes_nothing`.

- [ ] **Step 6: Commit**

```bash
git add crates/
git commit -m "feat(gui): the sidebar is a tree of a project's environments"
```

---

### Task 5: Managing environments in Preferences

**Files:** Modify `crates/bombadil-gui/src/preferences.rs`, `crates/bombadil-gui/src/app.rs`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn a_new_environment_starts_from_the_global_pin() {
        // `Settings::python` stops being inherited at sync time and becomes
        // the pin a new environment is created with -- the changed meaning
        // this design gives it, stated as a test so it cannot quietly revert.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            settings: Settings {
                python: PythonPin::Version("3.12".into()),
                ..Settings::default()
            },
            ..Config::default()
        });
        let id = app.config.projects[0].id;
        let before = app.config.projects[0].environments.len();

        let _ = update(&mut app, Message::EnvironmentAdded(id), unused_runner());

        let added = app.config.projects[0].environments.last().unwrap();
        assert_eq!(app.config.projects[0].environments.len(), before + 1);
        assert_eq!(added.python, PythonPin::Version("3.12".into()));
    }

    #[test]
    fn removing_the_active_environment_asks_first() {
        // It is the same act as Recreate venv: an environment stops existing.
        let mut app = App::new(Config {
            projects: vec![Project {
                environments: vec![Environment {
                    location: VenvLocation::Alongside,
                    python: PythonPin::Unpinned,
                }],
                active: VenvLocation::Alongside,
                ..project("api")
            }],
            ..Config::default()
        });
        let id = app.config.projects[0].id;

        let task = update(
            &mut app,
            Message::EnvironmentRemoveRequested(id, VenvLocation::Alongside),
            unused_runner(),
        );

        assert_eq!(task.units(), 0, "nothing may go before the user answers");
        assert_eq!(
            app.config.projects[0].environments.len(),
            1,
            "the environment must still be there until confirmed"
        );
        assert!(app.confirm_environment_removal.is_some());
    }

    #[test]
    fn removing_a_non_active_environment_leaves_the_active_one_alone() {
        let keep = VenvLocation::Alongside;
        let drop = VenvLocation::Custom {
            path: std::path::PathBuf::from("/envs/spare"),
        };
        let mut app = App::new(Config {
            projects: vec![Project {
                environments: vec![
                    Environment {
                        location: keep.clone(),
                        python: PythonPin::Unpinned,
                    },
                    Environment {
                        location: drop.clone(),
                        python: PythonPin::Unpinned,
                    },
                ],
                active: keep.clone(),
                ..project("api")
            }],
            ..Config::default()
        });
        let id = app.config.projects[0].id;

        let _ = update(
            &mut app,
            Message::EnvironmentRemoveRequested(id, drop),
            unused_runner(),
        );
        let _ = update(&mut app, Message::EnvironmentRemovalConfirmed, unused_runner());

        assert_eq!(app.config.projects[0].environments.len(), 1);
        assert_eq!(app.config.projects[0].active, keep);
    }
```

- [ ] **Step 2–4: Run, implement, re-run**

The Environments section in project scope becomes the list editor: one card per
environment with its location control and its own pin box, an "Add
environment" button, a "Make active" control, and a remove that opens
`ConfirmEnvironmentRemoval` — worded like `recreate_confirmation`, naming the
directory that stops existing.

- [ ] **Step 5: Run `just ci`, then check by hand**

Run: `cargo run -p bombadil-gui` and confirm, reporting what you saw:
1. An existing project shows exactly the environment it had before.
2. Adding a second, giving it a different pin, and activating it switches which one Sync builds.
3. Collapsing a project survives a restart.
4. Removing the active environment asks first, and cancelling removes nothing.

- [ ] **Step 6: Commit**

```bash
git add crates/
git commit -m "feat(gui): add, pin, activate and remove environments"
```

---

## Done when

- `just ci` passes on Linux, macOS and Windows.
- A version-1 config loads with exactly the environment it had, its pin intact,
  and is saved as version 2.
- Two environments of one project resolve to two directories.
- Sync builds with the active environment's pin, not another environment's.
- A collapsed project is still collapsed after a restart.
- Removing an environment asks first and names the directory.

## What this does not do

- **No running a command against a non-active environment** without switching.
- **No copying or cloning an environment.**
- **No per-environment env vars or indexes.** An environment is an interpreter
  and a directory, not a second configuration scope.
