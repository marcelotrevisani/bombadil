# Preferences: global defaults, per-project overrides

**Status:** design, awaiting review.

**Goal.** One place to set the six things a project's environment depends on —
where its virtual environment lives, its environment variables, its terminal,
its Python version, which uv runs it, and which package indexes it uses — each
settable globally and overridable per project.

## Where the six stand today

| Setting | Global | Per project |
|---|---|---|
| Root venv path | `Settings::default_venv_location` | `Project::venv` (`VenvLocation::Default` = inherit) |
| Environment variables | `Config::global_env` | `Project::env`, merged over global by key |
| Private index / PyPI | `Config::indexes` | `Project::index_names` — selection only |
| Terminal | `Config::terminal_defaults`, per OS | **missing** |
| uv source | `Settings::uv_source` | **missing** |
| Python version | **missing** | **missing** |

Half this work is model, not interface.

## Decisions taken

1. **One Preferences screen with a scope switch.** Reached from the top-bar
   menu. A scope control picks Global or a specific project; the same sections
   sit underneath either way, and in project scope each one reads *Inherit from
   global* until overridden. The Env vars, Indexes and Settings tabs are
   absorbed and removed. Dependencies and Members stay — they are readouts, not
   settings.
2. **Scripts keeps its own tab.** Same shape as the rest, deliberately left
   where it is.
3. **A Python version is a pin.** Sync asserts it; a venv on another version
   reads as drift. See the destructive consequence below.
4. **Indexes stay globally defined.** A project ticks which apply and picks
   which is primary. One definition means one keychain credential; project-local
   index definitions would make a credential belong to a project and let two
   projects give one name two meanings.
5. **Presentation: a modal with a section list down the left.** Scope across
   the top, sections on the left, the chosen section's fields on the right.

## What a real uv does — verified, not assumed

Run against uv 0.12.1 on a throwaway project before writing any of this.

| Question | Answer |
|---|---|
| `uv sync -p X` on a venv at another version | **Deletes and recreates it.** Prints `Removed virtual environment at: .venv`, then rebuilds from the lockfile. Exit 0. |
| The same pin re-run | Idempotent. No removal, packages untouched. |
| A pin *more precise* than `pyvenv.cfg` records (`-p 3.12.13` against a recorded `3.12`) | No removal. uv compares against the real interpreter, not the file. |
| What `pyvenv.cfg` records | What was **requested**. `-p 3.12` wrote `version_info = 3.12`; a system interpreter wrote `3.13.14`. It is not reliably a full version. |
| An unsatisfiable pin | Exit **2**, venv untouched, actionable text on stderr. |
| Anything installed outside the lockfile | Gone after a version change. |

Two consequences follow, and neither was visible from the outside.

### Changing a pin is destructive

Not "rebuilds": *deletes*. uv removes the environment and makes a new one, so
anything not in the lockfile is gone. That is the same class of action as
Recreate venv, which this application already guards behind a confirmation
naming the exact directory and warning that a failed rebuild leaves nothing.

**Changing a Python pin must reach that same confirmation** when it would
change an existing environment. A setting that silently deletes an environment
when saved would be the worst thing in this application.

### The drift check must not compare versions for equality

`pyvenv.cfg` records what was asked for, so a pin of `3.12.13` against a
recorded `3.12` is a *match* that naive equality calls a mismatch. The
consequence is not a cosmetic wrong glyph: the sidebar would show permanent
drift, and every press of Sync would delete and recreate a perfectly good
environment. A destructive loop out of a string comparison.

The rule is to **compare only the release segments both versions carry**:
`3.12.13` against `3.12` compares `[3,12]` against `[3,12]` and matches;
`3.12` against `3.13.14` compares `[3,12]` against `[3,13]` and does not.
`interpreter::cmp_segments` and `release_segments` already exist and do exactly
this arithmetic.

## Model

```rust
/// A project's answer for one setting: follow the global value, or replace it.
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Override<T> { #[default] Inherit, Set(T) }
```

Global holds the concrete `T`; a project holds `Override<T>`.

New fields: `Settings::python`, `Project::python`, `Project::uv_source`,
`Project::terminals` (per OS, mirroring the global map — a project override of
"iTerm2" must not make it unopenable on Linux), `Project::primary_index`.

A Python pin needs three states, not two, so its value type carries its own off
switch: `PythonPin = Unpinned | Version(String)`. A project can pin, inherit,
or explicitly *un*pin against a global pin.

**One deliberate exception.** `VenvLocation::Default` already means exactly
"inherit" and sits in every config on disk. It stays as it is — same behaviour,
no schema migration, and the generic postdates it.

**Lists do not inherit, they merge.** Env vars already work this way: a
project's variables layer over the global ones by key. Indexes are a global
list with a per-project selection. Only scalars get `Override<T>`, and the
Preferences UI says which shape each section is rather than pretending both are
the same.

## One resolver

```rust
resolve::effective(config, project) -> Effective
```

The single answer to "what applies here", consumed by sync, the terminal
launcher, the drift check, and the Preferences UI's greyed inherited values.

Without it, five call sites each re-derive inheritance and drift apart. This
project has already been bitten by exactly that: a whole-branch review found a
"two halves don't join" defect on all seven branches, and the worst was
`App::projects` being a stale clone, so a session's edits were dropped at Sync
while `config.toml` stayed correct.

## Two plans, not one

**Plan A — core.** `Override<T>`, the new fields, `resolve::effective`,
segment-wise drift, `-p` on sync, and the confirmation a destructive pin change
must reach. Headless and fully testable, and it carries every risk identified
above.

**Plan B — the Preferences modal.** The scope switch, the section list, the
inherit-or-override control, and removing the three absorbed tabs. Untestable
by this repo's `view` convention, which is the second reason to keep it
separate: the risky half ships with proofs, and the half that cannot be proven
carries nothing else.

## Testing

Core, all mutation-proven:

- inheritance precedence per setting, both directions
- `Override::Inherit` deserialising from a config that predates the field
- a pin of `3.12.13` against a recorded `3.12` is **not** drift — the
  destructive-loop guard, and the single most important test here
- a pin of `3.12` against a recorded `3.13.14` **is** drift
- sync builds `-p` only when a pin resolves
- changing a pin against an existing environment reaches the confirmation
- an unsatisfiable pin surfaces uv's own stderr, not a generic failure

`view` stays untested by convention, so Plan B ships without mutation proofs.
That is stated, not hidden.

## Out of scope

- Project-local index definitions — decided against; see decision 4.
- Migrating `VenvLocation::Default` onto `Override<T>`.
- Anything about Scripts, which keeps its tab.
