# Several environments per project

**Status:** design, awaiting review.

**Goal:** A project can have more than one virtual environment — a 3.11 and a
3.12, say — with one active. The sidebar shows them as a tree under the
project, and Sync, Open terminal and the drift glyph all act on the active one.

## Decisions taken

1. **Several environments per project, one active.** `Project::venv` becomes a
   list.
2. **The Python pin lives on the environment**, not the project. Each row's
   version *is* its pin, which is the reason to have more than one row.
   `Project::python` is removed; the global `Settings::python` becomes the
   default a newly added environment starts with.
3. **An environment is identified by its location.** No user-typed name: two
   environments cannot collide because two directories cannot be the same
   directory, and a label would be a second identity to keep in step with the
   folder it points at.
4. **The sidebar is a tree**, expandable per project, each child a real
   directory with its own glyph.

## This supersedes part of what shipped today

Said plainly rather than left for a reader to discover. PRs #20 and #21 put a
Python pin on the *project*, overridable from the global default:

```rust
Project::python: Override<PythonPin>   // removed by this design
Settings::python: PythonPin            // kept, meaning changes
```

Decision 2 moves that pin onto each environment. `Settings::python` survives
but stops being a value projects inherit at sync time — it becomes the pin a
newly added environment is created with. Everything else from those PRs stands:
`Override<T>`, `resolve::effective` for uv and terminal, `satisfies_pin`, and
the destructive-change confirmation, which becomes *more* important here
because a project now has several environments to lose one of.

## Model

```rust
/// One virtual environment belonging to a project.
///
/// Identified by `location`: there is no name, because two environments cannot
/// share a directory and a label would be a second identity to keep in step
/// with the folder.
pub struct Environment {
    pub location: VenvLocation,
    /// The interpreter this environment is built with. `Unpinned` lets uv
    /// choose, subject to the manifest's `requires-python`.
    pub python: PythonPin,
}

pub struct Project {
    // ...
    /// Every environment configured for this project. Never empty after load
    /// — see the migration below.
    pub environments: Vec<Environment>,
    /// Which one Sync, Open terminal and the sidebar glyph act on.
    ///
    /// The location rather than an index, for the reason `ConfirmRecreate`
    /// documents: an index is only meaningful against the list as it stood,
    /// and this list is edited while the application is running.
    pub active: VenvLocation,
}
```

`Project::venv` and `Project::python` are removed.

### Migration

`SCHEMA_VERSION` goes to 2. This is the case the mechanism exists for: the
change cannot be expressed by adding `#[serde(default)]` fields, because a
field is being *replaced*, and there is no honest default for "which of these
environments is active" that an older build could write.

Loading a version-1 config synthesises one environment from `venv`, with the
project's old `python` as its pin, and marks it active — so a config written
before this loads with exactly the environment it already had. A version-1
config is migrated in memory and saved as version 2; the existing "a newer
schema opens read-only" guard then protects a user who downgrades.

The golden fixture gains a version-1 twin so the migration is tested against a
real file rather than a hand-built `Config`.

## What acts on the active environment

| Surface | Change |
|---|---|
| `venv_path::resolve` | takes an `Environment`, not a `Project` + `Settings` |
| Sync | builds against the active environment; `-p` from its own pin |
| Open terminal | activates the active environment |
| Recreate venv | deletes the active environment only |
| `sidebar::state` | one glyph per environment, plus a rolled-up glyph on the project |
| Dependencies / Members | read the active environment |
| `resolve::effective` | keeps uv and terminal; loses `python` |

## The tree

```
Projects                    [Add]
▾ ● thorn
    ● .venv        3.12  active
    ○ .venv-3.11   3.11
    ◍ thorn-3.13   3.13  drifted
▾ ○ api
    ○ .venv        no environment yet
```

- Expand state lives on `App`, not in the config: which rows a user has open is
  not a setting worth persisting, and a config field nothing else reads is a
  field nothing else tests.
- Clicking a project selects it. Clicking an environment makes it active — a
  single click, because it is not destructive: nothing is deleted, the next
  Sync simply builds against a different directory.
- The project's own glyph rolls up its environments: drifted if any is,
  otherwise absent if the active one is, otherwise present. One glyph cannot
  say more than that, and the tree is there for the detail.
- A project with one environment still shows it as a child. A tree that
  collapsed the single case would make "how many are there" a question you
  answer by counting rows in two different layouts.

## Preferences

The Environments section, project scope, becomes the list editor: add, remove,
choose a location, set a per-environment pin, mark active. The global scope is
unchanged — the default location and the pin a new environment starts with.

Removing the active environment requires the same confirmation Recreate venv
uses. It is the same act: an environment stops existing.

## Testing

Core, all mutation-proven:

- a version-1 config migrates to exactly the environment it had, pin included
- a version-2 config round-trips
- `active` surviving the list being reordered, and falling back to the first
  environment when it names a location no longer in the list
- the rolled-up project glyph: drifted wins over absent wins over present
- sync builds `-p` from the *active* environment's pin, not the project's
- removing the active environment reaches the confirmation

`view` stays untested by convention, so the tree's rendering ships without
mutation proofs.

## Out of scope

- Running a command against a non-active environment without switching to it.
- Copying or cloning an environment.
- Per-environment env vars or indexes: those stay per project. An environment
  is an interpreter and a directory, not a second configuration scope.
