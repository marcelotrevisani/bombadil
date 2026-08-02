# Bombadil Monorepo Members Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a registered project contain many packages — detect them, let the user choose which are installed in editable "dev" mode (all chosen by default), and let arbitrary projects be added to that list by path.

**Architecture:** Discovery walks the registered project's tree for `pyproject.toml` files and classifies each against the root's `[tool.uv.workspace]` members, if any. Selection is expressed at sync time through `--all-packages` / `--package`, never by rewriting a shared manifest. Projects outside the tree are wired in with `uv add --editable`, preferring a relative path so the manifest stays portable.

**Tech Stack:** Rust (edition 2024), building on `bombadil-core`'s existing `pyproject`, `model`, `store`, `process` and `uv` modules. No new dependencies.

## Scope

This plan implements **spec §5a** and nothing else. It is headless: no GUI. The member checklist UI belongs to plan 3.

Plans 1 and 2a are merged on `main`. Plans 2b (secrets, environment pipeline) and 2c (terminals) are independent of this one and can be built in either order.

## Verification already done

Everything below was checked against a real uv 0.12.1 on 2026-08-02, not taken from documentation. Do not re-litigate these; do not "improve" code that depends on them.

- **Plain `uv sync` in a workspace root installs NO members** — the environment came back empty. "All members installed" is *not* uv's default; it is specifically `--all-packages`. This is the single most surprising fact in this plan.
- `uv sync --package <name>` installs only that member, editable. Multiple `--package` flags compose.
- **Re-syncing with a smaller selection removes the others.** After `--all-packages` installed `alpha, api, beta`, a plain `--package alpha` left only `alpha`. Deselection therefore needs no manifest edit and no `uv remove` — it is just the next sync.
- `uv pip list --format json` reports **`editable_project_location`** for editable installs, which is how the UI can tell which members are currently dev-installed.
- `uv add --editable <path>` writes `dependencies = ["<name>"]` plus `[tool.uv.sources] <name> = { path = "<path>", editable = true }`.
- **uv preserves the path form it is given**: a relative path in produces `path = "../outside"`; an absolute path in produces an absolute path in the manifest. Since `pyproject.toml` is shared with teammates, Bombadil must prefer the relative form.
- `uv pip` commands do **not** honour `UV_PROJECT_ENVIRONMENT` — `pip_list` already passes `--python <venv>` for this reason (learned the hard way in plan 2a). Any new `uv pip` command must do the same.

## Global Constraints

Every task's requirements implicitly include this section.

- **Rust edition 2024, `rust-version = "1.88"`.**
- **No new crate dependencies.** In particular, do not add a glob crate — the minimal matching this plan needs is written by hand in Task 2 and is table-tested.
- `bombadil-core` must not depend on `iced` or on `bombadil-uv-bin`.
- **Never write `pyproject.toml` or `uv.lock` directly.** Every write goes through uv. `uv add --editable` is uv writing, which is allowed; hand-editing `[tool.uv.sources]` is not.
- **Never edit `[tool.uv.workspace] members`.** That list describes the repository and is shared; a user's local selection must not be pushed into it. Selection lives in Bombadil's own config and is expressed at sync time. This is spec §5a and is not negotiable.
- **No process may be spawned outside `CommandRunner`.** Unit tests must not spawn anything or touch the network; only `tests/uv_integration.rs` may.
- **Discovery never installs anything.** It returns candidates for confirmation.
- Config schema stays at `CURRENT_SCHEMA_VERSION = 1`: every new field is `#[serde(default)]`, so no migration is needed.
- Two clippy lints are DENIED in CI: `derivable_impls` (use `#[derive(Default)]` with `#[default]`) and `field_reassign_with_default` (use struct-update syntax).
- CI must pass `cargo fmt --all --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, `cargo test --workspace --locked`, `cargo deny check`, the `msrv` job at 1.88.0, and `uv-integration` on all three platforms.

---

## File Structure

```
crates/bombadil-core/src/
    model/
        member.rs                     # MemberRef, MemberOrigin, MemberSelection
        project.rs                    # + members field  (modified)
        mod.rs                        # + re-exports     (modified)
    members/
        mod.rs                        # re-exports
        workspace.rs                  # [tool.uv.workspace] parsing + glob matching
        discover.rs                   # walk the tree for pyproject.toml
        relative.rs                   # prefer-relative path computation
    uv/
        command.rs                    # + member sync and editable builders (modified)
        results.rs                    # + editable_project_location         (modified)
    lib.rs                            # + pub mod members;                  (modified)
crates/bombadil-core/tests/
    uv_integration.rs                 # + workspace selection test          (modified)
```

`members/` owns "what packages does this project contain and which are selected"; `uv/` keeps owning "what do we run". Each file has one responsibility.

---

### Task 1: The member model and its persistence

**Files:**
- Create: `crates/bombadil-core/src/model/member.rs`
- Modify: `crates/bombadil-core/src/model/project.rs`
- Modify: `crates/bombadil-core/src/model/mod.rs`
- Modify: `crates/bombadil-core/tests/fixtures/full.toml`

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `model::MemberOrigin` — enum `Discovered`, `Added`
  - `model::MemberRef { path: PathBuf, origin: MemberOrigin, selected: bool }`
  - `model::MemberSelection { all_discovered: bool, entries: Vec<MemberRef> }`, deriving `Default`
  - `Project.members: MemberSelection`, `#[serde(default)]`

> **Why `all_discovered` is a flag rather than "tick everything on discovery":** spec §5a says all discovered members are selected by default. If that were stored as an explicit list, a member added to the repo *later* would arrive unselected, and the user would have to notice and tick it. The flag means "whatever discovery finds is selected", so new packages are picked up automatically. `entries` then holds only the deviations — explicit deselections and externally added projects.
>
> `MemberRef.path` is stored **relative to the project directory** when the member is inside the tree, and absolute when it is outside. That keeps a moved-but-intact checkout working.

- [ ] **Step 1: Write the failing test**

Create `crates/bombadil-core/src/model/member.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_fresh_selection_takes_everything_discovery_finds() {
        // Spec 5a: all discovered members are selected by default, and a
        // package added to the repo later must be picked up without the user
        // having to notice it.
        let selection = MemberSelection::default();
        assert!(selection.all_discovered);
        assert!(selection.entries.is_empty());
    }

    #[test]
    fn a_selection_round_trips_through_toml() {
        let selection = MemberSelection {
            all_discovered: false,
            entries: vec![
                MemberRef {
                    path: PathBuf::from("packages/alpha"),
                    origin: MemberOrigin::Discovered,
                    selected: true,
                },
                MemberRef {
                    path: PathBuf::from("/work/shared-lib"),
                    origin: MemberOrigin::Added,
                    selected: false,
                },
            ],
        };

        let text = toml::to_string_pretty(&selection).expect("serialize");
        let back: MemberSelection = toml::from_str(&text).expect("deserialize");

        assert_eq!(back, selection);
    }

    #[test]
    fn an_absent_members_table_deserializes_to_the_default() {
        // Config files written before this feature existed must keep loading,
        // which is why no schema bump is needed.
        let back: MemberSelection = toml::from_str("").expect("deserialize");
        assert_eq!(back, MemberSelection::default());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p bombadil-core model::member`
Expected: FAIL — `cannot find type MemberSelection`.

- [ ] **Step 3: Write the implementation**

Above the test module in `crates/bombadil-core/src/model/member.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How a member came to be in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberOrigin {
    /// Found by walking the project's own tree.
    Discovered,
    /// Named explicitly by the user, possibly outside the tree.
    Added,
}

/// One package that may be installed in editable mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRef {
    /// Relative to the project directory when the member is inside the tree,
    /// absolute when it is outside. Relative keeps a moved checkout working.
    pub path: PathBuf,
    pub origin: MemberOrigin,
    pub selected: bool,
}

/// Which of a project's packages are installed in editable mode.
///
/// `all_discovered` is a flag rather than an expanded list on purpose. Spec 5a
/// selects every discovered member by default; storing that as a list would
/// mean a package added to the repository later arrived unselected, and the
/// user would have to spot it. As a flag it means "whatever discovery finds",
/// so new packages are picked up on their own. `entries` therefore holds only
/// the deviations: explicit deselections, and projects added by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberSelection {
    #[serde(default = "default_true")]
    pub all_discovered: bool,
    #[serde(default)]
    pub entries: Vec<MemberRef>,
}

fn default_true() -> bool {
    true
}

impl Default for MemberSelection {
    fn default() -> Self {
        Self {
            all_discovered: true,
            entries: Vec::new(),
        }
    }
}
```

> The hand-written `Default` is required, not stylistic: `#[derive(Default)]` would make `all_discovered` false, which inverts the spec'd behaviour. clippy's `derivable_impls` will not fire, because the derived impl would differ.

In `crates/bombadil-core/src/model/project.rs`, add the field **after** `scripts` (TOML wants tables last, and `MemberSelection` serialises as a table):

```rust
    #[serde(default)]
    pub members: MemberSelection,
```

and `use super::MemberSelection;` at the top.

In `crates/bombadil-core/src/model/mod.rs` add `mod member;` and `pub use member::{MemberOrigin, MemberRef, MemberSelection};`.

- [ ] **Step 4: Update the golden fixture**

`crates/bombadil-core/tests/fixtures/full.toml` is hand-authored and asserted field by field, so it must gain the new table or its test will fail. Append to the existing `[[projects]]` entry:

```toml
[projects.members]
all_discovered = false

[[projects.members.entries]]
path = "packages/alpha"
origin = "discovered"
selected = true

[[projects.members.entries]]
path = "/work/shared-lib"
origin = "added"
selected = false
```

Update the golden test's expected `Config` to match. **Do not** regenerate the fixture with serde — it is hand-authored precisely so a serialisation change shows up as a failure.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p bombadil-core --all-features`
Expected: PASS. Report the new count.

- [ ] **Step 6: Commit**

```bash
git add crates/bombadil-core/src/model crates/bombadil-core/tests/fixtures/full.toml
git commit -m "feat(core): model which project members are installed editable

all_discovered is a flag rather than an expanded list so a package added to
the repository later is picked up automatically instead of arriving
unselected and waiting to be noticed. entries holds only the deviations.

No schema bump: the field is serde(default), so configs written before this
feature keep loading."
```

---

### Task 2: Detecting a uv workspace and matching its members

**Files:**
- Create: `crates/bombadil-core/src/members/mod.rs`
- Create: `crates/bombadil-core/src/members/workspace.rs`
- Modify: `crates/bombadil-core/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `members::WorkspaceKind` — enum `Workspace { member_globs: Vec<String> }`, `Loose`
  - `members::parse_workspace(pyproject_text: &str) -> WorkspaceKind`
  - `members::glob_matches(pattern: &str, candidate: &str) -> bool`

> `glob_matches` handles exactly the two forms uv workspace members use in practice — a trailing `*` (one path segment) and `**` (any depth) — plus literal paths. **Write it by hand; do not add a glob dependency.** Both `pattern` and `candidate` are forward-slash-joined relative paths, so the function is platform-independent and its tests are too.

- [ ] **Step 1: Write the failing tests**

Create `crates/bombadil-core/src/members/workspace.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_root_reports_its_member_globs() {
        let text = r#"
[project]
name = "root"
version = "0.1.0"

[tool.uv.workspace]
members = ["packages/*", "services/*"]
"#;
        match parse_workspace(text) {
            WorkspaceKind::Workspace { member_globs } => {
                assert_eq!(member_globs, vec!["packages/*", "services/*"]);
            }
            other => panic!("expected Workspace, got {other:?}"),
        }
    }

    #[test]
    fn a_plain_project_is_loose() {
        let text = "[project]\nname = \"solo\"\nversion = \"0.1.0\"\n";
        assert_eq!(parse_workspace(text), WorkspaceKind::Loose);
    }

    #[test]
    fn a_workspace_table_with_no_members_key_is_still_a_workspace() {
        // uv allows an empty workspace table; it is a workspace with no globs,
        // not a loose project, and treating it as loose would make Bombadil
        // start adding editable path dependencies to a real workspace.
        let text = "[project]\nname = \"root\"\nversion = \"0.1.0\"\n\n[tool.uv.workspace]\n";
        match parse_workspace(text) {
            WorkspaceKind::Workspace { member_globs } => assert!(member_globs.is_empty()),
            other => panic!("expected Workspace, got {other:?}"),
        }
    }

    #[test]
    fn unparseable_toml_is_treated_as_loose() {
        // Discovery must never be the thing that makes the app unusable; a
        // malformed root manifest surfaces elsewhere, through pyproject::load.
        assert_eq!(parse_workspace("{{{ not toml"), WorkspaceKind::Loose);
    }

    #[test]
    fn a_single_star_matches_one_segment_only() {
        assert!(glob_matches("packages/*", "packages/alpha"));
        assert!(!glob_matches("packages/*", "packages/alpha/nested"));
        assert!(!glob_matches("packages/*", "services/alpha"));
        assert!(!glob_matches("packages/*", "packages"));
    }

    #[test]
    fn a_double_star_matches_any_depth() {
        assert!(glob_matches("packages/**", "packages/alpha"));
        assert!(glob_matches("packages/**", "packages/alpha/nested/deep"));
        assert!(!glob_matches("packages/**", "services/alpha"));
    }

    #[test]
    fn a_literal_pattern_matches_exactly() {
        assert!(glob_matches("packages/alpha", "packages/alpha"));
        assert!(!glob_matches("packages/alpha", "packages/beta"));
        assert!(!glob_matches("packages/alpha", "packages/alpha/nested"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p bombadil-core members::workspace`
Expected: FAIL — `cannot find function parse_workspace`.

- [ ] **Step 3: Write the implementation**

Above the test module:

```rust
use serde::Deserialize;

/// Whether a registered project is a uv workspace, and if so what it declares
/// as members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceKind {
    Workspace { member_globs: Vec<String> },
    Loose,
}

#[derive(Deserialize)]
struct RawWorkspace {
    #[serde(default)]
    members: Vec<String>,
}

#[derive(Deserialize)]
struct RawUv {
    workspace: Option<RawWorkspace>,
}

#[derive(Deserialize)]
struct RawTool {
    uv: Option<RawUv>,
}

#[derive(Deserialize)]
struct RawRoot {
    tool: Option<RawTool>,
}

/// Classify a root `pyproject.toml`.
///
/// Unparseable input is reported as `Loose` rather than an error: discovery
/// must never be the thing that makes the app unusable, and a malformed
/// manifest already surfaces through `pyproject::load`.
pub fn parse_workspace(pyproject_text: &str) -> WorkspaceKind {
    let raw: RawRoot = match toml::from_str(pyproject_text) {
        Ok(raw) => raw,
        Err(_) => return WorkspaceKind::Loose,
    };

    match raw.tool.and_then(|t| t.uv).and_then(|u| u.workspace) {
        Some(ws) => WorkspaceKind::Workspace {
            member_globs: ws.members,
        },
        None => WorkspaceKind::Loose,
    }
}

/// Match a uv workspace member pattern against a candidate path.
///
/// Handles the only forms uv workspaces use in practice: a literal path, a
/// trailing `*` matching exactly one segment, and `**` matching any depth.
/// Both arguments are forward-slash-joined relative paths, so this is
/// platform-independent.
pub fn glob_matches(pattern: &str, candidate: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return candidate
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1);
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return match candidate.strip_prefix(prefix) {
            Some(rest) => {
                rest.starts_with('/') && rest.len() > 1 && !rest[1..].contains('/')
            }
            None => false,
        };
    }
    pattern == candidate
}
```

Create `crates/bombadil-core/src/members/mod.rs`:

```rust
mod workspace;

pub use workspace::{WorkspaceKind, glob_matches, parse_workspace};
```

Add `pub mod members;` to `crates/bombadil-core/src/lib.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p bombadil-core --all-features`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/bombadil-core/src/members crates/bombadil-core/src/lib.rs
git commit -m "feat(core): detect uv workspaces and match member globs

An empty [tool.uv.workspace] table is a workspace with no globs, not a loose
project -- treating it as loose would make Bombadil start adding editable
path dependencies to a repository that already has a workspace.

Glob matching is hand-written for the two forms uv actually uses rather than
taking a dependency for it."
```

---

### Task 3: Discovering the packages in a project's tree

**Files:**
- Create: `crates/bombadil-core/src/members/discover.rs`
- Modify: `crates/bombadil-core/src/members/mod.rs`

**Interfaces:**
- Consumes: `WorkspaceKind`, `glob_matches` (Task 2); `pyproject::parse` (plan 1).
- Produces:
  - `members::DiscoveredMember { relative_path: PathBuf, name: Option<String>, is_workspace_member: bool }`
  - `members::discover(project_dir: &Path, kind: &WorkspaceKind) -> Vec<DiscoveredMember>`
  - `members::MAX_DISCOVERY_DEPTH: usize` (= 4)
  - `members::SKIPPED_DIRS: &[&str]`

> The root's own `pyproject.toml` is never a member of its own list.
>
> `name` is `Option` because a `pyproject.toml` need not have `[project].name` — a workspace root carrying only `[tool.uv]` is valid. A member without a name cannot be passed to `uv sync --package`, and Task 5 refuses to try.
>
> Results are sorted by path so the UI order is stable across runs; `read_dir` order is not.

- [ ] **Step 1: Write the failing tests**

Create `crates/bombadil-core/src/members/discover.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tree of `pyproject.toml` files under a temp dir.
    fn tree(paths: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, name) in paths {
            let full = dir.path().join(rel);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(
                &full,
                format!("[project]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
            )
            .unwrap();
        }
        dir
    }

    #[test]
    fn finds_nested_packages_and_ignores_the_root_itself() {
        let dir = tree(&[
            ("pyproject.toml", "root"),
            ("packages/alpha/pyproject.toml", "alpha"),
            ("services/api/pyproject.toml", "api"),
        ]);

        let found = discover(dir.path(), &WorkspaceKind::Loose);

        let paths: Vec<_> = found.iter().map(|m| m.relative_path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                std::path::PathBuf::from("packages/alpha"),
                std::path::PathBuf::from("services/api"),
            ],
            "the root's own pyproject.toml is not a member of its own list, \
             and results must be sorted for a stable UI order"
        );
        assert_eq!(found[0].name.as_deref(), Some("alpha"));
    }

    #[test]
    fn skips_virtualenvs_and_build_output() {
        let dir = tree(&[
            ("packages/alpha/pyproject.toml", "alpha"),
            (".venv/lib/site-packages/evil/pyproject.toml", "evil"),
            ("node_modules/pkg/pyproject.toml", "nope"),
            ("build/tmp/pyproject.toml", "nope2"),
            ("target/debug/pyproject.toml", "nope3"),
        ]);

        let found = discover(dir.path(), &WorkspaceKind::Loose);

        assert_eq!(found.len(), 1, "got {found:?}");
        assert_eq!(found[0].name.as_deref(), Some("alpha"));
    }

    #[test]
    fn stops_at_the_depth_cap() {
        // Deeper than MAX_DISCOVERY_DEPTH so a pathological tree cannot make
        // discovery crawl a whole disk.
        let dir = tree(&[("a/b/c/d/e/f/pyproject.toml", "deep")]);
        assert!(discover(dir.path(), &WorkspaceKind::Loose).is_empty());
    }

    #[test]
    fn classifies_workspace_members_against_the_globs() {
        let dir = tree(&[
            ("packages/alpha/pyproject.toml", "alpha"),
            ("tools/helper/pyproject.toml", "helper"),
        ]);
        let kind = WorkspaceKind::Workspace {
            member_globs: vec!["packages/*".to_string()],
        };

        let found = discover(dir.path(), &kind);

        let alpha = found.iter().find(|m| m.name.as_deref() == Some("alpha")).unwrap();
        let helper = found.iter().find(|m| m.name.as_deref() == Some("helper")).unwrap();
        assert!(alpha.is_workspace_member);
        assert!(
            !helper.is_workspace_member,
            "a package outside the declared globs is present in the tree but is \
             not a workspace member, and uv sync --package would reject it"
        );
    }

    #[test]
    fn everything_is_a_member_when_the_project_is_loose() {
        let dir = tree(&[("packages/alpha/pyproject.toml", "alpha")]);
        let found = discover(dir.path(), &WorkspaceKind::Loose);
        assert!(!found[0].is_workspace_member);
    }

    #[test]
    fn a_manifest_without_a_project_name_is_still_reported() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("tooling");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("pyproject.toml"), "[tool.black]\nline-length = 88\n").unwrap();

        let found = discover(dir.path(), &WorkspaceKind::Loose);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, None, "reported, but not selectable by --package");
    }

    #[test]
    fn a_missing_directory_yields_nothing_rather_than_failing() {
        let found = discover(std::path::Path::new("/no/such/place/at/all"), &WorkspaceKind::Loose);
        assert!(found.is_empty());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p bombadil-core members::discover`
Expected: FAIL — `cannot find function discover`.

- [ ] **Step 3: Write the implementation**

Above the test module:

```rust
use super::{WorkspaceKind, glob_matches};
use std::path::{Path, PathBuf};

/// How far below the project directory discovery walks.
///
/// Deep enough for `services/api/` and one level of nesting beyond it,
/// shallow enough that a pathological tree cannot make Bombadil crawl a disk.
pub const MAX_DISCOVERY_DEPTH: usize = 4;

/// Directories never worth walking into: virtual environments, VCS metadata,
/// dependency caches and build output. A `pyproject.toml` inside any of these
/// belongs to something else.
pub const SKIPPED_DIRS: &[&str] = &[
    ".venv",
    "venv",
    ".git",
    "node_modules",
    "__pycache__",
    "build",
    "dist",
    "target",
    ".tox",
    ".mypy_cache",
];

/// One `pyproject.toml` found below a registered project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredMember {
    /// Directory holding the manifest, relative to the project directory.
    pub relative_path: PathBuf,
    /// `[project].name`, absent when the manifest declares no project.
    pub name: Option<String>,
    /// Whether the declared workspace globs cover this path. Always false for
    /// a loose project, which has no workspace to be a member of.
    pub is_workspace_member: bool,
}

/// Find the packages belonging to a registered project.
///
/// Never installs or modifies anything — the result is a list of candidates
/// for the user to confirm. A missing or unreadable directory yields an empty
/// list rather than an error: discovery is not the place to fail an app.
///
/// Results are sorted by path, because `read_dir` order is not stable and the
/// UI list would otherwise reshuffle between runs.
pub fn discover(project_dir: &Path, kind: &WorkspaceKind) -> Vec<DiscoveredMember> {
    let mut found = Vec::new();
    walk(project_dir, project_dir, 0, &mut found);
    found.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

    if let WorkspaceKind::Workspace { member_globs } = kind {
        for member in &mut found {
            let as_str = member.relative_path.to_string_lossy().replace('\\', "/");
            member.is_workspace_member =
                member_globs.iter().any(|g| glob_matches(g, &as_str));
        }
    }

    found
}

fn walk(root: &Path, dir: &Path, depth: usize, found: &mut Vec<DiscoveredMember>) {
    if depth > MAX_DISCOVERY_DEPTH {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if SKIPPED_DIRS.contains(&name.as_ref()) || name.starts_with('.') {
            continue;
        }

        let manifest = path.join("pyproject.toml");
        if manifest.is_file()
            && let Ok(relative) = path.strip_prefix(root)
        {
            let name = std::fs::read_to_string(&manifest)
                .ok()
                .and_then(|text| crate::pyproject::parse(&text).ok())
                .and_then(|parsed| parsed.name);
            found.push(DiscoveredMember {
                relative_path: relative.to_path_buf(),
                name,
                is_workspace_member: false,
            });
        }

        walk(root, &path, depth + 1, found);
    }
}
```

Add to `crates/bombadil-core/src/members/mod.rs`:

```rust
mod discover;

pub use discover::{
    DiscoveredMember, MAX_DISCOVERY_DEPTH, SKIPPED_DIRS, discover,
};
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p bombadil-core --all-features`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/bombadil-core/src/members
git commit -m "feat(core): discover the packages inside a registered project

Walks the tree for pyproject.toml, skipping virtualenvs, VCS metadata,
dependency caches and build output, with a depth cap so a pathological tree
cannot make Bombadil crawl a disk. Results are sorted because read_dir order
is not stable and the UI list would otherwise reshuffle between runs.

Discovery never installs anything -- the caller confirms first. A missing
directory yields an empty list rather than an error."
```

---

### Task 4: Preferring a relative path for editable adds

**Files:**
- Create: `crates/bombadil-core/src/members/relative.rs`
- Modify: `crates/bombadil-core/src/members/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `members::path_for_manifest(project_dir: &Path, target: &Path) -> PathBuf`

> **Why this exists.** `uv add --editable <path>` writes the path *verbatim* into `[tool.uv.sources]` — verified against uv 0.12.1: a relative argument produces `path = "../outside"`, an absolute argument produces an absolute path. `pyproject.toml` is shared with teammates, so an absolute path like `/home/marcelo/work/lib` breaks for everyone else. Bombadil therefore passes a relative path whenever one exists, and falls back to absolute only when it does not — different Windows drive letters being the real case.
>
> This is a pure function on path *components*; it never touches the filesystem, so it is testable with paths that do not exist.

- [ ] **Step 1: Write the failing tests**

Create `crates/bombadil-core/src/members/relative.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn a_child_of_the_project_is_relative() {
        assert_eq!(
            path_for_manifest(Path::new("/work/repo"), Path::new("/work/repo/packages/alpha")),
            PathBuf::from("packages/alpha")
        );
    }

    #[test]
    fn a_sibling_checkout_walks_up_and_back_down() {
        // The case that matters: a shared library checked out beside the repo.
        assert_eq!(
            path_for_manifest(Path::new("/work/repo"), Path::new("/work/shared-lib")),
            PathBuf::from("../shared-lib")
        );
    }

    #[test]
    fn a_distant_relative_is_still_expressed_relatively() {
        assert_eq!(
            path_for_manifest(Path::new("/a/b/c/d"), Path::new("/a/x/y")),
            PathBuf::from("../../../x/y")
        );
    }

    #[test]
    fn the_project_itself_is_the_current_directory() {
        assert_eq!(
            path_for_manifest(Path::new("/work/repo"), Path::new("/work/repo")),
            PathBuf::from(".")
        );
    }

    #[test]
    fn paths_with_no_common_root_stay_absolute() {
        // On Windows this is a different drive letter; there is no relative
        // path between C:\ and D:\, so the manifest has to carry an absolute
        // one even though that is not portable.
        let target = Path::new("/elsewhere/lib");
        let got = path_for_manifest(Path::new("relative/project"), target);
        assert_eq!(got, target);
    }

    #[test]
    fn a_relative_target_is_returned_unchanged() {
        // Nothing sensible to compute against; hand it to uv as given.
        assert_eq!(
            path_for_manifest(Path::new("/work/repo"), Path::new("../lib")),
            PathBuf::from("../lib")
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p bombadil-core members::relative`
Expected: FAIL — `cannot find function path_for_manifest`.

- [ ] **Step 3: Write the implementation**

Above the test module:

```rust
use std::path::{Component, Path, PathBuf};

/// The path form to hand `uv add --editable`, which uv writes verbatim into
/// `[tool.uv.sources]`.
///
/// Prefers a relative path, because `pyproject.toml` is shared: an absolute
/// path baked into it works only on the machine that added it. Falls back to
/// the target unchanged when no relative path exists — different Windows
/// drives, or a target that is already relative.
///
/// Pure: operates on path components and never touches the filesystem, so it
/// works for paths that do not exist yet.
pub fn path_for_manifest(project_dir: &Path, target: &Path) -> PathBuf {
    if !target.is_absolute() || !project_dir.is_absolute() {
        return target.to_path_buf();
    }

    let mut from = project_dir.components().peekable();
    let mut to = target.components().peekable();

    // Drop the shared prefix.
    while let (Some(a), Some(b)) = (from.peek(), to.peek()) {
        if a == b {
            from.next();
            to.next();
        } else {
            break;
        }
    }

    // A different prefix component means different roots — on Windows, a
    // different drive. No relative path exists.
    let from: Vec<Component<'_>> = from.collect();
    let to: Vec<Component<'_>> = to.collect();
    if from
        .iter()
        .chain(to.iter())
        .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
    {
        return target.to_path_buf();
    }

    let mut relative = PathBuf::new();
    for _ in &from {
        relative.push("..");
    }
    for component in &to {
        relative.push(component.as_os_str());
    }

    if relative.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        relative
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p bombadil-core --all-features`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/bombadil-core/src/members
git commit -m "feat(core): prefer a relative path when adding an editable project

uv writes the path it is given verbatim into [tool.uv.sources] -- verified
against uv 0.12.1, where a relative argument yields path = \"../outside\".
Since pyproject.toml is shared with teammates, an absolute path baked into
it works only on the machine that added it.

Falls back to absolute only when no relative path exists, which in practice
means different Windows drive letters."
```

---

### Task 5: Command builders for member selection and editable adds

**Files:**
- Modify: `crates/bombadil-core/src/uv/command.rs`

**Interfaces:**
- Consumes: `ResolvedUv`, `ProjectContext`, `project_spec` (plan 2a).
- Produces:
  - `uv::command::sync_all_members(uv, ctx) -> CommandSpec`
  - `uv::command::sync_members(uv, ctx, package_names: &[String]) -> CommandSpec`
  - `uv::command::add_editable(uv, ctx, path: &Path) -> CommandSpec`
  - `uv::command::remove_editable(uv, ctx, package_name: &str) -> CommandSpec`

> **The behaviour that makes this work, verified against uv 0.12.1:** re-syncing with a smaller `--package` set *removes* the members no longer selected. After `--all-packages` installed `alpha, api, beta`, `--package alpha` left only `alpha`. Deselection therefore needs no manifest edit and no `uv remove` — it is just the next sync. That is what lets spec §5a's "selection never touches `[tool.uv.workspace] members`" hold.
>
> **`sync_members` with an empty slice must not silently become `uv sync`.** Plain `uv sync` in a workspace root installs *no* members, so an empty selection and "sync nothing" happen to coincide today — but relying on that coincidence would break the moment uv changes its default. Emit `--no-install-workspace`, which says what is meant.

- [ ] **Step 1: Write the failing tests**

Append to the test module in `crates/bombadil-core/src/uv/command.rs`:

```rust
    #[test]
    fn syncing_all_members_uses_all_packages() {
        // Plain `uv sync` in a workspace installs NO members -- verified
        // against uv 0.12.1. "Everything" is specifically --all-packages.
        assert_eq!(
            sync_all_members(&uv(), &ctx()).args,
            vec!["sync", "--all-packages"]
        );
    }

    #[test]
    fn syncing_a_subset_repeats_the_package_flag() {
        let names = vec!["alpha".to_string(), "beta".to_string()];
        assert_eq!(
            sync_members(&uv(), &ctx(), &names).args,
            vec!["sync", "--package", "alpha", "--package", "beta"]
        );
    }

    #[test]
    fn syncing_an_empty_selection_says_so_explicitly() {
        // Plain `uv sync` happens to install no members today, but relying on
        // that coincidence would break if uv changed its default.
        assert_eq!(
            sync_members(&uv(), &ctx(), &[]).args,
            vec!["sync", "--no-install-workspace"]
        );
    }

    #[test]
    fn adding_an_editable_project_passes_the_path_through() {
        let spec = add_editable(&uv(), &ctx(), std::path::Path::new("../shared-lib"));
        assert_eq!(spec.args, vec!["add", "--editable", "../shared-lib"]);
    }

    #[test]
    fn removing_an_editable_project_names_the_package() {
        assert_eq!(
            remove_editable(&uv(), &ctx(), "shared-lib").args,
            vec!["remove", "shared-lib"]
        );
    }

    #[test]
    fn the_new_builders_are_project_scoped_like_the_rest() {
        // Same invariant the parametrized test enforces for the others: a
        // builder that bypassed project_spec would put the venv elsewhere.
        for (name, spec) in [
            ("sync_all_members", sync_all_members(&uv(), &ctx())),
            ("sync_members", sync_members(&uv(), &ctx(), &["alpha".to_string()])),
            ("add_editable", add_editable(&uv(), &ctx(), std::path::Path::new("../x"))),
            ("remove_editable", remove_editable(&uv(), &ctx(), "x")),
        ] {
            assert_eq!(
                spec.cwd.as_deref(),
                Some(std::path::Path::new("/proj/my-api")),
                "{name} must run in the project directory"
            );
            assert_eq!(
                spec.env.get("UV_PROJECT_ENVIRONMENT").map(String::as_str),
                Some("/envs/my-api-3f9c1a"),
                "{name} must point uv at the resolved venv"
            );
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p bombadil-core uv::command`
Expected: FAIL — `cannot find function sync_all_members`.

- [ ] **Step 3: Write the implementation**

Add to `crates/bombadil-core/src/uv/command.rs`, after `sync_with_python`:

```rust
/// Install every workspace member in editable mode.
///
/// `--all-packages` is required and not a nicety: plain `uv sync` in a
/// workspace root installs no members at all. Verified against uv 0.12.1.
pub fn sync_all_members(uv: &ResolvedUv, ctx: &ProjectContext) -> CommandSpec {
    project_spec(uv, ctx).args(["sync", "--all-packages"])
}

/// Install exactly the named members and remove the rest.
///
/// Re-syncing with a smaller set removes the members no longer named —
/// verified against uv 0.12.1 — which is what lets a user's selection live in
/// Bombadil's own config instead of being written into the shared
/// `[tool.uv.workspace] members` list.
pub fn sync_members(uv: &ResolvedUv, ctx: &ProjectContext, package_names: &[String]) -> CommandSpec {
    if package_names.is_empty() {
        // Say what is meant rather than relying on plain `uv sync` happening
        // to install nothing.
        return project_spec(uv, ctx).args(["sync", "--no-install-workspace"]);
    }

    let mut spec = project_spec(uv, ctx).arg("sync");
    for name in package_names {
        // Two `.arg` calls rather than `.args(["--package", name])`: the array
        // form would mix `&str` and `&String` elements, which does not compile.
        spec = spec.arg("--package").arg(name.clone());
    }
    spec
}

/// Add a project as an editable path dependency.
///
/// `path` is written verbatim by uv into `[tool.uv.sources]`, so callers
/// should pass the output of `members::path_for_manifest` rather than an
/// absolute path — `pyproject.toml` is shared with teammates.
pub fn add_editable(uv: &ResolvedUv, ctx: &ProjectContext, path: &Path) -> CommandSpec {
    project_spec(uv, ctx)
        .args(["add", "--editable"])
        .arg(path.to_string_lossy())
}

/// Drop a previously added editable dependency.
pub fn remove_editable(uv: &ResolvedUv, ctx: &ProjectContext, package_name: &str) -> CommandSpec {
    project_spec(uv, ctx).args(["remove", package_name])
}
```

Add `use std::path::Path;` to the file's imports if not already present.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p bombadil-core --all-features`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/bombadil-core/src/uv/command.rs
git commit -m "feat(core): build uv commands for member selection and editable adds

Plain uv sync in a workspace root installs NO members -- verified against uv
0.12.1 -- so 'everything selected' is specifically --all-packages, and an
empty selection emits --no-install-workspace rather than relying on plain
sync happening to install nothing today.

Re-syncing with a smaller --package set removes the deselected members, so a
user's selection never has to be written into the shared
[tool.uv.workspace] members list."
```

---

### Task 6: Reporting which packages are installed editable

**Files:**
- Modify: `crates/bombadil-core/src/uv/results.rs`
- Modify: `crates/bombadil-core/tests/fixtures/uv-pip-list.json`

**Interfaces:**
- Consumes: nothing new.
- Produces: `InstalledPackage.editable_project_location: Option<PathBuf>` and `InstalledPackage::is_editable()`

> `uv pip list --format json` reports `editable_project_location` for editable installs — verified against uv 0.12.1, where a workspace member appeared as
> `{"name":"beta","version":"0.1.0","editable_project_location":"/…/packages/beta"}`.
> Without this field the UI cannot tell an editable member from an ordinary dependency of the same name, which is the whole point of the members list.

- [ ] **Step 1: Update the fixture**

Replace `crates/bombadil-core/tests/fixtures/uv-pip-list.json` with a capture that includes both shapes:

```json
[{"name":"idna","version":"3.18"},{"name":"iniconfig","version":"2.3.0"},{"name":"beta","version":"0.1.0","editable_project_location":"/work/repo/packages/beta"}]
```

> **This grows the fixture from two entries to three,** so the existing `reads_installed_packages` test that asserts `got.len() == 2` will fail. Update it to `3` and keep its existing assertions about `idna`. Do not delete that test to make the new one fit.

- [ ] **Step 2: Write the failing test**

Append to the test module in `crates/bombadil-core/src/uv/results.rs`:

```rust
    #[test]
    fn an_editable_install_reports_where_it_came_from() {
        // This is what distinguishes a dev-installed member from an ordinary
        // dependency that happens to share its name.
        let got = parse_pip_list(PIP_LIST).unwrap();

        let ordinary = got.iter().find(|p| p.name == "idna").unwrap();
        assert!(!ordinary.is_editable());
        assert_eq!(ordinary.editable_project_location, None);

        let editable = got.iter().find(|p| p.name == "beta").unwrap();
        assert!(editable.is_editable());
        assert_eq!(
            editable.editable_project_location.as_deref(),
            Some(std::path::Path::new("/work/repo/packages/beta"))
        );
    }
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p bombadil-core uv::results`
Expected: FAIL — no field `editable_project_location`.

- [ ] **Step 4: Write the implementation**

In `crates/bombadil-core/src/uv/results.rs`, extend the struct:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    /// Present when uv installed this package in editable mode, naming the
    /// directory it was installed from. Absent for ordinary dependencies.
    #[serde(default)]
    pub editable_project_location: Option<PathBuf>,
}

impl InstalledPackage {
    /// Whether this package is a dev-mode install rather than an ordinary
    /// dependency. Distinguishing the two is the point of the members list.
    pub fn is_editable(&self) -> bool {
        self.editable_project_location.is_some()
    }
}
```

`PathBuf` is already imported in this file.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p bombadil-core --all-features`
Expected: PASS. The existing pip-list tests must still pass — `#[serde(default)]` keeps entries without the field parsing.

- [ ] **Step 6: Commit**

```bash
git add crates/bombadil-core/src/uv/results.rs crates/bombadil-core/tests/fixtures/uv-pip-list.json
git commit -m "feat(core): report where an editable install came from

uv pip list --format json carries editable_project_location for editable
installs. Without it the UI cannot tell a dev-installed member from an
ordinary dependency of the same name, which is the whole point of the
members list."
```

---

### Task 7: Integration test against a real uv workspace

**Files:**
- Modify: `crates/bombadil-core/tests/uv_integration.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: no library API.

> This is the only place any of this runs against a real uv. It is the test that would have caught the `pip_list` environment bug in plan 2a, and the one that proves the deselection behaviour this plan depends on.

- [ ] **Step 1: Write the test**

Append to `crates/bombadil-core/tests/uv_integration.rs`:

```rust
/// Build a two-member uv workspace under `root`.
fn write_workspace(root: &std::path::Path) {
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"wsroot\"\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\n\
         dependencies = []\n\n[tool.uv.workspace]\nmembers = [\"packages/*\"]\n",
    )
    .unwrap();

    for member in ["alpha", "beta"] {
        let dir = root.join("packages").join(member);
        std::fs::create_dir_all(dir.join("src").join(member)).unwrap();
        std::fs::write(
            dir.join("pyproject.toml"),
            format!(
                "[project]\nname = \"{member}\"\nversion = \"0.1.0\"\n\
                 requires-python = \">=3.10\"\ndependencies = []\n\n\
                 [build-system]\nrequires = [\"hatchling\"]\n\
                 build-backend = \"hatchling.build\"\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("src").join(member).join("__init__.py"), "").unwrap();
    }
}

#[test]
fn workspace_member_selection_agrees_with_a_real_uv() {
    let runner = SystemCommandRunner;
    let uv = resolve(&UvSource::FromPath, &runner, &locate_on_path, &|| {
        Err("bundled uv is not used by this test".to_string())
    })
    .expect("a uv on PATH at or above MIN_UV_VERSION is required for this test");

    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().join("repo");
    std::fs::create_dir_all(&project_dir).unwrap();
    write_workspace(&project_dir);

    let ctx = ProjectContext {
        project_dir: project_dir.clone(),
        venv_path: tmp.path().join("env"),
    };

    let installed = |ctx: &ProjectContext| -> Vec<String> {
        let outcome = runner.run(&command::pip_list(&uv, ctx)).unwrap();
        assert!(outcome.success(), "pip list failed: {}", outcome.stderr);
        let mut names: Vec<String> = parse_pip_list(&outcome.stdout)
            .expect("pip list JSON parses")
            .into_iter()
            .map(|p| p.name)
            .collect();
        names.sort();
        names
    };

    // Everything.
    let outcome = runner.run(&command::sync_all_members(&uv, &ctx)).unwrap();
    assert!(outcome.success(), "sync --all-packages failed: {}", outcome.stderr);
    assert_eq!(installed(&ctx), vec!["alpha", "beta"]);

    // The behaviour this whole plan rests on: a smaller selection REMOVES the
    // rest, so deselection needs no manifest edit.
    let outcome = runner
        .run(&command::sync_members(&uv, &ctx, &["alpha".to_string()]))
        .unwrap();
    assert!(outcome.success(), "sync --package alpha failed: {}", outcome.stderr);
    assert_eq!(
        installed(&ctx),
        vec!["alpha"],
        "re-syncing with a smaller selection must remove the deselected members"
    );

    // And the survivor is genuinely editable, which is what "dev mode" means.
    let outcome = runner.run(&command::pip_list(&uv, &ctx)).unwrap();
    let packages = parse_pip_list(&outcome.stdout).unwrap();
    let alpha = packages.iter().find(|p| p.name == "alpha").unwrap();
    assert!(
        alpha.is_editable(),
        "workspace members must be installed editable; got {alpha:?}"
    );
}
```

> Adapt the `resolve(..)` call to whatever signature is current — plan 2a added a `locate` parameter before `bundled`. Check `uv_integration.rs`'s existing tests and mirror them.

- [ ] **Step 2: Run it**

```bash
cargo test -p bombadil-core --features uv-integration --test uv_integration
```
Expected: PASS. Requires uv ≥ 0.12.1 on `PATH` and network access for the build backend.

- [ ] **Step 3: Full verification and commit**

```bash
cargo fmt --all --check \
  && cargo clippy --all-targets --all-features --locked -- -D warnings \
  && cargo test --workspace --locked \
  && cargo deny check
git add crates/bombadil-core/tests/uv_integration.rs
git commit -m "test(core): prove workspace member selection against a real uv

Asserts the behaviour the whole plan rests on: re-syncing with a smaller
--package set removes the deselected members, so a user's selection never
has to be written into the shared [tool.uv.workspace] members list. Also
asserts members really are installed editable, which is what dev mode means."
```

---

## Done when

- `cargo test --workspace --locked` passes on Linux, macOS and Windows.
- `cargo clippy --all-targets --all-features --locked -- -D warnings` and `cargo deny check` are clean.
- The `msrv` job at 1.88.0 passes.
- `uv-integration` is green on all three platforms.
- A config written before this feature still loads unchanged — no schema bump.

## What plan 3 picks up

The member checklist UI: the list from `members::discover`, all ticked when `all_discovered` is set, an **Add project…** action taking any path (inside the tree or outside) through `members::path_for_manifest`, and a marker on each row showing whether it is currently installed editable, from `InstalledPackage::is_editable()`.

Plan 3 must also resolve a question this plan deliberately leaves open: **what to do when a discovered package has no `[project].name`.** `discover` reports it, because hiding a real directory would be more confusing than showing it — but it cannot be passed to `uv sync --package`. The UI should show it, disabled, with the reason.
