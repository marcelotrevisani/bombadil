use crate::banner;
use crate::detail;
use crate::drawer;
use crate::job;
use crate::sidebar;
use bombadil_core::model::{Config, Project, Settings};
use bombadil_core::process::CommandRunner;
use bombadil_core::uv;
use bombadil_core::venv_path;
use iced::Task;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// How long a venv probe (a `pyvenv.cfg` read) gets before the sidebar shows
/// that project as hollow instead of waiting on it. Reading a small local
/// file is normally sub-millisecond; a few seconds already covers a slow
/// disk, and nothing here is worth making the user stare at a frozen sidebar
/// for longer than that.
const VENV_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a `uv sync` gets before it is treated as failed. Sync can
/// download and build packages, unlike the sub-millisecond `pyvenv.cfg`
/// read `VENV_PROBE_TIMEOUT` bounds, so this is generous rather than tight.
const SYNC_TIMEOUT: Duration = Duration::from_secs(300);

/// Everything the window shows. Deliberately plain data: `update` is the only
/// place it changes, which is what makes the logic testable without a display.
pub struct App {
    // `projects`, `selected` and `entries` are what most of `view` and
    // `update` need; `config` as a whole is also read directly by `view`,
    // which recomputes `banner::problems(&app.config)` on every render
    // rather than caching it on this struct -- see `banner`'s module doc.
    // Kept whole rather than trimmed to just the projects, because
    // settings/index/save features in later tasks need the rest of the
    // config and re-deriving `App` then would be needless churn.
    pub config: Config,
    pub projects: Vec<Project>,
    pub selected: Option<usize>,
    /// One row per project, in `sidebar::entries` order. Starts all-hollow;
    /// `boot`'s probe tasks fill in real venv status as they complete.
    pub entries: Vec<sidebar::Entry>,
    /// The output drawer for the most recent (or in-flight) Sync. Spec §8/§9:
    /// streams a running sync's output and collapses to a status line once
    /// it finishes.
    pub drawer: drawer::Drawer,
    /// The id of the project whose `uv sync` is in flight, if any.
    ///
    /// Spec line 121 requires at most one mutating uv command per project.
    /// A single slot rather than a set per project is deliberate and is
    /// slightly stricter than the spec: there is one shared output drawer,
    /// so a second concurrent sync -- even for a different project -- would
    /// interleave its transcript into the first one's. One at a time is what
    /// the UI can actually represent.
    ///
    /// A `String` rather than a `Uuid` because `uuid` is a dev-dependency of
    /// this crate, not a dependency, and adding one for an equality check is
    /// not worth it.
    pub syncing: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    ProjectSelected(usize),
    /// A background venv probe finished for `projects[usize]`. Carries the
    /// Python version on success; `None` covers "no venv", a probe that
    /// timed out, and a probe that failed -- all three degrade to hollow.
    VenvProbed(usize, Option<String>),
    /// The Sync action was pressed for the selected project. Never gated on
    /// whether a venv already exists -- creating one is how the user
    /// recovers from not having one.
    SyncRequested,
    /// A background sync finished. `success` is true only on a zero exit;
    /// `lines` is the transcript to hand to the output drawer -- uv's stdout
    /// and stderr on a real run, or a single line describing a failed
    /// resolve, a timeout, or a panicking job when there is no process
    /// output to show instead.
    SyncFinished {
        success: bool,
        lines: Vec<String>,
    },
}

impl App {
    pub fn new(config: Config) -> Self {
        let projects = config.projects.clone();
        let selected = (!projects.is_empty()).then_some(0);
        // No probe yet at construction time -- every project starts hollow;
        // `boot` kicks off the real probes and `update` fills results in as
        // they land.
        let entries = sidebar::entries(&config, &|_path| None);
        Self {
            config,
            projects,
            selected,
            entries,
            drawer: drawer::Drawer::default(),
            syncing: None,
        }
    }

    pub fn selected_project(&self) -> Option<&Project> {
        self.selected.and_then(|i| self.projects.get(i))
    }
}

/// The real boot function iced calls: builds the initial `App` plus one
/// background task per project to fill in its venv status.
///
/// Wired through `job::run`, not called inline -- reading `pyvenv.cfg` can
/// hang on a stale network mount, and doing that at startup on the render
/// thread would freeze the window before it ever appears.
pub fn boot(config: Config) -> (App, Task<Message>) {
    let app = App::new(config);
    let probes = app
        .projects
        .iter()
        .enumerate()
        .filter_map(|(i, project)| {
            let path = venv_path::resolve(project, &app.config.settings).ok()?;
            Some(Task::perform(probe_venv(path), move |version| {
                Message::VenvProbed(i, version)
            }))
        })
        .collect::<Vec<_>>();
    (app, Task::batch(probes))
}

/// Runs the blocking `pyvenv.cfg` read off the render thread and collapses
/// every non-success outcome to `None` -- a timed-out or panicking probe is
/// exactly as hollow to the sidebar as a project with no venv at all.
async fn probe_venv(path: PathBuf) -> Option<String> {
    match job::run(move || venv_path::probe(&path), VENV_PROBE_TIMEOUT).await {
        job::JobResult::Done(version) => version,
        job::JobResult::TimedOut | job::JobResult::Failed => None,
    }
}

/// `runner` is a parameter rather than a field on `App` so a test can hand in
/// a `FakeCommandRunner` and observe that `Message::SyncRequested` never
/// calls it synchronously -- the exact freeze `job::run` exists to prevent.
/// The real caller (`main.rs`) closes over a `SystemCommandRunner`, cloning
/// the `Arc` for each call.
pub fn update(app: &mut App, message: Message, runner: Arc<dyn CommandRunner>) -> Task<Message> {
    match message {
        // A stale index can arrive from a message queued before a project was
        // removed; ignoring it is better than taking the window down.
        Message::ProjectSelected(i) if i < app.projects.len() => {
            app.selected = Some(i);
        }
        Message::ProjectSelected(_) => {}
        Message::VenvProbed(i, version) if i < app.entries.len() => {
            app.entries[i].venv_exists = version.is_some();
            app.entries[i].python_version = version;
        }
        // A stale probe result for a project that no longer exists.
        Message::VenvProbed(_, _) => {}
        // A sync already in flight swallows the press. Without this the
        // button dispatches unconditionally, and a 300-second timeout makes
        // two concurrent `uv sync` processes on the same project easy to
        // reach: the job times out, the user presses Sync again, and the
        // abandoned child is still running.
        Message::SyncRequested if app.syncing.is_some() => {}
        Message::SyncRequested => {
            if let Some(project) = app.selected_project().cloned() {
                let settings = app.config.settings.clone();
                app.syncing = Some(project.id.to_string());
                // Fresh drawer per run, but showing a *running* status rather
                // than the default "idle": for the whole duration of the sync
                // this collapsed line is all the user has, and "idle" makes a
                // running sync look like a button that did nothing.
                app.drawer = drawer::Drawer {
                    status: format!("syncing {}...", project.label),
                    ..drawer::Drawer::default()
                };
                return Task::perform(
                    sync_project(project, settings, runner),
                    |(success, lines)| Message::SyncFinished { success, lines },
                );
            }
        }
        Message::SyncFinished { success, lines } => {
            app.syncing = None;
            // Already redacted on the job thread, where the composed
            // environment (and therefore the set of secret values) exists.
            // It cannot be done here: threading a secrets list through
            // `Message` would make a `Debug`-deriving type a carrier of
            // keychain material. `push_line` redacts again with an empty set,
            // which is a no-op -- kept so nothing that reaches the drawer by
            // some other route can skip redaction.
            for line in &lines {
                drawer::push_line(&mut app.drawer, line, &[]);
            }
            let status = if success {
                "sync succeeded".to_string()
            } else {
                "sync failed".to_string()
            };
            drawer::set_idle(&mut app.drawer, status);
            // Collapsing is right on success -- spec §8 asks for exactly that
            // status line. On failure spec §9 asks for a banner *plus the
            // full transcript*, and `set_idle` has just collapsed it: without
            // re-expanding here, iced only ever renders after `update`
            // returns, so `expanded` is never observably true and the user
            // sees "sync failed" with no exit code and no reason. Done at
            // the call site rather than inside `set_idle`, which means one
            // thing and should keep meaning it.
            if !success && !app.drawer.lines.is_empty() {
                app.drawer.expanded = true;
            }
        }
    }
    Task::none()
}

/// Runs off the render thread via `job::run` -- resolving uv and running
/// `uv sync` both spawn a process, and `CommandRunner::run` blocks until it
/// exits.
async fn sync_project(
    project: Project,
    settings: Settings,
    runner: Arc<dyn CommandRunner>,
) -> (bool, Vec<String>) {
    match job::run(
        move || run_sync(&project, &settings, runner.as_ref()),
        SYNC_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(result) => result,
        job::JobResult::TimedOut => (false, vec!["sync timed out".to_string()]),
        job::JobResult::Failed => (false, vec!["sync failed unexpectedly".to_string()]),
    }
}

/// The blocking half of a sync: resolve which uv to run, build the `uv sync`
/// spec (`bombadil_core::uv::command::sync`), and run it through `runner`.
/// Any resolution failure -- no project directory, no usable uv -- is
/// reported as `false` plus a one-line explanation rather than panicking,
/// same as `venv_path::probe`'s approach to a path that turns out not to be
/// what was expected. A real run's `lines` is uv's stdout followed by its
/// stderr, in the order `CommandOutcome` carries them.
fn run_sync(
    project: &Project,
    settings: &Settings,
    runner: &dyn CommandRunner,
) -> (bool, Vec<String>) {
    let Some(project_dir) = project
        .pyproject_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    else {
        return (false, vec!["no project directory to run uv in".to_string()]);
    };
    let Ok(venv_path) = venv_path::resolve(project, settings) else {
        return (
            false,
            vec!["could not resolve a virtual environment path".to_string()],
        );
    };

    // bombadil-gui does not yet bundle uv (no dependency on
    // bombadil-uv-bin), so `Auto`/`Bundled` sources can only ever resolve
    // through PATH here. Documented gap, not a silent one -- filling it in
    // is follow-up work once the GUI wires the bundled binary at all.
    let bundled = || -> Result<PathBuf, String> {
        Err("bombadil-gui does not bundle uv yet; install uv on PATH".to_string())
    };
    let Ok(resolved_uv) = uv::resolve(&settings.uv_source, runner, &uv::locate_on_path, &bundled)
    else {
        return (false, vec!["could not resolve a uv binary".to_string()]);
    };

    let ctx = uv::command::ProjectContext {
        project_dir: project_dir.to_path_buf(),
        venv_path,
    };
    let spec = uv::command::sync(&resolved_uv, &ctx);
    match runner.run(&spec) {
        Ok(outcome) => {
            let mut lines: Vec<String> = outcome.stdout.lines().map(str::to_string).collect();
            lines.extend(outcome.stderr.lines().map(str::to_string));
            (outcome.success(), lines)
        }
        Err(err) => (false, vec![err.to_string()]),
    }
}

pub fn view(app: &App) -> iced::Element<'_, Message> {
    let detail: iced::Element<'_, Message> = match app.selected_project() {
        // The probe answers from the already-probed `entries` cache rather
        // than touching disk again on every frame -- `boot`'s background
        // tasks are what keep that cache current.
        Some(project) => {
            let entry = app.selected.and_then(|i| app.entries.get(i));
            let header = detail::header(project, &|_| entry.and_then(|e| e.python_version.clone()));
            detail::view(&header)
        }
        None => iced::widget::text("").into(),
    };

    let detail_and_drawer = iced::widget::column![detail, drawer::view(&app.drawer)].spacing(8);

    let body = iced::widget::row![
        iced::widget::container(sidebar::view(&app.entries, app.selected)).width(240),
        iced::widget::container(detail_and_drawer).padding(12),
    ];

    // Recomputed from `app.config` on every call, not cached anywhere on
    // `App` -- see `banner`'s module doc for why that is what makes this
    // never go stale after a config change.
    iced::widget::column![banner::view(&banner::problems(&app.config)), body].into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bombadil_core::model::Config;
    use bombadil_core::process::FakeCommandRunner;

    /// A runner for tests that don't care about it -- most of them, since
    /// most messages here never touch a `CommandRunner` at all.
    fn unused_runner() -> Arc<dyn CommandRunner> {
        Arc::new(FakeCommandRunner::new())
    }

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
        assert_eq!(
            app.selected_project().map(|p| p.label.as_str()),
            Some("api")
        );
    }

    #[test]
    fn selecting_a_project_changes_the_selection() {
        let config = Config {
            projects: vec![project("api"), project("web")],
            ..Config::default()
        };
        let mut app = App::new(config);
        let _ = update(&mut app, Message::ProjectSelected(1), unused_runner());
        assert_eq!(
            app.selected_project().map(|p| p.label.as_str()),
            Some("web")
        );
    }

    #[test]
    fn selecting_out_of_range_is_ignored_rather_than_clamped() {
        // A stale index can arrive from a message queued before a project was
        // removed. Two projects with the FIRST selected is what separates
        // "ignored" from "clamped to the last valid index" -- with one project
        // both behaviours produce the same answer and the test proves nothing.
        let config = Config {
            projects: vec![project("api"), project("web")],
            ..Config::default()
        };
        let mut app = App::new(config);
        let _ = update(&mut app, Message::ProjectSelected(99), unused_runner());
        assert_eq!(
            app.selected_project().map(|p| p.label.as_str()),
            Some("api"),
            "an out-of-range selection must be ignored, not clamped to the last project"
        );
    }

    #[test]
    fn a_sync_message_never_calls_the_runner_synchronously() {
        // Task is opaque, so "it returned a Task" alone proves little -- a
        // buggy `update` could call `runner.run` directly and still return
        // one. The meaningful assertion is that the runner recorded nothing
        // by the time `update` has already returned: the exact freeze
        // `job::run` exists to prevent would show up here as a synchronous
        // call landing in `fake.calls()` before the async task ever runs.
        let config = Config {
            projects: vec![project("api")],
            ..Config::default()
        };
        let mut app = App::new(config);
        let fake = Arc::new(FakeCommandRunner::new());
        let runner: Arc<dyn CommandRunner> = fake.clone();

        let task = update(&mut app, Message::SyncRequested, runner);

        assert!(
            fake.calls().is_empty(),
            "runner.run must not be called synchronously inside update; got {:?}",
            fake.calls()
        );
        assert!(
            task.units() > 0,
            "a Sync message must dispatch real background work, not Task::none()"
        );
    }

    #[test]
    fn a_project_without_a_venv_still_dispatches_sync() {
        // Registered-without-a-venv is a normal state (see `sidebar`);
        // gating Sync on the venv already existing would strand the exact
        // user who needs it to create one.
        let config = Config {
            projects: vec![project("api")],
            ..Config::default()
        };
        let mut app = App::new(config);
        assert!(
            !app.entries[0].venv_exists,
            "a freshly constructed App has not been probed yet -- entries start hollow"
        );

        let task = update(&mut app, Message::SyncRequested, unused_runner());

        assert!(
            task.units() > 0,
            "Sync must still dispatch work for a project with no venv"
        );
    }

    #[test]
    fn a_failed_sync_keeps_the_transcript_visible() {
        // `push_line` expands the drawer and `set_idle` collapses it, both
        // inside one `update`. iced renders only after `update` returns, so
        // without re-expanding on failure `expanded` is never observably
        // true and the user sees "sync failed" with no exit code and no
        // reason -- spec §9 asks for a banner plus the full transcript.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::SyncFinished {
                success: false,
                lines: vec![
                    "Resolved 12 packages".to_string(),
                    "error: no solution found".to_string(),
                ],
            },
            unused_runner(),
        );

        assert!(
            app.drawer.expanded,
            "a failed sync must leave the transcript on screen; drawer was collapsed showing only {:?}",
            app.drawer.status
        );
        assert!(
            !app.drawer.lines.is_empty(),
            "the transcript must survive the collapse-to-status step"
        );
        assert!(
            app.drawer
                .lines
                .iter()
                .any(|l| l.contains("no solution found")),
            "the actionable line must be among them; got {:?}",
            app.drawer.lines
        );
    }

    #[test]
    fn a_successful_sync_collapses_to_a_status_line() {
        // The other half: collapsing IS right on success (spec §8), so the
        // failure fix must not turn into "always expanded".
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::SyncFinished {
                success: true,
                lines: vec!["Installed 12 packages".to_string()],
            },
            unused_runner(),
        );

        assert!(!app.drawer.expanded, "a successful sync must collapse");
        assert_eq!(app.drawer.status, "sync succeeded");
    }

    #[test]
    fn a_sync_in_flight_shows_a_running_status_not_idle() {
        // For the whole duration of the sync this collapsed line is all the
        // user has. Leaving it at the default "idle" makes a running sync
        // indistinguishable from a button that did nothing.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let _ = update(&mut app, Message::SyncRequested, unused_runner());

        assert!(
            app.drawer.status.contains("syncing"),
            "a dispatched sync must show a running status; got {:?}",
            app.drawer.status
        );
        assert_ne!(app.drawer.status, "idle");
    }

    #[test]
    fn a_second_sync_while_one_is_in_flight_dispatches_nothing() {
        // Spec line 121: at most one mutating uv command per project at a
        // time. With a 300-second timeout this is easy to hit -- the job
        // times out, the user presses Sync again, and the abandoned child is
        // still running.
        //
        // `units()` is the observable that matters: it counts the work the
        // returned Task will actually perform, so a second `update` that
        // still dispatched would report a non-zero count here regardless of
        // what any flag on `App` says. The runner assertion below covers the
        // other shape of the bug -- an `update` that gave up on the Task and
        // ran uv inline instead.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let fake = Arc::new(FakeCommandRunner::new());

        let first = update(&mut app, Message::SyncRequested, fake.clone());
        assert!(first.units() > 0, "the first Sync must dispatch work");

        let second = update(&mut app, Message::SyncRequested, fake.clone());

        assert_eq!(
            second.units(),
            0,
            "a second Sync while one is in flight must dispatch nothing"
        );
        assert!(
            fake.calls().is_empty(),
            "no uv command may be issued by the ignored press; got {:?}",
            fake.calls()
        );
    }

    #[test]
    fn a_sync_that_finishes_releases_the_in_flight_slot() {
        // The guard above is a deadlock if it is never cleared: Sync would
        // work exactly once per launch.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let _ = update(&mut app, Message::SyncRequested, unused_runner());
        let _ = update(
            &mut app,
            Message::SyncFinished {
                success: true,
                lines: vec![],
            },
            unused_runner(),
        );

        let again = update(&mut app, Message::SyncRequested, unused_runner());
        assert!(
            again.units() > 0,
            "Sync must dispatch again once the previous run has finished"
        );
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
