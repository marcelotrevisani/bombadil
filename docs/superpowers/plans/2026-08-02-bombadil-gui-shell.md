# Bombadil GUI Shell Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A running Bombadil window that lists registered projects, shows one in detail, and runs uv commands without ever freezing.

**Architecture:** A new `bombadil-gui` crate depending on `bombadil-core`. All application logic lives in a pure `update` function and free helper functions that are unit-tested; `view` is deliberately dumb and untested. Every blocking call into `bombadil-core` goes through one `job` seam that runs it on a real thread with a timeout, because `CommandRunner::run` is synchronous and a hung probe would otherwise freeze startup.

**Tech Stack:** Rust edition 2024, `iced` 0.14, `bombadil-core`. No other new dependencies.

## Scope

This is plan 3a of three. It delivers the shell: window, state, project sidebar, detail header, output drawer, and the off-thread job seam. **Plan 3b** adds the add-project dialog, venv creation and adoption, and the context menu including terminals. **Plan 3c** adds the four tab editors, settings, and the monorepo member checklist. Each produces working software on its own.

## Global Constraints

- Rust edition 2024, `rust-version = "1.88"`.
- **`bombadil-core` must not depend on `iced`.** The dependency points one way: `bombadil-gui` → `bombadil-core`.
- **No new dependencies beyond `iced`.** If a task seems to need one, stop and escalate.
- **No test may open a window, spawn a real process, touch the real OS keychain, or reach the network.** `FakeCommandRunner` and `InMemorySecretStore` only.
- **A secret must never appear in a log line, a `Debug` render, or a failure report.** The composed environment contains keychain values and index passwords.
- **Never block the render thread.** Every call into `bombadil-core` that spawns a process goes through `job::run`.
- Clippy lints denied in CI: `derivable_impls`, `field_reassign_with_default`.
- `view` functions are not unit-tested. Logic that deserves a test belongs in `update` or a free function, not in `view`.

## Facts Verified Empirically Before This Plan

Checked by compiling a real spike against the real crates. Do not re-derive.

- **`iced` resolves to 0.14.0** and compiles on this machine in ~11s from a warm cache. The API is the functional form: `iced::application(new, update, view).title(..).run()`, with `update` returning `iced::Task<Message>`.
- **`Task::perform(async { .. }, Message::Variant)` compiles and is the way to get a value back into `update`.** iced's executor is not a blocking pool, so blocking work goes in `std::thread::spawn` and is collected with `mpsc::recv_timeout`, which supplies the timeout the hung-probe finding requires. The exact spike that compiled is reproduced in Task 2.
- **iced 0.14 pulls 223 crates and fails the project's existing `cargo deny` licence policy.** Exactly six crates are rejected: `arrayref` (BSD-2-Clause), `clipboard-win` and `error-code` (BSL-1.0), `foldhash` ×2 and `slotmap` (Zlib), `hexf-parse` (CC0-1.0). Every other apparent violation resolves through an `OR` branch already allowed.
- **The agreed policy change makes `cargo deny check licenses` pass** — verified against the real tree, not assumed. Zlib, BSL-1.0 and BSD-2-Clause join the global allow list; CC0-1.0 becomes a per-crate exception for `hexf-parse`, because CC0 grants no patent rights and some corporate policies reject it broadly.

## A Note On Task Detail

Tasks 1–3 give literal test code. **Tasks 4–6 give precise acceptance criteria instead**, and their implementers write the tests to those criteria following the fixtures established in Tasks 1–3. This is a deliberate departure from the usual no-placeholders rule, taken because the criteria here are behavioural and checkable (*"keeps the most recent lines and drops the oldest"*, *"names both index names"*, *"redacted on the way in, not in `view`"*) rather than vague.

It shifts risk onto review: a criterion can be satisfied by a test that passes for the wrong reason. Nine such tests were found across earlier plans, every one in a test specified up front. **Each of Tasks 4–6 must therefore carry a mutation proof** — break the production behaviour the criterion names, confirm the test fails, restore — and the task review must check the assertion discriminates rather than merely runs.

## Debt This Plan Must Pay

`validate::validate()` has had **no caller for four plans**. Its consequence is no longer cosmetic: two indexes whose names normalise to the same credential variable would send one registry's password to another's server. `inject` now refuses that case at the point of harm, but every *other* problem `validate` detects — malformed index names, duplicate project ids, bad URLs — still reaches the user as a confusing failure rather than a clear message. Task 6 wires it.

---

### Task 1: The crate, the licence policy, and a window that opens

**Files:**
- Create: `crates/bombadil-gui/Cargo.toml`, `crates/bombadil-gui/src/main.rs`, `crates/bombadil-gui/src/app.rs`
- Modify: `Cargo.toml`, `deny.toml`, `.github/workflows/ci.yml`

**Interfaces:**
- Produces: `app::App`, `app::Message`, `app::update`, `app::view`

- [ ] **Step 1: Wire the workspace and the licence policy**

Add `crates/bombadil-gui` to `[workspace] members`. In `crates/bombadil-gui/Cargo.toml`, depend on `bombadil-core` by path and `iced = "0.14"`.

In `deny.toml`, extend the allow list and add one exception. **This is the only permitted `deny.toml` change in this plan** — any further licence failure is an escalation, not an edit:

```toml
allow = ["Apache-2.0", "MIT", "Unicode-3.0", "ISC", "CDLA-Permissive-2.0", "BSD-3-Clause", "Zlib", "BSL-1.0", "BSD-2-Clause"]
```

and, in `exceptions`, above the existing `option-ext` entry:

```toml
    # hexf-parse is CC0-1.0, reached through iced's font stack. CC0 is a
    # public-domain dedication that explicitly grants no patent rights, which
    # is why it is scoped to this one crate rather than allowed globally.
    { allow = ["CC0-1.0"], crate = "hexf-parse" },
```

In `.github/workflows/ci.yml`, the `check` job builds the whole workspace, so `bombadil-gui` is covered automatically — **but a GUI crate needs system libraries on Linux runners.** Add to the `check` job, before the build steps, guarded to Linux only:

```yaml
      - name: Install GUI system dependencies
        if: runner.os == 'Linux'
        run: sudo apt-get update && sudo apt-get install -y libxkbcommon-dev libwayland-dev
```

If the Linux job fails on a missing library not listed here, add it — that is a build requirement, not a workaround.

- [ ] **Step 2: Write the failing test**

Create `crates/bombadil-gui/src/app.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bombadil_core::model::Config;

    #[test]
    fn an_empty_config_produces_no_selection() {
        // Selecting a project that does not exist would panic in `view` on the
        // first frame, so the empty case has to be representable.
        let app = App::new(Config::default());
        assert!(app.selected.is_none());
        assert!(app.projects.is_empty());
    }

    #[test]
    fn the_first_project_is_selected_on_load() {
        let config = Config {
            projects: vec![project("api"), project("web")],
            ..Config::default()
        };
        let app = App::new(config);
        assert_eq!(app.selected_project().map(|p| p.label.as_str()), Some("api"));
    }

    #[test]
    fn selecting_a_project_changes_the_selection() {
        let config = Config {
            projects: vec![project("api"), project("web")],
            ..Config::default()
        };
        let mut app = App::new(config);
        let _ = update(&mut app, Message::ProjectSelected(1));
        assert_eq!(app.selected_project().map(|p| p.label.as_str()), Some("web"));
    }

    #[test]
    fn selecting_out_of_range_is_ignored_rather_than_panicking() {
        // A stale index can arrive from a message queued before a project was
        // removed. Panicking there would take the window down.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let _ = update(&mut app, Message::ProjectSelected(99));
        assert_eq!(app.selected_project().map(|p| p.label.as_str()), Some("api"));
    }

    fn project(label: &str) -> bombadil_core::model::Project {
        bombadil_core::model::Project {
            id: uuid::Uuid::new_v4(),
            label: label.into(),
            pyproject_path: std::path::PathBuf::from(format!("/p/{label}/pyproject.toml")),
            index_names: vec![],
            venv: bombadil_core::model::VenvLocation::Alongside,
            env: vec![],
            scripts: vec![],
            members: Default::default(),
        }
    }
}
```

`uuid` is already a dependency of `bombadil-core`; add it to `bombadil-gui`'s dev-dependencies if the test needs it directly, and say so in your report — that is the one dependency addition this task may make without escalating.

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p bombadil-gui`
Expected: FAIL — `cannot find type App`.

- [ ] **Step 4: Implement**

```rust
use bombadil_core::model::{Config, Project};
use iced::Task;

/// Everything the window shows. Deliberately plain data: `update` is the only
/// place it changes, which is what makes the logic testable without a display.
pub struct App {
    pub config: Config,
    pub projects: Vec<Project>,
    pub selected: Option<usize>,
}

#[derive(Debug, Clone)]
pub enum Message {
    ProjectSelected(usize),
}

impl App {
    pub fn new(config: Config) -> Self {
        let projects = config.projects.clone();
        let selected = (!projects.is_empty()).then_some(0);
        Self { config, projects, selected }
    }

    pub fn selected_project(&self) -> Option<&Project> {
        self.selected.and_then(|i| self.projects.get(i))
    }
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        // A stale index can arrive from a message queued before a project was
        // removed; ignoring it is better than taking the window down.
        Message::ProjectSelected(i) if i < app.projects.len() => {
            app.selected = Some(i);
        }
        Message::ProjectSelected(_) => {}
    }
    Task::none()
}

pub fn view(app: &App) -> iced::Element<'_, Message> {
    let mut list = iced::widget::column![].spacing(4);
    for (i, project) in app.projects.iter().enumerate() {
        list = list.push(
            iced::widget::button(iced::widget::text(project.label.clone()))
                .on_press(Message::ProjectSelected(i))
                .width(iced::Length::Fill),
        );
    }
    iced::widget::row![
        iced::widget::container(list).width(240),
        iced::widget::container(iced::widget::text(
            app.selected_project().map(|p| p.label.clone()).unwrap_or_default()
        ))
        .padding(12),
    ]
    .into()
}
```

`main.rs` loads the config through `bombadil_core::store` and runs the application. If loading fails, start with `Config::default()` rather than refusing to open — a corrupt config must not make the app unlaunchable. Note that decision in a comment.

- [ ] **Step 5: Verify and commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --workspace --locked && cargo deny check
git add crates/bombadil-gui Cargo.toml Cargo.lock deny.toml .github/workflows/ci.yml
git commit -m "feat(gui): add the bombadil-gui crate and a window that lists projects

iced 0.14 pulls 223 crates and six fail the existing licence policy. Zlib,
BSL-1.0 and BSD-2-Clause join the allow list; CC0-1.0 is scoped to hexf-parse
alone because it grants no patent rights.

A corrupt config starts an empty app rather than refusing to open."
```

---

### Task 2: The job seam — never block the render thread

**Files:**
- Create: `crates/bombadil-gui/src/job.rs`
- Modify: `crates/bombadil-gui/src/app.rs`

**Interfaces:**
- Produces: `job::run<T>(work, timeout) -> impl Future<Output = JobResult<T>>`, `job::JobResult<T>`

> **Why this exists.** `CommandRunner::run` is synchronous. `SystemCommandRunner` calls `Command::output()`, which blocks until the child exits. Plan 2c's review found that terminal detection fires up to seven probes at startup with nothing bounding them — a `which` walking a stale network mount, or a slow `open -Ra` waiting on LaunchServices, blocks forever, which the user experiences as "Bombadil never opens".
>
> The fix belongs here, not in `CommandRunner`: teaching the trait about timeouts would ripple into uv resolution and every other caller for a risk specific to eagerly probing at startup.
>
> **Verified by compiling against real iced 0.14:** `Task::perform` accepts an `async` block, and blocking work inside it must go to a real thread — iced's executor is not a blocking pool.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn work_that_finishes_returns_its_value() {
        let got = pollster_block(run(|| 21 * 2, Duration::from_secs(5)));
        assert_eq!(got, JobResult::Done(42));
    }

    #[test]
    fn work_that_hangs_times_out_rather_than_blocking_forever() {
        // This is the whole point: a probe walking a stale network mount must
        // degrade to a timeout, not freeze the window.
        let got: JobResult<()> = pollster_block(run(
            || std::thread::sleep(Duration::from_secs(30)),
            Duration::from_millis(50),
        ));
        assert_eq!(got, JobResult::TimedOut);
    }

    #[test]
    fn work_that_panics_is_reported_rather_than_killing_the_app() {
        // A panic inside a worker thread must not poison the UI. The user gets
        // a failure they can see instead of a window that stops updating.
        let got: JobResult<()> = pollster_block(run(
            || panic!("boom"),
            Duration::from_secs(5),
        ));
        assert_eq!(got, JobResult::Failed);
    }

    #[test]
    fn a_timed_out_job_does_not_block_the_caller_for_the_works_duration() {
        let started = std::time::Instant::now();
        let _: JobResult<()> = pollster_block(run(
            || std::thread::sleep(Duration::from_secs(30)),
            Duration::from_millis(50),
        ));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "returned after {:?}; the timeout did not bound the wait",
            started.elapsed()
        );
    }

    /// Drive a future to completion on this thread. `job::run`'s future does
    /// its waiting on a channel, so a trivial executor is enough and avoids
    /// pulling in a runtime.
    fn pollster_block<F: std::future::Future>(mut f: F) -> F::Output {
        use std::task::{Context, Poll, Waker};
        let mut f = unsafe { std::pin::Pin::new_unchecked(&mut f) };
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
            std::thread::yield_now();
        }
    }
}
```

If `Waker::noop()` is unstable on 1.88, build a no-op waker by hand with `RawWaker`/`RawWakerVTable` and say so in your report. Do not add a dependency for this.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p bombadil-gui job`
Expected: FAIL — `cannot find function run`.

- [ ] **Step 3: Implement**

```rust
use std::sync::mpsc;
use std::time::Duration;

/// What became of a piece of background work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobResult<T> {
    Done(T),
    /// The work exceeded its budget. The thread is left running — it cannot be
    /// killed safely — but the UI stops waiting on it.
    TimedOut,
    /// The work panicked. Reported rather than propagated so one bad call
    /// cannot take the window down.
    Failed,
}

/// Run blocking work off the render thread, bounded by `timeout`.
///
/// `CommandRunner::run` is synchronous and `SystemCommandRunner` blocks until
/// the child exits, so calling it from `update` would freeze the window. The
/// timeout exists because startup probes can hang indefinitely on a stale
/// network mount; a probe that never answers must degrade to "absent" rather
/// than making the application appear dead.
///
/// A timed-out thread is abandoned, not killed: there is no safe way to stop
/// it, and leaking one thread beats leaving the UI wedged.
pub async fn run<T, F>(work: F, timeout: Duration) -> JobResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
        let _ = tx.send(outcome);
    });

    match rx.recv_timeout(timeout) {
        Ok(Ok(value)) => JobResult::Done(value),
        Ok(Err(_)) => JobResult::Failed,
        Err(mpsc::RecvTimeoutError::Timeout) => JobResult::TimedOut,
        Err(mpsc::RecvTimeoutError::Disconnected) => JobResult::Failed,
    }
}
```

`recv_timeout` blocks the calling thread, which inside `Task::perform` is an executor thread rather than the render thread. That is acceptable for this application's handful of concurrent jobs and is the reason no async runtime is needed. **State this tradeoff in the module doc** so the next person does not mistake it for an oversight.

- [ ] **Step 4: Verify and commit**

```bash
cargo test --workspace --locked
git add crates/bombadil-gui/src
git commit -m "feat(gui): run blocking core calls off the render thread with a timeout

CommandRunner::run is synchronous and SystemCommandRunner blocks until the
child exits, so calling it from update would freeze the window. Terminal
detection fires up to seven probes at startup with nothing bounding them; a
which walking a stale network mount would otherwise make the app appear dead.

A timed-out thread is abandoned rather than killed -- leaking one thread
beats leaving the UI wedged."
```

---

### Task 3: The project sidebar

**Files:**
- Create: `crates/bombadil-gui/src/sidebar.rs`
- Modify: `crates/bombadil-gui/src/app.rs`

**Interfaces:**
- Consumes: `bombadil_core::venv_path::resolve`, `model::Project`.
- Produces: `sidebar::Entry { label, venv_exists, python_version }`, `sidebar::entries(config, venv_probe) -> Vec<Entry>`, `sidebar::view(entries, selected)`

> Spec §8: sidebar entries show **a filled marker when the venv exists and a hollow one when the project is registered without one**, plus the Python version. `venv_probe` is a parameter rather than a filesystem call so this is testable without touching disk.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_with_a_venv_is_marked_filled() {
        let entries = entries(&config_with(&["api"]), &|_| Some("3.12.1".into()));
        assert_eq!(entries.len(), 1);
        assert!(entries[0].venv_exists);
        assert_eq!(entries[0].python_version.as_deref(), Some("3.12.1"));
    }

    #[test]
    fn a_project_without_a_venv_is_marked_hollow_and_still_listed() {
        // Registered-without-a-venv is a normal state, not an error: the user
        // adds the project first and creates the environment afterwards.
        let entries = entries(&config_with(&["api"]), &|_| None);
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].venv_exists);
        assert_eq!(entries[0].python_version, None);
    }

    #[test]
    fn entries_follow_config_order_so_the_list_does_not_reshuffle() {
        let entries = entries(&config_with(&["web", "api"]), &|_| None);
        let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, vec!["web", "api"]);
    }

    #[test]
    fn a_project_whose_venv_path_cannot_be_resolved_is_listed_as_hollow() {
        // venv_path::resolve fails on a pyproject path with no parent. Dropping
        // the project from the list would make it unrecoverable through the UI.
        let mut config = config_with(&["api"]);
        config.projects[0].pyproject_path = std::path::PathBuf::from("pyproject.toml");
        let entries = entries(&config, &|_| Some("3.12.1".into()));
        assert_eq!(entries.len(), 1, "the project must still be listed");
        assert!(!entries[0].venv_exists);
    }

    fn config_with(labels: &[&str]) -> bombadil_core::model::Config { /* fixture */ }
}
```

Write the fixture following `app.rs`'s existing one.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p bombadil-gui sidebar`
Expected: FAIL — `cannot find function entries`.

- [ ] **Step 3: Implement**

`entries` maps each project through `venv_path::resolve`, then asks `venv_probe` for the interpreter version at that path. A project whose path cannot be resolved is listed hollow rather than dropped. `view` renders a filled or hollow marker, the label, and the version when known.

- [ ] **Step 4: Verify and commit**

```bash
cargo test --workspace --locked
git add crates/bombadil-gui/src
git commit -m "feat(gui): list projects with venv status and Python version

A project whose venv path cannot be resolved is listed hollow rather than
dropped -- dropping it would make it unrecoverable through the UI."
```

---

### Task 4: The detail header

**Files:**
- Create: `crates/bombadil-gui/src/detail.rs`
- Modify: `crates/bombadil-gui/src/app.rs`

**Interfaces:**
- Produces: `detail::Header { label, path, venv_status, python_version }`, `detail::header(project, venv_probe)`, `detail::view(header)`

> Spec §8: the header carries name, path, venv status, Python version and a Sync action. Sync is dispatched through `job::run` — never called directly from `update`.

- [ ] **Step 1: Write the failing tests**

Cover: a project with a venv shows its version; a project without one shows a "no environment" status and the Sync action is still offered (creating it is how the user recovers); the displayed path is the **project directory**, not the `pyproject.toml` file, because that is what a user recognises; and a Sync message returns a `Task` rather than performing work inline.

Write each as real code following the fixtures already in this crate.

- [ ] **Step 2–4:** run, implement, commit as in earlier tasks.

---

### Task 5: The output drawer

**Files:**
- Create: `crates/bombadil-gui/src/drawer.rs`
- Modify: `crates/bombadil-gui/src/app.rs`

**Interfaces:**
- Produces: `drawer::Drawer { lines, expanded, status }`, `drawer::push_line`, `drawer::view`

> Spec §8: a collapsible drawer streams live uv output and **reduces to a single status line when idle**. Spec §9: every uv invocation captures argv, exit code, stdout and stderr, and a failure shows a banner plus the full transcript.

- [ ] **Step 1: Write the failing tests**

Cover:

- Lines accumulate in order.
- **The buffer is bounded.** A `uv sync` with a large resolution can emit thousands of lines; an unbounded `Vec<String>` grows until the app is killed. Assert that pushing more than the cap keeps the *most recent* lines and drops the oldest — a test asserting only the length would pass against an implementation that dropped the newest, which is the wrong end.
- Idle collapses to one status line.
- **A secret never reaches the drawer.** The composed environment carries keychain values; a failure report is exactly where one would surface. Assert that a line containing a known secret value is redacted before it is stored — and that the redaction happens on the way *in*, not in `view`, so nothing can bypass it by reading `lines` directly.

- [ ] **Step 2–4:** run, implement, commit as in earlier tasks.

---

### Task 6: Wire `validate()` — four plans overdue

**Files:**
- Modify: `crates/bombadil-gui/src/app.rs`, `crates/bombadil-gui/src/banner.rs` (create)

**Interfaces:**
- Consumes: `bombadil_core::validate::validate`.
- Produces: `banner::Problems`, surfaced in `view`.

> `validate()` has had no caller since plan 1. Its most dangerous consequence — two index names normalising to one credential variable, sending one registry's password to another — is now refused by `inject` at the point of harm. Everything else it detects still reaches the user as a confusing failure instead of a clear message.

- [ ] **Step 1: Write the failing tests**

Cover:

- A clean config produces no problems and no banner.
- A config with two indexes whose names normalise to the same variable produces a problem naming **both** index names.
- Problems are recomputed after a config change, not only at startup — a test that mutates the config and re-runs must see the new problem appear, and an old one disappear once fixed.
- **The banner never renders a secret.** `Problem` carries messages built from config values; assert a config whose index name is unremarkable but whose *stored secret* is distinctive produces no banner text containing that secret.

- [ ] **Step 2–4:** run, implement, commit as in earlier tasks.

---

## Done when

- `cargo test --workspace --locked` passes on Linux, macOS and Windows.
- `cargo clippy --all-targets --all-features --locked -- -D warnings` and `cargo deny check` are clean.
- The `msrv` job at 1.88.0 passes.
- `bombadil-gui` builds on all three CI runners.
- No test opens a window, spawns a real process, touches the keychain, or reaches the network.
- Every call into `bombadil-core` that spawns a process goes through `job::run`.

## What the remaining plans pick up

**Plan 3b (project lifecycle)** adds the add-project dialog, venv creation and adoption, interpreter selection, and the context menu. It consumes `terminal::detect::available` for the terminal submenu and `terminal::open::open` for the action — both through `job::run`, since detection is the exact case the timeout exists for. It must surface `TerminalKind::activation_support()`: six of fourteen terminals degrade, and three deliver **no** composed environment at all, so a user picking Terminal.app on macOS needs telling rather than a silently unactivated shell.

**Plan 3c (editors)** adds the Dependencies, Env vars, Scripts and Indexes tabs, settings, and the monorepo member checklist. It must join installed-editable state on **path**, not name — the path is exact and survives members with no `[project].name`.

Two Windows items recorded in `bombadil-core` as `TODO(windows)` remain open and are not this plan's to fix: Git Bash detection is likely inverted (WSL's `bash.exe` is on `PATH`, Git for Windows' is not), and a Windows venv path is unusable inside a Git Bash `PATH` because the drive-letter colon is that shell's separator.
