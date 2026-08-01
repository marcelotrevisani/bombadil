# Bombadil — a cross-platform GUI for uv-managed Python environments

**Date:** 2026-08-01
**Status:** Approved design, ready for implementation planning

## 1. Purpose

Bombadil is a desktop application for people who manage several Python projects with
[uv](https://github.com/astral-sh/uv) and want to do it without memorising uv's CLI or
hand-editing `pyproject.toml`. It registers projects, creates and adopts their virtual
environments, edits their dependencies, layers environment variables and pre-activate
scripts over them, holds private index credentials in the OS keychain, and opens a
terminal already activated in the right directory.

It runs on Linux, macOS and Windows from a single self-contained binary per platform.

## 2. Scope

### In scope for v1

| Area | Behaviour |
| --- | --- |
| Project registry | Register a `pyproject.toml` by file picker. Projects persist across launches with a stable identity. |
| Virtual environments | Create at a chosen location, or adopt one that already exists. Recreate and delete. |
| Python interpreters | Choose from interpreters uv can find; install a missing one on demand via `uv python install`. |
| Dependencies | View declared constraints by group, with installed versions and an expandable transitive tree. Add, remove, change constraint, upgrade, sync. |
| Environment variables | Global set and per-project set, merged with per-project winning. Values may be marked secret and stored in the OS keychain. |
| Pre-activate scripts | Ordered, enable-able list per project and globally. Their exported variables are captured and applied. |
| Private indexes | Named indexes defined once globally, opted into per project. Credentials in the OS keychain, never in a file. |
| Terminals | Right-click a project to open a detected terminal emulator, activated, in the project directory. |
| uv itself | Bundled inside the executable; user-selectable source. |

### Explicitly not in v1

These are recorded so the implementation plan does not quietly absorb them:

- No embedded terminal emulator — we spawn the user's own.
- No PyPI network features: no package search, no *outdated* column, no package metadata.
- No standalone Python-version manager screen.
- No support for pip, Poetry, Conda, PDM or Pipenv projects.
- No scanning the filesystem for projects; registration is always deliberate.
- No dedicated UI for uv workspaces — a workspace registers as a single project.
- No application auto-update.
- No writing to `pyproject.toml` or `uv.lock` by Bombadil itself, ever. uv owns both.

## 3. Design decisions

Each of these was chosen deliberately; the rationale matters more than the choice.

**Projects are the unit, not environments.** The user registers a `pyproject.toml`. Its
virtual environment is a property of that project which can be created, adopted, deleted
or recreated without losing the entry, its variables, or its scripts. A registered project
with no venv yet is a normal state.

**One environment pipeline, used everywhere.** Environment variables, scripts and index
credentials apply to *everything the app launches* — both spawned terminals and the uv
commands Bombadil runs itself. Nothing is written into the venv's `activate` scripts.
The configuration therefore survives deleting and recreating the venv, leaves no trace if
Bombadil is uninstalled, and — critically — cannot drift between what the GUI does and
what the user's terminal does, because it is one code path.

**No `source activate`.** Activation is performed directly: set `VIRTUAL_ENV`, prepend the
venv's `bin` (POSIX) or `Scripts` (Windows) directory to `PATH`, and remove `PYTHONHOME`.
This is precisely what the `activate` scripts do, and doing it directly works identically
across bash, zsh, fish, PowerShell and cmd instead of needing a dialect per shell.

**Venvs are recreated, never moved.** A virtual environment hardcodes absolute paths in
`pyvenv.cfg` and in its console scripts. Pointing a project at a new location therefore
creates a fresh environment there and syncs it. The old directory is left untouched and
the user is offered a one-click delete.

**uv owns the project files.** Bombadil parses `pyproject.toml` to display it and re-reads
it from disk after every mutating command, but every write goes through uv. This removes a
whole class of corruption bugs and means Bombadil cannot disagree with the lockfile.

**Secrets never enter a config file.** Index credentials and secret environment variables
live in the platform keychain. The config file holds only the username and a reference.

## 4. Architecture

A Cargo workspace of three crates.

### `bombadil-core`

No `iced` dependency. All I/O sits behind traits so the logic most likely to be wrong is
testable headless, on any platform, without a display or a network.

| Module | Responsibility |
| --- | --- |
| `model` | `Project`, `VenvLocation`, `EnvVar`, `Index`, `Script` — plain serde data, no behaviour. |
| `store` | Load and save `config.toml`; atomic write; schema versioning and migration. |
| `secrets` | `SecretStore` trait over the OS keychain, with an in-memory implementation for tests. |
| `envspec` | Pure function composing the final environment map for a child process. |
| `pyproject` | Read-only parse: project name, `requires-python`, dependency groups and extras. |
| `uv` | Resolve the uv binary; build argv; spawn; stream output; parse JSON results. |
| `python` | Interpreter discovery and on-demand installation through uv. |
| `terminal` | Detect available emulators and shells; build the spawn command for a given OS. |

The `CommandRunner` trait is the single seam through which every subprocess is launched.
Tests substitute a fake returning canned stdout, stderr and exit codes.

### `bombadil-uv-bin`

Holds the embedded uv binary and nothing else. `build.rs` acquires the release artifact
for the target triple, verifies it, and `include_bytes!`s it. It is a separate crate behind
a default-on feature so that `cargo test -p bombadil-core --no-default-features` neither
links 64 MB nor touches the network.

### `bombadil-gui`

The iced application: `app` (State, Message, update, subscriptions), `screens/`
(projects, dependencies, env_vars, scripts, indexes, settings), `widgets/`
(tree_table, context_menu, output_drawer) and `theme`.

**The invariant that keeps the split honest:** the GUI crate never spawns a process, reads
a secret, or touches a path. It sends a request into core and receives messages back.

### Concurrency

uv commands run as asynchronous tasks that stream stdout and stderr line by line into the
output drawer. **At most one mutating uv command per project at a time**, enforced by a
per-project lock — two concurrent `uv add` invocations would race on `uv.lock`. Read-only
commands may overlap. Every running command is cancellable by killing its child process
group, because `uv sync` on a large project is not instantaneous.

## 5. Data model and persistence

A single `config.toml` in the platform configuration directory: `~/.config/bombadil/` on
Linux, `~/Library/Application Support/bombadil/` on macOS, `%APPDATA%\bombadil\` on Windows.

```
Config {
    schema_version: u32,
    settings:       Settings,
    indexes:        Vec<Index>,
    global_env:     Vec<EnvVar>,
    global_scripts: Vec<Script>,
    projects:       Vec<Project>,
}

Settings {
    default_venv_location: Alongside | Central(PathBuf),
    uv_source:             Auto | Bundled | FromPath | Custom(PathBuf),
    terminal_defaults:     Map<Os, TerminalChoice>,
    theme:                 Theme,
}

Project {
    id:             Uuid,                              // stable across moves and renames
    label:          String,                            // defaults to [project].name
    pyproject_path: PathBuf,
    venv:           Default | Alongside | Custom(PathBuf),
    env:            Vec<EnvVar>,
    scripts:        Vec<Script>,
    index_names:    Vec<String>,                       // references Config.indexes
}

Index  { name: String, url: Url, kind: Default | Extra,
         auth: None | Basic { username: String } | Token,
         default_for_new_projects: bool }

EnvVar { key: String, value: String, secret: bool }    // value empty when secret
Script { path: PathBuf, args: Vec<String>, enabled: bool }
```

`Index.name` is constrained to `[A-Za-z0-9_-]+` and must be unique, because it becomes part
of an environment variable name (see §7). `Project.index_names` holds references rather than
copies: deleting a global index warns which projects reference it, and a project referencing
a name that no longer exists is reported as a configuration error rather than ignored.

### Persistence rules

**Nothing derived is stored.** Venv existence, the Python version read from `pyvenv.cfg`,
the dependency table, detected terminals and detected interpreters are all recomputed at
load and on refresh. The config file stays small and cannot go stale against reality.

**Writes are atomic.** Serialise to a temporary file in the same directory, fsync, rename.

**Versioning is defensive in both directions.** On load, a lower `schema_version` is
migrated in memory and the original backed up as `config.v<N>.bak` before the first write.
A *higher* version opens read-only with an explanatory banner, so an older build cannot
silently destroy a config written by a newer one. A file that fails to parse is never
overwritten: the app starts on defaults and surfaces the parse error and the file path.

**Missing projects degrade gracefully.** If a `pyproject.toml` no longer exists, the project
is shown as *missing* with Relocate and Remove actions. It is never silently dropped.

### Secrets

Keychain service `bombadil`; entries `index/<name>` and `env/<project-id>/<KEY>`.

Headless Linux and WSL frequently have no Secret Service running. When the keychain is
unavailable the app states so plainly and offers session-only entry held in memory for the
lifetime of the process, rather than failing at spawn time with an opaque error.

## 6. Environment resolution

A single pure function in `envspec` produces the environment for every child process.
Layers, in order, each overriding the previous:

1. The parent process environment.
2. Global environment variables.
3. Project environment variables.
4. Secret values fetched from the keychain for any variable marked secret.
5. Index configuration and credentials (§7).
6. `UV_PROJECT_ENVIRONMENT` set to the resolved venv path.
7. Pre-activate scripts, run in order; their exported variables are captured and merged.
8. `VIRTUAL_ENV` set, venv `bin`/`Scripts` prepended to `PATH`, `PYTHONHOME` removed.

Spawning a terminal appends one final step — launching the emulator with the project
directory as the working directory. Running a uv command does not. Otherwise the pipeline
is identical.

### Pre-activate scripts must be able to export

A script run as a subprocess cannot normally alter its parent's environment, which would
make the phrase "pre-activate script" a lie. Bombadil therefore runs each script under the
platform shell followed by an environment dump, diffs the result against the input
environment, and folds the delta into the composed environment. Supported on POSIX shells
and PowerShell. A script that exits non-zero aborts the operation and reports its output;
it does not partially apply.

### Venv path resolution

| Project setting | Resolves to |
| --- | --- |
| `Alongside` | `<project dir>/.venv` |
| `Custom(p)` | `p` |
| `Default` + settings `Alongside` | `<project dir>/.venv` |
| `Default` + settings `Central(c)` | `c/<project-name>-<hash>` |

`<hash>` is the first six hex characters of the SHA-256 of the canonicalised project
directory path. Two unrelated projects both named `api` therefore cannot collide, and the
name remains recognisable.

Pointing at a path that already contains a virtual environment **adopts** it: Bombadil reads
`pyvenv.cfg` for the interpreter version and runs a sync to reconcile it with the lockfile.
It never deletes or recreates without being asked. Pointing at a path that exists but is not
a virtual environment is refused with an explanation rather than written into.

## 7. uv integration

### Resolving which uv to run

`Settings.uv_source` is an explicit four-way user choice, resolved once at startup and
re-resolved when changed:

| Setting | Behaviour |
| --- | --- |
| `Auto` (default) | Use uv from `PATH` if present and at least `MIN_UV_VERSION`; otherwise the bundled copy. |
| `Bundled` | Always the embedded copy. Maximum determinism. |
| `FromPath` | Always the uv on `PATH`. Fails loudly with a clear message if absent or too old — it does not silently fall back. |
| `Custom(p)` | A specific binary. Fails loudly if missing or not executable. |

Settings displays the resolved path and version for the current selection, so the choice is
verifiable before it is committed.

`MIN_UV_VERSION` starts at **0.11.32**, the version whose CLI surface this design was
verified against. It is lowered only when an older version has been explicitly tested, and
raised whenever Bombadil adopts a newer flag. Backwards compatibility is never assumed.

Because the user may select a uv newer than the bundled one, **the uv in use is never
hidden**: it appears in the status bar, in Settings, and verbatim in the output drawer's
copyable failure report, so version skew is visible in the first screenshot anyone sends.

### Bundling

`bombadil-uv-bin`'s `build.rs` downloads the pinned uv release for the *target* triple
(not the host), verifies its SHA-256 against a manifest committed to the repository, and
embeds it. `BOMBADIL_UV_ARCHIVE` points at a pre-fetched archive for offline or air-gapped
builds. At runtime the binary is extracted once to
`<data dir>/uv/<version>-<sha256 prefix>/uv`, made executable, and reused; it is
re-extracted only if missing or if its hash no longer matches.

uv is Apache-2.0 OR MIT, compatible with this project's Apache-2.0. Its license text ships
with every distributed artifact.

### Command mapping

Every user action corresponds to a named uv invocation. All were verified against uv 0.11.32.

| Action | Command |
| --- | --- |
| Create venv | `uv sync -p <python>` |
| Adopt existing venv | `uv sync` with `UV_PROJECT_ENVIRONMENT` pointed at it |
| Sync | `uv sync` |
| Add dependency | `uv add <spec>`, plus `--dev`, `--group <g>` or `--optional <extra>` |
| Change constraint | `uv add <name><new-specifier>` |
| Remove dependency | `uv remove <name>` with the matching group flag |
| Upgrade one package | `uv lock -P <name>` then `uv sync` |
| Declared dependencies | parsed from `pyproject.toml` |
| Installed versions | `uv pip list --format json` |
| Transitive tree | `uv tree --format json --depth <n>` |
| Interpreter discovery | `uv python list --output-format json` |
| Install an interpreter | `uv python install <version>` |

### Private indexes

Indexes are injected entirely through the environment. **No project file is modified.**
This was verified empirically against uv 0.11.32 against a local HTTP server:

- An index with `kind: Default` is injected as `UV_DEFAULT_INDEX="<name>=<url>"`; indexes
  with `kind: Extra` are injected together as `UV_INDEX`, a space-separated list of
  `name=url` pairs. **A project may select at most one `Default` index**; selecting a
  second is rejected by validation rather than resolved arbitrarily.
- `UV_INDEX_<NAME>_USERNAME` and `UV_INDEX_<NAME>_PASSWORD`, where `<NAME>` is the index
  name uppercased, supply credentials. uv sends them **preemptively** as HTTP Basic — no
  401 challenge round-trip is required.
- Both mechanisms work with a `pyproject.toml` that mentions no index at all.

Token authentication is expressed as a username of `__token__` with the token as the
password, the convention PyPI-compatible registries use.

Credentials are read from the keychain at spawn time and exist only in the composed
environment map of the child process. They are never written to `config.toml`, never
logged, and are redacted in failure reports.

## 8. User interface

### Main window

A sidebar of registered projects on the left; a detail pane on the right with a header
(name, path, venv status, Python version, Sync action) and tabs: **Dependencies**,
**Env vars**, **Scripts**, **Indexes**. A collapsible drawer along the bottom streams live
uv output and reduces to a single status line when idle.

This layout was chosen because the dependency table is the widest and densest element in
the application and should receive the full remaining width, and because long-running uv
commands need a persistent place to stream output that is not a modal dialog.

Sidebar entries show a filled marker when the venv exists and a hollow one when the project
is registered without one, plus the Python version.

### Dependencies tab

Rows grouped by `main`, each dependency group, and each optional-dependencies extra.
Columns: package, declared constraint, installed version. Direct dependencies expand to
reveal their transitive tree. A per-row menu offers Upgrade, Change constraint and Remove.

### Add project dialog

A file picker for the `pyproject.toml`, showing the parsed project name and
`requires-python` once chosen. A three-way virtual environment control — use the settings
default, place it next to the `pyproject.toml`, or choose a folder — with the resolved path
always displayed. When the chosen folder already holds a virtual environment, the dialog
states that it will be adopted and shows its Python version. An interpreter selector,
pre-selected to satisfy `requires-python`, offers to install a suitable interpreter through
uv when none is available.

### Context menu

Right-clicking a project offers Open terminal (a submenu of terminals actually detected on
this machine, with the configured default first, plus a Custom entry), Open folder, Sync,
Recreate venv, and Remove from list.

### Terminal detection

Probed at startup, and only what is found is offered:

- **Windows** — Windows Terminal, PowerShell, cmd, Git Bash, WSL
- **macOS** — Terminal.app, iTerm2
- **Linux** — gnome-terminal, konsole, kitty, alacritty, wezterm, foot, xterm

An advanced setting accepts a custom command template with `{cwd}` and `{shell}`
placeholders for anything unrecognised.

## 9. Failure handling

Every uv invocation captures its argv, exit code, stdout and stderr. A non-zero exit shows
an inline banner in the affected pane, the full transcript in the drawer, and a **Copy
failure report** action emitting the command, exit code, uv version and origin, operating
system, and the environment keys applied — with values redacted for anything marked secret.

Specific cases:

| Situation | Behaviour |
| --- | --- |
| uv command fails | Banner plus full transcript. State is re-read from disk; nothing is assumed. |
| `pyproject.toml` changed externally | Re-read on window focus, on manual refresh, and before every mutating command. No file watcher in v1. |
| Chosen terminal has disappeared | Error naming the exact argv attempted. |
| Keychain locked or denied | Explicit message and the session-only fallback described in §5. |
| Long operation | Cancellable; cancelling kills the child process group. |
| Venv path is not a venv | Refused before any command runs, with the reason. |

## 10. Testing

**Unit tests in `bombadil-core`** — no network, no display, no keychain:

- Environment layering precedence, as a table of layered inputs and expected outputs.
- Venv path resolution, including central-store naming and hash collision avoidance.
- `pyproject.toml` parsing against fixtures covering groups, extras and `requires-python`.
- Config migration, atomic write, and the read-only path for a newer schema version.
- uv version parsing and comparison against `MIN_UV_VERSION`.
- **Terminal argv construction for all three operating systems on any one machine.** The
  target OS is a function parameter rather than a `cfg!`, so Windows terminal construction
  is covered by a test run on Linux.

Fakes: a `CommandRunner` returning canned output and exit codes, and an in-memory
`SecretStore`.

**Integration tests**, behind a feature flag, run real uv against a temporary project:
create a venv, add a dependency, assert `pyproject.toml` and the installed list, remove it,
sync. These run on all three platforms in CI.

**GUI tests** exercise `update()` — a pure function in iced — by feeding `Message`s and
asserting the resulting `State`. No renderer is required.

**CI** runs `cargo fmt --check`, `cargo clippy -- -D warnings`, and both test suites on
ubuntu-latest, macos-latest and windows-latest.

## 11. Distribution

One self-contained binary per platform, with the uv binary embedded. No system webview, no
GTK runtime, and no separate uv installation are required. `cargo install` produces a
working application because the uv acquisition happens in `build.rs`.
