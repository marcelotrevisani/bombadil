# Bombadil Secrets and Environment Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compose the environment every child process runs in — global and per-project variables, secrets from the OS keychain, private index credentials, pre-activate script exports, and venv activation — through one pipeline used by both spawned terminals and Bombadil's own uv commands.

**Architecture:** A `SecretStore` trait keeps the OS keychain behind a seam so everything above it is testable in memory. `envspec` is one pure-ish function composing eight ordered layers into a `BTreeMap`. Pre-activate scripts run through `CommandRunner` and have their exports captured by diffing an environment dump, so a script can genuinely export.

**Tech Stack:** Rust (edition 2024), `keyring` 4.1.6 for the OS keychain, building on `bombadil-core`'s existing `model`, `process`, `uv` and `members` modules.

## Scope

This is **plan 2b of the remaining three**. It implements spec §6 (Environment resolution) and the secrets half of §5.

Merged already: plan 1, plan 2a, and spec §5a (monorepo members). Plan 2c (terminal detection and spawning) consumes this plan's output; plan 3 is the GUI.

**Not in this plan:** terminal detection, any GUI, and calling the pipeline from anywhere. Like its predecessors this is headless — it builds the composition and proves it, and 2c/3 are the consumers.

## Verification already done

Checked against this machine and a real uv 0.12.1 on 2026-08-02. Do not re-litigate; do not "improve" code that depends on these.

- **`keyring` 4.1.6 works with no feature flags.** Verified a full round-trip against the live Secret Service: `Entry::new(service, user) -> Result<Entry>`, `set_password(&str)`, `get_password() -> Result<String>`, `delete_credential()`, and `keyring::Error::NoEntry` for a missing entry. Unlike keyring 3.x there are no backend feature flags to select.
- **Direct activation works — no `source activate` needed.** With only `VIRTUAL_ENV` set and the venv's `bin` prepended to `PATH`, `python` resolved to the venv interpreter and imported from the venv's `site-packages`. Without it, the ambient conda environment won. This is spec §6 layer 8 proven.
- **`env -0` capture survives everything that matters.** Values containing spaces and embedded `=` come back intact (`EQUALS=k=v`); a **multiline** value survives as `'line1\nline2'`; a variable the script `unset`s is genuinely absent from the dump, so removals are detectable; and a script exiting 3 makes the whole `sh -c` exit 3, so failure is observable.
- **Index injection is env-only.** `UV_DEFAULT_INDEX="<name>=<url>"`, `UV_INDEX` as space-separated `name=url` pairs, and `UV_INDEX_<NAME>_USERNAME`/`_PASSWORD` where `<NAME>` is uppercased with `-` mapped to `_`. uv sends Basic auth preemptively. No project file is touched.

## Global Constraints

Every task's requirements implicitly include this section.

- **Rust edition 2024, `rust-version = "1.88"`.**
- **`keyring` 4.1.6** is the only new dependency. If adding it makes `cargo deny check` fail on a licence outside the allow list (`Apache-2.0`, `MIT`, `Unicode-3.0`, `ISC`, `CDLA-Permissive-2.0`, `BSD-3-Clause`, plus the scoped MPL-2.0 exception for `option-ext`), **stop and report** — do not widen the list.
- `bombadil-core` must not depend on `iced` or on `bombadil-uv-bin`.
- **No unit test may touch the real keychain.** Tests use the in-memory implementation. Only the feature-gated integration test may touch anything real, and it must namespace and clean up after itself.
- **No process may be spawned outside `CommandRunner`.** Unit tests spawn nothing and touch no network.
- **Secrets never enter `config.toml`** and must never appear in a `Debug` rendering, a log line, or a failure report.
- The composed environment is a `BTreeMap<String, String>` so ordering is stable for assertions and reports.
- Two clippy lints are DENIED in CI: `derivable_impls` and `field_reassign_with_default`.
- CI must pass `cargo fmt --all --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, `cargo test --workspace --locked`, `cargo deny check`, the `msrv` job at 1.88.0, and `uv-integration` on all three platforms.

## A platform risk to name up front

The pre-activate script mechanism is **verified on POSIX and unverified on Windows.** The POSIX side is `sh -c '. <script>; env -0'`, proven above. The Windows side must use PowerShell and emit the same NUL-separated contract, and cannot be exercised on this machine.

The last two branches each shipped a Windows defect that only CI caught — a config path that was wrong on Windows, and four tests silently asserting Unix path semantics. **Task 5's integration test therefore runs on all three platforms deliberately, and a red Windows job there is the expected way to learn the PowerShell command is wrong.** Do not weaken or skip that job to get green.

---

## File Structure

```
crates/bombadil-core/src/
    secrets/
        mod.rs              # SecretStore trait, SecretKey, re-exports
        memory.rs           # InMemorySecretStore for tests and fallback
        keyring_store.rs    # the real OS keychain
    envspec/
        mod.rs              # compose(), EnvSpec, re-exports
        dump.rs             # parse a NUL-separated environment dump
        script.rs           # run pre-activate scripts, capture their exports
        index.rs            # private index env injection
    error.rs                # + SecretError, EnvError   (modified)
    lib.rs                  # + pub mod secrets; pub mod envspec;  (modified)
crates/bombadil-core/tests/
    uv_integration.rs       # + composed-environment test  (modified)
```

`secrets/` owns "where does a secret live"; `envspec/` owns "what environment does a child process get". Each file has one responsibility.

---

### Task 1: The SecretStore seam

**Files:**
- Create: `crates/bombadil-core/src/secrets/mod.rs`, `secrets/memory.rs`
- Modify: `crates/bombadil-core/src/error.rs`, `src/lib.rs`

**Interfaces:**
- Produces:
  - `secrets::SecretKey` — enum `Index { name: String }`, `ProjectEnv { project_id: Uuid, key: String }`, `GlobalEnv { key: String }`, with `fn account(&self) -> String`
  - `secrets::SecretStore` — trait with `get(&self, key: &SecretKey) -> Result<Option<String>, SecretError>`, `set(&self, key: &SecretKey, value: &str) -> Result<(), SecretError>`, `delete(&self, key: &SecretKey) -> Result<(), SecretError>`, `is_available(&self) -> bool`
  - `secrets::InMemorySecretStore` with `new()`
  - `error::SecretError`
  - `secrets::SERVICE: &str = "bombadil"`

> **`get` returns `Ok(None)` for a missing secret, not an error.** A missing secret is an ordinary state — the user has not filled it in yet — and forcing every caller to distinguish "absent" from "keychain broken" via error matching guarantees someone gets it wrong.
>
> The three `SecretKey` variants are exactly spec §5's three entry shapes. `GlobalEnv` exists because the spec originally defined only `index/<name>` and `env/<project-id>/<KEY>`, leaving a global secret variable with nowhere to live; §5 now names `env/global/<KEY>`.

- [ ] **Step 1: Write the failing tests**

Create `crates/bombadil-core/src/secrets/mod.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn account_names_match_the_three_documented_shapes() {
        // Spec section 5. These strings are what actually lands in the OS
        // keychain, so changing them orphans every stored secret.
        assert_eq!(
            SecretKey::Index { name: "priv".into() }.account(),
            "index/priv"
        );
        assert_eq!(
            SecretKey::GlobalEnv { key: "TOKEN".into() }.account(),
            "env/global/TOKEN"
        );
        assert_eq!(
            SecretKey::ProjectEnv {
                project_id: Uuid::nil(),
                key: "DATABASE_PASSWORD".into(),
            }
            .account(),
            "env/00000000-0000-0000-0000-000000000000/DATABASE_PASSWORD"
        );
    }

    #[test]
    fn a_missing_secret_is_none_not_an_error() {
        // "Not set yet" is an ordinary state, not a failure. Making callers
        // distinguish it from a broken keychain by matching errors guarantees
        // someone eventually gets it wrong.
        let store = InMemorySecretStore::new();
        let key = SecretKey::GlobalEnv { key: "ABSENT".into() };
        assert_eq!(store.get(&key).unwrap(), None);
    }

    #[test]
    fn a_stored_secret_round_trips() {
        let store = InMemorySecretStore::new();
        let key = SecretKey::Index { name: "priv".into() };
        store.set(&key, "hunter2").unwrap();
        assert_eq!(store.get(&key).unwrap().as_deref(), Some("hunter2"));
    }

    #[test]
    fn deleting_removes_it_and_deleting_again_is_not_an_error() {
        let store = InMemorySecretStore::new();
        let key = SecretKey::Index { name: "priv".into() };
        store.set(&key, "hunter2").unwrap();
        store.delete(&key).unwrap();
        assert_eq!(store.get(&key).unwrap(), None);
        store.delete(&key).expect("deleting an absent secret is a no-op");
    }

    #[test]
    fn different_projects_do_not_share_a_secret() {
        // The project id is in the account name precisely so two projects with
        // the same variable name keep separate values.
        let store = InMemorySecretStore::new();
        let a = SecretKey::ProjectEnv {
            project_id: Uuid::from_u128(1),
            key: "TOKEN".into(),
        };
        let b = SecretKey::ProjectEnv {
            project_id: Uuid::from_u128(2),
            key: "TOKEN".into(),
        };
        store.set(&a, "alpha").unwrap();
        store.set(&b, "beta").unwrap();
        assert_eq!(store.get(&a).unwrap().as_deref(), Some("alpha"));
        assert_eq!(store.get(&b).unwrap().as_deref(), Some("beta"));
    }

    #[test]
    fn the_in_memory_store_reports_itself_available() {
        assert!(InMemorySecretStore::new().is_available());
    }

    #[test]
    fn a_secret_value_never_appears_in_a_debug_rendering() {
        // Failure reports and logs render Debug. A secret leaking through it
        // would defeat the entire point of the keychain.
        let store = InMemorySecretStore::new();
        store
            .set(&SecretKey::Index { name: "priv".into() }, "hunter2")
            .unwrap();
        let rendered = format!("{store:?}");
        assert!(!rendered.contains("hunter2"), "got {rendered}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p bombadil-core secrets`
Expected: FAIL — `cannot find type SecretKey`.

- [ ] **Step 3: Write the implementation**

Above the test module in `crates/bombadil-core/src/secrets/mod.rs`:

```rust
mod memory;

pub use memory::InMemorySecretStore;

use crate::error::SecretError;
use uuid::Uuid;

/// The keychain service every Bombadil secret is stored under.
pub const SERVICE: &str = "bombadil";

/// Which secret, addressed the way spec section 5 names them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SecretKey {
    /// Credentials for a named package index.
    Index { name: String },
    /// A secret environment variable belonging to one project.
    ProjectEnv { project_id: Uuid, key: String },
    /// A secret environment variable applying to every project.
    GlobalEnv { key: String },
}

impl SecretKey {
    /// The account name under [`SERVICE`].
    ///
    /// These strings are what actually lands in the OS keychain: changing one
    /// orphans every secret already stored under the old name, with no error
    /// and no way to find it again from inside the app.
    pub fn account(&self) -> String {
        match self {
            SecretKey::Index { name } => format!("index/{name}"),
            SecretKey::ProjectEnv { project_id, key } => format!("env/{project_id}/{key}"),
            SecretKey::GlobalEnv { key } => format!("env/global/{key}"),
        }
    }
}

/// Where secret material lives.
///
/// Behind a trait so everything above it is testable without a keychain —
/// which matters because headless Linux and WSL frequently have no Secret
/// Service at all.
pub trait SecretStore: Send + Sync {
    /// `Ok(None)` means "not set", which is an ordinary state rather than a
    /// failure. An `Err` means the store itself could not be consulted.
    fn get(&self, key: &SecretKey) -> Result<Option<String>, SecretError>;

    fn set(&self, key: &SecretKey, value: &str) -> Result<(), SecretError>;

    /// Deleting a secret that is not there succeeds.
    fn delete(&self, key: &SecretKey) -> Result<(), SecretError>;

    /// Whether this store can be used at all. False on a machine with no
    /// usable keychain, which the caller surfaces rather than failing later at
    /// spawn time with something cryptic.
    fn is_available(&self) -> bool;
}
```

Create `crates/bombadil-core/src/secrets/memory.rs`:

```rust
use super::{SecretKey, SecretStore};
use crate::error::SecretError;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// An in-memory store, used by tests and as the session-only fallback when the
/// machine has no usable keychain.
///
/// `Debug` is hand-written to print only how many secrets are held: the
/// derived one would render values, and failure reports render `Debug`.
#[derive(Default)]
pub struct InMemorySecretStore {
    entries: Mutex<BTreeMap<String, String>>,
}

impl std::fmt::Debug for InMemorySecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.entries.lock().map(|e| e.len()).unwrap_or(0);
        write!(f, "InMemorySecretStore({count} secrets)")
    }
}

impl InMemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, key: &SecretKey) -> Result<Option<String>, SecretError> {
        Ok(self.entries.lock().unwrap().get(&key.account()).cloned())
    }

    fn set(&self, key: &SecretKey, value: &str) -> Result<(), SecretError> {
        self.entries
            .lock()
            .unwrap()
            .insert(key.account(), value.to_string());
        Ok(())
    }

    fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        self.entries.lock().unwrap().remove(&key.account());
        Ok(())
    }

    fn is_available(&self) -> bool {
        true
    }
}
```

Add to `crates/bombadil-core/src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("the OS keychain is not available on this machine: {message}")]
    Unavailable { message: String },

    #[error("could not read or write the secret for {account}: {message}")]
    Backend { account: String, message: String },
}
```

> `SecretError` deliberately carries a `String` message rather than the underlying `keyring::Error`: the account name is safe to render, but wrapping the backend error risks a `Debug` rendering that includes attempted values. Nothing about a secret's *value* may reach a log.

Add `pub mod secrets;` to `crates/bombadil-core/src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p bombadil-core --all-features`
Expected: PASS — 7 new tests.

- [ ] **Step 5: Commit**

```bash
git add crates/bombadil-core/src/secrets crates/bombadil-core/src/error.rs crates/bombadil-core/src/lib.rs
git commit -m "feat(core): add the SecretStore seam and an in-memory store

get returns Ok(None) for a missing secret rather than an error: 'not set
yet' is an ordinary state, and making callers distinguish it from a broken
keychain by matching errors guarantees someone eventually gets it wrong.

Debug is hand-written to print only a count. Failure reports render Debug,
and a secret leaking through one would defeat the point of the keychain."
```

---

### Task 2: The real OS keychain

**Files:**
- Create: `crates/bombadil-core/src/secrets/keyring_store.rs`
- Modify: `crates/bombadil-core/src/secrets/mod.rs`, root `Cargo.toml`, `crates/bombadil-core/Cargo.toml`

**Interfaces:**
- Consumes: `SecretStore`, `SecretKey`, `SERVICE`, `SecretError` (Task 1).
- Produces: `secrets::KeyringSecretStore` with `new()`.

> **`keyring` 4.1.6 needs no feature flags** — verified by a live round-trip against Secret Service. Add it plainly.
>
> **No unit test in this task may touch the real keychain.** Reading or writing a developer's actual credential store from `cargo test` is unacceptable, and CI has no keychain at all. The only permitted test is that `is_available()` returns a bool without panicking. Real round-trip coverage is the feature-gated integration test in Task 5.

- [ ] **Step 1: Add the dependency**

Root `Cargo.toml` under `[workspace.dependencies]`: `keyring = "4.1.6"`.
`crates/bombadil-core/Cargo.toml`: `keyring.workspace = true`.

Run `cargo deny check` immediately. If it fails on a licence, **stop and report** — do not edit `deny.toml`.

- [ ] **Step 2: Write the failing test**

Create `crates/bombadil-core/src/secrets/keyring_store.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_can_be_probed_without_panicking() {
        // The only unit test permitted against the real keychain: that asking
        // whether it exists is safe. CI machines have no keychain, and a
        // developer's real credential store must never be read or written by
        // `cargo test`. Round-trip coverage lives in the integration test.
        let store = KeyringSecretStore::new();
        let _: bool = store.is_available();
    }

    #[test]
    fn the_store_reveals_nothing_in_debug() {
        let rendered = format!("{:?}", KeyringSecretStore::new());
        assert!(!rendered.is_empty());
        assert!(!rendered.to_lowercase().contains("password"), "got {rendered}");
    }
}
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo test -p bombadil-core secrets::keyring_store`
Expected: FAIL — `cannot find type KeyringSecretStore`.

- [ ] **Step 4: Write the implementation**

Above the test module:

```rust
use super::{SECRET_PROBE_ACCOUNT, SecretKey, SecretStore, SERVICE};
use crate::error::SecretError;

/// The real OS keychain: Keychain on macOS, Credential Manager on Windows,
/// Secret Service on Linux.
///
/// Verified against keyring 4.1.6, which needs no backend feature flags.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringSecretStore;

impl KeyringSecretStore {
    pub fn new() -> Self {
        Self
    }

    fn entry(key: &SecretKey) -> Result<keyring::Entry, SecretError> {
        let account = key.account();
        keyring::Entry::new(SERVICE, &account).map_err(|e| SecretError::Backend {
            account,
            message: e.to_string(),
        })
    }
}

impl SecretStore for KeyringSecretStore {
    fn get(&self, key: &SecretKey) -> Result<Option<String>, SecretError> {
        match Self::entry(key)?.get_password() {
            Ok(value) => Ok(Some(value)),
            // Not set yet is ordinary, not a failure.
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Backend {
                account: key.account(),
                message: e.to_string(),
            }),
        }
    }

    fn set(&self, key: &SecretKey, value: &str) -> Result<(), SecretError> {
        Self::entry(key)?
            .set_password(value)
            .map_err(|e| SecretError::Backend {
                account: key.account(),
                message: e.to_string(),
            })
    }

    fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Backend {
                account: key.account(),
                message: e.to_string(),
            }),
        }
    }

    /// Probe by asking for a namespaced account that is never written.
    ///
    /// A `NoEntry` answer proves the backend responded, which is what we care
    /// about; only a hard error means unusable. Headless Linux and WSL often
    /// have no Secret Service, and the caller offers session-only entry
    /// instead of failing later at spawn time with something cryptic.
    fn is_available(&self) -> bool {
        match keyring::Entry::new(SERVICE, SECRET_PROBE_ACCOUNT) {
            Ok(entry) => !matches!(entry.get_password(), Err(keyring::Error::PlatformFailure(_))),
            Err(_) => false,
        }
    }
}
```

Add to `crates/bombadil-core/src/secrets/mod.rs`:

```rust
mod keyring_store;

pub use keyring_store::KeyringSecretStore;

/// An account name never written to, used only to probe whether the keychain
/// answers at all.
pub const SECRET_PROBE_ACCOUNT: &str = "availability-probe";
```

> `keyring::Error::PlatformFailure(_)` is verified to exist in 4.1.6 — a compile check against the real crate confirmed the variant. The rule is: `NoEntry` means the backend answered and is therefore available; a platform failure means unusable.

- [ ] **Step 5: Verify and commit**

```bash
cargo test -p bombadil-core --all-features && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo deny check
git add crates/bombadil-core/src/secrets Cargo.toml crates/bombadil-core/Cargo.toml
git commit -m "feat(core): store secrets in the OS keychain

keyring 4.1.6 needs no backend feature flags -- verified by a live
round-trip. NoEntry maps to Ok(None) so 'not set' stays an ordinary state.

No unit test touches the real keychain: reading a developer's actual
credential store from cargo test is unacceptable and CI has none at all.
Round-trip coverage is the feature-gated integration test."
```

---

### Task 3: Parsing an environment dump

**Files:**
- Create: `crates/bombadil-core/src/envspec/mod.rs`, `envspec/dump.rs`
- Modify: `crates/bombadil-core/src/lib.rs`

**Interfaces:**
- Produces: `envspec::parse_dump(bytes: &[u8]) -> BTreeMap<String, String>`

> **The contract, verified with a real `env -0`:** records are separated by NUL, and each record splits on its **first** `=`. Values may contain spaces, further `=` signs, and **newlines** — a multiline value came back as `line1\nline2` intact. Splitting on newlines instead of NUL, or on the last `=`, silently corrupts real environments.
>
> Invalid UTF-8 is dropped rather than erroring: an environment with an undecodable variable should still yield the rest.

- [ ] **Step 1: Write the failing tests**

Create `crates/bombadil-core/src/envspec/dump.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_records_on_nul_and_keys_on_the_first_equals() {
        let raw = b"SIMPLE=one\0EQUALS=k=v\0SPACED=two words\0";
        let got = parse_dump(raw);
        assert_eq!(got.get("SIMPLE").map(String::as_str), Some("one"));
        assert_eq!(
            got.get("EQUALS").map(String::as_str),
            Some("k=v"),
            "splitting on the last '=' would give 'k' and lose the rest"
        );
        assert_eq!(got.get("SPACED").map(String::as_str), Some("two words"));
    }

    #[test]
    fn a_multiline_value_survives_intact() {
        // Verified against a real `env -0`: this is exactly why the dump is
        // NUL-separated. Splitting on newlines would truncate it to "line1".
        let raw = b"MULTI=line1\nline2\0OTHER=x\0";
        let got = parse_dump(raw);
        assert_eq!(got.get("MULTI").map(String::as_str), Some("line1\nline2"));
        assert_eq!(got.get("OTHER").map(String::as_str), Some("x"));
    }

    #[test]
    fn an_empty_value_is_kept_not_dropped() {
        // An exported-but-empty variable is meaningfully different from an
        // unset one, and some tools rely on the distinction.
        let got = parse_dump(b"EMPTY=\0");
        assert_eq!(got.get("EMPTY").map(String::as_str), Some(""));
    }

    #[test]
    fn records_without_an_equals_are_skipped() {
        let got = parse_dump(b"NOEQUALS\0GOOD=yes\0");
        assert!(!got.contains_key("NOEQUALS"));
        assert_eq!(got.get("GOOD").map(String::as_str), Some("yes"));
    }

    #[test]
    fn a_trailing_separator_does_not_produce_an_empty_entry() {
        let got = parse_dump(b"A=1\0");
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn an_empty_dump_is_an_empty_map() {
        assert!(parse_dump(b"").is_empty());
    }

    #[test]
    fn undecodable_records_are_dropped_and_the_rest_survive() {
        // One bad variable must not cost us the whole environment.
        let mut raw = b"GOOD=yes\0BAD=".to_vec();
        raw.push(0xff);
        raw.push(0);
        let got = parse_dump(&raw);
        assert_eq!(got.get("GOOD").map(String::as_str), Some("yes"));
        assert!(!got.contains_key("BAD"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p bombadil-core envspec::dump`
Expected: FAIL — `cannot find function parse_dump`.

- [ ] **Step 3: Implement**

Above the test module:

```rust
use std::collections::BTreeMap;

/// Parse the output of a NUL-separated environment dump (`env -0`, or the
/// PowerShell equivalent).
///
/// Records are separated by NUL and each splits on its **first** `=`. Values
/// may contain spaces, further `=` signs and newlines — verified against a
/// real `env -0`, where a multiline value survives as `line1\nline2`. This is
/// precisely why the dump is NUL-separated rather than line-separated.
///
/// Records that are not valid UTF-8, or that carry no `=`, are dropped: one
/// undecodable variable must not cost the caller the whole environment.
pub fn parse_dump(bytes: &[u8]) -> BTreeMap<String, String> {
    bytes
        .split(|b| *b == 0)
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let text = std::str::from_utf8(record).ok()?;
            let (key, value) = text.split_once('=')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}
```

Create `crates/bombadil-core/src/envspec/mod.rs`:

```rust
mod dump;

pub use dump::parse_dump;
```

Add `pub mod envspec;` to `crates/bombadil-core/src/lib.rs`.

- [ ] **Step 4: Verify and commit**

```bash
cargo test -p bombadil-core --all-features
git add crates/bombadil-core/src/envspec crates/bombadil-core/src/lib.rs
git commit -m "feat(core): parse a NUL-separated environment dump

Records split on NUL and keys on the FIRST '='. Verified against a real
env -0: values contain spaces, further '=' signs, and newlines -- a
multiline value survives as line1\\nline2, which is exactly why the dump is
NUL-separated. Splitting on newlines would silently truncate it."
```

---

### Task 4: Capturing what a pre-activate script exports

**Files:**
- Create: `crates/bombadil-core/src/envspec/script.rs`
- Modify: `crates/bombadil-core/src/envspec/mod.rs`, `crates/bombadil-core/src/error.rs`

**Interfaces:**
- Consumes: `CommandRunner`, `CommandSpec` (plan 2a); `parse_dump` (Task 3); `model::Script` (plan 1).
- Produces:
  - `envspec::script::dump_command(script: &Script) -> CommandSpec`
  - `envspec::script::apply(script, runner, base) -> Result<BTreeMap<String, String>, EnvError>`
  - `error::EnvError`

> **Why this exists.** A script run as a subprocess cannot normally change its parent's environment, which would make "pre-activate script" a lie. So the script is sourced and the resulting environment dumped, and the difference against the input is folded in.
>
> **Verified on POSIX:** `sh -c '. <script>; env -0'` captures exports including spaces, `=` and newlines; a variable the script `unset`s is genuinely absent from the dump, so **removals are detectable and must be honoured**; and a script exiting 3 makes the whole `sh -c` exit 3, so failure is observable.
>
> **Unverified on Windows.** The PowerShell form emits the same NUL-separated contract. It cannot be tested on the development machine — Task 5's integration test runs on all three platforms so CI is what proves it.
>
> A script that exits non-zero **aborts** and its output is reported. It does not partially apply.

- [ ] **Step 1: Write the failing tests**

Create `crates/bombadil-core/src/envspec/script.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{CommandOutcome, FakeCommandRunner};
    use std::path::PathBuf;

    fn script(path: &str) -> Script {
        Script {
            path: PathBuf::from(path),
            args: vec![],
            enabled: true,
        }
    }

    fn base() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("KEEP".to_string(), "unchanged".to_string()),
            ("REPLACED".to_string(), "before".to_string()),
            ("REMOVED".to_string(), "present".to_string()),
        ])
    }

    fn dump(records: &[&str]) -> String {
        let mut out = String::new();
        for r in records {
            out.push_str(r);
            out.push('\0');
        }
        out
    }

    #[test]
    fn an_exported_variable_is_added() {
        let runner = FakeCommandRunner::new();
        runner.expect(
            "sh",
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: dump(&["KEEP=unchanged", "REPLACED=before", "REMOVED=present", "NEW=added"]),
                stderr: String::new(),
            },
        );

        let got = apply(&script("/p/pre.sh"), &runner, &base()).unwrap();

        assert_eq!(got.get("NEW").map(String::as_str), Some("added"));
        assert_eq!(got.get("KEEP").map(String::as_str), Some("unchanged"));
    }

    #[test]
    fn a_changed_variable_takes_the_scripts_value() {
        let runner = FakeCommandRunner::new();
        runner.expect(
            "sh",
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: dump(&["KEEP=unchanged", "REPLACED=after", "REMOVED=present"]),
                stderr: String::new(),
            },
        );

        let got = apply(&script("/p/pre.sh"), &runner, &base()).unwrap();

        assert_eq!(got.get("REPLACED").map(String::as_str), Some("after"));
    }

    #[test]
    fn a_variable_the_script_unsets_is_removed() {
        // Verified against a real shell: an unset variable is genuinely absent
        // from the dump, so a script can remove one and we must honour it.
        let runner = FakeCommandRunner::new();
        runner.expect(
            "sh",
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: dump(&["KEEP=unchanged", "REPLACED=before"]),
                stderr: String::new(),
            },
        );

        let got = apply(&script("/p/pre.sh"), &runner, &base()).unwrap();

        assert!(!got.contains_key("REMOVED"), "got {got:?}");
    }

    #[test]
    fn a_failing_script_aborts_and_reports_its_output() {
        // Partial application would leave a half-built environment that is
        // worse than none, so a non-zero exit stops the whole composition.
        let runner = FakeCommandRunner::new();
        runner.expect(
            "sh",
            &[],
            CommandOutcome {
                status: Some(3),
                stdout: String::new(),
                stderr: "boom".into(),
            },
        );

        let err = apply(&script("/p/pre.sh"), &runner, &base()).unwrap_err();

        let rendered = err.to_string();
        assert!(rendered.contains("/p/pre.sh"), "got {rendered}");
        assert!(rendered.contains("boom"), "got {rendered}");
    }

    #[test]
    fn a_disabled_script_is_not_run_at_all() {
        let runner = FakeCommandRunner::new();
        let mut s = script("/p/pre.sh");
        s.enabled = false;

        let got = apply(&s, &runner, &base()).unwrap();

        assert_eq!(got, base());
        assert!(runner.calls().is_empty(), "a disabled script must not run");
    }

    #[cfg(unix)]
    #[test]
    fn the_posix_command_sources_the_script_and_dumps_the_environment() {
        let spec = dump_command(&script("/p/pre.sh"));
        assert_eq!(spec.program, PathBuf::from("sh"));
        assert_eq!(spec.args.len(), 2);
        assert_eq!(spec.args[0], "-c");
        assert!(spec.args[1].contains("/p/pre.sh"), "got {}", spec.args[1]);
        assert!(
            spec.args[1].contains("env -0"),
            "the dump must be NUL-separated; got {}",
            spec.args[1]
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_command_uses_powershell_and_the_same_nul_contract() {
        let spec = dump_command(&script(r"C:\p\pre.ps1"));
        assert!(
            spec.program.to_string_lossy().to_lowercase().contains("powershell"),
            "got {:?}",
            spec.program
        );
        let joined = spec.args.join(" ");
        assert!(joined.contains(r"C:\p\pre.ps1"), "got {joined}");
        assert!(joined.contains("[char]0"), "the dump must be NUL-separated; got {joined}");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p bombadil-core envspec::script`
Expected: FAIL — `cannot find function apply`.

- [ ] **Step 3: Implement**

Above the test module:

```rust
use super::parse_dump;
use crate::error::EnvError;
use crate::model::Script;
use crate::process::{CommandRunner, CommandSpec};
use std::collections::BTreeMap;

/// Build the command that sources a script and dumps the resulting
/// environment.
///
/// POSIX uses `sh -c '. <script>; env -0'`, verified to capture values with
/// spaces, `=` and newlines, and to surface the script's exit status.
///
/// Windows uses PowerShell writing the same NUL-separated records. That form
/// is NOT verified on a development machine — the integration test runs on the
/// Windows CI job, which is what proves it.
pub fn dump_command(script: &Script) -> CommandSpec {
    let path = script.path.to_string_lossy();
    let args = script.args.join(" ");

    #[cfg(not(windows))]
    {
        let program = format!(". '{path}' {args} >/dev/null 2>&1; env -0");
        CommandSpec::new("sh").arg("-c").arg(program)
    }

    #[cfg(windows)]
    {
        let program = format!(
            ". '{path}' {args} | Out-Null; \
             Get-ChildItem Env: | ForEach-Object {{ \
                 [Console]::Out.Write($_.Name + '=' + $_.Value + [char]0) }}"
        );
        CommandSpec::new("powershell")
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-Command")
            .arg(program)
    }
}

/// Run one pre-activate script and fold its environment changes into `base`.
///
/// A script cannot alter its parent's environment directly, so it is sourced
/// and the result dumped; the dump replaces `base` wholesale, which is what
/// makes both additions and **removals** take effect — a variable the script
/// `unset`s is genuinely absent from the dump.
///
/// A disabled script is not run. A script exiting non-zero aborts with its
/// output attached rather than applying partially.
pub fn apply(
    script: &Script,
    runner: &dyn CommandRunner,
    base: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, EnvError> {
    if !script.enabled {
        return Ok(base.clone());
    }

    let mut spec = dump_command(script);
    spec.env = base.clone();
    spec.clear_env = true;

    let outcome = runner.run(&spec).map_err(|source| EnvError::ScriptSpawn {
        script: script.path.clone(),
        message: source.to_string(),
    })?;

    if !outcome.success() {
        return Err(EnvError::ScriptFailed {
            script: script.path.clone(),
            status: outcome.status,
            output: outcome.stderr,
        });
    }

    Ok(parse_dump(outcome.stdout.as_bytes()))
}
```

Add to `crates/bombadil-core/src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("could not run the pre-activate script {script}: {message}")]
    ScriptSpawn {
        script: std::path::PathBuf,
        message: String,
    },

    #[error("the pre-activate script {script} failed (exit {status:?}): {output}")]
    ScriptFailed {
        script: std::path::PathBuf,
        status: Option<i32>,
        output: String,
    },
}
```

Add `pub mod script;` and `pub use script::{apply, dump_command};` to `crates/bombadil-core/src/envspec/mod.rs`.

- [ ] **Step 4: Verify and commit**

```bash
cargo test -p bombadil-core --all-features
git add crates/bombadil-core/src/envspec crates/bombadil-core/src/error.rs
git commit -m "feat(core): capture what a pre-activate script exports

A subprocess cannot change its parent's environment, so the script is
sourced and the resulting environment dumped; the dump replaces the input
wholesale, which is what makes removals work too -- a variable the script
unsets is genuinely absent from the dump, verified against a real shell.

A failing script aborts with its output attached: partial application would
leave a half-built environment that is worse than none.

The POSIX form is verified. The PowerShell form is not testable here and is
proven by the Windows CI job."
```

---

### Task 5: Private index injection

**Files:**
- Create: `crates/bombadil-core/src/envspec/index.rs`
- Modify: `crates/bombadil-core/src/envspec/mod.rs`

**Interfaces:**
- Consumes: `model::{Index, IndexKind, IndexAuth}` (plan 1); `SecretStore`, `SecretKey` (Task 1); `validate::index_credential_var` (spec §5a work).
- Produces: `envspec::index::inject(indexes: &[&Index], store: &dyn SecretStore, into: &mut BTreeMap<String, String>) -> Result<(), SecretError>`

> **Verified against uv 0.12.1 with a local HTTP server capturing headers:** a named index and its credentials work purely from environment variables, with a `pyproject.toml` that mentions no index at all, and uv sends Basic auth **preemptively** — no 401 round-trip.
>
> `UV_INDEX_<NAME>_USERNAME` uppercases the index name and maps `-` to `_`. **Reuse `validate::index_credential_var`** rather than reimplementing it: two copies of that rule is exactly how the collision check drifts away from the thing it protects.
>
> Token auth is a username of `__token__` with the token as the password, the convention PyPI-compatible registries use.

- [ ] **Step 1: Write the failing tests**

Create `crates/bombadil-core/src/envspec/index.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Index, IndexAuth, IndexKind};
    use crate::secrets::InMemorySecretStore;

    fn index(name: &str, kind: IndexKind, auth: IndexAuth) -> Index {
        Index {
            name: name.into(),
            url: format!("https://{name}.example/simple"),
            kind,
            default_for_new_projects: false,
            auth,
        }
    }

    #[test]
    fn a_default_index_becomes_uv_default_index() {
        let idx = index("priv", IndexKind::Default, IndexAuth::None);
        let store = InMemorySecretStore::new();
        let mut env = BTreeMap::new();

        inject(&[&idx], &store, &mut env).unwrap();

        assert_eq!(
            env.get("UV_DEFAULT_INDEX").map(String::as_str),
            Some("priv=https://priv.example/simple")
        );
    }

    #[test]
    fn extra_indexes_are_space_separated_in_uv_index() {
        let a = index("one", IndexKind::Extra, IndexAuth::None);
        let b = index("two", IndexKind::Extra, IndexAuth::None);
        let store = InMemorySecretStore::new();
        let mut env = BTreeMap::new();

        inject(&[&a, &b], &store, &mut env).unwrap();

        assert_eq!(
            env.get("UV_INDEX").map(String::as_str),
            Some("one=https://one.example/simple two=https://two.example/simple")
        );
        assert!(!env.contains_key("UV_DEFAULT_INDEX"));
    }

    #[test]
    fn basic_auth_credentials_come_from_the_keychain() {
        let idx = index(
            "priv",
            IndexKind::Default,
            IndexAuth::Basic { username: "alice".into() },
        );
        let store = InMemorySecretStore::new();
        store
            .set(&SecretKey::Index { name: "priv".into() }, "hunter2")
            .unwrap();
        let mut env = BTreeMap::new();

        inject(&[&idx], &store, &mut env).unwrap();

        assert_eq!(env.get("UV_INDEX_PRIV_USERNAME").map(String::as_str), Some("alice"));
        assert_eq!(env.get("UV_INDEX_PRIV_PASSWORD").map(String::as_str), Some("hunter2"));
    }

    #[test]
    fn a_hyphenated_index_name_maps_to_underscores() {
        // uv uppercases the name AND maps '-' to '_'. Verified against uv
        // 0.12.1 with a header-capturing server.
        let idx = index(
            "my-index",
            IndexKind::Extra,
            IndexAuth::Basic { username: "alice".into() },
        );
        let store = InMemorySecretStore::new();
        store
            .set(&SecretKey::Index { name: "my-index".into() }, "hunter2")
            .unwrap();
        let mut env = BTreeMap::new();

        inject(&[&idx], &store, &mut env).unwrap();

        assert_eq!(env.get("UV_INDEX_MY_INDEX_USERNAME").map(String::as_str), Some("alice"));
    }

    #[test]
    fn token_auth_uses_the_conventional_token_username() {
        let idx = index("priv", IndexKind::Extra, IndexAuth::Token);
        let store = InMemorySecretStore::new();
        store
            .set(&SecretKey::Index { name: "priv".into() }, "tok-123")
            .unwrap();
        let mut env = BTreeMap::new();

        inject(&[&idx], &store, &mut env).unwrap();

        assert_eq!(env.get("UV_INDEX_PRIV_USERNAME").map(String::as_str), Some("__token__"));
        assert_eq!(env.get("UV_INDEX_PRIV_PASSWORD").map(String::as_str), Some("tok-123"));
    }

    #[test]
    fn an_index_with_no_stored_secret_injects_no_credentials() {
        // The user has not filled it in yet. Injecting an empty password would
        // make uv attempt an auth that cannot work, producing a worse error
        // than attempting none.
        let idx = index(
            "priv",
            IndexKind::Extra,
            IndexAuth::Basic { username: "alice".into() },
        );
        let store = InMemorySecretStore::new();
        let mut env = BTreeMap::new();

        inject(&[&idx], &store, &mut env).unwrap();

        assert!(!env.contains_key("UV_INDEX_PRIV_PASSWORD"), "got {env:?}");
    }

    #[test]
    fn an_index_with_no_auth_injects_no_credentials() {
        let idx = index("pub", IndexKind::Extra, IndexAuth::None);
        let store = InMemorySecretStore::new();
        let mut env = BTreeMap::new();

        inject(&[&idx], &store, &mut env).unwrap();

        assert!(!env.contains_key("UV_INDEX_PUB_USERNAME"), "got {env:?}");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p bombadil-core envspec::index`
Expected: FAIL — `cannot find function inject`.

- [ ] **Step 3: Implement**

Above the test module:

```rust
use crate::error::SecretError;
use crate::model::{Index, IndexAuth, IndexKind};
use crate::secrets::{SecretKey, SecretStore};
use crate::validate::index_credential_var;
use std::collections::BTreeMap;

/// The username uv expects for token authentication, the convention
/// PyPI-compatible registries use.
const TOKEN_USERNAME: &str = "__token__";

/// Inject index configuration and credentials as environment variables.
///
/// Verified against uv 0.12.1: a named index and its credentials work purely
/// from the environment, with a `pyproject.toml` that mentions no index at
/// all, and uv sends Basic auth preemptively.
///
/// The variable name comes from [`index_credential_var`] rather than being
/// rebuilt here: two copies of that rule is how the collision check in
/// `validate` drifts away from the thing it is protecting.
pub fn inject(
    indexes: &[&Index],
    store: &dyn SecretStore,
    into: &mut BTreeMap<String, String>,
) -> Result<(), SecretError> {
    let mut extras = Vec::new();

    for index in indexes {
        let pair = format!("{}={}", index.name, index.url);
        match index.kind {
            IndexKind::Default => {
                into.insert("UV_DEFAULT_INDEX".to_string(), pair);
            }
            IndexKind::Extra => extras.push(pair),
        }

        let username = match &index.auth {
            IndexAuth::None => continue,
            IndexAuth::Basic { username } => username.clone(),
            IndexAuth::Token => TOKEN_USERNAME.to_string(),
        };

        // No stored secret means the user has not filled it in. Injecting an
        // empty password would make uv attempt an auth that cannot succeed,
        // which reports worse than attempting none.
        let Some(secret) = store.get(&SecretKey::Index {
            name: index.name.clone(),
        })?
        else {
            continue;
        };

        into.insert(index_credential_var(&index.name, "USERNAME"), username);
        into.insert(index_credential_var(&index.name, "PASSWORD"), secret);
    }

    if !extras.is_empty() {
        into.insert("UV_INDEX".to_string(), extras.join(" "));
    }

    Ok(())
}
```

Add `pub mod index;` and `pub use index::inject;` to `crates/bombadil-core/src/envspec/mod.rs`.

- [ ] **Step 4: Verify and commit**

```bash
cargo test -p bombadil-core --all-features
git add crates/bombadil-core/src/envspec
git commit -m "feat(core): inject private index configuration and credentials

Verified against uv 0.12.1 with a header-capturing server: named indexes
and their credentials work purely from the environment, with a
pyproject.toml that mentions no index, and uv sends Basic auth
preemptively. No project file is touched.

Variable names come from validate::index_credential_var rather than being
rebuilt here -- two copies of that rule is how the collision check drifts
away from the thing it protects.

An index with no stored secret injects no credentials: an empty password
would make uv attempt an auth that cannot succeed."
```

---

### Task 6: Composing the whole environment

**Files:**
- Modify: `crates/bombadil-core/src/envspec/mod.rs`

**Interfaces:**
- Consumes: everything above, plus `model::{Config, Project, EnvVar}` and `venv_path::resolve`.
- Produces:
  - `envspec::EnvInputs { parent: BTreeMap<String, String>, config: Config, project: Project, venv_path: PathBuf }` — owned, not borrowed: the GUI builds one per launch and borrowing would tie it to a config lock held across a subprocess spawn
  - `envspec::compose(inputs, store, runner) -> Result<BTreeMap<String, String>, EnvError>`

> **The layer order is spec §6 and is load-bearing.** Each layer overrides the previous:
>
> 1. the parent process environment
> 2. global environment variables
> 3. project environment variables
> 4. secret values fetched from the keychain for variables marked secret
> 5. index configuration and credentials
> 6. `UV_PROJECT_ENVIRONMENT` set to the resolved venv path
> 7. pre-activate scripts, in order, their exports folded in
> 8. `VIRTUAL_ENV` set, venv `bin`/`Scripts` prepended to `PATH`, `PYTHONHOME` removed
>
> Scripts run at 7, **after** the variables they might depend on and **before** activation, so a script can see the project's configuration but cannot accidentally undo activation.
>
> Activation is last and direct — **verified**: with only `VIRTUAL_ENV` set and the venv's `bin` prepended to `PATH`, `python` resolved to the venv interpreter and imported from its `site-packages`. No `source activate` anywhere, so it works identically in bash, zsh, fish, PowerShell and cmd.
>
> `PYTHONHOME` is removed because a stale value silently redirects the interpreter's stdlib search and produces failures that look nothing like their cause.

- [ ] **Step 1: Write the failing tests**

Append to `crates/bombadil-core/src/envspec/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Config, EnvVar, Project, Script, VenvLocation};
    use crate::process::{CommandOutcome, FakeCommandRunner};
    use crate::secrets::InMemorySecretStore;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn project() -> Project {
        Project {
            id: Uuid::from_u128(7),
            label: "my-api".into(),
            pyproject_path: PathBuf::from("/proj/my-api/pyproject.toml"),
            index_names: vec![],
            venv: VenvLocation::Alongside,
            env: vec![],
            scripts: vec![],
            members: Default::default(),
        }
    }

    fn inputs(config: Config, project: Project) -> EnvInputs {
        EnvInputs {
            parent: BTreeMap::from([
                ("PATH".to_string(), "/usr/bin".to_string()),
                ("HOME".to_string(), "/home/t".to_string()),
                ("PYTHONHOME".to_string(), "/stale".to_string()),
            ]),
            config,
            project,
            venv_path: PathBuf::from("/envs/my-api"),
        }
    }

    fn compose_with(inputs: EnvInputs) -> BTreeMap<String, String> {
        let store = InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();
        compose(&inputs, &store, &runner).unwrap()
    }

    #[test]
    fn the_parent_environment_is_inherited() {
        let got = compose_with(inputs(Config::default(), project()));
        assert_eq!(got.get("HOME").map(String::as_str), Some("/home/t"));
    }

    #[test]
    fn a_project_variable_overrides_a_global_one() {
        let config = Config {
            global_env: vec![EnvVar { key: "SHARED".into(), value: "global".into(), secret: false }],
            ..Config::default()
        };
        let project = Project {
            env: vec![EnvVar { key: "SHARED".into(), value: "project".into(), secret: false }],
            ..project()
        };

        let got = compose_with(inputs(config, project));

        assert_eq!(got.get("SHARED").map(String::as_str), Some("project"));
    }

    #[test]
    fn a_secret_variable_takes_its_value_from_the_keychain() {
        let project = Project {
            env: vec![EnvVar { key: "TOKEN".into(), value: String::new(), secret: true }],
            ..project()
        };
        let store = InMemorySecretStore::new();
        store
            .set(
                &crate::secrets::SecretKey::ProjectEnv {
                    project_id: Uuid::from_u128(7),
                    key: "TOKEN".into(),
                },
                "from-keychain",
            )
            .unwrap();
        let runner = FakeCommandRunner::new();

        let got = compose(&inputs(Config::default(), project), &store, &runner).unwrap();

        assert_eq!(got.get("TOKEN").map(String::as_str), Some("from-keychain"));
    }

    #[test]
    fn a_secret_with_nothing_stored_is_omitted_rather_than_empty() {
        // An empty value looks set to the child process and fails in a way
        // that points nowhere near the real cause.
        let project = Project {
            env: vec![EnvVar { key: "TOKEN".into(), value: String::new(), secret: true }],
            ..project()
        };

        let got = compose_with(inputs(Config::default(), project));

        assert!(!got.contains_key("TOKEN"), "got {got:?}");
    }

    #[test]
    fn activation_sets_virtual_env_and_prepends_path() {
        // Verified against a real venv: VIRTUAL_ENV plus a prepended PATH is
        // exactly what `source activate` does, and works in every shell.
        let got = compose_with(inputs(Config::default(), project()));

        assert_eq!(got.get("VIRTUAL_ENV").map(String::as_str), Some("/envs/my-api"));
        let path = got.get("PATH").expect("PATH");
        assert!(path.starts_with("/envs/my-api"), "got {path}");
        assert!(path.contains("/usr/bin"), "the inherited PATH must survive; got {path}");
    }

    #[test]
    fn pythonhome_is_removed() {
        // A stale PYTHONHOME silently redirects the stdlib search and fails in
        // a way that looks nothing like its cause.
        let got = compose_with(inputs(Config::default(), project()));
        assert!(!got.contains_key("PYTHONHOME"), "got {got:?}");
    }

    #[test]
    fn uv_project_environment_points_at_the_venv() {
        let got = compose_with(inputs(Config::default(), project()));
        assert_eq!(
            got.get("UV_PROJECT_ENVIRONMENT").map(String::as_str),
            Some("/envs/my-api")
        );
    }

    #[test]
    fn a_script_runs_after_the_variables_and_before_activation() {
        // Order matters: the script must see the project's configuration, and
        // must not be able to undo activation.
        let project = Project {
            env: vec![EnvVar { key: "SEEN_BY_SCRIPT".into(), value: "yes".into(), secret: false }],
            scripts: vec![Script {
                path: PathBuf::from("/p/pre.sh"),
                args: vec![],
                enabled: true,
            }],
            ..project()
        };

        let runner = FakeCommandRunner::new();
        // The script echoes back what it saw plus its own export, and tries to
        // clobber VIRTUAL_ENV — which activation must overwrite afterwards.
        runner.expect(
            "sh",
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: "SEEN_BY_SCRIPT=yes\0FROM_SCRIPT=set\0VIRTUAL_ENV=/wrong\0PATH=/usr/bin\0"
                    .to_string(),
                stderr: String::new(),
            },
        );
        let store = InMemorySecretStore::new();

        let got = compose(&inputs(Config::default(), project), &store, &runner).unwrap();

        assert_eq!(got.get("FROM_SCRIPT").map(String::as_str), Some("set"));
        assert_eq!(
            got.get("VIRTUAL_ENV").map(String::as_str),
            Some("/envs/my-api"),
            "activation runs after scripts and must win"
        );
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p bombadil-core envspec`
Expected: FAIL — `cannot find type EnvInputs`.

- [ ] **Step 3: Implement**

Add to `crates/bombadil-core/src/envspec/mod.rs`, above the tests:

```rust
mod dump;
mod index;
mod script;

pub use dump::parse_dump;
pub use index::inject;
pub use script::{apply, dump_command};

use crate::error::EnvError;
use crate::model::{Config, EnvVar, Project};
use crate::process::CommandRunner;
use crate::secrets::{SecretKey, SecretStore};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Everything needed to compose a child process's environment.
pub struct EnvInputs {
    /// The environment Bombadil itself is running in.
    pub parent: BTreeMap<String, String>,
    pub config: Config,
    pub project: Project,
    /// Already resolved by `venv_path::resolve`.
    pub venv_path: PathBuf,
}

/// The directory holding a venv's executables, which differs by platform.
fn venv_bin_dir(venv: &std::path::Path) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts")
    } else {
        venv.join("bin")
    }
}

fn resolve_var(
    var: &EnvVar,
    key: SecretKey,
    store: &dyn SecretStore,
    into: &mut BTreeMap<String, String>,
) -> Result<(), EnvError> {
    if !var.secret {
        into.insert(var.key.clone(), var.value.clone());
        return Ok(());
    }

    // A secret with nothing stored is omitted rather than set empty: an empty
    // value looks set to the child and fails somewhere far from the cause.
    let stored = store
        .get(&key)
        .map_err(|source| EnvError::Secret { message: source.to_string() })?;
    if let Some(value) = stored {
        into.insert(var.key.clone(), value);
    }
    Ok(())
}

/// Compose the environment a child process runs in.
///
/// Layers, each overriding the previous, per spec section 6:
/// parent environment; global variables; project variables; secrets;
/// index configuration; `UV_PROJECT_ENVIRONMENT`; pre-activate scripts;
/// activation.
///
/// Scripts run second to last so they can see the project's configuration but
/// cannot undo activation, which is applied afterwards and wins.
pub fn compose(
    inputs: &EnvInputs,
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
) -> Result<BTreeMap<String, String>, EnvError> {
    // 1. parent
    let mut env = inputs.parent.clone();

    // 2. global variables
    for var in &inputs.config.global_env {
        resolve_var(
            var,
            SecretKey::GlobalEnv { key: var.key.clone() },
            store,
            &mut env,
        )?;
    }

    // 3 and 4. project variables, secrets resolved from the keychain
    for var in &inputs.project.env {
        resolve_var(
            var,
            SecretKey::ProjectEnv {
                project_id: inputs.project.id,
                key: var.key.clone(),
            },
            store,
            &mut env,
        )?;
    }

    // 5. indexes this project selected
    let selected: Vec<&crate::model::Index> = inputs
        .config
        .indexes
        .iter()
        .filter(|i| inputs.project.index_names.contains(&i.name))
        .collect();
    inject(&selected, store, &mut env)
        .map_err(|source| EnvError::Secret { message: source.to_string() })?;

    // 6. point uv at this project's venv
    env.insert(
        "UV_PROJECT_ENVIRONMENT".to_string(),
        inputs.venv_path.to_string_lossy().into_owned(),
    );

    // 7. pre-activate scripts, in order
    for script in inputs.config.global_scripts.iter().chain(&inputs.project.scripts) {
        env = apply(script, runner, &env)?;
    }

    // 8. activation, last so a script cannot undo it
    let bin = venv_bin_dir(&inputs.venv_path);
    let bin = bin.to_string_lossy().into_owned();
    let path = match env.get("PATH") {
        Some(existing) if !existing.is_empty() => {
            format!("{bin}{}{existing}", if cfg!(windows) { ";" } else { ":" })
        }
        _ => bin.clone(),
    };
    env.insert("PATH".to_string(), path);
    env.insert(
        "VIRTUAL_ENV".to_string(),
        inputs.venv_path.to_string_lossy().into_owned(),
    );
    // A stale PYTHONHOME silently redirects the stdlib search.
    env.remove("PYTHONHOME");

    Ok(env)
}
```

Add to `EnvError` in `crates/bombadil-core/src/error.rs`:

```rust
    #[error("could not read a secret while composing the environment: {message}")]
    Secret { message: String },
```

- [ ] **Step 4: Verify and commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --workspace --locked
git add crates/bombadil-core/src/envspec crates/bombadil-core/src/error.rs
git commit -m "feat(core): compose the environment every child process runs in

Eight ordered layers per spec section 6. Scripts run second to last so they
see the project's configuration but cannot undo activation, which is
applied afterwards and wins -- there is a test where a script tries to
clobber VIRTUAL_ENV and loses.

Activation is direct: VIRTUAL_ENV plus a prepended PATH, verified against a
real venv to resolve python and its site-packages correctly. No source
activate, so it behaves identically in every shell.

A secret with nothing stored is omitted rather than set empty: an empty
value looks set to the child and fails far from its cause."
```

---

### Task 7: Proving the composed environment against a real uv

**Files:**
- Modify: `crates/bombadil-core/tests/uv_integration.rs`

**Interfaces:** no library API.

> Everything above is hermetic. This proves three things nothing else can: that the **PowerShell** script form works on Windows, that a composed environment actually **activates** a real venv, and that a real `keyring` round-trip works where a keychain exists.
>
> **This test runs on all three platforms and is the only proof the Windows script path works.** A red Windows job here means the PowerShell command is wrong — which is the point. Do not weaken or skip it to get green.

- [ ] **Step 1: Write the test**

Append to `crates/bombadil-core/tests/uv_integration.rs`:

```rust
#[test]
fn a_composed_environment_activates_a_real_venv() {
    let runner = SystemCommandRunner;
    let uv = resolve_uv(&runner);

    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("pyproject.toml"),
        "[project]\nname = \"envtest\"\nversion = \"0.1.0\"\n\
         requires-python = \">=3.10\"\ndependencies = [\"iniconfig\"]\n",
    )
    .unwrap();

    let venv_path = tmp.path().join("env");
    let ctx = ProjectContext {
        project_dir: project_dir.clone(),
        venv_path: venv_path.clone(),
    };

    let outcome = runner.run(&command::sync(&uv, &ctx)).unwrap();
    assert!(outcome.success(), "sync failed: {}", outcome.stderr);

    let project = bombadil_core::model::Project {
        id: uuid::Uuid::from_u128(1),
        label: "envtest".into(),
        pyproject_path: project_dir.join("pyproject.toml"),
        index_names: vec![],
        venv: bombadil_core::model::VenvLocation::Custom { path: venv_path.clone() },
        env: vec![],
        scripts: vec![],
        members: Default::default(),
    };

    let inputs = bombadil_core::envspec::EnvInputs {
        parent: std::env::vars().collect(),
        config: bombadil_core::model::Config::default(),
        project,
        venv_path: venv_path.clone(),
    };

    let store = bombadil_core::secrets::InMemorySecretStore::new();
    let env = bombadil_core::envspec::compose(&inputs, &store, &runner)
        .expect("composing an environment must succeed");

    // The proof: run python through the composed environment and confirm it is
    // the venv's interpreter, importing from the venv's site-packages — not
    // whatever Python happens to be ambient on this machine.
    let mut spec = bombadil_core::process::CommandSpec::new("python");
    spec.args = vec![
        "-c".into(),
        "import sys, iniconfig; print(sys.executable); print(iniconfig.__file__)".into(),
    ];
    spec.env = env.clone();
    spec.clear_env = true;

    let outcome = runner.run(&spec).unwrap();
    assert!(outcome.success(), "python failed: {}", outcome.stderr);

    let venv_str = venv_path.to_string_lossy().to_string();
    for line in outcome.stdout.lines() {
        assert!(
            line.starts_with(&venv_str),
            "the composed environment must activate the venv; got {line}"
        );
    }
}

#[test]
fn a_pre_activate_script_can_export_into_the_composed_environment() {
    // The only proof the PowerShell dump form works on Windows. A red Windows
    // job here means that command is wrong -- which is exactly what this test
    // is for.
    let runner = SystemCommandRunner;
    let tmp = tempfile::tempdir().unwrap();

    let (script_path, body) = if cfg!(windows) {
        (tmp.path().join("pre.ps1"), "$env:FROM_SCRIPT = 'exported'\n")
    } else {
        (tmp.path().join("pre.sh"), "export FROM_SCRIPT=exported\n")
    };
    std::fs::write(&script_path, body).unwrap();

    let script = bombadil_core::model::Script {
        path: script_path,
        args: vec![],
        enabled: true,
    };

    let base: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let got = bombadil_core::envspec::apply(&script, &runner, &base)
        .expect("a pre-activate script must run and its exports be captured");

    assert_eq!(
        got.get("FROM_SCRIPT").map(String::as_str),
        Some("exported"),
        "the script's export was not captured"
    );
}

#[test]
fn the_real_keychain_round_trips_where_one_exists() {
    use bombadil_core::secrets::{KeyringSecretStore, SecretKey, SecretStore};

    let store = KeyringSecretStore::new();
    if !store.is_available() {
        eprintln!("no keychain on this machine; skipping");
        return;
    }

    // Namespaced so it cannot collide with a real secret, and deleted below.
    let key = SecretKey::Index { name: "bombadil-integration-throwaway".into() };
    store.set(&key, "hunter2").expect("set");
    assert_eq!(store.get(&key).unwrap().as_deref(), Some("hunter2"));
    store.delete(&key).expect("delete");
    assert_eq!(store.get(&key).unwrap(), None, "the secret must be gone");
}
```

> Adapt `resolve_uv`, `ProjectContext` and the import style to whatever the file already uses. If `Project`'s fields have changed, follow the struct rather than this snippet.

- [ ] **Step 2: Run it**

```bash
cargo test -p bombadil-core --features uv-integration --test uv_integration
```

Needs uv ≥ 0.12.1 on `PATH` and network.

- [ ] **Step 3: Full verification and commit**

```bash
cargo fmt --all --check \
  && cargo clippy --all-targets --all-features --locked -- -D warnings \
  && cargo test --workspace --locked \
  && cargo deny check
git add crates/bombadil-core/tests/uv_integration.rs
git commit -m "test(core): prove the composed environment against real tools

Three things no hermetic test can show: that a composed environment
actually activates a real venv -- python and its site-packages both resolve
inside it, not to whatever is ambient -- that a pre-activate script's
exports are captured, and that a real keychain round-trips where one exists.

The script test is the only proof the PowerShell dump form works. It runs
on all three platforms deliberately; a red Windows job means that command
is wrong, which is what it is for."
```

---

## Done when

- `cargo test --workspace --locked` passes on Linux, macOS and Windows.
- `cargo clippy --all-targets --all-features --locked -- -D warnings` and `cargo deny check` are clean.
- The `msrv` job at 1.88.0 passes.
- `uv-integration` is green on all three platforms — **including the Windows script test**.
- No unit test touches the real keychain, spawns a process, or reaches the network.

## What the remaining plans pick up

**Plan 2c (terminals)** consumes `compose` directly: detect the emulators present, build the spawn argv, and launch one with the composed environment and the project directory as its working directory. The environment side is finished here.

**Plan 3 (the GUI)** must additionally handle the case this plan deliberately leaves to it: when `KeyringSecretStore::is_available()` is false — common on headless Linux and WSL — the app says so plainly and offers session-only entry backed by `InMemorySecretStore`, rather than failing later at spawn time with something cryptic. The seam for that is already here; the choice is a UI one.
