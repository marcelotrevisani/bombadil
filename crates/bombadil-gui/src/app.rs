use crate::banner;
use crate::detail;
use crate::drawer;
use crate::job;
use crate::sidebar;
use bombadil_core::envspec::{self, EnvInputs};
use bombadil_core::model::{Config, Project};
use bombadil_core::process::CommandRunner;
use bombadil_core::secrets::{KeyringSecretStore, SecretStore};
use bombadil_core::terminal::open::secret_values;
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
                let config = app.config.clone();
                app.syncing = Some(project.id.to_string());
                // Fresh drawer per run, but showing a *running* status rather
                // than the default "idle": for the whole duration of the sync
                // this collapsed line is all the user has, and "idle" makes a
                // running sync look like a button that did nothing.
                app.drawer = drawer::Drawer {
                    status: format!("syncing {}...", project.label),
                    ..drawer::Drawer::default()
                };
                return Task::perform(sync_project(project, config, runner), |(success, lines)| {
                    Message::SyncFinished { success, lines }
                });
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
///
/// `KeyringSecretStore` is constructed here, on the job thread, rather than
/// taken as a parameter of `update`: it is the one piece of this path a test
/// must never reach, so it lives at the boundary and `run_sync` below takes
/// the store abstractly.
async fn sync_project(
    project: Project,
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> (bool, Vec<String>) {
    match job::run(
        move || {
            run_sync(
                &project,
                &config,
                &KeyringSecretStore::new(),
                runner.as_ref(),
            )
        },
        SYNC_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(result) => result,
        job::JobResult::TimedOut => (false, vec!["sync timed out".to_string()]),
        job::JobResult::Failed => (false, vec!["sync failed unexpectedly".to_string()]),
    }
}

/// The blocking half of a sync: resolve which uv to run, compose the
/// environment it runs in (`envspec::compose`), build the `uv sync` spec
/// (`bombadil_core::uv::command::sync`), and run it through `runner`.
/// Any resolution failure -- no project directory, no usable uv, a
/// pre-activate script that fails -- is reported as `false` plus the error's
/// own text rather than panicking, same as `venv_path::probe`'s approach to
/// a path that turns out not to be what was expected. A real run's `lines`
/// is uv's stdout followed by its stderr, in the order `CommandOutcome`
/// carries them.
///
/// Redaction happens *here*, not in `update`: this is the only place the
/// composed environment exists, and `Message` derives `Debug`, so a secrets
/// list must never be threaded through one.
///
/// `store` is a parameter so tests can hand in an `InMemorySecretStore`;
/// `sync_project` supplies the real keychain.
fn run_sync(
    project: &Project,
    config: &Config,
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
) -> (bool, Vec<String>) {
    let Some(project_dir) = project
        .pyproject_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    else {
        return (false, vec!["no project directory to run uv in".to_string()]);
    };
    // `match`, not `let ... else`: the error names the offending path, and
    // "could not resolve a virtual environment path" tells the user nothing
    // they can act on.
    let venv_path = match venv_path::resolve(project, &config.settings) {
        Ok(path) => path,
        Err(err) => return (false, vec![err.to_string()]),
    };

    // bombadil-gui does not bundle uv (no dependency on bombadil-uv-bin), so
    // `Auto`/`Bundled` sources can only ever resolve through PATH here. The
    // message below is the actionable one, and surfacing it is why the error
    // is matched rather than discarded.
    let bundled = || -> Result<PathBuf, String> {
        Err("bombadil-gui does not bundle uv yet; install uv on PATH".to_string())
    };
    let resolved_uv = match uv::resolve(
        &config.settings.uv_source,
        runner,
        &uv::locate_on_path,
        &bundled,
    ) {
        Ok(resolved) => resolved,
        Err(err) => return (false, vec![err.to_string()]),
    };

    // Without this, `uv sync` inherits Bombadil's *ambient* environment plus
    // whatever one variable `uv::command::sync` sets -- so behaviour depends
    // on whether the user launched from a shell or a desktop icon, and
    // project/global variables, keychain secrets and private index
    // credentials are all absent, which makes private-index authentication
    // impossible.
    let inputs = EnvInputs {
        parent: std::env::vars().collect(),
        config: config.clone(),
        project: project.clone(),
        venv_path: venv_path.clone(),
    };
    let env = match envspec::compose(&inputs, store, runner) {
        // `compose` already scrubs its own errors of the secrets it resolved.
        Ok(env) => env,
        Err(err) => return (false, vec![err.to_string()]),
    };
    let secrets = secret_values(&inputs, &env);

    let ctx = uv::command::ProjectContext {
        project_dir: project_dir.to_path_buf(),
        venv_path,
    };
    let mut spec = uv::command::sync(&resolved_uv, &ctx);
    // `clear_env`, the discipline plan 2b established and `uv::command::sync`
    // leaves off: the composition is authoritative, not a layer on top of
    // whatever launched Bombadil.
    spec.env = env;
    spec.clear_env = true;

    match runner.run(&spec) {
        Ok(outcome) => {
            let mut lines: Vec<String> = outcome.stdout.lines().map(str::to_string).collect();
            lines.extend(outcome.stderr.lines().map(str::to_string));
            let lines = lines
                .into_iter()
                .map(|line| envspec::redact(line, &secrets))
                .collect();
            (outcome.success(), lines)
        }
        Err(err) => (false, vec![envspec::redact(err.to_string(), &secrets)]),
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

    /// A uv that resolves through `UvSource::Custom`, which probes the given
    /// path through `runner` and never touches PATH or the filesystem.
    fn fake_uv_settings() -> bombadil_core::model::Settings {
        bombadil_core::model::Settings {
            uv_source: bombadil_core::model::UvSource::Custom {
                path: PathBuf::from("/fake/bin/uv"),
            },
            ..Default::default()
        }
    }

    fn version_probe(runner: &FakeCommandRunner) {
        runner.expect(
            "uv",
            &["--version"],
            bombadil_core::process::CommandOutcome {
                status: Some(0),
                stdout: "uv 0.12.1".into(),
                stderr: String::new(),
            },
        );
    }

    #[test]
    fn sync_runs_uv_with_the_composed_environment_and_nothing_ambient() {
        // Without `envspec::compose`, `uv sync` inherits Bombadil's ambient
        // environment plus one variable: project and global env vars,
        // keychain secrets and private index credentials are all absent, so
        // private-index authentication cannot work, and behaviour depends on
        // whether the user launched from a shell or a desktop icon.
        let project = bombadil_core::model::Project {
            env: vec![bombadil_core::model::EnvVar {
                key: "MARKER".into(),
                value: "set".into(),
                secret: false,
            }],
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

        let (success, _lines) = run_sync(&project, &config, &store, &runner);
        assert!(success);

        let calls = runner.calls();
        let sync = calls
            .iter()
            .find(|c| c.args.iter().any(|a| a == "sync"))
            .expect("uv sync must have been run");
        assert!(
            sync.clear_env,
            "the composition must be authoritative, not layered on the ambient environment"
        );
        assert_eq!(
            sync.env.get("MARKER").map(String::as_str),
            Some("set"),
            "a project env var must reach uv; got {:?}",
            sync.env.keys().collect::<Vec<_>>()
        );
        assert!(
            sync.env.contains_key("VIRTUAL_ENV"),
            "the composed activation layer must be present"
        );
    }

    #[test]
    fn a_secret_in_the_composed_environment_never_reaches_the_drawer() {
        // uv echoing a credential back in its own error output is exactly how
        // a private-index failure looks. The composed environment is the only
        // place the secret's value is known, so redaction has to happen on
        // the job thread -- `Message` derives `Debug`, and threading a
        // secrets list through `SyncFinished` would make it a carrier of
        // keychain material.
        let project = bombadil_core::model::Project {
            env: vec![bombadil_core::model::EnvVar {
                key: "TOKEN".into(),
                value: String::new(),
                secret: true,
            }],
            ..project("api")
        };
        let config = Config {
            projects: vec![project.clone()],
            settings: fake_uv_settings(),
            ..Config::default()
        };
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        bombadil_core::secrets::SecretStore::set(
            &store,
            &bombadil_core::secrets::SecretKey::ProjectEnv {
                project_id: project.id,
                key: "TOKEN".into(),
            },
            "s3cr3t-sync-value",
        )
        .unwrap();

        let runner = FakeCommandRunner::new();
        version_probe(&runner);
        runner.expect(
            "uv",
            &["sync"],
            bombadil_core::process::CommandOutcome {
                status: Some(1),
                stdout: String::new(),
                stderr: "error: 401 for index (token: s3cr3t-sync-value)".into(),
            },
        );

        let (success, lines) = run_sync(&project, &config, &store, &runner);
        assert!(!success);

        // Through the real `update` path, so this covers everything between
        // the process output and what the drawer holds.
        let mut app = App::new(config);
        let _ = update(
            &mut app,
            Message::SyncFinished { success, lines },
            unused_runner(),
        );

        assert!(
            app.drawer
                .lines
                .iter()
                .all(|l| !l.contains("s3cr3t-sync-value")),
            "a keychain secret leaked into the drawer transcript: {:?}",
            app.drawer.lines
        );
        assert!(
            app.drawer.lines.iter().any(|l| l.contains("***")),
            "the failing line must still be shown, redacted; got {:?}",
            app.drawer.lines
        );
    }

    #[test]
    fn an_unresolvable_uv_reports_the_actionable_message() {
        // The `bundled` closure's own message is the one the user can act on
        // ("install uv on PATH"); a generic "could not resolve a uv binary"
        // throws away the only sentence that helps.
        let project = project("api");
        let config = Config {
            projects: vec![project.clone()],
            settings: bombadil_core::model::Settings {
                uv_source: bombadil_core::model::UvSource::Bundled,
                ..Default::default()
            },
            ..Config::default()
        };
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();

        let (success, lines) = run_sync(&project, &config, &store, &runner);

        assert!(!success);
        let joined = lines.join("\n");
        assert!(
            joined.contains("install uv on PATH"),
            "the resolve error's own text must survive; got {joined}"
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
