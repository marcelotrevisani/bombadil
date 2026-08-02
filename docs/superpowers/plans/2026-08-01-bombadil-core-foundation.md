# Bombadil Core Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the headless foundation of Bombadil — the persisted data model, a crash-safe config store, `pyproject.toml` parsing, and virtual-environment path resolution — with no GUI and no subprocesses.

**Architecture:** A Cargo workspace whose only member for now is `bombadil-core`, a library with no `iced` dependency and no I/O beyond reading and writing its own config file. Every function added here is pure or takes an explicit path, so the whole crate is testable on any platform without a display, a network, or a keychain.

**Tech Stack:** Rust (edition 2024), `serde` + `toml` for persistence, `uuid` for stable project identity, `sha2` for venv directory naming, `directories` for platform config paths, `thiserror` for error types, `tempfile` for atomic writes.

This is **plan 1 of 3** derived from `docs/superpowers/specs/2026-08-01-bombadil-uv-gui-design.md`. Plan 2 adds uv integration, the keychain, the environment pipeline and terminal spawning. Plan 3 adds the iced GUI. Nothing in this plan spawns a process or reads a secret.

## Global Constraints

Every task's requirements implicitly include this section.

- **Rust edition 2024**, `rust-version = "1.85"`. Verified against rustc 1.97.1. (Raised to 1.88 in plan 2's task 3, once `zip` 8.6.0's own MSRV required it — this plan's own code needed no more than 1.85.)
- **Dependency versions are pinned to those verified against crates.io on 2026-08-01:** `serde` 1.0.229 (feature `derive`), `toml` 1.1.4, `uuid` 1.24.0 (features `v4`, `serde`), `sha2` 0.11.0, `directories` 6.0.0, `thiserror` 2.0.19, `tempfile` 3.27.0. Note `toml` is 1.x and `sha2` is 0.11 — both moved APIs from the versions you may remember. The code in this plan compiles against these exact versions.
- **`bombadil-core` must not depend on `iced`.** Not now, not in later plans.
- **Never write `pyproject.toml` or `uv.lock`.** uv owns both. This crate only reads `pyproject.toml`.
- **Secrets never enter `config.toml`.** An `EnvVar` with `secret = true` always persists an empty `value`.
- **`Index.name` must match `[A-Za-z0-9_-]+`** because it becomes part of an environment variable name (`UV_INDEX_<NAME>_USERNAME`).
- **Index names are unique case-insensitively**, because uppercasing them to build environment variable names would otherwise collide.
- **Central-store venv directories are named `<project-dir-name>-<hash>`**, where `<hash>` is the first **6** hex characters of the SHA-256 of the project directory path.
- **Config file location** is `directories::ProjectDirs::from("", "", "bombadil")` → `config_dir()/config.toml`. Verified against `directories` 6.0.0's source: `~/.config/bombadil/config.toml` on Linux, `~/Library/Application Support/bombadil/config.toml` on macOS, and `%APPDATA%\bombadil\config\config.toml` on Windows — Windows' `config_dir()` has an extra `config` path segment the other two platforms don't.
- **A config with a newer `schema_version` opens read-only.** A config that fails to parse is never overwritten.
- **`CURRENT_SCHEMA_VERSION = 1`.**
- CI must pass `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` on ubuntu-latest, macos-latest and windows-latest.
- **Two clippy lints bite constantly in this codebase and are denied, not warned.** Both were confirmed to fire against these exact dependency versions:
  - `derivable_impls` — an enum whose `Default` returns a unit variant must use `#[derive(Default)]` with `#[default]` on that variant, never a hand-written `impl Default`. (`impl Default for Config` is exempt: it sets `schema_version` to `1` rather than `0`, so it is not derivable.)
  - `field_reassign_with_default` — never `let mut c = Config::default(); c.field = …`. Use struct-update syntax: `Config { field: …, ..Config::default() }`.

---

## File Structure

```
Cargo.toml                                  # workspace manifest
.github/workflows/ci.yml                    # fmt + clippy + test on 3 OSes
crates/bombadil-core/
    Cargo.toml
    src/
        lib.rs                              # module wiring + public re-exports
        error.rs                            # StoreError, PyProjectError, VenvPathError
        model/
            mod.rs                          # Config, CURRENT_SCHEMA_VERSION, re-exports
            env_var.rs                      # EnvVar
            script.rs                       # Script
            index.rs                        # Index, IndexKind, IndexAuth
            project.rs                      # Project, VenvLocation
            settings.rs                     # Settings, UvSource, DefaultVenvLocation
        store/
            mod.rs                          # Store, Loaded — load/save policy
            paths.rs                        # platform config directory
            atomic.rs                       # crash-safe file write
        validate.rs                         # whole-config invariants
        pyproject.rs                        # read-only pyproject.toml parsing
        venv_path.rs                        # venv location resolution
    tests/
        fixtures/
            full.toml                       # a fully populated config
            realistic_pyproject.toml        # PEP 621 + PEP 735 fixture
```

One responsibility per file. `model/` holds data with no behaviour; `store/` owns everything that touches the filesystem for our own config; `validate`, `pyproject` and `venv_path` are pure functions.

---

### Task 1: Workspace, model, and CI

Sets up the workspace and defines the entire persisted schema in one go, because a half-defined schema would force a migration before version 1 ships. Scaffolding and CI live here rather than in a task of their own.

**Files:**
- Create: `Cargo.toml`
- Create: `crates/bombadil-core/Cargo.toml`
- Create: `crates/bombadil-core/src/lib.rs`
- Create: `crates/bombadil-core/src/model/mod.rs`
- Create: `crates/bombadil-core/src/model/env_var.rs`
- Create: `crates/bombadil-core/src/model/script.rs`
- Create: `crates/bombadil-core/src/model/index.rs`
- Create: `crates/bombadil-core/src/model/project.rs`
- Create: `crates/bombadil-core/src/model/settings.rs`
- Create: `.github/workflows/ci.yml`
- Test: `crates/bombadil-core/src/model/mod.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: nothing.
- Produces: `bombadil_core::model::{Config, Settings, UvSource, DefaultVenvLocation, Project, VenvLocation, Index, IndexKind, IndexAuth, EnvVar, Script, CURRENT_SCHEMA_VERSION}`. `Config: Default` yields `schema_version = CURRENT_SCHEMA_VERSION` and empty collections. All model types derive `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`.

> **Note on field order:** field order in the struct below is not load-bearing for parsing — `toml` 1.1.4 reorders scalars before tables automatically on serialization. `schema_version` is declared first purely so the on-disk file is readable by a human, not because reordering would break anything.

- [ ] **Step 1: Create the workspace manifest**

`Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = ["crates/bombadil-core"]

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
license = "Apache-2.0"

[workspace.dependencies]
serde = { version = "1.0.229", features = ["derive"] }
toml = "1.1.4"
uuid = { version = "1.24.0", features = ["v4", "serde"] }
sha2 = "0.11.0"
directories = "6.0.0"
thiserror = "2.0.19"
tempfile = "3.27.0"
```

`crates/bombadil-core/Cargo.toml`:

```toml
[package]
name = "bombadil-core"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
serde.workspace = true
toml.workspace = true
uuid.workspace = true
sha2.workspace = true
directories.workspace = true
thiserror.workspace = true
tempfile.workspace = true
```

- [ ] **Step 2: Write the failing test**

Create `crates/bombadil-core/src/lib.rs`:

```rust
pub mod model;
```

Create `crates/bombadil-core/src/model/mod.rs` containing *only* the test module for now, so the test fails to compile for the right reason:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn fully_populated() -> Config {
        Config {
            schema_version: CURRENT_SCHEMA_VERSION,
            settings: Settings {
                uv_source: UvSource::Custom { path: PathBuf::from("/opt/uv/bin/uv") },
                default_venv_location: DefaultVenvLocation::Central {
                    path: PathBuf::from("/home/t/.venvs"),
                },
            },
            indexes: vec![
                Index {
                    name: "priv".into(),
                    url: "https://nexus.corp/simple".into(),
                    kind: IndexKind::Default,
                    default_for_new_projects: true,
                    auth: IndexAuth::Basic { username: "alice".into() },
                },
                Index {
                    name: "extra".into(),
                    url: "https://pypi.org/simple".into(),
                    kind: IndexKind::Extra,
                    default_for_new_projects: false,
                    auth: IndexAuth::None,
                },
            ],
            global_env: vec![EnvVar {
                key: "HTTP_PROXY".into(),
                value: "http://proxy:8080".into(),
                secret: false,
            }],
            global_scripts: vec![Script {
                path: PathBuf::from("/etc/bombadil/pre.sh"),
                args: vec!["--quiet".into()],
                enabled: true,
            }],
            projects: vec![Project {
                id: Uuid::nil(),
                label: "my-api".into(),
                pyproject_path: PathBuf::from("/proj/my-api/pyproject.toml"),
                index_names: vec!["priv".into(), "extra".into()],
                venv: VenvLocation::Custom { path: PathBuf::from("/mnt/envs/my-api") },
                env: vec![EnvVar {
                    key: "DATABASE_PASSWORD".into(),
                    value: String::new(),
                    secret: true,
                }],
                scripts: vec![Script {
                    path: PathBuf::from("./scripts/decrypt.sh"),
                    args: vec![],
                    enabled: false,
                }],
            }],
        }
    }

    #[test]
    fn fully_populated_config_round_trips_through_toml() {
        let original = fully_populated();
        let text = toml::to_string_pretty(&original).expect("serialize");
        let back: Config = toml::from_str(&text).expect("deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn default_config_round_trips() {
        let original = Config::default();
        let text = toml::to_string_pretty(&original).expect("serialize");
        let back: Config = toml::from_str(&text).expect("deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn a_minimal_file_fills_in_every_default() {
        let back: Config = toml::from_str("schema_version = 1\n").expect("deserialize");
        assert_eq!(back, Config::default());
    }

    #[test]
    fn a_secret_env_var_persists_an_empty_value() {
        // Struct-update syntax rather than `let mut c = Config::default(); c.x = ..`:
        // clippy's field_reassign_with_default rejects the latter, and CI denies warnings.
        let config = Config {
            global_env: vec![EnvVar {
                key: "DATABASE_PASSWORD".into(),
                // What a caller must store for a secret: the real value lives
                // in the keychain, added in plan 2.
                value: String::new(),
                secret: true,
            }],
            ..Config::default()
        };

        let text = toml::to_string_pretty(&config).expect("serialize");

        assert!(text.contains("secret = true"), "got:\n{text}");
        assert!(text.contains("value = \"\""), "got:\n{text}");

        let back: Config = toml::from_str(&text).expect("deserialize");
        assert_eq!(back.global_env[0].value, "");
        assert!(back.global_env[0].secret);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p bombadil-core`
Expected: FAIL — compile errors, `cannot find type Config in this scope` and similar for every model type.

- [ ] **Step 4: Write the model types**

`crates/bombadil-core/src/model/env_var.rs`:

```rust
use serde::{Deserialize, Serialize};

/// A single environment variable entry.
///
/// When `secret` is true, `value` is always empty in the persisted config: the
/// real value lives in the OS keychain under `env/<project-id>/<key>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvVar {
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub secret: bool,
}
```

`crates/bombadil-core/src/model/script.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_true() -> bool {
    true
}

/// A script run before a virtual environment is activated.
///
/// Variables it exports are captured and folded into the composed environment;
/// that behaviour is implemented in plan 2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Script {
    pub path: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}
```

`crates/bombadil-core/src/model/index.rs`:

```rust
use serde::{Deserialize, Serialize};

/// Whether an index replaces PyPI or supplements it.
///
/// Maps to `UV_DEFAULT_INDEX` and `UV_INDEX` respectively when the environment
/// is composed in plan 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexKind {
    Default,
    Extra,
}

/// How to authenticate to an index. Never holds secret material.
///
/// `Token` is expressed to uv as a username of `__token__` with the token as
/// the password, the convention PyPI-compatible registries use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IndexAuth {
    None,
    Basic { username: String },
    Token,
}

/// A named package index, defined once globally and opted into per project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    /// Must match `[A-Za-z0-9_-]+`; becomes part of `UV_INDEX_<NAME>_USERNAME`.
    pub name: String,
    pub url: String,
    pub kind: IndexKind,
    #[serde(default)]
    pub default_for_new_projects: bool,
    pub auth: IndexAuth,
}
```

`crates/bombadil-core/src/model/project.rs`:

```rust
use super::{EnvVar, Script};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Where a project's virtual environment lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VenvLocation {
    /// Follow `Settings::default_venv_location`.
    Default,
    /// `<project dir>/.venv`.
    Alongside,
    /// A directory the user picked explicitly.
    Custom { path: PathBuf },
}

/// A registered project. Identity is the `id`, not the path, so moving or
/// renaming a project directory never orphans its variables and scripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub label: String,
    pub pyproject_path: PathBuf,
    /// References `Index::name` values in `Config::indexes`.
    #[serde(default)]
    pub index_names: Vec<String>,
    pub venv: VenvLocation,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    #[serde(default)]
    pub scripts: Vec<Script>,
}
```

`crates/bombadil-core/src/model/settings.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where new virtual environments go when a project says `VenvLocation::Default`.
///
/// Use `#[derive(Default)]` with `#[default]`, not a hand-written `impl Default`:
/// clippy's `derivable_impls` lint rejects the manual form, and CI denies warnings.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DefaultVenvLocation {
    /// `<project dir>/.venv`, which is what uv does on its own.
    #[default]
    Alongside,
    /// One shared directory holding `<project-dir-name>-<hash>` subdirectories.
    Central { path: PathBuf },
}

/// Which uv binary to run. Consumed in plan 2.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UvSource {
    /// uv from `PATH` if it is at least `MIN_UV_VERSION`, otherwise the bundled copy.
    #[default]
    Auto,
    /// Always the embedded copy.
    Bundled,
    /// Always uv from `PATH`; fails loudly rather than falling back.
    FromPath,
    /// A specific binary; fails loudly if missing.
    Custom { path: PathBuf },
}

/// Application-wide settings.
///
/// Later plans add fields (theme, terminal defaults). Because every field is
/// `#[serde(default)]`, adding one needs no schema migration.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub uv_source: UvSource,
    #[serde(default)]
    pub default_venv_location: DefaultVenvLocation,
}
```

Now replace `crates/bombadil-core/src/model/mod.rs`, keeping the test module you already wrote at the bottom of the file:

```rust
mod env_var;
mod index;
mod project;
mod script;
mod settings;

pub use env_var::EnvVar;
pub use index::{Index, IndexAuth, IndexKind};
pub use project::{Project, VenvLocation};
pub use script::Script;
pub use settings::{DefaultVenvLocation, Settings, UvSource};

use serde::{Deserialize, Serialize};

/// Bumped only when a change cannot be expressed by adding `#[serde(default)]`
/// fields. A config declaring a higher version opens read-only.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// The entire persisted state of the application.
///
/// Field order is not load-bearing for parsing — `toml` 1.1.4 reorders
/// scalars before tables automatically on serialization. `schema_version` is
/// declared first purely for human readability of the on-disk file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub schema_version: u32,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub indexes: Vec<Index>,
    #[serde(default)]
    pub global_env: Vec<EnvVar>,
    #[serde(default)]
    pub global_scripts: Vec<Script>,
    #[serde(default)]
    pub projects: Vec<Project>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            settings: Settings::default(),
            indexes: Vec::new(),
            global_env: Vec::new(),
            global_scripts: Vec::new(),
            projects: Vec::new(),
        }
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p bombadil-core`
Expected: PASS — 4 tests.

- [ ] **Step 6: Add CI**

`.github/workflows/ci.yml`:

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

jobs:
  check:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - name: Format
        run: cargo fmt --all --check
      - name: Clippy
        run: cargo clippy --all-targets --all-features -- -D warnings
      - name: Test
        run: cargo test --workspace
```

- [ ] **Step 7: Verify the build is clean**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --workspace`
Expected: all three succeed with no warnings.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/bombadil-core .github/workflows/ci.yml
git commit -m "feat(core): workspace scaffold and persisted data model

Defines the whole schema at version 1 so no migration is needed before
the first release. schema_version is declared first for human
readability; toml 1.1.4 reorders scalars before tables on its own, so
field order is not load-bearing for parsing."
```

---

### Task 2: Config location and crash-safe writes

**Files:**
- Create: `crates/bombadil-core/src/error.rs`
- Create: `crates/bombadil-core/src/store/mod.rs`
- Create: `crates/bombadil-core/src/store/paths.rs`
- Create: `crates/bombadil-core/src/store/atomic.rs`
- Modify: `crates/bombadil-core/src/lib.rs`
- Test: inline `#[cfg(test)]` in `atomic.rs` and `paths.rs`

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces:
  - `bombadil_core::error::StoreError` (enum, `thiserror::Error`)
  - `bombadil_core::store::paths::default_config_dir() -> Result<PathBuf, StoreError>`
  - `bombadil_core::store::paths::config_file_in(dir: &Path) -> PathBuf`
  - `bombadil_core::store::atomic::write_atomic(path: &Path, contents: &str) -> Result<(), StoreError>`

- [ ] **Step 1: Write the failing tests**

`crates/bombadil-core/src/store/atomic.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_new_file_including_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested").join("config.toml");

        write_atomic(&target, "schema_version = 1\n").unwrap();

        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "schema_version = 1\n"
        );
    }

    #[test]
    fn overwrites_an_existing_file_completely() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");

        write_atomic(&target, "a-much-longer-first-write\n").unwrap();
        write_atomic(&target, "short\n").unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "short\n");
    }

    #[test]
    fn leaves_no_temporary_files_behind() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");

        write_atomic(&target, "one\n").unwrap();
        write_atomic(&target, "two\n").unwrap();

        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 1, "expected only config.toml, found {entries:?}");
    }
}
```

`crates/bombadil-core/src/store/paths.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_directory_resolves_on_this_platform() {
        let dir = default_config_dir().expect("every supported platform has a config dir");
        assert!(dir.ends_with("bombadil"), "got {dir:?}");
    }

    #[test]
    fn the_config_file_is_named_config_toml() {
        let file = config_file_in(std::path::Path::new("/somewhere"));
        assert_eq!(file, std::path::Path::new("/somewhere/config.toml"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p bombadil-core`
Expected: FAIL — `cannot find function write_atomic`, `cannot find function default_config_dir`, and `file not found for module store`.

- [ ] **Step 3: Write the implementation**

`crates/bombadil-core/src/error.rs`:

```rust
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not determine a configuration directory for this platform")]
    NoHomeDirectory,

    #[error("{path} has no parent directory")]
    BadPath { path: PathBuf },

    #[error("i/o error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not serialize configuration: {source}")]
    Serialize {
        #[source]
        source: toml::ser::Error,
    },

    #[error(
        "{path} was written by a newer version of Bombadil \
         (schema version {found}, this build supports {supported}); it will not be modified"
    )]
    ReadOnly {
        path: PathBuf,
        found: u32,
        supported: u32,
    },
}
```

`crates/bombadil-core/src/store/paths.rs` — put this **above** the test module you already wrote:

```rust
use crate::error::StoreError;
use std::path::{Path, PathBuf};

/// The directory holding `config.toml`.
///
/// `~/.config/bombadil` on Linux, `~/Library/Application Support/bombadil` on
/// macOS, `%APPDATA%\bombadil` on Windows.
pub fn default_config_dir() -> Result<PathBuf, StoreError> {
    directories::ProjectDirs::from("", "", "bombadil")
        .map(|dirs| dirs.config_dir().to_path_buf())
        .ok_or(StoreError::NoHomeDirectory)
}

/// The config file inside a given directory. Separate from
/// [`default_config_dir`] so tests can point at a temporary directory.
pub fn config_file_in(dir: &Path) -> PathBuf {
    dir.join("config.toml")
}
```

`crates/bombadil-core/src/store/atomic.rs` — above its test module:

```rust
use crate::error::StoreError;
use std::io::Write;
use std::path::Path;

/// Write `contents` to `path` such that a concurrent reader sees either the old
/// file or the new one, never a partial write.
///
/// The temporary file is created in the *same directory* as the target so the
/// final rename cannot cross a filesystem boundary. On Windows `persist` maps to
/// `MoveFileEx` with `MOVEFILE_REPLACE_EXISTING`, so an existing target is
/// replaced rather than causing an error.
pub fn write_atomic(path: &Path, contents: &str) -> Result<(), StoreError> {
    let dir = path.parent().ok_or_else(|| StoreError::BadPath {
        path: path.to_path_buf(),
    })?;

    std::fs::create_dir_all(dir).map_err(|source| StoreError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|source| StoreError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    tmp.write_all(contents.as_bytes())
        .map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;

    tmp.as_file().sync_all().map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    tmp.persist(path).map_err(|e| StoreError::Io {
        path: path.to_path_buf(),
        source: e.error,
    })?;

    Ok(())
}
```

`crates/bombadil-core/src/store/mod.rs`:

```rust
pub mod atomic;
pub mod paths;
```

`crates/bombadil-core/src/lib.rs`:

```rust
pub mod error;
pub mod model;
pub mod store;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p bombadil-core`
Expected: PASS — 9 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/bombadil-core/src
git commit -m "feat(core): platform config paths and crash-safe writes

The temporary file is created alongside the target so the rename never
crosses a filesystem boundary, and the fsync before rename means a power
loss cannot leave a truncated config."
```

---

### Task 3: Loading and saving, with schema version policy

The store must refuse to destroy data it does not understand. Two cases matter: a config written by a newer build, and a config that does not parse.

> **AMENDED DURING EXECUTION — the code blocks below are superseded. See commit `ca288e6`; `git show ca288e6` is authoritative.**
>
> Review of the original implementation found a real defect in this plan's own code. `Store::save` raised `StoreError::ReadOnly { path, found, supported }` for *every* non-writable cause, but that variant's message claims the file "was written by a newer version of Bombadil". For the I/O-error and version-probe-failure branches the plan hardcoded `found_version: CURRENT_SCHEMA_VERSION`, so saving over a *corrupt* config reported "schema version 1, this build supports 1" — false, and self-contradictory. It also contradicted the design spec, which requires a corrupt file to surface its parse error.
>
> The shipped code replaces `Store`'s `writable: bool` + `found_version: u32` pair with a private `Writability` enum (`Writable` | `TooNew { found }` | `Unreadable { message }`), adds `StoreError::RefusedUnreadable { path, message }`, and routes `save` to whichever error states the true cause. The seven repeated `Self { .. }` literals in `Store::open` collapse into a single private `Store::new(path, writability)`. The two "never overwritten" tests were strengthened in place: the too-new case now asserts the reported `found` version, and the unparseable case asserts `RefusedUnreadable` with a non-empty message instead of a bare `is_err()`. Branch-to-writability mapping is unchanged — exactly the same two branches remain writable.

**Files:**
- Modify: `crates/bombadil-core/src/store/mod.rs`
- Test: inline `#[cfg(test)]` in `store/mod.rs`

**Interfaces:**
- Consumes: `Config`, `CURRENT_SCHEMA_VERSION` (Task 1); `write_atomic`, `config_file_in`, `StoreError` (Task 2).
- Produces:
  - `bombadil_core::store::Loaded` — enum with variants `Ok(Config)`, `Fresh(Config)`, `ReadOnly { config: Config, found: u32 }`, `Unreadable { config: Config, message: String }`
  - `Loaded::config(&self) -> &Config`
  - `bombadil_core::store::Store` with `Store::open(path: PathBuf) -> (Store, Loaded)`, `Store::save(&self, config: &Config) -> Result<(), StoreError>`, `Store::is_writable(&self) -> bool`, `Store::path(&self) -> &Path`

> Writability is enforced by the `Store`, not by a convention the caller must remember. `save` on a read-only store returns `StoreError::ReadOnly` rather than corrupting a newer config.

- [ ] **Step 1: Write the failing tests**

Append to `crates/bombadil-core/src/store/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CURRENT_SCHEMA_VERSION, Config, DefaultVenvLocation, Settings};
    use std::path::PathBuf;

    fn temp_config_path(dir: &tempfile::TempDir) -> PathBuf {
        paths::config_file_in(dir.path())
    }

    #[test]
    fn a_missing_file_loads_as_fresh_defaults_and_is_writable() {
        let dir = tempfile::tempdir().unwrap();
        let (store, loaded) = Store::open(temp_config_path(&dir));

        assert!(matches!(loaded, Loaded::Fresh(_)));
        assert_eq!(loaded.config(), &Config::default());
        assert!(store.is_writable());
    }

    #[test]
    fn a_saved_config_loads_back_identically() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_config_path(&dir);

        let config = Config {
            settings: Settings {
                default_venv_location: DefaultVenvLocation::Central {
                    path: PathBuf::from("/home/t/.venvs"),
                },
                ..Settings::default()
            },
            ..Config::default()
        };

        let (store, _) = Store::open(path.clone());
        store.save(&config).unwrap();

        let (_, loaded) = Store::open(path);
        assert!(matches!(loaded, Loaded::Ok(_)));
        assert_eq!(loaded.config(), &config);
    }

    #[test]
    fn a_newer_schema_version_loads_read_only_and_refuses_to_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_config_path(&dir);
        let future = CURRENT_SCHEMA_VERSION + 7;
        std::fs::write(&path, format!("schema_version = {future}\n")).unwrap();

        let (store, loaded) = Store::open(path.clone());

        match loaded {
            Loaded::ReadOnly { found, .. } => assert_eq!(found, future),
            other => panic!("expected ReadOnly, got {other:?}"),
        }
        assert!(!store.is_writable());

        let err = store.save(&Config::default()).unwrap_err();
        assert!(matches!(err, StoreError::ReadOnly { .. }), "got {err:?}");

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("schema_version = {future}\n"),
            "a newer config must never be modified"
        );
    }

    #[test]
    fn an_unparseable_file_yields_defaults_and_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_config_path(&dir);
        let garbage = "this is not = = valid toml [[[\n";
        std::fs::write(&path, garbage).unwrap();

        let (store, loaded) = Store::open(path.clone());

        match &loaded {
            Loaded::Unreadable { message, .. } => assert!(!message.is_empty()),
            other => panic!("expected Unreadable, got {other:?}"),
        }
        assert_eq!(loaded.config(), &Config::default());
        assert!(!store.is_writable());
        assert!(store.save(&Config::default()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), garbage);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p bombadil-core store::tests`
Expected: FAIL — `cannot find type Store`, `cannot find type Loaded`.

- [ ] **Step 3: Write the implementation**

Replace the top of `crates/bombadil-core/src/store/mod.rs` (keeping the test module below):

```rust
pub mod atomic;
pub mod paths;

use crate::error::StoreError;
use crate::model::{CURRENT_SCHEMA_VERSION, Config};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Only enough of the file to decide whether we are allowed to understand it.
#[derive(Deserialize)]
struct VersionProbe {
    schema_version: u32,
}

/// The outcome of reading the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded {
    /// Parsed cleanly at a version we support.
    Ok(Config),
    /// No file existed yet; these are defaults.
    Fresh(Config),
    /// Written by a newer build. Usable for display, but must not be written back.
    ReadOnly { config: Config, found: u32 },
    /// Could not be parsed. Defaults are supplied and the file is left alone.
    Unreadable { config: Config, message: String },
}

impl Loaded {
    pub fn config(&self) -> &Config {
        match self {
            Loaded::Ok(c)
            | Loaded::Fresh(c)
            | Loaded::ReadOnly { config: c, .. }
            | Loaded::Unreadable { config: c, .. } => c,
        }
    }
}

/// Owns the config file path and whether writing to it is permitted.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
    writable: bool,
    found_version: u32,
}

impl Store {
    /// Read the file at `path`, returning both the store and what was found.
    ///
    /// Never fails: an unreadable or too-new file yields defaults plus a
    /// non-writable store, so the application can always start.
    pub fn open(path: PathBuf) -> (Self, Loaded) {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let store = Self { path, writable: true, found_version: CURRENT_SCHEMA_VERSION };
                return (store, Loaded::Fresh(Config::default()));
            }
            Err(e) => {
                let store = Self { path, writable: false, found_version: CURRENT_SCHEMA_VERSION };
                return (
                    store,
                    Loaded::Unreadable {
                        config: Config::default(),
                        message: e.to_string(),
                    },
                );
            }
        };

        // Check the version before trusting the rest of the file: a newer build
        // may have written fields whose meaning we would otherwise guess at.
        let found = match toml::from_str::<VersionProbe>(&text) {
            Ok(probe) => probe.schema_version,
            Err(e) => {
                let store = Self { path, writable: false, found_version: CURRENT_SCHEMA_VERSION };
                return (
                    store,
                    Loaded::Unreadable {
                        config: Config::default(),
                        message: e.to_string(),
                    },
                );
            }
        };

        match toml::from_str::<Config>(&text) {
            Ok(config) if found > CURRENT_SCHEMA_VERSION => {
                let store = Self { path, writable: false, found_version: found };
                (store, Loaded::ReadOnly { config, found })
            }
            Ok(config) => {
                let store = Self { path, writable: true, found_version: found };
                (store, Loaded::Ok(config))
            }
            // Too new to parse at all. The version already explains the
            // situation better than the parse error would, so it is discarded —
            // but the file still must not be overwritten.
            Err(_) if found > CURRENT_SCHEMA_VERSION => {
                let store = Self { path, writable: false, found_version: found };
                (
                    store,
                    Loaded::ReadOnly { config: Config::default(), found },
                )
            }
            Err(e) => {
                let store = Self { path, writable: false, found_version: found };
                (
                    store,
                    Loaded::Unreadable {
                        config: Config::default(),
                        message: e.to_string(),
                    },
                )
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Persist `config`, or refuse if this store was opened on a file we must
    /// not modify.
    pub fn save(&self, config: &Config) -> Result<(), StoreError> {
        if !self.writable {
            return Err(StoreError::ReadOnly {
                path: self.path.clone(),
                found: self.found_version,
                supported: CURRENT_SCHEMA_VERSION,
            });
        }
        let text = toml::to_string_pretty(config)
            .map_err(|source| StoreError::Serialize { source })?;
        atomic::write_atomic(&self.path, &text)
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p bombadil-core`
Expected: PASS — 13 tests.

- [ ] **Step 5: Verify clippy is clean**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/bombadil-core/src/store/mod.rs
git commit -m "feat(core): config store with a defensive schema version policy

A config written by a newer build opens read-only rather than being
silently downgraded, and an unparseable file is never overwritten. Both
are enforced by the Store rather than by caller discipline."
```

---

### Task 4: Whole-config validation

Catches the invariants that individual types cannot express, most importantly that index names stay unique *after* being uppercased into environment variable names.

**Files:**
- Create: `crates/bombadil-core/src/validate.rs`
- Modify: `crates/bombadil-core/src/lib.rs`
- Test: inline `#[cfg(test)]` in `validate.rs`

**Interfaces:**
- Consumes: `Config`, `Index`, `IndexKind`, `Project`, `EnvVar` (Task 1).
- Produces: `bombadil_core::validate::{Problem, validate}` where `validate(config: &Config) -> Vec<Problem>` and `Problem { pub message: String }`. An empty vector means the config is valid.

- [ ] **Step 1: Write the failing tests**

`crates/bombadil-core/src/validate.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Config, EnvVar, Index, IndexAuth, IndexKind, Project, VenvLocation,
    };
    use std::path::PathBuf;
    use uuid::Uuid;

    fn index(name: &str, kind: IndexKind) -> Index {
        Index {
            name: name.into(),
            url: "https://example.com/simple".into(),
            kind,
            default_for_new_projects: false,
            auth: IndexAuth::None,
        }
    }

    fn project(index_names: Vec<String>) -> Project {
        Project {
            id: Uuid::nil(),
            label: "p".into(),
            pyproject_path: PathBuf::from("/p/pyproject.toml"),
            index_names,
            venv: VenvLocation::Alongside,
            env: vec![],
            scripts: vec![],
        }
    }

    /// Struct-update syntax throughout, not `let mut c = Config::default(); c.x = ..`:
    /// clippy's `field_reassign_with_default` rejects the latter and CI denies warnings.
    fn config_with(indexes: Vec<Index>, projects: Vec<Project>, global_env: Vec<EnvVar>) -> Config {
        Config { indexes, projects, global_env, ..Config::default() }
    }

    fn messages(config: &Config) -> Vec<String> {
        validate(config).into_iter().map(|p| p.message).collect()
    }

    #[test]
    fn a_default_config_is_valid() {
        assert!(validate(&Config::default()).is_empty());
    }

    #[test]
    fn index_names_must_be_usable_in_an_environment_variable() {
        let c = config_with(vec![index("has spaces", IndexKind::Extra)], vec![], vec![]);
        assert!(
            messages(&c).iter().any(|m| m.contains("has spaces")),
            "got {:?}",
            messages(&c)
        );
    }

    #[test]
    fn index_names_collide_case_insensitively() {
        // Both would become UV_INDEX_PRIV_USERNAME.
        let c = config_with(
            vec![index("priv", IndexKind::Extra), index("PRIV", IndexKind::Extra)],
            vec![],
            vec![],
        );
        assert!(
            messages(&c).iter().any(|m| m.contains("UV_INDEX_PRIV_USERNAME")),
            "got {:?}",
            messages(&c)
        );
    }

    #[test]
    fn an_index_url_must_be_http_or_https() {
        let bad = Index {
            url: "ftp://example.com/simple".into(),
            ..index("priv", IndexKind::Extra)
        };
        let c = config_with(vec![bad], vec![], vec![]);
        assert!(messages(&c).iter().any(|m| m.contains("ftp://")));
    }

    #[test]
    fn a_project_cannot_reference_an_unknown_index() {
        let c = config_with(vec![], vec![project(vec!["nope".into()])], vec![]);
        assert!(messages(&c).iter().any(|m| m.contains("nope")));
    }

    #[test]
    fn a_project_may_select_at_most_one_default_index() {
        let c = config_with(
            vec![index("a", IndexKind::Default), index("b", IndexKind::Default)],
            vec![project(vec!["a".into(), "b".into()])],
            vec![],
        );
        assert!(
            messages(&c).iter().any(|m| m.contains("more than one default")),
            "got {:?}",
            messages(&c)
        );
    }

    #[test]
    fn one_default_and_several_extras_is_fine() {
        let c = config_with(
            vec![
                index("a", IndexKind::Default),
                index("b", IndexKind::Extra),
                index("c", IndexKind::Extra),
            ],
            vec![project(vec!["a".into(), "b".into(), "c".into()])],
            vec![],
        );
        assert!(validate(&c).is_empty(), "got {:?}", messages(&c));
    }

    #[test]
    fn an_environment_variable_key_cannot_be_empty_or_contain_equals() {
        let c = config_with(
            vec![],
            vec![],
            vec![
                EnvVar { key: String::new(), value: "x".into(), secret: false },
                EnvVar { key: "A=B".into(), value: "x".into(), secret: false },
            ],
        );
        assert_eq!(messages(&c).len(), 2, "got {:?}", messages(&c));
    }

    #[test]
    fn a_secret_variable_must_not_carry_a_value() {
        let c = config_with(
            vec![],
            vec![],
            vec![EnvVar { key: "TOKEN".into(), value: "leaked".into(), secret: true }],
        );
        assert!(messages(&c).iter().any(|m| m.contains("TOKEN")));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p bombadil-core validate`
Expected: FAIL — `cannot find function validate`, `cannot find type Problem`.

- [ ] **Step 3: Write the implementation**

Above the test module in `crates/bombadil-core/src/validate.rs`:

```rust
use crate::model::{Config, EnvVar, IndexKind};
use std::collections::HashMap;

/// One human-readable configuration problem. An empty result means valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub message: String,
}

impl Problem {
    fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

fn is_valid_index_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn check_env(problems: &mut Vec<Problem>, scope: &str, vars: &[EnvVar]) {
    for var in vars {
        if var.key.is_empty() {
            problems.push(Problem::new(format!("{scope}: an environment variable has an empty name")));
        } else if var.key.contains('=') || var.key.contains('\0') {
            problems.push(Problem::new(format!(
                "{scope}: environment variable name {:?} may not contain '=' or a null byte",
                var.key
            )));
        }
        if var.secret && !var.value.is_empty() {
            problems.push(Problem::new(format!(
                "{scope}: {} is marked secret but carries a value; secret values belong in the keychain",
                var.key
            )));
        }
    }
}

/// Check invariants that span more than one value.
pub fn validate(config: &Config) -> Vec<Problem> {
    let mut problems = Vec::new();

    // Index names become part of UV_INDEX_<NAME>_USERNAME, so they must be
    // usable in an environment variable name and unique once uppercased.
    let mut seen: HashMap<String, &str> = HashMap::new();
    for index in &config.indexes {
        if !is_valid_index_name(&index.name) {
            problems.push(Problem::new(format!(
                "index name {:?} must contain only letters, digits, '_' or '-'",
                index.name
            )));
        }
        if let Some(first) = seen.insert(index.name.to_ascii_uppercase(), &index.name) {
            problems.push(Problem::new(format!(
                "index names {:?} and {:?} collide: both become UV_INDEX_{}_USERNAME",
                first,
                index.name,
                index.name.to_ascii_uppercase()
            )));
        }
        if !(index.url.starts_with("http://") || index.url.starts_with("https://")) {
            problems.push(Problem::new(format!(
                "index {:?} has URL {:?}; only http:// and https:// are supported",
                index.name, index.url
            )));
        }
    }

    check_env(&mut problems, "global", &config.global_env);

    let by_name: HashMap<&str, IndexKind> = config
        .indexes
        .iter()
        .map(|i| (i.name.as_str(), i.kind))
        .collect();

    for project in &config.projects {
        let scope = format!("project {:?}", project.label);
        check_env(&mut problems, &scope, &project.env);

        let mut defaults = 0;
        for name in &project.index_names {
            match by_name.get(name.as_str()) {
                None => problems.push(Problem::new(format!(
                    "{scope} references index {name:?}, which is not defined"
                ))),
                Some(IndexKind::Default) => defaults += 1,
                Some(IndexKind::Extra) => {}
            }
        }
        if defaults > 1 {
            problems.push(Problem::new(format!(
                "{scope} selects more than one default index; at most one is allowed"
            )));
        }
    }

    problems
}
```

Add to `crates/bombadil-core/src/lib.rs`:

```rust
pub mod validate;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p bombadil-core`
Expected: PASS — 22 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/bombadil-core/src/validate.rs crates/bombadil-core/src/lib.rs
git commit -m "feat(core): whole-config validation

Index names are checked for case-insensitive uniqueness because they are
uppercased into UV_INDEX_<NAME>_USERNAME, so 'priv' and 'PRIV' would
otherwise silently share credentials."
```

---

### Task 5: Reading pyproject.toml

Read-only. Bombadil displays this file and never writes it.

**Files:**
- Create: `crates/bombadil-core/src/pyproject.rs`
- Create: `crates/bombadil-core/tests/fixtures/realistic_pyproject.toml`
- Modify: `crates/bombadil-core/src/lib.rs`
- Modify: `crates/bombadil-core/src/error.rs`
- Test: inline `#[cfg(test)]` in `pyproject.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `bombadil_core::pyproject::PyProject { name: Option<String>, requires_python: Option<String>, dependencies: Vec<String>, optional_dependencies: BTreeMap<String, Vec<String>>, dependency_groups: BTreeMap<String, Vec<GroupEntry>> }`
  - `bombadil_core::pyproject::GroupEntry` — enum `Requirement(String)` | `IncludeGroup(String)`
  - `bombadil_core::pyproject::parse(text: &str) -> Result<PyProject, PyProjectError>`
  - `bombadil_core::pyproject::load(path: &Path) -> Result<PyProject, PyProjectError>`
  - `bombadil_core::error::PyProjectError`

> `[dependency-groups]` is PEP 735 and its entries are **not** all strings — an entry may be a table `{ include-group = "test" }`. Deserializing into `Vec<String>` fails on any real project that uses group inclusion, which is why `GroupEntry` exists.

- [ ] **Step 1: Create the fixture**

`crates/bombadil-core/tests/fixtures/realistic_pyproject.toml`:

```toml
[project]
name = "my-api"
version = "0.1.0"
requires-python = ">=3.12"
dependencies = [
    "fastapi>=0.115",
    "httpx~=0.27",
]

[project.optional-dependencies]
cli = ["typer"]
postgres = ["asyncpg>=0.29", "sqlalchemy>=2"]

[dependency-groups]
test = ["pytest>=8", "pytest-cov"]
dev = [{ include-group = "test" }, "ruff"]

[tool.uv]
package = false
```

- [ ] **Step 2: Write the failing tests**

`crates/bombadil-core/src/pyproject.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/realistic_pyproject.toml");

    #[test]
    fn reads_name_and_requires_python() {
        let p = parse(FIXTURE).unwrap();
        assert_eq!(p.name.as_deref(), Some("my-api"));
        assert_eq!(p.requires_python.as_deref(), Some(">=3.12"));
    }

    #[test]
    fn reads_direct_dependencies_in_declaration_order() {
        let p = parse(FIXTURE).unwrap();
        assert_eq!(p.dependencies, vec!["fastapi>=0.115", "httpx~=0.27"]);
    }

    #[test]
    fn reads_optional_dependency_extras() {
        let p = parse(FIXTURE).unwrap();
        assert_eq!(p.optional_dependencies["cli"], vec!["typer"]);
        assert_eq!(
            p.optional_dependencies["postgres"],
            vec!["asyncpg>=0.29", "sqlalchemy>=2"]
        );
    }

    #[test]
    fn reads_pep735_groups_including_group_inclusion() {
        let p = parse(FIXTURE).unwrap();
        assert_eq!(
            p.dependency_groups["test"],
            vec![
                GroupEntry::Requirement("pytest>=8".into()),
                GroupEntry::Requirement("pytest-cov".into()),
            ]
        );
        assert_eq!(
            p.dependency_groups["dev"],
            vec![
                GroupEntry::IncludeGroup("test".into()),
                GroupEntry::Requirement("ruff".into()),
            ]
        );
    }

    #[test]
    fn an_almost_empty_file_is_not_an_error() {
        let p = parse("[project]\nname = \"x\"\n").unwrap();
        assert_eq!(p.name.as_deref(), Some("x"));
        assert!(p.dependencies.is_empty());
        assert!(p.requires_python.is_none());
    }

    #[test]
    fn a_file_with_no_project_table_still_parses() {
        // Valid for a uv workspace root that only carries [tool.uv].
        let p = parse("[tool.uv]\npackage = false\n").unwrap();
        assert!(p.name.is_none());
    }

    #[test]
    fn invalid_toml_is_an_error_naming_the_problem() {
        let err = parse("[project\nname =").unwrap_err();
        assert!(!err.to_string().is_empty());
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p bombadil-core pyproject`
Expected: FAIL — `cannot find function parse`, `cannot find type GroupEntry`.

- [ ] **Step 4: Write the implementation**

Above the test module in `crates/bombadil-core/src/pyproject.rs`:

```rust
use crate::error::PyProjectError;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// One entry in a PEP 735 dependency group: either a requirement string or a
/// reference to another group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupEntry {
    Requirement(String),
    IncludeGroup(String),
}

/// The parts of `pyproject.toml` Bombadil displays. Everything else is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PyProject {
    pub name: Option<String>,
    pub requires_python: Option<String>,
    pub dependencies: Vec<String>,
    pub optional_dependencies: BTreeMap<String, Vec<String>>,
    pub dependency_groups: BTreeMap<String, Vec<GroupEntry>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawGroupEntry {
    Requirement(String),
    IncludeGroup {
        #[serde(rename = "include-group")]
        include_group: String,
    },
}

#[derive(Deserialize, Default)]
struct RawProject {
    name: Option<String>,
    #[serde(rename = "requires-python")]
    requires_python: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default, rename = "optional-dependencies")]
    optional_dependencies: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize, Default)]
struct RawRoot {
    #[serde(default)]
    project: RawProject,
    #[serde(default, rename = "dependency-groups")]
    dependency_groups: BTreeMap<String, Vec<RawGroupEntry>>,
}

/// Parse the contents of a `pyproject.toml`.
pub fn parse(text: &str) -> Result<PyProject, PyProjectError> {
    let raw: RawRoot =
        toml::from_str(text).map_err(|source| PyProjectError::Parse { source })?;

    let dependency_groups = raw
        .dependency_groups
        .into_iter()
        .map(|(group, entries)| {
            let entries = entries
                .into_iter()
                .map(|e| match e {
                    RawGroupEntry::Requirement(r) => GroupEntry::Requirement(r),
                    RawGroupEntry::IncludeGroup { include_group } => {
                        GroupEntry::IncludeGroup(include_group)
                    }
                })
                .collect();
            (group, entries)
        })
        .collect();

    Ok(PyProject {
        name: raw.project.name,
        requires_python: raw.project.requires_python,
        dependencies: raw.project.dependencies,
        optional_dependencies: raw.project.optional_dependencies,
        dependency_groups,
    })
}

/// Read and parse a `pyproject.toml` from disk.
pub fn load(path: &Path) -> Result<PyProject, PyProjectError> {
    let text = std::fs::read_to_string(path).map_err(|source| PyProjectError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&text)
}
```

Add to `crates/bombadil-core/src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum PyProjectError {
    #[error("could not read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not parse pyproject.toml: {source}")]
    Parse {
        #[source]
        source: toml::de::Error,
    },
}
```

Add to `crates/bombadil-core/src/lib.rs`:

```rust
pub mod pyproject;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p bombadil-core`
Expected: PASS — 29 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/bombadil-core/src/pyproject.rs crates/bombadil-core/src/error.rs \
        crates/bombadil-core/src/lib.rs crates/bombadil-core/tests/fixtures
git commit -m "feat(core): read-only pyproject.toml parsing

PEP 735 dependency groups may contain { include-group = ... } tables as
well as requirement strings, so entries are modelled as an enum rather
than Vec<String>, which would fail on any project using group inclusion."
```

---

### Task 6: Resolving where a virtual environment lives

**Files:**
- Create: `crates/bombadil-core/src/venv_path.rs`
- Modify: `crates/bombadil-core/src/lib.rs`
- Modify: `crates/bombadil-core/src/error.rs`
- Test: inline `#[cfg(test)]` in `venv_path.rs`

**Interfaces:**
- Consumes: `Project`, `VenvLocation`, `Settings`, `DefaultVenvLocation` (Task 1).
- Produces:
  - `bombadil_core::venv_path::resolve(project: &Project, settings: &Settings) -> Result<PathBuf, VenvPathError>`
  - `bombadil_core::venv_path::central_dir_name(project_dir: &Path) -> String`
  - `bombadil_core::error::VenvPathError`

> **This function touches no filesystem.** The hash is taken over the project directory path *as written*, not over its canonicalised form, so `resolve` stays pure and testable with paths that do not exist. The trade-off is that two symlinked paths to the same project hash differently; that is acceptable because the path is what the user chose and what is displayed back to them.

- [ ] **Step 1: Write the failing tests**

`crates/bombadil-core/src/venv_path.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DefaultVenvLocation, Project, Settings, UvSource, VenvLocation};
    use uuid::Uuid;

    fn project(venv: VenvLocation) -> Project {
        Project {
            id: Uuid::nil(),
            label: "my-api".into(),
            pyproject_path: PathBuf::from("/proj/my-api/pyproject.toml"),
            index_names: vec![],
            venv,
            env: vec![],
            scripts: vec![],
        }
    }

    fn settings(default_venv_location: DefaultVenvLocation) -> Settings {
        Settings { uv_source: UvSource::Auto, default_venv_location }
    }

    #[test]
    fn alongside_puts_the_venv_next_to_the_pyproject() {
        let got = resolve(
            &project(VenvLocation::Alongside),
            &settings(DefaultVenvLocation::Alongside),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/proj/my-api/.venv"));
    }

    #[test]
    fn a_custom_location_is_used_verbatim() {
        let got = resolve(
            &project(VenvLocation::Custom { path: PathBuf::from("/mnt/fast/envs/api") }),
            &settings(DefaultVenvLocation::Central { path: PathBuf::from("/ignored") }),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/mnt/fast/envs/api"));
    }

    #[test]
    fn default_follows_the_alongside_setting() {
        let got = resolve(
            &project(VenvLocation::Default),
            &settings(DefaultVenvLocation::Alongside),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/proj/my-api/.venv"));
    }

    #[test]
    fn default_follows_the_central_setting_and_names_by_directory_plus_hash() {
        let got = resolve(
            &project(VenvLocation::Default),
            &settings(DefaultVenvLocation::Central { path: PathBuf::from("/home/t/.venvs") }),
        )
        .unwrap();

        let name = got.file_name().unwrap().to_string_lossy().into_owned();
        assert!(got.starts_with("/home/t/.venvs"), "got {got:?}");
        assert!(name.starts_with("my-api-"), "got {name}");
        assert_eq!(name.len(), "my-api-".len() + 6, "hash must be 6 chars: {name}");
        assert!(
            name["my-api-".len()..].chars().all(|c| c.is_ascii_hexdigit()),
            "got {name}"
        );
    }

    #[test]
    fn two_projects_with_the_same_directory_name_do_not_collide() {
        let a = central_dir_name(Path::new("/work/alpha/api"));
        let b = central_dir_name(Path::new("/work/beta/api"));
        assert!(a.starts_with("api-") && b.starts_with("api-"));
        assert_ne!(a, b, "different projects named 'api' must get different folders");
    }

    #[test]
    fn the_same_path_always_produces_the_same_name() {
        assert_eq!(
            central_dir_name(Path::new("/work/alpha/api")),
            central_dir_name(Path::new("/work/alpha/api")),
        );
    }

    #[test]
    fn awkward_directory_characters_are_replaced() {
        let name = central_dir_name(Path::new("/work/my project (v2)"));
        let stem = name.rsplit_once('-').unwrap().0;
        assert_eq!(stem, "my-project--v2-");
        assert!(
            name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'),
            "got {name}"
        );
    }

    #[test]
    fn a_pyproject_path_with_no_parent_is_an_error() {
        let mut p = project(VenvLocation::Default);
        p.pyproject_path = PathBuf::from("pyproject.toml");
        assert!(resolve(&p, &settings(DefaultVenvLocation::Alongside)).is_err());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p bombadil-core venv_path`
Expected: FAIL — `cannot find function resolve`, `cannot find function central_dir_name`.

- [ ] **Step 3: Write the implementation**

Above the test module in `crates/bombadil-core/src/venv_path.rs`:

```rust
use crate::error::VenvPathError;
use crate::model::{DefaultVenvLocation, Project, Settings, VenvLocation};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Characters of the project directory name kept as-is; anything else becomes '-'.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The folder name a project gets inside the central venv store:
/// `<sanitized directory name>-<first 6 hex of SHA-256 of the directory path>`.
///
/// The hash keeps two unrelated projects that happen to share a directory name
/// from colliding, while the prefix keeps the folder recognisable.
pub fn central_dir_name(project_dir: &Path) -> String {
    let stem = project_dir
        .file_name()
        .map(|n| sanitize(&n.to_string_lossy()))
        .unwrap_or_else(|| "project".to_string());

    let digest = Sha256::digest(project_dir.to_string_lossy().as_bytes());
    let hash: String = digest.iter().take(3).map(|b| format!("{b:02x}")).collect();

    format!("{stem}-{hash}")
}

/// Where this project's virtual environment lives.
///
/// Performs no filesystem access, so it is safe to call for projects whose
/// directory no longer exists.
pub fn resolve(project: &Project, settings: &Settings) -> Result<PathBuf, VenvPathError> {
    let project_dir = project
        .pyproject_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| VenvPathError::NoProjectDirectory {
            pyproject_path: project.pyproject_path.clone(),
        })?;

    Ok(match &project.venv {
        VenvLocation::Custom { path } => path.clone(),
        VenvLocation::Alongside => project_dir.join(".venv"),
        VenvLocation::Default => match &settings.default_venv_location {
            DefaultVenvLocation::Alongside => project_dir.join(".venv"),
            DefaultVenvLocation::Central { path } => path.join(central_dir_name(project_dir)),
        },
    })
}
```

Add to `crates/bombadil-core/src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum VenvPathError {
    #[error("{pyproject_path} has no parent directory, so the project directory is unknown")]
    NoProjectDirectory { pyproject_path: PathBuf },
}
```

Add to `crates/bombadil-core/src/lib.rs`:

```rust
pub mod venv_path;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p bombadil-core`
Expected: PASS — 37 tests.

> The `awkward_directory_characters_are_replaced` test asserts `"my project (v2)"` sanitizes to `"my-project--v2-"`: the space, both parentheses become `-`. If your `sanitize` produces something else, fix the implementation rather than the test — collapsing runs of `-` would make two different directory names sanitize identically, which is exactly what the hash suffix exists to prevent.

- [ ] **Step 5: Full verification**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --workspace`
Expected: all clean.

- [ ] **Step 6: Commit**

```bash
git add crates/bombadil-core/src/venv_path.rs crates/bombadil-core/src/error.rs \
        crates/bombadil-core/src/lib.rs
git commit -m "feat(core): resolve virtual environment locations

Central-store folders are named <dir>-<hash6> so two unrelated projects
named 'api' cannot collide. The hash covers the path as written rather
than its canonical form, keeping the function free of filesystem access."
```

---

## Done when

- `cargo test --workspace` passes on Linux, macOS and Windows.
- `cargo clippy --all-targets -- -D warnings` is clean.
- `bombadil-core` has no dependency on `iced` and spawns no processes.
- A `config.toml` written by this crate round-trips exactly, and a config from a newer build is never modified.

## What plan 2 picks up

The `CommandRunner` and `SecretStore` traits, the embedded uv binary and its resolution chain, the `envspec` environment pipeline, pre-activate script export capture, index environment injection, and terminal detection. `Settings` gains `terminal_defaults` there; because every field is `#[serde(default)]`, that needs no schema migration.
