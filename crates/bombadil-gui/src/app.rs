use crate::add_project;
use crate::banner;
use crate::context_menu;
use crate::detail;
use crate::drawer;
use crate::interpreter;
use crate::job;
use crate::sidebar;
use bombadil_core::envspec::{self, EnvInputs};
use bombadil_core::model::{Config, Project, TerminalChoice};
use bombadil_core::process::CommandRunner;
use bombadil_core::secrets::{KeyringSecretStore, SecretStore};
use bombadil_core::store::Loaded;
use bombadil_core::terminal::open::secret_values;
use bombadil_core::terminal::{self, Os, TerminalKind};
use bombadil_core::uv;
use bombadil_core::uv::results::Interpreter;
use bombadil_core::venv_path;
use iced::Task;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// How long a venv probe (a `pyvenv.cfg` read) gets before the sidebar shows
/// that project as hollow instead of waiting on it. Reading a small local
/// file is normally sub-millisecond; a few seconds already covers a slow
/// disk, and nothing here is worth making the user stare at a frozen sidebar
/// for longer than that. Reused by `load_manifest_draft` for the same reason:
/// a `pyproject.toml` read is the same shape of small local file read.
const VENV_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a `uv sync` gets before it is treated as failed. Sync can
/// download and build packages, unlike the sub-millisecond `pyvenv.cfg`
/// read `VENV_PROBE_TIMEOUT` bounds, so this is generous rather than tight.
const SYNC_TIMEOUT: Duration = Duration::from_secs(300);

/// Saves the whole config to disk. A trait rather than a bare
/// `bombadil_core::store::Store` so a test can hand in a fake that records
/// what would have been written instead of touching a real file -- the same
/// shape as `CommandRunner`. Implemented for `Store` below; `main.rs` wires
/// the real one in after `boot`.
pub trait ConfigPersist: Send + Sync {
    fn save(&self, config: &Config) -> Result<(), String>;
}

impl ConfigPersist for bombadil_core::store::Store {
    fn save(&self, config: &Config) -> Result<(), String> {
        bombadil_core::store::Store::save(self, config).map_err(|e| e.to_string())
    }
}

/// `App::new`'s default `persist` before a real `Store` is wired in. Every
/// save fails rather than silently succeeding -- a config that reports
/// success while writing nothing would be a worse bug than one that never
/// pretends to have saved at all. Exists only so tests that never confirm
/// the add-project dialog don't need to supply a store; `main.rs` always
/// replaces this immediately after `boot`.
pub struct NullPersist;

impl ConfigPersist for NullPersist {
    fn save(&self, _config: &Config) -> Result<(), String> {
        Err("no config store configured".to_string())
    }
}

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
    /// Problems from *loading* the config, computed once at startup by
    /// `boot`. Unlike `banner::problems`, which `view` recomputes from
    /// `config` every render, the load happened once and cannot be
    /// re-derived from the config it produced -- an unreadable file and an
    /// empty one both leave a default `Config` behind.
    pub load_problems: banner::Problems,
    /// The id of the project whose `uv sync` is in flight, if any.
    ///
    /// Spec line 121 requires at most one mutating uv command per project.
    /// A single slot rather than a set per project is deliberate and is
    /// slightly stricter than the spec: there is one shared output drawer,
    /// so a second concurrent sync -- even for a different project -- would
    /// interleave its transcript into the first one's. One at a time is what
    /// the UI can actually represent.
    ///
    /// A `String` rather than a `Uuid`: this predates `uuid` becoming a real
    /// dependency of this crate (added for `add_project::to_project`), and an
    /// equality check has no need for the typed form.
    pub syncing: Option<String>,
    /// The in-progress add-project dialog, if the user has picked a manifest.
    /// `None` before a manifest is chosen and after Cancel -- there is
    /// nothing else for the dialog to show in either state.
    pub add_project: Option<add_project::Draft>,
    /// Where confirming the add-project dialog saves the config. A trait
    /// object, not a bare `Store`, so a test can substitute a fake -- see
    /// `ConfigPersist`.
    pub persist: Arc<dyn ConfigPersist>,
    /// The open context menu (spec §8: right-click a project), if any.
    /// `None` before the first right-click and after it closes -- there is
    /// nothing else for the menu to show in either state, the same shape
    /// `add_project` uses for its own dialog.
    pub context_menu: Option<context_menu::ContextMenu>,
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
    /// The "Add project" button was pressed: open the async file picker for
    /// a `pyproject.toml`.
    AddProjectPickManifestRequested,
    /// The manifest picker finished. `None` when the user cancelled it.
    AddProjectManifestPicked(Option<PathBuf>),
    /// The chosen manifest has been read and parsed into a `Draft`.
    AddProjectManifestLoaded(add_project::Draft),
    /// `uv python list` finished, on manifest load or after an install.
    /// Spec §8: the interpreter selector is pre-selected to satisfy
    /// `requires-python`, computed fresh from this list every render by
    /// `interpreter::preselect` rather than stored -- the same "recompute,
    /// don't cache" trick `resolved_venv_path_display` uses.
    AddProjectInterpretersLoaded(Vec<Interpreter>),
    /// The dialog's "Install compatible interpreter" button was pressed:
    /// spec §8's offer to install one through uv when none is available.
    AddProjectInstallInterpreterRequested,
    /// A venv option other than "chosen folder" was picked directly.
    AddProjectVenvChoiceSelected(add_project::VenvChoice),
    /// "Choose folder..." was pressed: open the async folder picker.
    AddProjectPickVenvFolderRequested,
    /// The folder picker finished. `None` when the user cancelled it.
    AddProjectVenvFolderPicked(Option<PathBuf>),
    /// The dialog was dismissed without adding a project.
    AddProjectCancelled,
    /// "Add project" was pressed on an open dialog: append the draft to the
    /// config, persist it, and adopt or create its venv.
    AddProjectConfirmed,
    /// The background half of `AddProjectConfirmed` finished. `index` is
    /// where the project landed in `projects`/`entries`, so the venv status
    /// can be refreshed the same way `boot`'s probes fill it in.
    AddProjectConfirmFinished {
        success: bool,
        lines: Vec<String>,
        index: usize,
    },
    /// A project was right-clicked: open the context menu for `projects[usize]`
    /// and kick off terminal detection. Spec §8.
    ContextMenuOpened(usize),
    /// The context menu was dismissed without an action -- Close was pressed,
    /// or an action that closes it as a side effect (Open terminal, Remove).
    ContextMenuClosed,
    /// Terminal detection finished for the open context menu. Criterion 1:
    /// `terminal::detect::available` fires up to seven probes, run through
    /// `job::run` (see `detect_terminals`) so none of them can hang the
    /// window on a stale mount or a slow `open -Ra`.
    ContextMenuTerminalsDetected(Vec<TerminalKind>),
    /// "Open terminal" (or its Custom entry) was picked for `projects[usize]`.
    /// Criterion 5: dispatched as background work, never run inline --
    /// `open_terminal` wraps the call in `job::run`, and `terminal::open::open`
    /// itself spawns the emulator via `CommandRunner::spawn`, not `run`, so
    /// the window is never held hostage for the terminal's lifetime.
    OpenTerminalRequested(usize, TerminalChoice),
    /// The background open finished. `Ok(())` on a successful spawn; `Err`
    /// carries text `terminal::open::open` has already redacted of any
    /// secret the composed environment resolved.
    OpenTerminalFinished(Result<(), String>),
    /// "Remove from list" was picked for `projects[usize]`. Criterion 4:
    /// removes the project from `config`/`projects`/`entries` in memory,
    /// synchronously, and dispatches a background persist -- the same
    /// in-memory-first shape `AddProjectConfirmed` uses. Never touches the
    /// venv on disk: nothing in this path calls a `CommandRunner`, and
    /// `bombadil-core` has no function that could delete one anyway.
    RemoveProjectRequested(usize),
    /// The background persist for a removal finished.
    RemoveProjectFinished {
        success: bool,
    },
}

impl App {
    pub fn new(config: Config) -> Self {
        let projects = config.projects.clone();
        let selected = (!projects.is_empty()).then_some(0);
        // No probe yet at construction time -- every project starts hollow;
        // `boot` kicks off the real probes and `update` fills results in as
        // they land.
        let entries = sidebar::entries(&config);
        Self {
            config,
            projects,
            selected,
            entries,
            drawer: drawer::Drawer::default(),
            load_problems: Vec::new(),
            syncing: None,
            add_project: None,
            persist: Arc::new(NullPersist),
            context_menu: None,
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
///
/// Takes the whole `Loaded`, not just its `Config`: a corrupt or too-new
/// file yields a *default* config, so dropping the variant here would leave
/// the user staring at an empty project list with nothing to explain it.
pub fn boot(loaded: Loaded) -> (App, Task<Message>) {
    let mut app = App::new(loaded.config().clone());
    app.load_problems = banner::load_problems(&loaded);
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
        Message::AddProjectPickManifestRequested => {
            return Task::perform(pick_pyproject(), Message::AddProjectManifestPicked);
        }
        // The dialog was cancelled -- nothing chosen, nothing to do.
        Message::AddProjectManifestPicked(None) => {}
        Message::AddProjectManifestPicked(Some(path)) => {
            return Task::perform(load_manifest_draft(path), Message::AddProjectManifestLoaded);
        }
        Message::AddProjectManifestLoaded(draft) => {
            app.add_project = Some(draft);
            // Fired unconditionally, even for a manifest with no
            // requires-python: the dialog still needs a list to preselect
            // "newest available" from, and a fetch that only ran when a
            // requirement was present would leave that case stuck on
            // "checking..." forever.
            return Task::perform(
                list_interpreters(app.config.clone(), runner),
                Message::AddProjectInterpretersLoaded,
            );
        }
        Message::AddProjectInterpretersLoaded(interpreters) => {
            if let Some(draft) = &mut app.add_project {
                draft.interpreters = interpreters;
            }
        }
        Message::AddProjectInstallInterpreterRequested => {
            if let Some(draft) = &app.add_project
                && let Some(version) =
                    interpreter::suggested_install_version(draft.requires_python.as_deref())
            {
                return Task::perform(
                    install_and_relist(version, app.config.clone(), runner),
                    Message::AddProjectInterpretersLoaded,
                );
            }
        }
        Message::AddProjectVenvChoiceSelected(choice) => {
            if let Some(draft) = &mut app.add_project {
                draft.venv_choice = choice;
            }
        }
        Message::AddProjectPickVenvFolderRequested => {
            return Task::perform(pick_folder(), Message::AddProjectVenvFolderPicked);
        }
        // Folder picker cancelled -- leave the previous choice untouched.
        Message::AddProjectVenvFolderPicked(None) => {}
        Message::AddProjectVenvFolderPicked(Some(path)) => {
            if let Some(draft) = &mut app.add_project {
                draft.venv_choice = add_project::VenvChoice::Chosen(path);
            }
        }
        Message::AddProjectCancelled => {
            app.add_project = None;
        }
        Message::AddProjectConfirmed => {
            if let Some(draft) = app.add_project.take() {
                let project = add_project::to_project(&draft);
                let python_version =
                    interpreter::preselect(&draft.interpreters, draft.requires_python.as_deref())
                        .map(|i| i.version.clone());

                // In-memory first, synchronously -- criterion 3's other
                // failure mode is a dialog that looks like it worked but
                // dispatched nothing at all. Persisting is what makes this
                // survive a restart; that part happens off the render
                // thread below, the same as every other blocking call in
                // this file.
                app.config.projects.push(project.clone());
                app.projects.push(project.clone());
                app.entries.push(sidebar::Entry {
                    label: project.label.clone(),
                    venv_exists: false,
                    python_version: None,
                });
                let index = app.projects.len() - 1;
                app.selected = Some(index);

                let config = app.config.clone();
                let persist = app.persist.clone();
                return Task::perform(
                    confirm_add_project(project, config, python_version, persist, runner),
                    move |(success, lines)| Message::AddProjectConfirmFinished {
                        success,
                        lines,
                        index,
                    },
                );
            }
        }
        Message::AddProjectConfirmFinished {
            success,
            lines,
            index,
        } => {
            // Already redacted on the job thread, same reasoning as
            // `SyncFinished`.
            for line in &lines {
                drawer::push_line(&mut app.drawer, line, &[]);
            }
            let status = if success {
                "project added".to_string()
            } else {
                "add project failed".to_string()
            };
            drawer::set_idle(&mut app.drawer, status);
            if !success && !app.drawer.lines.is_empty() {
                app.drawer.expanded = true;
            }
            // Refreshes the sidebar entry the same way `boot` fills in every
            // other project's venv status, rather than trusting `success` to
            // imply a venv now exists.
            if let Some(project) = app.projects.get(index).cloned()
                && let Ok(path) = venv_path::resolve(&project, &app.config.settings)
            {
                return Task::perform(probe_venv(path), move |version| {
                    Message::VenvProbed(index, version)
                });
            }
        }
        // A stale index can arrive the same way `ProjectSelected`'s can.
        Message::ContextMenuOpened(i) if i < app.projects.len() => {
            app.context_menu = Some(context_menu::ContextMenu {
                project_index: i,
                terminals: Vec::new(),
            });
            return Task::perform(
                detect_terminals(Os::host(), runner),
                Message::ContextMenuTerminalsDetected,
            );
        }
        Message::ContextMenuOpened(_) => {}
        Message::ContextMenuClosed => {
            app.context_menu = None;
        }
        // A stale detection result for a menu the user already closed.
        Message::ContextMenuTerminalsDetected(terminals) => {
            if let Some(menu) = &mut app.context_menu {
                menu.terminals = terminals;
            }
        }
        Message::OpenTerminalRequested(i, choice) if i < app.projects.len() => {
            app.context_menu = None;
            if let Some(project) = app.projects.get(i).cloned() {
                let config = app.config.clone();
                return Task::perform(
                    open_terminal(choice, project, config, runner),
                    Message::OpenTerminalFinished,
                );
            }
        }
        Message::OpenTerminalRequested(_, _) => {}
        Message::OpenTerminalFinished(Ok(())) => {}
        Message::OpenTerminalFinished(Err(message)) => {
            // Redacted already, on the job thread inside `terminal::open::open`
            // -- the composed environment never reaches this `Message`, the
            // same discipline `SyncFinished` follows for `run_sync`.
            drawer::push_line(&mut app.drawer, &message, &[]);
            drawer::set_idle(&mut app.drawer, "open terminal failed".to_string());
            if !app.drawer.lines.is_empty() {
                app.drawer.expanded = true;
            }
        }
        Message::RemoveProjectRequested(i) if i < app.projects.len() => {
            // In-memory first, synchronously -- the same discipline
            // `AddProjectConfirmed` uses, so the sidebar reflects the removal
            // the instant `update` returns rather than only once the
            // background persist lands.
            app.config.projects.remove(i);
            app.projects.remove(i);
            app.entries.remove(i);
            app.selected = match app.selected {
                Some(s) if s == i => (!app.projects.is_empty())
                    .then_some(i.min(app.projects.len().saturating_sub(1))),
                Some(s) if s > i => Some(s - 1),
                other => other,
            };
            app.context_menu = None;

            let config = app.config.clone();
            let persist = app.persist.clone();
            return Task::perform(persist_removal(config, persist), |success| {
                Message::RemoveProjectFinished { success }
            });
        }
        Message::RemoveProjectRequested(_) => {}
        Message::RemoveProjectFinished { success } => {
            if !success {
                drawer::set_idle(&mut app.drawer, "remove failed to persist".to_string());
            }
        }
    }
    Task::none()
}

/// Opens the async manifest picker. Never the blocking `rfd::FileDialog`: a
/// modal dialog on the render thread would freeze the window exactly like a
/// synchronous `CommandRunner::run` call would.
async fn pick_pyproject() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .add_filter("pyproject.toml", &["toml"])
        .pick_file()
        .await
        .map(|handle| handle.path().to_path_buf())
}

/// Opens the async folder picker for a chosen venv location. Same reasoning
/// as `pick_pyproject`: async only.
async fn pick_folder() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .pick_folder()
        .await
        .map(|handle| handle.path().to_path_buf())
}

/// Reads the chosen manifest off the render thread via `job::run` -- the
/// same seam `probe_venv` uses for `pyvenv.cfg` -- then hands the text to
/// `add_project::draft_from`, which is pure. A read failure (permissions, the
/// file vanishing between pick and read) degrades to empty text rather than
/// losing the chosen path: `draft_from` already treats unparseable text as
/// "no fields parsed, path kept", so an empty string lands in exactly that
/// path with no separate error case to wire up.
async fn load_manifest_draft(path: PathBuf) -> add_project::Draft {
    let read_path = path.clone();
    let text = match job::run(
        move || std::fs::read_to_string(&read_path).unwrap_or_default(),
        VENV_PROBE_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(text) => text,
        job::JobResult::TimedOut | job::JobResult::Failed => String::new(),
    };
    add_project::draft_from(&path, &text)
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

/// Resolves which uv binary to run, given `config.settings.uv_source`.
/// Shared by `run_sync` and the interpreter discovery/install jobs below --
/// all three need exactly the same Auto/FromPath/Custom/Bundled decision.
/// The bundled closure extracts the embedded uv (see `bombadil_uv_bin`) into
/// the platform data dir, re-using it on later calls -- `ensure_extracted`
/// only rewrites when the file is missing or its hash no longer matches.
/// `Auto` falls back to this when nothing usable is on PATH; `Bundled` uses
/// it unconditionally. The error is pre-stringified because every caller
/// does the same thing with it: surface `uv::resolve`'s own actionable text
/// verbatim rather than a generic "could not resolve uv."
fn resolve_uv_for(config: &Config, runner: &dyn CommandRunner) -> Result<uv::ResolvedUv, String> {
    let bundled = || -> Result<PathBuf, String> {
        let data_dir =
            bombadil_core::store::paths::default_data_dir().map_err(|e| e.to_string())?;
        bombadil_uv_bin::ensure_extracted(&data_dir).map_err(|e| e.to_string())
    };
    uv::resolve(
        &config.settings.uv_source,
        runner,
        &uv::locate_on_path,
        &bundled,
    )
    .map_err(|err| err.to_string())
}

/// The blocking half of interpreter discovery: resolve uv, run
/// `uv python list --output-format json`, and parse it. Any failure -- no
/// usable uv, a non-zero exit, JSON this build cannot parse -- degrades to
/// an empty list rather than propagating an error: the dialog already has a
/// way to show "nothing found yet" that is indistinguishable from, and no
/// worse than, a genuine failure here.
fn list_interpreters_blocking(config: &Config, runner: &dyn CommandRunner) -> Vec<Interpreter> {
    let Ok(resolved) = resolve_uv_for(config, runner) else {
        return Vec::new();
    };
    let spec = uv::command::python_list(&resolved);
    match runner.run(&spec) {
        Ok(outcome) if outcome.success() => {
            uv::results::parse_python_list(&outcome.stdout).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// Fetches the interpreters uv can see, off the render thread -- the same
/// seam `probe_venv` uses. `VENV_PROBE_TIMEOUT`, not `SYNC_TIMEOUT`: `uv
/// python list` only reads uv's own installed-interpreter cache, the same
/// quick shape of call as a `pyvenv.cfg` read, not a `sync` that can
/// download and build packages.
async fn list_interpreters(config: Config, runner: Arc<dyn CommandRunner>) -> Vec<Interpreter> {
    match job::run(
        move || list_interpreters_blocking(&config, runner.as_ref()),
        VENV_PROBE_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(interpreters) => interpreters,
        job::JobResult::TimedOut | job::JobResult::Failed => Vec::new(),
    }
}

/// The blocking half of installing an interpreter: resolve uv and run `uv
/// python install <version>`. Failure collapses to `false` -- the caller
/// re-lists regardless, so a failed install just means the refreshed list
/// still has nothing satisfying, and the dialog already shows exactly that.
fn install_interpreter_blocking(
    version: &str,
    config: &Config,
    runner: &dyn CommandRunner,
) -> bool {
    let Ok(resolved) = resolve_uv_for(config, runner) else {
        return false;
    };
    let spec = uv::command::python_install(&resolved, version);
    runner
        .run(&spec)
        .map(|outcome| outcome.success())
        .unwrap_or(false)
}

/// Installs `version` through uv, then re-fetches the interpreter list.
/// Bounded by `SYNC_TIMEOUT`, not `VENV_PROBE_TIMEOUT`: unlike listing,
/// `uv python install` can download a whole interpreter.
async fn install_and_relist(
    version: String,
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> Vec<Interpreter> {
    let install_config = config.clone();
    let install_runner = runner.clone();
    let _ = job::run(
        move || install_interpreter_blocking(&version, &install_config, install_runner.as_ref()),
        SYNC_TIMEOUT,
    )
    .await;
    list_interpreters(config, runner).await
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

    let resolved_uv = match resolve_uv_for(config, runner) {
        Ok(resolved) => resolved,
        Err(err) => return (false, vec![err]),
    };

    let ctx = uv::command::ProjectContext {
        project_dir: project_dir.to_path_buf(),
        venv_path: venv_path.clone(),
    };
    let spec = uv::command::sync(&resolved_uv, &ctx);
    compose_and_run(spec, project, config, &venv_path, store, runner)
}

/// The blocking half of confirming the add-project dialog: persist the
/// updated config, then decide whether the chosen location already holds a
/// venv (adopt it, run nothing) or needs one created
/// (`uv sync -p <python>`, same composition discipline as `run_sync`).
///
/// `persist` and `probe` are parameters, not a concrete `Store` and
/// `venv_path::probe` called directly, so a test can hand in fakes that
/// never touch a real filesystem -- the real caller, `confirm_add_project`,
/// supplies both.
fn run_confirm_add_project(
    project: &Project,
    config: &Config,
    python_version: Option<&str>,
    persist: &dyn ConfigPersist,
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
    probe: impl Fn(&Path) -> Option<String>,
) -> (bool, Vec<String>) {
    if let Err(err) = persist.save(config) {
        return (false, vec![err]);
    }

    let venv_path = match venv_path::resolve(project, &config.settings) {
        Ok(path) => path,
        Err(err) => return (false, vec![err.to_string()]),
    };

    // Adopting: the venv already exists, so running sync anyway would
    // silently rebuild an environment the user asked to keep (criterion 5).
    if let add_project::VenvOutcome::Adopt { python_version } =
        add_project::venv_outcome(&venv_path, probe)
    {
        return (
            true,
            vec![format!(
                "existing environment adopted (python {python_version})"
            )],
        );
    }

    let Some(project_dir) = project
        .pyproject_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    else {
        return (false, vec!["no project directory to run uv in".to_string()]);
    };
    let resolved_uv = match resolve_uv_for(config, runner) {
        Ok(resolved) => resolved,
        Err(err) => return (false, vec![err]),
    };
    let ctx = uv::command::ProjectContext {
        project_dir: project_dir.to_path_buf(),
        venv_path: venv_path.clone(),
    };
    let spec = match python_version {
        Some(python) => uv::command::sync_with_python(&resolved_uv, &ctx, python),
        None => uv::command::sync(&resolved_uv, &ctx),
    };
    compose_and_run(spec, project, config, &venv_path, store, runner)
}

/// Shared by `run_sync` and `run_confirm_add_project`: compose the
/// environment `spec` runs in, mark it authoritative with `clear_env`, run
/// it, and redact every secret the composition resolved out of the result.
///
/// Without composing, `uv sync` inherits Bombadil's *ambient* environment
/// plus whatever few variables the spec's own builder sets -- so behaviour
/// depends on whether the user launched from a shell or a desktop icon, and
/// project/global variables, keychain secrets and private index credentials
/// are all absent, which makes private-index authentication impossible.
///
/// Redaction happens *here*, not in `update`: this is the only place the
/// composed environment exists, and `Message` derives `Debug`, so a secrets
/// list must never be threaded through one.
fn compose_and_run(
    mut spec: bombadil_core::process::CommandSpec,
    project: &Project,
    config: &Config,
    venv_path: &Path,
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
) -> (bool, Vec<String>) {
    let inputs = EnvInputs {
        parent: std::env::vars().collect(),
        config: config.clone(),
        project: project.clone(),
        venv_path: venv_path.to_path_buf(),
    };
    let env = match envspec::compose(&inputs, store, runner) {
        // `compose` already scrubs its own errors of the secrets it resolved.
        Ok(env) => env,
        Err(err) => return (false, vec![err.to_string()]),
    };
    let secrets = secret_values(&inputs, &env);

    spec.env = env;
    // `clear_env`, the discipline plan 2b established and the spec builders
    // leave off: the composition is authoritative, not a layer on top of
    // whatever launched Bombadil.
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

/// Runs off the render thread via `job::run` -- persisting the config,
/// probing the chosen venv location and possibly running `uv sync` are all
/// blocking. Same shape as `sync_project`: `KeyringSecretStore` is
/// constructed here, on the job thread, never taken as a parameter of
/// `update`, and `venv_path::probe` is the real filesystem probe -- tests
/// exercise `run_confirm_add_project` directly with a fake instead of going
/// through this async wrapper.
async fn confirm_add_project(
    project: Project,
    config: Config,
    python_version: Option<String>,
    persist: Arc<dyn ConfigPersist>,
    runner: Arc<dyn CommandRunner>,
) -> (bool, Vec<String>) {
    match job::run(
        move || {
            run_confirm_add_project(
                &project,
                &config,
                python_version.as_deref(),
                persist.as_ref(),
                &KeyringSecretStore::new(),
                runner.as_ref(),
                venv_path::probe,
            )
        },
        SYNC_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(result) => result,
        job::JobResult::TimedOut => (false, vec!["add project timed out".to_string()]),
        job::JobResult::Failed => (false, vec!["add project failed unexpectedly".to_string()]),
    }
}

/// Detects installed terminals off the render thread. Criterion 1:
/// `terminal::detect::available` fires up to seven probes (`which`, `where`,
/// `open -Ra`), any one of which can hang -- a `which` walking a stale
/// network mount, a slow `open -Ra` waiting on LaunchServices -- exactly what
/// `job::run`'s timeout exists for. Degrades to an empty list on a timeout or
/// a panic, the same as every other probe in this file: offering nothing is
/// safer than offering a terminal that turns out not to be there (spec §9).
async fn detect_terminals(os: Os, runner: Arc<dyn CommandRunner>) -> Vec<TerminalKind> {
    match job::run(
        move || terminal::detect::available(os, runner.as_ref()),
        VENV_PROBE_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(kinds) => kinds,
        job::JobResult::TimedOut | job::JobResult::Failed => Vec::new(),
    }
}

/// The blocking half of opening a terminal: resolves the venv path and hands
/// off to `terminal::open::open`, which composes the environment and spawns
/// the emulator via `CommandRunner::spawn` -- never `run`, which would hold
/// the window hostage for the terminal's whole lifetime (criterion 5).
///
/// `KeyringSecretStore` is constructed by the caller, on the job thread, the
/// same discipline `sync_project` uses -- it is the one piece of this path a
/// test must never reach.
///
/// `shell` comes from `$SHELL` rather than being hardcoded: unset (as it
/// commonly is on Windows) degrades to an unrecognised shell family, which
/// `terminal::open::open` already handles by opening without the
/// re-assertion rather than refusing to open at all.
fn run_open_terminal(
    choice: &TerminalChoice,
    project: &Project,
    config: &Config,
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
) -> Result<(), String> {
    let venv_path = venv_path::resolve(project, &config.settings).map_err(|e| e.to_string())?;
    let inputs = EnvInputs {
        parent: std::env::vars().collect(),
        config: config.clone(),
        project: project.clone(),
        venv_path,
    };
    let shell = std::env::var("SHELL").unwrap_or_default();
    // A Bombadil-owned scratch directory for the re-assertion init file
    // `terminal::open::open` writes -- namespaced per-project underneath by
    // `open` itself, so two concurrent opens for different projects cannot
    // collide (see its own doc comment).
    let scratch_dir = std::env::temp_dir().join("bombadil-terminal-init");
    terminal::open::open(
        choice,
        Os::host(),
        &inputs,
        &shell,
        store,
        runner,
        &scratch_dir,
    )
    .map_err(|err| err.to_string())
}

/// Runs off the render thread via `job::run` -- same shape as `sync_project`.
async fn open_terminal(
    choice: TerminalChoice,
    project: Project,
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> Result<(), String> {
    match job::run(
        move || {
            run_open_terminal(
                &choice,
                &project,
                &config,
                &KeyringSecretStore::new(),
                runner.as_ref(),
            )
        },
        VENV_PROBE_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(result) => result,
        job::JobResult::TimedOut => Err("opening the terminal timed out".to_string()),
        job::JobResult::Failed => Err("opening the terminal failed unexpectedly".to_string()),
    }
}

/// The blocking half of persisting a removal: just a save. Its own function,
/// mirroring `run_confirm_add_project`, so a test can exercise it directly
/// with a `FakeConfigPersist` instead of through `update`'s opaque `Task`.
fn run_persist(config: &Config, persist: &dyn ConfigPersist) -> bool {
    persist.save(config).is_ok()
}

/// Runs off the render thread via `job::run`: persisting the config after a
/// removal is a blocking file write, same as every other `ConfigPersist` call
/// in this file.
async fn persist_removal(config: Config, persist: Arc<dyn ConfigPersist>) -> bool {
    match job::run(move || run_persist(&config, persist.as_ref()), SYNC_TIMEOUT).await {
        job::JobResult::Done(success) => success,
        job::JobResult::TimedOut | job::JobResult::Failed => false,
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

    let add_project_button = iced::widget::button(iced::widget::text("Add project"))
        .on_press(Message::AddProjectPickManifestRequested);
    let sidebar_and_controls = iced::widget::column![
        add_project_button,
        sidebar::view(&app.entries, app.selected)
    ]
    .spacing(8);

    let body = iced::widget::row![
        iced::widget::container(sidebar_and_controls).width(240),
        iced::widget::container(detail_and_drawer).padding(12),
    ];

    // Recomputed fresh on every render from `app.add_project`'s current
    // `venv_choice`, the same trick `banner::problems` uses to stay live --
    // this is what makes the resolved path track a venv choice the user just
    // changed, per spec §8, rather than a value cached from when the
    // manifest was first picked.
    let add_project_dialog: iced::Element<'_, Message> = match &app.add_project {
        Some(draft) => {
            let resolved = add_project::resolved_venv_path_display(draft, &app.config.settings);
            add_project::view(draft, &resolved)
        }
        None => iced::widget::text("").into(),
    };

    // The config problems are recomputed from `app.config` on every call,
    // not cached anywhere on `App` -- see `banner`'s module doc for why that
    // is what makes this never go stale after a config change. The load
    // problems are the exception: the load happened once, and an unreadable
    // file leaves no trace in the default `Config` it produced.
    // Same freshness reasoning as `add_project_dialog`: the default terminal
    // is read straight from `app.config` on every render rather than cached,
    // so a settings change (plan 3c) would not need this to be re-derived.
    let context_menu_view: iced::Element<'_, Message> = match &app.context_menu {
        Some(menu) => {
            let default = app.config.terminal_defaults.get(&Os::host());
            context_menu::view(menu, default)
        }
        None => iced::widget::text("").into(),
    };

    let mut problems = app.load_problems.clone();
    problems.extend(banner::problems(&app.config));
    let footer = iced::widget::text(crate::about::attribution()).size(11);
    iced::widget::column![
        banner::view(&problems),
        body,
        add_project_dialog,
        context_menu_view,
        footer
    ]
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bombadil_core::model::Config;
    use bombadil_core::process::FakeCommandRunner;
    use std::path::Path;

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
    fn a_corrupt_config_boots_with_a_banner_rather_than_a_silently_empty_list() {
        // `banner::load_problems` is only useful if it is actually wired: a
        // user whose config is corrupt gets a default `Config`, so without
        // this the window opens on zero projects and says nothing, and the
        // reasonable conclusion is that their data is gone. No projects means
        // `boot` starts no probe tasks, so this touches no filesystem.
        let (app, _task) = boot(Loaded::Unreadable {
            config: Config::default(),
            message: "expected `=` at line 4".into(),
        });

        assert!(app.projects.is_empty());
        assert!(
            app.load_problems
                .iter()
                .any(|p| p.message.contains("could not be read")),
            "the load failure must reach the banner; got {:?}",
            app.load_problems
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
        // A `Custom` pin at a path the fake runner has no rule for fails to
        // probe, so `resolve`'s own `ConfiguredUnavailable` message is what
        // `run_sync` must surface -- not a generic "could not resolve a uv
        // binary" that throws the actionable sentence away.
        //
        // Deliberately not `UvSource::Bundled` here: since `run_sync` now
        // wires the real bundled fallback, exercising `Bundled` would extract
        // the ~35 MB embedded uv to the real platform data directory, which
        // is exactly the real-filesystem access a unit test must not do.
        let project = project("api");
        let config = Config {
            projects: vec![project.clone()],
            settings: bombadil_core::model::Settings {
                uv_source: bombadil_core::model::UvSource::Custom {
                    path: PathBuf::from("/definitely/not/a/real/uv"),
                },
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
            joined.contains("/definitely/not/a/real/uv"),
            "the resolve error's own text must survive; got {joined}"
        );
    }

    fn interpreter(version: &str) -> Interpreter {
        Interpreter {
            key: format!("cpython-{version}"),
            version: version.to_string(),
            path: Some(PathBuf::from(format!("/usr/bin/python{version}"))),
            implementation: "cpython".to_string(),
        }
    }

    fn manifest_draft(text: &str) -> add_project::Draft {
        add_project::draft_from(Path::new("/p/my-api/pyproject.toml"), text)
    }

    #[test]
    fn loading_a_manifest_dispatches_an_interpreter_fetch_without_blocking() {
        // Spec §8's pre-selection needs a real interpreter list, and fetching
        // it means running `uv python list` -- exactly the kind of blocking
        // process spawn `job::run` exists to keep off the render thread.
        let mut app = App::new(Config::default());
        let fake = Arc::new(FakeCommandRunner::new());

        let task = update(
            &mut app,
            Message::AddProjectManifestLoaded(manifest_draft("")),
            fake.clone(),
        );

        assert!(
            fake.calls().is_empty(),
            "runner.run must not be called synchronously; got {:?}",
            fake.calls()
        );
        assert!(
            task.units() > 0,
            "loading a manifest must dispatch the interpreter fetch"
        );
    }

    #[test]
    fn a_finished_interpreter_fetch_lands_on_the_open_draft() {
        let mut app = App::new(Config::default());
        app.add_project = Some(manifest_draft(""));
        let interpreters = vec![interpreter("3.12.4")];

        let _ = update(
            &mut app,
            Message::AddProjectInterpretersLoaded(interpreters.clone()),
            unused_runner(),
        );

        assert_eq!(
            app.add_project.as_ref().map(|d| d.interpreters.clone()),
            Some(interpreters)
        );
    }

    #[test]
    fn a_stale_interpreter_fetch_after_cancel_is_dropped() {
        // The dialog was cancelled before the fetch landed -- there is
        // nothing left to attach the result to.
        let mut app = App::new(Config::default());
        app.add_project = None;

        let _ = update(
            &mut app,
            Message::AddProjectInterpretersLoaded(vec![interpreter("3.12.4")]),
            unused_runner(),
        );

        assert!(app.add_project.is_none());
    }

    #[test]
    fn the_install_button_dispatches_without_blocking_when_a_version_can_be_suggested() {
        let mut app = App::new(Config::default());
        let mut draft =
            manifest_draft("[project]\nname = \"my-api\"\nrequires-python = \">=3.11\"\n");
        draft.interpreters = vec![interpreter("3.9.18")];
        app.add_project = Some(draft);
        let fake = Arc::new(FakeCommandRunner::new());

        let task = update(
            &mut app,
            Message::AddProjectInstallInterpreterRequested,
            fake.clone(),
        );

        assert!(
            fake.calls().is_empty(),
            "runner.run must not be called synchronously; got {:?}",
            fake.calls()
        );
        assert!(
            task.units() > 0,
            "the install button must dispatch background work"
        );
    }

    #[test]
    fn the_install_button_dispatches_nothing_without_a_requirement_to_suggest_a_version_from() {
        // A manifest with no requires-python (a workspace root) gives
        // `interpreter::suggested_install_version` nothing to work with --
        // guessing a version to install would not be "suitable" by any
        // definition, so nothing must be dispatched.
        let mut app = App::new(Config::default());
        app.add_project = Some(manifest_draft("[tool.uv.workspace]\nmembers = []\n"));

        let task = update(
            &mut app,
            Message::AddProjectInstallInterpreterRequested,
            unused_runner(),
        );

        assert_eq!(
            task.units(),
            0,
            "nothing to install for a manifest with no requires-python"
        );
    }

    /// Records every config it was asked to save instead of touching a real
    /// file -- the `ConfigPersist` counterpart of `FakeCommandRunner`.
    #[derive(Default)]
    struct FakeConfigPersist {
        saved: std::sync::Mutex<Vec<Config>>,
    }

    impl FakeConfigPersist {
        fn saved(&self) -> Vec<Config> {
            self.saved.lock().unwrap().clone()
        }
    }

    impl ConfigPersist for FakeConfigPersist {
        fn save(&self, config: &Config) -> Result<(), String> {
            self.saved.lock().unwrap().push(config.clone());
            Ok(())
        }
    }

    #[test]
    fn confirming_the_dialog_appends_the_project_to_the_in_memory_list_and_closes() {
        // Criterion 3, the in-memory half: this must be true the instant
        // `update` returns, not only once the background save lands --
        // `Confirmed` without this would leave the dialog looking like it
        // did nothing.
        let mut app = App::new(Config::default());
        app.add_project = Some(manifest_draft(
            "[project]\nname = \"my-api\"\nrequires-python = \">=3.11\"\n",
        ));

        let _ = update(&mut app, Message::AddProjectConfirmed, unused_runner());

        assert_eq!(
            app.config.projects.len(),
            1,
            "got {:?}",
            app.config.projects
        );
        assert_eq!(app.projects.len(), 1);
        assert_eq!(app.projects[0].label, "my-api");
        assert_eq!(app.entries.len(), 1);
        assert!(
            app.add_project.is_none(),
            "the dialog must close once confirmed"
        );
    }

    #[test]
    fn confirming_a_dialog_with_nothing_chosen_does_nothing() {
        // No manifest was ever picked -- there is no draft to confirm.
        let mut app = App::new(Config::default());
        app.add_project = None;

        let task = update(&mut app, Message::AddProjectConfirmed, unused_runner());

        assert!(app.config.projects.is_empty());
        assert_eq!(task.units(), 0);
    }

    #[test]
    fn confirming_the_dialog_never_persists_or_runs_uv_synchronously() {
        // Criterion 3 and criterion 4's shared failure mode, in the shape
        // plan 3a settled on for `SyncRequested`: the meaningful assertion
        // is that neither `persist.save` nor `runner.run` has recorded
        // anything by the time `update` has already returned -- the exact
        // freeze `job::run` exists to prevent, for two different calls this
        // time instead of one.
        let mut app = App::new(Config::default());
        app.add_project = Some(manifest_draft(""));
        let persist = Arc::new(FakeConfigPersist::default());
        app.persist = persist.clone();
        let fake = Arc::new(FakeCommandRunner::new());

        let task = update(&mut app, Message::AddProjectConfirmed, fake.clone());

        assert!(
            persist.saved().is_empty(),
            "persist.save must not be called synchronously inside update"
        );
        assert!(
            fake.calls().is_empty(),
            "runner.run must not be called synchronously inside update; got {:?}",
            fake.calls()
        );
        assert!(
            task.units() > 0,
            "confirming must dispatch real background work, not Task::none()"
        );
    }

    #[test]
    fn confirming_the_dialog_persists_the_appended_project_to_disk() {
        // Criterion 3's other failure mode: the project reaches
        // `app.config` in memory but never reaches the store, so it
        // vanishes on restart. Calls the blocking half directly, the same
        // way `sync_runs_uv_with_the_composed_environment_and_nothing_ambient`
        // tests `run_sync` directly rather than through `update`'s opaque
        // `Task`.
        let project = project("api");
        let config = Config {
            projects: vec![project.clone()],
            ..Config::default()
        };
        let persist = FakeConfigPersist::default();
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();

        let (success, _lines) = run_confirm_add_project(
            &project,
            &config,
            None,
            &persist,
            &store,
            &runner,
            |_| Some("3.12.0".to_string()), // an existing venv -- adopt, not create
        );

        assert!(success);
        let saved = persist.saved();
        assert_eq!(saved.len(), 1, "persist.save must be called exactly once");
        assert_eq!(
            saved[0].projects,
            vec![project],
            "the project must be part of what gets persisted"
        );
    }

    #[test]
    fn creating_a_venv_on_confirm_runs_sync_with_python_using_the_composed_environment() {
        // Criterion 4: the two defects plan 3a shipped and had to fix for
        // `run_sync` -- an uncomposed environment (private-index auth
        // silently impossible) and clear_env left unset -- must not come
        // back for the add-project path. Tests the blocking half directly;
        // `confirming_the_dialog_never_persists_or_runs_uv_synchronously`
        // above covers the "never inline" half through `update`.
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
        let persist = FakeConfigPersist::default();
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();
        version_probe(&runner);
        runner.expect(
            "uv",
            &["sync", "-p", "3.12.1"],
            bombadil_core::process::CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let (success, _lines) = run_confirm_add_project(
            &project,
            &config,
            Some("3.12.1"),
            &persist,
            &store,
            &runner,
            |_| None, // no venv present yet -- must create
        );

        assert!(success);
        let calls = runner.calls();
        let sync = calls
            .iter()
            .find(|c| c.args.iter().any(|a| a == "sync"))
            .expect("uv sync -p must have been run");
        assert_eq!(
            sync.args,
            vec!["sync", "-p", "3.12.1"],
            "the preselected interpreter must be pinned; got {:?}",
            sync.args
        );
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
    fn adopting_an_existing_venv_on_confirm_does_not_run_sync() {
        // Criterion 5: adoption must not silently rebuild an environment the
        // user asked to keep. No rule is programmed on `runner` for `uv
        // sync` at all -- if `run_confirm_add_project` ran it anyway,
        // `FakeCommandRunner` would return `CommandError::Unexpected`
        // instead of quietly succeeding.
        let project = project("api");
        let config = Config {
            projects: vec![project.clone()],
            settings: fake_uv_settings(),
            ..Config::default()
        };
        let persist = FakeConfigPersist::default();
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();

        let (success, lines) = run_confirm_add_project(
            &project,
            &config,
            Some("3.12.1"),
            &persist,
            &store,
            &runner,
            |_| Some("3.11.9".to_string()), // an existing venv
        );

        assert!(success);
        assert!(
            runner.calls().is_empty(),
            "adoption must not run uv at all; got {:?}",
            runner.calls()
        );
        assert!(
            lines.iter().any(|l| l.contains("3.11.9")),
            "the adopted version should be reported; got {lines:?}"
        );
        assert_eq!(
            persist.saved().len(),
            1,
            "the config must still be persisted on adoption"
        );
    }

    /// Drive a future to completion on this thread, for tests that need the
    /// result of one of this file's `async fn`s (`detect_terminals`,
    /// `open_terminal`) without going through iced's opaque `Task`. Same
    /// trick `job::run`'s own tests use: its future does its waiting on an
    /// `mpsc` channel via `recv_timeout`, which blocks the polling thread
    /// itself, so a trivial spin-poll executor is enough and avoids pulling
    /// in a runtime.
    fn block_on<F: std::future::Future>(mut f: F) -> F::Output {
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

    // --- Criterion 1: only detected terminals are offered ---

    #[test]
    fn opening_the_context_menu_dispatches_terminal_detection_without_blocking() {
        // The dispatch-shape half of criterion 1: `available` fires up to
        // seven probes, any of which can hang, so detection must never run
        // synchronously inside `update` -- exactly the freeze `job::run`
        // exists to prevent.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let fake = Arc::new(FakeCommandRunner::new());

        let task = update(&mut app, Message::ContextMenuOpened(0), fake.clone());

        assert!(
            fake.calls().is_empty(),
            "runner.run must not be called synchronously inside update; got {:?}",
            fake.calls()
        );
        assert!(
            task.units() > 0,
            "opening the context menu must dispatch the terminal probe"
        );
        assert_eq!(
            app.context_menu.as_ref().map(|m| m.project_index),
            Some(0),
            "the menu must open for the right-clicked project immediately"
        );
    }

    #[test]
    fn context_menu_terminal_detection_only_offers_what_the_probe_finds() {
        // The content half of criterion 1: exercises `detect_terminals`
        // directly, the async function `job::run` wraps -- the same pattern
        // `run_sync`'s own tests use for its blocking counterpart. Proves the
        // GUI's wiring actually reaches `terminal::detect::available` rather
        // than a hardcoded list of every kind for the OS.
        let runner = FakeCommandRunner::new();
        runner.expect(
            "which",
            &["kitty"],
            bombadil_core::process::CommandOutcome {
                status: Some(0),
                stdout: "/usr/bin/kitty".into(),
                stderr: String::new(),
            },
        );

        let got = block_on(detect_terminals(
            bombadil_core::terminal::Os::Linux,
            Arc::new(runner),
        ));

        assert_eq!(
            got,
            vec![bombadil_core::terminal::TerminalKind::Kitty],
            "only the terminal the probe actually found must be offered"
        );
    }

    #[test]
    fn a_stale_detection_result_after_the_menu_closed_is_dropped() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.context_menu = None;

        let _ = update(
            &mut app,
            Message::ContextMenuTerminalsDetected(vec![
                bombadil_core::terminal::TerminalKind::Kitty,
            ]),
            unused_runner(),
        );

        assert!(app.context_menu.is_none());
    }

    // --- Criterion 2: the configured default appears first ---
    // Pure ordering logic (`context_menu::ordered`) is covered in
    // `context_menu.rs`'s own tests, the same file it lives in; this test
    // covers the GUI wiring that carries `terminal_defaults` from `Config`
    // into `view` -- see `context_menu_view` in `view`, which reads
    // `app.config.terminal_defaults` fresh on every render.

    #[test]
    fn the_configured_default_is_read_from_config_for_the_hosts_os() {
        let config = Config {
            projects: vec![project("api")],
            terminal_defaults: std::collections::BTreeMap::from([(
                bombadil_core::terminal::Os::host(),
                TerminalChoice::Detected(bombadil_core::terminal::TerminalKind::Kitty),
            )]),
            ..Config::default()
        };
        let app = App::new(config);
        assert_eq!(
            app.config
                .terminal_defaults
                .get(&bombadil_core::terminal::Os::host()),
            Some(&TerminalChoice::Detected(
                bombadil_core::terminal::TerminalKind::Kitty
            )),
            "the configured default for this OS must be reachable for view to order by"
        );
    }

    // --- Criterion 4: Remove from list ---

    #[test]
    fn removing_a_project_deletes_it_from_the_in_memory_config_immediately() {
        let config = Config {
            projects: vec![project("api"), project("web")],
            ..Config::default()
        };
        let mut app = App::new(config);

        let task = update(
            &mut app,
            Message::RemoveProjectRequested(0),
            unused_runner(),
        );

        assert_eq!(
            app.config.projects.len(),
            1,
            "got {:?}",
            app.config.projects
        );
        assert_eq!(app.config.projects[0].label, "web");
        assert_eq!(app.projects.len(), 1);
        assert_eq!(app.projects[0].label, "web");
        assert_eq!(app.entries.len(), 1);
        assert!(
            task.units() > 0,
            "removal must still dispatch a background persist"
        );
    }

    #[test]
    fn removing_a_project_persists_the_config_without_it() {
        // Exercises `run_persist` directly, the same pattern
        // `confirming_the_dialog_persists_the_appended_project_to_disk` uses
        // for the add-project path.
        let project_a = project("api");
        let project_b = project("web");
        let config = Config {
            projects: vec![project_b.clone()],
            ..Config::default()
        };
        let persist = FakeConfigPersist::default();

        let success = run_persist(&config, &persist);

        assert!(success);
        let saved = persist.saved();
        assert_eq!(saved.len(), 1, "persist.save must be called exactly once");
        assert_eq!(
            saved[0].projects,
            vec![project_b],
            "the removed project must be absent from what gets persisted"
        );
        assert!(!saved[0].projects.contains(&project_a));
    }

    #[test]
    fn removing_the_last_remaining_project_leaves_nothing_selected() {
        // The subtraction `projects.len() - 1` underflows once the removed
        // project was the only one -- must clear the selection instead of
        // panicking.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.selected = Some(0);

        let _ = update(
            &mut app,
            Message::RemoveProjectRequested(0),
            unused_runner(),
        );

        assert!(app.projects.is_empty());
        assert!(app.selected.is_none());
    }

    #[test]
    fn removing_a_project_never_touches_the_venv_on_disk() {
        // bombadil-core has no function capable of deleting a venv at all --
        // confirmed by inspection: the only `fs::remove_*` call in the whole
        // crate lives inside `venv_path`'s own test fixture teardown, not in
        // any code `update` can reach. So this proves the removal path we
        // built calls no `CommandRunner` at all, which is everything there
        // is to guard against today; there is no delete-venv primitive for a
        // future regression to accidentally wire up removal into yet.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let fake = Arc::new(FakeCommandRunner::new());

        let _ = update(&mut app, Message::RemoveProjectRequested(0), fake.clone());

        assert!(
            fake.calls().is_empty(),
            "removing a project must not run or spawn any process; got {:?}",
            fake.calls()
        );
    }

    #[test]
    fn removing_an_out_of_range_project_is_ignored() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let task = update(
            &mut app,
            Message::RemoveProjectRequested(99),
            unused_runner(),
        );

        assert_eq!(
            app.projects.len(),
            1,
            "an invalid index must change nothing"
        );
        assert_eq!(task.units(), 0);
    }

    #[test]
    fn removing_the_selected_project_clears_or_shifts_the_selection() {
        let mut app = App::new(Config {
            projects: vec![project("api"), project("web")],
            ..Config::default()
        });
        app.selected = Some(0);

        let _ = update(
            &mut app,
            Message::RemoveProjectRequested(0),
            unused_runner(),
        );

        assert_eq!(
            app.selected_project().map(|p| p.label.as_str()),
            Some("web"),
            "removing the selected project must not leave a dangling index"
        );
    }

    // --- Criterion 5: opening a terminal dispatches through job::run ---

    #[test]
    fn opening_a_terminal_never_calls_the_runner_synchronously() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let fake = Arc::new(FakeCommandRunner::new());

        let task = update(
            &mut app,
            Message::OpenTerminalRequested(
                0,
                TerminalChoice::Detected(bombadil_core::terminal::TerminalKind::Kitty),
            ),
            fake.clone(),
        );

        assert!(
            fake.calls().is_empty(),
            "runner.run/spawn must not be called synchronously inside update; got {:?}",
            fake.calls()
        );
        assert!(
            task.units() > 0,
            "opening a terminal must dispatch real background work, not Task::none()"
        );
        assert!(
            app.context_menu.is_none(),
            "the menu should close once an action is taken from it"
        );
    }

    #[test]
    fn opening_a_terminal_for_an_out_of_range_project_dispatches_nothing() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let task = update(
            &mut app,
            Message::OpenTerminalRequested(
                99,
                TerminalChoice::Detected(bombadil_core::terminal::TerminalKind::Kitty),
            ),
            unused_runner(),
        );

        assert_eq!(task.units(), 0);
    }

    #[test]
    fn opening_a_terminal_spawns_the_chosen_kind_with_the_composed_environment() {
        // Exercises `run_open_terminal` directly, the blocking half
        // `open_terminal` wraps in `job::run` -- the same pattern
        // `run_sync`'s own tests use. Proves the GUI wiring reaches
        // `terminal::open::open` with the right project/config, not just
        // that *some* process gets run.
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
            ..Config::default()
        };
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();
        runner.expect(
            "gnome-terminal",
            &[],
            bombadil_core::process::CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let result = run_open_terminal(
            &TerminalChoice::Detected(bombadil_core::terminal::TerminalKind::GnomeTerminal),
            &project,
            &config,
            &store,
            &runner,
        );

        assert!(result.is_ok(), "got {result:?}");
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].clear_env,
            "the composition must be authoritative, not layered on the ambient environment"
        );
        assert_eq!(
            calls[0].env.get("MARKER").map(String::as_str),
            Some("set"),
            "a project env var must reach the terminal; got {:?}",
            calls[0].env.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_terminal_that_has_disappeared_reports_a_failure_not_a_panic() {
        // No rule programmed -- the spawn itself fails, the "terminal has
        // disappeared" case spec §9 names. `run_open_terminal` must surface
        // it as an `Err`, not unwrap and take the window down.
        let config = Config {
            projects: vec![project("api")],
            ..Config::default()
        };
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();

        let result = run_open_terminal(
            &TerminalChoice::Detected(bombadil_core::terminal::TerminalKind::Kitty),
            &project("api"),
            &config,
            &store,
            &runner,
        );

        assert!(result.is_err());
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
