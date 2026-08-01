# Bombadil uv Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `bombadil-core` the ability to find, ship, and drive the `uv` binary — every subprocess behind one testable seam, with uv embedded in the executable.

**Architecture:** A `CommandRunner` trait becomes the single seam through which any process is launched, so every layer above it is testable without spawning anything. A separate `bombadil-uv-bin` crate downloads a pinned uv release at build time, verifies its checksum, and embeds it. Argv construction and result parsing are pure functions tested against real captured uv output.

**Tech Stack:** Rust (edition 2024), `serde`/`serde_json` for uv's JSON output, `ureq` + `sha2` + `flate2`/`tar`/`zip` as build-time-only dependencies for acquiring uv.

## Scope

This is **plan 2a of 3**, and the first of three parts of the spec's "plan 2". The original plan-2 scope covered three subsystems that barely depend on each other; splitting them keeps each plan's deliverable independently testable:

- **2a (this plan)** — process execution, the embedded uv binary, uv source resolution, argv construction, result parsing.
- **2b** — `SecretStore` (OS keychain), and the `envspec` environment-composition pipeline including pre-activate script export capture and private index injection.
- **2c** — terminal detection and spawning.

Plan 1 (`bombadil-core`: `model`, `store`, `validate`, `pyproject`, `venv_path`) is merged on `main`.

## Verification already done

Every claim below was checked against a real uv 0.12.1 binary on 2026-08-01, not taken from documentation. Do not re-litigate these; do not "modernise" code that depends on them.

- uv publishes per-target release assets **with `.sha256` companion files**. Checksum file format is `<64 hex chars>  <filename>` (two spaces).
- Unix assets are `uv-<target-triple>.tar.gz`; the Windows asset is `uv-x86_64-pc-windows-msvc.zip`. Both contain a single top-level directory holding the binary — extract by matching the **file name** (`uv` / `uv.exe`), not a hardcoded path.
- `UV_PROJECT_ENVIRONMENT` still redirects the project venv on 0.12.1. Verified functionally: `uv sync` created the venv at the named directory and did **not** create `.venv`.
- Every flag this plan uses exists on 0.12.1: `uv tree --format json --depth N`, `uv pip list --format json`, `uv python list --output-format json`, `uv add --dev/--group/--optional/--frozen/--no-sync`, `uv lock -P`, `uv sync -p`, `uv venv -p`, `uv python install`.
- `uv tree --format json` emits `"schema": {"version": "preview"}` — uv declares that schema **unstable**. See Global Constraints.

## Global Constraints

Every task's requirements implicitly include this section.

- **Rust edition 2024**, `rust-version = "1.88"`. Raised from 1.85 during task 3: `zip` 8.6.0 declares MSRV 1.88, and `bombadil-uv-bin`'s let-chain in `ensure_extracted` needs it too.
- **Dependency versions**, verified on crates.io 2026-08-01: `serde_json` 1 (workspace), and build-only: `ureq` 3.3.0, `flate2` 1.1.9, `tar` 0.4.46, `zip` 8.6.0 (**not** 9.0.0-pre2, which is a prerelease). Existing pins from plan 1 are unchanged.
- **`bombadil-core` must not depend on `iced`.**
- **`BUNDLED_UV_VERSION = "0.12.1"` and `MIN_UV_VERSION = "0.12.1"`.** The spec named 0.11.32; 0.12.1 has been verified to have an identical relevant CLI surface, so the pin moves forward. `MIN_UV_VERSION` is lowered only when an older version is explicitly re-tested.
- **Never write `pyproject.toml` or `uv.lock`.** uv owns both. Bombadil only reads them and invokes uv.
- **No process is spawned outside `CommandRunner`.** Not in `uv`, not in tests, nowhere. That trait is the seam that makes everything above it testable.
- **`uv tree --format json` is a preview schema.** The parser must check `schema.version` and, when it is not the tested value, degrade to direct-dependencies-only rather than erroring or panicking. A future uv must never make the app unusable.
- **Unit tests must not download anything, spawn any process, or touch the network.** The only exception is the feature-gated integration test in Task 7.
- **Two clippy lints are DENIED in CI:** `derivable_impls` (use `#[derive(Default)]` with `#[default]`) and `field_reassign_with_default` (use struct-update syntax).
- CI must pass `cargo fmt --all --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, and `cargo test --workspace --locked` on ubuntu-latest, macos-latest and windows-latest, plus the `msrv`, `deny` and `hygiene` jobs.
- **New dependencies must pass `cargo deny check`.** Policy is in `deny.toml`; the allow list is `Apache-2.0`, `MIT`, `Unicode-3.0` with a scoped MPL-2.0 exception for `option-ext`. If a new dependency introduces another license, that is a finding to report, not a reason to widen the allow list unilaterally.

---

## File Structure

```
Cargo.toml                                    # add crates/bombadil-uv-bin to members
crates/bombadil-uv-bin/
    Cargo.toml
    uv-manifest.toml                          # pinned version + per-target sha256, committed
    build.rs                                  # download, verify, extract the uv member
    src/lib.rs                                # include_bytes! + ensure_bundled_uv()
crates/bombadil-core/
    src/
        process/
            mod.rs                            # CommandRunner trait, CommandOutcome, CommandError
            system.rs                         # SystemCommandRunner over std::process
            fake.rs                           # FakeCommandRunner for tests
        uv/
            mod.rs                            # re-exports
            version.rs                        # UvVersion, MIN_UV_VERSION, BUNDLED_UV_VERSION
            resolve.rs                        # UvSource -> ResolvedUv
            command.rs                        # pure argv builders
            results.rs                        # JSON parsing of uv output
    tests/fixtures/
        uv-tree.json                          # captured from uv 0.12.1
        uv-pip-list.json
        uv-python-list.json
```

`process/` owns "how do we run things", `uv/` owns "what do we run and what comes back". Each file has one responsibility; `command.rs` and `results.rs` are pure and hold the bulk of the tests.

---

### Task 1: The CommandRunner seam

Everything else in this plan sits on top of this. Get the trait right and the rest is testable without a single real process.

**Files:**
- Create: `crates/bombadil-core/src/process/mod.rs`
- Create: `crates/bombadil-core/src/process/system.rs`
- Create: `crates/bombadil-core/src/process/fake.rs`
- Modify: `crates/bombadil-core/src/lib.rs`

**Interfaces:**
- Consumes: nothing from earlier plans.
- Produces:
  - `bombadil_core::process::CommandSpec { program: PathBuf, args: Vec<String>, cwd: Option<PathBuf>, env: BTreeMap<String, String>, clear_env: bool }`
  - `bombadil_core::process::CommandOutcome { status: Option<i32>, stdout: String, stderr: String }` with `fn success(&self) -> bool`
  - `bombadil_core::process::CommandError` (enum: `Spawn { program: PathBuf, source: std::io::Error }`)
  - `bombadil_core::process::CommandRunner` — trait with `fn run(&self, spec: &CommandSpec) -> Result<CommandOutcome, CommandError>`
  - `bombadil_core::process::SystemCommandRunner` (unit struct implementing it)
  - `bombadil_core::process::FakeCommandRunner` with `fn new() -> Self`, `fn expect(&self, program_suffix: &str, args_contains: &[&str], outcome: CommandOutcome)`, `fn calls(&self) -> Vec<CommandSpec>`

> `status` is `Option<i32>` because a process killed by a signal has no exit code on Unix. Treating that as `Some(0)` would report a crash as success.
>
> `FakeCommandRunner` is compiled for tests **and** for downstream crates' tests, so it is gated on `#[cfg(any(test, feature = "test-support"))]` with a `test-support` feature declared in `Cargo.toml`.

- [ ] **Step 1: Declare the feature and module**

In `crates/bombadil-core/Cargo.toml`, add:

```toml
[features]
default = []
test-support = []
```

In `crates/bombadil-core/src/lib.rs`, add `pub mod process;` in alphabetical position (after `pub mod pyproject;` is wrong — it sorts before, so place it after `model`).

- [ ] **Step 2: Write the failing tests**

Create `crates/bombadil-core/src/process/mod.rs` with only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_exit_is_success() {
        let outcome = CommandOutcome {
            status: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(outcome.success());
    }

    #[test]
    fn a_nonzero_exit_is_not_success() {
        let outcome = CommandOutcome {
            status: Some(1),
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(!outcome.success());
    }

    #[test]
    fn a_signal_killed_process_is_not_success() {
        // No exit code at all: on Unix this is a process killed by a signal.
        // Reporting it as success would turn a crash into a silent no-op.
        let outcome = CommandOutcome {
            status: None,
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(!outcome.success());
    }

    #[test]
    fn the_fake_returns_the_outcome_matching_program_and_args() {
        let fake = FakeCommandRunner::new();
        fake.expect(
            "uv",
            &["sync"],
            CommandOutcome {
                status: Some(0),
                stdout: "synced".into(),
                stderr: String::new(),
            },
        );

        let spec = CommandSpec::new("/opt/bin/uv").arg("sync").arg("--frozen");
        let got = fake.run(&spec).unwrap();

        assert_eq!(got.stdout, "synced");
    }

    #[test]
    fn the_fake_records_every_call_for_assertion() {
        let fake = FakeCommandRunner::new();
        fake.expect("uv", &[], CommandOutcome { status: Some(0), stdout: String::new(), stderr: String::new() });

        let spec = CommandSpec::new("/opt/bin/uv")
            .arg("add")
            .arg("requests")
            .env("UV_PROJECT_ENVIRONMENT", "/envs/x");
        fake.run(&spec).unwrap();

        let calls = fake.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args, vec!["add", "requests"]);
        assert_eq!(
            calls[0].env.get("UV_PROJECT_ENVIRONMENT").map(String::as_str),
            Some("/envs/x")
        );
    }

    #[test]
    fn an_unexpected_call_is_an_explicit_failure_not_a_default() {
        // A fake that silently returns success for an unprogrammed call turns
        // "we never ran this" into a passing test.
        let fake = FakeCommandRunner::new();
        let spec = CommandSpec::new("/opt/bin/uv").arg("sync");
        let err = fake.run(&spec).unwrap_err();
        assert!(format!("{err}").contains("uv"), "got {err}");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p bombadil-core process`
Expected: FAIL — `cannot find type CommandOutcome`, `cannot find type CommandSpec`, `cannot find type FakeCommandRunner`.

- [ ] **Step 4: Write the implementation**

Above the test module in `crates/bombadil-core/src/process/mod.rs`:

```rust
mod fake;
mod system;

#[cfg(any(test, feature = "test-support"))]
pub use fake::FakeCommandRunner;
pub use system::SystemCommandRunner;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Everything needed to launch one process.
///
/// `BTreeMap` rather than `HashMap` so the environment is ordered: tests can
/// assert on it, and failure reports render the same way twice.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    /// When true the child starts from an empty environment and sees only
    /// `env`. Plan 2b composes a full environment and sets this.
    pub clear_env: bool,
}

impl CommandSpec {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }
}

/// What a finished process left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    /// `None` when the process was killed by a signal and has no exit code.
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutcome {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("could not run {program}: {source}")]
    Spawn {
        program: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("no fake outcome was programmed for {program} {args:?}")]
    Unexpected { program: PathBuf, args: Vec<String> },
}

/// The single seam through which Bombadil launches processes.
///
/// Nothing above this trait spawns anything directly, which is what lets the
/// uv layer, the environment pipeline and the GUI be tested without processes.
pub trait CommandRunner: Send + Sync {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutcome, CommandError>;
}

/// Shared helper so both implementations agree on what a program "is".
fn program_matches(program: &Path, suffix: &str) -> bool {
    program
        .file_stem()
        .map(|stem| stem.eq_ignore_ascii_case(suffix))
        .unwrap_or(false)
}
```

Create `crates/bombadil-core/src/process/system.rs`:

```rust
use super::{CommandError, CommandOutcome, CommandRunner, CommandSpec};

/// Runs commands for real, via `std::process`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutcome, CommandError> {
        let mut command = std::process::Command::new(&spec.program);
        command.args(&spec.args);

        if let Some(dir) = &spec.cwd {
            command.current_dir(dir);
        }
        if spec.clear_env {
            command.env_clear();
        }
        for (key, value) in &spec.env {
            command.env(key, value);
        }

        let output = command.output().map_err(|source| CommandError::Spawn {
            program: spec.program.clone(),
            source,
        })?;

        Ok(CommandOutcome {
            status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}
```

Create `crates/bombadil-core/src/process/fake.rs`:

```rust
#![cfg(any(test, feature = "test-support"))]

use super::{CommandError, CommandOutcome, CommandRunner, CommandSpec, program_matches};
use std::sync::Mutex;

struct Rule {
    program_suffix: String,
    args_contains: Vec<String>,
    outcome: CommandOutcome,
}

/// A `CommandRunner` that returns programmed outcomes and records every call.
///
/// An unprogrammed call is an error rather than a default success: a fake that
/// quietly succeeds turns "this code never ran the command" into a green test.
#[derive(Default)]
pub struct FakeCommandRunner {
    rules: Mutex<Vec<Rule>>,
    calls: Mutex<Vec<CommandSpec>>,
}

impl FakeCommandRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Program an outcome for any call whose program file stem matches
    /// `program_suffix` and whose args contain every entry of `args_contains`.
    pub fn expect(&self, program_suffix: &str, args_contains: &[&str], outcome: CommandOutcome) {
        self.rules.lock().unwrap().push(Rule {
            program_suffix: program_suffix.to_string(),
            args_contains: args_contains.iter().map(|s| s.to_string()).collect(),
            outcome,
        });
    }

    /// Every spec passed to `run`, in order.
    pub fn calls(&self) -> Vec<CommandSpec> {
        self.calls.lock().unwrap().clone()
    }
}

impl CommandRunner for FakeCommandRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutcome, CommandError> {
        self.calls.lock().unwrap().push(spec.clone());

        let rules = self.rules.lock().unwrap();
        rules
            .iter()
            .find(|rule| {
                program_matches(&spec.program, &rule.program_suffix)
                    && rule.args_contains.iter().all(|want| spec.args.contains(want))
            })
            .map(|rule| rule.outcome.clone())
            .ok_or_else(|| CommandError::Unexpected {
                program: spec.program.clone(),
                args: spec.args.clone(),
            })
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p bombadil-core --all-features`
Expected: PASS — 6 new tests on top of the existing 47.

- [ ] **Step 6: Verify the feature gate both ways**

Run: `cargo test -p bombadil-core` then `cargo clippy --all-targets --all-features --locked -- -D warnings`
Expected: both clean. The default-feature build must not fail on `FakeCommandRunner` being absent.

- [ ] **Step 7: Commit**

```bash
git add crates/bombadil-core/src/process crates/bombadil-core/src/lib.rs crates/bombadil-core/Cargo.toml
git commit -m "feat(core): add the CommandRunner process seam

Every subprocess in the application goes through this trait, which is what
makes the uv layer and the environment pipeline testable without spawning
anything. The fake errors on unprogrammed calls rather than defaulting to
success, so a test cannot pass by never running the command it claims to."
```

---

### Task 2: uv version parsing and comparison

**Files:**
- Create: `crates/bombadil-core/src/uv/mod.rs`
- Create: `crates/bombadil-core/src/uv/version.rs`
- Modify: `crates/bombadil-core/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `bombadil_core::uv::UvVersion { major: u32, minor: u32, patch: u32 }`, deriving `Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord`, with `impl std::fmt::Display`
  - `UvVersion::parse(text: &str) -> Option<UvVersion>` — accepts both a bare `0.12.1` and the full `uv --version` line `uv 0.12.1 (x86_64-unknown-linux-gnu)`
  - `bombadil_core::uv::MIN_UV_VERSION: UvVersion` and `bombadil_core::uv::BUNDLED_UV_VERSION: UvVersion`, both `0.12.1`

> Derived `Ord` on a struct compares fields in declaration order, which is exactly semver precedence for major/minor/patch. That is why the fields are declared in that order — do not reorder them.

- [ ] **Step 1: Write the failing tests**

Create `crates/bombadil-core/src/uv/version.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_bare_version() {
        assert_eq!(
            UvVersion::parse("0.12.1"),
            Some(UvVersion { major: 0, minor: 12, patch: 1 })
        );
    }

    #[test]
    fn parses_the_full_uv_version_line() {
        // Exactly what `uv --version` prints.
        assert_eq!(
            UvVersion::parse("uv 0.12.1 (x86_64-unknown-linux-gnu)"),
            Some(UvVersion { major: 0, minor: 12, patch: 1 })
        );
    }

    #[test]
    fn tolerates_surrounding_whitespace_and_newlines() {
        assert_eq!(
            UvVersion::parse("  uv 0.12.1 (abc)\n"),
            Some(UvVersion { major: 0, minor: 12, patch: 1 })
        );
    }

    #[test]
    fn rejects_text_with_no_version() {
        assert_eq!(UvVersion::parse("uv"), None);
        assert_eq!(UvVersion::parse(""), None);
        assert_eq!(UvVersion::parse("not a version"), None);
    }

    #[test]
    fn rejects_a_two_component_version() {
        // uv always prints three components; two means we are parsing
        // something else and should not guess a patch level.
        assert_eq!(UvVersion::parse("0.12"), None);
    }

    #[test]
    fn orders_by_major_then_minor_then_patch() {
        let v = |m, n, p| UvVersion { major: m, minor: n, patch: p };
        assert!(v(0, 12, 1) > v(0, 12, 0));
        assert!(v(0, 12, 0) > v(0, 9, 99));
        assert!(v(1, 0, 0) > v(0, 99, 99));
        assert_eq!(v(0, 12, 1), v(0, 12, 1));
    }

    #[test]
    fn the_bundled_version_satisfies_the_minimum() {
        // If this ever fails, the bundled uv would be rejected by our own
        // version gate and the app would fall back to nothing.
        assert!(BUNDLED_UV_VERSION >= MIN_UV_VERSION);
    }

    #[test]
    fn displays_in_the_form_uv_prints() {
        assert_eq!(UvVersion { major: 0, minor: 12, patch: 1 }.to_string(), "0.12.1");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p bombadil-core uv::version`
Expected: FAIL — `cannot find type UvVersion`.

- [ ] **Step 3: Write the implementation**

Above the test module in `crates/bombadil-core/src/uv/version.rs`:

```rust
/// A uv release version.
///
/// Field order is load-bearing for the derived `Ord`: comparison walks fields
/// in declaration order, which gives major-then-minor-then-patch precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UvVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

/// The uv shipped inside our binary.
pub const BUNDLED_UV_VERSION: UvVersion = UvVersion { major: 0, minor: 12, patch: 1 };

/// The oldest uv whose CLI surface has been verified against this code.
///
/// Lower it only after explicitly re-testing an older version; raise it
/// whenever Bombadil adopts a flag a newer uv introduced. Backwards
/// compatibility is never assumed.
pub const MIN_UV_VERSION: UvVersion = UvVersion { major: 0, minor: 12, patch: 1 };

impl UvVersion {
    /// Parse a bare `0.12.1` or a full `uv 0.12.1 (target-triple)` line.
    pub fn parse(text: &str) -> Option<Self> {
        // Take the first whitespace-separated token that looks like a version,
        // so this works whether or not the `uv ` prefix is present.
        text.split_whitespace().find_map(Self::parse_token)
    }

    fn parse_token(token: &str) -> Option<Self> {
        let mut parts = token.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        // Trim anything trailing the patch number, such as a `+local` suffix.
        let patch_text = parts.next()?;
        let digits: String = patch_text.chars().take_while(char::is_ascii_digit).collect();
        let patch = digits.parse().ok()?;
        // A fourth component means this is not a uv version.
        if parts.next().is_some() {
            return None;
        }
        Some(Self { major, minor, patch })
    }
}

impl std::fmt::Display for UvVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}
```

> `char::is_ascii_digit` takes `&char`, and `take_while` yields `&char` for a `Chars` iterator adapted this way — if the compiler objects, use `.take_while(|c| c.is_ascii_digit())`.

Create `crates/bombadil-core/src/uv/mod.rs`:

```rust
mod version;

pub use version::{BUNDLED_UV_VERSION, MIN_UV_VERSION, UvVersion};
```

Add `pub mod uv;` to `crates/bombadil-core/src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p bombadil-core --all-features`
Expected: PASS — 8 new tests.

- [ ] **Step 5: Commit**

```bash
git add crates/bombadil-core/src/uv crates/bombadil-core/src/lib.rs
git commit -m "feat(core): parse and compare uv versions

Parses both the bare version and the full 'uv 0.12.1 (triple)' line that
uv --version prints. MIN_UV_VERSION is 0.12.1, the version whose CLI
surface was verified against this code; a test pins that the bundled
version satisfies our own gate, since otherwise the fallback would have
nothing to fall back to."
```

---

### Task 3: The embedded uv binary crate

**Files:**
- Create: `crates/bombadil-uv-bin/Cargo.toml`
- Create: `crates/bombadil-uv-bin/uv-manifest.toml`
- Create: `crates/bombadil-uv-bin/build.rs`
- Create: `crates/bombadil-uv-bin/src/lib.rs`
- Modify: `Cargo.toml` (workspace members)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `bombadil_uv_bin::BUNDLED_VERSION: &str` (`"0.12.1"`)
  - `bombadil_uv_bin::ensure_extracted(data_dir: &Path) -> Result<PathBuf, ExtractError>` — writes the embedded binary to `data_dir/uv/<version>-<sha256 prefix>/uv[.exe]` if absent or if its hash no longer matches, marks it executable on Unix, and returns its path
  - `bombadil_uv_bin::ExtractError`

> **This crate exists solely so `bombadil-core`'s tests do not link a ~40 MB blob.** Keep it that small.
>
> `build.rs` needs network access. `BOMBADIL_UV_ARCHIVE` pointing at a pre-downloaded archive skips the download, which is how offline and air-gapped builds work — and how CI avoids hammering GitHub.

- [ ] **Step 1: Create the manifest and the crate manifest**

`crates/bombadil-uv-bin/uv-manifest.toml` — committed, and the source of truth for what gets embedded:

```toml
# Pinned uv release. Checksums come from the `.sha256` files GitHub publishes
# alongside each asset; verify with:
#   curl -sSL https://github.com/astral-sh/uv/releases/download/0.12.1/<asset>.sha256
version = "0.12.1"

[targets.x86_64-unknown-linux-gnu]
asset = "uv-x86_64-unknown-linux-gnu.tar.gz"
sha256 = "90b2f223fb69d19db49e117da601f64978593417988530aa733d456141b4bcbb"
```

> Only the host triple used for development is pinned here. **Task 3 Step 6 adds the remaining targets** by fetching their published checksums — do not invent hashes.

`crates/bombadil-uv-bin/Cargo.toml`:

```toml
[package]
name = "bombadil-uv-bin"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
sha2.workspace = true
thiserror.workspace = true

[build-dependencies]
flate2 = "1.1.9"
sha2.workspace = true
tar = "0.4.46"
toml.workspace = true
ureq = "3.3.0"
zip = "8.6.0"
```

Add `"crates/bombadil-uv-bin"` to the workspace `members` array in the root `Cargo.toml`.

- [ ] **Step 2: Write build.rs**

`crates/bombadil-uv-bin/build.rs`:

```rust
//! Acquires the pinned uv binary for the *target* triple and places it in
//! OUT_DIR for `include_bytes!`.
//!
//! Network access happens here, at build time, never at runtime. Set
//! BOMBADIL_UV_ARCHIVE to a pre-downloaded archive to build offline.

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=uv-manifest.toml");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=BOMBADIL_UV_ARCHIVE");

    let target = std::env::var("TARGET").expect("TARGET is set by cargo");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));

    let manifest: toml::Value = toml::from_str(
        &std::fs::read_to_string("uv-manifest.toml").expect("uv-manifest.toml is readable"),
    )
    .expect("uv-manifest.toml parses");

    let version = manifest["version"].as_str().expect("version is a string");
    let entry = manifest
        .get("targets")
        .and_then(|t| t.get(&target))
        .unwrap_or_else(|| {
            panic!(
                "uv-manifest.toml has no entry for target {target}. \
                 Add its asset name and sha256 from the uv 0.12.1 release."
            )
        });
    let asset = entry["asset"].as_str().expect("asset is a string");
    let expected_sha = entry["sha256"].as_str().expect("sha256 is a string");

    let archive = match std::env::var("BOMBADIL_UV_ARCHIVE") {
        Ok(path) => std::fs::read(&path)
            .unwrap_or_else(|e| panic!("BOMBADIL_UV_ARCHIVE={path} could not be read: {e}")),
        Err(_) => download(&format!(
            "https://github.com/astral-sh/uv/releases/download/{version}/{asset}"
        )),
    };

    let actual_sha = hex(&Sha256::digest(&archive));
    assert_eq!(
        actual_sha, expected_sha,
        "checksum mismatch for {asset}: refusing to embed an unverified binary"
    );

    let exe_name = if target.contains("windows") { "uv.exe" } else { "uv" };
    let binary = if asset.ends_with(".zip") {
        extract_from_zip(&archive, exe_name)
    } else {
        extract_from_tar_gz(&archive, exe_name)
    };

    std::fs::write(out_dir.join(exe_name), &binary).expect("write uv to OUT_DIR");
    println!("cargo:rustc-env=BOMBADIL_UV_EXE_NAME={exe_name}");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn download(url: &str) -> Vec<u8> {
    let mut body = Vec::new();
    ureq::get(url)
        .call()
        .unwrap_or_else(|e| panic!("could not download {url}: {e}"))
        .into_body()
        .into_reader()
        .read_to_end(&mut body)
        .unwrap_or_else(|e| panic!("could not read body of {url}: {e}"));
    body
}

/// Match on file name, not on a hardcoded path: the archives wrap the binary
/// in a top-level directory whose name has changed between uv releases.
fn matches_name(path: &Path, exe_name: &str) -> bool {
    path.file_name().map(|n| n == exe_name).unwrap_or(false)
}

fn extract_from_tar_gz(archive: &[u8], exe_name: &str) -> Vec<u8> {
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries().expect("tar entries") {
        let mut entry = entry.expect("tar entry");
        let path = entry.path().expect("entry path").into_owned();
        if matches_name(&path, exe_name) {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).expect("read uv from tar");
            return bytes;
        }
    }
    panic!("no {exe_name} member found in the uv archive");
}

fn extract_from_zip(archive: &[u8], exe_name: &str) -> Vec<u8> {
    let reader = std::io::Cursor::new(archive);
    let mut zip = zip::ZipArchive::new(reader).expect("open uv zip");
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).expect("zip entry");
        let name = entry.name().to_string();
        if matches_name(Path::new(&name), exe_name) {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).expect("read uv from zip");
            return bytes;
        }
    }
    panic!("no {exe_name} member found in the uv archive");
}
```

- [ ] **Step 3: Write the failing tests and the library**

`crates/bombadil-uv-bin/src/lib.rs`:

```rust
//! The uv binary, embedded.
//!
//! Separate from `bombadil-core` so that crate's tests neither link tens of
//! megabytes nor need the network.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The uv release embedded in this build.
pub const BUNDLED_VERSION: &str = "0.12.1";

const EXE_NAME: &str = env!("BOMBADIL_UV_EXE_NAME");
const UV_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/", env!("BOMBADIL_UV_EXE_NAME")));

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("could not write the bundled uv to {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where the extracted binary lives, keyed by version and content hash so a
/// new build never reuses an old extraction.
pub fn extraction_path(data_dir: &Path) -> PathBuf {
    let digest = hex(&Sha256::digest(UV_BYTES));
    data_dir
        .join("uv")
        .join(format!("{BUNDLED_VERSION}-{}", &digest[..12]))
        .join(EXE_NAME)
}

/// Write the embedded uv to disk if it is not already there with the right
/// contents, and return its path.
///
/// Re-extracts when the file is missing or its hash does not match, so a
/// truncated or tampered extraction repairs itself.
pub fn ensure_extracted(data_dir: &Path) -> Result<PathBuf, ExtractError> {
    let path = extraction_path(data_dir);

    if let Ok(existing) = std::fs::read(&path)
        && Sha256::digest(&existing) == Sha256::digest(UV_BYTES)
    {
        return Ok(path);
    }

    let parent = path.parent().expect("extraction path always has a parent");
    std::fs::create_dir_all(parent).map_err(|source| ExtractError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    std::fs::write(&path, UV_BYTES).map_err(|source| ExtractError::Io {
        path: path.clone(),
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).map_err(
            |source| ExtractError::Io {
                path: path.clone(),
                source,
            },
        )?;
    }

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_binary_is_not_empty() {
        assert!(UV_BYTES.len() > 1_000_000, "got {} bytes", UV_BYTES.len());
    }

    #[test]
    fn extraction_writes_a_runnable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = ensure_extracted(dir.path()).unwrap();

        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap().len(), UV_BYTES.len());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "extracted uv must be executable");
        }
    }

    #[test]
    fn a_second_extraction_reuses_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let first = ensure_extracted(dir.path()).unwrap();
        let second = ensure_extracted(dir.path()).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn a_corrupted_extraction_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let path = ensure_extracted(dir.path()).unwrap();
        std::fs::write(&path, b"truncated").unwrap();

        ensure_extracted(dir.path()).unwrap();

        assert_eq!(std::fs::read(&path).unwrap().len(), UV_BYTES.len());
    }
}
```

Add `tempfile.workspace = true` under a `[dev-dependencies]` section in `crates/bombadil-uv-bin/Cargo.toml`.

> `if let Ok(x) = ... && cond` is a let-chain, stable in edition 2024. If the compiler rejects it, use a nested `if`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p bombadil-uv-bin`
Expected: PASS — 4 tests. The first build downloads ~20 MB; later builds reuse OUT_DIR.

- [ ] **Step 5: Verify the offline path**

```bash
curl -sSL https://github.com/astral-sh/uv/releases/download/0.12.1/uv-x86_64-unknown-linux-gnu.tar.gz -o /tmp/uv.tar.gz
BOMBADIL_UV_ARCHIVE=/tmp/uv.tar.gz cargo build -p bombadil-uv-bin
```
Expected: builds without network access to GitHub. If it downloads anyway, `rerun-if-env-changed` is missing or the env var name is wrong.

- [ ] **Step 6: Add the remaining target checksums**

For each of `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`, `aarch64-unknown-linux-gnu`, fetch the published checksum and add a `[targets.<triple>]` block to `uv-manifest.toml`:

```bash
for t in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu; do
  echo "$t: $(curl -sSL https://github.com/astral-sh/uv/releases/download/0.12.1/uv-$t.tar.gz.sha256 | cut -d' ' -f1)"
done
echo "x86_64-pc-windows-msvc: $(curl -sSL https://github.com/astral-sh/uv/releases/download/0.12.1/uv-x86_64-pc-windows-msvc.zip.sha256 | cut -d' ' -f1)"
```

Use the printed values verbatim. Do not hand-write a hash.

- [ ] **Step 7: Commit**

```bash
git add crates/bombadil-uv-bin Cargo.toml
git commit -m "feat(uv-bin): embed a checksum-verified uv binary

build.rs downloads the pinned release for the *target* triple, verifies
its published sha256 before embedding, and extracts the member by file
name rather than a hardcoded path, since the wrapping directory name has
changed between uv releases. BOMBADIL_UV_ARCHIVE builds offline.

Kept in its own crate so bombadil-core's tests link none of it."
```

---

### Task 4: Resolving which uv to run

**Files:**
- Create: `crates/bombadil-core/src/uv/resolve.rs`
- Modify: `crates/bombadil-core/src/uv/mod.rs`
- Modify: `crates/bombadil-core/src/error.rs`

**Interfaces:**
- Consumes: `UvSource` (plan 1, `model::settings`); `CommandRunner`, `CommandSpec`, `CommandOutcome` (Task 1); `UvVersion`, `MIN_UV_VERSION` (Task 2).
- Produces:
  - `bombadil_core::uv::UvOrigin` — enum `Bundled`, `Path`, `Configured`
  - `bombadil_core::uv::ResolvedUv { path: PathBuf, version: UvVersion, origin: UvOrigin, newer_than_bundled: bool }`
  - `bombadil_core::uv::resolve(source: &UvSource, runner: &dyn CommandRunner, bundled: &dyn Fn() -> Result<PathBuf, String>) -> Result<ResolvedUv, UvResolveError>`
  - `bombadil_core::error::UvResolveError`

> `bundled` is a closure rather than a direct call into `bombadil-uv-bin` so `bombadil-core` does not depend on that crate — and so tests can supply a path without extracting 40 MB. The GUI passes a closure that calls `bombadil_uv_bin::ensure_extracted`.
>
> Resolution semantics, from the spec: `Auto` prefers `PATH` when it satisfies `MIN_UV_VERSION`, else the bundled copy. `Bundled` is always the embedded copy. `FromPath` and `Custom` **fail loudly** — they never silently fall back, because a user who pinned a uv needs to know their pin is broken.

- [ ] **Step 1: Write the failing tests**

Create `crates/bombadil-core/src/uv/resolve.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{CommandOutcome, FakeCommandRunner};
    use std::path::PathBuf;

    fn version_outcome(line: &str) -> CommandOutcome {
        CommandOutcome { status: Some(0), stdout: line.into(), stderr: String::new() }
    }

    fn bundled_ok() -> Box<dyn Fn() -> Result<PathBuf, String>> {
        Box::new(|| Ok(PathBuf::from("/data/uv/0.12.1-abc/uv")))
    }

    #[test]
    fn auto_prefers_a_path_uv_that_meets_the_minimum() {
        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.12.1 (x86_64-unknown-linux-gnu)"));

        let got = resolve(&UvSource::Auto, &runner, &*bundled_ok()).unwrap();

        assert_eq!(got.origin, UvOrigin::Path);
        assert_eq!(got.version, UvVersion { major: 0, minor: 12, patch: 1 });
        assert!(!got.newer_than_bundled);
    }

    #[test]
    fn auto_falls_back_to_bundled_when_path_uv_is_too_old() {
        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.9.0 (x)"));

        let got = resolve(&UvSource::Auto, &runner, &*bundled_ok()).unwrap();

        assert_eq!(got.origin, UvOrigin::Bundled);
        assert_eq!(got.path, PathBuf::from("/data/uv/0.12.1-abc/uv"));
    }

    #[test]
    fn auto_falls_back_to_bundled_when_no_uv_is_on_path() {
        // The fake errors on unprogrammed calls, which is exactly what a
        // missing binary looks like.
        let runner = FakeCommandRunner::new();

        let got = resolve(&UvSource::Auto, &runner, &*bundled_ok()).unwrap();

        assert_eq!(got.origin, UvOrigin::Bundled);
    }

    #[test]
    fn auto_flags_a_path_uv_newer_than_the_bundled_one() {
        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.99.0 (x)"));

        let got = resolve(&UvSource::Auto, &runner, &*bundled_ok()).unwrap();

        assert_eq!(got.origin, UvOrigin::Path);
        assert!(
            got.newer_than_bundled,
            "a uv newer than the one we test against must be visibly flagged"
        );
    }

    #[test]
    fn bundled_never_consults_path() {
        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.99.0 (x)"));

        let got = resolve(&UvSource::Bundled, &runner, &*bundled_ok()).unwrap();

        assert_eq!(got.origin, UvOrigin::Bundled);
        assert_eq!(got.version, BUNDLED_UV_VERSION);
    }

    #[test]
    fn from_path_fails_loudly_when_uv_is_missing() {
        let runner = FakeCommandRunner::new();

        let err = resolve(&UvSource::FromPath, &runner, &*bundled_ok()).unwrap_err();

        assert!(
            matches!(err, UvResolveError::PathUvUnavailable { .. }),
            "an explicit pin must never silently fall back to bundled; got {err:?}"
        );
    }

    #[test]
    fn from_path_fails_loudly_when_uv_is_too_old() {
        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.9.0 (x)"));

        let err = resolve(&UvSource::FromPath, &runner, &*bundled_ok()).unwrap_err();

        assert!(matches!(err, UvResolveError::TooOld { .. }), "got {err:?}");
    }

    #[test]
    fn a_configured_binary_is_used_and_reports_its_own_version() {
        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.13.0 (x)"));

        let source = UvSource::Custom { path: PathBuf::from("/opt/uv/bin/uv") };
        let got = resolve(&source, &runner, &*bundled_ok()).unwrap();

        assert_eq!(got.origin, UvOrigin::Configured);
        assert_eq!(got.path, PathBuf::from("/opt/uv/bin/uv"));
        assert_eq!(got.version, UvVersion { major: 0, minor: 13, patch: 0 });
    }

    #[test]
    fn a_configured_binary_that_is_missing_fails_loudly() {
        let runner = FakeCommandRunner::new();
        let source = UvSource::Custom { path: PathBuf::from("/opt/uv/bin/uv") };

        let err = resolve(&source, &runner, &*bundled_ok()).unwrap_err();

        assert!(matches!(err, UvResolveError::ConfiguredUnavailable { .. }), "got {err:?}");
    }

    #[test]
    fn a_failed_bundled_extraction_is_reported() {
        let runner = FakeCommandRunner::new();
        let failing: Box<dyn Fn() -> Result<PathBuf, String>> =
            Box::new(|| Err("disk full".to_string()));

        let err = resolve(&UvSource::Bundled, &runner, &*failing).unwrap_err();

        assert!(matches!(err, UvResolveError::BundledUnavailable { .. }), "got {err:?}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p bombadil-core --all-features uv::resolve`
Expected: FAIL — `cannot find function resolve`, `cannot find type ResolvedUv`.

- [ ] **Step 3: Write the implementation**

Above the test module in `crates/bombadil-core/src/uv/resolve.rs`:

```rust
use super::{BUNDLED_UV_VERSION, MIN_UV_VERSION, UvVersion};
use crate::error::UvResolveError;
use crate::model::UvSource;
use crate::process::{CommandRunner, CommandSpec};
use std::path::{Path, PathBuf};

/// Where the uv we are about to run came from. Surfaced in the UI and in
/// failure reports, so version skew is visible rather than mysterious.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UvOrigin {
    Bundled,
    Path,
    Configured,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedUv {
    pub path: PathBuf,
    pub version: UvVersion,
    pub origin: UvOrigin,
    /// True when this uv is newer than the one we ship and therefore newer
    /// than anything this code has been tested against.
    pub newer_than_bundled: bool,
}

/// Ask a uv binary for its version. `None` when it cannot be run or its output
/// cannot be parsed — both mean "unusable", and the caller decides whether
/// that is fatal.
fn probe(path: &Path, runner: &dyn CommandRunner) -> Option<UvVersion> {
    let spec = CommandSpec::new(path).arg("--version");
    let outcome = runner.run(&spec).ok()?;
    if !outcome.success() {
        return None;
    }
    UvVersion::parse(&outcome.stdout)
}

fn resolved(path: PathBuf, version: UvVersion, origin: UvOrigin) -> ResolvedUv {
    ResolvedUv {
        path,
        version,
        origin,
        newer_than_bundled: version > BUNDLED_UV_VERSION,
    }
}

fn use_bundled(
    bundled: &dyn Fn() -> Result<PathBuf, String>,
) -> Result<ResolvedUv, UvResolveError> {
    let path = bundled().map_err(|message| UvResolveError::BundledUnavailable { message })?;
    Ok(resolved(path, BUNDLED_UV_VERSION, UvOrigin::Bundled))
}

/// Decide which uv binary to run.
///
/// `Auto` prefers a new-enough uv from PATH and otherwise falls back to the
/// bundled copy. `FromPath` and `Custom` are explicit pins: they fail loudly
/// rather than falling back, because silently ignoring a user's pin hides the
/// very problem they need to see.
pub fn resolve(
    source: &UvSource,
    runner: &dyn CommandRunner,
    bundled: &dyn Fn() -> Result<PathBuf, String>,
) -> Result<ResolvedUv, UvResolveError> {
    match source {
        UvSource::Bundled => use_bundled(bundled),

        UvSource::Auto => {
            let on_path = PathBuf::from("uv");
            match probe(&on_path, runner) {
                Some(version) if version >= MIN_UV_VERSION => {
                    Ok(resolved(on_path, version, UvOrigin::Path))
                }
                _ => use_bundled(bundled),
            }
        }

        UvSource::FromPath => {
            let on_path = PathBuf::from("uv");
            match probe(&on_path, runner) {
                Some(version) if version >= MIN_UV_VERSION => {
                    Ok(resolved(on_path, version, UvOrigin::Path))
                }
                Some(version) => Err(UvResolveError::TooOld {
                    path: on_path,
                    found: version.to_string(),
                    minimum: MIN_UV_VERSION.to_string(),
                }),
                None => Err(UvResolveError::PathUvUnavailable),
            }
        }

        UvSource::Custom { path } => match probe(path, runner) {
            Some(version) if version >= MIN_UV_VERSION => {
                Ok(resolved(path.clone(), version, UvOrigin::Configured))
            }
            Some(version) => Err(UvResolveError::TooOld {
                path: path.clone(),
                found: version.to_string(),
                minimum: MIN_UV_VERSION.to_string(),
            }),
            None => Err(UvResolveError::ConfiguredUnavailable { path: path.clone() }),
        },
    }
}
```

Add to `crates/bombadil-core/src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum UvResolveError {
    #[error("the bundled uv could not be made available: {message}")]
    BundledUnavailable { message: String },

    #[error("settings require uv from PATH, but no runnable uv was found there")]
    PathUvUnavailable,

    #[error("settings point at {path}, which could not be run")]
    ConfiguredUnavailable { path: PathBuf },

    #[error("{path} is uv {found}, older than the minimum supported {minimum}")]
    TooOld {
        path: PathBuf,
        found: String,
        minimum: String,
    },
}
```

Add `mod resolve;` and `pub use resolve::{ResolvedUv, UvOrigin, resolve};` to `crates/bombadil-core/src/uv/mod.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p bombadil-core --all-features`
Expected: PASS — 10 new tests.

> The `let bundled = |_: ()| {}; let _ = bundled;` lines in the last test are dead scaffolding; delete them if clippy objects.

- [ ] **Step 5: Commit**

```bash
git add crates/bombadil-core/src/uv crates/bombadil-core/src/error.rs
git commit -m "feat(core): resolve which uv binary to run

Auto prefers a new-enough uv from PATH and falls back to the bundled copy.
FromPath and Custom are explicit pins and fail loudly instead of falling
back, because silently ignoring a user's pin hides exactly the problem
they need to see. A uv newer than the bundled one is flagged, since it is
newer than anything this code has been tested against.

The bundled path arrives as a closure so bombadil-core does not depend on
bombadil-uv-bin and its tests link none of the embedded binary."
```

---

### Task 5: Building uv command lines

Pure functions. No process is run here, which is what makes every uv invocation in the app assertable in a table test.

**Files:**
- Create: `crates/bombadil-core/src/uv/command.rs`
- Modify: `crates/bombadil-core/src/uv/mod.rs`

**Interfaces:**
- Consumes: `CommandSpec` (Task 1); `ResolvedUv` (Task 4).
- Produces, all returning `CommandSpec`:
  - `uv::command::sync(uv: &ResolvedUv, ctx: &ProjectContext) -> CommandSpec`
  - `uv::command::sync_with_python(uv, ctx, python: &str) -> CommandSpec`
  - `uv::command::add(uv, ctx, requirement: &str, group: DependencyGroup) -> CommandSpec`
  - `uv::command::remove(uv, ctx, package: &str, group: DependencyGroup) -> CommandSpec`
  - `uv::command::upgrade_package(uv, ctx, package: &str) -> CommandSpec`
  - `uv::command::tree(uv, ctx, depth: u32) -> CommandSpec`
  - `uv::command::pip_list(uv, ctx) -> CommandSpec`
  - `uv::command::python_list(uv) -> CommandSpec`
  - `uv::command::python_install(uv, version: &str) -> CommandSpec`
  - `uv::command::ProjectContext { project_dir: PathBuf, venv_path: PathBuf }`
  - `uv::command::DependencyGroup` — enum `Main`, `Dev`, `Named(String)`, `Optional(String)`

> Every command carries `UV_PROJECT_ENVIRONMENT` set to the resolved venv path and runs with `cwd` set to the project directory. That pairing is the whole mechanism by which a project's venv can live anywhere — verified working on uv 0.12.1.

- [ ] **Step 1: Write the failing tests**

Create `crates/bombadil-core/src/uv/command.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::uv::{UvOrigin, UvVersion};
    use std::path::PathBuf;

    fn uv() -> ResolvedUv {
        ResolvedUv {
            path: PathBuf::from("/data/uv/uv"),
            version: UvVersion { major: 0, minor: 12, patch: 1 },
            origin: UvOrigin::Bundled,
            newer_than_bundled: false,
        }
    }

    fn ctx() -> ProjectContext {
        ProjectContext {
            project_dir: PathBuf::from("/proj/my-api"),
            venv_path: PathBuf::from("/envs/my-api-3f9c1a"),
        }
    }

    #[test]
    fn every_command_runs_in_the_project_directory() {
        let spec = sync(&uv(), &ctx());
        assert_eq!(spec.cwd, Some(PathBuf::from("/proj/my-api")));
    }

    #[test]
    fn every_command_points_uv_at_the_resolved_venv() {
        // This pairing is what lets a project's venv live anywhere.
        let spec = sync(&uv(), &ctx());
        assert_eq!(
            spec.env.get("UV_PROJECT_ENVIRONMENT").map(String::as_str),
            Some("/envs/my-api-3f9c1a")
        );
    }

    #[test]
    fn sync_is_a_bare_sync() {
        assert_eq!(sync(&uv(), &ctx()).args, vec!["sync"]);
    }

    #[test]
    fn creating_a_venv_pins_the_interpreter() {
        assert_eq!(
            sync_with_python(&uv(), &ctx(), "3.12").args,
            vec!["sync", "-p", "3.12"]
        );
    }

    #[test]
    fn add_targets_the_right_group() {
        let cases = [
            (DependencyGroup::Main, vec!["add", "httpx>=0.27"]),
            (DependencyGroup::Dev, vec!["add", "httpx>=0.27", "--dev"]),
            (
                DependencyGroup::Named("test".into()),
                vec!["add", "httpx>=0.27", "--group", "test"],
            ),
            (
                DependencyGroup::Optional("cli".into()),
                vec!["add", "httpx>=0.27", "--optional", "cli"],
            ),
        ];
        for (group, expected) in cases {
            assert_eq!(add(&uv(), &ctx(), "httpx>=0.27", group.clone()).args, expected, "group {group:?}");
        }
    }

    #[test]
    fn remove_targets_the_right_group() {
        let cases = [
            (DependencyGroup::Main, vec!["remove", "httpx"]),
            (DependencyGroup::Dev, vec!["remove", "httpx", "--dev"]),
            (
                DependencyGroup::Named("test".into()),
                vec!["remove", "httpx", "--group", "test"],
            ),
            (
                DependencyGroup::Optional("cli".into()),
                vec!["remove", "httpx", "--optional", "cli"],
            ),
        ];
        for (group, expected) in cases {
            assert_eq!(remove(&uv(), &ctx(), "httpx", group.clone()).args, expected, "group {group:?}");
        }
    }

    #[test]
    fn upgrading_one_package_locks_only_that_package() {
        assert_eq!(
            upgrade_package(&uv(), &ctx(), "httpx").args,
            vec!["lock", "-P", "httpx"]
        );
    }

    #[test]
    fn tree_requests_json_at_the_given_depth() {
        assert_eq!(
            tree(&uv(), &ctx(), 2).args,
            vec!["tree", "--format", "json", "--depth", "2"]
        );
    }

    #[test]
    fn pip_list_requests_json() {
        assert_eq!(pip_list(&uv(), &ctx()).args, vec!["pip", "list", "--format", "json"]);
    }

    #[test]
    fn python_list_requests_json_and_needs_no_project() {
        let spec = python_list(&uv());
        assert_eq!(spec.args, vec!["python", "list", "--output-format", "json"]);
        assert_eq!(spec.cwd, None, "interpreter discovery is not project-scoped");
    }

    #[test]
    fn python_install_names_the_version() {
        assert_eq!(python_install(&uv(), "3.13").args, vec!["python", "install", "3.13"]);
    }

    #[test]
    fn the_program_is_always_the_resolved_binary() {
        assert_eq!(sync(&uv(), &ctx()).program, PathBuf::from("/data/uv/uv"));
        assert_eq!(python_list(&uv()).program, PathBuf::from("/data/uv/uv"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p bombadil-core --all-features uv::command`
Expected: FAIL — `cannot find function sync`, `cannot find type ProjectContext`.

- [ ] **Step 3: Write the implementation**

Above the test module:

```rust
use super::ResolvedUv;
use crate::process::CommandSpec;
use std::path::PathBuf;

/// Which part of `pyproject.toml` a dependency operation targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyGroup {
    Main,
    Dev,
    /// A PEP 735 dependency group.
    Named(String),
    /// An entry under `[project.optional-dependencies]`.
    Optional(String),
}

impl DependencyGroup {
    /// The flags uv needs to target this group. Main needs none.
    fn flags(&self) -> Vec<String> {
        match self {
            DependencyGroup::Main => Vec::new(),
            DependencyGroup::Dev => vec!["--dev".into()],
            DependencyGroup::Named(name) => vec!["--group".into(), name.clone()],
            DependencyGroup::Optional(extra) => vec!["--optional".into(), extra.clone()],
        }
    }
}

/// The project a uv command applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectContext {
    pub project_dir: PathBuf,
    pub venv_path: PathBuf,
}

/// Base spec for a project-scoped command: correct working directory, and uv
/// pointed at the venv this project actually uses.
fn project_spec(uv: &ResolvedUv, ctx: &ProjectContext) -> CommandSpec {
    CommandSpec::new(&uv.path)
        .cwd(&ctx.project_dir)
        .env("UV_PROJECT_ENVIRONMENT", ctx.venv_path.to_string_lossy())
}

pub fn sync(uv: &ResolvedUv, ctx: &ProjectContext) -> CommandSpec {
    project_spec(uv, ctx).arg("sync")
}

pub fn sync_with_python(uv: &ResolvedUv, ctx: &ProjectContext, python: &str) -> CommandSpec {
    project_spec(uv, ctx).args(["sync", "-p", python])
}

pub fn add(
    uv: &ResolvedUv,
    ctx: &ProjectContext,
    requirement: &str,
    group: DependencyGroup,
) -> CommandSpec {
    project_spec(uv, ctx)
        .args(["add", requirement])
        .args(group.flags())
}

pub fn remove(
    uv: &ResolvedUv,
    ctx: &ProjectContext,
    package: &str,
    group: DependencyGroup,
) -> CommandSpec {
    project_spec(uv, ctx)
        .args(["remove", package])
        .args(group.flags())
}

pub fn upgrade_package(uv: &ResolvedUv, ctx: &ProjectContext, package: &str) -> CommandSpec {
    project_spec(uv, ctx).args(["lock", "-P", package])
}

pub fn tree(uv: &ResolvedUv, ctx: &ProjectContext, depth: u32) -> CommandSpec {
    project_spec(uv, ctx).args([
        "tree".to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--depth".to_string(),
        depth.to_string(),
    ])
}

pub fn pip_list(uv: &ResolvedUv, ctx: &ProjectContext) -> CommandSpec {
    project_spec(uv, ctx).args(["pip", "list", "--format", "json"])
}

/// Interpreter discovery is machine-wide, not project-scoped, so it carries
/// neither a working directory nor UV_PROJECT_ENVIRONMENT.
pub fn python_list(uv: &ResolvedUv) -> CommandSpec {
    CommandSpec::new(&uv.path).args(["python", "list", "--output-format", "json"])
}

pub fn python_install(uv: &ResolvedUv, version: &str) -> CommandSpec {
    CommandSpec::new(&uv.path).args(["python", "install", version])
}
```

Add `pub mod command;` to `crates/bombadil-core/src/uv/mod.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p bombadil-core --all-features`
Expected: PASS — 12 new tests.

- [ ] **Step 5: Commit**

```bash
git add crates/bombadil-core/src/uv
git commit -m "feat(core): build uv command lines as pure functions

Every uv invocation the app makes is now a pure CommandSpec builder, so
the exact argv is assertable in a table test rather than discoverable by
running it. Project-scoped commands always pair cwd with
UV_PROJECT_ENVIRONMENT; that pairing is what lets a venv live anywhere."
```

---

### Task 6: Parsing uv's JSON output

**Files:**
- Create: `crates/bombadil-core/src/uv/results.rs`
- Create: `crates/bombadil-core/tests/fixtures/uv-pip-list.json`
- Create: `crates/bombadil-core/tests/fixtures/uv-python-list.json`
- Create: `crates/bombadil-core/tests/fixtures/uv-tree.json`
- Modify: `crates/bombadil-core/src/uv/mod.rs`, `crates/bombadil-core/Cargo.toml`, root `Cargo.toml`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `uv::results::InstalledPackage { name: String, version: String }` and `parse_pip_list(json: &str) -> Result<Vec<InstalledPackage>, ResultsError>`
  - `uv::results::Interpreter { key: String, version: String, path: Option<PathBuf>, implementation: String }` with `fn is_installed(&self) -> bool`, and `parse_python_list(json: &str) -> Result<Vec<Interpreter>, ResultsError>`
  - `uv::results::TreeOutcome` — enum `Parsed(Vec<TreeRoot>)` | `UnsupportedSchema { found: String }`
  - `uv::results::TreeRoot { id: String }` and `parse_tree(json: &str) -> Result<TreeOutcome, ResultsError>`
  - `uv::results::TESTED_TREE_SCHEMA: &str` (`"preview"`)
  - `bombadil_core::error::ResultsError`

> **`uv tree --format json` declares `"schema": {"version": "preview"}` — uv marks it unstable.** `parse_tree` must therefore return `UnsupportedSchema` when the version is not `TESTED_TREE_SCHEMA`, so the GUI degrades to showing direct dependencies only. A future uv release must never make the app unusable.
>
> `Interpreter.path` is `None` for an interpreter uv could download but has not installed. That distinction is what drives "pick an interpreter, or offer to install one" — it is not incidental.

- [ ] **Step 1: Add serde_json and create the fixtures**

Add to the root `Cargo.toml` `[workspace.dependencies]`: `serde_json = "1"`. Add `serde_json.workspace = true` to `crates/bombadil-core/Cargo.toml`.

`crates/bombadil-core/tests/fixtures/uv-pip-list.json` — captured verbatim from uv 0.12.1:

```json
[{"name":"idna","version":"3.18"},{"name":"iniconfig","version":"2.3.0"}]
```

`crates/bombadil-core/tests/fixtures/uv-python-list.json` — one downloadable and one installed entry, trimmed from real output:

```json
[
  {
    "key": "cpython-3.15.0b4-linux-x86_64-gnu",
    "version": "3.15.0b4",
    "version_parts": { "major": 3, "minor": 15, "patch": 0 },
    "path": null,
    "symlink": null,
    "url": "https://releases.astral.sh/example.tar.gz",
    "os": "linux",
    "variant": "default",
    "implementation": "cpython",
    "arch": "x86_64",
    "libc": "gnu"
  },
  {
    "key": "cpython-3.12.4-linux-x86_64-gnu",
    "version": "3.12.4",
    "version_parts": { "major": 3, "minor": 12, "patch": 4 },
    "path": "/usr/bin/python3.12",
    "symlink": null,
    "url": null,
    "os": "linux",
    "variant": "default",
    "implementation": "cpython",
    "arch": "x86_64",
    "libc": "gnu"
  }
]
```

`crates/bombadil-core/tests/fixtures/uv-tree.json` — the real shape, trimmed:

```json
{
  "schema": { "version": "preview" },
  "workspace_root": "/proj/jsonprobe",
  "workspace": { "path": "/proj/jsonprobe", "id": "workspace+/proj/jsonprobe" },
  "roots": [
    { "id": "jsonprobe:dev==0.1.0@virtual+/proj/jsonprobe/" },
    { "id": "jsonprobe==0.1.0@virtual+/proj/jsonprobe/" }
  ]
}
```

- [ ] **Step 2: Write the failing tests**

Create `crates/bombadil-core/src/uv/results.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const PIP_LIST: &str = include_str!("../../tests/fixtures/uv-pip-list.json");
    const PYTHON_LIST: &str = include_str!("../../tests/fixtures/uv-python-list.json");
    const TREE: &str = include_str!("../../tests/fixtures/uv-tree.json");

    #[test]
    fn reads_installed_packages() {
        let got = parse_pip_list(PIP_LIST).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "idna");
        assert_eq!(got[0].version, "3.18");
    }

    #[test]
    fn an_empty_pip_list_is_not_an_error() {
        assert!(parse_pip_list("[]").unwrap().is_empty());
    }

    #[test]
    fn malformed_pip_list_json_is_an_error() {
        assert!(parse_pip_list("not json").is_err());
    }

    #[test]
    fn reads_interpreters_and_distinguishes_installed_from_downloadable() {
        let got = parse_python_list(PYTHON_LIST).unwrap();
        assert_eq!(got.len(), 2);

        // path: null means uv could download it but it is not on this machine.
        assert!(!got[0].is_installed(), "3.15.0b4 has no path and is not installed");
        assert_eq!(got[0].version, "3.15.0b4");

        assert!(got[1].is_installed(), "3.12.4 has a path and is installed");
        assert_eq!(
            got[1].path.as_deref(),
            Some(std::path::Path::new("/usr/bin/python3.12"))
        );
        assert_eq!(got[1].implementation, "cpython");
    }

    #[test]
    fn reads_the_tree_when_the_schema_is_the_one_we_tested() {
        match parse_tree(TREE).unwrap() {
            TreeOutcome::Parsed(roots) => {
                assert_eq!(roots.len(), 2);
                assert!(roots[0].id.starts_with("jsonprobe"));
            }
            other => panic!("expected Parsed, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_tree_schema_degrades_instead_of_failing() {
        // uv declares this schema unstable. A future uv must not make the app
        // unusable -- the GUI falls back to direct dependencies only.
        let json = r#"{"schema":{"version":"v2"},"roots":[]}"#;
        match parse_tree(json).unwrap() {
            TreeOutcome::UnsupportedSchema { found } => assert_eq!(found, "v2"),
            other => panic!("expected UnsupportedSchema, got {other:?}"),
        }
    }

    #[test]
    fn a_tree_with_no_schema_field_degrades_too() {
        match parse_tree(r#"{"roots":[]}"#).unwrap() {
            TreeOutcome::UnsupportedSchema { found } => assert_eq!(found, "<absent>"),
            other => panic!("expected UnsupportedSchema, got {other:?}"),
        }
    }

    #[test]
    fn malformed_tree_json_is_an_error() {
        assert!(parse_tree("{{{").is_err());
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p bombadil-core --all-features uv::results`
Expected: FAIL — `cannot find function parse_pip_list`.

- [ ] **Step 4: Write the implementation**

Above the test module:

```rust
use crate::error::ResultsError;
use serde::Deserialize;
use std::path::PathBuf;

/// The `uv tree --format json` schema version this code was written against.
/// uv marks the schema unstable, so anything else degrades rather than fails.
pub const TESTED_TREE_SCHEMA: &str = "preview";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
}

pub fn parse_pip_list(json: &str) -> Result<Vec<InstalledPackage>, ResultsError> {
    serde_json::from_str(json).map_err(|source| ResultsError::Json {
        command: "uv pip list",
        source,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Interpreter {
    pub key: String,
    pub version: String,
    /// `None` when uv can download this interpreter but it is not installed.
    #[serde(default)]
    pub path: Option<PathBuf>,
    pub implementation: String,
}

impl Interpreter {
    /// An interpreter present on this machine, as opposed to one uv offers to
    /// download.
    pub fn is_installed(&self) -> bool {
        self.path.is_some()
    }
}

pub fn parse_python_list(json: &str) -> Result<Vec<Interpreter>, ResultsError> {
    serde_json::from_str(json).map_err(|source| ResultsError::Json {
        command: "uv python list",
        source,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TreeRoot {
    pub id: String,
}

/// What came back from `uv tree`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeOutcome {
    Parsed(Vec<TreeRoot>),
    /// uv produced a schema we have not been tested against. The caller shows
    /// direct dependencies only rather than guessing at an unknown shape.
    UnsupportedSchema { found: String },
}

#[derive(Deserialize)]
struct RawSchema {
    version: String,
}

#[derive(Deserialize)]
struct RawTree {
    #[serde(default)]
    schema: Option<RawSchema>,
    #[serde(default)]
    roots: Vec<TreeRoot>,
}

pub fn parse_tree(json: &str) -> Result<TreeOutcome, ResultsError> {
    let raw: RawTree = serde_json::from_str(json).map_err(|source| ResultsError::Json {
        command: "uv tree",
        source,
    })?;

    match raw.schema {
        Some(schema) if schema.version == TESTED_TREE_SCHEMA => Ok(TreeOutcome::Parsed(raw.roots)),
        Some(schema) => Ok(TreeOutcome::UnsupportedSchema {
            found: schema.version,
        }),
        None => Ok(TreeOutcome::UnsupportedSchema {
            found: "<absent>".to_string(),
        }),
    }
}
```

Add to `crates/bombadil-core/src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum ResultsError {
    #[error("could not parse the JSON output of `{command}`: {source}")]
    Json {
        command: &'static str,
        #[source]
        source: serde_json::Error,
    },
}
```

Add `pub mod results;` to `crates/bombadil-core/src/uv/mod.rs`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p bombadil-core --all-features`
Expected: PASS — 8 new tests.

- [ ] **Step 6: Verify cargo-deny still passes with serde_json**

Run: `cargo deny check`
Expected: all four checks ok. If `serde_json` pulls a crate with a license outside `Apache-2.0`/`MIT`/`Unicode-3.0`, stop and report it rather than widening the allow list.

- [ ] **Step 7: Commit**

```bash
git add crates/bombadil-core/src/uv crates/bombadil-core/src/error.rs \
        crates/bombadil-core/tests/fixtures crates/bombadil-core/Cargo.toml Cargo.toml
git commit -m "feat(core): parse uv's JSON output

Fixtures are captured verbatim from uv 0.12.1 rather than hand-written, so
a shape change shows up as a test failure instead of a runtime surprise.

uv tree declares its JSON schema 'preview' -- explicitly unstable -- so an
unrecognised schema degrades to UnsupportedSchema and the GUI shows direct
dependencies only. A future uv release must never make the app unusable.

python list reports path: null for an interpreter uv can download but has
not installed; is_installed() exposes that distinction, which is what
drives offering to install a missing interpreter."
```

---

### Task 7: Integration test against a real uv

Everything above is tested without spawning a process. This task proves the argv builders and parsers actually agree with uv.

**Files:**
- Create: `crates/bombadil-core/tests/uv_integration.rs`
- Modify: `crates/bombadil-core/Cargo.toml`, `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: everything from Tasks 1, 4, 5, 6.
- Produces: no library API.

> Gated behind a `uv-integration` feature so the default `cargo test` stays hermetic. It uses whatever `uv` is on `PATH`; CI installs one explicitly.

- [ ] **Step 1: Declare the feature**

In `crates/bombadil-core/Cargo.toml`, add `uv-integration = []` under `[features]`.

- [ ] **Step 2: Write the test**

`crates/bombadil-core/tests/uv_integration.rs`:

```rust
//! Proves the command builders and parsers agree with a real uv.
//!
//! Run with: cargo test -p bombadil-core --features uv-integration,test-support --test uv_integration
#![cfg(feature = "uv-integration")]

use bombadil_core::model::UvSource;
use bombadil_core::process::{CommandRunner, SystemCommandRunner};
use bombadil_core::uv::command::{self, DependencyGroup, ProjectContext};
use bombadil_core::uv::resolve;
use bombadil_core::uv::results::{parse_pip_list, parse_tree};

fn write_project(dir: &std::path::Path) {
    std::fs::write(
        dir.join("pyproject.toml"),
        "[project]\nname = \"itest\"\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\ndependencies = []\n",
    )
    .unwrap();
}

#[test]
fn sync_add_and_list_agree_with_a_real_uv() {
    let runner = SystemCommandRunner;
    let uv = resolve(&UvSource::FromPath, &runner, &|| {
        Err("bundled uv is not used by this test".to_string())
    })
    .expect("a uv on PATH at or above MIN_UV_VERSION is required for this test");

    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();
    write_project(&project_dir);

    let ctx = ProjectContext {
        project_dir: project_dir.clone(),
        venv_path: tmp.path().join("env"),
    };

    // Create the environment.
    let outcome = runner.run(&command::sync(&uv, &ctx)).unwrap();
    assert!(outcome.success(), "sync failed: {}", outcome.stderr);
    assert!(
        ctx.venv_path.join("pyvenv.cfg").exists(),
        "UV_PROJECT_ENVIRONMENT must place the venv where we asked"
    );

    // Add a tiny, dependency-free package.
    let outcome = runner
        .run(&command::add(&uv, &ctx, "iniconfig", DependencyGroup::Main))
        .unwrap();
    assert!(outcome.success(), "add failed: {}", outcome.stderr);

    // uv, not Bombadil, wrote the manifest.
    let manifest = std::fs::read_to_string(project_dir.join("pyproject.toml")).unwrap();
    assert!(manifest.contains("iniconfig"), "got:\n{manifest}");

    // The installed list parses and contains what we added.
    let outcome = runner.run(&command::pip_list(&uv, &ctx)).unwrap();
    assert!(outcome.success(), "pip list failed: {}", outcome.stderr);
    let packages = parse_pip_list(&outcome.stdout).expect("pip list JSON parses");
    assert!(
        packages.iter().any(|p| p.name == "iniconfig"),
        "got {packages:?}"
    );

    // The tree parses, or degrades in the way we designed for.
    let outcome = runner.run(&command::tree(&uv, &ctx, 2)).unwrap();
    assert!(outcome.success(), "tree failed: {}", outcome.stderr);
    parse_tree(&outcome.stdout).expect("tree JSON parses or degrades, never errors on valid JSON");
}
```

> This test needs both the `uv-integration` and `test-support` features, because `resolve` takes `&dyn CommandRunner` and the integration binary links `bombadil-core` as an external crate.

- [ ] **Step 3: Run it**

```bash
cargo test -p bombadil-core --features uv-integration,test-support --test uv_integration
```
Expected: PASS. Requires `uv >= 0.12.1` on `PATH`.

- [ ] **Step 4: Add a CI job**

Add to `.github/workflows/ci.yml`:

```yaml
  uv-integration:
    # Proves the argv builders and JSON parsers agree with a real uv, which
    # the hermetic unit tests deliberately cannot.
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - uses: astral-sh/setup-uv@v5
        with:
          version: "0.12.1"
      - name: Integration test
        run: cargo test -p bombadil-core --locked --features uv-integration,test-support --test uv_integration
```

- [ ] **Step 5: Full verification and commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --workspace --locked
git add crates/bombadil-core/tests/uv_integration.rs crates/bombadil-core/Cargo.toml .github/workflows/ci.yml
git commit -m "test(core): integration test against a real uv

Everything else in this plan is tested without spawning a process, which
is the point -- but it means nothing verifies that our argv actually match
what uv accepts. This does: it creates a venv at a redirected location,
adds a package, and parses the real pip list and tree output.

Feature-gated so the default test run stays hermetic and offline."
```

---

## Done when

- `cargo test --workspace --locked` passes on Linux, macOS and Windows.
- `cargo clippy --all-targets --all-features --locked -- -D warnings` is clean.
- `cargo deny check` passes with the unchanged allow list.
- `cargo test -p bombadil-core` (default features) neither downloads nor spawns anything.
- `BOMBADIL_UV_ARCHIVE=<path> cargo build -p bombadil-uv-bin` builds with no network access to GitHub.
- The `uv-integration` job is green in CI.

## What plan 2b picks up

`SecretStore` over the OS keychain with an in-memory implementation, and the `envspec` pipeline: layering global and per-project variables, resolving secrets, injecting `UV_DEFAULT_INDEX`/`UV_INDEX`/`UV_INDEX_<NAME>_USERNAME`, setting `VIRTUAL_ENV` and prepending to `PATH`, and capturing pre-activate script exports via the env-dump-diff mechanism (verified working: `sh -c '. script; env -0'` captures values containing spaces).
