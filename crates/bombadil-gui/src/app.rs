use crate::add_project;
use crate::banner;
use crate::context_menu;
use crate::deps;
use crate::detail;
use crate::drawer;
use crate::env_editor;
use crate::index_editor;
use crate::interpreter;
use crate::job;
use crate::members_editor;
use crate::preferences;
use crate::run_tab;
use crate::scripts_editor;
use crate::settings_editor;
use crate::shell;
use crate::sidebar;
use crate::theme;
use crate::tray;
use bombadil_core::envspec::{self, EnvInputs};
use bombadil_core::ide;
use bombadil_core::model::{
    Config, DefaultVenvLocation, Override, Project, PythonPin, Settings, TerminalChoice, UvSource,
    VenvLocation,
};
use bombadil_core::process::CommandRunner;
use bombadil_core::pyproject::PyProject;
use bombadil_core::secrets::{KeyringSecretStore, SecretKey, SecretStore};
use bombadil_core::store::Loaded;
use bombadil_core::terminal::open::secret_values;
use bombadil_core::terminal::{self, Os, TerminalKind};
use bombadil_core::uv;
use bombadil_core::uv::results::{InstalledPackage, Interpreter, TreeOutcome};
use bombadil_core::venv_path;
use futures::StreamExt;
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

/// How deep `uv tree` resolves before this build stops asking. Generous
/// rather than the smallest number that satisfies today's fixtures: the
/// Dependencies tab expands a direct dependency's whole transitive tree
/// (spec §8), and a shallow depth would silently truncate it for a project
/// with a deep dependency chain.
const DEPS_TREE_DEPTH: u32 = 20;

/// Saves the whole config to disk. A trait rather than a bare
/// `bombadil_core::store::Store` so a test can hand in a fake that records
/// what would have been written instead of touching a real file -- the same
/// shape as `CommandRunner`. Implemented for `Store` below; `main.rs` wires
/// the real one in after `boot`.
pub trait ConfigPersist: Send + Sync {
    fn save(&self, config: &Config) -> Result<(), String>;

    /// Copies the config file aside before an import replaces it.
    /// `Ok(None)` when there is nothing to copy.
    fn backup(&self) -> Result<Option<PathBuf>, String> {
        Ok(None)
    }
}

impl ConfigPersist for bombadil_core::store::Store {
    fn save(&self, config: &Config) -> Result<(), String> {
        bombadil_core::store::Store::save(self, config).map_err(|e| e.to_string())
    }

    fn backup(&self) -> Result<Option<PathBuf>, String> {
        self.backup_copy(&bombadil_core::store::stamp(std::time::SystemTime::now()))
            .map_err(|e| e.to_string())
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
    /// Whether a tray icon is actually on screen.
    ///
    /// The close button hides the window only when this is true. False means
    /// no StatusNotifierItem host, no tray, and therefore nothing that could
    /// bring a hidden window back -- so it closes instead. Defaults to false
    /// so the safe behaviour is the one you get by forgetting to set it.
    pub tray_is_running: bool,
    /// The whole config, and the *only* copy of the project list.
    ///
    /// There was once an `App::projects` beside this, a clone taken in
    /// `App::new` and described as a convenience view. It was not one: every
    /// editor writes `config.projects`, while Sync, Open terminal and
    /// Recreate venv read the clone -- so a variable, script, index selection
    /// or member choice edited in a session reached nothing Bombadil ran
    /// until the app was restarted. Deleting the field is what makes that
    /// whole class unrepresentable; `entries` below stays because it is
    /// genuinely derived state (probe results) that no config field carries.
    ///
    /// `view` reads `config` as a whole too, recomputing
    /// `banner::problems(&app.config)` on every render rather than caching it
    /// here -- see `banner`'s module doc.
    pub config: Config,
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
    /// The one uv command running, if any: which project, what, since when.
    ///
    /// Spec line 121 requires at most one mutating uv command per project.
    /// A single slot rather than one per project is deliberate and slightly
    /// stricter: there is one shared output drawer, so a second concurrent
    /// command -- even for a different project -- would interleave its
    /// transcript into the first one's. Views read it through [`can_start`]
    /// to disable every other uv-starting control.
    pub busy: Option<crate::busy::Busy>,
    /// Finished commands, newest first. See `busy::push`.
    pub toasts: Vec<crate::busy::Toast>,
    /// The last GitHub lookup per project, by id. See `github_tab`.
    pub github: std::collections::HashMap<uuid::Uuid, crate::github_tab::GitHubState>,
    /// Projects whose lookup is running; Refresh is disabled for them and
    /// ticks skip them.
    pub github_in_flight: std::collections::HashSet<uuid::Uuid>,
    /// `uv sync --check` per project, from the last probe round.
    pub sync_checks: std::collections::HashMap<uuid::Uuid, uv::results::SyncCheck>,
    /// The interpreters on this machine (pre-releases are filtered in
    /// `health`); `None` until listed or when listing failed.
    pub installed_pythons: Option<Vec<String>>,
    /// The token being typed in Preferences > GitHub; cleared once saved.
    pub github_token_draft: String,
    /// How Bombadil reaches GitHub, as Preferences shows it; `None` until checked.
    pub github_source: Option<String>,
    /// The github.com accounts gh is signed in to; `None` until listed.
    pub github_accounts: Option<Vec<bombadil_core::github::gh::Account>>,
    /// An "Add account" in progress.
    pub github_login: Option<crate::github_tab::GitHubLogin>,
    /// Set by Cancel; the running `gh auth login` is killed when it is.
    pub github_login_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// The last event announced per pull request URL, so projects sharing
    /// one pull request (worktrees, monorepo members) announce it once.
    pub github_notified: std::collections::HashMap<
        String,
        (
            String,
            std::mem::Discriminant<bombadil_core::github::transition::Event>,
        ),
    >,
    /// The clock as of the last frame tick: what views measure elapsed time
    /// and the spinner's angle against. Only advances while `animating`.
    pub now: std::time::Instant,
    /// When the window last regained focus and re-read the selected project.
    pub last_focus_refresh: Option<std::time::Instant>,
    /// The sidebar's edge is being dragged. The pointer is listened to only
    /// while this is true.
    pub sidebar_drag: bool,
    /// The window's size as last reported; written to disk only on quit.
    pub window_size: Option<iced::Size>,
    /// The tab being dragged, whether it has moved yet, and the tab it last
    /// swapped with (not swapped with again until the pointer leaves it).
    pub tab_drag: Option<(Tab, Vec<String>, Option<Tab>)>,
    /// The sidebar's filter text.
    pub project_filter: String,
    /// A picked folder held several projects: which to add.
    pub project_chooser: Option<Vec<PathBuf>>,
    /// The open "Add local package" dialog.
    pub add_local: Option<crate::add_local::Draft>,
    /// The Dependencies tab's filter text.
    pub dep_filter: String,
    /// The project whose "Upgrade all" is waiting for a yes, by id: the list
    /// can change while it waits.
    pub confirm_upgrade_all: Option<uuid::Uuid>,
    /// What the tray menu last showed.
    pub tray_menu: Vec<crate::tray::MenuEntry>,
    /// The open window, or `None` while Bombadil lives only in the tray.
    pub window: Option<iced::window::Id>,
    /// How to open the window again (icon, app id); its size comes from
    /// `window_size`.
    pub window_settings: iced::window::Settings,
    /// The in-progress add-project dialog, if the user has picked a manifest.
    /// `None` before a manifest is chosen and after Cancel -- there is
    /// nothing else for the dialog to show in either state.
    pub add_project: Option<add_project::Draft>,
    /// Where confirming the add-project dialog saves the config. A trait
    /// object, not a bare `Store`, so a test can substitute a fake -- see
    /// `ConfigPersist`.
    pub persist: Arc<dyn ConfigPersist>,
    /// Whether a path exists. `|_| true` until `lib.rs` wires the real
    /// check, so tests with made-up paths still select and probe.
    pub path_exists: fn(&std::path::Path) -> bool,
    /// Projects whose `pyproject.toml` was gone when last looked at.
    pub missing: std::collections::HashSet<uuid::Uuid>,
    /// Each project's run, live or the last one ended (Run tab).
    pub runs: std::collections::HashMap<uuid::Uuid, run_tab::Run>,
    /// What to do once a project's run has stopped (a queued Sync).
    pub after_stop: std::collections::HashMap<uuid::Uuid, Message>,
    /// "Stop the run and continue?": the project and what continues.
    pub confirm_stop: Option<(uuid::Uuid, Box<Message>)>,
    /// "A program is still running. Stop it and quit?"
    pub confirm_quit: bool,
    /// Quit was confirmed: exit as soon as no run is live.
    pub quitting: bool,
    /// The selected project's `[project.scripts]` names, for the Run tab.
    pub entry_points: Option<(uuid::Uuid, Vec<String>)>,
    /// The Run tab's add/edit form.
    pub command_draft: run_tab::CommandDraft,
    /// The selected project's `just` recipes.
    pub just: Option<(uuid::Uuid, run_tab::JustState)>,
    /// Arguments typed for each recipe, by recipe name.
    pub just_args: std::collections::HashMap<String, String>,
    /// The recipe whose source is open, and that source.
    pub just_shown: Option<(String, String)>,
    /// Each project's installed list as last read, for "what changed".
    pub installed: std::collections::HashMap<uuid::Uuid, Vec<uv::results::InstalledPackage>>,
    /// Projects whose next installed list follows an action, mapped to that
    /// action's success toast text, which the summary is appended to.
    pub awaiting_change: std::collections::HashMap<uuid::Uuid, String>,
    /// The selected project's last change, for the Dependencies tab.
    pub last_change: Option<(uuid::Uuid, String)>,
    /// The package whose details panel is open, and its METADATA once read.
    pub dep_details: Option<(
        uuid::Uuid,
        String,
        Option<bombadil_core::metadata::PackageInfo>,
    )>,
    /// The last vulnerability check per environment (by venv path).
    pub vulns: std::collections::HashMap<PathBuf, VulnCheck>,
    /// Advisories already fetched this session, by id.
    pub vuln_details:
        Arc<std::sync::Mutex<std::collections::HashMap<String, bombadil_core::osv::Vuln>>>,
    /// Where advisories come from: osv.dev, or a fake in tests.
    pub osv: Arc<dyn bombadil_core::osv::Http>,
    /// Preferences › Disk: sizes, a pending delete, a prune's starting size.
    pub disk: crate::disk::State,
    /// The Find package dialog: the query, and every project's lock once read.
    pub find: Option<(String, Option<Vec<crate::find_package::Entry>>)>,
    /// The "New project" dialog, when open.
    pub new_project: Option<crate::new_project::Draft>,
    /// `uv init` is running for the dialog.
    pub creating_project: bool,
    /// Preferences > Backup: the export passphrase and an import in progress.
    pub backup: crate::backup_panel::State,
    /// The open context menu (spec §8: right-click a project), if any.
    /// `None` before the first right-click and after it closes -- there is
    /// nothing else for the menu to show in either state, the same shape
    /// `add_project` uses for its own dialog.
    pub context_menu: Option<context_menu::ContextMenu>,
    /// The pending Recreate venv confirmation (spec §8), if any. Deleting an
    /// environment cannot be undone, so the press that starts the action only
    /// fills this in -- `RecreateVenvConfirmed` is the only message that
    /// reaches `venv_path::delete`.
    pub confirm_recreate: Option<context_menu::ConfirmRecreate>,
    /// The pending "syncing will delete this environment" confirmation.
    /// `Message::PinChangeConfirmed` is the only message that reaches a
    /// pinned sync against an environment on another version.
    pub confirm_pin_change: Option<context_menu::ConfirmPinChange>,
    /// The pending "this environment will stop existing" confirmation.
    pub confirm_environment_removal: Option<context_menu::ConfirmEnvironmentRemoval>,
    /// Which top-bar menu is open, if any.
    ///
    /// One field rather than a `bool` per menu: two menus open at once is a
    /// state neither of them wants, and a `bool` each makes that
    /// representable and then someone has to remember to close the other.
    pub open_menu: Option<shell::TopMenu>,
    /// Which modal is open over the application, if any.
    ///
    /// This is what reaches the bundled licence text Apache-2.0 section 4 and
    /// OFL 1.1 oblige a distributed build to carry: `Modal::Licences` renders
    /// all three in full. It used to be an expandable footer taking up the
    /// bottom of every frame -- the obligation is that the user *can* read
    /// thousands of words of boilerplate, not that they always are.
    pub modal: Option<shell::Modal>,
    /// What Preferences is showing, or `None` when it is closed.
    ///
    /// Separate from `modal`: that is a stack of read-only cards with a Close
    /// button, and this is a working surface whose controls write through to
    /// `Config`. Sharing the field would mean one Close doing two different
    /// things.
    pub preferences: Option<preferences::State>,
    /// The Dependencies tab (spec §8) for whichever project it was last
    /// fetched for, paired with that project's index so a result that
    /// arrives after the selection has moved on is never shown against the
    /// wrong project -- the same staleness guard `entries`/`VenvProbed` uses.
    /// `None` before the first fetch lands; `Err` once one has landed that
    /// could not read the environment -- which the tab reports, rather than
    /// showing rows that would claim nothing is installed.
    pub deps: Option<(usize, Result<deps::DepsTab, String>)>,
    /// Newer releases for the selected project's packages, by project index.
    pub outdated: Option<(usize, Result<Vec<uv::results::Outdated>, String>)>,
    /// The last failed uv command per project, diagnosed; cleared by its next
    /// success.
    pub last_failure: std::collections::HashMap<
        uuid::Uuid,
        (crate::busy::Action, bombadil_core::doctor::Diagnosis),
    >,
    /// The version cell open for editing on the Dependencies tab, if any.
    pub dep_edit: Option<deps::DepEdit>,
    /// Tree or All packages, on the Dependencies tab.
    pub deps_view: deps::DepsView,
    /// What the OS last said its appearance is. Only read when the setting
    /// is `System`. Dark until the first answer lands.
    pub system_mode: iced::theme::Mode,
    /// What the user has typed so far into each `config.indexes` row's
    /// credential textbox (spec §7, Task 4) -- always the same length as
    /// `config.indexes`, kept in step by `IndexAdded`/`IndexRemoved`.
    ///
    /// Deliberately *not* a `Config` field: `IndexAuth` has no plaintext
    /// variant for a password to sit in even transiently (unlike
    /// `EnvVar::value`), so there is nowhere in `Config` this could go
    /// without writing credential material to disk. See `index_editor`'s
    /// module doc for the fuller argument. Cleared the instant its text is
    /// handed to the keychain job (`IndexCredentialSaveRequested`) -- so
    /// this never holds a credential any longer than the user is actively
    /// typing it.
    pub index_credential_drafts: Vec<String>,
    /// The name each `config.indexes` row's keychain credential is actually
    /// stored under -- always the same length as `config.indexes`, kept in
    /// step by `IndexAdded`/`IndexRemoved` exactly as
    /// `index_credential_drafts` is.
    ///
    /// `SecretKey::Index` is keyed on the index's *name*, so renaming a row
    /// has to move the entry. That used to happen inside `IndexNameChanged`,
    /// which meant a keychain `get` per character typed; the rename now
    /// happens once, at `IndexEditCommitted`, and this is what still knows
    /// where the entry was left in the meantime. Seeded from the loaded
    /// config, whose names are by definition the ones the entries were last
    /// written under.
    pub index_credential_names: Vec<String>,
    /// What the user has typed into each Env vars row's value textbox but has
    /// not committed -- see `env_editor::ValueDrafts`.
    ///
    /// Deliberately *not* in `Config`, for the reason `index_credential_drafts`
    /// is not: an env var's value becomes a secret the moment the user ticks
    /// "secret", and every keystroke before that used to be written to
    /// `config.toml` and then released to free blocks by the store's atomic
    /// rename. Nothing here is ever handed to a `ConfigPersist`.
    pub env_value_drafts: env_editor::ValueDrafts,
    /// The Members tab (spec §5a, Task 5) raw fetch for whichever project it
    /// was last fetched for, paired with that project's index -- same
    /// staleness guard `deps` uses. Deliberately the *raw* fetch, not a
    /// built `members_editor::MembersTab`: the built rows depend on
    /// `app.config.projects[i].members`, which a toggle changes without a
    /// new fetch, so `view` rebuilds the tab fresh from this every render
    /// rather than caching one that a toggle would then have to remember to
    /// invalidate.
    pub members: Option<(usize, MembersFetch)>,
    /// The pending "sync would remove members" confirmation (spec §5a
    /// criterion 4), if any. `None` before the first press and after it
    /// resolves -- same shape as `confirm_recreate`.
    pub confirm_member_sync: Option<members_editor::ConfirmMemberSync>,
    /// The Settings tab's (spec §5, Task 6) custom-uv-path textbox: what the
    /// user has typed so far, kept separate from `Config` until it validates
    /// (criterion 2) -- the same "typed text isn't committed yet" shape
    /// `index_credential_drafts` uses. Seeded from the current
    /// `UvSource::Custom` path, if any, so the box is not blank on reopening
    /// a config that already names one.
    pub uv_custom_path_draft: String,
    /// The most recent validation of `uv_custom_path_draft` (criterion 2):
    /// `None` before the first Validate press. `Ok` names the version that
    /// was accepted and already written to `Settings`; `Err` is
    /// `uv::resolve`'s own text for why it was refused -- including a
    /// version below `MIN_UV_VERSION`, reported through
    /// `UvResolveError::TooOld` -- and in that case `Settings::uv_source` was
    /// never touched.
    pub uv_custom_validation: Option<Result<String, String>>,
    /// What detection found, for Settings > PyCharm: `None` until it has
    /// looked, `Some(None)` when it found nothing.
    pub pycharm_detected: Option<Option<PathBuf>>,
    /// Which pane the detail area is showing (spec §8). See [`Tab`].
    pub tab: Tab,
}

/// Which pane the detail area is showing (spec §8).
///
/// `view` renders exactly one of them. It used to concatenate all seven into
/// a single column and then stack the add-project dialog, the context menu
/// and both confirmations *below* that -- and `scrollable` appeared only in
/// `about` and `drawer`, so nothing scrolled. With a real dependency tree
/// rendered flat, "Delete and recreate" sat thousands of pixels below the
/// viewport with no way to reach it. Every task built its own pane correctly;
/// no task owned the container holding them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Dependencies,
    /// Entry points, saved commands, a picked file and Jupyter.
    Run,
    Members,
    EnvVars,
    Scripts,
    Indexes,
    GitHub,
    Doctor,
}

impl Tab {
    /// Every tab, in the order the selector renders them.
    /// Every tab, in the order the selector renders them.
    ///
    /// The split these follow: **a tab is what this project has, the Settings
    /// menu is what every project has.** Env vars and Indexes each have both
    /// halves, so they appear in both places showing different things -- the
    /// tab shows this project's own variables and index selection, the menu
    /// shows the global ones they layer over.
    ///
    /// Settings has no per-project half worth a tab (uv, terminal and the
    /// venv default are all resolved per project from Preferences), so it is
    /// the one that stayed absorbed.
    pub const ALL: [Tab; 8] = [
        Tab::Dependencies,
        Tab::Members,
        Tab::EnvVars,
        Tab::Scripts,
        Tab::Indexes,
        Tab::GitHub,
        Tab::Doctor,
        // Last: a saved order from before it existed appends it here too.
        Tab::Run,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Dependencies => "Dependencies",
            Tab::Run => "Run",
            Tab::Members => "Members",
            Tab::EnvVars => "Env vars",
            Tab::Scripts => "Scripts",
            Tab::Indexes => "Indexes",
            Tab::GitHub => "GitHub",
            Tab::Doctor => "Doctor",
        }
    }

    /// The jargon the tab's name is, for its tooltip.
    pub fn term(self) -> Option<crate::glossary::Term> {
        use crate::glossary::Term;
        match self {
            Tab::Dependencies => Some(Term::Dependency),
            Tab::Members => Some(Term::Member),
            Tab::EnvVars => Some(Term::EnvVars),
            Tab::Indexes => Some(Term::Index),
            Tab::Run | Tab::Scripts | Tab::GitHub | Tab::Doctor => None,
        }
    }

    /// How the tab is named in `config.toml`'s saved order. Never change one:
    /// a renamed key drops that tab back to the built-in position.
    pub fn key(self) -> &'static str {
        match self {
            Tab::Dependencies => "deps",
            Tab::Run => "run",
            Tab::Members => "members",
            Tab::EnvVars => "env",
            Tab::Scripts => "scripts",
            Tab::Indexes => "indexes",
            Tab::GitHub => "github",
            Tab::Doctor => "doctor",
        }
    }
}

/// The tabs in the user's saved order: unknown and repeated keys dropped,
/// any tab the saved order does not name appended in built-in order.
pub fn tab_order(keys: &[String]) -> Vec<Tab> {
    let mut order: Vec<Tab> = Vec::with_capacity(Tab::ALL.len());
    for key in keys {
        if let Some(tab) = Tab::ALL.into_iter().find(|t| t.key() == key)
            && !order.contains(&tab)
        {
            order.push(tab);
        }
    }
    for tab in Tab::ALL {
        if !order.contains(&tab) {
            order.push(tab);
        }
    }
    order
}

/// `order` with `dragged` moved into `target`'s place.
pub fn move_tab(order: &[Tab], dragged: Tab, target: Tab) -> Vec<Tab> {
    let mut order = order.to_vec();
    if let (Some(from), Some(to)) = (
        order.iter().position(|t| *t == dragged),
        order.iter().position(|t| *t == target),
    ) {
        let tab = order.remove(from);
        order.insert(to, tab);
    }
    order
}

#[derive(Debug, Clone)]
pub enum Message {
    ProjectSelected(usize),
    /// A tab in the detail area's selector was pressed (spec §8).
    TabSelected(Tab),
    /// A background venv probe finished for `projects[index]`. Carries the
    /// Python version on success; `None` covers "no venv", a probe that
    /// timed out, and a probe that failed -- all three degrade to hollow.
    /// Also carries `requires-python`, re-read from the manifest alongside
    /// the venv probe (see `probe_venv_and_requirement`) so the sidebar can
    /// tell a healthy venv from a drifted one via `sidebar::state`; `None`
    /// there covers both "no requirement" and "could not be read", the same
    /// permissive fallback `interpreter::version_satisfies` already applies.
    VenvProbed {
        index: usize,
        /// Which environment of that project was probed. A project has
        /// several probes in flight at once and they land in whatever order
        /// the filesystem answers, so the result has to say which row it
        /// belongs to -- a position would be meaningless by then.
        location: VenvLocation,
        python_version: Option<String>,
        requires_python: Option<String>,
    },
    /// The background fetch for the Dependencies tab finished for
    /// `projects[usize]` -- criterion 5: dispatched through `job::run`,
    /// never run inline, because `uv tree` on a large project is slow.
    /// Carries the index for the same staleness reason `VenvProbed` does:
    /// the user can select a different project before this lands.
    ///
    /// `Err` carries why the environment could not be read at all, so the tab
    /// can say so instead of rendering every declared package as
    /// `"(not installed)"` -- see `fetch_deps_blocking`.
    DepsLoaded(usize, Result<DepsFetch, String>),
    OutdatedLoaded(usize, Result<Vec<uv::results::Outdated>, String>),
    /// A doctor fix button, for the project with this id.
    DoctorFixRequested(uuid::Uuid, bombadil_core::doctor::DoctorFix),
    /// A doctor fix's edit to this project's pyproject.toml, written or not.
    PyprojectEdited(uuid::Uuid, Result<(), String>),
    /// Doctor tab "Update pin": install this `name==latest` on `projects[usize]`.
    DoctorPinRequested(usize, String),
    /// The Sync action was pressed for `projects[usize]`. Never gated on
    /// whether a venv already exists -- creating one is how the user
    /// recovers from not having one.
    ///
    /// Carries the index rather than resolving against `app.selected`, the
    /// same shape `OpenTerminalRequested` and `RemoveProjectRequested` use:
    /// the context menu is opened by a right-press that deliberately does
    /// *not* move the selection, so a selection-resolved Sync would run
    /// `uv sync` -- which prunes packages absent from the target's lockfile
    /// -- against an environment the user never named.
    SyncRequested(usize),
    /// The output drawer's own disclosure control. The only thing that opens
    /// or closes it -- output arriving no longer does, and neither does a
    /// command finishing.
    DrawerToggled,
    /// The drawer's Copy button: the whole transcript to the clipboard.
    DrawerCopyRequested,
    /// The drawer's Export button: ask where to save the transcript.
    DrawerExportRequested,
    /// The save dialog closed. `None` when cancelled.
    DrawerExportPicked(Option<PathBuf>),
    /// The transcript was written to the path, or the error saying why not.
    DrawerExportFinished(Result<PathBuf, String>),
    /// The sidebar's disclosure control for `projects[usize]`. Persisted, so
    /// a project you collapse stays collapsed across a restart.
    ProjectExpandToggled(usize),
    /// An environment row in the tree was pressed: make it the one Sync,
    /// Open terminal and Recreate act on.
    ///
    /// A single press, because it is not destructive: nothing is deleted, the
    /// next Sync simply builds against a different directory.
    EnvironmentActivated(usize, VenvLocation),
    /// One line of a running uv command's output, already redacted on the job
    /// thread. Spec §8: the drawer streams live output, so these arrive one
    /// at a time while the command runs rather than in a batch after it
    /// exits.
    ///
    /// Redacted *before* the message is constructed, never after: `Message`
    /// derives `Debug`, so a raw line here would put keychain material into
    /// every rendering of it. The composed environment -- the only place the
    /// set of secret values exists -- lives on the job thread, which is why
    /// the redaction has to happen there.
    SyncLine(String),
    /// A background sync finished. `success` is true only on a zero exit.
    ///
    /// `lines` is whatever the drawer has *not* already been given as
    /// `SyncLine`: a failed resolve, a timeout, a panicking job, or the note
    /// a recreate appends when the environment could not be rebuilt. A real
    /// run's uv output arrives as `SyncLine` and is not repeated here.
    SyncFinished {
        success: bool,
        lines: Vec<String>,
    },
    /// A declared dependency's version was double-clicked.
    DepVersionEditStarted {
        name: String,
        group: deps::GroupKind,
        current: String,
    },
    DepVersionDraftChanged(String),
    DepsViewSelected(deps::DepsView),
    /// The window's close button.
    CloseRequested,
    /// Preferences > Appearance: whether closing keeps Bombadil in the tray.
    CloseToTraySelected(bool),
    /// Where PyCharm was found, for the Settings panel to show. `None`:
    /// nowhere it looks.
    PycharmDetected(Option<PathBuf>),
    /// "Choose…" in Settings > PyCharm: opens the file picker.
    PycharmBrowse,
    /// The picker's answer; `None` when it was cancelled.
    PycharmPicked(Option<PathBuf>),
    /// Back to detecting PyCharm on every open.
    PycharmAutomatic,
    /// Preferences > Appearance: System, Light or Dark.
    ThemeChoiceSelected(bombadil_core::model::ThemeChoice),
    /// The OS reported its light/dark preference, at boot or on a change.
    SystemThemeChanged(iced::theme::Mode),
    /// The window regained focus.
    WindowFocused,
    /// The folder picked for "Add project".
    AddProjectFolderPicked(Option<PathBuf>),
    /// What that folder holds: how many manifests were found, and the ones
    /// not yet registered.
    AddProjectFolderScanned(PathBuf, (usize, Vec<PathBuf>)),
    /// One manifest chosen from a folder that held several.
    ProjectChosen(PathBuf),
    ProjectChooserCancelled,
    /// A path dropped on the window (not on Wayland: winit has no drop
    /// support there).
    FileDropped(PathBuf),
    /// "Add local package…" on `projects[i]`'s Dependencies tab.
    AddLocalRequested(usize),
    AddLocalPicked(uuid::Uuid, Option<PathBuf>),
    /// What the picked folder turned out to be: its package name, or why it
    /// is not a package.
    AddLocalLoaded(uuid::Uuid, PathBuf, Result<Option<String>, String>),
    AddLocalGroupSelected(deps::GroupKind),
    AddLocalEditableToggled(bool),
    AddLocalConfirmed,
    AddLocalCancelled,
    /// The sidebar's filter field.
    ProjectFilterChanged(String),
    /// The Dependencies tab's filter field.
    DepFilterChanged(String),
    /// Ctrl+F: show the Dependencies tab and focus its filter.
    FindRequested,
    /// Upgrade one package of `projects[i]` within its constraint.
    DepUpgradeRequested(usize, String),
    /// "Upgrade all": asks first.
    DepUpgradeAllRequested(usize),
    DepUpgradeAllConfirmed,
    DepUpgradeAllCancelled,
    /// A press on a tab: selects it, and may become a drag.
    TabDragStarted(Tab),
    /// The pointer entered a tab; moves the dragged tab there, if any.
    TabDragOver(Tab),
    /// A window the app opened is up.
    WindowOpened(iced::window::Id),
    /// The window was resized.
    WindowResized(iced::Size),
    /// Collapse the sidebar to its rail, or expand it back.
    SidebarToggled,
    /// A press on the sidebar's edge.
    SidebarDragStarted,
    /// The pointer's x while the sidebar's edge is held.
    SidebarDragged(f32),
    /// The left button came up somewhere, ending any drag.
    DragEnded,
    /// A keyboard shortcut, already mapped from its key.
    Shortcut(crate::shortcuts::Shortcut),
    /// F5: re-read the selected project, as a focus does, but undebounced --
    /// it was asked for.
    RefreshRequested,
    /// A frame was drawn while something animates (see [`animating`]).
    Tick(std::time::Instant),
    /// A toast's dismiss button.
    ToastDismissed(usize),
    /// A failure toast's "Show output": open the drawer's transcript.
    ShowOutput,
    /// The sidebar's Sponsor button: open the GitHub Sponsors page.
    SponsorRequested,
    /// Every two minutes while watching: look up every project's pull request.
    GitHubTick,
    /// The GitHub tab's Refresh: this project now.
    GitHubRefreshRequested(uuid::Uuid),
    GitHubLooked(Vec<(uuid::Uuid, bombadil_core::github::Lookup)>),
    /// A pull request or check run link.
    GitHubOpen(String),
    GitHubWatchSelected(bool),
    /// Redacted in `Debug`, like an index credential.
    GitHubTokenDraftChanged(index_editor::CredentialInput),
    GitHubTokenSaved,
    GitHubTokenRemoved,
    /// The toast text, or why the keychain refused.
    GitHubTokenStored(Result<String, String>),
    GitHubSourceChecked(String),
    GitHubAccountsListed(Vec<bombadil_core::github::gh::Account>),
    /// Preferences > GitHub: look pull requests up as this gh account.
    GitHubAccountSelected(String),
    /// Preferences > GitHub > Add account: `gh auth login` in the background.
    GitHubAddAccount,
    /// The one-time code gh printed, to enter at github.com/login/device.
    GitHubLoginCode(String),
    /// The new account's login, or why it failed.
    GitHubLoginFinished(Result<String, String>),
    GitHubLoginCancelled,
    /// Sign in through GitHub's device flow, for machines without gh.
    GitHubBrowserSignIn,
    /// The device flow's outcome: the token is already in the keychain.
    GitHubBrowserFinished(Result<String, String>),
    GitHubCopyLoginCode,
    /// A project's git status landed, `None` outside a repository.
    GitProbed {
        id: uuid::Uuid,
        status: Option<bombadil_core::git::GitStatus>,
    },
    /// A project's `uv sync --check`, from a probe round.
    SyncChecked {
        id: uuid::Uuid,
        check: uv::results::SyncCheck,
    },
    /// The interpreters installed on this machine, from a probe round.
    PythonsListed(Option<Vec<String>>),
    DepVersionEditCancelled,
    /// Enter in the version cell: install that exact version with `uv add`.
    DepVersionSubmitted,
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
    ///
    /// A `Result`, not a bare `Vec`: an empty list and a failed fetch are
    /// different things to show. Collapsing both to `vec![]` left the dialog
    /// reading "(checking available interpreters...)" forever with the
    /// install button suppressed -- no error, no retry.
    AddProjectInterpretersLoaded(Result<Vec<Interpreter>, String>),
    /// The dialog's "Install compatible interpreter" button was pressed:
    /// spec §8's offer to install one through uv when none is available.
    AddProjectInstallInterpreterRequested,
    /// A venv option other than "chosen folder" was picked directly.
    AddProjectVenvChoiceSelected(add_project::VenvChoice),
    /// The adopt-or-create probe for the dialog's chosen venv location
    /// finished (spec §8: the dialog states that an existing environment
    /// will be adopted, and shows its Python version).
    ///
    /// Carries the path it probed, not just the answer: the user can change
    /// the choice again while a probe is in flight, and an older result
    /// landing last would describe a folder that is no longer chosen. Same
    /// "drop the stale result" discipline as `VenvProbed` and
    /// `ContextMenuTerminalsDetected`.
    AddProjectVenvOutcomeProbed(PathBuf, add_project::VenvOutcome),
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
    /// where the project landed in `config.projects`/`entries`, so the venv status
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
    /// The detail header's terminal button: open a terminal for
    /// `projects[usize]` in one press, with no menu in between.
    ///
    /// The same composed environment, activated venv and project-directory
    /// cwd `OpenTerminalRequested` delivers -- it goes through the identical
    /// `run_open_terminal`. All that differs is who picks the terminal:
    /// `context_menu::one_press_choice` rather than the user, from the
    /// configured default or, failing that, whatever is installed.
    OpenTerminalHereRequested(usize),
    /// The detection a one-press open had to run first, because nothing was
    /// configured. Carries the project's id, not its index: the list can
    /// change while a probe is in flight, and an index would then name a
    /// different project. The third field is the environment a tray press
    /// asked for, when it was not the active one.
    OpenTerminalHereDetected(
        uuid::Uuid,
        Vec<TerminalKind>,
        Option<bombadil_core::model::VenvLocation>,
    ),
    /// The background open finished. `Ok(())` on a successful spawn; `Err`
    /// carries text `terminal::open::open` has already redacted of any
    /// secret the composed environment resolved.
    OpenTerminalFinished(Result<(), String>),
    /// The window's close button was pressed, or a tray menu asked for one
    /// of the three things a tray can ask for.
    ///
    /// `exit_on_close_request(false)` is what routes the close button here
    /// instead of straight to the window manager, so `update` gets to decide
    /// between hiding and quitting.
    Tray(tray::TrayCommand),
    /// The PyCharm button was pressed for `projects[usize]`: find the
    /// launcher and open the project folder in it, carrying the same composed
    /// environment `OpenTerminalHereRequested` delivers.
    OpenPycharmRequested(usize),
    /// The background open finished. Same redaction guarantee as
    /// `OpenTerminalFinished`, and for the same reason -- `ide::open` composes
    /// an environment that can hold keychain values.
    OpenPycharmFinished(Result<Option<String>, String>),
    /// "Remove from list" was picked for `projects[usize]`. Criterion 4:
    /// removes the project from `config.projects`/`entries` in memory,
    /// synchronously, and dispatches a background persist -- the same
    /// in-memory-first shape `AddProjectConfirmed` uses. Never touches the
    /// venv on disk -- "Remove from list" unregisters a project, it does not
    /// destroy anything. That is now a rule this arm has to keep rather than
    /// one the crate enforces: `venv_path::delete` exists, and it needs no
    /// `CommandRunner`, so nothing outside this arm's own body prevents a
    /// future edit from wiring it in. `RecreateVenvConfirmed` is the only
    /// message that may reach it.
    RemoveProjectRequested(usize),
    /// "Locate…" on a project whose folder is missing: opens a file picker.
    LocateRequested(uuid::Uuid),
    /// The picked `pyproject.toml`, or `None` when the picker was cancelled.
    LocatePicked(uuid::Uuid, Option<PathBuf>),
    // --- Run lane ---
    /// Start `uv run --no-sync <args>` in a project's lane.
    RunRequested {
        project: uuid::Uuid,
        label: String,
        args: Vec<String>,
    },
    RunLine(uuid::Uuid, String),
    RunFinished(uuid::Uuid, run_tab::RunEnd),
    RunStopRequested(uuid::Uuid),
    StopThenConfirmed,
    StopThenCancelled,
    QuitConfirmed,
    QuitCancelled,
    // --- Run tab ---
    EntryPointsLoaded(uuid::Uuid, Vec<String>),
    CommandDraftLabel(String),
    CommandDraftCommand(String),
    CommandSaved,
    CommandEdit(usize),
    CommandRemoved(usize),
    RunFileRequested,
    RunFilePicked(uuid::Uuid, Option<PathBuf>),
    JustLoaded(uuid::Uuid, run_tab::JustState),
    JustArgsChanged(String, String),
    /// Show (or hide) a recipe's source.
    JustShowRequested(String),
    JustShown(String, Result<String, String>),
    /// Settings: which `just` runs recipes.
    JustSourceSelected(bombadil_core::model::JustSource),
    CheckVulnerabilitiesSelected(bool),
    /// A package row's name was pressed: open or close its details.
    DepDetailsToggled(usize, String),
    DepDetailsLoaded(
        uuid::Uuid,
        String,
        Option<bombadil_core::metadata::PackageInfo>,
    ),
    /// Install exactly `name==version` in `group` (Upgrade to a fixed version).
    DepPinRequested(usize, String, String, deps::GroupKind),
    /// An environment's vulnerability check: its venv, the installed-list
    /// key it was made for, and the vulnerable packages.
    VulnsChecked(
        PathBuf,
        String,
        Result<std::collections::BTreeMap<String, Vec<bombadil_core::osv::Finding>>, String>,
    ),
    // --- Disk ---
    /// A measurement, and whether it is the one a prune asked for.
    DiskMeasured(crate::disk::State, bool),
    DiskDeleteRequested(crate::disk::Delete),
    DiskDeleteConfirmed,
    DiskDeleteCancelled,
    DiskDeleted(crate::disk::Delete, Result<(), String>),
    CachePruneRequested,
    /// The projects whose `pyproject.toml` a background sweep didn't find.
    MissingSwept(Vec<uuid::Uuid>),
    // --- Find package ---
    FindOpened,
    FindIndexed(Vec<crate::find_package::Entry>),
    FindQuery(String),
    /// A result: select that project and filter its Dependencies to `name`.
    FindPicked(uuid::Uuid, String),
    FindClosed,
    // --- New project ---
    NewProjectRequested,
    NewProjectName(String),
    NewProjectBrowse,
    NewProjectLocation(Option<PathBuf>),
    NewProjectKind(crate::new_project::InitKind),
    NewProjectPython(Option<String>),
    NewProjectGit(bool),
    NewProjectCreate,
    NewProjectCancelled,
    NewProjectCreated(Result<PathBuf, String>),
    // --- Move to another computer (Preferences > Backup) ---
    BackupPassphraseChanged(index_editor::CredentialInput),
    BackupConfirmChanged(index_editor::CredentialInput),
    BackupExportRequested,
    BackupExportPicked(Option<PathBuf>),
    BackupExportFinished(Result<bombadil_core::backup::Exported, String>),
    BackupImportRequested,
    BackupImportPicked(Option<PathBuf>),
    BackupImportRead(PathBuf, Result<bombadil_core::backup::Bundle, String>),
    BackupUnlockChanged(index_editor::CredentialInput),
    BackupUnlockRequested,
    /// The file it was unlocked for, so a late result never lands on another.
    BackupUnlocked(PathBuf, Result<bombadil_core::backup::Secrets, String>),
    BackupMapChanged(usize, String),
    BackupMapBrowse(usize),
    BackupMapPicked(usize, Option<PathBuf>),
    BackupModeSelected(bombadil_core::backup::import::Mode),
    BackupImportConfirmed,
    BackupImportFinished(
        Result<
            (
                Config,
                bombadil_core::backup::import::Report,
                Option<PathBuf>,
            ),
            String,
        >,
    ),
    BackupImportCancelled,
    /// A background save of `config.toml` finished. `what` names the edit
    /// in the drawer line a failure leaves: "<what> failed to persist".
    Persisted {
        what: &'static str,
        success: bool,
    },
    /// "Recreate venv" was picked for `projects[usize]` (spec §8). Opens the
    /// confirmation and dispatches *nothing*: recreating is delete-then-sync,
    /// the delete cannot be undone, and if the sync then fails the user has
    /// no environment at all. `context_menu::recreate_confirmation` is the
    /// sentence they get to read first.
    RecreateVenvRequested(usize),
    /// The confirmation was accepted: delete the environment and rebuild it.
    /// The only message that reaches `venv_path::delete`.
    RecreateVenvConfirmed,
    /// The confirmation was dismissed. Nothing has been deleted.
    RecreateVenvCancelled,
    /// The pin-change confirmation was accepted: run the sync that deletes
    /// the environment and rebuilds it on the pinned interpreter.
    PinChangeConfirmed,
    /// It was declined. Nothing runs and nothing is deleted.
    PinChangeCancelled,
    /// "Open folder" was picked for `projects[usize]` (spec §8): show the
    /// project's directory in the platform's file manager. Non-destructive,
    /// and dispatched as background work like every other process spawn here.
    OpenFolderRequested(usize),
    /// The background open finished. `Err` carries the spawn failure --
    /// nothing composed reaches this path, so there is nothing to redact.
    OpenFolderFinished(Result<(), String>),
    /// A top-bar menu button was pressed: open that menu, or close it if it
    /// is already open. A press that did nothing would read as a broken
    /// control.
    MenuToggled(shell::TopMenu),
    /// A press landed outside the open menu: close it without choosing.
    MenuDismissed,
    /// A menu entry was chosen. Opening a modal always closes the menu --
    /// leaving it hanging over the modal it opened would be two layers
    /// claiming focus at once.
    ModalOpened(shell::Modal),
    /// The modal's Close button, or a press on its scrim.
    ModalClosed,
    /// A Settings-menu entry: open Preferences straight onto that section, in
    /// Global scope.
    PreferencesSectionOpened(preferences::Section),
    /// Its Close button, or a press on its scrim. Not an undo: every control
    /// in there has already written through to `Config` and persisted.
    PreferencesClosed,
    /// The scope switch: whose settings to show.
    PreferencesScopeSelected(preferences::Scope),
    /// The section list down the left.
    PreferencesSectionSelected(preferences::Section),
    /// The Python section's version box, in `scope`. An empty box is not a
    /// pin -- typing and clearing must leave `Unpinned`, not a `Version("")`
    /// that uv would refuse with a confusing error.
    PreferencesPythonPinChanged(preferences::Scope, String),
    /// "Inherit from global" was ticked back on for `section` in `scope`.
    /// Only meaningful for a project scope; Global has nothing to inherit.
    PreferencesInheritToggled(preferences::Scope, preferences::Section),
    /// The uv section's choice, in `scope`.
    PreferencesUvSourceOverridden(preferences::Scope, UvSource),
    /// The Terminal section's choice, in `scope`.
    PreferencesTerminalOverridden(preferences::Scope, TerminalChoice),
    /// Move one environment to another location. Carries where it is now,
    /// because that is how an environment is identified.
    EnvironmentLocationSelected(uuid::Uuid, VenvLocation, VenvLocation),
    /// Choose a folder for one.
    EnvironmentPickFolderRequested(uuid::Uuid, VenvLocation),
    /// That picker finished. `None` when cancelled, which leaves it alone.
    EnvironmentFolderPicked(uuid::Uuid, VenvLocation, Option<PathBuf>),
    /// Add an environment to `Uuid`: opens a folder picker.
    ///
    /// A picker rather than a generated name. The first version invented
    /// `.venv-1` and added it silently, which told the user nothing about
    /// where it was -- and an environment is its directory, so choosing that
    /// directory *is* creating it. It also has to be a picker for the case
    /// this exists for: pointing at an environment that already exists on
    /// disk, including one another project uses.
    EnvironmentAdded(uuid::Uuid),
    /// The picker finished. `None` when cancelled, which adds nothing.
    EnvironmentAddFolderPicked(uuid::Uuid, Option<PathBuf>),
    /// Set one environment's own interpreter pin.
    EnvironmentPinChanged(uuid::Uuid, VenvLocation, String),
    /// Remove one. Opens a confirmation rather than removing: an environment
    /// stopping existing is the same act as Recreate venv.
    EnvironmentRemoveRequested(uuid::Uuid, VenvLocation),
    /// The confirmation was accepted.
    EnvironmentRemovalConfirmed,
    /// It was declined. Nothing is removed.
    EnvironmentRemovalCancelled,
    /// The Env vars tab's "Add variable" button was pressed for `scope`
    /// (spec §5, Task 2).
    EnvVarAdded(env_editor::EnvScope),
    /// A row's "remove" button was pressed.
    EnvVarRemoved(env_editor::EnvScope, usize),
    /// A row's key textbox changed. Never secret material -- a variable's
    /// name is never hidden from the keychain, only its value.
    EnvVarKeyChanged(env_editor::EnvScope, usize, String),
    /// A row's value textbox changed. Carries the typed text in plaintext,
    /// which is sound only because `env_editor::rows` renders no value
    /// textbox at all for a row that is already marked secret -- see
    /// `env_editor`'s module doc for the whole argument. This can never be
    /// produced for a secret row.
    ///
    /// Lands in `App::env_value_drafts` and nowhere else: it dispatches no
    /// persist and never touches `Config`, so a value the user is still
    /// deciding whether to mark secret cannot reach `config.toml` on the way.
    EnvVarValueChanged(env_editor::EnvScope, usize, String),
    /// The user committed a row's typed value -- Enter in the textbox, or the
    /// row's "save value" button. Carries no text: the value is already in
    /// `App::env_value_drafts`, and `env_editor::commit_value` is what moves
    /// it into `Config`. This is the only path by which a plaintext value
    /// reaches disk.
    EnvVarValueCommitted(env_editor::EnvScope, usize),
    /// A row's "secret" checkbox was toggled. Carries no value at all --
    /// marking secret moves the plaintext sitting in this row's draft (or in
    /// `app.config`, if the user had already committed it) into a `job::run`
    /// closure by ordinary move semantics inside `update`, never through this
    /// message. See `env_editor`'s module doc.
    EnvVarSecretToggled(env_editor::EnvScope, usize),
    /// A secret toggle's keychain write (or delete) plus persist finished.
    ///
    /// Separate from [`Message::Persisted`] because this one is
    /// not a report -- it is the instruction to apply the toggle to
    /// `app.config`, which deliberately has not been touched until now.
    /// Carries the row it belongs to, and no value: what the keychain now
    /// holds never travels through a message.
    EnvVarSecretWritten {
        scope: env_editor::EnvScope,
        index: usize,
        success: bool,
    },
    /// The Scripts tab's "Add script" button was pressed for `scope` (spec
    /// §6, Task 3): open the async file picker for the script to run.
    /// Dispatches `pick_file` through `Task::perform` rather than adding a
    /// row synchronously -- there is no path to add until the picker
    /// resolves, and `rfd::AsyncFileDialog` never blocks `update`.
    ScriptAddRequested(scripts_editor::ScriptScope),
    /// The script picker finished. `None` when the user cancelled it.
    ScriptFilePicked(scripts_editor::ScriptScope, Option<PathBuf>),
    /// A row's "remove" button was pressed.
    ScriptRemoved(scripts_editor::ScriptScope, usize),
    /// A row's "enabled" checkbox was toggled (criterion 2): flips the flag
    /// in place, never removes the row.
    ScriptEnabledToggled(scripts_editor::ScriptScope, usize),
    /// A row's "up" button was pressed: move it one position earlier within
    /// its own scope (criterion 1). Never crosses the global/project
    /// boundary -- see `scripts_editor`'s module doc for why that would be
    /// meaningless against `envspec::compose`'s own ordering.
    ScriptMovedUp(scripts_editor::ScriptScope, usize),
    /// A row's "down" button was pressed: the mirror of `ScriptMovedUp`.
    ScriptMovedDown(scripts_editor::ScriptScope, usize),
    /// The Indexes tab's "Add index" button was pressed (spec §7, Task 4).
    IndexAdded,
    /// A row's "remove" button was pressed.
    IndexRemoved(usize),
    /// A row's name textbox changed. Plaintext, and validated for
    /// credential-variable collisions (criterion 2) by `index_editor::view`
    /// re-deriving `collision_message` on every render -- never secret
    /// material, the same as an env var's key.
    ///
    /// In-memory only: no persist, and no keychain call. Renaming an index
    /// has to move its credential, and doing that from here meant a keychain
    /// `get` per character typed. `IndexEditCommitted` is where both happen,
    /// once.
    IndexNameChanged(usize, String),
    /// A row's URL textbox changed. In-memory only, same as
    /// `IndexNameChanged`: `IndexEditCommitted` is what persists it.
    IndexUrlChanged(usize, String),
    /// The user committed a row's typed fields -- Enter in one of its
    /// textboxes, or the row's "save" button. Persists the config, and moves
    /// the row's keychain credential if its name has changed since the last
    /// commit (`App::index_credential_names`).
    IndexEditCommitted(usize),
    /// The kind button was pressed: flips `Default`/`Extra` for this row.
    IndexKindChanged(usize, bombadil_core::model::IndexKind),
    /// The "default for new projects" checkbox was toggled.
    IndexDefaultToggled(usize),
    /// One of the three auth-kind buttons was pressed.
    IndexAuthChoiceChanged(usize, index_editor::AuthChoice),
    /// The username textbox changed. Plaintext -- `IndexAuth::Basic`'s
    /// username is not secret, only the password/token paired with it is.
    /// In-memory only, same as `IndexUrlChanged`.
    IndexUsernameChanged(usize, String),
    /// The credential textbox changed. Carries the typed text wrapped in
    /// `index_editor::CredentialInput`, whose hand-written `Debug` never
    /// prints it -- see `index_editor`'s module doc for why, unlike an env
    /// var's value, this can never be an ordinary `String` field here.
    IndexCredentialChanged(usize, index_editor::CredentialInput),
    /// The "save credential" button was pressed for `config.indexes[usize]`.
    /// Only ever dispatched when `index_editor::collision_message` is `None`
    /// for this row's current name (criterion 2) -- `view` disables the
    /// button otherwise, so this arm never has to re-check before writing.
    IndexCredentialSaveRequested(usize),
    /// The keychain write and persist behind a "save credential" press
    /// finished. Like [`Message::EnvVarSecretWritten`], this is the
    /// instruction to clear the draft and record the account the credential
    /// now lives under -- neither of which happens until the write has landed.
    IndexCredentialWritten {
        index: usize,
        success: bool,
    },
    /// A project index checkbox was toggled: `usize` is the project's index
    /// in `app.config.projects`/`app.config.projects`, `String` the index name
    /// (criterion 3). Adds or removes the association only -- never reaches
    /// `Config::indexes`, so the index itself is untouched either way.
    ProjectIndexToggled(usize, String),
    /// The background fetch for the Members tab (spec §5a, Task 5) finished
    /// for `projects[usize]`: discovery plus `uv pip list`, dispatched
    /// through `job::run` for the same reason `DepsLoaded` is -- discovery
    /// walks the filesystem and `uv pip list` spawns a process, neither of
    /// which may run on the render thread. Carries the index for the same
    /// staleness reason `DepsLoaded` does.
    MembersLoaded(usize, MembersFetch),
    /// A member's checkbox was pressed: `usize` is the project's index,
    /// `PathBuf` the member's path as carried on its `MemberRow`. Flips the
    /// selection through `members_editor::toggle` (criterion 3: that
    /// function is the only place the flip is decided) and persists.
    MemberToggled(usize, PathBuf),
    /// The Members tab's "Sync members" button was pressed for
    /// `projects[usize]`. If nothing selected is currently installed and
    /// about to be dropped, syncs immediately; otherwise opens
    /// `confirm_member_sync` and dispatches nothing yet (criterion 4: the UI
    /// states what will be removed before the user confirms).
    MembersSyncRequested(usize),
    /// The pending member-removal confirmation was accepted: run the
    /// member-aware sync it described.
    MembersSyncConfirmed,
    /// The pending member-removal confirmation was dismissed. Nothing was synced.
    MembersSyncCancelled,
    /// One of the Settings tab's (spec §5, Task 6) three no-validation uv
    /// source buttons was pressed: applies immediately and persists
    /// (criterion 1) -- unlike `Custom`, none of these three names a path the
    /// user just typed, so there is nothing to validate first.
    SettingsUvSourceChoiceChanged(settings_editor::UvSourceChoice),
    /// The custom-uv-path textbox changed. Plaintext, never secret material,
    /// and never persisted on its own -- see `App::uv_custom_path_draft`.
    SettingsUvCustomPathChanged(String),
    /// "Validate and use" was pressed for the typed custom path (criterion
    /// 2): dispatches `job::run`, never resolved inline -- probing a uv
    /// binary spawns it, the same reason every other uv call in this file
    /// goes through `job::run`.
    SettingsUvCustomValidateRequested,
    /// The background validation finished. Only `Ok` writes
    /// `Settings::uv_source` and persists; `Err` -- including a version below
    /// `MIN_UV_VERSION` -- only updates what the tab shows, so a bad path
    /// never reaches `config.toml`.
    SettingsUvCustomValidated(Result<uv::ResolvedUv, String>),
    /// "Alongside each project" was pressed for `default_venv_location`
    /// (criterion 3): applies immediately and persists. Never touches any
    /// project's existing venv -- see `settings_editor`'s module doc.
    SettingsDefaultVenvLocationAlongsideSelected,
    /// "Choose a central folder..." was pressed: open the async folder
    /// picker, the same one `AddProjectPickVenvFolderRequested` uses.
    SettingsDefaultVenvLocationPickFolderRequested,
    /// The folder picker finished. `None` when the user cancelled it --
    /// leaves the previous `default_venv_location` untouched.
    SettingsDefaultVenvLocationFolderPicked(Option<PathBuf>),
}

/// What background loading produced for the Members tab (spec §5a): the
/// project's workspace classification, every `pyproject.toml` discovery
/// found below it, and what `uv pip list` reports installed.
/// `members_editor::build` -- pure, tested on its own -- turns this into the
/// tab; nothing here does that joining itself, the same split `DepsFetch`
/// and `deps::build` use.
#[derive(Debug, Clone)]
pub struct MembersFetch {
    kind: bombadil_core::members::WorkspaceKind,
    discovered: Vec<bombadil_core::members::DiscoveredMember>,
    installed: Vec<InstalledPackage>,
}

impl App {
    /// The mode to render in: the explicit choice, or the OS's for `System`.
    pub fn theme_mode(&self) -> iced::theme::Mode {
        use bombadil_core::model::ThemeChoice;
        match self.config.settings.theme {
            ThemeChoice::Light => iced::theme::Mode::Light,
            ThemeChoice::Dark => iced::theme::Mode::Dark,
            ThemeChoice::System => self.system_mode,
        }
    }

    pub fn new(config: Config) -> Self {
        let selected = (!config.projects.is_empty()).then_some(0);
        // No probe yet at construction time -- every project starts hollow;
        // `boot` kicks off the real probes and `update` fills results in as
        // they land.
        let entries = sidebar::entries(&config);
        // One blank draft per already-configured index, matching what
        // `IndexAdded`/`IndexRemoved` keep in step with `config.indexes`
        // from here on.
        let index_credential_drafts = vec![String::new(); config.indexes.len()];
        // Where each row's credential currently lives -- the loaded config's
        // own names, since that is what any stored entry was written under.
        let index_credential_names = config
            .indexes
            .iter()
            .map(|index| index.name.clone())
            .collect();
        // Seeded from whatever `UvSource::Custom` path is already configured,
        // if any, so the Settings tab's textbox is not blank on reopening a
        // config that already names one -- an empty draft for every other
        // source, since there is nothing to pre-fill.
        let uv_custom_path_draft = match &config.settings.uv_source {
            UvSource::Custom { path } => path.display().to_string(),
            _ => String::new(),
        };
        Self {
            // False until `main` says otherwise: without a tray, hiding the
            // window would be a one-way trip.
            tray_is_running: false,
            config,
            selected,
            entries,
            drawer: drawer::Drawer::default(),
            load_problems: Vec::new(),
            busy: None,
            toasts: Vec::new(),
            github: Default::default(),
            github_in_flight: Default::default(),
            sync_checks: Default::default(),
            installed_pythons: None,
            github_token_draft: String::new(),
            github_source: None,
            github_accounts: None,
            github_login: None,
            github_login_cancel: None,
            github_notified: Default::default(),
            now: std::time::Instant::now(),
            last_focus_refresh: None,
            sidebar_drag: false,
            window_size: None,
            tab_drag: None,
            tray_menu: Vec::new(),
            confirm_upgrade_all: None,
            dep_filter: String::new(),
            project_filter: String::new(),
            add_local: None,
            project_chooser: None,
            window: None,
            window_settings: iced::window::Settings::default(),
            add_project: None,
            persist: Arc::new(NullPersist),
            path_exists: |_| true,
            missing: Default::default(),
            runs: Default::default(),
            after_stop: Default::default(),
            confirm_stop: None,
            confirm_quit: false,
            quitting: false,
            entry_points: None,
            command_draft: Default::default(),
            just: None,
            just_args: Default::default(),
            just_shown: None,
            installed: Default::default(),
            awaiting_change: Default::default(),
            last_change: None,
            dep_details: None,
            vulns: Default::default(),
            vuln_details: Default::default(),
            osv: Arc::new(bombadil_core::github::rest::UreqFetch::new()),
            disk: Default::default(),
            find: None,
            new_project: None,
            creating_project: false,
            backup: Default::default(),
            context_menu: None,
            confirm_recreate: None,
            confirm_pin_change: None,
            confirm_environment_removal: None,
            open_menu: None,
            modal: None,
            preferences: None,
            deps: None,
            outdated: None,
            last_failure: Default::default(),
            dep_edit: None,
            deps_view: deps::DepsView::default(),
            system_mode: iced::theme::Mode::Dark,
            index_credential_drafts,
            index_credential_names,
            env_value_drafts: env_editor::ValueDrafts::new(),
            members: None,
            confirm_member_sync: None,
            uv_custom_path_draft,
            uv_custom_validation: None,
            pycharm_detected: None,
            tab: Tab::default(),
        }
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
    let mut probes = vec![
        probe_every_venv(&app),
        // For `System`: what the desktop is using right now.
        // `theme_changes` in the subscription covers later flips.
        iced::system::theme().map(Message::SystemThemeChanged),
    ];
    // The initially selected project's Dependencies tab, so it is populated
    // the moment the window appears rather than only after the user re-clicks
    // the already-selected project. `boot` has no `CommandRunner` (`update`
    // is the only place that gets one -- see its own doc), so this cannot
    // call `fetch_deps_task` directly; re-dispatching the same message
    // `ProjectSelected` handles reaches `update`, and so a real runner, on
    // the very next step instead.
    if let Some(i) = app.selected {
        probes.push(Task::done(Message::ProjectSelected(i)));
    }
    // The first GitHub lookup, at launch; silent, as every first one is.
    probes.push(Task::done(Message::GitHubTick));
    (app, Task::batch(probes))
}

/// One background venv probe per project, against wherever each one resolves
/// *now*.
///
/// Shared by `boot` and by the two `SettingsDefaultVenvLocation*` arms.
/// `entries[i].venv_exists` and `python_version` describe the location that
/// was resolved when the probe ran, so a change to `default_venv_location`
/// moves the ground under every `VenvLocation::Default` project: without
/// this the sidebar goes on showing green ticks and Python versions for
/// environments at the old path until the app is restarted.
fn probe_every_venv(app: &App) -> Task<Message> {
    Task::batch(
        app.config
            .projects
            .iter()
            .enumerate()
            // One probe per *environment*, not per project: the tree shows
            // them all, and a project's glyph rolls up what every one of them
            // reports.
            .flat_map(|(i, project)| {
                project.environments.iter().filter_map(move |environment| {
                    let path =
                        venv_path::resolve(project, environment, &app.config.settings).ok()?;
                    let pyproject_path = project.pyproject_path.clone();
                    let location = environment.location.clone();
                    Some(Task::perform(
                        probe_venv_and_requirement(path, pyproject_path),
                        move |(python_version, requires_python)| Message::VenvProbed {
                            index: i,
                            location: location.clone(),
                            python_version,
                            requires_python,
                        },
                    ))
                })
            })
            .chain(app.config.projects.iter().filter_map(|project| {
                // Alongside the venv probes because these run wherever a
                // project's on-disk state may have moved: boot, after a sync,
                // after an edit. The status is only ever as fresh as that.
                let dir = project.pyproject_path.parent()?.to_path_buf();
                let id = project.id;
                Some(Task::perform(probe_git(dir), move |status| {
                    Message::GitProbed { id, status }
                }))
            }))
            // Health reads, at the same moments: whether each environment
            // matches its lock, and which Pythons could replace it.
            .chain(health_round_targets(app).into_iter().filter_map(|id| {
                let project = app.config.projects.iter().find(|p| p.id == id)?.clone();
                Some(probe_sync_check(project, app.config.clone()))
            }))
            .chain(std::iter::once(probe_installed_pythons(app.config.clone())))
            .collect::<Vec<_>>(),
    )
}

/// How long one GitHub lookup may take before the UI stops waiting for it.
const GITHUB_TIMEOUT: Duration = Duration::from_secs(45);

/// Looks the projects up one after another, so a tick over thirty projects
/// starts one `gh` at a time rather than thirty. The real runner, keychain
/// and HTTP client are built here at the boundary, as `probe_git` does, so no
/// test reaches GitHub.
fn look_up_github(projects: Vec<(uuid::Uuid, PathBuf)>, account: Option<String>) -> Task<Message> {
    Task::perform(
        look_up_each(projects, GITHUB_TIMEOUT, move |dir: PathBuf| {
            bombadil_core::github::lookup(
                &dir,
                &bombadil_core::process::SystemCommandRunner,
                &KeyringSecretStore::new(),
                &bombadil_core::github::rest::UreqFetch::new(),
                account.as_deref(),
            )
        }),
        Message::GitHubLooked,
    )
}

/// Each lookup on its own job with its own `timeout`, so one hung `gh` costs
/// only its own project a "took too long" rather than stalling the rest.
async fn look_up_each<F>(
    projects: Vec<(uuid::Uuid, PathBuf)>,
    timeout: Duration,
    look: F,
) -> Vec<(uuid::Uuid, bombadil_core::github::Lookup)>
where
    F: Fn(PathBuf) -> bombadil_core::github::Lookup + Clone + Send + 'static,
{
    let mut found = Vec::with_capacity(projects.len());
    for (id, dir) in projects {
        let look = look.clone();
        let lookup = match job::run(move || look(dir), timeout).await {
            job::JobResult::Done(lookup) => lookup,
            job::JobResult::TimedOut | job::JobResult::Failed => {
                bombadil_core::github::Lookup::Failed("GitHub took too long to answer".into())
            }
        };
        found.push((id, lookup));
    }
    found
}

/// Whether `event` on `status` is news: not already announced for the same
/// pull request and commit by another project. Records it when it is.
fn github_news(
    notified: &mut std::collections::HashMap<
        String,
        (
            String,
            std::mem::Discriminant<bombadil_core::github::transition::Event>,
        ),
    >,
    status: &bombadil_core::github::PrStatus,
    event: &bombadil_core::github::transition::Event,
) -> bool {
    let this = (status.pr.head_sha.clone(), std::mem::discriminant(event));
    notified.insert(status.pr.url.clone(), this.clone()) != Some(this)
}

/// The folder a project's lookups run in: its manifest's directory.
fn project_dir(project: &bombadil_core::model::Project) -> PathBuf {
    project
        .pyproject_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// A desktop notification, shown off the render thread; a failure (no
/// notification daemon) has nowhere useful to go -- the toast already said it.
fn notify_desktop(title: String, body: String) -> Task<Message> {
    Task::future(async move {
        job::run(
            move || {
                notify_rust::Notification::new()
                    .appname("Bombadil")
                    .summary(&title)
                    .body(&body)
                    .show()
                    .map(|_| ())
                    .ok()
            },
            Duration::from_secs(5),
        )
        .await
    })
    .discard()
}

/// Writes (`Some`) or deletes (`None`) the GitHub token, on a job thread.
fn store_github_token(token: Option<String>) -> Task<Message> {
    Task::perform(
        async move {
            job::run(
                move || {
                    let store = KeyringSecretStore::new();
                    match token {
                        Some(t) => store
                            .set(&SecretKey::GitHubToken, t.trim())
                            .map(|()| "Saved the GitHub token".to_string()),
                        None => store
                            .delete(&SecretKey::GitHubToken)
                            .map(|()| "Removed the GitHub token".to_string()),
                    }
                    .map_err(|e| e.to_string())
                },
                VENV_PROBE_TIMEOUT,
            )
            .await
        },
        |result| {
            Message::GitHubTokenStored(match result {
                job::JobResult::Done(r) => r,
                _ => Err("the keychain did not answer".into()),
            })
        },
    )
}

/// How long gh's one-time code lasts; gh gives up on its own after it.
const GITHUB_LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// `gh auth login` in the background: the code as soon as gh prints it, then
/// the outcome. Built lazily, so nothing starts until iced polls it -- a test
/// that only inspects `update` never reaches the real gh.
fn add_github_account(
    before: Vec<bombadil_core::github::gh::Account>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> impl futures::Stream<Item = Message> {
    futures::stream::once(async move {
        job::stream(
            move |emit| {
                let result = bombadil_core::github::gh::add_account(
                    &bombadil_core::process::SystemCommandRunner,
                    &before,
                    &mut |code| emit(Message::GitHubLoginCode(code)),
                    &cancel,
                );
                emit(Message::GitHubLoginFinished(result));
            },
            GITHUB_LOGIN_TIMEOUT,
        )
        .map(|result| match result {
            job::JobResult::Done(message) => message,
            job::JobResult::TimedOut => {
                Message::GitHubLoginFinished(Err("the sign-in code expired".into()))
            }
            job::JobResult::Failed => {
                Message::GitHubLoginFinished(Err("gh stopped unexpectedly".into()))
            }
        })
    })
    .flatten()
}

/// The device flow in the background: the code as soon as GitHub gives it,
/// then the token straight into the keychain. Lazy, as `add_github_account`.
fn sign_in_with_browser(
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> impl futures::Stream<Item = Message> {
    use bombadil_core::github::{device, rest::UreqFetch};
    futures::stream::once(async move {
        job::stream(
            move |emit| {
                let result = device::login(
                    &UreqFetch::new(),
                    device::CLIENT_ID,
                    &mut |code| emit(Message::GitHubLoginCode(code)),
                    &mut |d| device::sleep_unless(&cancel, d),
                )
                .and_then(|token| {
                    KeyringSecretStore::new()
                        .set(&SecretKey::GitHubToken, &token)
                        .map_err(|e| format!("could not save the token: {e}"))
                })
                .map(|()| "Signed in to GitHub".to_string());
                emit(Message::GitHubBrowserFinished(result));
            },
            GITHUB_LOGIN_TIMEOUT,
        )
        .map(|result| match result {
            job::JobResult::Done(message) => message,
            job::JobResult::TimedOut => {
                Message::GitHubBrowserFinished(Err("the sign-in code expired".into()))
            }
            job::JobResult::Failed => {
                Message::GitHubBrowserFinished(Err("the sign-in stopped unexpectedly".into()))
            }
        })
    })
    .flatten()
}

/// Preferences' source line and account list, both from gh, off the render
/// thread.
fn check_github(account: Option<String>) -> Task<Message> {
    let accounts = Task::perform(
        async move {
            job::run(
                || {
                    bombadil_core::github::gh::accounts(
                        &bombadil_core::process::SystemCommandRunner,
                    )
                },
                Duration::from_secs(15),
            )
            .await
        },
        |result| {
            Message::GitHubAccountsListed(match result {
                job::JobResult::Done(accounts) => accounts,
                _ => Vec::new(),
            })
        },
    );
    Task::batch([check_github_source(account), accounts])
}

fn check_github_source(account: Option<String>) -> Task<Message> {
    Task::perform(
        async move {
            job::run(
                move || {
                    bombadil_core::github::source(
                        &bombadil_core::process::SystemCommandRunner,
                        &KeyringSecretStore::new(),
                        account.as_deref(),
                    )
                },
                Duration::from_secs(15),
            )
            .await
        },
        |result| {
            Message::GitHubSourceChecked(match result {
                job::JobResult::Done(text) => text,
                _ => "Could not tell how Bombadil reaches GitHub".into(),
            })
        },
    )
}

/// How long one background uv read may take before it says nothing.
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// `uv sync --check` for one project, off the render thread. Any failure is
/// `Unknown`: a background read has nothing useful to report.
fn probe_sync_check(project: Project, config: Config) -> Task<Message> {
    let id = project.id;
    Task::perform(
        async move {
            job::run(
                move || {
                    let runner = bombadil_core::process::SystemCommandRunner;
                    let Some(ctx) = deps_command_context(&project, &config) else {
                        return uv::results::SyncCheck::Unknown;
                    };
                    let Ok(uv) = resolve_uv_for(&config, &runner) else {
                        return uv::results::SyncCheck::Unknown;
                    };
                    let (kind, discovered) = discover_members(&project);
                    let plan = members_editor::sync_plan(&project.members, &kind, &discovered);
                    // A member selection means two syncs want different
                    // environments: Sync (root dependencies) and Sync
                    // members (the selected members, exactly). No one check
                    // matches both, so say nothing; the Dependencies and
                    // Members tiles describe it.
                    if plan != members_editor::SyncPlan::Plain {
                        return uv::results::SyncCheck::Unknown;
                    }
                    let spec = sync_check_spec(&uv, &ctx, &active_pin(&project), &plan);
                    match runner.run(&spec) {
                        Ok(out) => uv::results::parse_sync_check(out.status, &out.stderr),
                        Err(_) => uv::results::SyncCheck::Unknown,
                    }
                },
                HEALTH_PROBE_TIMEOUT,
            )
            .await
        },
        move |result| Message::SyncChecked {
            id,
            check: match result {
                job::JobResult::Done(check) => check,
                _ => uv::results::SyncCheck::Unknown,
            },
        },
    )
}

/// The interpreters already installed, once per probe round.
fn probe_installed_pythons(config: Config) -> Task<Message> {
    Task::perform(
        async move {
            job::run(
                move || {
                    let runner = bombadil_core::process::SystemCommandRunner;
                    let uv = resolve_uv_for(&config, &runner).ok()?;
                    let out = runner.run(&uv::command::python_list_installed(&uv)).ok()?;
                    let list = uv::results::parse_python_list(&out.stdout).ok()?;
                    Some(list.into_iter().map(|i| i.version).collect::<Vec<_>>())
                },
                HEALTH_PROBE_TIMEOUT,
            )
            .await
        },
        |result| {
            Message::PythonsListed(match result {
                job::JobResult::Done(list) => list,
                _ => None,
            })
        },
    )
}

/// `git status` for the sidebar and header, off the render thread. The real
/// runner is built here at the boundary, as `KeyringSecretStore` is elsewhere,
/// so no test reaches a real `git`.
async fn probe_git(dir: PathBuf) -> Option<bombadil_core::git::GitStatus> {
    let runner = bombadil_core::process::SystemCommandRunner;
    match job::run(
        move || bombadil_core::git::status(&dir, &runner),
        VENV_PROBE_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(status) => status,
        job::JobResult::TimedOut | job::JobResult::Failed => None,
    }
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

/// Runs the venv probe and a `requires-python` re-read together, off the
/// render thread, for the sidebar's drift check (`sidebar::state`). Not
/// folded into `probe_venv` itself: that function is also reused by the
/// add-project dialog's adopt-or-create probe (`probe_venv_outcome`), which
/// has no project manifest in play at all, so bundling a pyproject read into
/// it would give that call site a read it never asked for.
///
/// One `job::run` call rather than two: both reads are the same shape of
/// small local file `VENV_PROBE_TIMEOUT` already covers -- see that
/// constant's own doc. A read failure on either side (missing file,
/// unparseable manifest) degrades to `None`, same as `probe_venv`.
async fn probe_venv_and_requirement(
    venv_path: PathBuf,
    pyproject_path: PathBuf,
) -> (Option<String>, Option<String>) {
    match job::run(
        move || {
            let python_version = venv_path::probe(&venv_path);
            let requires_python = bombadil_core::pyproject::load(&pyproject_path)
                .ok()
                .and_then(|p| p.requires_python);
            (python_version, requires_python)
        },
        VENV_PROBE_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(result) => result,
        job::JobResult::TimedOut | job::JobResult::Failed => (None, None),
    }
}

/// What background loading produced for the Dependencies tab (spec §8):
/// the parsed manifest, what `uv pip list` reports installed, and (when it
/// could be read) the resolved tree. `deps::build` -- pure, tested on its
/// own -- turns this into the tab; nothing here does that joining itself.
#[derive(Debug, Clone)]
pub struct DepsFetch {
    pyproject: PyProject,
    installed: Vec<InstalledPackage>,
    /// `Err` carries uv's own text for a tree that could not be read at all --
    /// a non-zero exit, or output this build cannot parse. Kept apart from
    /// `Ok(TreeOutcome::UnsupportedSchema)`, which is uv answering with a
    /// schema this build does not understand; see `deps::build`'s own doc for
    /// why the two must stay distinguishable. Not fatal to the whole fetch:
    /// the rows above are still true without a tree.
    tree: Result<TreeOutcome, String>,
}

/// Resolves what a Dependencies fetch needs to run project-scoped uv
/// commands: the project's own directory and its venv. `None` when either
/// cannot be resolved (no directory component, an unresolvable venv
/// location) -- the same two failure modes `run_sync` reports as errors, but
/// there is no drawer line to write here, so this just leaves nothing to
/// fetch.
fn deps_command_context(project: &Project, config: &Config) -> Option<uv::command::ProjectContext> {
    let project_dir = project
        .pyproject_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())?;
    let venv_path =
        venv_path::resolve(project, project.active_environment()?, &config.settings).ok()?;
    Some(uv::command::ProjectContext {
        project_dir: project_dir.to_path_buf(),
        venv_path,
    })
}

/// What a uv command that ran and refused has to say for itself: its own
/// stderr where there is any, `fallback` where there is none. The exit code
/// alone tells the user nothing, which is the whole reason this is carried
/// rather than collapsed into a bare failure.
fn uv_refusal(outcome: &bombadil_core::process::CommandOutcome, fallback: &str) -> String {
    match outcome.stderr.trim() {
        "" => fallback.to_string(),
        stderr => stderr.to_string(),
    }
}

/// The blocking half of a Dependencies fetch: resolve uv, read the manifest,
/// then run `uv pip list` and `uv tree`.
///
/// Every failure -- no resolvable uv, an unresolvable venv location, an
/// unreadable manifest, a command that exits non-zero, output this build
/// cannot parse -- comes back as `Err` carrying uv's own text where there is
/// any, the same shape `list_interpreters_blocking` uses.
///
/// Degrading to an empty result instead is what made a failed fetch
/// indistinguishable from an environment with nothing in it: `installed` came
/// back empty, so every declared package rendered as `"(not installed)"` -- a
/// positive claim about the environment when the truth was that we could not
/// ask it. A project with no venv is not exempt from that: `uv pip list`
/// refuses, and saying so beats claiming that nothing is installed.
///
/// The tree is the one part that does not fail the whole fetch: its `Err`
/// travels as far as `deps::build`, which explains it in the tab while still
/// showing every row `uv pip list` did answer for.
fn fetch_deps_blocking(
    project: &Project,
    config: &Config,
    runner: &dyn CommandRunner,
) -> Result<DepsFetch, String> {
    let resolved_uv = resolve_uv_for(config, runner)?;
    let ctx = deps_command_context(project, config)
        .ok_or_else(|| "this project has no directory or no resolvable venv path".to_string())?;

    let installed = match runner.run(&uv::command::pip_list(&resolved_uv, &ctx)) {
        Ok(outcome) if outcome.success() => uv::results::parse_pip_list(&outcome.stdout)
            .map_err(|err| format!("could not read uv's installed package list: {err}"))?,
        Ok(outcome) => return Err(uv_refusal(&outcome, "uv pip list failed")),
        Err(err) => return Err(err.to_string()),
    };

    let tree = match runner.run(&uv::command::tree(&resolved_uv, &ctx, DEPS_TREE_DEPTH)) {
        Ok(outcome) if outcome.success() => uv::results::parse_tree(&outcome.stdout)
            .map_err(|err| format!("could not read uv's dependency tree: {err}")),
        Ok(outcome) => Err(uv_refusal(&outcome, "uv tree failed")),
        Err(err) => Err(err.to_string()),
    };

    // Read last, and its own failure reported like any other: there is nothing
    // to render a manifest against once the environment could not be described,
    // and doing it in this order keeps every failure above a pure uv question.
    let pyproject = bombadil_core::pyproject::load(&project.pyproject_path)
        .map_err(|err| format!("could not read this project's manifest: {err}"))?;

    Ok(DepsFetch {
        pyproject,
        installed,
        tree,
    })
}

/// Runs a Dependencies fetch off the render thread via `job::run` --
/// criterion 5: `uv tree` on a large project is slow, and `CommandRunner::run`
/// blocks until the child exits, so calling `fetch_deps_blocking` from
/// `update` would freeze the window exactly like `sync_project` would.
/// `SYNC_TIMEOUT`, not `VENV_PROBE_TIMEOUT`, for the same reason `sync_project`
/// uses it: this can run a full uv resolve, not a sub-millisecond file read.
async fn load_deps(
    project: Project,
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> Result<DepsFetch, String> {
    // Reported rather than degraded to an empty tab, for the reason
    // `fetch_deps_blocking`'s own doc gives: an empty tab is a claim about
    // the environment, and a timeout is not in a position to make one.
    job::attempt(
        move || fetch_deps_blocking(&project, &config, runner.as_ref()),
        SYNC_TIMEOUT,
        "reading dependencies",
    )
    .await
}

/// Dispatches a Dependencies fetch for `projects[index]`, tagging the result
/// with `index` so a later `Message::DepsLoaded` can be dropped if the
/// selection has since moved on.
fn fetch_deps_task(
    index: usize,
    project: Project,
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> Task<Message> {
    let outdated = fetch_outdated_task(index, project.clone(), config.clone(), runner.clone());
    Task::batch([
        Task::perform(load_deps(project, config, runner), move |fetch| {
            Message::DepsLoaded(index, fetch)
        }),
        outdated,
    ])
}

/// Newer releases of the installed packages. Needs the network, so it is
/// its own task: the Dependencies tab never waits on it.
fn fetch_outdated_task(
    index: usize,
    project: Project,
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> Task<Message> {
    Task::perform(
        async move {
            job::run(
                move || -> Result<Vec<uv::results::Outdated>, String> {
                    let ctx = deps_command_context(&project, &config)
                        .ok_or("this project has no environment")?;
                    let uv = resolve_uv_for(&config, runner.as_ref())?;
                    let out = runner
                        .run(&uv::command::pip_list_outdated(&uv, &ctx))
                        .map_err(|e| e.to_string())?;
                    if !out.success() {
                        return Err(uv_refusal(&out, "uv could not check for newer versions"));
                    }
                    uv::results::parse_outdated(&out.stdout).map_err(|e| e.to_string())
                },
                SYNC_TIMEOUT,
            )
            .await
        },
        move |result| {
            Message::OutdatedLoaded(
                index,
                match result {
                    job::JobResult::Done(r) => r,
                    job::JobResult::TimedOut => Err("checking for newer versions timed out".into()),
                    job::JobResult::Failed => Err("checking for newer versions failed".into()),
                },
            )
        },
    )
}

/// The blocking half of a Members fetch (spec §5a, Task 5): classify the
/// project's own manifest, walk its tree for other `pyproject.toml` files,
/// then (if a uv can be resolved and the project has a venv to ask about)
/// run `uv pip list`. Mirrors `fetch_deps_blocking`'s degrade-don't-error
/// discipline: an unreadable manifest, no resolvable uv, a project with no
/// venv yet -- all leave `discovered`/`installed` empty rather than failing,
/// since a project that has never been synced or has no siblings at all is a
/// normal state for this tab, not an error to report.
/// The project's workspace kind and the members on disk: reads its manifest
/// and walks its tree. Blocking.
fn discover_members(
    project: &Project,
) -> (
    bombadil_core::members::WorkspaceKind,
    Vec<bombadil_core::members::DiscoveredMember>,
) {
    let project_dir = project
        .pyproject_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty());
    let kind = project_dir
        .and_then(|_| std::fs::read_to_string(&project.pyproject_path).ok())
        .map(|text| bombadil_core::members::parse_workspace(&text))
        .unwrap_or(bombadil_core::members::WorkspaceKind::Loose);
    let discovered = match project_dir {
        Some(dir) => bombadil_core::members::discover(dir, &kind),
        None => Vec::new(),
    };
    (kind, discovered)
}

fn fetch_members_blocking(
    project: &Project,
    config: &Config,
    runner: &dyn CommandRunner,
) -> MembersFetch {
    let (kind, discovered) = discover_members(project);

    let installed = match (
        resolve_uv_for(config, runner).ok(),
        deps_command_context(project, config),
    ) {
        (Some(resolved_uv), Some(ctx)) => runner
            .run(&uv::command::pip_list(&resolved_uv, &ctx))
            .ok()
            .filter(|outcome| outcome.success())
            .and_then(|outcome| uv::results::parse_pip_list(&outcome.stdout).ok())
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    MembersFetch {
        kind,
        discovered,
        installed,
    }
}

/// Runs a Members fetch off the render thread via `job::run` -- discovery
/// walks the filesystem and `uv pip list` spawns a process, neither of which
/// may run on the render thread. `SYNC_TIMEOUT`, not `VENV_PROBE_TIMEOUT`,
/// for the same reason `load_deps` uses it: this can spawn a real uv process.
async fn load_members(
    project: Project,
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> MembersFetch {
    match job::run(
        move || fetch_members_blocking(&project, &config, runner.as_ref()),
        SYNC_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(fetch) => fetch,
        job::JobResult::TimedOut | job::JobResult::Failed => MembersFetch {
            kind: bombadil_core::members::WorkspaceKind::Loose,
            discovered: Vec::new(),
            installed: Vec::new(),
        },
    }
}

/// Dispatches a Members fetch for `projects[index]`, tagging the result with
/// `index` so a later `Message::MembersLoaded` can be dropped if the
/// selection has since moved on -- mirrors `fetch_deps_task` exactly.
fn fetch_members_task(
    index: usize,
    project: Project,
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> Task<Message> {
    Task::perform(load_members(project, config, runner), move |fetch| {
        Message::MembersLoaded(index, fetch)
    })
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
        Message::ProjectSelected(i) if i < app.config.projects.len() => {
            app.selected = Some(i);
            app.dep_edit = None;
            app.dep_filter.clear();
            let opened = selection_changed(app);
            if folder_missing(app, i) {
                return Task::none();
            }
            // Re-read on every selection: commits and checkouts happen
            // outside Bombadil, and this is the moment the user looks.
            return Task::batch([reprobe_project(app, i, runner), opened]);
        }
        Message::WindowFocused => {
            let now = std::time::Instant::now();
            if should_refresh_on_focus(app, now)
                && let Some(i) = app.selected
            {
                app.last_focus_refresh = Some(now);
                // The other moment the user looks. Venvs too: a `uv` run in
                // a terminal can create or replace one.
                return Task::batch([
                    reprobe_project(app, i, runner),
                    probe_every_venv(app),
                    sweep_missing(app),
                ]);
            }
            // Folders even with nothing selected: a re-mounted drive clears
            // its warning without a click. Same debounce as above.
            if app.selected.is_none()
                && app.last_focus_refresh.is_none_or(|at| {
                    now.saturating_duration_since(at) >= crate::busy::FOCUS_DEBOUNCE
                })
            {
                app.last_focus_refresh = Some(now);
                return sweep_missing(app);
            }
        }
        Message::Shortcut(s) => {
            // Through `update` again, so a key runs exactly what its button does.
            if let Some(message) = shortcut_message(app, s) {
                return update(app, message, runner);
            }
        }
        // A minimised window reports a tiny (on Windows, zero) size; it is
        // not the size to come back at.
        Message::WindowOpened(id) => {
            app.window = Some(id);
            return iced::window::gain_focus(id);
        }
        Message::WindowResized(size) => {
            if size.width >= MIN_WINDOW.width && size.height >= MIN_WINDOW.height {
                app.window_size = Some(size);
            }
        }
        Message::DepFilterChanged(text) => app.dep_filter = text,
        // By id from here on: the folder picker does not block the window,
        // and the list can change while it is up.
        Message::AddLocalRequested(i) => {
            let Some(id) = app.config.projects.get(i).map(|p| p.id) else {
                return Task::none();
            };
            return Task::perform(pick_folder(), move |folder| {
                Message::AddLocalPicked(id, folder)
            });
        }
        Message::AddLocalPicked(_, None) => {}
        Message::AddLocalPicked(id, Some(folder)) => {
            let read = folder.clone();
            return Task::perform(
                async move {
                    match job::run(
                        move || {
                            crate::add_local::inspect(&read, &|path| {
                                std::fs::read_to_string(path).ok()
                            })
                        },
                        VENV_PROBE_TIMEOUT,
                    )
                    .await
                    {
                        job::JobResult::Done(result) => result,
                        job::JobResult::TimedOut | job::JobResult::Failed => {
                            Err("could not read the folder".to_string())
                        }
                    }
                },
                move |result| Message::AddLocalLoaded(id, folder.clone(), result),
            );
        }
        Message::AddLocalLoaded(id, folder, result) => match result {
            Ok(name) => {
                let i = app.config.projects.iter().position(|p| p.id == id);
                // Main first, then every group and extra the project has.
                let mut groups = vec![deps::GroupKind::Main];
                if let Some((j, Ok(tab))) = &app.deps
                    && Some(*j) == i
                {
                    for group in &tab.groups {
                        if !groups.contains(&group.kind) {
                            groups.push(group.kind.clone());
                        }
                    }
                }
                app.add_local = Some(crate::add_local::Draft {
                    project: id,
                    folder,
                    name,
                    groups,
                    group: deps::GroupKind::Main,
                    editable: true,
                });
            }
            Err(why) => crate::busy::push(
                &mut app.toasts,
                crate::busy::ToastKind::Warning,
                why,
                std::time::Instant::now(),
            ),
        },
        Message::AddLocalGroupSelected(kind) => {
            if let Some(draft) = &mut app.add_local {
                draft.group = kind;
            }
        }
        Message::AddLocalEditableToggled(on) => {
            if let Some(draft) = &mut app.add_local {
                draft.editable = on;
            }
        }
        Message::AddLocalCancelled => app.add_local = None,
        Message::AddLocalConfirmed => {
            // The dialog stays open while another command runs.
            if !can_start(app) {
                return Task::none();
            }
            if let Some(id) = app.add_local.as_ref().map(|d| d.project)
                && refuse_while_running(app, id)
            {
                return Task::none();
            }
            let Some(draft) = app.add_local.take() else {
                return Task::none();
            };
            let Some(i) = app
                .config
                .projects
                .iter()
                .position(|p| p.id == draft.project)
            else {
                return Task::none();
            };
            let project = app.config.projects[i].clone();
            let project_dir = project
                .pyproject_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            // Relative, so `[tool.uv.sources]` works on a teammate's machine.
            let path = bombadil_core::members::path_for_manifest(&project_dir, &draft.folder);
            let group = deps::uv_group(&draft.group);
            let editable = draft.editable;
            app.busy = Some(crate::busy::Busy::new(
                project.id,
                crate::busy::Action::AddLocal(draft.label()),
            ));
            drawer::start(&mut app.drawer, format!("adding {}...", draft.label()));
            let (pin, plan) = (active_pin(&project), sync_plan_for(app, i));
            return Task::stream(stream_in_project(
                project,
                app.config.clone(),
                move |uv, ctx| {
                    let add = uv::command::add_path(uv, ctx, &path, editable, group.clone());
                    dep_commands(uv, ctx, &pin, &plan, &DepOp::Add(add))
                },
                (
                    "adding the package timed out",
                    "adding the package failed unexpectedly",
                ),
                runner,
            ));
        }
        Message::ProjectFilterChanged(text) => app.project_filter = text,
        Message::FindRequested => {
            app.tab = Tab::Dependencies;
            return iced::widget::operation::focus(deps::FILTER_INPUT);
        }
        Message::DepUpgradeRequested(i, name) => {
            return start_upgrade(app, i, Some(name), runner);
        }
        Message::DepUpgradeAllRequested(i) => {
            app.confirm_upgrade_all = app.config.projects.get(i).map(|p| p.id);
        }
        Message::DepUpgradeAllCancelled => app.confirm_upgrade_all = None,
        Message::DepUpgradeAllConfirmed => {
            // Still asking while another command runs, rather than closing
            // with nothing done.
            if !can_start(app) {
                return Task::none();
            }
            if let Some(id) = app.confirm_upgrade_all.take()
                && let Some(i) = app.config.projects.iter().position(|p| p.id == id)
            {
                return start_upgrade(app, i, None, runner);
            }
        }
        Message::TabDragStarted(tab) => {
            app.tab = tab;
            // The saved order as it was, to tell at the end whether anything
            // really moved.
            app.tab_drag = Some((tab, app.config.settings.layout.tab_order.clone(), None));
            // A click on a tab arrives here, not as `TabSelected`.
            return tab_opened(app);
        }
        Message::TabDragOver(target) => {
            // Tabs differ in width, so after a swap the pointer can sit over
            // the tab it just passed; swapping again there would bounce.
            if let Some((dragged, _, last)) = &mut app.tab_drag
                && (*dragged == target || *last != Some(target))
            {
                *last = None;
            }
            if let Some((dragged, _, last)) = &mut app.tab_drag
                && *dragged != target
                && last.is_none()
            {
                *last = Some(target);
                let layout = &mut app.config.settings.layout;
                layout.tab_order = move_tab(&tab_order(&layout.tab_order), *dragged, target)
                    .into_iter()
                    .map(|t| t.key().to_string())
                    .collect();
            }
        }
        Message::SidebarToggled => {
            let layout = &mut app.config.settings.layout;
            layout.sidebar_collapsed = !layout.sidebar_collapsed;
            return persist_edit(app, "settings change");
        }
        Message::SidebarDragStarted => app.sidebar_drag = true,
        Message::SidebarDragged(x) => {
            if app.sidebar_drag {
                app.config.settings.layout.sidebar_width = clamp_sidebar(x);
            }
        }
        Message::DragEnded => {
            let resized = std::mem::take(&mut app.sidebar_drag);
            // Saved only when the order differs from where the drag began; a
            // drag that came back puts the saved order back as it was, so an
            // untouched built-in order stays implicit.
            let reordered = match app.tab_drag.take() {
                Some((_, before, _)) => {
                    let layout = &mut app.config.settings.layout;
                    if tab_order(&layout.tab_order) == tab_order(&before) {
                        layout.tab_order = before;
                        false
                    } else {
                        true
                    }
                }
                None => false,
            };
            if resized || reordered {
                return persist_edit(app, "settings change");
            }
        }
        Message::RefreshRequested => {
            if let Some(i) = app.selected.filter(|i| *i < app.config.projects.len()) {
                return Task::batch([
                    reprobe_project(app, i, runner),
                    probe_every_venv(app),
                    sweep_missing(app),
                ]);
            }
        }
        Message::ProjectSelected(_) => {}
        Message::TabSelected(tab) => {
            app.tab = tab;
            return tab_opened(app);
        }
        Message::SyncChecked { id, check } => {
            // A project removed while its check ran is not kept.
            if app.config.projects.iter().any(|p| p.id == id) {
                app.sync_checks.insert(id, check);
            }
        }
        Message::PythonsListed(versions) => app.installed_pythons = versions,
        Message::GitProbed { id, status } => {
            // By id: the list may have been reordered or edited since.
            if let Some(entry) = app.entries.iter_mut().find(|entry| entry.id == id) {
                entry.git = status;
            }
        }
        Message::VenvProbed {
            index,
            location,
            python_version,
            requires_python,
        } if index < app.entries.len() => {
            // Matched by location, never by position: a project has several
            // probes in flight at once, and they land in whatever order the
            // filesystem answers.
            if let Some(row) = app.entries[index]
                .environments
                .iter_mut()
                .find(|row| row.location == location)
            {
                row.python_version = python_version;
            }
            app.entries[index].requires_python = requires_python;
        }
        // A stale probe result for a project that no longer exists.
        Message::VenvProbed { .. } => {}
        Message::DepsLoaded(i, fetch) if i < app.config.projects.len() => {
            let check = match &fetch {
                Ok(fetch) => {
                    report_change(app, i, &fetch.installed);
                    start_vuln_check(app, i, &fetch.installed)
                }
                Err(_) => Task::none(),
            };
            // The failure is kept, not flattened into an empty tab: `deps::view`
            // renders it as the reason it is, which is the one thing an empty
            // tab could never say.
            app.deps = Some((
                i,
                fetch.map(|fetch| deps::build(&fetch.pyproject, &fetch.installed, fetch.tree)),
            ));
            return check;
        }
        // A stale fetch for a project that no longer exists, or that the
        // user has since moved away from -- `view` only ever reads
        // `app.deps` when its index still matches the current selection.
        Message::DepsLoaded(_, _) => {}
        // Only the selected project's: a slow lookup for one left behind
        // must not replace the current one's.
        Message::OutdatedLoaded(i, result) if Some(i) == app.selected => {
            app.outdated = Some((i, result));
        }
        Message::OutdatedLoaded(..) => {}
        Message::DoctorFixRequested(id, fix) => {
            use bombadil_core::doctor::DoctorFix;
            let Some(i) = app.config.projects.iter().position(|p| p.id == id) else {
                return Task::none();
            };
            let runs_uv = matches!(
                fix,
                DoctorFix::LetUvPick(_) | DoctorFix::RequirePython(_) | DoctorFix::Retry
            );
            if runs_uv && !can_start(app) {
                drawer::push_line(
                    &mut app.drawer,
                    "another uv command is still running; try again when it finishes",
                    &[],
                );
                return Task::none();
            }
            // Its toast has done its job; the fix's own outcome follows.
            let pressed = Some((id, fix.clone()));
            app.toasts.retain(|t| t.fix != pressed);
            return apply_fix(app, i, fix, runner);
        }
        Message::DoctorPinRequested(i, requirement) => {
            let group = declared_group(app, i, &requirement).unwrap_or(deps::GroupKind::Main);
            return add_requirement(app, i, requirement, group, runner);
        }
        Message::PyprojectEdited(id, Ok(())) => {
            if let Some(i) = app.config.projects.iter().position(|p| p.id == id) {
                return update(app, Message::SyncRequested(i), runner);
            }
        }
        Message::PyprojectEdited(_, Err(e)) => crate::busy::push(
            &mut app.toasts,
            crate::busy::ToastKind::Warning,
            format!("Could not update pyproject.toml: {e}"),
            std::time::Instant::now(),
        ),
        Message::MembersLoaded(i, fetch) if i < app.config.projects.len() => {
            app.members = Some((i, fetch));
        }
        // A stale fetch for a project that no longer exists, or that the
        // user has since moved away from -- same staleness guard `DepsLoaded` uses.
        Message::MembersLoaded(_, _) => {}
        // A sync already in flight swallows the press. Without this the
        // button dispatches unconditionally, and a 300-second timeout makes
        // two concurrent `uv sync` processes on the same project easy to
        // reach: the job times out, the user presses Sync again, and the
        // abandoned child is still running.
        //
        // The menu closes either way, the same as `OpenTerminalRequested`
        // and `RemoveProjectRequested`: an action taken from the menu
        // dismisses it, whether or not the press was swallowed.
        Message::DrawerToggled => {
            app.drawer.expanded = !app.drawer.expanded;
        }
        Message::DrawerCopyRequested => {
            crate::busy::push(
                &mut app.toasts,
                crate::busy::ToastKind::Success,
                "Copied output".to_string(),
                std::time::Instant::now(),
            );
            return iced::clipboard::write(drawer::transcript(&app.drawer));
        }
        Message::DrawerExportRequested => {
            return Task::perform(pick_save_file(), Message::DrawerExportPicked);
        }
        Message::DrawerExportPicked(None) => {}
        Message::DrawerExportPicked(Some(path)) => {
            let text = drawer::transcript(&app.drawer);
            return Task::perform(
                async move { write_transcript(path, &text) },
                Message::DrawerExportFinished,
            );
        }
        Message::DrawerExportFinished(result) => {
            let (kind, text) = match result {
                Ok(path) => (
                    crate::busy::ToastKind::Success,
                    format!("Saved output to {}", path.display()),
                ),
                Err(error) => (
                    crate::busy::ToastKind::Warning,
                    format!("Could not save output: {error}"),
                ),
            };
            crate::busy::push(&mut app.toasts, kind, text, std::time::Instant::now());
        }
        Message::ProjectExpandToggled(i) => {
            let Some(project) = app.config.projects.get_mut(i) else {
                return Task::none();
            };
            project.expanded = !project.expanded;
            if let Some(entry) = app.entries.get_mut(i) {
                entry.expanded = project.expanded;
            }
            return persist_edit(app, "env var change");
        }
        Message::EnvironmentActivated(i, location) => {
            let Some(project) = app.config.projects.get_mut(i) else {
                return Task::none();
            };
            // Only an environment this project actually has. A press naming
            // one it does not would otherwise leave `active` pointing at
            // nothing, and every caller falling back to the first -- which
            // looks like the click did something arbitrary.
            if !project
                .environments
                .iter()
                .any(|environment| environment.location == location)
            {
                return Task::none();
            }
            project.active = location.clone();
            for row in app
                .entries
                .get_mut(i)
                .into_iter()
                .flat_map(|entry| entry.environments.iter_mut())
            {
                row.active = row.location == location;
            }
            // Selecting the project too. Pressing one of its environments is
            // a statement about *that* project, and leaving the selection
            // where it was meant the detail header, the tabs and the refresh
            // below all went on describing a different one -- so the press
            // appeared to do nothing at all.
            app.selected = Some(i);
            app.dep_filter.clear();
            // The tabs describe the active environment, so they are re-read
            // for the same reason a finished sync re-reads them -- and now
            // for the project that was actually pressed.
            return Task::batch([
                persist_edit(app, "env var change"),
                selection_changed(app),
                refresh_after_sync(app, runner),
            ]);
        }
        Message::SyncRequested(i) => {
            app.context_menu = None;
            if let Some(id) = app.config.projects.get(i).map(|p| p.id)
                && running(app, id)
            {
                app.confirm_stop = Some((id, Box::new(Message::SyncRequested(i))));
                return Task::none();
            }
            if refuse_missing(app, i) {
                return Task::none();
            }
            if can_start(app)
                && let Some(project) = sync_target(app, i)
            {
                // A workspace root's plain `uv sync` installs no members at
                // all, so this button produced an environment with none of
                // the workspace's own packages in it and member-aware sync
                // was reachable only from the Members tab. `members_context`
                // answers `None` for an ordinary project, which is what keeps
                // the substitution a no-op for everyone else.
                if let Some((kind, discovered)) = members_context(app, i) {
                    return dispatch_members_sync(app, project, kind, discovered, runner);
                }
                // uv does not migrate an environment to another interpreter,
                // it deletes the directory and builds a new one -- so a pin
                // that disagrees with what is there is a destructive action,
                // and gets the same confirmation Recreate venv does.
                // The active environment's own pin -- see `run_sync`.
                let pin = project
                    .active_environment()
                    .map(|environment| environment.python.clone())
                    .unwrap_or_default();
                let venv_version = app.entries.get(i).and_then(|entry| {
                    entry
                        .environments
                        .iter()
                        .find(|row| row.active)
                        .and_then(|row| row.python_version.clone())
                });
                if sync_would_delete(&pin, venv_version.as_deref())
                    && let PythonPin::Version(to) = &pin
                    && let Some(from) = &venv_version
                    && let Some(environment) = project.active_environment()
                    && let Ok(venv_path) =
                        venv_path::resolve(&project, environment, &app.config.settings)
                {
                    app.confirm_pin_change = Some(context_menu::ConfirmPinChange {
                        project_id: project.id,
                        label: project.label.clone(),
                        venv_path,
                        from: from.clone(),
                        to: to.clone(),
                    });
                    return Task::none();
                }
                let config = app.config.clone();
                app.busy = Some(crate::busy::Busy::new(
                    project.id,
                    crate::busy::Action::Sync,
                ));
                // Fresh drawer per run, but showing a *running* status rather
                // than the default "idle": for the whole duration of the sync
                // this collapsed line is all the user has, and "idle" makes a
                // running sync look like a button that did nothing.
                drawer::start(&mut app.drawer, format!("syncing {}...", project.label));
                return Task::stream(sync_project(project, config, runner));
            }
        }
        Message::SyncLine(line) => {
            // Already redacted on the job thread, where the composed
            // environment (and therefore the set of secret values) exists --
            // see `Message::SyncLine`. `push_line` redacts again with an
            // empty set, a no-op kept so nothing reaching the drawer by some
            // other route can skip redaction, and it also re-expands the
            // drawer: output arriving is exactly what "a command is running"
            // means.
            drawer::push_line(&mut app.drawer, &line, &[]);
        }
        Message::DepVersionEditStarted {
            name,
            group,
            current,
        } => {
            if let Some(project) = app.selected {
                app.dep_edit = Some(deps::DepEdit {
                    project,
                    name,
                    group,
                    draft: current,
                });
                return Task::batch([
                    iced::widget::operation::focus(deps::VERSION_INPUT),
                    iced::widget::operation::select_all(deps::VERSION_INPUT),
                ]);
            }
        }
        Message::DepVersionDraftChanged(draft) => {
            if let Some(edit) = &mut app.dep_edit {
                edit.draft = draft;
            }
        }
        Message::DepVersionEditCancelled => app.dep_edit = None,
        Message::ToastDismissed(i) => {
            if i < app.toasts.len() {
                app.toasts.remove(i);
            }
        }
        Message::ShowOutput => app.drawer.expanded = true,
        Message::Tick(now) => {
            app.now = now;
            crate::busy::expire(&mut app.toasts, now);
        }
        Message::DepsViewSelected(view) => app.deps_view = view,
        Message::CloseRequested => {
            return update(app, Message::Tray(close_command(app)), runner);
        }
        Message::CloseToTraySelected(keep) => {
            app.config.settings.close_to_tray = keep;
            return persist_edit(app, "settings change");
        }
        Message::ThemeChoiceSelected(choice) => {
            app.config.settings.theme = choice;
            return persist_edit(app, "settings change");
        }
        Message::SystemThemeChanged(mode) => {
            // "No preference" is what stock GNOME reports with its dark-style
            // switch off -- the light desktop -- so it reads as light.
            app.system_mode = match mode {
                iced::theme::Mode::None => iced::theme::Mode::Light,
                other => other,
            };
        }
        Message::GitHubTick => {
            if !app.config.settings.watch_github {
                return Task::none();
            }
            let now = bombadil_core::github::now_unix();
            let due: Vec<_> = app
                .config
                .projects
                .iter()
                .filter(|p| !app.github_in_flight.contains(&p.id))
                .filter(|p| {
                    !matches!(
                        app.github.get(&p.id).map(|s| &s.lookup),
                        Some(bombadil_core::github::Lookup::RateLimited { until }) if *until > now
                    )
                })
                .map(|p| (p.id, project_dir(p)))
                .collect();
            if due.is_empty() {
                return Task::none();
            }
            app.github_in_flight.extend(due.iter().map(|(id, _)| *id));
            return look_up_github(due, app.config.settings.github_account.clone());
        }
        Message::GitHubRefreshRequested(id) => {
            if app.github_in_flight.contains(&id) {
                return Task::none();
            }
            let Some(project) = app.config.projects.iter().find(|p| p.id == id) else {
                return Task::none();
            };
            let due = vec![(id, project_dir(project))];
            app.github_in_flight.insert(id);
            return look_up_github(due, app.config.settings.github_account.clone());
        }
        Message::GitHubLooked(results) => {
            let mut tasks = Vec::new();
            for (id, lookup) in results {
                app.github_in_flight.remove(&id);
                // By id: the project may have been removed while this ran.
                let Some(project) = app.config.projects.iter().find(|p| p.id == id) else {
                    continue;
                };
                let previous = app.github.get(&id).and_then(|s| s.last_found.clone());
                let last_found = match &lookup {
                    bombadil_core::github::Lookup::Found(status) => {
                        if let Some(event) =
                            bombadil_core::github::transition::event(previous.as_ref(), status)
                            && app.config.settings.watch_github
                            && github_news(&mut app.github_notified, status, &event)
                        {
                            let (kind, title, body) =
                                crate::github_tab::notice(&project.label, status, &event);
                            crate::busy::push(
                                &mut app.toasts,
                                kind,
                                format!("{title}. {body}"),
                                std::time::Instant::now(),
                            );
                            tasks.push(notify_desktop(title, body));
                        }
                        Some(status.clone())
                    }
                    // A hiccup is not news: keep showing what was there.
                    bombadil_core::github::Lookup::Failed(_)
                    | bombadil_core::github::Lookup::RateLimited { .. } => previous,
                    _ => None,
                };
                app.github.insert(
                    id,
                    crate::github_tab::GitHubState {
                        lookup,
                        last_found,
                        checked_at: std::time::Instant::now(),
                    },
                );
            }
            return Task::batch(tasks);
        }
        Message::GitHubOpen(url) => {
            if !crate::github_tab::openable(&url) {
                return Task::none();
            }
            return Task::future(async move {
                job::run(
                    move || bombadil_core::folder::open_url(Os::host(), &url, runner.as_ref()).ok(),
                    VENV_PROBE_TIMEOUT,
                )
                .await
            })
            .discard();
        }
        Message::GitHubWatchSelected(on) => {
            app.config.settings.watch_github = on;
            let persist = persist_edit(app, "settings change");
            // Look now rather than at the next two-minute tick.
            let tick = if on {
                update(app, Message::GitHubTick, runner)
            } else {
                Task::none()
            };
            return Task::batch([persist, tick]);
        }
        Message::GitHubTokenDraftChanged(text) => app.github_token_draft = text.0,
        Message::GitHubTokenSaved => {
            let token = app.github_token_draft.clone();
            if token.trim().is_empty() {
                return Task::none();
            }
            return store_github_token(Some(token));
        }
        Message::GitHubTokenRemoved => return store_github_token(None),
        Message::GitHubTokenStored(result) => {
            let (kind, text) = match result {
                Ok(text) => {
                    app.github_token_draft.clear();
                    (crate::busy::ToastKind::Success, text)
                }
                Err(e) => (
                    crate::busy::ToastKind::Warning,
                    format!("Could not save the token: {e}"),
                ),
            };
            crate::busy::push(&mut app.toasts, kind, text, std::time::Instant::now());
            app.github_source = None;
            let refresh = app
                .selected
                .and_then(|i| app.config.projects.get(i))
                .map(|p| p.id);
            return Task::batch(
                [check_github(app.config.settings.github_account.clone())]
                    .into_iter()
                    .chain(refresh.map(|id| Task::done(Message::GitHubRefreshRequested(id)))),
            );
        }
        Message::GitHubSourceChecked(text) => app.github_source = Some(text),
        Message::GitHubAccountsListed(accounts) => app.github_accounts = Some(accounts),
        Message::GitHubAccountSelected(login) => {
            app.config.settings.github_account = Some(login);
            app.github_source = None;
            let persist = persist_edit(app, "settings change");
            let tick = update(app, Message::GitHubTick, runner);
            return Task::batch([
                persist,
                check_github(app.config.settings.github_account.clone()),
                tick,
            ]);
        }
        Message::GitHubAddAccount => {
            if app.github_login.is_some() {
                return Task::none();
            }
            app.github_login = Some(crate::github_tab::GitHubLogin::Waiting);
            let before = app.github_accounts.clone().unwrap_or_default();
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            app.github_login_cancel = Some(cancel.clone());
            return Task::stream(add_github_account(before, cancel));
        }
        Message::GitHubLoginCode(code) => {
            if app.github_login.is_some() {
                app.github_login = Some(crate::github_tab::GitHubLogin::Code(code));
            }
        }
        Message::GitHubLoginFinished(result) => {
            // Cancelled: gh may still have signed in, so the list is
            // refreshed, but nothing is chosen or announced.
            app.github_login_cancel = None;
            if app.github_login.take().is_none() {
                return check_github(app.config.settings.github_account.clone());
            }
            match result {
                Ok(login) => {
                    crate::busy::push(
                        &mut app.toasts,
                        crate::busy::ToastKind::Success,
                        format!("Signed in to GitHub as {login}"),
                        std::time::Instant::now(),
                    );
                    return update(app, Message::GitHubAccountSelected(login), runner);
                }
                Err(e) => crate::busy::push(
                    &mut app.toasts,
                    crate::busy::ToastKind::Warning,
                    format!("Could not add a GitHub account: {e}"),
                    std::time::Instant::now(),
                ),
            }
        }
        Message::GitHubLoginCancelled => {
            app.github_login = None;
            if let Some(cancel) = app.github_login_cancel.take() {
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        Message::GitHubBrowserSignIn => {
            if app.github_login.is_some() {
                return Task::none();
            }
            app.github_login = Some(crate::github_tab::GitHubLogin::Waiting);
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            app.github_login_cancel = Some(cancel.clone());
            return Task::stream(sign_in_with_browser(cancel));
        }
        Message::GitHubBrowserFinished(result) => {
            app.github_login_cancel = None;
            if app.github_login.take().is_none() {
                return Task::none();
            }
            return match result {
                Ok(text) => update(app, Message::GitHubTokenStored(Ok(text)), runner),
                Err(e) => {
                    crate::busy::push(
                        &mut app.toasts,
                        crate::busy::ToastKind::Warning,
                        format!("Could not sign in to GitHub: {e}"),
                        std::time::Instant::now(),
                    );
                    Task::none()
                }
            };
        }
        Message::GitHubCopyLoginCode => {
            if let Some(crate::github_tab::GitHubLogin::Code(code)) = &app.github_login {
                return iced::clipboard::write(code.clone());
            }
        }
        Message::SponsorRequested => {
            // Spawned, never waited on, off the render thread -- the browser
            // outlives the click. A failure has nowhere useful to go: the
            // link is in the README too.
            return Task::future(async move {
                job::run(
                    move || {
                        bombadil_core::folder::open_url(
                            Os::host(),
                            shell::SPONSOR_URL,
                            runner.as_ref(),
                        )
                        .ok()
                    },
                    VENV_PROBE_TIMEOUT,
                )
                .await
            })
            .discard();
        }
        Message::DepVersionSubmitted => {
            let Some(edit) = app.dep_edit.clone() else {
                return Task::none();
            };
            let requirement = match deps::pinned_requirement(&edit.name, &edit.draft) {
                Ok(requirement) => requirement,
                Err(why) => {
                    // The cell stays open so the user can correct it.
                    drawer::push_line(&mut app.drawer, &why, &[]);
                    drawer::set_idle(&mut app.drawer, "version not changed".to_string());
                    app.drawer.expanded = true;
                    return Task::none();
                }
            };
            if !can_start(app) {
                drawer::push_line(
                    &mut app.drawer,
                    "another uv command is still running; try again when it finishes",
                    &[],
                );
                return Task::none();
            }
            app.dep_edit = None;
            return add_requirement(app, edit.project, requirement, edit.group, runner);
        }
        Message::SyncFinished { success, lines } => {
            let finished = app.busy.take();
            let pruned = matches!(
                finished.as_ref().map(|b| &b.action),
                Some(crate::busy::Action::CachePrune)
            );
            // The selected project's Dependencies tab is re-read below; its
            // new installed list is what this action changed.
            let changed_selected = finished.as_ref().filter(|b| {
                success
                    && app
                        .selected
                        .and_then(|i| app.config.projects.get(i))
                        .is_some_and(|p| p.id == b.project_id)
            });
            let awaiting = changed_selected.map(|b| b.project_id);
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
            let status = match &finished {
                Some(b) => {
                    let name = project_label(app, b.project_id);
                    if success {
                        b.action.done(name)
                    } else {
                        b.action.failed(name)
                    }
                }
                None if success => "sync succeeded".to_string(),
                None => "sync failed".to_string(),
            };
            if let Some(id) = awaiting {
                app.awaiting_change.insert(id, status.clone());
            }
            // The drawer was cleared when this command started, so its
            // transcript is exactly this command's output.
            let diagnosis = match &finished {
                // No project: nothing to remember a failure against, and no
                // fix a button could apply.
                Some(b) if matches!(b.action, crate::busy::Action::CachePrune) => None,
                Some(b) if success => {
                    app.last_failure.remove(&b.project_id);
                    None
                }
                Some(b) => {
                    let mut d = bombadil_core::doctor::diagnose(&drawer::transcript(&app.drawer));
                    // These cannot be re-run from here: their input is gone.
                    if matches!(
                        b.action,
                        crate::busy::Action::AddLocal(_) | crate::busy::Action::AddProject
                    ) {
                        d.fixes
                            .retain(|f| *f != bombadil_core::doctor::DoctorFix::Retry);
                    }
                    app.last_failure
                        .insert(b.project_id, (b.action.clone(), d.clone()));
                    Some((b.project_id, d))
                }
                None => None,
            };
            if finished.is_some() {
                // The drawer stays as the user left it; a failure's toast
                // carries "Show output" for the reason.
                crate::busy::push(
                    &mut app.toasts,
                    if success {
                        crate::busy::ToastKind::Success
                    } else {
                        crate::busy::ToastKind::Failure
                    },
                    // An unrecognised failure keeps the action's own words,
                    // which at least name what failed.
                    match &diagnosis {
                        Some((_, d))
                            if !matches!(d.kind, bombadil_core::doctor::Kind::Other { .. }) =>
                        {
                            d.sentence.clone()
                        }
                        _ => status.clone(),
                    },
                    std::time::Instant::now(),
                );
                if let Some((id, d)) = diagnosis {
                    // The toast offers "Show output" itself; the doctor's
                    // first other fix is the one worth a button.
                    app.toasts[0].fix = d
                        .fixes
                        .iter()
                        .find(|fix| **fix != bombadil_core::doctor::DoctorFix::ShowOutput)
                        .cloned()
                        .map(|fix| (id, fix));
                }
            }
            drawer::set_idle(&mut app.drawer, status);
            // Nothing re-expands here any more. `set_idle` no longer
            // collapses, so there is nothing to undo -- and forcing the
            // drawer open on failure would overrule a user who had closed
            // it. A failed status renders in `boot` and the header says how
            // many lines are waiting, which is the signal without the
            // hijack.
            // A sync is the one action that changes what every read-only
            // surface describes: it installs and removes packages, and it
            // creates the environment when there was none. Without re-reading
            // here the Dependencies tab goes on listing what was installed
            // *before* the sync, and the sidebar goes on showing "no env" for
            // an environment that now exists -- both correct only until the
            // first Sync, and only fixed by restarting.
            //
            // Re-read on failure too: a failed sync is not an unchanged one.
            // uv installs what it can resolve before it hits the package it
            // cannot, and the tab has to describe the environment as it now
            // is, not as it was before an attempt that partly landed.
            if pruned {
                // Measured again: the toast says what a successful prune
                // freed; a failed one freed nothing worth saying.
                if !success {
                    app.disk.before_prune = None;
                }
                return measure_disk(app, runner, success);
            }
            return refresh_after_sync(app, runner);
        }
        // A folder, not the pyproject.toml inside it: the folder is what
        // people think of as the project.
        Message::AddProjectPickManifestRequested => {
            return Task::perform(pick_folder(), Message::AddProjectFolderPicked);
        }
        Message::AddProjectFolderPicked(None) => {}
        Message::AddProjectFolderPicked(Some(dir)) => {
            let registered: Vec<PathBuf> = app
                .config
                .projects
                .iter()
                .map(|p| p.pyproject_path.clone())
                .collect();
            let scanned = dir.clone();
            return Task::perform(scan_folder(dir, registered), move |found| {
                Message::AddProjectFolderScanned(scanned.clone(), found)
            });
        }
        Message::AddProjectFolderScanned(dir, (total, mut manifests)) => {
            let warn = |app: &mut App, text: String| {
                crate::busy::push(
                    &mut app.toasts,
                    crate::busy::ToastKind::Warning,
                    text,
                    std::time::Instant::now(),
                );
            };
            match manifests.len() {
                0 if total == 0 => warn(
                    app,
                    format!("No pyproject.toml in {} or its subfolders", dir.display()),
                ),
                0 => warn(
                    app,
                    format!("Every project in {} is already in Bombadil", dir.display()),
                ),
                1 => {
                    let manifest = manifests.remove(0);
                    return update(
                        app,
                        Message::AddProjectManifestPicked(Some(manifest)),
                        runner,
                    );
                }
                // One at a time: each gets its own interpreter and venv
                // choices in the dialog.
                _ => app.project_chooser = Some(manifests),
            }
        }
        Message::ProjectChosen(manifest) => {
            app.project_chooser = None;
            return update(
                app,
                Message::AddProjectManifestPicked(Some(manifest)),
                runner,
            );
        }
        Message::ProjectChooserCancelled => app.project_chooser = None,
        Message::FileDropped(path) => {
            // Not under another dialog: it would open beneath what is shown.
            if overlay_open(app) {
                return Task::none();
            }
            // A pyproject.toml, or else a folder. Nothing touches the disk
            // here: the folder scan runs off the render thread and says so
            // when a dropped path holds no project.
            let message = if path.file_name().is_some_and(|n| n == "pyproject.toml") {
                Message::AddProjectManifestPicked(Some(path))
            } else {
                Message::AddProjectFolderPicked(Some(path))
            };
            return update(app, message, runner);
        }
        // The dialog was cancelled -- nothing chosen, nothing to do.
        Message::AddProjectManifestPicked(None) => {}
        Message::AddProjectManifestPicked(Some(path)) => {
            // Picked or dropped: either way, a project is registered once.
            if app.config.projects.iter().any(|p| p.pyproject_path == path) {
                let dir = path.parent().unwrap_or(&path).display().to_string();
                crate::busy::push(
                    &mut app.toasts,
                    crate::busy::ToastKind::Warning,
                    format!("{dir} is already in Bombadil"),
                    std::time::Instant::now(),
                );
                return Task::none();
            }
            return Task::perform(load_manifest_draft(path), Message::AddProjectManifestLoaded);
        }
        Message::AddProjectManifestLoaded(draft)
            if app
                .config
                .projects
                .iter()
                .any(|p| p.pyproject_path == draft.pyproject_path) =>
        {
            // Registered while this was loading (two drops of one folder).
            crate::busy::push(
                &mut app.toasts,
                crate::busy::ToastKind::Warning,
                format!(
                    "{} is already in Bombadil",
                    draft
                        .pyproject_path
                        .parent()
                        .unwrap_or(&draft.pyproject_path)
                        .display()
                ),
                std::time::Instant::now(),
            );
        }
        Message::AddProjectManifestLoaded(draft) => {
            // The default venv choice already resolves to a real path, so
            // the adopt-or-create question has an answer from the moment the
            // dialog opens -- waiting for the user to touch the venv control
            // would leave the commonest case (accept the default) unanswered.
            let outcome_probe = probe_draft_venv(&draft, &app.config.settings);
            app.add_project = Some(draft);
            // Fired unconditionally, even for a manifest with no
            // requires-python: the dialog still needs a list to preselect
            // "newest available" from, and a fetch that only ran when a
            // requirement was present would leave that case stuck on
            // "checking..." forever.
            return Task::batch([
                Task::perform(
                    list_interpreters(app.config.clone(), runner),
                    Message::AddProjectInterpretersLoaded,
                ),
                outcome_probe,
            ]);
        }
        Message::AddProjectInterpretersLoaded(result) => {
            if let Some(draft) = &mut app.add_project {
                draft.interpreters = match result {
                    Ok(interpreters) => add_project::Interpreters::Loaded(interpreters),
                    Err(why) => add_project::Interpreters::Failed(why),
                };
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
                // Clearing first: the statement on screen describes the
                // folder the user just moved away from.
                draft.venv_outcome = None;
                return probe_draft_venv(draft, &app.config.settings);
            }
        }
        Message::AddProjectVenvOutcomeProbed(path, outcome) => {
            // Dropped unless it still describes the currently chosen
            // location -- `resolved_venv_path` performs no filesystem
            // access, so re-resolving here is free.
            if let Some(draft) = &mut app.add_project
                && add_project::resolved_venv_path(draft, &app.config.settings)
                    .is_ok_and(|current| current == path)
            {
                draft.venv_outcome = Some(outcome);
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
                draft.venv_outcome = None;
                return probe_draft_venv(draft, &app.config.settings);
            }
        }
        Message::AddProjectCancelled => {
            app.add_project = None;
        }
        Message::AddProjectConfirmed => {
            // Checked before `take`, so a refused press leaves the dialog
            // open: its first `uv sync` waits like any other uv command.
            if can_start(app)
                && let Some(draft) = app.add_project.take()
            {
                let project = add_project::to_project(&draft, &app.config.settings);
                let python_version = interpreter::preselect(
                    draft.interpreters.available(),
                    draft.requires_python.as_deref(),
                )
                .map(|i| i.version.clone());

                // In-memory first, synchronously -- criterion 3's other
                // failure mode is a dialog that looks like it worked but
                // dispatched nothing at all. Persisting is what makes this
                // survive a restart; that part happens off the render
                // thread below, the same as every other blocking call in
                // this file.
                app.config.projects.push(project.clone());
                app.entries.push(sidebar::Entry {
                    id: project.id,
                    label: project.label.clone(),
                    // Free: the dialog already parsed the manifest into
                    // `draft`, so there is no read to wait on here the way
                    // `probe_every_venv` needs one for a project loaded from
                    // config with no draft behind it.
                    requires_python: draft.requires_python.clone(),
                    // Resolved against the config the project was just pushed
                    // into, so a global pin applies to it from the first
                    // frame rather than only after the next `boot`.
                    environments: project
                        .environments
                        .iter()
                        .map(|environment| sidebar::EnvironmentRow {
                            location: environment.location.clone(),
                            path: venv_path::resolve(&project, environment, &app.config.settings)
                                .unwrap_or_default(),
                            python_version: None,
                            pin: environment.python.clone(),
                            active: environment.location == project.active,
                        })
                        .collect(),
                    expanded: project.expanded,
                    git: None,
                });
                let index = app.config.projects.len() - 1;
                app.selected = Some(index);
                app.dep_filter.clear();
                let opened = selection_changed(app);
                // This path runs `uv sync -p <version>` too, so it claims the
                // same single in-flight slot `SyncRequested` does. Without
                // it, confirming the dialog -- which also selects the new
                // project, and the detail header always renders Sync --
                // leaves the user one press away from two concurrent
                // `uv sync` on the same venv, interleaving into the one
                // shared drawer. See `can_start`, the one rule for that slot.
                app.busy = Some(crate::busy::Busy::new(
                    project.id,
                    crate::busy::Action::AddProject,
                ));

                let config = app.config.clone();
                let persist = app.persist.clone();
                return Task::perform(
                    confirm_add_project(project, config, python_version, persist, runner),
                    move |(success, lines)| Message::AddProjectConfirmFinished {
                        success,
                        lines,
                        index,
                    },
                )
                .chain(opened);
            }
        }
        Message::AddProjectConfirmFinished {
            success,
            lines,
            index,
        } => {
            // Releases the slot `AddProjectConfirmed` claimed. Never
            // clearing it would make the guard a deadlock: Sync would work
            // exactly zero times after adding a project.
            let finished = app.busy.take();
            if let Some(b) = &finished {
                // By the id the slot was claimed for: `index` can name a
                // different project once the list has moved.
                let name = project_label(app, b.project_id);
                let text = if success {
                    b.action.done(name)
                } else {
                    b.action.failed(name)
                };
                crate::busy::push(
                    &mut app.toasts,
                    if success {
                        crate::busy::ToastKind::Success
                    } else {
                        crate::busy::ToastKind::Failure
                    },
                    text,
                    std::time::Instant::now(),
                );
            }
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
            // Adding selected the project before its first sync built the
            // environment, so its tabs were read against nothing. Read them
            // again now that the sync has landed.
            let tabs = if app.selected == Some(index) {
                reprobe_project(app, index, runner.clone())
            } else {
                Task::none()
            };
            // Refreshes the sidebar entry the same way `boot` fills in every
            // other project's venv status, rather than trusting `success` to
            // imply a venv now exists.
            if let Some(project) = app.config.projects.get(index).cloned()
                && let Some(environment) = project.active_environment()
                && let Ok(path) = venv_path::resolve(&project, environment, &app.config.settings)
            {
                let pyproject_path = project.pyproject_path.clone();
                let location = environment.location.clone();
                let probe = Task::perform(
                    probe_venv_and_requirement(path, pyproject_path),
                    move |(python_version, requires_python)| Message::VenvProbed {
                        index,
                        location: location.clone(),
                        python_version,
                        requires_python,
                    },
                );
                return Task::batch([probe, tabs]);
            }
            return tabs;
        }
        // A stale index can arrive the same way `ProjectSelected`'s can.
        Message::ContextMenuOpened(i) if i < app.config.projects.len() => {
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
        Message::OpenTerminalRequested(i, choice) if i < app.config.projects.len() => {
            app.context_menu = None;
            if let Some(project) = app.config.projects.get(i).cloned() {
                let config = app.config.clone();
                return Task::perform(
                    open_terminal(choice, project, config, runner),
                    Message::OpenTerminalFinished,
                );
            }
        }
        Message::OpenTerminalRequested(_, _) => {}
        Message::OpenTerminalHereRequested(i) => {
            return open_terminal_here(app, i, None, runner);
        }
        Message::OpenTerminalHereDetected(id, detected, environment) => {
            let Some(project) = app
                .config
                .projects
                .iter()
                .position(|p| p.id == id)
                .and_then(|i| project_for_open(app, i, environment.as_ref()))
            else {
                return Task::none();
            };
            // `None` for the default: this arm is only reached because there
            // was not one. Passing it again would re-ask a question already
            // answered.
            match context_menu::one_press_plan(None, Some(&detected)) {
                context_menu::OpenPlan::Open(choice) => {
                    let config = app.config.clone();
                    return Task::perform(
                        open_terminal(choice, project, config, runner),
                        Message::OpenTerminalFinished,
                    );
                }
                context_menu::OpenPlan::Nothing => report_no_terminal(app),
                // Cannot happen: `detected` is `Some`, so the plan is never
                // "look again". Matched rather than panicked, as above.
                context_menu::OpenPlan::Detect => {}
            }
        }
        Message::OpenTerminalFinished(Ok(())) => {}
        Message::OpenTerminalFinished(Err(message)) => {
            // Redacted already, on the job thread inside `terminal::open::open`
            // -- the composed environment never reaches this `Message`, the
            // same discipline `SyncFinished` follows for `run_sync`.
            drawer::push_line(&mut app.drawer, &message, &[]);
            drawer::set_idle(&mut app.drawer, "open terminal failed".to_string());
        }
        // The whole minimise-to-tray behaviour. `hide` is only ever reached
        // when a tray is actually running -- see `tray_is_running` and the
        // rule in `tray`'s module doc: hiding with nothing to un-hide from
        // leaves a process with no window, no icon and no way back.
        Message::Tray(tray::TrayCommand::OpenTerminal {
            project,
            environment,
        }) => {
            if let Some(i) = app.config.projects.iter().position(|p| p.id == project)
                && project_for_open(app, i, Some(&environment)).is_some()
            {
                return open_terminal_here(app, i, Some(environment), runner);
            }
        }
        Message::Tray(tray::TrayCommand::OpenPycharm {
            project,
            environment,
        }) => {
            let Some(project) = app
                .config
                .projects
                .iter()
                .find(|p| p.id == project)
                .and_then(|p| pointed_at(p, &environment))
            else {
                return Task::none();
            };
            let config = app.config.clone();
            return Task::perform(
                open_pycharm(project, config, runner),
                Message::OpenPycharmFinished,
            );
        }
        Message::Tray(command) => {
            // "Hide" closes the window and keeps the process: Wayland cannot
            // hide a window at all (winit's `set_visible` is a no-op there),
            // and closing works the same on every platform. The state lives
            // on in `App`; Show opens a fresh window onto it.
            return match tray::window_action(command, app.tray_is_running) {
                tray::WindowAction::Show => match app.window {
                    // Focus as well as show: a window behind three others
                    // has technically obeyed and practically not.
                    Some(id) => iced::window::gain_focus(id),
                    None => open_window(app),
                },
                tray::WindowAction::Hide => match app.window.take() {
                    Some(id) => iced::window::close(id),
                    None => Task::none(),
                },
                tray::WindowAction::Close if runs_live(app) => {
                    app.confirm_quit = true;
                    // Quit from the tray with the window closed to it: the
                    // question needs a window to be asked in.
                    match app.window {
                        Some(_) => Task::none(),
                        None => open_window(app),
                    }
                }
                tray::WindowAction::Close => {
                    remember_window_size(app);
                    iced::exit()
                }
                tray::WindowAction::Stay => Task::none(),
            };
        }
        Message::OpenPycharmRequested(i) => {
            let Some(project) = app.config.projects.get(i).cloned() else {
                return Task::none();
            };
            let config = app.config.clone();
            return Task::perform(
                open_pycharm(project, config, runner),
                Message::OpenPycharmFinished,
            );
        }
        Message::OpenPycharmFinished(Ok(None)) => {}
        Message::OpenPycharmFinished(Ok(Some(warning))) => {
            // PyCharm is up; only the interpreter step fell short. A line, not
            // an opened drawer -- the IDE window is where the user is looking.
            drawer::push_line(
                &mut app.drawer,
                &format!("PyCharm opened, but {warning}"),
                &[],
            );
        }
        Message::OpenPycharmFinished(Err(message)) => {
            // Already redacted on the job thread, same as the terminal path.
            drawer::push_line(&mut app.drawer, &message, &[]);
            drawer::set_idle(&mut app.drawer, "open PyCharm failed".to_string());
            // Opened, unlike a failed terminal open: "PyCharm was not found"
            // is the likely message and it names something the user can act
            // on, but only if they can see it.
            app.drawer.expanded = true;
        }
        Message::RemoveProjectRequested(i) if i < app.config.projects.len() => {
            // In-memory first, synchronously -- the same discipline
            // `AddProjectConfirmed` uses, so the sidebar reflects the removal
            // the instant `update` returns rather than only once the
            // background persist lands.
            let removed = app.config.projects.remove(i);
            // Unregistering a project ends what it was running.
            if let Some(run) = app.runs.remove(&removed.id) {
                run.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            app.after_stop.remove(&removed.id);
            app.github.remove(&removed.id);
            app.github_in_flight.remove(&removed.id);
            app.dep_filter.clear();
            app.entries.remove(i);
            // Indices shift on removal; `deps` is tagged with one, and a
            // shifted-but-uncleared tag would show a stale tab against
            // whichever project now sits at that index rather than the one
            // it was actually fetched for. `ProjectSelected` refetches it
            // the next time a selection lands.
            app.deps = None;
            app.dep_edit = None;
            // Same reasoning as `deps`, plus a pending removal confirmation
            // that could now be naming a project index that no longer means
            // what it did.
            app.members = None;
            app.confirm_member_sync = None;
            // A pending Recreate venv confirmation describes the list as it
            // was. Its `project_id` already refuses to resolve to a different
            // project, but the sentence on screen still names one the user is
            // no longer looking at, so it goes too.
            app.confirm_recreate = None;
            app.selected = match app.selected {
                Some(s) if s == i => (!app.config.projects.is_empty())
                    .then_some(i.min(app.config.projects.len().saturating_sub(1))),
                Some(s) if s > i => Some(s - 1),
                other => other,
            };
            app.context_menu = None;

            let config = app.config.clone();
            let persist = app.persist.clone();
            let opened = selection_changed(app);
            return Task::batch([
                Task::perform(persist_config(config, persist), |success| {
                    Message::Persisted {
                        what: "remove",
                        success,
                    }
                }),
                opened,
            ]);
        }
        Message::RemoveProjectRequested(_) => {}
        // --- Run lane ---
        Message::RunRequested {
            project,
            label,
            args,
        } => {
            let Some(i) = app.config.projects.iter().position(|p| p.id == project) else {
                return Task::none();
            };
            if run_blocked(app, i).is_some() {
                return Task::none();
            }
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            app.runs.insert(
                project,
                run_tab::Run {
                    label,
                    lines: Vec::new(),
                    started: std::time::Instant::now(),
                    cancel: cancel.clone(),
                    end: None,
                },
            );
            let target = app.config.projects[i].clone();
            let config = app.config.clone();
            return Task::stream(
                job::stream_untimed(move |emit| {
                    let source = config.settings.just_source;
                    let end = match run_tab::with_resolved_just(args, &|| resolve_just_for(source))
                    {
                        Err(why) => run_tab::RunEnd::Failed(why),
                        Ok(args) => run_program(
                            &target,
                            &config,
                            &args,
                            &KeyringSecretStore::new(),
                            runner.as_ref(),
                            &cancel,
                            &mut |line| emit(Message::RunLine(project, line)),
                        ),
                    };
                    emit(Message::RunFinished(project, end));
                })
                .map(move |result| match result {
                    job::JobResult::Done(message) => message,
                    job::JobResult::TimedOut | job::JobResult::Failed => Message::RunFinished(
                        project,
                        run_tab::RunEnd::Failed("the run ended unexpectedly".into()),
                    ),
                }),
            );
        }
        Message::RunLine(id, line) => {
            if let Some(run) = app.runs.get_mut(&id) {
                run.push(line);
            }
        }
        Message::RunFinished(id, end) => {
            if let Some(run) = app.runs.get_mut(&id) {
                run.end = Some(end);
            }
            if app.quitting && !app.runs.values().any(run_tab::Run::live) {
                remember_window_size(app);
                return iced::exit();
            }
            if let Some(next) = app.after_stop.remove(&id) {
                return update(app, next, runner);
            }
        }
        Message::RunStopRequested(id) => {
            if let Some(run) = app.runs.get(&id) {
                run.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        Message::StopThenConfirmed => {
            if let Some((id, next)) = app.confirm_stop.take() {
                app.after_stop.insert(id, *next);
                return update(app, Message::RunStopRequested(id), runner);
            }
        }
        Message::StopThenCancelled => app.confirm_stop = None,
        Message::QuitConfirmed => {
            app.confirm_quit = false;
            // The program may have ended while the question was open.
            if !runs_live(app) {
                remember_window_size(app);
                return iced::exit();
            }
            app.quitting = true;
            for run in app.runs.values() {
                run.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        Message::QuitCancelled => app.confirm_quit = false,
        Message::VulnsChecked(venv, key, result) => {
            app.vulns.insert(
                venv,
                VulnCheck {
                    key,
                    at: std::time::Instant::now(),
                    result,
                },
            );
        }
        Message::DepDetailsToggled(i, name) => {
            let Some(id) = app.config.projects.get(i).map(|p| p.id) else {
                return Task::none();
            };
            if matches!(&app.dep_details, Some((p, n, _)) if *p == id && *n == name) {
                app.dep_details = None;
                return Task::none();
            }
            app.dep_details = Some((id, name.clone(), None));
            let venv = app
                .entries
                .get(i)
                .and_then(|e| e.environments.iter().find(|r| r.active))
                .map(|r| r.path.clone());
            let version = installed_version(app, i, &name);
            let (Some(venv), Some(version)) = (venv, version) else {
                return Task::none();
            };
            return Task::perform(
                async move {
                    let info = bombadil_core::metadata::read(&venv, &name, &version);
                    (name, info)
                },
                move |(name, info)| Message::DepDetailsLoaded(id, name, info),
            );
        }
        Message::DepDetailsLoaded(id, name, info) => {
            if let Some((p, n, slot)) = &mut app.dep_details
                && *p == id
                && *n == name
            {
                *slot = info;
            }
        }
        Message::DepPinRequested(i, name, version, group) => {
            return add_requirement(app, i, format!("{name}=={version}"), group, runner);
        }
        Message::JustSourceSelected(source) => {
            app.config.settings.just_source = source;
            let reread = tab_opened(app);
            return Task::batch([persist_edit(app, "settings change"), reread]);
        }
        Message::CheckVulnerabilitiesSelected(on) => {
            app.config.settings.check_vulnerabilities = on;
            if !on {
                // Off means no answers either, not old ones left standing.
                app.vulns.clear();
            }
            return persist_edit(app, "settings change");
        }
        // --- Disk ---
        Message::DiskMeasured(mut measured, after_prune) => {
            // Only the measurement the prune asked for says what it freed:
            // one already under way when it started would compare the wrong
            // sizes.
            let before = if after_prune {
                app.disk.before_prune.take()
            } else {
                None
            };
            if let Some(before) = before
                && let Some((_, Some(after))) = &measured.cache
            {
                crate::busy::push(
                    &mut app.toasts,
                    crate::busy::ToastKind::Success,
                    crate::disk::freed_text(before, *after),
                    std::time::Instant::now(),
                );
            }
            measured.confirm = app.disk.confirm.take();
            measured.before_prune = app.disk.before_prune.take();
            app.disk = measured;
        }
        Message::DiskDeleteRequested(delete) => {
            if let crate::disk::Delete::Environment(id, _, _) = &delete {
                let id = *id;
                if refuse_while_running(app, id) {
                    return Task::none();
                }
                if app.busy.as_ref().is_some_and(|b| b.project_id == id) {
                    crate::busy::push(
                        &mut app.toasts,
                        crate::busy::ToastKind::Warning,
                        "Wait for the current uv command to finish.".into(),
                        std::time::Instant::now(),
                    );
                    return Task::none();
                }
            }
            app.disk.confirm = Some(delete);
        }
        Message::DiskDeleteCancelled => app.disk.confirm = None,
        Message::DiskDeleteConfirmed => {
            let Some(delete) = app.disk.confirm.take() else {
                return Task::none();
            };
            let config = app.config.clone();
            let asked = delete.clone();
            return Task::perform(
                job::attempt(
                    move || crate::disk::delete_blocking(&asked, &config),
                    SYNC_TIMEOUT,
                    "deleting the environment",
                ),
                move |result| Message::DiskDeleted(delete.clone(), result),
            );
        }
        Message::DiskDeleted(delete, result) => {
            let path = delete.path().display().to_string();
            let save = match (&delete, &result) {
                (crate::disk::Delete::Environment(id, location, _), Ok(())) => {
                    if let Some(project) = app.config.projects.iter_mut().find(|p| p.id == *id) {
                        project.environments.retain(|e| e.location != *location);
                    }
                    rebuild_entries(app);
                    persist_edit(app, "env var change")
                }
                _ => Task::none(),
            };
            let (kind, text) = match result {
                Ok(()) => (crate::busy::ToastKind::Success, format!("Deleted {path}")),
                Err(why) => (
                    crate::busy::ToastKind::Warning,
                    format!("Could not delete {path}: {why}"),
                ),
            };
            crate::busy::push(&mut app.toasts, kind, text, std::time::Instant::now());
            return Task::batch([save, measure_disk(app, runner, false)]);
        }
        Message::CachePruneRequested => {
            if !can_start(app) {
                return Task::none();
            }
            app.disk.before_prune = app.disk.cache.as_ref().and_then(|(_, size)| *size);
            app.busy = Some(crate::busy::Busy::new(
                uuid::Uuid::nil(),
                crate::busy::Action::CachePrune,
            ));
            drawer::start(&mut app.drawer, "cleaning uv's cache...".to_string());
            let config = app.config.clone();
            return Task::stream(sync_stream(
                move |on_line| {
                    let uv = match resolve_uv_for(&config, runner.as_ref()) {
                        Ok(uv) => uv,
                        Err(e) => return (false, vec![e]),
                    };
                    let spec = uv::command::cache_prune(&uv);
                    match runner.stream(&spec, &mut |line| on_line(line.text().to_string())) {
                        Ok(outcome) => (outcome.success(), Vec::new()),
                        Err(e) => (false, vec![e.to_string()]),
                    }
                },
                "cleaning uv's cache timed out",
                "cleaning uv's cache failed unexpectedly",
            ));
        }
        Message::MissingSwept(gone) => {
            app.missing = gone
                .into_iter()
                .filter(|id| app.config.projects.iter().any(|p| p.id == *id))
                .collect();
        }
        // --- Find package ---
        Message::FindOpened => {
            app.open_menu = None;
            app.find = Some((String::new(), None));
            let projects = app.config.projects.clone();
            // Its own thread with a time limit: a project on a sleeping
            // network drive must not hold one of the executor's threads.
            return Task::perform(
                async move {
                    match job::run(
                        move || crate::find_package::index_blocking(&projects),
                        HEALTH_PROBE_TIMEOUT,
                    )
                    .await
                    {
                        job::JobResult::Done(index) => index,
                        job::JobResult::TimedOut | job::JobResult::Failed => Vec::new(),
                    }
                },
                Message::FindIndexed,
            );
        }
        Message::FindIndexed(index) => {
            if let Some((_, slot)) = &mut app.find {
                *slot = Some(index);
            }
        }
        Message::FindQuery(query) => {
            if let Some((q, _)) = &mut app.find {
                *q = query;
            }
        }
        Message::FindClosed => app.find = None,
        Message::FindPicked(id, name) => {
            app.find = None;
            let Some(i) = app.config.projects.iter().position(|p| p.id == id) else {
                return Task::none();
            };
            app.tab = Tab::Dependencies;
            let selected = update(app, Message::ProjectSelected(i), runner);
            app.dep_filter = name;
            return selected;
        }
        // --- New project ---
        Message::NewProjectRequested => {
            app.new_project = Some(crate::new_project::Draft {
                name: String::new(),
                location: crate::new_project::default_location(&app.config.projects, &home_dir()),
                kind: crate::new_project::InitKind::App,
                python: None,
                git: true,
                error: None,
            });
            return probe_installed_pythons(app.config.clone());
        }
        Message::NewProjectName(v) => {
            if let Some(d) = &mut app.new_project {
                d.name = v;
                d.error = None;
            }
        }
        Message::NewProjectBrowse => {
            return Task::perform(pick_folder(), Message::NewProjectLocation);
        }
        Message::NewProjectLocation(None) => {}
        Message::NewProjectLocation(Some(dir)) => {
            if let Some(d) = &mut app.new_project {
                d.location = dir;
                d.error = None;
            }
        }
        Message::NewProjectKind(kind) => {
            if let Some(d) = &mut app.new_project {
                d.kind = kind;
            }
        }
        Message::NewProjectPython(python) => {
            if let Some(d) = &mut app.new_project {
                d.python = python;
            }
        }
        Message::NewProjectGit(on) => {
            if let Some(d) = &mut app.new_project {
                d.git = on;
            }
        }
        Message::NewProjectCancelled => {
            if !app.creating_project {
                app.new_project = None;
            }
        }
        Message::NewProjectCreate => {
            let Some(draft) = app.new_project.clone() else {
                return Task::none();
            };
            if app.creating_project || crate::new_project::name_error(&draft.name).is_some() {
                return Task::none();
            }
            app.creating_project = true;
            let config = app.config.clone();
            return Task::perform(
                job::attempt(
                    move || crate::new_project::init_blocking(&draft, &config, runner.as_ref()),
                    SYNC_TIMEOUT,
                    "creating the project",
                ),
                Message::NewProjectCreated,
            );
        }
        Message::NewProjectCreated(result) => {
            app.creating_project = false;
            match result {
                Ok(manifest) => {
                    app.new_project = None;
                    return update(
                        app,
                        Message::AddProjectManifestPicked(Some(manifest)),
                        runner,
                    );
                }
                Err(e) => {
                    if let Some(d) = &mut app.new_project {
                        d.error = Some(e);
                    }
                }
            }
        }
        // --- Run tab ---
        Message::EntryPointsLoaded(id, names) => app.entry_points = Some((id, names)),
        Message::CommandDraftLabel(v) => app.command_draft.label = v,
        Message::CommandDraftCommand(v) => app.command_draft.command = v,
        Message::CommandEdit(row) => {
            if let Some(c) = app
                .selected
                .and_then(|i| app.config.projects.get(i)?.commands.get(row))
            {
                app.command_draft = run_tab::CommandDraft {
                    label: c.label.clone(),
                    command: c.command.clone(),
                    editing: Some(row),
                };
            }
        }
        Message::CommandSaved => {
            let draft = std::mem::take(&mut app.command_draft);
            let command = draft.command.trim().to_string();
            let Some(project) = app.selected.and_then(|i| app.config.projects.get_mut(i)) else {
                return Task::none();
            };
            if command.is_empty() {
                app.command_draft = draft;
                return Task::none();
            }
            let label = match draft.label.trim() {
                "" => command.clone(),
                l => l.to_string(),
            };
            let saved = bombadil_core::model::SavedCommand { label, command };
            match draft.editing.filter(|row| *row < project.commands.len()) {
                Some(row) => project.commands[row] = saved,
                None => project.commands.push(saved),
            }
            return persist_edit(app, "saved command");
        }
        Message::CommandRemoved(row) => {
            if let Some(project) = app.selected.and_then(|i| app.config.projects.get_mut(i))
                && row < project.commands.len()
            {
                project.commands.remove(row);
                app.command_draft = run_tab::CommandDraft::default();
                return persist_edit(app, "saved command");
            }
        }
        Message::RunFileRequested => {
            let Some(project) = app.selected.and_then(|i| app.config.projects.get(i)) else {
                return Task::none();
            };
            let id = project.id;
            let dir = project
                .pyproject_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            return Task::perform(
                async move {
                    rfd::AsyncFileDialog::new()
                        .set_directory(dir)
                        .add_filter("Python", &["py"])
                        .pick_file()
                        .await
                        .map(|handle| handle.path().to_path_buf())
                },
                move |file| Message::RunFilePicked(id, file),
            );
        }
        Message::JustLoaded(id, state) => {
            if app
                .selected
                .and_then(|i| app.config.projects.get(i))
                .is_some_and(|p| p.id == id)
            {
                app.just = Some((id, state));
            }
        }
        Message::JustArgsChanged(recipe, text) => {
            app.just_args.insert(recipe, text);
        }
        Message::JustShowRequested(recipe) => {
            if app.just_shown.as_ref().is_some_and(|(r, _)| *r == recipe) {
                app.just_shown = None;
                return Task::none();
            }
            let Some(dir) = app
                .selected
                .and_then(|i| app.config.projects.get(i))
                .and_then(|p| p.pyproject_path.parent().map(Path::to_path_buf))
            else {
                return Task::none();
            };
            let name = recipe.clone();
            let source = app.config.settings.just_source;
            return Task::perform(
                job::attempt(
                    move || {
                        let runner = bombadil_core::process::SystemCommandRunner;
                        let just = resolve_just_for(source)?
                            .ok_or_else(|| run_tab::JUST_NOT_ON_PATH.to_string())?
                            .path;
                        let out = runner
                            .run(&bombadil_core::just::show(&just, &dir, &name))
                            .map_err(|e| e.to_string())?;
                        if out.success() {
                            Ok(out.stdout)
                        } else {
                            Err(out.stderr.trim().to_string())
                        }
                    },
                    VENV_PROBE_TIMEOUT,
                    "reading the recipe",
                ),
                move |result| Message::JustShown(recipe.clone(), result),
            );
        }
        Message::JustShown(recipe, result) => {
            app.just_shown = Some((
                recipe,
                match result {
                    Ok(text) => text,
                    Err(why) => why,
                },
            ));
        }
        Message::RunFilePicked(_, None) => {}
        Message::RunFilePicked(id, Some(file)) => {
            let Some(project) = app.config.projects.iter_mut().find(|p| p.id == id) else {
                return Task::none();
            };
            let dir = project
                .pyproject_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            let command = run_tab::file_command(&dir, &file);
            let label = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| command.clone());
            if !project.commands.iter().any(|c| c.command == command) {
                project.commands.push(bombadil_core::model::SavedCommand {
                    label: label.clone(),
                    command: command.clone(),
                });
            }
            let save = persist_edit(app, "saved command");
            let run = update(
                app,
                Message::RunRequested {
                    project: id,
                    label,
                    args: run_tab::split_command(&command),
                },
                runner,
            );
            return Task::batch([save, run]);
        }
        Message::LocateRequested(id) => {
            return Task::perform(pick_pyproject(), move |picked| {
                Message::LocatePicked(id, picked)
            });
        }
        // --- Move to another computer ---
        Message::BackupPassphraseChanged(v) => app.backup.passphrase = v.0,
        Message::BackupConfirmChanged(v) => app.backup.confirm = v.0,
        Message::BackupExportRequested => {
            let (p, c) = (&app.backup.passphrase, &app.backup.confirm);
            let problem = if p != c {
                Some("The passphrases don't match")
            } else if !p.is_empty() && p.chars().count() < 8 {
                Some("Use at least 8 characters for the passphrase")
            } else {
                None
            };
            if let Some(text) = problem {
                crate::busy::push(
                    &mut app.toasts,
                    crate::busy::ToastKind::Warning,
                    text.into(),
                    std::time::Instant::now(),
                );
                return Task::none();
            }
            return Task::perform(pick_bombadil_save(), Message::BackupExportPicked);
        }
        Message::BackupExportPicked(None) => {}
        Message::BackupExportPicked(Some(path)) => {
            let config = app.config.clone();
            let passphrase = app.backup.passphrase.clone();
            return Task::perform(
                async move {
                    job::attempt(
                        move || {
                            crate::backup_panel::export_blocking(
                                &config,
                                &KeyringSecretStore::new(),
                                &passphrase,
                                bombadil_core::backup::crypto::Kdf::DEFAULT,
                                &home_dir(),
                                &path,
                            )
                        },
                        SYNC_TIMEOUT,
                        "exporting",
                    )
                    .await
                },
                Message::BackupExportFinished,
            );
        }
        Message::BackupExportFinished(Ok(out)) => {
            app.backup.passphrase.clear();
            app.backup.confirm.clear();
            let mut text = format!(
                "Exported {}, {}, {}",
                crate::health::plural(app.config.projects.len(), "project", "projects"),
                crate::health::plural(app.config.indexes.len(), "index", "indexes"),
                crate::health::plural(out.secrets, "secret", "secrets"),
            );
            if !out.missing.is_empty() {
                let n = out.missing.len();
                text.push_str(&format!(
                    ". {} in the keychain",
                    crate::health::plural(n, "secret wasn't", "secrets weren't")
                ));
            }
            crate::busy::push(
                &mut app.toasts,
                crate::busy::ToastKind::Success,
                text,
                std::time::Instant::now(),
            );
        }
        Message::BackupExportFinished(Err(e)) => {
            crate::busy::push(
                &mut app.toasts,
                crate::busy::ToastKind::Warning,
                format!("Could not export: {e}"),
                std::time::Instant::now(),
            );
        }
        Message::BackupImportRequested => {
            return Task::perform(pick_bombadil_open(), Message::BackupImportPicked);
        }
        Message::BackupImportPicked(None) => {}
        Message::BackupImportPicked(Some(path)) => {
            return Task::perform(
                async move {
                    let read = std::fs::read_to_string(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|t| bombadil_core::backup::read(&t).map_err(|e| e.to_string()));
                    (path, read)
                },
                |(path, read)| Message::BackupImportRead(path, read),
            );
        }
        Message::BackupImportRead(_, Err(e)) => {
            crate::busy::push(
                &mut app.toasts,
                crate::busy::ToastKind::Warning,
                format!("Could not open: {e}"),
                std::time::Instant::now(),
            );
        }
        Message::BackupImportRead(path, Ok(bundle)) => {
            let maps =
                bombadil_core::backup::mapping::suggest(&bundle.config, &bundle.home, &home_dir());
            let secrets = bundle
                .secrets
                .is_none()
                .then(bombadil_core::backup::Secrets::default);
            app.backup.import = Some(crate::backup_panel::Import {
                path,
                bundle,
                passphrase: String::new(),
                secrets,
                maps,
                mode: None,
                error: None,
            });
        }
        Message::BackupUnlockChanged(v) => {
            if let Some(import) = &mut app.backup.import {
                import.passphrase = v.0;
            }
        }
        Message::BackupUnlockRequested => {
            if let Some(import) = &app.backup.import {
                let bundle = import.bundle.clone();
                let passphrase = import.passphrase.clone();
                let path = import.path.clone();
                return Task::perform(
                    async move {
                        match job::run(
                            move || {
                                bombadil_core::backup::unlock(&bundle, &passphrase)
                                    .map_err(|e| e.to_string())
                            },
                            SYNC_TIMEOUT,
                        )
                        .await
                        {
                            job::JobResult::Done(r) => r,
                            _ => Err("unlocking failed unexpectedly".into()),
                        }
                    },
                    move |result| Message::BackupUnlocked(path.clone(), result),
                );
            }
        }
        Message::BackupUnlocked(path, result) => {
            if let Some(import) = &mut app.backup.import
                && import.path == path
            {
                match result {
                    Ok(secrets) => {
                        import.secrets = Some(secrets);
                        import.error = None;
                        import.passphrase.clear();
                    }
                    Err(e) => import.error = Some(e),
                }
            }
        }
        Message::BackupMapChanged(row, text) => {
            if let Some(map) = app.backup.import.as_mut().and_then(|i| i.maps.get_mut(row)) {
                map.to = PathBuf::from(text);
            }
        }
        Message::BackupMapBrowse(row) => {
            return Task::perform(pick_folder(), move |folder| {
                Message::BackupMapPicked(row, folder)
            });
        }
        Message::BackupMapPicked(_, None) => {}
        Message::BackupMapPicked(row, Some(folder)) => {
            if let Some(map) = app.backup.import.as_mut().and_then(|i| i.maps.get_mut(row)) {
                map.to = folder;
            }
        }
        Message::BackupModeSelected(mode) => {
            if let Some(import) = &mut app.backup.import {
                import.mode = Some(mode);
            }
        }
        Message::BackupImportCancelled => app.backup.import = None,
        Message::BackupImportConfirmed => {
            let Some(import) = &app.backup.import else {
                return Task::none();
            };
            let (Some(secrets), Some(mode)) = (import.secrets.clone(), import.mode) else {
                return Task::none();
            };
            let incoming =
                bombadil_core::backup::mapping::apply(import.bundle.config.clone(), &import.maps);
            let current = app.config.clone();
            let persist = app.persist.clone();
            let exists = app.path_exists;
            return Task::perform(
                async move {
                    job::attempt(
                        move || {
                            crate::backup_panel::import_blocking(
                                &current,
                                incoming,
                                secrets,
                                mode,
                                persist.as_ref(),
                                &KeyringSecretStore::new(),
                                exists,
                            )
                        },
                        SYNC_TIMEOUT,
                        "importing",
                    )
                    .await
                },
                Message::BackupImportFinished,
            );
        }
        Message::BackupImportFinished(Err(e)) => {
            crate::busy::push(
                &mut app.toasts,
                crate::busy::ToastKind::Warning,
                format!("Could not import: {e}"),
                std::time::Instant::now(),
            );
        }
        Message::BackupImportFinished(Ok((config, report, copy))) => {
            app.config = config;
            app.backup.import = None;
            app.entries = sidebar::entries(&app.config);
            // Index rows and env drafts are keyed by position in lists the
            // import just replaced: re-seeded the way `App::new` does, or an
            // edit to row 0 would move the old index's credential over the
            // imported one.
            app.index_credential_drafts = vec![String::new(); app.config.indexes.len()];
            app.index_credential_names =
                app.config.indexes.iter().map(|i| i.name.clone()).collect();
            app.env_value_drafts.clear();
            app.missing = app
                .config
                .projects
                .iter()
                .filter(|p| !(app.path_exists)(&p.pyproject_path))
                .map(|p| p.id)
                .collect();
            let mut text = format!(
                "Imported {}, {}, {}",
                crate::health::plural(report.projects, "project", "projects"),
                crate::health::plural(report.indexes, "index", "indexes"),
                crate::health::plural(report.secrets, "secret", "secrets"),
            );
            if report.skipped > 0 {
                text.push_str(&format!(". Skipped {} already here", report.skipped));
            }
            if !report.not_found.is_empty() {
                text.push_str(&format!(
                    ". {} not found, use Locate",
                    crate::health::plural(
                        report.not_found.len(),
                        "project folder",
                        "project folders"
                    )
                ));
            }
            if let Some(copy) = copy {
                text.push_str(&format!(". Old settings saved as {}", copy.display()));
            }
            if let Some(e) = &report.secrets_error {
                text.push_str(&format!(". The secrets weren't saved: {e}"));
            }
            crate::busy::push(
                &mut app.toasts,
                match report.secrets_error {
                    Some(_) => crate::busy::ToastKind::Warning,
                    None => crate::busy::ToastKind::Success,
                },
                text,
                std::time::Instant::now(),
            );
            // Tabs are tagged by index, and the list they were read for is
            // gone: dropped, then the first project is selected the way a
            // click does, which re-reads it.
            app.deps = None;
            app.dep_edit = None;
            app.members = None;
            app.selected = None;
            let venvs = probe_every_venv(app);
            if app.config.projects.is_empty() {
                return venvs;
            }
            return Task::batch([venvs, update(app, Message::ProjectSelected(0), runner)]);
        }

        Message::LocatePicked(_, None) => {}
        Message::LocatePicked(id, Some(path)) => {
            let Some(i) = app.config.projects.iter().position(|p| p.id == id) else {
                return Task::none();
            };
            app.config.projects[i].pyproject_path = path;
            app.missing.remove(&id);
            // The sidebar caches each environment's resolved path, which
            // hangs off the project's folder.
            rebuild_entries(app);
            let config = app.config.clone();
            let persist = app.persist.clone();
            let save = Task::perform(persist_config(config, persist), |success| {
                Message::Persisted {
                    what: "settings change",
                    success,
                }
            });
            let venvs = probe_every_venv(app);
            if app.selected == Some(i) {
                return Task::batch([
                    save,
                    venvs,
                    update(app, Message::ProjectSelected(i), runner),
                ]);
            }
            return Task::batch([save, venvs]);
        }
        Message::Persisted { what, success } => {
            if !success {
                drawer::set_idle(&mut app.drawer, format!("{what} failed to persist"));
            }
        }
        // Opens the confirmation and nothing else. The menu closes either
        // way, like every other action taken from it.
        Message::RecreateVenvRequested(i) => {
            app.context_menu = None;
            if let Some(id) = app.config.projects.get(i).map(|p| p.id)
                && running(app, id)
            {
                app.confirm_stop = Some((id, Box::new(Message::RecreateVenvRequested(i))));
                return Task::none();
            }
            if let Some(project) = app.config.projects.get(i)
                && let Some(environment) = project.active_environment()
                && let Ok(venv_path) =
                    venv_path::resolve(project, environment, &app.config.settings)
            {
                app.confirm_recreate = Some(context_menu::ConfirmRecreate {
                    project_id: project.id,
                    label: project.label.clone(),
                    venv_path,
                });
            }
        }
        Message::RecreateVenvCancelled => {
            app.confirm_recreate = None;
        }
        Message::PinChangeCancelled => {
            app.confirm_pin_change = None;
        }
        Message::PinChangeConfirmed => {
            if let Some(i) =
                confirmed_index(app, app.confirm_pin_change.as_ref().map(|c| c.project_id))
                && refuse_missing(app, i)
            {
                app.confirm_pin_change = None;
                return Task::none();
            }
            // `take` unconditionally: whether or not work is dispatched, the
            // user has answered and the dialog is done. Re-resolved by id,
            // not by an index, for the reason `ConfirmRecreate` documents.
            if let Some(confirm) = app.confirm_pin_change.take()
                && can_start(app)
                && let Some(project) = app
                    .config
                    .projects
                    .iter()
                    .find(|p| p.id == confirm.project_id)
                    .cloned()
            {
                let config = app.config.clone();
                app.busy = Some(crate::busy::Busy::new(
                    project.id,
                    crate::busy::Action::PinChange,
                ));
                drawer::start(&mut app.drawer, format!("syncing {}...", project.label));
                return Task::stream(sync_project(project, config, runner));
            }
        }
        Message::RecreateVenvConfirmed => {
            if let Some(i) =
                confirmed_index(app, app.confirm_recreate.as_ref().map(|c| c.project_id))
                && refuse_missing(app, i)
            {
                app.confirm_recreate = None;
                return Task::none();
            }
            // `take` unconditionally: whether or not the work is dispatched,
            // the user has answered the question and the dialog is done.
            // Re-resolved by id, not by the index the press carried: the
            // list can have shifted underneath the open confirmation (see
            // `ConfirmRecreate`), and a project that no longer exists
            // resolves to nothing rather than to whoever took its place.
            if let Some(confirm) = app.confirm_recreate.take()
                && can_start(app)
                && let Some(project) = app
                    .config
                    .projects
                    .iter()
                    .find(|p| p.id == confirm.project_id)
                    .cloned()
            {
                let config = app.config.clone();
                app.busy = Some(crate::busy::Busy::new(
                    project.id,
                    crate::busy::Action::Recreate,
                ));
                drawer::start(&mut app.drawer, format!("recreating {}...", project.label));
                // Reuses `SyncLine`/`SyncFinished`: a recreate ends in the
                // same `uv sync` with the same single-slot release, the same
                // transcript and the same expand-on-failure. What it does NOT
                // reuse is the status text's meaning -- `run_recreate_venv`
                // appends a line saying the environment was deleted and not
                // rebuilt, which is the part "sync failed" cannot express on
                // its own.
                return Task::stream(recreate_venv(project, config, runner));
            }
        }
        Message::OpenFolderRequested(i) => {
            app.context_menu = None;
            if let Some(project) = app.config.projects.get(i).cloned() {
                return Task::perform(
                    open_folder(Os::host(), project, runner),
                    Message::OpenFolderFinished,
                );
            }
        }
        Message::OpenFolderFinished(Ok(())) => {}
        Message::OpenFolderFinished(Err(message)) => {
            drawer::push_line(&mut app.drawer, &message, &[]);
            drawer::set_idle(&mut app.drawer, "open folder failed".to_string());
        }
        Message::MenuToggled(menu) => {
            app.open_menu = if app.open_menu == Some(menu) {
                None
            } else {
                Some(menu)
            };
        }
        Message::MenuDismissed => {
            app.open_menu = None;
        }
        Message::ModalOpened(modal) => {
            app.modal = Some(modal);
            app.open_menu = None;
        }
        Message::ModalClosed => {
            app.modal = None;
        }
        Message::PreferencesSectionOpened(section) => {
            // Straight to the section the user named, always in Global scope.
            // The Settings menu exists so the settings are *visible* without
            // opening anything first -- landing on a different section than
            // the one pressed would undo that.
            app.preferences = Some(preferences::State {
                scope: preferences::Scope::Global,
                section,
            });
            // The menu closes behind what it opened, the same as
            // `ModalOpened`: two layers claiming the pointer at once is how a
            // press both dismisses and activates.
            app.open_menu = None;
            if section == preferences::Section::Pycharm {
                return detect_pycharm(runner);
            }
            if section == preferences::Section::Disk {
                return measure_disk(app, runner, false);
            }
            if section == preferences::Section::GitHub && app.github_source.is_none() {
                return check_github(app.config.settings.github_account.clone());
            }
        }
        Message::PreferencesClosed => {
            // No undo: every control in there has already written through to
            // `Config` and persisted, the same as the tabs it replaces --
            // except a value typed and not saved, which is kept as a draft and
            // said out loud rather than silently left pending.
            app.preferences = None;
            let unsaved = env_editor::unsaved_count(&app.config, &app.env_value_drafts);
            if unsaved > 0 {
                crate::busy::push(
                    &mut app.toasts,
                    crate::busy::ToastKind::Warning,
                    format!(
                        "{unsaved} environment variable value{} not saved",
                        if unsaved == 1 { " is" } else { "s are" }
                    ),
                    std::time::Instant::now(),
                );
            }
        }
        Message::PreferencesScopeSelected(scope) => {
            if let Some(state) = &mut app.preferences {
                // The section is deliberately untouched. The two axes are
                // independent, and comparing one project against the global
                // value is the main thing this screen is for -- resetting to
                // the top of the list on every scope change would undo that.
                state.scope = scope;
            }
        }
        Message::PreferencesSectionSelected(section) => {
            if let Some(state) = &mut app.preferences {
                state.section = section;
            }
            // Looked up afresh each time the panel shows: an install or an
            // upgrade since the last look should show up.
            if section == preferences::Section::Pycharm {
                return detect_pycharm(runner);
            }
            if section == preferences::Section::Disk {
                return measure_disk(app, runner, false);
            }
            if section == preferences::Section::GitHub && app.github_source.is_none() {
                return check_github(app.config.settings.github_account.clone());
            }
        }
        Message::PycharmDetected(found) => app.pycharm_detected = Some(found),
        Message::PycharmBrowse => {
            return Task::perform(pick_file(), Message::PycharmPicked);
        }
        Message::PycharmPicked(None) => {}
        Message::PycharmPicked(Some(path)) => {
            app.config.settings.pycharm = Some(path);
            return persist_edit(app, "settings change");
        }
        Message::PycharmAutomatic => {
            app.config.settings.pycharm = None;
            return persist_edit(app, "settings change");
        }
        Message::PreferencesPythonPinChanged(scope, version) => {
            let pin = preferences::pin_from_input(&version);
            // The RAW scope, never `resolve_scope`. That function answers
            // `Global` for a project that no longer exists, which is right for
            // deciding what to *render* and catastrophic for deciding where to
            // *write*: an edit meant for a removed project would silently
            // change the global setting for every project. Landing nowhere is
            // the only safe answer.
            match scope {
                preferences::Scope::Global => app.config.settings.python = pin,
                preferences::Scope::Project(id) => {
                    // Onto the active environment, which is where the pin
                    // lives now. A project no longer has one of its own.
                    let Some(project) = app.config.projects.iter_mut().find(|p| p.id == id) else {
                        return Task::none();
                    };
                    let active = project.active.clone();
                    let index = project
                        .environments
                        .iter()
                        .position(|environment| environment.location == active)
                        .or(if project.environments.is_empty() {
                            None
                        } else {
                            Some(0)
                        });
                    let Some(index) = index else {
                        return Task::none();
                    };
                    project.environments[index].python = pin;
                }
            }
            return persist_edit(app, "env var change");
        }
        Message::PreferencesUvSourceOverridden(scope, source) => {
            // The raw scope -- see `PreferencesPythonPinChanged`.
            match scope {
                preferences::Scope::Global => app.config.settings.uv_source = source,
                preferences::Scope::Project(id) => {
                    let Some(project) = app.config.projects.iter_mut().find(|p| p.id == id) else {
                        return Task::none();
                    };
                    project.uv_source = Override::Set(source);
                }
            }
            return persist_edit(app, "env var change");
        }
        Message::PreferencesTerminalOverridden(scope, choice) => {
            let os = Os::host();
            // The raw scope -- see `PreferencesPythonPinChanged`.
            match scope {
                preferences::Scope::Global => {
                    app.config.terminal_defaults.insert(os, choice);
                }
                preferences::Scope::Project(id) => {
                    let Some(project) = app.config.projects.iter_mut().find(|p| p.id == id) else {
                        return Task::none();
                    };
                    project.terminals.insert(os, choice);
                }
            }
            return persist_edit(app, "env var change");
        }
        Message::EnvironmentAdded(id) => {
            return Task::perform(pick_folder(), move |path| {
                Message::EnvironmentAddFolderPicked(id, path)
            });
        }
        // Cancelled -- nothing is added.
        Message::EnvironmentAddFolderPicked(_, None) => {}
        Message::EnvironmentAddFolderPicked(id, Some(path)) => {
            let seed = app.config.settings.python.clone();
            let Some(project) = app.config.projects.iter_mut().find(|p| p.id == id) else {
                return Task::none();
            };
            let location = VenvLocation::Custom { path };
            // Two environments cannot share a directory -- that is what
            // identifies them -- so choosing one this project already has is
            // a no-op rather than a duplicate row.
            if project
                .environments
                .iter()
                .any(|environment| environment.location == location)
            {
                return Task::none();
            }
            project
                .environments
                .push(bombadil_core::model::Environment {
                    location,
                    // `Settings::python` is the pin a new environment starts with,
                    // which is the meaning it has now that the pin lives here. A
                    // folder that already holds an environment reports its real
                    // version once the probe lands, whatever this says.
                    python: seed,
                });
            rebuild_entries(app);
            return Task::batch([persist_edit(app, "env var change"), probe_every_venv(app)]);
        }
        Message::EnvironmentPinChanged(id, location, version) => {
            let pin = preferences::pin_from_input(&version);
            let Some(project) = app.config.projects.iter_mut().find(|p| p.id == id) else {
                return Task::none();
            };
            let Some(environment) = project
                .environments
                .iter_mut()
                .find(|environment| environment.location == location)
            else {
                return Task::none();
            };
            environment.python = pin;
            // The row shows its own pin, and drift is judged against it.
            rebuild_entries(app);
            return persist_edit(app, "env var change");
        }
        Message::EnvironmentRemoveRequested(id, location) => {
            let Some(project) = app.config.projects.iter().find(|p| p.id == id) else {
                return Task::none();
            };
            let Some(environment) = project
                .environments
                .iter()
                .find(|environment| environment.location == location)
            else {
                return Task::none();
            };
            let Ok(venv_path) = venv_path::resolve(project, environment, &app.config.settings)
            else {
                return Task::none();
            };
            app.confirm_environment_removal = Some(context_menu::ConfirmEnvironmentRemoval {
                project_id: id,
                label: project.label.clone(),
                location,
                venv_path,
            });
        }
        Message::EnvironmentRemovalCancelled => {
            app.confirm_environment_removal = None;
        }
        Message::EnvironmentRemovalConfirmed => {
            // `take` unconditionally: the user has answered either way.
            let Some(confirm) = app.confirm_environment_removal.take() else {
                return Task::none();
            };
            let Some(project) = app
                .config
                .projects
                .iter_mut()
                .find(|p| p.id == confirm.project_id)
            else {
                return Task::none();
            };
            project
                .environments
                .retain(|environment| environment.location != confirm.location);
            // If the removed one was active, `active` now names nothing.
            // `active_environment` already falls back to the first, but
            // leaving it dangling would make the sidebar's active marker
            // disagree with what Sync would build.
            if project.active == confirm.location {
                project.active = project
                    .environments
                    .first()
                    .map(|environment| environment.location.clone())
                    .unwrap_or_default();
            }
            rebuild_entries(app);
            return Task::batch([persist_edit(app, "env var change"), probe_every_venv(app)]);
        }
        Message::EnvironmentLocationSelected(id, from, to) => {
            let Some(project) = app.config.projects.iter_mut().find(|p| p.id == id) else {
                return Task::none();
            };
            // Two environments cannot share a directory -- that is what
            // identifies them -- so a move onto one that is taken is refused
            // rather than silently merging the two.
            if from != to
                && project
                    .environments
                    .iter()
                    .any(|environment| environment.location == to)
            {
                return Task::none();
            }
            let Some(environment) = project
                .environments
                .iter_mut()
                .find(|environment| environment.location == from)
            else {
                return Task::none();
            };
            environment.location = to.clone();
            // `active` names a location, so moving the active environment has
            // to move it too or it would name one that no longer exists.
            if project.active == from {
                project.active = to;
            }
            // Re-probed: the sidebar's glyph describes wherever an
            // environment last resolved, and this moved that.
            rebuild_entries(app);
            return Task::batch([persist_edit(app, "env var change"), probe_every_venv(app)]);
        }
        Message::EnvironmentPickFolderRequested(id, from) => {
            return Task::perform(pick_folder(), move |path| {
                Message::EnvironmentFolderPicked(id, from.clone(), path)
            });
        }
        // Cancelled -- leave the environment where it is.
        Message::EnvironmentFolderPicked(_, _, None) => {}
        Message::EnvironmentFolderPicked(id, from, Some(path)) => {
            return Task::done(Message::EnvironmentLocationSelected(
                id,
                from,
                VenvLocation::Custom { path },
            ));
        }
        Message::PreferencesInheritToggled(scope, section) => {
            // Only a project can inherit. Global has nothing above it, and a
            // control that did something there would be lying about what the
            // scope means.
            // The raw scope -- see `PreferencesPythonPinChanged`. Here it
            // matters twice: `resolve_scope` would turn a removed project into
            // `Global`, and Global has nothing to inherit, so this would
            // silently do nothing instead of visibly doing nothing.
            let preferences::Scope::Project(id) = scope else {
                return Task::none();
            };
            let Some(project) = app.config.projects.iter_mut().find(|p| p.id == id) else {
                return Task::none();
            };
            match section {
                // The pin is per environment now, and an environment either
                // has one or does not -- there is nothing above it to inherit
                // from. Task 5 replaces this control for the Python section.
                preferences::Section::Python => return Task::none(),
                preferences::Section::Uv => project.uv_source = Override::Inherit,
                preferences::Section::Terminal => {
                    project.terminals.remove(&Os::host());
                }
                // The list sections merge rather than override, so there is
                // nothing here to hand back. See `preferences`'s module doc.
                preferences::Section::Appearance
                | preferences::Section::Pycharm
                | preferences::Section::Environments
                | preferences::Section::Scripts
                | preferences::Section::EnvVars
                | preferences::Section::Indexes
                | preferences::Section::GitHub
                | preferences::Section::Backup
                | preferences::Section::Disk => return Task::none(),
            }
            return persist_edit(app, "env var change");
        }
        Message::EnvVarAdded(scope) => {
            if let Some(vars) = env_editor::vars_mut(&mut app.config, scope) {
                env_editor::add_var(vars);
            }
            return persist_edit(app, "env var change");
        }
        Message::EnvVarRemoved(scope, index) => {
            // Resolved before the row goes -- see `env_removal_secret_key`.
            // `unmark_secret` deletes the keychain entry; removing the row
            // entirely used not to, so the value stayed in the keychain
            // unreachable while the row that named it was gone.
            let owed = env_removal_secret_key(app, scope, index);
            if let Some(vars) = env_editor::vars_mut(&mut app.config, scope) {
                env_editor::remove_var(vars, index);
            }
            // Every later row in this scope has just shifted down one; its
            // draft has to shift with it or it commits into its neighbour.
            env_editor::drop_draft_row(&mut app.env_value_drafts, scope, index);
            if let Some(key) = owed {
                let config = app.config.clone();
                let persist = app.persist.clone();
                return Task::perform(unmark_secret_job(key, config, persist), |success| {
                    Message::Persisted {
                        what: "env var change",
                        success,
                    }
                });
            }
            return persist_edit(app, "env var change");
        }
        Message::EnvVarKeyChanged(scope, index, key) => {
            if let Some(vars) = env_editor::vars_mut(&mut app.config, scope) {
                env_editor::set_key(vars, index, key);
            }
            return persist_edit(app, "env var change");
        }
        // Draft only: no `Config` write and no persist, so a value the user is
        // still deciding whether to mark secret never transits `config.toml`.
        // Sound only because `env_editor::rows` never produces this message
        // for a row that is already secret -- see `env_editor`'s module doc.
        Message::EnvVarValueChanged(scope, index, value) => {
            app.env_value_drafts.insert((scope, index), value);
        }
        // The one path a plaintext value reaches disk by. Nothing to persist
        // when the row had no draft (a stray Enter), which is what keeps this
        // from being a save per press.
        Message::EnvVarValueCommitted(scope, index) => {
            let committed = match env_editor::vars_mut(&mut app.config, scope) {
                Some(vars) => {
                    env_editor::commit_value(vars, &mut app.env_value_drafts, scope, index)
                }
                None => false,
            };
            if committed {
                return persist_edit(app, "env var change");
            }
        }
        Message::EnvVarSecretToggled(scope, index) => {
            // Resolved before the mutable borrow of `app.config` below, and
            // before this row's own current state (secret or not) is known
            // -- a `Project` scope's `SecretKey` needs the project's *id*,
            // not its index, since a removal shifts every later index while
            // the id is stable.
            let project_id = match scope {
                env_editor::EnvScope::Global => None,
                env_editor::EnvScope::Project(i) => match app.config.projects.get(i) {
                    Some(project) => Some(project.id),
                    // A stale index for a project that no longer exists.
                    None => return Task::none(),
                },
            };
            let Some(key_name) = env_editor::vars(&app.config, scope)
                .and_then(|vars| vars.get(index))
                .map(|var| var.key.clone())
            else {
                return Task::none();
            };
            let Some(secret_key) = env_editor::secret_key(scope, project_id, &key_name) else {
                return Task::none();
            };
            let persist = app.persist.clone();

            // `app.config` is deliberately not touched here -- see
            // `plan_secret_toggle` for why this one edit cannot follow the
            // "in-memory first" discipline the rest of this file uses.
            match plan_secret_toggle(&app.config, &app.env_value_drafts, scope, index) {
                Some(SecretToggle::Mark {
                    candidate,
                    plain_value,
                }) => {
                    return Task::perform(
                        mark_secret_job(secret_key, plain_value, candidate, persist),
                        move |success| Message::EnvVarSecretWritten {
                            scope,
                            index,
                            success,
                        },
                    );
                }
                Some(SecretToggle::Unmark { candidate }) => {
                    return Task::perform(
                        unmark_secret_job(secret_key, candidate, persist),
                        move |success| Message::EnvVarSecretWritten {
                            scope,
                            index,
                            success,
                        },
                    );
                }
                None => {}
            }
        }
        // Applies to `app.config` what the job already wrote to the keychain
        // and to disk, and only then. Re-deriving the direction from the row's
        // *current* state is what keeps this arm honest: `app.config` has not
        // been touched since the toggle, so a row that still reads
        // `secret == false` was being marked and one that reads `true` was
        // being unmarked. A `secret: bool` passed through the message would
        // say the same thing while being free to disagree with the config it
        // is about to edit.
        Message::EnvVarSecretWritten {
            scope,
            index,
            success,
        } => {
            if !success {
                // Nothing was blanked, nothing was persisted, and the draft
                // the user typed is still in the textbox. Retrying the toggle
                // is all this costs them.
                drawer::set_idle(
                    &mut app.drawer,
                    "env var change failed to persist".to_string(),
                );
            } else if let Some(vars) = env_editor::vars_mut(&mut app.config, scope)
                && env_editor::take_secret_value(vars, &mut app.env_value_drafts, scope, index)
                    .is_none()
            {
                env_editor::unmark_secret(vars, index);
            }
        }
        Message::ScriptAddRequested(scope) => {
            return Task::perform(pick_file(), move |path| {
                Message::ScriptFilePicked(scope, path)
            });
        }
        // Cancelled -- nothing chosen, nothing to add.
        Message::ScriptFilePicked(_, None) => {}
        Message::ScriptFilePicked(scope, Some(path)) => {
            if let Some(scripts) = scripts_editor::scripts_mut(&mut app.config, scope) {
                scripts_editor::add_script(scripts, path);
            }
            return persist_edit(app, "script change");
        }
        Message::ScriptRemoved(scope, index) => {
            if let Some(scripts) = scripts_editor::scripts_mut(&mut app.config, scope) {
                scripts_editor::remove_script(scripts, index);
            }
            return persist_edit(app, "script change");
        }
        Message::ScriptEnabledToggled(scope, index) => {
            if let Some(scripts) = scripts_editor::scripts_mut(&mut app.config, scope) {
                scripts_editor::toggle_enabled(scripts, index);
            }
            return persist_edit(app, "script change");
        }
        Message::ScriptMovedUp(scope, index) => {
            if let Some(scripts) = scripts_editor::scripts_mut(&mut app.config, scope) {
                scripts_editor::move_up(scripts, index);
            }
            return persist_edit(app, "script change");
        }
        Message::ScriptMovedDown(scope, index) => {
            if let Some(scripts) = scripts_editor::scripts_mut(&mut app.config, scope) {
                scripts_editor::move_down(scripts, index);
            }
            return persist_edit(app, "script change");
        }
        Message::IndexAdded => {
            index_editor::add_index(&mut app.config.indexes);
            app.index_credential_drafts.push(String::new());
            app.index_credential_names.push(String::new());
            return persist_edit(app, "index change");
        }
        Message::IndexRemoved(i) => {
            // Resolved before the row goes: `SecretKey::Index` is keyed on
            // the index's name, and afterwards there is nothing left to find
            // the entry by -- it would sit in the user's keychain forever.
            let owed = index_credential_key(app, i);
            index_editor::remove_index(&mut app.config.indexes, i);
            // Kept in step with `config.indexes` -- see
            // `App::index_credential_drafts`'s own doc.
            if i < app.index_credential_drafts.len() {
                app.index_credential_drafts.remove(i);
            }
            if i < app.index_credential_names.len() {
                app.index_credential_names.remove(i);
            }
            if let Some(key) = owed {
                let config = app.config.clone();
                let persist = app.persist.clone();
                return Task::perform(unmark_secret_job(key, config, persist), |success| {
                    Message::Persisted {
                        what: "index change",
                        success,
                    }
                });
            }
            return persist_edit(app, "index change");
        }
        // The three typed fields write in-memory only: no persist, and -- for
        // the name -- no keychain call. `IndexEditCommitted` is where both
        // happen, once per edit instead of once per character.
        Message::IndexNameChanged(i, name) => {
            index_editor::set_name(&mut app.config.indexes, i, name);
        }
        Message::IndexUrlChanged(i, url) => {
            index_editor::set_url(&mut app.config.indexes, i, url);
        }
        Message::IndexUsernameChanged(i, username) => {
            index_editor::set_username(&mut app.config.indexes, i, username);
        }
        Message::IndexEditCommitted(i) => return commit_index_edit(app, i),
        // A discrete press rather than typing, so it commits the row as it
        // stands -- which is also what keeps a rename typed just before it
        // from being persisted while the credential stays under the old name.
        Message::IndexKindChanged(i, kind) => {
            index_editor::set_kind(&mut app.config.indexes, i, kind);
            return commit_index_edit(app, i);
        }
        Message::IndexDefaultToggled(i) => {
            index_editor::toggle_default_for_new_projects(&mut app.config.indexes, i);
            return commit_index_edit(app, i);
        }
        Message::IndexAuthChoiceChanged(i, choice) => {
            index_editor::set_auth_choice(&mut app.config.indexes, i, choice);
            return commit_index_edit(app, i);
        }
        // Never persisted, and no job dispatched: this is App-local scratch
        // state, never `Config` -- see `App::index_credential_drafts`'s doc.
        Message::IndexCredentialChanged(i, input) => {
            if let Some(slot) = app.index_credential_drafts.get_mut(i) {
                *slot = input.0;
            }
        }
        // Criterion 1 and criterion 2 together: writes to the keychain under
        // `SecretKey::Index`, never `Config` (`mark_secret_job` -> `run_secret_write`
        // -> `store.set`, then `persist.save(config)`, and `config` never
        // gained a credential field to carry one in), and only for a name
        // that does not currently collide with another's credential
        // variable -- `view` already disables the button that sends this for
        // a colliding row, but the guard is repeated here so nothing else
        // that might someday construct this message can bypass it.
        Message::IndexCredentialSaveRequested(i) => {
            if let Some((key, plain_value)) =
                index_editor::credential_save(&app.config, &app.index_credential_drafts, i)
            {
                // Neither the draft nor the record of which account holds the
                // credential is touched here -- both wait for the write to
                // land, in `IndexCredentialWritten`. Doing them now costs
                // twice on a keychain that refuses: the typed credential is
                // gone from the box that was the only place it existed, and
                // `index_credential_names` claims an account the keychain has
                // never heard of, so the next rename commit moves *from* an
                // entry that is not there and the real one is left behind
                // under its old name.
                let config = app.config.clone();
                let persist = app.persist.clone();
                return Task::perform(
                    mark_secret_job(key, plain_value, config, persist),
                    move |success| Message::IndexCredentialWritten { index: i, success },
                );
            }
        }
        Message::IndexCredentialWritten { index: i, success } => {
            if !success {
                drawer::set_idle(
                    &mut app.drawer,
                    "index change failed to persist".to_string(),
                );
            } else {
                // Criterion 4's other angle: the plaintext must not keep
                // sitting in `App` state, readable by a later render, any
                // longer than it has to. It is in the keychain now, so now is
                // when it goes.
                if let Some(slot) = app.index_credential_drafts.get_mut(i) {
                    *slot = String::new();
                }
                // `credential_save` resolved the account from the row's
                // *current* name and the job persisted that same config, so
                // this is where the credential lives -- recorded, or a later
                // commit would try to rename an entry that has already moved
                // and overwrite it with the old one.
                if let (Some(index), Some(slot)) = (
                    app.config.indexes.get(i),
                    app.index_credential_names.get_mut(i),
                ) {
                    *slot = index.name.clone();
                }
            }
        }
        Message::ProjectIndexToggled(project_index, name) => {
            if let Some(project) = app.config.projects.get_mut(project_index) {
                index_editor::toggle_selected(&mut project.index_names, &name);
            }
            return persist_edit(app, "index change");
        }
        Message::MemberToggled(i, path) => {
            // Only reachable when this project's Members fetch is the one
            // cached -- without it there is no `discovered`/`kind` to read
            // the current effective state from, and `members_editor::toggle`
            // needs that to decide what a flip even means.
            if let Some((mi, fetch)) = &app.members
                && *mi == i
                && let Some(project) = app.config.projects.get_mut(i)
            {
                members_editor::toggle(&mut project.members, &fetch.discovered, &fetch.kind, &path);
                return persist_edit(app, "member selection");
            }
        }
        Message::MembersSyncRequested(i) => {
            if let Some((mi, fetch)) = &app.members
                && *mi == i
                && let Some(project) = app.config.projects.get(i).cloned()
            {
                let project_dir = project
                    .pyproject_path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                let tab = members_editor::build(
                    &project_dir,
                    &project.members,
                    &fetch.kind,
                    &fetch.discovered,
                    &fetch.installed,
                );
                if let Some(warning) = members_editor::removal_warning(&tab.rows) {
                    app.confirm_member_sync = Some(members_editor::ConfirmMemberSync {
                        project_index: i,
                        label: project.label.clone(),
                        warning,
                    });
                } else if can_start(app) {
                    let kind = fetch.kind.clone();
                    let discovered = fetch.discovered.clone();
                    return dispatch_members_sync(app, project, kind, discovered, runner);
                }
            }
        }
        Message::MembersSyncCancelled => {
            app.confirm_member_sync = None;
        }
        Message::MembersSyncConfirmed => {
            // `take` unconditionally -- the same discipline
            // `RecreateVenvConfirmed` follows: whether or not the work is
            // dispatched, the user has answered the question and the dialog
            // is done.
            if let Some(confirm) = app.confirm_member_sync.take()
                && can_start(app)
                && let Some((mi, fetch)) = &app.members
                && *mi == confirm.project_index
                && let Some(project) = app.config.projects.get(confirm.project_index).cloned()
            {
                let kind = fetch.kind.clone();
                let discovered = fetch.discovered.clone();
                return dispatch_members_sync(app, project, kind, discovered, runner);
            }
        }
        // Criterion 1: applies immediately and persists -- the write every
        // later `resolve_uv_for(&app.config, ..)` call reads fresh.
        Message::SettingsUvSourceChoiceChanged(choice) => {
            settings_editor::apply_choice(&mut app.config.settings, choice);
            // A stale result from a previous Custom validation no longer
            // describes the current source.
            app.uv_custom_validation = None;
            return persist_edit(app, "settings change");
        }
        Message::SettingsUvCustomPathChanged(text) => {
            app.uv_custom_path_draft = text;
        }
        Message::SettingsUvCustomValidateRequested => {
            let path = PathBuf::from(app.uv_custom_path_draft.trim());
            let config = app.config.clone();
            return Task::perform(
                validate_uv_source(path, config, runner),
                Message::SettingsUvCustomValidated,
            );
        }
        // Criterion 2: only a passing validation ever writes `Settings` --
        // this is the only arm that does.
        Message::SettingsUvCustomValidated(Ok(resolved)) => {
            app.config.settings.uv_source = UvSource::Custom {
                path: resolved.path.clone(),
            };
            app.uv_custom_validation = Some(Ok(resolved.version.to_string()));
            return persist_edit(app, "settings change");
        }
        // A refused path -- including one below `MIN_UV_VERSION`, reported
        // through `UvResolveError::TooOld`'s own text -- only updates what
        // the tab shows. `Settings::uv_source` is untouched, so the bad path
        // never reaches `config.toml`.
        Message::SettingsUvCustomValidated(Err(message)) => {
            app.uv_custom_validation = Some(Err(message));
        }
        Message::SettingsDefaultVenvLocationAlongsideSelected => {
            settings_editor::apply_default_venv_location(
                &mut app.config.settings,
                DefaultVenvLocation::Alongside,
            );
            // Re-probed, not just persisted: the sidebar's venv status
            // describes wherever each project resolved when it was last
            // probed, and this just moved that. See `probe_every_venv`.
            return Task::batch([persist_edit(app, "settings change"), probe_every_venv(app)]);
        }
        Message::SettingsDefaultVenvLocationPickFolderRequested => {
            return Task::perform(
                pick_folder(),
                Message::SettingsDefaultVenvLocationFolderPicked,
            );
        }
        // Cancelled -- leave the previous location untouched.
        Message::SettingsDefaultVenvLocationFolderPicked(None) => {}
        Message::SettingsDefaultVenvLocationFolderPicked(Some(path)) => {
            settings_editor::apply_default_venv_location(
                &mut app.config.settings,
                DefaultVenvLocation::Central { path },
            );
            return Task::batch([persist_edit(app, "settings change"), probe_every_venv(app)]);
        }
    }
    Task::none()
}

/// Rebuilds `app.entries` from `app.config`, keeping what has already been
/// probed.
///
/// `entries` is what the sidebar renders; `config` is what is persisted.
/// Anything that changes a project's environments has to update both, and
/// writing only the second is a bug this project has already been bitten by
/// once -- `App::projects` was a stale clone, the file on disk stayed
/// correct, and restarting "fixed" it, which is precisely why manual testing
/// missed it. It came back here as "the add environment button is not
/// working": the environment was added and persisted, and the tree went on
/// showing the old list.
///
/// Probed versions are carried across by location rather than dropped. A
/// rebuild that dropped them would blank every row to "not created" on each
/// add, until the probes landed again.
fn rebuild_entries(app: &mut App) {
    /// What a rebuild has to carry across: a project's `requires-python` and
    /// each environment's probed version, keyed by the location that
    /// identifies it.
    type Probed = (
        Option<String>,
        Vec<(bombadil_core::model::VenvLocation, Option<String>)>,
        (uuid::Uuid, Option<bombadil_core::git::GitStatus>),
    );

    let probed: Vec<Probed> = app
        .entries
        .iter()
        .map(|entry| {
            (
                entry.requires_python.clone(),
                entry
                    .environments
                    .iter()
                    .map(|row| (row.location.clone(), row.python_version.clone()))
                    .collect(),
                (entry.id, entry.git.clone()),
            )
        })
        .collect();

    app.entries = sidebar::entries(&app.config);

    for (entry, (requires_python, rows, (id, git))) in app.entries.iter_mut().zip(probed) {
        entry.requires_python = requires_python;
        if entry.id == id {
            entry.git = git;
        }
        for row in &mut entry.environments {
            if let Some((_, version)) = rows.iter().find(|(location, _)| *location == row.location)
            {
                row.python_version = version.clone();
            }
        }
    }
}

/// Re-read everything a finished sync has just invalidated.
///
/// The Dependencies and Members tabs are built from `uv pip list` output
/// captured when they were last fetched, and the sidebar's glyph from a
/// `pyvenv.cfg` read at the same time. A sync changes all three, so all three
/// are re-read -- there is no cheaper subset, because a sync that installs a
/// package, removes another and creates the environment does all of it at
/// once.
///
/// Only for the selected project's tabs, since those are the only ones
/// rendered; the venv probe covers every project, because a sync can move an
/// environment into existence and the sidebar shows them all.
fn refresh_after_sync(app: &App, runner: Arc<dyn CommandRunner>) -> Task<Message> {
    let mut tasks = vec![probe_every_venv(app)];
    if let Some(i) = app.selected
        && let Some(project) = app.config.projects.get(i)
    {
        tasks.push(fetch_deps_task(
            i,
            project.clone(),
            app.config.clone(),
            runner.clone(),
        ));
        tasks.push(fetch_members_task(
            i,
            project.clone(),
            app.config.clone(),
            runner,
        ));
    }
    Task::batch(tasks)
}

/// Finished commands, bottom-right, newest at the bottom edge's top. Not
/// `opaque` as a whole: the empty area around the toasts must let presses
/// through to the application underneath.
fn toast_layer<'a>(toasts: &[crate::busy::Toast]) -> iced::Element<'a, Message> {
    use crate::busy::ToastKind;
    use crate::icons::{self, Icon};
    let mut list = iced::widget::column![]
        .spacing(theme::SPACE_2)
        .align_x(iced::Alignment::End);
    for (i, toast) in toasts.iter().enumerate() {
        let (icon, swatch) = match toast.kind {
            ToastKind::Success => (Icon::Check, theme::Swatch::Moss),
            ToastKind::Failure | ToastKind::Warning => (Icon::Alert, theme::Swatch::Boot),
        };
        let mut row = iced::widget::row![
            icons::icon(icon, 16.0, swatch),
            iced::widget::text(toast.text.clone()).size(theme::BODY),
        ]
        .spacing(theme::SPACE_2)
        .align_y(iced::Alignment::Center);
        if let Some((project, fix)) = &toast.fix {
            row = row.push(
                iced::widget::button(iced::widget::text(fix_label(fix)).size(theme::LABEL))
                    .on_press(Message::DoctorFixRequested(*project, fix.clone()))
                    .padding([theme::SPACE_1, theme::SPACE_2])
                    .style(theme::button_primary),
            );
        }
        if toast.kind == ToastKind::Failure {
            row = row.push(
                iced::widget::button(iced::widget::text("Show output").size(theme::LABEL))
                    .on_press(Message::ShowOutput)
                    .padding([theme::SPACE_1, theme::SPACE_2])
                    .style(theme::button_secondary),
            );
        }
        row = row.push(icons::icon_button(
            Icon::X,
            "Dismiss",
            Some(Message::ToastDismissed(i)),
        ));
        list = list.push(iced::widget::opaque(
            iced::widget::container(row)
                .padding([theme::SPACE_2, theme::SPACE_3])
                .style(theme::panel),
        ));
    }
    iced::widget::container(list)
        .width(iced::Length::Fill)
        .height(iced::Length::Fill)
        .align_x(iced::Alignment::End)
        .align_y(iced::Alignment::End)
        // Clear of the drawer's status line along the bottom.
        .padding([theme::SPACE_4 * 2.0, theme::SPACE_3])
        .into()
}

/// Re-reads what can change outside Bombadil for `projects[i]`: its git
/// status, its dependencies and its members.
fn reprobe_project(app: &App, i: usize, runner: Arc<dyn CommandRunner>) -> Task<Message> {
    let Some(project) = app.config.projects.get(i) else {
        return Task::none();
    };
    let id = project.id;
    let git = match project.pyproject_path.parent() {
        Some(dir) => Task::perform(probe_git(dir.to_path_buf()), move |status| {
            Message::GitProbed { id, status }
        }),
        None => Task::none(),
    };
    Task::batch([
        git,
        fetch_deps_task(i, project.clone(), app.config.clone(), runner.clone()),
        fetch_members_task(i, project.clone(), app.config.clone(), runner),
    ])
}

/// Whether coming back to the window should re-read the selected project:
/// idle, something selected, and not within [`crate::busy::FOCUS_DEBOUNCE`]
/// of the last time -- alt-tabbing must not stack up `git` and `uv` calls.
pub fn should_refresh_on_focus(app: &App, now: std::time::Instant) -> bool {
    can_start(app)
        && app.selected.is_some()
        && app
            .last_focus_refresh
            .is_none_or(|at| now.saturating_duration_since(at) >= crate::busy::FOCUS_DEBOUNCE)
}

/// Whether anything on screen moves: the spinner or a success toast's
/// countdown. A failure toast waits for the user and needs no frames. The
/// frame subscription exists only while this is true.
/// Whether any project has a program running: its elapsed time is shown,
/// so the clock has to keep moving -- once a second, not every frame.
pub fn runs_live(app: &App) -> bool {
    app.runs.values().any(run_tab::Run::live)
}

pub fn animating(app: &App) -> bool {
    app.busy.is_some() || app.toasts.iter().any(|t| t.expires.is_some())
}

/// The sidebar's width for a pointer at `x`: wide enough for a label, never
/// so wide the detail pane is squeezed out.
pub fn clamp_sidebar(x: f32) -> u32 {
    x.clamp(180.0, 480.0).round() as u32
}

/// Copies the window's size into the layout and saves it, if it changed.
///
/// ponytail: a blocking write on the update thread, once, at quit -- a
/// background task would race the window closing. A few hundred bytes of
/// TOML; make it async if quitting ever visibly stalls.
fn remember_window_size(app: &mut App) {
    let Some(size) = app.window_size else {
        return;
    };
    let size = Some((size.width.round() as u32, size.height.round() as u32));
    if app.config.settings.layout.window != size {
        app.config.settings.layout.window = size;
        run_persist(&app.config, app.persist.as_ref());
    }
}

/// Opens a terminal for `projects[i]` with one press -- the configured
/// terminal, or detection first when there is none. `environment` is a tray
/// press's choice; `None` is the active environment.
fn open_terminal_here(
    app: &mut App,
    i: usize,
    environment: Option<bombadil_core::model::VenvLocation>,
    runner: Arc<dyn CommandRunner>,
) -> Task<Message> {
    let default = app.config.terminal_defaults.get(&Os::host()).cloned();
    let Some(project) = project_for_open(app, i, environment.as_ref()) else {
        return Task::none();
    };
    let id = project.id;
    // `None` for `detected`: no probe has run. The decision itself
    // lives in `one_press_plan`, tested there -- this arm only carries
    // it out.
    match context_menu::one_press_plan(default.as_ref(), None) {
        context_menu::OpenPlan::Open(choice) => {
            let config = app.config.clone();
            Task::perform(
                open_terminal(choice, project, config, runner),
                Message::OpenTerminalFinished,
            )
        }
        context_menu::OpenPlan::Detect => {
            Task::perform(detect_terminals(Os::host(), runner), move |kinds| {
                Message::OpenTerminalHereDetected(id, kinds, environment.clone())
            })
        }
        // Unreachable before a probe has run -- `one_press_plan`
        // answers `Nothing` only for `Some([])` -- and matched rather
        // than `unreachable!()` so a future third caller cannot turn
        // a wrong plan into a panic in the user's window.
        context_menu::OpenPlan::Nothing => {
            report_no_terminal(app);
            Task::none()
        }
    }
}

/// `projects[i]`, pointed at `environment` when one is given; `None` when
/// either is gone.
fn project_for_open(
    app: &App,
    i: usize,
    environment: Option<&bombadil_core::model::VenvLocation>,
) -> Option<Project> {
    let project = app.config.projects.get(i)?;
    match environment {
        Some(location) => pointed_at(project, location),
        None => Some(project.clone()),
    }
}

/// A copy of `project` whose active environment is `location`, for one open
/// from the tray. The saved project, and so Sync's target, never changes.
/// `None` when the project has no such environment.
pub fn pointed_at(
    project: &Project,
    location: &bombadil_core::model::VenvLocation,
) -> Option<Project> {
    project
        .environments
        .iter()
        .any(|e| e.location == *location)
        .then(|| Project {
            active: location.clone(),
            ..project.clone()
        })
}

/// Starts `uv sync --upgrade[-package]` for `projects[i]`, if nothing else
/// runs.
/// `uv add <requirement>` into `group`, then the usual sync: the one path a
/// version edit and the doctor's fixes install through.
fn add_requirement(
    app: &mut App,
    i: usize,
    requirement: String,
    group: deps::GroupKind,
    runner: Arc<dyn CommandRunner>,
) -> Task<Message> {
    let Some(project) = app.config.projects.get(i).cloned() else {
        return Task::none();
    };
    if refuse_while_running(app, project.id) {
        return Task::none();
    }
    if !can_start(app) {
        return Task::none();
    }
    app.busy = Some(crate::busy::Busy::new(
        project.id,
        crate::busy::Action::InstallVersion(requirement.clone()),
    ));
    drawer::start(&mut app.drawer, format!("installing {requirement}..."));
    let group = deps::uv_group(&group);
    let (pin, plan) = (active_pin(&project), sync_plan_for(app, i));
    Task::stream(stream_in_project(
        project,
        app.config.clone(),
        move |uv, ctx| {
            let add = uv::command::add(uv, ctx, &requirement, group.clone());
            dep_commands(uv, ctx, &pin, &plan, &DepOp::Add(add))
        },
        (
            "installing the version timed out",
            "installing the version failed unexpectedly",
        ),
        runner,
    ))
}

/// The group `requirement`'s package is declared in on project `i`'s
/// Dependencies tab; `None` when it is not declared or the tab is not loaded.
fn declared_group(app: &App, i: usize, requirement: &str) -> Option<deps::GroupKind> {
    let key = deps::normalise(requirement_name(requirement));
    match &app.deps {
        Some((index, Ok(tab))) if *index == i => tab
            .groups
            .iter()
            .find(|g| {
                g.rows
                    .iter()
                    .any(|r| r.declared && deps::normalise(&r.name) == key)
            })
            .map(|g| g.kind.clone()),
        _ => None,
    }
}

/// The package a requirement such as `six==9.9` names.
fn requirement_name(requirement: &str) -> &str {
    requirement
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .next()
        .unwrap_or(requirement)
}

/// Rewrites project `i`'s pyproject.toml with `edit` off the UI thread;
/// `PyprojectEdited` then syncs, or says why nothing was written.
fn edit_pyproject(
    app: &App,
    i: usize,
    edit: impl FnOnce(&str) -> Result<String, String> + Send + 'static,
) -> Task<Message> {
    let Some((id, path)) = app
        .config
        .projects
        .get(i)
        .map(|p| (p.id, p.pyproject_path.clone()))
    else {
        return Task::none();
    };
    Task::perform(
        async move {
            job::run(
                move || -> Result<(), String> {
                    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                    let out = edit(&text)?;
                    bombadil_core::store::atomic::write_atomic(&path, &out)
                        .map_err(|e| e.to_string())
                },
                VENV_PROBE_TIMEOUT,
            )
            .await
        },
        move |r| {
            Message::PyprojectEdited(
                id,
                match r {
                    job::JobResult::Done(r) => r,
                    _ => Err("writing pyproject.toml timed out".into()),
                },
            )
        },
    )
}

pub fn fix_label(fix: &bombadil_core::doctor::DoctorFix) -> String {
    use bombadil_core::doctor::DoctorFix;
    match fix {
        DoctorFix::LetUvPick(p) => format!("Let uv pick a {p} that fits"),
        DoctorFix::RequirePython(spec) => format!("Require Python {spec}"),
        DoctorFix::Retry => "Retry".into(),
        DoctorFix::OpenIndexes => "Open Indexes".into(),
        DoctorFix::ShowOutput => "Show output".into(),
    }
}

fn apply_fix(
    app: &mut App,
    i: usize,
    fix: bombadil_core::doctor::DoctorFix,
    runner: Arc<dyn CommandRunner>,
) -> Task<Message> {
    use bombadil_core::doctor::DoctorFix;
    match fix {
        DoctorFix::OpenIndexes => {
            app.tab = Tab::Indexes;
            Task::none()
        }
        DoctorFix::ShowOutput => {
            app.drawer.expanded = true;
            Task::none()
        }
        DoctorFix::LetUvPick(package) => {
            let failed_edit = app
                .last_failure
                .get(&app.config.projects[i].id)
                .is_some_and(|(action, _)| match action {
                    crate::busy::Action::InstallVersion(req) => {
                        deps::normalise(requirement_name(req)) == deps::normalise(&package)
                    }
                    _ => false,
                });
            if failed_edit {
                // A failed version edit never reached pyproject.toml, so the
                // bare name is all uv needs.
                let group = declared_group(app, i, &package).unwrap_or(deps::GroupKind::Main);
                return add_requirement(app, i, package, group, runner);
            }
            // `uv add six` keeps a declared `six==9.9`, so the specifier is
            // dropped in pyproject.toml itself and the sync lets uv choose.
            edit_pyproject(app, i, move |text| {
                bombadil_core::pyproject::loosen_requirement(text, &package)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| {
                        format!(
                            "{package} isn't listed in pyproject.toml; another package asks for that version"
                        )
                    })
            })
        }
        DoctorFix::RequirePython(spec) => edit_pyproject(app, i, move |text| {
            bombadil_core::pyproject::set_requires_python(text, &spec).map_err(|e| e.to_string())
        }),
        DoctorFix::Retry => {
            let action = app
                .config
                .projects
                .get(i)
                .and_then(|p| app.last_failure.get(&p.id))
                .map(|(a, _)| a.clone());
            match action {
                Some(crate::busy::Action::Upgrade(name)) => start_upgrade(app, i, name, runner),
                Some(crate::busy::Action::MembersSync) => {
                    update(app, Message::MembersSyncRequested(i), runner)
                }
                Some(crate::busy::Action::InstallVersion(req)) => {
                    let group = declared_group(app, i, &req).unwrap_or(deps::GroupKind::Main);
                    add_requirement(app, i, req, group, runner)
                }
                _ => update(app, Message::SyncRequested(i), runner),
            }
        }
    }
}

fn start_upgrade(
    app: &mut App,
    i: usize,
    package: Option<String>,
    runner: Arc<dyn CommandRunner>,
) -> Task<Message> {
    let Some(project) = app.config.projects.get(i).cloned() else {
        return Task::none();
    };
    if refuse_while_running(app, project.id) {
        return Task::none();
    }
    if !can_start(app) {
        return Task::none();
    }
    app.busy = Some(crate::busy::Busy::new(
        project.id,
        crate::busy::Action::Upgrade(package.clone()),
    ));
    drawer::start(&mut app.drawer, "upgrading...".to_string());
    let (pin, plan) = (active_pin(&project), sync_plan_for(app, i));
    let op = DepOp::Upgrade(package);
    Task::stream(stream_in_project(
        project,
        app.config.clone(),
        move |uv, ctx| dep_commands(uv, ctx, &pin, &plan, &op),
        ("upgrading timed out", "upgrading failed unexpectedly"),
        runner,
    ))
}

/// What the window's close button asks the tray for. `Hide` still becomes a
/// quit when no tray is running -- see `tray::window_action`.
pub fn close_command(app: &App) -> crate::tray::TrayCommand {
    if app.config.settings.close_to_tray {
        crate::tray::TrayCommand::Hide
    } else {
        crate::tray::TrayCommand::Quit
    }
}

/// The settings to open the window with now: the start-up ones, at the size
/// it was last seen at.
pub fn window_settings_now(app: &App) -> iced::window::Settings {
    let mut settings = app.window_settings.clone();
    if let Some(size) = app.window_size {
        settings.size = size;
    }
    settings
}

/// Opens the window; `WindowOpened` records it once it is up.
pub fn open_window(app: &App) -> Task<Message> {
    let (_, opened) = iced::window::open(window_settings_now(app));
    opened.map(Message::WindowOpened)
}

/// The smallest window Bombadil is usable in.
const MIN_WINDOW: iced::Size = iced::Size::new(640.0, 420.0);

/// The size to open the window at: the remembered one, never smaller than a
/// usable window, or `None` to let the toolkit choose.
pub fn initial_size(layout: &bombadil_core::model::Layout) -> Option<iced::Size> {
    layout.window.map(|(w, h)| {
        iced::Size::new(
            (w as f32).max(MIN_WINDOW.width),
            (h as f32).max(MIN_WINDOW.height),
        )
    })
}

/// The health findings for `projects[i]`, from what has been probed so far.
pub fn project_findings(app: &App, i: usize) -> Vec<crate::health::Finding> {
    let (Some(project), Some(entry)) = (app.config.projects.get(i), app.entries.get(i)) else {
        return Vec::new();
    };
    let row = entry.environments.iter().find(|r| r.active);
    let pr = app.github.get(&project.id).and_then(|s| match &s.lookup {
        bombadil_core::github::Lookup::Found(status) => Some(status),
        _ => None,
    });
    let unpinned = PythonPin::Unpinned;
    crate::health::findings(&crate::health::Input {
        python: row.and_then(|r| r.python_version.as_deref()),
        requires_python: entry.requires_python.as_deref(),
        pin: row.map_or(&unpinned, |r| &r.pin),
        sync: app.sync_checks.get(&project.id),
        git: entry.git.as_ref(),
        pr,
        installed: app.installed_pythons.as_deref(),
        missing: app.missing.contains(&project.id),
        vulnerable: current_vulns(app, i)
            .and_then(|r| r.as_ref().ok())
            .map_or(0, std::collections::BTreeMap::len),
    })
}

/// Projects a probe round sync-checks: all but the one a uv command is
/// running on, which would read as out of step mid-sync.
fn health_round_targets(app: &App) -> Vec<uuid::Uuid> {
    let busy = app.busy.as_ref().map(|b| b.project_id);
    app.config
        .projects
        .iter()
        .map(|p| p.id)
        .filter(|id| Some(*id) != busy)
        .collect()
}

/// The header card's tiles for `projects[i]`, from what has been probed and
/// fetched so far. Members and Git appear only when there are any.
fn summary_tiles(
    app: &App,
    i: usize,
    header: &detail::Header,
    entry: Option<&sidebar::Entry>,
) -> Vec<detail::Tile> {
    use theme::State;
    let env_state = entry.map_or(State::Absent, sidebar::state);
    // A pin the environment does not run yet: say so, and offer the Sync
    // that applies it, beside the signal.
    let pin_note = entry
        .and_then(|e| e.environments.iter().find(|r| r.active))
        .and_then(sidebar::pin_note);
    let mut value = detail::env_value(
        header.venv_status,
        header.python_version.as_ref(),
        env_state,
        entry.and_then(|e| e.requires_python.as_deref()),
    );
    if let Some(note) = &pin_note {
        value = format!("{value}, {note}");
    }
    let mut tiles = vec![detail::Tile {
        label: "Environment",
        state: env_state,
        value,
        note: project_findings(app, i)
            .into_iter()
            .find(|f| f.level == crate::health::Level::Note && f.text.starts_with("Python "))
            .map(|f| f.text),
        press: None,
        sync: (pin_note.is_some() && can_start(app)).then_some(Message::SyncRequested(i)),
    }];
    let fetched = matches!(&app.deps, Some((j, Ok(_))) if *j == i);
    let (state, value) = match &app.deps {
        Some((j, Ok(tab))) if *j == i => detail::deps_summary(tab),
        Some((j, Err(_))) if *j == i => (State::Absent, "unavailable".to_string()),
        _ => (State::Absent, "loading\u{2026}".to_string()),
    };
    tiles.push(detail::Tile {
        label: "Dependencies",
        state,
        value,
        note: None,
        press: Some(Message::TabSelected(Tab::Dependencies)),
        // The fix beside the signal: drift or missing packages, nothing busy.
        // Only for a real answer: "loading" and "unavailable" are not drift,
        // and Sync is not their fix.
        sync: (fetched && state != State::Present && can_start(app))
            .then_some(Message::SyncRequested(i)),
    });
    if let (Some((j, fetch)), Some(project)) = (&app.members, app.config.projects.get(i))
        && *j == i
    {
        let dir = project
            .pyproject_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let tab = members_editor::build(
            &dir,
            &project.members,
            &fetch.kind,
            &fetch.discovered,
            &fetch.installed,
        );
        if let Some((state, value)) = detail::members_summary(&tab.rows) {
            tiles.push(detail::Tile {
                label: "Members",
                state,
                value,
                note: None,
                press: Some(Message::TabSelected(Tab::Members)),
                sync: None,
            });
        }
    }
    if let Some(git) = &header.git {
        tiles.push(detail::Tile {
            label: "Git",
            state: if git.is_clean() {
                State::Present
            } else {
                State::Drifted
            },
            value: detail::git_summary(git),
            note: None,
            press: None,
            sync: None,
        });
    }
    let pr = app
        .config
        .projects
        .get(i)
        .and_then(|p| app.github.get(&p.id))
        .and_then(|s| match &s.lookup {
            bombadil_core::github::Lookup::Found(status) => Some(status),
            _ => None,
        });
    if let Some(status) = pr {
        let c = status.counts();
        let (state, value) = if c.failed > 0 {
            (
                State::Drifted,
                format!("{} of {} failed", c.failed, c.total),
            )
        } else if c.done < c.total {
            (State::Absent, "running".to_string())
        } else {
            (State::Present, format!("all {} passed", c.total))
        };
        tiles.push(detail::Tile {
            label: "Checks",
            state,
            value,
            note: None,
            press: Some(Message::TabSelected(Tab::GitHub)),
            sync: None,
        });
    }
    tiles
}

/// The sidebar's width on screen.
pub fn sidebar_width(app: &App) -> f32 {
    if app.config.settings.layout.sidebar_collapsed {
        theme::RAIL_WIDTH
    } else {
        // Clamped on read too: a hand-edited width must not push the handle
        // off-screen where it cannot be dragged back.
        clamp_sidebar(app.config.settings.layout.sidebar_width as f32) as f32
    }
}

/// Whether a drag is under way, so the pointer has to be listened to.
pub fn dragging(app: &App) -> bool {
    app.sidebar_drag || app.tab_drag.is_some()
}

/// The close message for the top layer, in `view`'s stacking order, top
/// first, so Esc peels one layer per press.
fn escape_message(app: &App) -> Option<Message> {
    // Exactly `view`'s layer order, top first, so Esc closes what is drawn
    // on top -- never a dialog hidden beneath it.
    let layers = [
        (app.modal.is_some(), Message::ModalClosed),
        (app.preferences.is_some(), Message::PreferencesClosed),
        (app.open_menu.is_some(), Message::MenuDismissed),
        (app.confirm_quit, Message::QuitCancelled),
        (app.confirm_stop.is_some(), Message::StopThenCancelled),
        (app.new_project.is_some(), Message::NewProjectCancelled),
        (app.find.is_some(), Message::FindClosed),
        (
            app.confirm_member_sync.is_some(),
            Message::MembersSyncCancelled,
        ),
        (
            app.confirm_environment_removal.is_some(),
            Message::EnvironmentRemovalCancelled,
        ),
        (
            app.confirm_pin_change.is_some(),
            Message::PinChangeCancelled,
        ),
        (
            app.confirm_recreate.is_some(),
            Message::RecreateVenvCancelled,
        ),
        (app.add_local.is_some(), Message::AddLocalCancelled),
        (
            app.project_chooser.is_some(),
            Message::ProjectChooserCancelled,
        ),
        (
            app.confirm_upgrade_all.is_some(),
            Message::DepUpgradeAllCancelled,
        ),
        (app.context_menu.is_some(), Message::ContextMenuClosed),
        (app.add_project.is_some(), Message::AddProjectCancelled),
        (app.dep_edit.is_some(), Message::DepVersionEditCancelled),
        (
            !app.dep_filter.is_empty() && app.tab == Tab::Dependencies,
            Message::DepFilterChanged(String::new()),
        ),
    ];
    layers
        .into_iter()
        .find_map(|(open, close)| open.then_some(close))
}

/// Whether a dialog, menu or card is open over the window -- the inline
/// version edit and the filter are not.
fn overlay_open(app: &App) -> bool {
    escape_message(app).is_some_and(|m| {
        !matches!(
            m,
            Message::DepVersionEditCancelled | Message::DepFilterChanged(_)
        )
    })
}

/// What shortcut `s` does right now, if anything. Nothing acts behind an
/// overlay (the inline version edit is not one), and nothing that starts a
/// uv command runs while one is running.
pub fn shortcut_message(app: &App, s: crate::shortcuts::Shortcut) -> Option<Message> {
    use crate::shortcuts::Shortcut;
    let top = escape_message(app);
    if s == Shortcut::Escape {
        return top;
    }
    if overlay_open(app) {
        return None;
    }
    let selected = app.selected.filter(|i| *i < app.config.projects.len());
    let idle = can_start(app);
    match s {
        Shortcut::AddProject => idle.then_some(Message::AddProjectPickManifestRequested),
        Shortcut::Sync => selected.filter(|_| idle).map(Message::SyncRequested),
        Shortcut::Terminal => selected.map(Message::OpenTerminalHereRequested),
        Shortcut::Refresh => selected.filter(|_| idle).map(|_| Message::RefreshRequested),
        Shortcut::ToggleSidebar => Some(Message::SidebarToggled),
        Shortcut::Find => selected.map(|_| Message::FindRequested),
        Shortcut::Escape => None,
    }
}

/// Whether a new uv command may start. The one rule every uv-starting
/// handler and button follows.
pub fn can_start(app: &App) -> bool {
    app.busy.is_none()
}

/// The label of the project with `id`, if it still exists.
pub fn project_label(app: &App, id: uuid::Uuid) -> Option<&str> {
    app.config
        .projects
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.label.as_str())
}

/// The project a Sync press for `projects[i]` acts on.
///
/// A function rather than an inline `.get(i)` so a test can assert the exact
/// `Project` a press resolves to and hand it to `run_sync` -- the composed
/// environment only exists on the job thread, so the resolution is the last
/// point at which what the sync will run in is observable at all.
fn sync_target(app: &App, i: usize) -> Option<Project> {
    // `app.config.projects`, never a copy of it: this is the list every
    // editor writes, so an edit made a moment ago is in the environment the
    // sync composes. See `App::config`.
    app.config.projects.get(i).cloned()
}

/// The blocking half of renaming an index: move its credential to the account
/// the new name addresses, then persist.
///
/// `SecretKey::Index` is keyed on the index's *name*, so a rename that leaves
/// the entry where it was makes `envspec::index::inject` look under a name
/// nothing was stored under -- a 401 at sync time with nothing on screen
/// explaining it -- while the old entry stays in the user's keychain with
/// nothing left in the app able to name it.
///
/// The set comes before the delete deliberately: the other order loses the
/// credential outright if the second call fails. `Ok(None)` -- no credential
/// was ever saved under the old name -- is the commonest case by far, since
/// most indexes never get one, and it is nothing to move rather than a
/// failure.
///
/// The persist is attempted whichever way the keychain half went: a machine
/// with no usable Secret Service must still be able to rename an index, and
/// making the config write depend on the keychain would be a worse bug than
/// the orphan this fixes.
fn run_index_credential_rename(
    from: &SecretKey,
    to: &SecretKey,
    config: &Config,
    store: &dyn SecretStore,
    persist: &dyn ConfigPersist,
) -> bool {
    let moved = match store.get(from) {
        Ok(Some(value)) => store.set(to, &value).is_ok() && store.delete(from).is_ok(),
        Ok(None) => true,
        Err(_) => false,
    };
    persist.save(config).is_ok() && moved
}

/// Runs an index credential rename off the render thread via `job::run`.
/// `KeyringSecretStore` is constructed here, on the job thread, the same
/// discipline `mark_secret_job` and every other keychain call in this file
/// follow.
async fn rename_index_credential_job(
    from: SecretKey,
    to: SecretKey,
    config: Config,
    persist: Arc<dyn ConfigPersist>,
) -> bool {
    match job::run(
        move || {
            run_index_credential_rename(
                &from,
                &to,
                &config,
                &KeyringSecretStore::new(),
                persist.as_ref(),
            )
        },
        SYNC_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(success) => success,
        job::JobResult::TimedOut | job::JobResult::Failed => false,
    }
}

/// The keychain account `config.indexes[i]`'s credential actually lives
/// under -- `App::index_credential_names[i]`, not whatever is currently in the
/// name textbox, which the user may be part-way through retyping.
///
/// Must be resolved *before* a rename or a removal updates that record: after
/// that there is nothing left to find the entry by, and it sits in the user's
/// keychain forever.
///
/// An empty name is deliberately not filtered out: a row added this session
/// starts with one, and a `get` against it simply finds nothing. (An index
/// left permanently unnamed is M4's problem, which this round does not touch.)
fn index_credential_key(app: &App, i: usize) -> Option<SecretKey> {
    let name = app.index_credential_names.get(i)?;
    Some(SecretKey::Index { name: name.clone() })
}

/// The credential move committing `config.indexes[i]` owes: from the account
/// the entry is actually under (`App::index_credential_names`) to the one the
/// row's current name resolves to. `None` when they already agree -- the row
/// was not renamed since the last commit -- or when there is no such row.
///
/// Its own function so the exact pair of accounts a real commit would move
/// between is directly testable, the same reason `index_editor::credential_save`
/// is not inlined in `update`.
fn index_rename_on_commit(app: &App, i: usize) -> Option<(SecretKey, SecretKey)> {
    let name = app.config.indexes.get(i)?.name.clone();
    let from = index_credential_key(app, i)?;
    let to = SecretKey::Index { name };
    (from != to).then_some((from, to))
}

/// Persists `config.indexes[i]` as it now stands, moving its keychain
/// credential first if the row has been renamed since the last commit.
///
/// This is the whole reason the rename is no longer done from
/// `IndexNameChanged`: that ran `SecretStore::get` once per character typed
/// into the name box -- on a machine whose keychain prompts, once per
/// character. The move itself is unchanged (`run_index_credential_rename`);
/// only how often it happens is.
/// Where the record of the account is updated matters: only the branch that
/// actually dispatches the move touches it, so the two can never disagree
/// about whether the entry has been moved.
fn commit_index_edit(app: &mut App, i: usize) -> Task<Message> {
    if let Some((from, to)) = index_rename_on_commit(app, i) {
        if let (Some(index), Some(slot)) = (
            app.config.indexes.get(i),
            app.index_credential_names.get_mut(i),
        ) {
            *slot = index.name.clone();
        }
        let config = app.config.clone();
        let persist = app.persist.clone();
        return Task::perform(
            rename_index_credential_job(from, to, config, persist),
            |success| Message::Persisted {
                what: "index change",
                success,
            },
        );
    }
    persist_edit(app, "index change")
}

/// The keychain account a removed env var row owes a delete of, if any.
///
/// Resolved before the row is dropped, for the same reason
/// `index_credential_key` is: `SecretKey::GlobalEnv` and
/// `SecretKey::ProjectEnv` are keyed on the variable's name, which the removal
/// is about to take away. `env_editor::unmark_secret` already deletes;
/// removing the row entirely did not, so the value stayed in the keychain
/// unreachable while the row that named it was gone.
///
/// `None` for a plain row: it has no entry, and issuing a delete anyway would
/// be a keychain call per removal on a machine that may have no keychain.
fn env_removal_secret_key(
    app: &App,
    scope: env_editor::EnvScope,
    index: usize,
) -> Option<SecretKey> {
    let (project_id, vars) = match scope {
        env_editor::EnvScope::Global => (None, &app.config.global_env),
        env_editor::EnvScope::Project(i) => {
            let project = app.config.projects.get(i)?;
            (Some(project.id), &project.env)
        }
    };
    let var = vars.get(index)?;
    if !var.secret {
        return None;
    }
    env_editor::secret_key(scope, project_id, &var.key)
}

/// The member context a Sync press for `projects[i]` must use, or `None` when
/// the plain `uv sync` `run_sync` builds is already the right command.
///
/// `None` covers two cases that behave the same and must not be conflated
/// with each other in the drawer: no Members fetch for this project in hand
/// yet, and a fetch whose plan is `SyncPlan::Plain` -- `members_editor`'s own
/// way of saying an ordinary, non-monorepo project's sync command must not be
/// touched by the member machinery at all.
///
/// This exists because plain `uv sync` in a workspace *root* installs no
/// members -- verified against a real uv, which is why
/// `uv::command::sync_all_members` exists. Without this the Sync button
/// produced an environment with none of the workspace's own packages in it,
/// and member-aware sync was reachable only from the Members tab.
fn members_context(
    app: &App,
    i: usize,
) -> Option<(
    bombadil_core::members::WorkspaceKind,
    Vec<bombadil_core::members::DiscoveredMember>,
)> {
    let (fetched_for, fetch) = app.members.as_ref()?;
    if *fetched_for != i {
        return None;
    }
    let project = app.config.projects.get(i)?;
    match members_editor::sync_plan(&project.members, &fetch.kind, &fetch.discovered) {
        members_editor::SyncPlan::Plain => None,
        _ => Some((fetch.kind.clone(), fetch.discovered.clone())),
    }
}

/// Dispatches a background persist of `app.config` as it stands right now,
/// for an edit that needs no keychain call. `what` names the edit in the
/// drawer line a failure leaves.
fn persist_edit(app: &App, what: &'static str) -> Task<Message> {
    let config = app.config.clone();
    let persist = app.persist.clone();
    Task::perform(persist_config(config, persist), move |success| {
        Message::Persisted { what, success }
    })
}

/// Claims the single sync slot, shows a running status naming the project,
/// and dispatches the background members-aware sync -- shared by
/// `MembersSyncRequested`'s no-warning-needed path and
/// `MembersSyncConfirmed`, the same in-flight-guard discipline `SyncRequested`
/// and `RecreateVenvConfirmed` each apply at their own call site.
fn dispatch_members_sync(
    app: &mut App,
    project: Project,
    kind: bombadil_core::members::WorkspaceKind,
    discovered: Vec<bombadil_core::members::DiscoveredMember>,
    runner: Arc<dyn CommandRunner>,
) -> Task<Message> {
    if refuse_while_running(app, project.id) {
        return Task::none();
    }
    let config = app.config.clone();
    app.busy = Some(crate::busy::Busy::new(
        project.id,
        crate::busy::Action::MembersSync,
    ));
    drawer::start(
        &mut app.drawer,
        format!("syncing members for {}...", project.label),
    );
    // Reuses `SyncLine`/`SyncFinished`: this is still an ordinary `uv sync`
    // under the hood, with the same single-slot release, transcript handling
    // and expand-on-failure.
    Task::stream(members_sync(project, config, kind, discovered, runner))
}

/// Dispatches the adopt-or-create probe for wherever the draft's venv choice
/// resolves to right now (spec §8).
///
/// Through `job::run`, like every other filesystem read in this file, and
/// never from `view`: the answer comes from reading a `pyvenv.cfg`, which
/// can hang on a stale network mount, and `view` runs on the render thread.
///
/// `Task::none()` when the path cannot be resolved at all -- only possible
/// for a manifest with no directory component, which the dialog already
/// reports in place of the resolved path.
fn probe_draft_venv(draft: &add_project::Draft, settings: &Settings) -> Task<Message> {
    match add_project::resolved_venv_path(draft, settings) {
        Ok(path) => Task::perform(probe_venv_outcome(path), |(path, outcome)| {
            Message::AddProjectVenvOutcomeProbed(path, outcome)
        }),
        Err(_) => Task::none(),
    }
}

/// Decides adopt-or-create for `path` off the render thread, reusing
/// `probe_venv` (and so `VENV_PROBE_TIMEOUT`, and its degrade-to-hollow
/// behaviour) rather than a second copy of the same job wiring. A probe that
/// times out reports `Create`, which is the honest answer: nothing has been
/// found there.
///
/// Returns the path alongside the outcome so `update` can drop a result the
/// user's later choice has already superseded.
async fn probe_venv_outcome(path: PathBuf) -> (PathBuf, add_project::VenvOutcome) {
    let version = probe_venv(path.clone()).await;
    let outcome = add_project::venv_outcome(&path, |_| version.clone());
    (path, outcome)
}

/// Looks in `dir` for projects to add: its own `pyproject.toml`, or its
/// subfolders'. Off the render thread, like every other read.
async fn scan_folder(dir: PathBuf, registered: Vec<PathBuf>) -> (usize, Vec<PathBuf>) {
    match job::run(
        move || {
            add_project::manifests_in(&dir, &registered, &|p| p.is_file(), &|d| {
                std::fs::read_dir(d)
                    .map(|entries| {
                        entries
                            .flatten()
                            .map(|e| e.path())
                            .filter(|p| p.is_dir())
                            .collect()
                    })
                    .unwrap_or_default()
            })
        },
        VENV_PROBE_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(found) => found,
        job::JobResult::TimedOut | job::JobResult::Failed => (0, Vec::new()),
    }
}

/// The save dialog for an export, suggesting `bombadil-<YYYY-MM-DD>.bombadil`.
async fn pick_bombadil_save() -> Option<PathBuf> {
    let s = bombadil_core::store::stamp(std::time::SystemTime::now());
    rfd::AsyncFileDialog::new()
        .set_file_name(format!(
            "bombadil-{}-{}-{}.bombadil",
            &s[..4],
            &s[4..6],
            &s[6..8]
        ))
        .add_filter("Bombadil export", &["bombadil"])
        .save_file()
        .await
        .map(|handle| handle.path().to_path_buf())
}

async fn pick_bombadil_open() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .add_filter("Bombadil export", &["bombadil"])
        .pick_file()
        .await
        .map(|handle| handle.path().to_path_buf())
}

/// Everything that follows the selected project, for every path that moves
/// the selection -- a click, adding a project, removing one, pressing one of
/// its environments: the Run tab's form starts empty, and its entry points
/// are read for the project now shown.
fn selection_changed(app: &mut App) -> Task<Message> {
    app.command_draft = run_tab::CommandDraft::default();
    app.just = None;
    app.just_args.clear();
    app.just_shown = None;
    app.dep_details = None;
    app.last_change = None;
    tab_opened(app)
}

/// What the tab just opened needs read: the Run tab's entry points.
fn tab_opened(app: &mut App) -> Task<Message> {
    let Some(project) = app
        .selected
        .and_then(|i| app.config.projects.get(i))
        .filter(|_| app.tab == Tab::Run)
        .cloned()
    else {
        return Task::none();
    };
    app.just = Some((project.id, run_tab::JustState::Loading));
    Task::batch([
        load_entry_points(&project),
        load_just(&project, app.config.settings.just_source),
    ])
}

/// Asks `just` for the project's recipes, off the render thread.
fn load_just(project: &Project, source: bombadil_core::model::JustSource) -> Task<Message> {
    let id = project.id;
    let dir = project
        .pyproject_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    Task::perform(
        async move {
            match job::run(
                move || {
                    run_tab::just_blocking(
                        &dir,
                        &|| resolve_just_for(source),
                        &bombadil_core::process::SystemCommandRunner,
                    )
                },
                HEALTH_PROBE_TIMEOUT,
            )
            .await
            {
                job::JobResult::Done(state) => state,
                job::JobResult::TimedOut => {
                    run_tab::JustState::Error("just took too long to answer".into())
                }
                job::JobResult::Failed => {
                    run_tab::JustState::Error("just failed unexpectedly".into())
                }
            }
        },
        move |state| Message::JustLoaded(id, state),
    )
}

/// The Dependencies tab's extras for project `i`: its environment's
/// vulnerabilities, the open details panel, and its last change.
fn deps_extras<'e>(
    app: &'e App,
    i: usize,
    tab: &Result<deps::DepsTab, String>,
) -> deps::Extras<'e> {
    let Some(project) = app.config.projects.get(i) else {
        return deps::Extras::default();
    };
    let vulns = current_vulns(app, i);
    let details = match (&app.dep_details, tab) {
        (Some((id, name, info)), Ok(tab)) if *id == project.id => Some(deps::Details {
            name,
            info: info.as_ref(),
            why: deps::why_installed(tab, name),
        }),
        _ => None,
    };
    let last_change = app
        .last_change
        .as_ref()
        .filter(|(id, _)| *id == project.id)
        .map(|(_, s)| s.as_str());
    deps::Extras {
        vulns,
        details,
        last_change,
    }
}

/// Keeps project `i`'s installed list and, when an action is waiting on it,
/// says what that action changed: appended to its success toast, line by
/// line in the drawer, and on the Dependencies tab.
fn report_change(app: &mut App, i: usize, installed: &[uv::results::InstalledPackage]) {
    let Some(id) = app.config.projects.get(i).map(|p| p.id) else {
        return;
    };
    let before = app.installed.insert(id, installed.to_vec());
    let Some(toast_text) = app.awaiting_change.remove(&id) else {
        return;
    };
    let changes = crate::changes::diff(before.as_deref(), installed);
    let summary = crate::changes::summary(changes.as_deref(), installed.len());
    for line in crate::changes::drawer_lines(changes.as_deref().unwrap_or(&[])) {
        drawer::push_line(&mut app.drawer, &line, &[]);
    }
    let text = format!("{toast_text}: {summary}");
    match app.toasts.iter_mut().find(|t| t.text == toast_text) {
        Some(toast) => toast.text = text,
        None => crate::busy::push(
            &mut app.toasts,
            crate::busy::ToastKind::Success,
            text,
            std::time::Instant::now(),
        ),
    }
    app.last_change = Some((id, summary));
}

/// The installed version of `name` in project `i`'s loaded Dependencies tab.
fn installed_version(app: &App, i: usize, name: &str) -> Option<String> {
    let (j, Ok(tab)) = app.deps.as_ref()? else {
        return None;
    };
    if *j != i {
        return None;
    }
    let want = deps::normalise(name);
    tab.groups
        .iter()
        .flat_map(|g| &g.rows)
        .find(|r| deps::normalise(&r.name) == want)
        .and_then(|r| r.version.clone())
}

/// One environment's vulnerability check: what it was made for, when, and
/// what it found (vulnerable packages only), or why it couldn't be made.
#[derive(Debug, Clone)]
pub struct VulnCheck {
    pub key: String,
    pub at: std::time::Instant,
    pub result:
        Result<std::collections::BTreeMap<String, Vec<bombadil_core::osv::Finding>>, String>,
}

/// Project `i`'s vulnerability check, only when it was made for the
/// packages installed now: an answer about the list before an upgrade
/// would warn about, and offer fixes for, versions that are gone.
fn current_vulns(app: &App, i: usize) -> Option<&VulnResult> {
    let project = app.config.projects.get(i)?;
    let venv = &app
        .entries
        .get(i)?
        .environments
        .iter()
        .find(|r| r.active)?
        .path;
    let check = app.vulns.get(venv)?;
    let installed = app.installed.get(&project.id)?;
    (check.key == vuln_key(installed)).then_some(&check.result)
}

/// What a check found: the vulnerable packages, or why it couldn't tell.
type VulnResult =
    Result<std::collections::BTreeMap<String, Vec<bombadil_core::osv::Finding>>, String>;

/// How long a check of an unchanged environment stands.
const VULN_CHECK_FRESH: Duration = Duration::from_secs(24 * 3600);

/// What is asked of osv.dev: published packages only. A package installed
/// editable is the person's own project or a workspace member -- its name
/// is theirs to keep, and osv.dev knows nothing about it anyway.
fn vuln_packages(installed: &[uv::results::InstalledPackage]) -> Vec<(String, String)> {
    installed
        .iter()
        .filter(|p| p.editable_project_location.is_none())
        .map(|p| (p.name.clone(), p.version.clone()))
        .collect()
}

/// The installed list as one string: a check is redone when this changes.
fn vuln_key(installed: &[uv::results::InstalledPackage]) -> String {
    let mut pairs: Vec<String> = vuln_packages(installed)
        .iter()
        .map(|(name, version)| format!("{}=={version}", deps::normalise(name)))
        .collect();
    pairs.sort();
    pairs.join(",")
}

/// Whether `venv` should be checked now: the setting is on, and there is no
/// check for this exact installed list from the last day.
fn vuln_check_due(app: &App, venv: &Path, key: &str, now: std::time::Instant) -> bool {
    app.config.settings.check_vulnerabilities
        && !app.vulns.get(venv).is_some_and(|c| {
            // A failed check is no answer: the next load tries again.
            c.result.is_ok()
                && c.key == key
                && now.saturating_duration_since(c.at) < VULN_CHECK_FRESH
        })
}

/// Asks osv.dev about project `i`'s active environment, when due.
fn start_vuln_check(
    app: &App,
    i: usize,
    installed: &[uv::results::InstalledPackage],
) -> Task<Message> {
    let Some(venv) = app
        .entries
        .get(i)
        .and_then(|e| e.environments.iter().find(|r| r.active))
        .map(|r| r.path.clone())
    else {
        return Task::none();
    };
    let key = vuln_key(installed);
    if !vuln_check_due(app, &venv, &key, std::time::Instant::now()) {
        return Task::none();
    }
    let packages = vuln_packages(installed);
    let http = app.osv.clone();
    let known = app.vuln_details.clone();
    Task::perform(
        job::attempt(
            move || {
                let mut known = known.lock().unwrap_or_else(|p| p.into_inner());
                bombadil_core::osv::check(http.as_ref(), &packages, &mut known)
            },
            SYNC_TIMEOUT,
            "checking for vulnerabilities",
        ),
        move |result| Message::VulnsChecked(venv.clone(), key.clone(), result),
    )
}

/// Reads a project's `[project.scripts]` off the render thread.
fn load_entry_points(project: &Project) -> Task<Message> {
    let id = project.id;
    let path = project.pyproject_path.clone();
    Task::perform(
        async move {
            std::fs::read_to_string(&path)
                .map(|text| bombadil_core::pyproject::entry_points(&text))
                .unwrap_or_default()
        },
        move |names| Message::EntryPointsLoaded(id, names),
    )
}

/// Why project `i` cannot start a run now, for the disabled buttons'
/// tooltips; `None` when it can.
fn run_blocked(app: &App, i: usize) -> Option<&'static str> {
    let project = app.config.projects.get(i)?;
    if running(app, project.id) {
        return Some("Already running. Stop it first.");
    }
    if app
        .busy
        .as_ref()
        .is_some_and(|b| b.project_id == project.id)
    {
        return Some("Wait for the current uv command to finish.");
    }
    if app.missing.contains(&project.id) {
        return Some(FOLDER_MISSING);
    }
    let has_venv = app.entries.get(i).is_some_and(|e| {
        e.environments
            .iter()
            .any(|row| row.active && row.python_version.is_some())
    });
    if !has_venv {
        return Some("Sync first to create the environment.");
    }
    None
}

/// Whether a uv command that changes project `id`'s environment must wait
/// for its running program, saying so in a warning toast. uv would replace
/// files the program holds open; on Windows that fails half way.
fn refuse_while_running(app: &mut App, id: uuid::Uuid) -> bool {
    if !running(app, id) {
        return false;
    }
    let label = project_label(app, id).unwrap_or("This project").to_string();
    crate::busy::push(
        &mut app.toasts,
        crate::busy::ToastKind::Warning,
        format!("{label} is running a program. Stop it on the Run tab first."),
        std::time::Instant::now(),
    );
    true
}

/// Whether project `id` has a run that has not ended.
fn running(app: &App, id: uuid::Uuid) -> bool {
    app.runs.get(&id).is_some_and(run_tab::Run::live)
}

/// The picker behind "Locate…", async for the same reason as `pick_folder`.
async fn pick_pyproject() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .add_filter("pyproject.toml", &["toml"])
        .pick_file()
        .await
        .map(|handle| handle.path().to_path_buf())
}

pub(crate) const FOLDER_MISSING: &str = "This project's folder isn't on this computer";

/// Measures what the Disk section shows, off the render thread.
fn measure_disk(app: &mut App, runner: Arc<dyn CommandRunner>, after_prune: bool) -> Task<Message> {
    app.disk.measuring = true;
    let config = app.config.clone();
    Task::perform(
        async move {
            match job::run(
                move || crate::disk::measure_blocking(&config, runner.as_ref()),
                SYNC_TIMEOUT,
            )
            .await
            {
                job::JobResult::Done(state) => state,
                job::JobResult::TimedOut | job::JobResult::Failed => crate::disk::State::default(),
            }
        },
        move |state| Message::DiskMeasured(state, after_prune),
    )
}

/// Looks for every project's `pyproject.toml` off the render thread -- a
/// sleeping network drive can take seconds to answer -- so a lost folder is
/// flagged without the project being clicked first.
pub fn sweep_missing(app: &App) -> Task<Message> {
    let exists = app.path_exists;
    let projects: Vec<(uuid::Uuid, PathBuf)> = app
        .config
        .projects
        .iter()
        .map(|p| (p.id, p.pyproject_path.clone()))
        .collect();
    // A drive that never answers keeps the marks there are, rather than
    // clearing them or holding one of the executor's few threads.
    let current: Vec<uuid::Uuid> = app.missing.iter().copied().collect();
    Task::perform(
        async move {
            match job::run(
                move || {
                    projects
                        .into_iter()
                        .filter(|(_, path)| !exists(path))
                        .map(|(id, _)| id)
                        .collect::<Vec<_>>()
                },
                VENV_PROBE_TIMEOUT,
            )
            .await
            {
                job::JobResult::Done(gone) => gone,
                job::JobResult::TimedOut | job::JobResult::Failed => current,
            }
        },
        Message::MissingSwept,
    )
}

/// Whether project `i`'s `pyproject.toml` is gone. Records the answer in
/// `app.missing`, which the detail pane and the sidebar's warning read.
fn folder_missing(app: &mut App, i: usize) -> bool {
    let Some(project) = app.config.projects.get(i) else {
        return false;
    };
    let id = project.id;
    let gone = !(app.path_exists)(&project.pyproject_path);
    if gone {
        app.missing.insert(id);
    } else {
        app.missing.remove(&id);
    }
    gone
}

/// [`folder_missing`], saying so in a warning toast: for an action that
/// cannot start without the folder.
fn refuse_missing(app: &mut App, i: usize) -> bool {
    let gone = folder_missing(app, i);
    if gone {
        crate::busy::push(
            &mut app.toasts,
            crate::busy::ToastKind::Warning,
            FOLDER_MISSING.into(),
            std::time::Instant::now(),
        );
    }
    gone
}

/// The index of the project an open confirmation names, by id.
fn confirmed_index(app: &App, id: Option<uuid::Uuid>) -> Option<usize> {
    let id = id?;
    app.config.projects.iter().position(|p| p.id == id)
}

/// Opens the async folder picker. Never the blocking `rfd::FileDialog`: a
/// modal dialog on the render thread would freeze the window exactly like a
/// synchronous `CommandRunner::run` call would.
async fn pick_folder() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .pick_folder()
        .await
        .map(|handle| handle.path().to_path_buf())
}

/// The save dialog for the drawer's Export, async for the same reason as
/// `pick_folder`.
async fn pick_save_file() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_file_name("bombadil-output.txt")
        .add_filter("Text", &["txt", "log"])
        .save_file()
        .await
        .map(|handle| handle.path().to_path_buf())
}

/// Writes an exported transcript, the error as a sentence for the toast.
fn write_transcript(path: PathBuf, text: &str) -> Result<PathBuf, String> {
    std::fs::write(&path, text)
        .map(|()| path)
        .map_err(|error| error.to_string())
}

/// Opens the async file picker: a pre-activate script (criterion 3), or the
/// PyCharm launcher in Settings. Same
/// reasoning as `pick_pyproject`: the blocking `rfd::FileDialog` must never
/// be used, since a modal dialog on the render thread would freeze the
/// window for as long as the user takes to answer it.
async fn pick_file() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .pick_file()
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

/// Turns the blocking half of a uv command into the stream of messages the
/// drawer consumes: one `SyncLine` per line of output as uv prints it, then
/// one terminal `SyncFinished`.
///
/// Shared by every sync-shaped action -- a plain sync, a members-aware sync,
/// a recreate -- because all three are a `uv sync` with the same transcript
/// handling and the same single in-flight slot. Only the wording of a timeout
/// differs, which is what `timed_out` and `failed` are for.
///
/// `job::stream` off the render thread for the same reason `job::run` was
/// there before: `CommandRunner::stream` still blocks its caller until the
/// child exits. What changed is that it no longer blocks the *output*.
fn sync_stream<F>(
    work: F,
    timed_out: &'static str,
    failed: &'static str,
) -> impl futures::Stream<Item = Message>
where
    F: FnOnce(&mut dyn FnMut(String)) -> (bool, Vec<String>) + Send + 'static,
{
    job::stream(
        move |emit| {
            let (success, lines) = work(&mut |line| emit(Message::SyncLine(line)));
            emit(Message::SyncFinished { success, lines });
        },
        SYNC_TIMEOUT,
    )
    .map(move |result| match result {
        job::JobResult::Done(message) => message,
        job::JobResult::TimedOut => Message::SyncFinished {
            success: false,
            lines: vec![timed_out.to_string()],
        },
        job::JobResult::Failed => Message::SyncFinished {
            success: false,
            lines: vec![failed.to_string()],
        },
    })
}

/// Streams a plain `uv sync` off the render thread.
///
/// `KeyringSecretStore` is constructed here, on the job thread, rather than
/// taken as a parameter of `update`: it is the one piece of this path a test
/// must never reach, so it lives at the boundary and `run_sync` below takes
/// the store abstractly.
fn sync_project(
    project: Project,
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> impl futures::Stream<Item = Message> {
    sync_stream(
        move |on_line| {
            run_sync(
                &project,
                &config,
                &KeyringSecretStore::new(),
                runner.as_ref(),
                on_line,
            )
        },
        "sync timed out",
        "sync failed unexpectedly",
    )
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
pub(crate) fn resolve_uv_for(
    config: &Config,
    runner: &dyn CommandRunner,
) -> Result<uv::ResolvedUv, String> {
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

/// Which `just` runs recipes, given the `just_source` setting. The
/// bundled copy is unpacked into the data dir only when it is the answer.
/// `Ok(None)`: the settings want `just` from PATH and there is none.
pub(crate) fn resolve_just_for(
    source: bombadil_core::model::JustSource,
) -> Result<Option<bombadil_core::just::Found>, String> {
    let bundled = || -> Result<PathBuf, String> {
        let data_dir =
            bombadil_core::store::paths::default_data_dir().map_err(|e| e.to_string())?;
        bombadil_uv_bin::ensure_just_extracted(&data_dir).map_err(|e| e.to_string())
    };
    bombadil_core::just::resolve(source, uv::locate_on_path("just"), &bundled)
}

/// The blocking half of interpreter discovery: resolve uv, run
/// `uv python list --output-format json`, and parse it.
///
/// Every failure -- no usable uv, a non-zero exit, output this build cannot
/// parse -- comes back as `Err` carrying uv's own text where there is any.
/// Degrading to an empty list instead is what left the dialog stuck on
/// "(checking available interpreters...)" forever: the empty list is
/// indistinguishable from a fetch that has not landed yet, so the dialog
/// showed neither an error nor the install offer, and there was nothing the
/// user could do but cancel.
fn list_interpreters_blocking(
    config: &Config,
    runner: &dyn CommandRunner,
) -> Result<Vec<Interpreter>, String> {
    let resolved = resolve_uv_for(config, runner)?;
    let spec = uv::command::python_list(&resolved);
    match runner.run(&spec) {
        Ok(outcome) if outcome.success() => uv::results::parse_python_list(&outcome.stdout)
            .map_err(|err| format!("could not read uv's interpreter list: {err}")),
        // uv ran and refused. Its stderr is the actionable part; the exit
        // code alone tells the user nothing.
        Ok(outcome) => Err(uv_refusal(&outcome, "uv python list failed")),
        Err(err) => Err(err.to_string()),
    }
}

/// Fetches the interpreters uv can see, off the render thread -- the same
/// seam `probe_venv` uses.
///
/// `SYNC_TIMEOUT`, not `VENV_PROBE_TIMEOUT`: `uv python list` itself only
/// reads uv's own interpreter cache, but this is also the first call that
/// resolves uv at all, and on a machine with nothing usable on PATH --
/// exactly the case the bundled uv exists for -- resolving means extracting
/// and hashing a ~35 MB binary and then executing a freshly written
/// executable, which on Windows means waiting for Defender to scan it. Three
/// seconds does not cover that, and a timeout here is not a hollow sidebar
/// row: it is a dialog with no interpreter to offer.
async fn list_interpreters(
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> Result<Vec<Interpreter>, String> {
    job::attempt(
        move || list_interpreters_blocking(&config, runner.as_ref()),
        SYNC_TIMEOUT,
        "listing interpreters",
    )
    .await
}

/// The blocking half of installing an interpreter: resolve uv and run `uv
/// python install <version>`. Failure collapses to `false` -- the caller
/// re-lists regardless, so a failed install just means the refreshed list
/// still has nothing satisfying, and the dialog already shows exactly that.
/// `Err` carries uv's own words (or why uv could not be run), so the dialog
/// can say why no interpreter appeared.
fn install_interpreter_blocking(
    version: &str,
    config: &Config,
    runner: &dyn CommandRunner,
) -> Result<(), String> {
    let resolved = resolve_uv_for(config, runner)?;
    let spec = uv::command::python_install(&resolved, version);
    match runner.run(&spec) {
        Ok(outcome) if outcome.success() => Ok(()),
        Ok(outcome) => Err(uv_refusal(&outcome, "uv python install failed")),
        Err(err) => Err(err.to_string()),
    }
}

/// Installs `version` through uv, then re-fetches the interpreter list.
/// Bounded by `SYNC_TIMEOUT`, not `VENV_PROBE_TIMEOUT`: unlike listing,
/// `uv python install` can download a whole interpreter.
async fn install_and_relist(
    version: String,
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> Result<Vec<Interpreter>, String> {
    let install_config = config.clone();
    let install_runner = runner.clone();
    // A failed install is the answer, not a reason to list again: the list
    // would look unchanged and say nothing about why.
    match job::run(
        move || install_interpreter_blocking(&version, &install_config, install_runner.as_ref()),
        SYNC_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(Ok(())) => list_interpreters(config, runner).await,
        job::JobResult::Done(Err(why)) => Err(why),
        job::JobResult::TimedOut => Err("installing the interpreter timed out".to_string()),
        job::JobResult::Failed => Err("installing the interpreter failed unexpectedly".to_string()),
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
    on_line: &mut dyn FnMut(String),
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
    let Some(environment) = project.active_environment().cloned() else {
        return (false, vec!["this project has no environment".to_string()]);
    };
    let venv_path = match venv_path::resolve(project, &environment, &config.settings) {
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
    // The ACTIVE environment's own pin. Not the project's -- a project has
    // none now -- and not another environment's: a project with a 3.11 and a
    // 3.12 environment must build each with its own interpreter, which is the
    // entire reason to have both.
    //
    // `sync_with_python` is the same builder the add-project dialog uses, so
    // there is one way to say "-p".
    let spec = match environment.python.clone() {
        PythonPin::Version(version) => uv::command::sync_with_python(&resolved_uv, &ctx, &version),
        PythonPin::Unpinned => uv::command::sync(&resolved_uv, &ctx),
    };
    compose_and_stream(spec, project, config, &venv_path, store, runner, on_line)
}

/// The blocking half of a version change: `uv add name==version` in the
/// row's group. uv resolves the whole project first and, when that fails,
/// leaves `pyproject.toml` and the lock as they were and prints why -- which
/// streams to the drawer like any sync's output. On success it also syncs.
/// Runs the uv command `build` makes, in `project`'s directory against its
/// active environment, through the composed environment and the transcript.
/// Shared by every Dependencies-tab command: set a version, upgrade, add a
/// local package.
fn run_in_project(
    project: &Project,
    config: &Config,
    build: &dyn Fn(
        &uv::ResolvedUv,
        &uv::command::ProjectContext,
    ) -> Vec<bombadil_core::process::CommandSpec>,
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
    on_line: &mut dyn FnMut(String),
) -> (bool, Vec<String>) {
    let (resolved_uv, ctx) = match project_target(project, config, runner) {
        Ok(target) => target,
        Err(err) => return (false, vec![err]),
    };
    let venv_path = ctx.venv_path.clone();
    // In order, stopping at the first failure: a workspace add is `uv add
    // --no-sync` then the member-aware sync, and the sync must not run
    // after an add uv refused.
    let mut transcript = Vec::new();
    for spec in build(&resolved_uv, &ctx) {
        let (ok, lines) =
            compose_and_stream(spec, project, config, &venv_path, store, runner, on_line);
        transcript.extend(lines);
        if !ok {
            return (false, transcript);
        }
    }
    (true, transcript)
}

/// The uv and the folder and environment a project-scoped uv command runs
/// against: the project's directory and its active environment.
fn project_target(
    project: &Project,
    config: &Config,
    runner: &dyn CommandRunner,
) -> Result<(uv::ResolvedUv, uv::command::ProjectContext), String> {
    let Some(project_dir) = project
        .pyproject_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    else {
        return Err("no project directory to run uv in".to_string());
    };
    let Some(environment) = project.active_environment().cloned() else {
        return Err("this project has no environment".to_string());
    };
    let venv_path = venv_path::resolve(project, &environment, &config.settings)
        .map_err(|err| err.to_string())?;
    let resolved_uv = resolve_uv_for(config, runner)?;
    let ctx = uv::command::ProjectContext {
        project_dir: project_dir.to_path_buf(),
        venv_path,
    };
    Ok((resolved_uv, ctx))
}

/// The blocking half of a Run-tab run: `uv run --no-sync <args>` in the
/// project's folder and active environment, composed exactly as a terminal
/// is, streamed line by line and stoppable through `cancel`.
fn run_program(
    project: &Project,
    config: &Config,
    args: &[String],
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
    cancel: &std::sync::atomic::AtomicBool,
    on_line: &mut dyn FnMut(String),
) -> run_tab::RunEnd {
    use run_tab::RunEnd;
    let (resolved_uv, ctx) = match project_target(project, config, runner) {
        Ok(target) => target,
        Err(e) => return RunEnd::Failed(e),
    };
    let spec = uv::command::run(&resolved_uv, &ctx, args);
    let (mut spec, secrets) = match composed(spec, project, config, &ctx.venv_path, store, runner) {
        Ok(c) => c,
        Err(e) => return RunEnd::Failed(e),
    };
    // Piped, Python holds its output back until 8 KB or exit, and on Windows
    // writes cp1252: a progress line a second would show nothing, and an
    // emoji would crash. A value the project set itself wins.
    for (key, value) in [("PYTHONUNBUFFERED", "1"), ("PYTHONIOENCODING", "utf-8")] {
        spec.env
            .entry(key.to_string())
            .or_insert_with(|| value.to_string());
    }
    let result = runner.stream_cancellable(
        &spec,
        &mut |line| on_line(envspec::redact(line.text().to_string(), &secrets)),
        cancel,
    );
    match result {
        Ok(_) if cancel.load(std::sync::atomic::Ordering::SeqCst) => RunEnd::Stopped,
        Ok(outcome) => RunEnd::Exited(outcome.status),
        Err(e) => RunEnd::Failed(envspec::redact(e.to_string(), &secrets)),
    }
}

/// A Dependencies-tab change: upgrade one package or all, or an add whose
/// `uv add` command the core builders made.
#[derive(Debug, Clone)]
pub enum DepOp {
    Upgrade(Option<String>),
    Add(bombadil_core::process::CommandSpec),
}

/// The uv commands for `op`, keeping what a plain Sync keeps: the active
/// environment's pin (`-p`), and a workspace's installed members. Plain
/// `uv sync`, and the sync `uv add` runs on its own, both uninstall workspace
/// members (verified against uv 0.12) -- so in a workspace the add runs with
/// `--no-sync` and the member-aware sync follows.
pub fn dep_commands(
    uv: &uv::ResolvedUv,
    ctx: &uv::command::ProjectContext,
    pin: &PythonPin,
    plan: &members_editor::SyncPlan,
    op: &DepOp,
) -> Vec<bombadil_core::process::CommandSpec> {
    use members_editor::SyncPlan;
    let python = |spec: bombadil_core::process::CommandSpec| with_pin(spec, pin);
    let sync = || sync_spec(uv, ctx, pin, plan);
    match op {
        DepOp::Upgrade(Some(name)) => vec![sync().args(["--upgrade-package", name.as_str()])],
        DepOp::Upgrade(None) => vec![sync().arg("--upgrade")],
        DepOp::Add(add) if *plan == SyncPlan::Plain => vec![python(add.clone())],
        DepOp::Add(add) => vec![add.clone().arg("--no-sync"), sync()],
    }
}

/// `spec` with the environment's pin, when it has one.
fn with_pin(
    spec: bombadil_core::process::CommandSpec,
    pin: &PythonPin,
) -> bombadil_core::process::CommandSpec {
    match pin {
        PythonPin::Version(version) => spec.args(["-p", version.as_str()]),
        PythonPin::Unpinned => spec,
    }
}

/// The sync Bombadil runs for this pin and member plan.
fn sync_spec(
    uv: &uv::ResolvedUv,
    ctx: &uv::command::ProjectContext,
    pin: &PythonPin,
    plan: &members_editor::SyncPlan,
) -> bombadil_core::process::CommandSpec {
    use members_editor::SyncPlan;
    with_pin(
        match plan {
            SyncPlan::Plain => uv::command::sync(uv, ctx),
            SyncPlan::AllPackages => uv::command::sync_all_members(uv, ctx),
            SyncPlan::Named(names) => uv::command::sync_members(uv, ctx, names),
        },
        pin,
    )
}

/// Whether that same sync would change anything: the health probe asks
/// about the sync the user would actually run, or it reports drift that
/// Sync never clears.
fn sync_check_spec(
    uv: &uv::ResolvedUv,
    ctx: &uv::command::ProjectContext,
    pin: &PythonPin,
    plan: &members_editor::SyncPlan,
) -> bombadil_core::process::CommandSpec {
    uv::command::as_check(sync_spec(uv, ctx, pin, plan))
}

/// How a Sync of `projects[i]` treats workspace members, from the Members
/// fetch in hand; `Plain` when there is none or it is not a workspace.
fn sync_plan_for(app: &App, i: usize) -> members_editor::SyncPlan {
    match (members_context(app, i), app.config.projects.get(i)) {
        (Some((kind, discovered)), Some(project)) => {
            members_editor::sync_plan(&project.members, &kind, &discovered)
        }
        _ => members_editor::SyncPlan::Plain,
    }
}

/// The active environment's pin of `project`.
fn active_pin(project: &Project) -> PythonPin {
    project
        .active_environment()
        .map(|e| e.python.clone())
        .unwrap_or_default()
}

/// Streams [`run_in_project`] off the render thread, through the same slot
/// and transcript a sync uses.
fn stream_in_project(
    project: Project,
    config: Config,
    build: impl Fn(
        &uv::ResolvedUv,
        &uv::command::ProjectContext,
    ) -> Vec<bombadil_core::process::CommandSpec>
    + Send
    + 'static,
    (timed_out, failed): (&'static str, &'static str),
    runner: Arc<dyn CommandRunner>,
) -> impl futures::Stream<Item = Message> {
    sync_stream(
        move |on_line| {
            run_in_project(
                &project,
                &config,
                &build,
                &KeyringSecretStore::new(),
                runner.as_ref(),
                on_line,
            )
        },
        timed_out,
        failed,
    )
}

/// Whether pressing Sync would destroy the existing environment.
///
/// True only when there is an environment, there is a pin, and the two
/// disagree -- because that is exactly when uv removes the directory rather
/// than reusing it. Verified against uv 0.12.1: `uv sync -p 3.12` on a 3.13
/// venv prints `Removed virtual environment at: .venv`.
///
/// `None` for `venv_version` is the ordinary create path and must never be
/// gated behind a confirmation: there is nothing to lose, and making a user
/// confirm the creation of something that does not exist yet teaches them to
/// click through the one that matters.
fn sync_would_delete(pin: &PythonPin, venv_version: Option<&str>) -> bool {
    let Some(version) = venv_version else {
        return false;
    };
    !interpreter::satisfies_pin(version, pin)
}

/// The blocking half of a members-aware sync (spec §5a, Task 5, criterion
/// 4): builds the sync command from the project's own member selection
/// through `members_editor::sync_plan` -- the only place that decision is
/// made -- instead of `run_sync`'s unconditional plain `uv sync`. Reached
/// only from the Members tab's own "Sync members" action
/// (`Message::MembersSyncRequested`/`MembersSyncConfirmed`); the ordinary
/// Sync button still calls `run_sync` unmodified, so an everyday project's
/// sync behaviour is untouched by this feature.
fn run_members_sync(
    project: &Project,
    config: &Config,
    kind: &bombadil_core::members::WorkspaceKind,
    discovered: &[bombadil_core::members::DiscoveredMember],
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
    on_line: &mut dyn FnMut(String),
) -> (bool, Vec<String>) {
    let Some(project_dir) = project
        .pyproject_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    else {
        return (false, vec!["no project directory to run uv in".to_string()]);
    };
    let Some(environment) = project.active_environment().cloned() else {
        return (false, vec!["this project has no environment".to_string()]);
    };
    let venv_path = match venv_path::resolve(project, &environment, &config.settings) {
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
    let spec = match members_editor::sync_plan(&project.members, kind, discovered) {
        members_editor::SyncPlan::Plain => uv::command::sync(&resolved_uv, &ctx),
        members_editor::SyncPlan::AllPackages => uv::command::sync_all_members(&resolved_uv, &ctx),
        members_editor::SyncPlan::Named(names) => {
            uv::command::sync_members(&resolved_uv, &ctx, &names)
        }
    };
    compose_and_stream(spec, project, config, &venv_path, store, runner, on_line)
}

/// Streams a members-aware sync off the render thread -- same shape as
/// `sync_project`, including `SYNC_TIMEOUT`, since this is also a real `uv
/// sync` under the hood.
fn members_sync(
    project: Project,
    config: Config,
    kind: bombadil_core::members::WorkspaceKind,
    discovered: Vec<bombadil_core::members::DiscoveredMember>,
    runner: Arc<dyn CommandRunner>,
) -> impl futures::Stream<Item = Message> {
    sync_stream(
        move |on_line| {
            run_members_sync(
                &project,
                &config,
                &kind,
                &discovered,
                &KeyringSecretStore::new(),
                runner.as_ref(),
                on_line,
            )
        },
        "sync timed out",
        "sync failed unexpectedly",
    )
}

/// The blocking half of a recreate: delete the project's environment, then
/// rebuild it with the same `uv sync` an ordinary Sync runs.
///
/// `delete` is a parameter, not `venv_path::delete` called directly, for the
/// same reason `run_confirm_add_project` takes its `probe`: it is the one
/// call in this file that removes a directory tree, and a GUI test must not
/// touch the filesystem. The real caller supplies
/// `bombadil_core::venv_path::delete`, which independently re-derives the
/// project's venv location and refuses anything that is not exactly it -- so
/// this function's `resolve` deciding wrongly cannot make it delete the
/// wrong thing.
///
/// A refused delete stops the recreate rather than falling through to the
/// sync: syncing anyway would silently do something other than what the user
/// confirmed, and the reason core refused is what they need to see.
fn run_recreate_venv(
    project: &Project,
    config: &Config,
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
    on_line: &mut dyn FnMut(String),
    delete: impl Fn(&Path) -> Result<(), String>,
) -> (bool, Vec<String>) {
    let Some(environment) = project.active_environment().cloned() else {
        return (false, vec!["this project has no environment".to_string()]);
    };
    let venv_path = match venv_path::resolve(project, &environment, &config.settings) {
        Ok(path) => path,
        Err(err) => return (false, vec![err.to_string()]),
    };

    if let Err(err) = delete(&venv_path) {
        return (false, vec![err, "nothing was deleted".to_string()]);
    }

    let (success, mut lines) = run_sync(project, config, store, runner, on_line);
    if !success {
        // The outcome the confirmation warned about. "sync failed" on its own
        // leaves the user unaware that the environment they had is gone.
        lines.push(format!(
            "{} was deleted and could not be rebuilt; the environment no longer exists",
            venv_path.display()
        ));
    }
    (success, lines)
}

/// Streams a recreate off the render thread -- same shape as `sync_project`,
/// including `SYNC_TIMEOUT`, since the rebuild half *is* a `uv sync`.
fn recreate_venv(
    project: Project,
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> impl futures::Stream<Item = Message> {
    sync_stream(
        move |on_line| {
            let settings = config.settings.clone();
            let target = &project;
            run_recreate_venv(
                &project,
                &config,
                &KeyringSecretStore::new(),
                runner.as_ref(),
                on_line,
                |path| {
                    let environment = target.active_environment().cloned().unwrap_or_default();
                    venv_path::delete(target, &environment, &settings, path)
                        .map_err(|e| e.to_string())
                },
            )
        },
        "recreating the environment timed out",
        "recreating the environment failed unexpectedly",
    )
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

    let Some(environment) = project.active_environment().cloned() else {
        return (false, vec!["this project has no environment".to_string()]);
    };
    let venv_path = match venv_path::resolve(project, &environment, &config.settings) {
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
    // Collected, not streamed: this path ends in `AddProjectConfirmFinished`,
    // which carries the dialog's outcome and the index of the project it
    // created -- there is no per-line message shaped like it, and inventing
    // one is a separate feature. The command still runs through
    // `CommandRunner::stream`; only the delivery is batched.
    collect_lines(|on_line| {
        compose_and_stream(spec, project, config, &venv_path, store, runner, on_line)
    })
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
/// list must never be threaded through one. Every line goes through it on
/// the way to `on_line`, so a secret cannot reach the drawer even one line
/// at a time.
///
/// `CommandRunner::stream`, not `run`: the process output goes to `on_line`
/// as uv prints it. Which is why the returned `Vec` holds only what
/// `on_line` never saw -- the errors that happen *instead* of a run, so a
/// caller that shows both does not show uv's output twice.
fn compose_and_stream(
    spec: bombadil_core::process::CommandSpec,
    project: &Project,
    config: &Config,
    venv_path: &Path,
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
    on_line: &mut dyn FnMut(String),
) -> (bool, Vec<String>) {
    let (spec, secrets) = match composed(spec, project, config, venv_path, store, runner) {
        Ok(c) => c,
        Err(err) => return (false, vec![err]),
    };

    match runner.stream(&spec, &mut |line| {
        on_line(envspec::redact(line.text().to_string(), &secrets))
    }) {
        // Nothing to return: every line uv printed has already gone to
        // `on_line`, and handing the same transcript back would show it twice.
        Ok(outcome) => (outcome.success(), Vec::new()),
        Err(err) => (false, vec![envspec::redact(err.to_string(), &secrets)]),
    }
}

/// `spec` with the project's composed environment, ready to run, and the
/// secret values its output must be scrubbed of.
fn composed(
    mut spec: bombadil_core::process::CommandSpec,
    project: &Project,
    config: &Config,
    venv_path: &Path,
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
) -> Result<(bombadil_core::process::CommandSpec, Vec<String>), String> {
    let inputs = EnvInputs {
        parent: std::env::vars().collect(),
        config: config.clone(),
        project: project.clone(),
        venv_path: venv_path.to_path_buf(),
    };
    // `compose` already scrubs its own errors of the secrets it resolved.
    let env = envspec::compose(&inputs, store, runner).map_err(|err| err.to_string())?;
    let secrets = secret_values(&inputs, &env);
    spec.env = env;
    // `clear_env`, the discipline plan 2b established and the spec builders
    // leave off: the composition is authoritative, not a layer on top of
    // whatever launched Bombadil.
    spec.clear_env = true;
    Ok((spec, secrets))
}

/// Run one of the streaming halves above and gather what it streamed, for the
/// callers that still want the whole transcript in one piece -- the
/// add-project dialog, whose outcome is a single result, and the tests that
/// assert on what a sync produced.
fn collect_lines(
    work: impl FnOnce(&mut dyn FnMut(String)) -> (bool, Vec<String>),
) -> (bool, Vec<String>) {
    let mut lines = Vec::new();
    let (success, rest) = work(&mut |line| lines.push(line));
    lines.extend(rest);
    (success, lines)
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
/// `scratch_dir` is a parameter rather than derived from
/// `std::env::temp_dir()` in here: `terminal::open::open` *writes* the
/// re-assertion init file into it, and a test that reached this function
/// therefore wrote into the machine's real temp directory -- the exact
/// no-filesystem constraint these tests exist to honour. The real caller
/// (`open_terminal`) supplies the Bombadil-owned temp directory.
fn run_open_terminal(
    choice: &TerminalChoice,
    project: &Project,
    config: &Config,
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
    scratch_dir: &Path,
) -> Result<(), String> {
    let environment = project
        .active_environment()
        .cloned()
        .ok_or_else(|| "this project has no environment".to_string())?;
    let venv_path =
        venv_path::resolve(project, &environment, &config.settings).map_err(|e| e.to_string())?;
    let inputs = EnvInputs {
        parent: std::env::vars().collect(),
        config: config.clone(),
        project: project.clone(),
        venv_path,
    };
    let shell = std::env::var("SHELL").unwrap_or_default();
    terminal::open::open(
        choice,
        Os::host(),
        &inputs,
        &shell,
        store,
        runner,
        scratch_dir,
    )
    .map_err(|err| err.to_string())
}

/// The blocking half of Open in PyCharm: find the launcher, then hand it the
/// project directory with the composed environment.
///
/// The venv comes from the project's *active* environment, exactly as
/// `run_open_terminal` resolves it -- a project with several environments
/// must open PyCharm on the same one the terminal button and Sync act on, or
/// the two buttons beside each other silently mean different things.
fn run_open_pycharm(
    project: &Project,
    config: &Config,
    home: &Path,
    jetbrains: Option<&Path>,
    store: &dyn SecretStore,
    runner: &dyn CommandRunner,
) -> Result<Option<String>, String> {
    // A launcher chosen in Settings is used as is, never second-guessed by
    // detection; one that has gone says so rather than quietly opening
    // whichever PyCharm detection would find instead.
    let launcher = match &config.settings.pycharm {
        Some(chosen) if chosen.exists() => chosen.clone(),
        Some(chosen) => {
            return Err(format!(
                "The PyCharm chosen in Settings > PyCharm, {}, no longer exists. \
                 Choose it again, or switch back to detecting it.",
                chosen.display()
            ));
        }
        None => ide::detect(Os::host(), home, &|p| p.exists(), runner).ok_or_else(|| {
            // Names where it looked. "PyCharm was not found" alone leaves a
            // user with a working install nothing to check.
            let looked: Vec<String> = ide::candidates(Os::host(), home)
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            format!(
                "PyCharm was not found. Looked in:\n  {}\nand for `pycharm` on PATH. \
                 Choose it in Settings > PyCharm.",
                looked.join("\n  ")
            )
        })?,
    };
    let environment = project
        .active_environment()
        .cloned()
        .ok_or_else(|| "this project has no environment".to_string())?;
    let venv_path =
        venv_path::resolve(project, &environment, &config.settings).map_err(|e| e.to_string())?;
    let inputs = EnvInputs {
        parent: std::env::vars().collect(),
        config: config.clone(),
        project: project.clone(),
        venv_path,
    };
    // Before the launch: PyCharm reads `misc.xml` when it opens the project.
    // A failure here is a warning, not a reason to keep the IDE closed.
    let warning = match jetbrains {
        Some(dir) => {
            let running = ide::is_running(Os::host(), runner);
            ide::configure_interpreter(&inputs, dir, home, running).err()
        }
        None => Some("this platform has no config directory; set the interpreter by hand".into()),
    };
    ide::open(&launcher, &inputs, store, runner).map_err(|err| err.to_string())?;
    Ok(warning)
}

/// Runs Open in PyCharm off the render thread. `VENV_PROBE_TIMEOUT`, like
/// `open_terminal`: this launches and returns, so a wait measured in seconds
/// already means something is wrong.
async fn open_pycharm(
    project: Project,
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> Result<Option<String>, String> {
    job::attempt(
        move || {
            // Read here, at the boundary, so no test reaches the real home
            // directory -- the same discipline `KeyringSecretStore::new()`
            // follows one line down.
            let home = home_dir();
            run_open_pycharm(
                &project,
                &config,
                &home,
                ide::jetbrains_config_dir().as_deref(),
                &KeyringSecretStore::new(),
                runner.as_ref(),
            )
        },
        VENV_PROBE_TIMEOUT,
        "opening PyCharm",
    )
    .await
}

/// This user's home directory, or an empty path when the platform will not
/// say. An empty path makes every `home`-relative candidate a relative path
/// that will not exist, so detection falls through to `PATH` rather than
/// probing nonsense.
/// Looks for PyCharm off the render thread, for Settings > PyCharm. A
/// timeout or a failure reads as "not found", as it would on open.
fn detect_pycharm(runner: Arc<dyn CommandRunner>) -> Task<Message> {
    Task::perform(
        async move {
            match job::run(
                move || ide::detect(Os::host(), &home_dir(), &|p| p.exists(), runner.as_ref()),
                VENV_PROBE_TIMEOUT,
            )
            .await
            {
                job::JobResult::Done(found) => found,
                job::JobResult::TimedOut | job::JobResult::Failed => None,
            }
        },
        Message::PycharmDetected,
    )
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Says in the drawer that there is no terminal to open.
///
/// Reported, not silent: a button that does nothing at all reads as broken,
/// and "nothing installed" is something the user can act on by installing or
/// configuring one.
fn report_no_terminal(app: &mut App) {
    drawer::push_line(
        &mut app.drawer,
        "no terminal emulator was found on this machine",
        &[],
    );
    drawer::set_idle(&mut app.drawer, "open terminal failed".to_string());
    app.drawer.expanded = true;
}

/// Runs off the render thread via `job::run` -- same shape as `sync_project`.
async fn open_terminal(
    choice: TerminalChoice,
    project: Project,
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> Result<(), String> {
    job::attempt(
        move || {
            // A Bombadil-owned scratch directory for the re-assertion init
            // file `terminal::open::open` writes -- namespaced per-project
            // underneath by `open` itself, so two concurrent opens for
            // different projects cannot collide (see its own doc comment).
            // Built here, at the boundary, so no test ever reaches it.
            let scratch_dir = std::env::temp_dir().join("bombadil-terminal-init");
            run_open_terminal(
                &choice,
                &project,
                &config,
                &KeyringSecretStore::new(),
                runner.as_ref(),
                &scratch_dir,
            )
        },
        VENV_PROBE_TIMEOUT,
        "opening the terminal",
    )
    .await
}

/// The blocking half of Open folder: show the project's own directory --
/// where `pyproject.toml` lives, the same place Open terminal opens in -- in
/// the platform's file manager.
///
/// `os` is a parameter, never `Os::host()` read in here, for the reason the
/// whole `terminal` module states: it is the only thing that lets the macOS
/// and Windows commands be covered by tests running on Linux.
///
/// Nothing composed is involved: `folder::open` attaches no environment, so
/// there is no secret to redact and no keychain to reach. That is why this
/// takes no `SecretStore` while `run_open_terminal` does.
fn run_open_folder(os: Os, project: &Project, runner: &dyn CommandRunner) -> Result<(), String> {
    let dir = project
        .pyproject_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            format!(
                "{} has no directory to open",
                project.pyproject_path.display()
            )
        })?;
    bombadil_core::folder::open(os, dir, runner).map_err(|err| err.to_string())
}

/// Runs Open folder off the render thread. `VENV_PROBE_TIMEOUT`, like
/// `open_terminal`: this only launches a file manager and returns, so a wait
/// measured in seconds already means something is wrong.
async fn open_folder(
    os: Os,
    project: Project,
    runner: Arc<dyn CommandRunner>,
) -> Result<(), String> {
    job::attempt(
        move || run_open_folder(os, &project, runner.as_ref()),
        VENV_PROBE_TIMEOUT,
        "opening the folder",
    )
    .await
}

/// The blocking half of persisting a config, with nothing else to do: just a
/// save. Its own function, mirroring `run_confirm_add_project`, so a test can
/// exercise it directly with a `FakeConfigPersist` instead of through
/// `update`'s opaque `Task`. Shared by every caller that only needs a plain
/// save -- a removal, and the plain (non-secret-toggling) env var edits.
fn run_persist(config: &Config, persist: &dyn ConfigPersist) -> bool {
    persist.save(config).is_ok()
}

/// The blocking half of validating a candidate `uv_source: Custom { path }`
/// (criterion 2): resolves it through the same `resolve_uv_for` every other
/// uv call in this file uses, against a config whose `uv_source` has been
/// swapped for the candidate -- so a version below `MIN_UV_VERSION` comes
/// back as `UvResolveError::TooOld`'s own text, the same as it would for a
/// real sync, and nothing here re-derives that check.
///
/// Its own function, mirroring `list_interpreters_blocking`, so a test can
/// call it directly with a `FakeCommandRunner` instead of only through the
/// `Task` `SettingsUvCustomValidateRequested` returns.
fn validate_uv_source_blocking(
    path: PathBuf,
    mut config: Config,
    runner: &dyn CommandRunner,
) -> Result<uv::ResolvedUv, String> {
    config.settings.uv_source = UvSource::Custom { path };
    resolve_uv_for(&config, runner)
}

/// Runs `validate_uv_source_blocking` off the render thread via `job::run` --
/// criterion 2's "through `job::run`": probing a uv binary spawns it, the
/// same reason `sync_project` and `list_interpreters` do the same.
async fn validate_uv_source(
    path: PathBuf,
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> Result<uv::ResolvedUv, String> {
    job::attempt(
        move || validate_uv_source_blocking(path, config, runner.as_ref()),
        SYNC_TIMEOUT,
        "validating uv",
    )
    .await
}

/// Runs off the render thread via `job::run`: persisting the config is a
/// blocking file write, same as every other `ConfigPersist` call in this
/// file.
async fn persist_config(config: Config, persist: Arc<dyn ConfigPersist>) -> bool {
    match job::run(move || run_persist(&config, persist.as_ref()), SYNC_TIMEOUT).await {
        job::JobResult::Done(success) => success,
        job::JobResult::TimedOut | job::JobResult::Failed => false,
    }
}

/// What a secret toggle owes the keychain, and the `Config` that must reach
/// disk alongside it.
///
/// `candidate` is a *copy* of `app.config` with the edit already applied.
/// Nothing in `App` has been changed to produce it.
#[derive(Debug)]
enum SecretToggle {
    /// The row is becoming secret: write `plain_value` to the keychain, then
    /// persist `candidate`, in which that value is already blank.
    Mark {
        candidate: Config,
        plain_value: String,
    },
    /// The row is no longer secret: delete the keychain entry, then persist.
    Unmark { candidate: Config },
}

/// Works out what toggling `index` in `scope` would do, without doing any of
/// it. `None` when there is no such row.
///
/// This is the one edit in this file that cannot follow the usual "in-memory
/// first, synchronously" discipline, and the reason is that marking a row
/// secret does not *change* a value -- it destroys one. `take_secret_value`
/// blanks the row and yields the only remaining copy of the plaintext, which
/// then goes into the background job. Applied to `app.config` up front, a
/// keychain that refuses the write leaves the plaintext gone with the job,
/// the row empty and flagged secret, and the next unrelated env edit
/// persisting that -- taking the plaintext off disk too. A status line saying
/// "failed" does not give it back.
///
/// So the edit is made here, on a copy. `app.config` is caught up in
/// `EnvVarSecretWritten` once the write has actually landed. That the copy is
/// what gets persisted is the point rather than a side effect: the config
/// reaching disk has to be the blanked one, or the plaintext this whole
/// exchange exists to move into the keychain stays sitting in `config.toml`.
fn plan_secret_toggle(
    config: &Config,
    drafts: &env_editor::ValueDrafts,
    scope: env_editor::EnvScope,
    index: usize,
) -> Option<SecretToggle> {
    let mut candidate = config.clone();
    // A copy too: `take_secret_value` consumes the row's draft, and the real
    // one must survive a failed write -- it is where the typed value still is.
    let mut drafts = drafts.clone();
    let plain_value = {
        let vars = env_editor::vars_mut(&mut candidate, scope)?;
        // Takes the plaintext from the row's draft -- where, on this tab's own
        // workflow, it is the only copy, `Config` never having seen it -- or
        // from `Config` for a value committed earlier.
        match env_editor::take_secret_value(vars, &mut drafts, scope, index) {
            Some(value) => Some(value),
            None if env_editor::unmark_secret(vars, index) => None,
            None => return None,
        }
    };
    Some(match plain_value {
        Some(plain_value) => SecretToggle::Mark {
            candidate,
            plain_value,
        },
        None => SecretToggle::Unmark { candidate },
    })
}

/// The blocking half of marking an env var secret (criterion 2): write the
/// plaintext to the keychain, then persist the config -- which by the time
/// this runs already has the value blanked in memory by
/// `env_editor::mark_secret`. Both under one `job::run` call so a keychain
/// write can never land while the on-disk config still disagrees with it.
///
/// `store` is a parameter so a test can hand in an `InMemorySecretStore` and
/// assert both halves of criterion 2 -- the store holds the value, and
/// `persist`'s recorded config does not; the real caller (`mark_secret_job`)
/// supplies the keychain.
fn run_secret_write(
    key: &SecretKey,
    plain_value: &str,
    config: &Config,
    store: &dyn SecretStore,
    persist: &dyn ConfigPersist,
) -> bool {
    store.set(key, plain_value).is_ok() && persist.save(config).is_ok()
}

/// The blocking half of unmarking an env var secret (criterion 3): delete the
/// keychain entry, then persist. Same pairing discipline as
/// `run_secret_write`, and the same reason: an orphaned keychain entry (a
/// delete that never happened) or a config that still claims secret (a
/// persist that never happened) are each exactly the bug the two criteria
/// exist to catch.
fn run_secret_delete(
    key: &SecretKey,
    config: &Config,
    store: &dyn SecretStore,
    persist: &dyn ConfigPersist,
) -> bool {
    store.delete(key).is_ok() && persist.save(config).is_ok()
}

/// Runs off the render thread via `job::run`. `KeyringSecretStore` is
/// constructed here, on the job thread, never taken as a parameter of
/// `update` -- the same discipline `sync_project` and every other keychain
/// call in this file follow, so no test can reach the real keychain through
/// it.
///
/// Shared verbatim by `IndexCredentialSaveRequested` (Task 4): writing a
/// plaintext value under some `SecretKey` and persisting the config that no
/// longer carries it is exactly the same pairing an index credential save
/// needs, and index credentials have no analogue of `mark_secret`'s
/// in-memory half to call first -- `IndexAuth` never held the plaintext to
/// begin with, so there is nothing to blank.
async fn mark_secret_job(
    key: SecretKey,
    plain_value: String,
    config: Config,
    persist: Arc<dyn ConfigPersist>,
) -> bool {
    match job::run(
        move || {
            run_secret_write(
                &key,
                &plain_value,
                &config,
                &KeyringSecretStore::new(),
                persist.as_ref(),
            )
        },
        SYNC_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(success) => success,
        job::JobResult::TimedOut | job::JobResult::Failed => false,
    }
}

/// Runs off the render thread via `job::run`. Same discipline as
/// `mark_secret_job`.
async fn unmark_secret_job(
    key: SecretKey,
    config: Config,
    persist: Arc<dyn ConfigPersist>,
) -> bool {
    match job::run(
        move || run_secret_delete(&key, &config, &KeyringSecretStore::new(), persist.as_ref()),
        SYNC_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(success) => success,
        job::JobResult::TimedOut | job::JobResult::Failed => false,
    }
}

pub fn view(app: &App) -> iced::Element<'_, Message> {
    // The running command's label: every button that would start another
    // one is disabled and says it is waiting for this.
    let waiting = app
        .busy
        .as_ref()
        .map(|b| b.label(project_label(app, b.project_id), app.now));
    let waiting = waiting.as_deref();
    // The index is carried through rather than dropped: the header's Sync
    // button names the project it acts on (`Message::SyncRequested`), so the
    // selected index has to survive as far as the button.
    let selected = app
        .selected
        .and_then(|i| app.config.projects.get(i).map(|project| (i, project)));
    let detail: iced::Element<'_, Message> = match selected {
        // The probe answers from the already-probed `entries` cache rather
        // than touching disk again on every frame -- `boot`'s background
        // tasks are what keep that cache current.
        Some((i, project)) => {
            let entry = app.entries.get(i);
            // The active environment's version: the header describes what
            // Sync and Open terminal would act on.
            // The active environment's resolved path, from the entries cache
            // the sidebar already built -- not a fresh `venv_path::resolve` on
            // every frame.
            let venv_path = entry.and_then(|entry| {
                entry
                    .environments
                    .iter()
                    .find(|row| row.active)
                    .map(|row| row.path.clone())
            });
            let mut header = detail::header(project, venv_path, &|_| {
                entry.and_then(|entry| {
                    entry
                        .environments
                        .iter()
                        .find(|row| row.active)
                        .and_then(|row| row.python_version.clone())
                })
            });
            header.git = entry.and_then(|entry| entry.git.clone());
            let busy = app.busy.as_ref().map(|b| {
                (
                    b.label(project_label(app, b.project_id), app.now),
                    crate::busy::spin(b.started, app.now),
                    b.project_id == project.id,
                )
            });
            let tiles = summary_tiles(app, i, &header, entry);
            detail::view(&header, i, busy, tiles)
        }
        None => iced::widget::text("").into(),
    };

    // Exactly one pane, chosen by `app.tab` (spec §8). This used to be all
    // seven concatenated into one column -- see `Tab`'s own doc for what that
    // did to the confirmations sitting below them.
    let pane: iced::Element<'_, Message> = match app.tab {
        // Only rendered once its index still matches the current selection --
        // `Message::DepsLoaded` guards the same way when it lands.
        Tab::Dependencies => match &app.deps {
            Some((i, tab)) if Some(*i) == app.selected => deps::view(
                tab,
                app.dep_edit.as_ref().filter(|edit| edit.project == *i),
                app.deps_view,
                &app.dep_filter,
                *i,
                waiting,
                app.entries.get(*i).is_some_and(|entry| {
                    entry
                        .environments
                        .iter()
                        .find(|row| row.active)
                        .and_then(|row| row.python_version.as_ref())
                        .is_none()
                }),
                app.outdated
                    .as_ref()
                    .filter(|(j, _)| j == i)
                    .and_then(|(_, r)| r.as_ref().ok())
                    .map_or(&[][..], Vec::as_slice),
                &deps_extras(app, *i, tab),
            ),
            _ => iced::widget::text("").into(),
        },
        // Rebuilt fresh from `app.config` on every render, unlike the
        // Dependencies tab above: a member toggle changes
        // `app.config.projects[i].members` without a new fetch, and this is
        // what keeps the checklist reflecting it immediately rather than
        // waiting on `MembersLoaded` to land again -- the same "recompute,
        // don't cache" trick `banner::problems` and
        // `add_project::resolved_venv_path_display` use for the same reason.
        Tab::Members => match &app.members {
            Some((i, fetch)) if Some(*i) == app.selected => match app.config.projects.get(*i) {
                Some(project) => {
                    let project_dir = project
                        .pyproject_path
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_default();
                    let tab = members_editor::build(
                        &project_dir,
                        &project.members,
                        &fetch.kind,
                        &fetch.discovered,
                        &fetch.installed,
                    );
                    members_editor::view(&tab, *i, waiting)
                }
                None => iced::widget::text("").into(),
            },
            _ => iced::widget::text("").into(),
        },
        // This project's own variables, and which global ones they shadow.
        // The global list itself is Settings > Environment variables.
        Tab::EnvVars => match app.selected {
            Some(i) => env_editor::project_view(&app.config, i, &app.env_value_drafts),
            None => iced::widget::text("").into(),
        },
        // Global scripts always shown, the project's own beneath them.
        Tab::Scripts => scripts_editor::view(&app.config, app.selected),
        // Which of the globally defined indexes this project resolves
        // against. Defining them is Settings > Package indexes and auth.
        Tab::Indexes => match app.selected {
            Some(i) => index_editor::project_view(&app.config, i),
            None => iced::widget::text("").into(),
        },
        // This project's open pull request and its checks, when there is one.
        Tab::Run => match app
            .selected
            .and_then(|i| app.config.projects.get(i).map(|p| (i, p)))
        {
            Some((i, project)) => {
                let names = match &app.entry_points {
                    Some((id, names)) if *id == project.id => names.as_slice(),
                    _ => &[],
                };
                run_tab::view(
                    project,
                    i,
                    names,
                    app.runs.get(&project.id),
                    &app.command_draft,
                    run_blocked(app, i),
                    app.now,
                    run_tab::JustView {
                        state: app
                            .just
                            .as_ref()
                            .filter(|(id, _)| *id == project.id)
                            .map(|(_, s)| s),
                        args: &app.just_args,
                        shown: app.just_shown.as_ref(),
                    },
                )
            }
            None => iced::widget::text("").into(),
        },
        Tab::GitHub => match app.selected.and_then(|i| app.config.projects.get(i)) {
            Some(project) => crate::github_tab::view(
                project.id,
                app.github.get(&project.id),
                app.config.settings.watch_github,
                app.github_in_flight.contains(&project.id),
            ),
            None => iced::widget::text("").into(),
        },
        Tab::Doctor => match app
            .selected
            .and_then(|i| app.config.projects.get(i).map(|p| (i, p.id)))
        {
            Some((i, id)) => {
                let pinned: Vec<String> = match &app.deps {
                    Some((j, Ok(tab))) if *j == i => tab
                        .groups
                        .iter()
                        .flat_map(|g| &g.rows)
                        .filter(|r| {
                            r.constraint
                                .as_deref()
                                .is_some_and(|c| c.trim().starts_with("=="))
                        })
                        .map(|r| deps::normalise(&r.name))
                        .collect(),
                    _ => Vec::new(),
                };
                crate::doctor_tab::view(
                    i,
                    id,
                    app.last_failure.get(&id).map(|(_, d)| d),
                    app.outdated
                        .as_ref()
                        .filter(|(j, _)| *j == i)
                        .map(|(_, r)| r),
                    &pinned,
                    waiting.is_none(),
                )
            }
            None => iced::widget::text("").into(),
        },
    };

    // The current tab's own button is disabled rather than hidden -- the
    // same "no press needed, no press possible" treatment `index_editor`'s
    // auth buttons and `scripts_editor`'s move buttons give a control whose
    // action is already the state.
    //
    // A single row of words; the selected one in `parchment` with a
    // `jacket` underline beneath it, the rest in `slate` -- no boxes, no
    // pills, no background fill. Each button is styled with no background
    // and no border to strip the default button chrome down to bare text;
    // the underline is a `Fill`-width `container` holding fixed-height
    // `Space`, coloured `jacket` only when selected. That indicator is
    // deliberately the tab bar's only affordance, consistent with the state
    // glyph column being the application's one symbolic vocabulary (see
    // `theme`'s module doc).
    //
    // The column MUST be built with `Column::from_vec`, not `column![]`.
    // `column![]` goes through `Column::push`, which calls
    // `Length::enclose` on each child's `size_hint` -- and the underline's
    // hint is `Fill` (`Container::new` derives its width from its content
    // via `Length::fluid`), so `push` would promote the whole column to
    // `Fill`. A `Fill` column inside this `Fill` row makes iced's flex third
    // pass hand every tab an equal share of the bar's full width, and the
    // underline then spans that share instead of its label: measured at
    // 256pt under an 800pt bar for an 80pt label. `from_vec` deliberately
    // skips `enclose`, so the column stays `Shrink`, the row stays `Shrink`,
    // and the cross-axis pass sizes the underline to the label -- measured
    // at exactly the label's 80pt.

    let mut tab_bar = iced::widget::row![].spacing(theme::SPACE_3);
    for tab in tab_order(&app.config.settings.layout.tab_order) {
        let is_selected = tab == app.tab;
        let text_color = if is_selected {
            theme::Swatch::Parchment
        } else {
            theme::Swatch::Slate
        };

        // A press area, not a button: a button keeps the press from the
        // drag. Pressing selects, as the button did; dragging reorders.
        let label = iced::widget::mouse_area(
            iced::widget::container(
                iced::widget::text(tab.label())
                    .size(theme::TITLE)
                    .style(theme::tinted(text_color)),
            )
            .padding([theme::SPACE_1, theme::SPACE_2]),
        )
        .interaction(iced::mouse::Interaction::Pointer)
        .on_press(Message::TabDragStarted(tab))
        // Also here, not only in the drag subscription: a tap delivers the
        // press and release together, before that subscription exists.
        .on_release(Message::DragEnded)
        .on_enter(Message::TabDragOver(tab));
        let label: iced::Element<'_, Message> = match tab.term() {
            Some(term) => crate::glossary::tip(label, term),
            None => label.into(),
        };

        let underline = iced::widget::container(
            iced::widget::Space::new()
                .width(iced::Length::Fill)
                .height(theme::TAB_UNDERLINE),
        )
        .width(iced::Length::Fill)
        .style(move |t| {
            let style = iced::widget::container::Style::default();
            if is_selected {
                style.background(theme::palette(t).jacket)
            } else {
                style
            }
        });

        tab_bar = tab_bar.push(
            iced::widget::Column::from_vec(vec![label, underline.into()]).spacing(theme::SPACE_1),
        );
    }

    // Nothing selected means the tab bar names panes for a project that is
    // not there, and the detail header is an empty line where the largest
    // type on screen should be. Both are suppressed, and with no projects at
    // all the whole detail area becomes the one screen whose only content is
    // the action that gets the user out of it.
    let detail_area: iced::Element<'_, Message> = if app.config.projects.is_empty() {
        shell::empty_state()
    } else if selected.is_none() {
        iced::widget::center(
            iced::widget::text("Select a project on the left")
                .size(theme::BODY)
                .style(theme::text_muted),
        )
        .into()
    } else if let Some((i, project)) = selected
        && app.missing.contains(&project.id)
    {
        detail::missing_folder_view(project, i)
    } else if let Some((i, project)) = selected
        && project.environments.is_empty()
    {
        detail::no_environment_view(project, i)
    } else {
        iced::widget::column![
            detail,
            theme::hairline_row(),
            tab_bar,
            // Right padding keeps the scroll bar off the content's edge.
            iced::widget::scrollable(
                iced::widget::container(pane).padding(iced::Padding::ZERO.right(theme::SPACE_4))
            )
            .width(iced::Length::Fill)
            .height(iced::Length::Fill),
        ]
        .spacing(theme::SPACE_3)
        .into()
    };

    // The sidebar is a `bark` surface with its own hairline against the
    // detail ground beside it: it used to be the same `ink` as the pane it
    // sat next to, with nothing between them, so the two regions read as one
    // undivided canvas.
    let sidebar_header = iced::widget::row![
        iced::widget::text("Projects")
            .font(theme::FONT_PROSE_SEMIBOLD)
            .size(theme::LABEL)
            .style(theme::text_muted),
        iced::widget::Space::new().width(iced::Length::Fill),
        theme::tip(
            iced::widget::button(iced::widget::text("New").size(theme::LABEL))
                .on_press(Message::NewProjectRequested)
                .padding([theme::SPACE_1, theme::SPACE_2])
                .style(theme::button_secondary),
            "Create a new project",
            iced::widget::tooltip::Position::Bottom,
        ),
        // Labelled "Add", not a bare `+`: an icon-only control with no icon
        // set behind it is a glyph the user has to guess at.
        match waiting {
            None => theme::tip(
                iced::widget::button(iced::widget::text("Add").size(theme::LABEL))
                    .on_press(Message::AddProjectPickManifestRequested)
                    .padding([theme::SPACE_1, theme::SPACE_2])
                    .style(theme::button_secondary),
                format!(
                    "Add a project ({})",
                    crate::shortcuts::keys(crate::shortcuts::Shortcut::AddProject)
                ),
                iced::widget::tooltip::Position::Bottom,
            ),
            Some(_) => theme::wait_tip(
                iced::widget::button(iced::widget::text("Add").size(theme::LABEL))
                    .padding([theme::SPACE_1, theme::SPACE_2])
                    .style(theme::button_secondary),
                waiting,
            ),
        },
        crate::icons::icon_button(
            crate::icons::Icon::PanelLeft,
            format!(
                "Collapse sidebar ({})",
                crate::shortcuts::keys(crate::shortcuts::Shortcut::ToggleSidebar)
            ),
            Some(Message::SidebarToggled),
        ),
    ]
    .spacing(theme::SPACE_1)
    .align_y(iced::Alignment::Center);

    let dirs: Vec<PathBuf> = app
        .config
        .projects
        .iter()
        .map(|p| {
            p.pyproject_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default()
        })
        .collect();
    let shown = sidebar::visible(&app.entries, &dirs, &app.project_filter, app.selected);
    let running_ids: std::collections::HashSet<uuid::Uuid> = app
        .runs
        .iter()
        .filter(|(_, run)| run.live())
        .map(|(id, _)| *id)
        .collect();
    let sidebar_body: iced::Element<'_, Message> = if app.entries.is_empty() {
        iced::widget::text("None yet")
            .size(theme::DATA)
            .style(theme::text_muted)
            .into()
    } else {
        let findings: Vec<Vec<crate::health::Finding>> = (0..app.entries.len())
            .map(|i| project_findings(app, i))
            .collect();
        iced::widget::scrollable(sidebar::view(
            &running_ids,
            &app.entries,
            &findings,
            app.selected,
            app.busy
                .as_ref()
                .map(|b| (b.project_id, crate::busy::spin(b.started, app.now))),
            waiting,
            &shown,
        ))
        .height(iced::Length::Fill)
        .into()
    };

    let collapsed = app.config.settings.layout.sidebar_collapsed;
    let sidebar_panel = if collapsed {
        iced::widget::container(sidebar::rail(
            &app.entries,
            app.selected,
            app.busy
                .as_ref()
                .map(|b| (b.project_id, crate::busy::spin(b.started, app.now))),
            // Every project: the rail has no field to show a filter in.
            &(0..app.entries.len()).collect::<Vec<_>>(),
        ))
        .padding([theme::SPACE_2, theme::SPACE_1])
    } else {
        iced::widget::container(
            iced::widget::column![sidebar_header]
                // Only once there is enough to need it.
                // Shown with two or more projects -- and whenever it holds
                // text, so a filter can always be seen and cleared.
                .push(
                    (app.entries.len() > 1 || !app.project_filter.is_empty()).then(|| {
                        iced::widget::text_input("Filter projects", &app.project_filter)
                            .style(crate::theme::text_field)
                            .on_input(Message::ProjectFilterChanged)
                            .size(theme::DATA)
                    }),
                )
                .push(sidebar_body)
                .push(shell::sponsor_button())
                .spacing(theme::SPACE_2),
        )
        .padding(theme::SPACE_2)
    }
    .width(sidebar_width(app))
    .height(iced::Length::Fill)
    .style(theme::surface);

    // The edge between the panes is the handle: a hairline drawn in a strip
    // wide enough to grab.
    let handle: iced::Element<'_, Message> = if collapsed {
        theme::hairline_column()
    } else {
        iced::widget::mouse_area(
            iced::widget::container(theme::hairline_column())
                .width(theme::HANDLE_WIDTH)
                .height(iced::Length::Fill)
                .align_x(iced::alignment::Horizontal::Center),
        )
        .interaction(iced::mouse::Interaction::ResizingHorizontally)
        .on_press(Message::SidebarDragStarted)
        .on_release(Message::DragEnded)
        .into()
    };

    let body = iced::widget::row![
        sidebar_panel,
        handle,
        iced::widget::container(detail_area)
            .width(iced::Length::Fill)
            .height(iced::Length::Fill)
            .padding(theme::SPACE_3),
    ]
    .height(iced::Length::Fill);

    // Recomputed fresh on every render from `app.add_project`'s current
    // `venv_choice`, the same trick `banner::problems` uses to stay live --
    // this is what makes the resolved path track a venv choice the user just
    // changed, per spec §8, rather than a value cached from when the
    // manifest was first picked.
    let add_project_dialog: iced::Element<'_, Message> = match &app.add_project {
        Some(draft) => {
            let resolved = add_project::resolved_venv_path_display(draft, &app.config.settings);
            add_project::view(draft, &resolved, waiting)
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
            let label = app
                .config
                .projects
                .get(menu.project_index)
                .map(|project| project.label.as_str())
                .unwrap_or_default();
            let has_environment = app
                .config
                .projects
                .get(menu.project_index)
                .is_some_and(|project| !project.environments.is_empty());
            context_menu::view(menu, label, default, waiting, has_environment)
        }
        None => iced::widget::text("").into(),
    };

    // The Recreate venv confirmation (spec §8). Rendered from the resolved
    // path captured when it opened, not re-resolved here: the sentence the
    // user agrees to and the path that gets deleted must be the same one.
    let confirm_recreate_view: iced::Element<'_, Message> = match &app.confirm_recreate {
        Some(confirm) => context_menu::confirm_view(confirm),
        None => iced::widget::text("").into(),
    };

    // The pin-change confirmation, rendered from the versions captured when
    // it opened -- same freshness argument as `confirm_recreate_view`.
    let confirm_pin_view: iced::Element<'_, Message> = match &app.confirm_pin_change {
        Some(confirm) => context_menu::confirm_pin_view(confirm),
        None => iced::widget::text("").into(),
    };

    // The pending "sync would remove members" confirmation (spec §5a
    // criterion 4), rendered from the warning captured when it opened --
    // same freshness argument as `confirm_recreate_view`.
    let confirm_member_sync_view: iced::Element<'_, Message> = match &app.confirm_member_sync {
        Some(confirm) => members_editor::confirm_view(confirm),
        None => iced::widget::text("").into(),
    };

    let mut problems = app.load_problems.clone();
    problems.extend(banner::problems(&app.config));

    // The drawer is pinned below the body rather than stacked inside it, with
    // a hairline above: a running command's transcript is the one thing that
    // must not move when the pane above it scrolls.
    // Above an open transcript, the doctor's reading of it and its fixes.
    let doctor_header: Option<iced::Element<'_, Message>> =
        app.selected.filter(|_| app.drawer.expanded).and_then(|i| {
            let project_id = app.config.projects.get(i)?.id;
            let (_, d) = app.last_failure.get(&project_id)?;
            Some(
                iced::widget::container(
                    iced::widget::row![
                        iced::widget::text(d.sentence.clone()).size(theme::BODY),
                        crate::doctor_tab::fix_buttons(project_id, &d.fixes, waiting.is_none()),
                    ]
                    .spacing(theme::SPACE_3)
                    .align_y(iced::Alignment::Center),
                )
                .padding([theme::SPACE_2, theme::SPACE_3])
                .into(),
            )
        });
    let mut frame = iced::widget::column![
        shell::topbar(app.open_menu),
        banner::view(&problems),
        body,
        theme::hairline_row(),
    ];
    if let Some(header) = doctor_header {
        frame = frame.push(header);
    }
    let frame = frame
        .push(drawer::view(&app.drawer, waiting.map(str::to_string)))
        .height(iced::Length::Fill);

    // Layered, not appended. Every one of these used to be a row in this
    // column, so opening any of them shoved the whole application down the
    // window -- and with a real dependency tree behind it, a confirmation
    // could land thousands of pixels below the viewport in a column that did
    // not scroll. Each layer is opaque, so a press reaches the top one only:
    // without that, one click could both dismiss a popup and activate
    // whatever happened to be under it.
    //
    // At most one of the four dialogs is open at a time -- `update` never
    // opens a second over a first -- but they are pushed unconditionally in a
    // fixed order so that if that ever stops being true the result is a
    // predictable stack rather than whichever `match` arm ran last.
    let mut layers = iced::widget::stack![frame];
    if app.add_project.is_some() {
        layers = layers.push(shell::overlay_layer(add_project_dialog));
    }
    if app.context_menu.is_some() {
        // Anchored beside the sidebar, not centred over a scrim: a right-click
        // menu belongs next to what it acts on.
        layers = layers.push(shell::anchored_layer(
            context_menu_view,
            sidebar_width(app),
            Message::ContextMenuClosed,
        ));
    }
    if let Some(label) = app
        .confirm_upgrade_all
        .and_then(|id| app.config.projects.iter().find(|p| p.id == id))
        .map(|p| p.label.clone())
    {
        layers = layers.push(shell::overlay_layer(deps::confirm_upgrade_all_view(&label)));
    }
    if let Some(manifests) = &app.project_chooser {
        layers = layers.push(shell::overlay_layer(add_project::chooser_view(manifests)));
    }
    if let Some(draft) = &app.add_local {
        let to = app
            .config
            .projects
            .iter()
            .find(|p| p.id == draft.project)
            .map(|p| p.label.as_str())
            .unwrap_or("");
        layers = layers.push(shell::overlay_layer(crate::add_local::view(
            draft, to, waiting,
        )));
    }
    if app.confirm_recreate.is_some() {
        layers = layers.push(shell::overlay_layer(confirm_recreate_view));
    }
    if app.confirm_pin_change.is_some() {
        layers = layers.push(shell::overlay_layer(confirm_pin_view));
    }
    if let Some(confirm) = &app.confirm_environment_removal {
        layers = layers.push(shell::overlay_layer(
            context_menu::confirm_environment_removal_view(confirm),
        ));
    }
    if app.confirm_member_sync.is_some() {
        layers = layers.push(shell::overlay_layer(confirm_member_sync_view));
    }
    if let Some((query, index)) = &app.find {
        layers = layers.push(shell::overlay_layer(crate::find_package::view(
            query,
            index.as_deref(),
        )));
    }
    if let Some(draft) = &app.new_project {
        layers = layers.push(shell::overlay_layer(crate::new_project::view(
            draft,
            app.installed_pythons.as_deref().unwrap_or(&[]),
            app.creating_project,
        )));
    }
    if let Some((id, _)) = &app.confirm_stop {
        let label = project_label(app, *id).unwrap_or("This project");
        layers = layers.push(shell::overlay_layer(run_tab::confirm_view(
            format!("{label} is running. Stop the run and continue?"),
            "Stop and continue",
            Message::StopThenConfirmed,
            Message::StopThenCancelled,
        )));
    }
    if app.confirm_quit {
        let live = app.runs.values().filter(|r| r.live()).count();
        let text = match live {
            1 => "A program is still running. Stop it and quit?".to_string(),
            n => format!("{n} programs are still running. Stop them and quit?"),
        };
        layers = layers.push(shell::overlay_layer(run_tab::confirm_view(
            text,
            "Stop and quit",
            Message::QuitConfirmed,
            Message::QuitCancelled,
        )));
    }
    if !app.toasts.is_empty() {
        layers = layers.push(toast_layer(&app.toasts));
    }
    if let Some(menu) = app.open_menu {
        layers = layers.push(shell::menu_layer(menu));
    }
    if let Some(state) = app.preferences {
        // `resolve_scope` here, at render, rather than stored: recomputing it
        // every frame is the same "recompute, don't cache" discipline
        // `banner::problems` follows, and it is what makes a project removed
        // behind this surface take effect immediately.
        let scope = preferences::resolve_scope(state.scope, &app.config);
        layers = layers.push(preferences::view(
            state.section,
            scope,
            &app.config,
            &app.env_value_drafts,
            &app.index_credential_drafts,
            &app.uv_custom_path_draft,
            app.uv_custom_validation.as_ref(),
            app.pycharm_detected.as_ref(),
            &app.github_token_draft,
            app.github_source.as_ref(),
            app.github_accounts.as_ref(),
            app.github_login.as_ref(),
            &app.backup,
            &app.disk,
            app.busy.is_none(),
        ));
    }
    if let Some(modal) = app.modal {
        layers = layers.push(shell::modal_layer(modal));
    }
    layers.into()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use bombadil_core::model::Config;
    use bombadil_core::process::FakeCommandRunner;
    use std::path::Path;

    /// Marks a project's ACTIVE environment as probed at `version`.
    ///
    /// A helper rather than an inline field write: the sidebar carries one
    /// row per environment now, and a test that reached for "the project's
    /// version" would have to pick one -- which is exactly the ambiguity the
    /// rows exist to remove.
    fn set_probed_version(app: &mut App, project: usize, version: &str) {
        let row = app.entries[project]
            .environments
            .iter_mut()
            .find(|row| row.active)
            .expect("the project must have an active environment to probe");
        row.python_version = Some(version.to_string());
    }

    /// The selected project's label. A test helper rather than a method on
    /// `App`: nothing in the real code path resolves a project from the
    /// selection alone any more -- `view` carries the index alongside the
    /// project, because the header's Sync button has to name what it acts on.
    fn selected_label(app: &App) -> Option<&str> {
        app.selected
            .and_then(|i| app.config.projects.get(i))
            .map(|p| p.label.as_str())
    }

    /// The first terminal kind the *host* OS supports.
    ///
    /// `run_open_terminal` builds argv for `Os::host()`, and `argv::build`
    /// correctly refuses a kind belonging to another OS -- so a test pairing
    /// a hardcoded Linux kind with the host either fails outright on macOS
    /// and Windows or, worse, passes off the wrong-OS guard instead of the
    /// behaviour it claims to cover. `Os` is a parameter everywhere in
    /// `terminal`, never a `cfg!`, precisely so this is derivable rather
    /// than something to compile the test away over.
    fn host_terminal_kind() -> TerminalKind {
        *TerminalKind::for_os(Os::host())
            .first()
            .expect("every supported OS has at least one terminal kind")
    }

    /// A runner for tests that don't care about it -- most of them, since
    /// most messages here never touch a `CommandRunner` at all.
    fn unused_runner() -> Arc<dyn CommandRunner> {
        Arc::new(FakeCommandRunner::new())
    }

    #[test]
    fn exporting_the_drawer_writes_the_transcript_and_says_where() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let written = write_transcript(path.clone(), "a\nb\n").unwrap();
        assert_eq!(written, path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nb\n");

        let mut app = App::new(Config::default());
        let _ = update(
            &mut app,
            Message::DrawerExportFinished(Ok(path.clone())),
            unused_runner(),
        );
        assert_eq!(app.toasts[0].kind, crate::busy::ToastKind::Success);
        assert!(app.toasts[0].text.contains("out.txt"));
    }

    #[test]
    fn a_failed_export_is_a_warning_that_stays() {
        let dir = tempfile::tempdir().unwrap();
        // A directory, so the write fails.
        let error = write_transcript(dir.path().to_path_buf(), "x").unwrap_err();
        let mut app = App::new(Config::default());
        let _ = update(
            &mut app,
            Message::DrawerExportFinished(Err(error)),
            unused_runner(),
        );
        assert_eq!(app.toasts[0].kind, crate::busy::ToastKind::Warning);
        assert!(app.toasts[0].text.starts_with("Could not save output"));
    }

    #[test]
    fn an_empty_config_produces_no_selection() {
        // Selecting a project that does not exist would panic in `view` on the
        // first frame, so the empty case has to be representable.
        let app = App::new(Config::default());
        assert!(app.selected.is_none());
        assert!(app.config.projects.is_empty());
    }

    #[test]
    fn the_first_project_is_selected_on_load() {
        let config = Config {
            projects: vec![project("api"), project("web")],
            ..Config::default()
        };
        let app = App::new(config);
        assert_eq!(selected_label(&app), Some("api"));
    }

    #[test]
    fn selecting_a_project_changes_the_selection() {
        let config = Config {
            projects: vec![project("api"), project("web")],
            ..Config::default()
        };
        let mut app = App::new(config);
        let _ = update(&mut app, Message::ProjectSelected(1), unused_runner());
        assert_eq!(selected_label(&app), Some("web"));
    }

    // --- the tab container (spec §8) ---

    #[test]
    fn the_detail_area_starts_on_the_dependencies_tab() {
        assert_eq!(App::new(Config::default()).tab, Tab::Dependencies);
    }

    #[test]
    fn selecting_a_tab_switches_which_pane_renders() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let task = update(
            &mut app,
            Message::TabSelected(Tab::Scripts),
            unused_runner(),
        );

        assert_eq!(app.tab, Tab::Scripts);
        assert_eq!(
            task.units(),
            0,
            "switching tabs is pure state -- nothing to fetch, nothing to persist"
        );
    }

    #[test]
    fn every_tab_renders_with_both_confirmations_open() {
        // The container nobody owned: `view` concatenated all seven panes and
        // then stacked the dialog, the context menu and both confirmations
        // below them, in a column with no `scrollable` anywhere. This does not
        // prove the layout scrolls -- no test here can drive a renderer -- but
        // it does exercise every arm of the new `match app.tab` with the
        // overlays present, which is where a pane and an overlay could
        // otherwise disagree about what they may borrow.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            indexes: vec![index_with_auth(
                "corp",
                bombadil_core::model::IndexAuth::Token,
            )],
            ..Config::default()
        });
        app.deps = Some((0, Ok(deps::build(&PyProject::default(), &[], no_tree()))));
        app.members = Some((
            0,
            MembersFetch {
                kind: bombadil_core::members::WorkspaceKind::Loose,
                discovered: vec![],
                installed: vec![],
            },
        ));
        app.confirm_recreate = Some(context_menu::ConfirmRecreate {
            project_id: app.config.projects[0].id,
            label: "api".to_string(),
            venv_path: PathBuf::from("/p/api/.venv"),
        });
        app.confirm_member_sync = Some(members_editor::ConfirmMemberSync {
            project_index: 0,
            label: "api".to_string(),
            warning: "would remove alpha".to_string(),
        });
        app.context_menu = Some(context_menu::ContextMenu {
            project_index: 0,
            terminals: vec![host_terminal_kind()],
        });

        for tab in Tab::ALL {
            app.tab = tab;
            let _ = view(&app);
        }
    }

    // --- Task 1: the Dependencies tab ---

    #[test]
    fn selecting_a_project_fetches_its_dependencies_through_job_run_not_inline() {
        // Criterion 5: `uv tree` on a large project is slow, so this must
        // never run inside `update` itself -- the same "nothing recorded
        // synchronously, but real work dispatched" proof `SyncRequested` and
        // `OpenFolderRequested` use.
        let config = Config {
            projects: vec![project("api")],
            ..Config::default()
        };
        let mut app = App::new(config);
        let fake = Arc::new(FakeCommandRunner::new());

        let task = update(&mut app, Message::ProjectSelected(0), fake.clone());

        assert!(
            fake.calls().is_empty(),
            "the dependency fetch must not call the runner synchronously inside update; got {:?}",
            fake.calls()
        );
        assert!(
            task.units() > 0,
            "selecting a project must dispatch a real background fetch, not Task::none()"
        );
    }

    fn with_theme(choice: bombadil_core::model::ThemeChoice) -> App {
        let mut config = Config::default();
        config.settings.theme = choice;
        App::new(config)
    }

    #[test]
    fn system_follows_what_the_os_last_reported() {
        let mut app = with_theme(bombadil_core::model::ThemeChoice::System);
        assert_eq!(
            app.theme_mode(),
            iced::theme::Mode::Dark,
            "dark until the OS answers"
        );
        let _ = update(
            &mut app,
            Message::SystemThemeChanged(iced::theme::Mode::Light),
            unused_runner(),
        );
        assert_eq!(app.theme_mode(), iced::theme::Mode::Light);
    }

    #[test]
    fn an_explicit_choice_ignores_the_os() {
        let mut app = with_theme(bombadil_core::model::ThemeChoice::Light);
        let _ = update(
            &mut app,
            Message::SystemThemeChanged(iced::theme::Mode::Dark),
            unused_runner(),
        );
        assert_eq!(app.theme_mode(), iced::theme::Mode::Light);
        let mut app = with_theme(bombadil_core::model::ThemeChoice::Dark);
        let _ = update(
            &mut app,
            Message::SystemThemeChanged(iced::theme::Mode::Light),
            unused_runner(),
        );
        assert_eq!(app.theme_mode(), iced::theme::Mode::Dark);
    }

    #[test]
    fn no_system_preference_reads_as_light() {
        // Stock GNOME reports "no preference" (`Mode::None`) when its
        // dark-style switch is off, which is the light desktop. Reading it as
        // "keep what we had" left System users on dark there forever.
        let mut app = with_theme(bombadil_core::model::ThemeChoice::System);
        let _ = update(
            &mut app,
            Message::SystemThemeChanged(iced::theme::Mode::Dark),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::SystemThemeChanged(iced::theme::Mode::None),
            unused_runner(),
        );
        assert_eq!(app.theme_mode(), iced::theme::Mode::Light);
    }

    #[test]
    fn choosing_an_appearance_applies_and_persists_it() {
        let mut app = App::new(Config::default());
        let task = update(
            &mut app,
            Message::ThemeChoiceSelected(bombadil_core::model::ThemeChoice::Light),
            unused_runner(),
        );
        assert_eq!(
            app.config.settings.theme,
            bombadil_core::model::ThemeChoice::Light
        );
        assert_eq!(app.theme_mode(), iced::theme::Mode::Light);
        assert!(
            task.units() > 0,
            "the choice must be persisted, not just applied"
        );
    }

    #[test]
    fn boot_asks_the_os_for_its_theme_even_with_no_projects() {
        // With nothing to probe, the theme query is the only work boot has;
        // without it a System user sees dark until the OS next changes.
        let (_app, task) = boot(Loaded::Fresh(Config::default()));
        assert!(task.units() > 0, "boot must ask the OS for its appearance");
    }

    #[test]
    fn booting_with_a_selected_project_also_fetches_its_dependencies_without_blocking() {
        // `boot` has no `CommandRunner` to fetch with directly; it must
        // re-dispatch through `ProjectSelected` rather than skip the fetch
        // for whichever project starts selected.
        let (_app, task) = boot(Loaded::Fresh(Config {
            projects: vec![project("api")],
            ..Config::default()
        }));
        assert!(
            task.units() > 0,
            "boot must dispatch work for the initially selected project's dependencies"
        );
    }

    /// A tree result for a test that is not about the tree. `Err` rather than
    /// a hand-built graph: what it holds does not matter here, and building a
    /// `DependencyGraph` by hand would only obscure what does.
    fn no_tree() -> Result<TreeOutcome, String> {
        Err("this test is not about the tree".to_string())
    }

    #[test]
    fn a_failed_deps_fetch_is_kept_as_the_failure_it_is() {
        // The defect: a fetch that could not ask the environment anything
        // landed as an empty `installed` list, so every declared package
        // rendered `"(not installed)"` -- a positive claim about the venv, and
        // indistinguishable from one that really is empty. The failure has to
        // survive as far as the tab.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::DepsLoaded(0, Err("No virtual environment found".to_string())),
            unused_runner(),
        );

        let (index, fetched) = app
            .deps
            .as_ref()
            .expect("a failed fetch must still land, or the tab stays blank forever");
        assert_eq!(*index, 0);
        let cause = fetched
            .as_ref()
            .expect_err("the failure must not have been flattened into a tab");
        assert_eq!(cause, "No virtual environment found");
        assert!(
            deps::unavailable_message(cause).contains("No virtual environment found"),
            "what the tab renders must name the cause"
        );
    }

    #[test]
    fn an_unresolvable_uv_fails_the_deps_fetch_with_its_own_message() {
        // The producer half. A `Custom` pin the fake runner has no rule for
        // fails to probe -- `uv::resolve`'s actionable text is what has to
        // survive, the same discipline `list_interpreters_blocking` follows.
        let config = Config {
            settings: bombadil_core::model::Settings {
                uv_source: bombadil_core::model::UvSource::Custom {
                    path: PathBuf::from("/definitely/not/a/real/uv"),
                },
                ..Default::default()
            },
            ..Config::default()
        };

        let got = fetch_deps_blocking(&project("api"), &config, &FakeCommandRunner::new());

        let err = got.expect_err("an unresolvable uv must not look like an empty environment");
        assert!(
            err.contains("/definitely/not/a/real/uv"),
            "the resolve error's own text must survive; got {err}"
        );
    }

    #[test]
    fn a_uv_pip_list_that_refuses_fails_the_fetch_carrying_uvs_stderr() {
        // uv ran and refused -- no venv, a broken one, a permissions problem.
        // Its stderr is the actionable part; the exit code alone tells the
        // user nothing, and an empty tab tells them something false.
        let config = Config {
            settings: fake_uv_settings(),
            ..Config::default()
        };
        let runner = FakeCommandRunner::new();
        version_probe(&runner);
        runner.expect(
            "uv",
            &["pip", "list"],
            bombadil_core::process::CommandOutcome {
                status: Some(2),
                stdout: String::new(),
                stderr: "error: No virtual environment found at /p/api/.venv".into(),
            },
        );

        let got = fetch_deps_blocking(&project("api"), &config, &runner);

        let err = got.expect_err("a non-zero exit must not read as nothing installed");
        assert!(
            err.contains("No virtual environment found"),
            "uv's own stderr must survive; got {err}"
        );
    }

    #[test]
    fn a_deps_fetch_result_is_built_and_stored_against_its_project_index() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let fetch = DepsFetch {
            pyproject: bombadil_core::pyproject::PyProject {
                dependencies: vec!["httpx>=0.27".to_string()],
                ..Default::default()
            },
            installed: vec![],
            tree: no_tree(),
        };

        let _ = update(&mut app, Message::DepsLoaded(0, Ok(fetch)), unused_runner());

        let (index, fetched) = app
            .deps
            .as_ref()
            .expect("a landed fetch must populate app.deps");
        assert_eq!(*index, 0);
        let tab = fetched.as_ref().expect("this fetch succeeded");
        assert!(
            tab.groups[0].rows.iter().any(|r| r.name == "httpx"),
            "the tab must be built from the fetched manifest; got {:?}",
            tab.groups
        );
    }

    #[test]
    fn a_stale_deps_fetch_for_a_project_that_no_longer_exists_is_dropped() {
        let mut app = App::new(Config::default());
        let fetch = DepsFetch {
            pyproject: PyProject::default(),
            installed: vec![],
            tree: no_tree(),
        };

        let _ = update(&mut app, Message::DepsLoaded(0, Ok(fetch)), unused_runner());

        assert!(
            app.deps.is_none(),
            "a fetch result for an index with no matching project must be dropped"
        );
    }

    #[test]
    fn removing_the_displayed_project_clears_its_stale_dependency_tab() {
        // `deps` is tagged with an index; removal shifts every later index,
        // so an uncleared tag would show a stale tab against whichever
        // project now sits there instead of the one it was fetched for.
        let mut app = App::new(Config {
            projects: vec![project("api"), project("web")],
            ..Config::default()
        });
        app.deps = Some((0, Ok(deps::build(&PyProject::default(), &[], no_tree()))));

        let _ = update(
            &mut app,
            Message::RemoveProjectRequested(0),
            unused_runner(),
        );

        assert!(
            app.deps.is_none(),
            "removal must clear a dependency tab that may now describe the wrong project"
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
            selected_label(&app),
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

        let task = update(&mut app, Message::SyncRequested(0), runner);

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
            app.entries[0]
                .environments
                .iter()
                .all(|row| row.python_version.is_none()),
            "a freshly constructed App has not been probed yet -- entries start hollow"
        );

        let task = update(&mut app, Message::SyncRequested(0), unused_runner());

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

        assert!(app.config.projects.is_empty());
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
    fn closing_the_drawer_holds_until_a_failure_reopens_it() {
        // The whole point of the control: output arriving and a command
        // finishing both used to drive it, so "closed" lasted until the next
        // line. Now nothing but this message changes it.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        assert!(!app.drawer.expanded, "it starts collapsed");

        let _ = update(&mut app, Message::DrawerToggled, unused_runner());
        assert!(app.drawer.expanded, "one press opens it");
        let _ = update(&mut app, Message::DrawerToggled, unused_runner());
        assert!(!app.drawer.expanded);

        let _ = update(
            &mut app,
            Message::SyncLine("Resolved 12 packages".to_string()),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::SyncFinished {
                success: false,
                lines: Vec::new(),
            },
            unused_runner(),
        );

        assert!(
            app.drawer.expanded,
            "output alone never opens it, but a failure does: its reason is shown"
        );
        assert!(
            !app.drawer.lines.is_empty(),
            "the transcript is still collected, ready for when they open it"
        );

        let _ = update(&mut app, Message::DrawerToggled, unused_runner());
        assert!(!app.drawer.expanded, "and it closes again with one press");
    }

    #[test]
    fn a_successful_sync_leaves_its_transcript_on_screen() {
        // Supersedes spec §8's "reduces to a single status line once idle",
        // at the user's request: the output of a command they just ran is
        // what they asked for, and collapsing it on success meant a
        // successful sync's transcript existed only while it was running.
        // Closing it is now a press, and it stays closed.
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

        assert_eq!(app.drawer.status, "sync succeeded");
        assert!(!app.drawer.lines.is_empty(), "and its lines with it");
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

        let _ = update(&mut app, Message::SyncRequested(0), unused_runner());

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

        let first = update(&mut app, Message::SyncRequested(0), fake.clone());
        assert!(first.units() > 0, "the first Sync must dispatch work");

        let second = update(&mut app, Message::SyncRequested(0), fake.clone());

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
    fn syncing_from_the_context_menu_targets_the_right_clicked_project_not_the_selected_one() {
        // The whole reason `SyncRequested` carries an index. A right-press
        // opens the menu for a project without moving `app.selected` (see
        // `sidebar::view`), so a Sync resolved against the selection runs
        // `uv sync` -- which prunes packages absent from the target's
        // lockfile -- against an environment the user never named.
        //
        // Two projects with the FIRST selected and the SECOND right-clicked
        // is what separates the two behaviours: every other `SyncRequested`
        // test uses a single-project config, where right and wrong are the
        // same value, which is exactly why this was invisible.
        let mut app = App::new(Config {
            projects: vec![project("api"), project("web")],
            ..Config::default()
        });
        assert_eq!(app.selected, Some(0), "api must be the selected project");
        let web_id = app.config.projects[1].id.to_string();

        let _ = update(&mut app, Message::ContextMenuOpened(1), unused_runner());
        assert_eq!(
            app.selected,
            Some(0),
            "right-pressing must NOT move the selection -- that would silently \
             change what the detail pane shows"
        );

        let _ = update(&mut app, Message::SyncRequested(1), unused_runner());

        // `syncing` holds the id of the project the dispatched job was built
        // for, and the drawer names it: both are the observable end of the
        // same resolution, and the spec handed to `run_sync` is built from
        // that very `Project`.
        assert_eq!(
            busy_project(&app).as_deref(),
            Some(web_id.as_str()),
            "the sync must target the right-clicked project (web), not the selected one (api)"
        );
        assert!(
            app.drawer.status.contains("web"),
            "the running status must name the project actually being synced; got {:?}",
            app.drawer.status
        );
    }

    #[test]
    fn syncing_from_the_context_menu_closes_it() {
        // `OpenTerminalRequested` and `RemoveProjectRequested` both clear the
        // menu; leaving it open for Sync alone is an inconsistency the user
        // has to clean up by hand.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let _ = update(&mut app, Message::ContextMenuOpened(0), unused_runner());
        assert!(app.context_menu.is_some());

        let _ = update(&mut app, Message::SyncRequested(0), unused_runner());

        assert!(app.context_menu.is_none());
    }

    #[test]
    fn syncing_an_out_of_range_project_dispatches_nothing() {
        // A stale index can arrive from a message queued before a project was
        // removed, the same way `ProjectSelected`'s can.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let task = update(&mut app, Message::SyncRequested(99), unused_runner());

        assert_eq!(task.units(), 0);
        assert!(can_start(&app));
    }

    #[test]
    fn a_sync_that_finishes_releases_the_in_flight_slot() {
        // The guard above is a deadlock if it is never cleared: Sync would
        // work exactly once per launch.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let _ = update(&mut app, Message::SyncRequested(0), unused_runner());
        let _ = update(
            &mut app,
            Message::SyncFinished {
                success: true,
                lines: vec![],
            },
            unused_runner(),
        );

        let again = update(&mut app, Message::SyncRequested(0), unused_runner());
        assert!(
            again.units() > 0,
            "Sync must dispatch again once the previous run has finished"
        );
    }

    /// A uv that resolves through `UvSource::Custom`, which probes the given
    /// path through `runner` and never touches PATH or the filesystem.
    pub(crate) fn fake_uv_settings() -> bombadil_core::model::Settings {
        bombadil_core::model::Settings {
            uv_source: bombadil_core::model::UvSource::Custom {
                path: PathBuf::from("/fake/bin/uv"),
            },
            ..Default::default()
        }
    }

    pub(crate) fn version_probe(runner: &FakeCommandRunner) {
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

        let (success, _lines) =
            collect_lines(|on_line| run_sync(&project, &config, &store, &runner, on_line));
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

    fn busy_project(app: &App) -> Option<String> {
        app.busy.as_ref().map(|b| b.project_id.to_string())
    }

    #[test]
    fn a_failed_command_raises_one_toast_and_opens_the_drawer() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.drawer.expanded = false;
        app.busy = Some(crate::busy::Busy::new(
            app.config.projects[0].id,
            crate::busy::Action::InstallVersion("fastapi==0.1.0".into()),
        ));
        let _ = update(
            &mut app,
            Message::SyncFinished {
                success: false,
                lines: vec!["No solution found".into()],
            },
            unused_runner(),
        );
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(app.toasts[0].kind, crate::busy::ToastKind::Failure);
        assert_eq!(app.toasts[0].text, "Could not install fastapi==0.1.0");
        // At the user's request, a failure shows its reason right away.
        assert!(app.drawer.expanded, "the failure opens the drawer");
        let _ = update(&mut app, Message::ShowOutput, unused_runner());
        assert!(app.drawer.expanded, "Show output keeps it open");
        let _ = update(&mut app, Message::ToastDismissed(0), unused_runner());
        assert!(app.toasts.is_empty());
    }

    #[test]
    fn adding_a_project_ends_in_a_toast_too() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.busy = Some(crate::busy::Busy::new(
            app.config.projects[0].id,
            crate::busy::Action::AddProject,
        ));
        let _ = update(
            &mut app,
            Message::AddProjectConfirmFinished {
                success: true,
                lines: vec![],
                index: 0,
            },
            unused_runner(),
        );
        assert!(app.busy.is_none());
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(app.toasts[0].text, "Added api");
    }

    #[test]
    fn a_project_just_added_and_selected_reads_its_dependencies_and_git() {
        // Adding selects the new project before its first sync has built the
        // environment, so its tabs have nothing to show yet. Without a re-read
        // once the sync lands, Dependencies stays on "loading..." and the Git
        // tile never appears until the window happens to regain focus.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.selected = Some(0);
        app.busy = Some(crate::busy::Busy::new(
            app.config.projects[0].id,
            crate::busy::Action::AddProject,
        ));
        let task = update(
            &mut app,
            Message::AddProjectConfirmFinished {
                success: true,
                lines: vec![],
                index: 0,
            },
            Arc::new(FakeCommandRunner::new()),
        );
        assert!(
            task.units() > 1,
            "the venv probe alone leaves the tabs empty; git, deps and members must be read too"
        );
    }

    #[test]
    fn focus_refreshes_only_when_idle_selected_and_not_too_soon() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.selected = Some(0);
        assert!(should_refresh_on_focus(&app, now));

        app.last_focus_refresh = Some(now);
        assert!(!should_refresh_on_focus(
            &app,
            now + Duration::from_millis(1999)
        ));
        assert!(should_refresh_on_focus(&app, now + Duration::from_secs(2)));

        app.busy = Some(crate::busy::Busy::new(
            app.config.projects[0].id,
            crate::busy::Action::Sync,
        ));
        assert!(!should_refresh_on_focus(
            &app,
            now + Duration::from_secs(10)
        ));

        app.busy = None;
        app.selected = None;
        assert!(!should_refresh_on_focus(
            &app,
            now + Duration::from_secs(10)
        ));
    }

    #[test]
    fn focusing_the_window_reprobes_the_selected_project() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.selected = Some(0);
        let task = update(
            &mut app,
            Message::WindowFocused,
            Arc::new(FakeCommandRunner::new()),
        );
        assert!(task.units() > 0, "git, venv and deps must be re-read");
        assert!(app.last_focus_refresh.is_some());
        let again = update(
            &mut app,
            Message::WindowFocused,
            Arc::new(FakeCommandRunner::new()),
        );
        assert_eq!(
            again.units(),
            0,
            "a second focus inside the debounce does nothing"
        );
    }

    #[test]
    fn the_sidebar_width_stays_usable() {
        assert_eq!(clamp_sidebar(20.0), 180);
        assert_eq!(clamp_sidebar(300.4), 300);
        assert_eq!(clamp_sidebar(9000.0), 480);
    }

    #[test]
    fn dragging_the_handle_resizes_and_saves_once_on_release() {
        let mut app = App::new(Config::default());
        assert!(!dragging(&app));
        let _ = update(&mut app, Message::SidebarDragStarted, unused_runner());
        assert!(dragging(&app));
        let moved = update(&mut app, Message::SidebarDragged(320.0), unused_runner());
        assert_eq!(app.config.settings.layout.sidebar_width, 320);
        assert_eq!(moved.units(), 0, "no disk write per pixel");
        let released = update(&mut app, Message::DragEnded, unused_runner());
        assert!(!dragging(&app));
        assert!(released.units() > 0, "the width is saved on release");
        let stray = update(&mut app, Message::DragEnded, unused_runner());
        assert_eq!(stray.units(), 0, "a release with no drag saves nothing");
        let _ = update(&mut app, Message::SidebarDragged(400.0), unused_runner());
        assert_eq!(
            app.config.settings.layout.sidebar_width, 320,
            "a move with no drag is ignored"
        );
    }

    #[test]
    fn the_sidebar_collapses_and_remembers() {
        use crate::shortcuts::Shortcut;
        let mut app = App::new(Config::default());
        assert!(matches!(
            shortcut_message(&app, Shortcut::ToggleSidebar),
            Some(Message::SidebarToggled)
        ));
        let task = update(&mut app, Message::SidebarToggled, unused_runner());
        assert!(app.config.settings.layout.sidebar_collapsed);
        assert!(task.units() > 0, "collapsing is saved");
        app.modal = Some(shell::Modal::About);
        assert!(shortcut_message(&app, Shortcut::ToggleSidebar).is_none());
    }

    #[test]
    fn a_saved_tab_order_is_cleaned_up() {
        let keys = |v: &[&str]| v.iter().map(|k| k.to_string()).collect::<Vec<_>>();
        assert_eq!(tab_order(&[]), Tab::ALL.to_vec());
        assert_eq!(
            tab_order(&keys(&["scripts", "nope", "deps", "scripts"])),
            vec![
                Tab::Scripts,
                Tab::Dependencies,
                Tab::Members,
                Tab::EnvVars,
                Tab::Indexes,
                Tab::GitHub,
                Tab::Doctor,
                Tab::Run
            ],
            "unknown and repeated keys go; missing tabs follow in built-in order"
        );
        for tab in Tab::ALL {
            assert_eq!(tab_order(&[tab.key().to_string()])[0], tab);
        }
    }

    #[test]
    fn moving_a_tab_puts_it_where_the_other_was() {
        let all = Tab::ALL.to_vec();
        let moved = move_tab(&all, Tab::Dependencies, Tab::EnvVars);
        assert_eq!(
            &moved[..3],
            &[Tab::Members, Tab::EnvVars, Tab::Dependencies]
        );
        let back = move_tab(&moved, Tab::Dependencies, Tab::Members);
        assert_eq!(back, all);
        assert_eq!(move_tab(&all, Tab::Scripts, Tab::Scripts), all);
    }

    #[test]
    fn dragging_a_tab_reorders_live_and_saves_on_release() {
        let mut app = App::new(Config::default());
        let _ = update(
            &mut app,
            Message::TabDragStarted(Tab::Scripts),
            unused_runner(),
        );
        assert_eq!(app.tab, Tab::Scripts, "a press selects, as before");
        assert!(dragging(&app));
        let over = update(
            &mut app,
            Message::TabDragOver(Tab::Members),
            unused_runner(),
        );
        assert_eq!(over.units(), 0);
        assert_eq!(
            tab_order(&app.config.settings.layout.tab_order)[1],
            Tab::Scripts
        );
        let end = update(&mut app, Message::DragEnded, unused_runner());
        assert!(!dragging(&app));
        assert!(end.units() > 0, "the new order is saved");
    }

    #[test]
    fn a_tab_click_without_a_drag_saves_nothing() {
        let mut app = App::new(Config::default());
        let _ = update(
            &mut app,
            Message::TabDragStarted(Tab::Members),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::TabDragOver(Tab::Members),
            unused_runner(),
        );
        let end = update(&mut app, Message::DragEnded, unused_runner());
        assert_eq!(app.tab, Tab::Members);
        assert_eq!(end.units(), 0);
        let stray = update(
            &mut app,
            Message::TabDragOver(Tab::Indexes),
            unused_runner(),
        );
        assert_eq!(stray.units(), 0);
        assert!(
            app.config.settings.layout.tab_order.is_empty(),
            "hovering without a drag moves nothing"
        );
    }

    #[test]
    fn a_corrupt_sidebar_width_is_clamped_when_read() {
        let mut app = App::new(Config::default());
        app.config.settings.layout.sidebar_width = 100_000;
        assert_eq!(sidebar_width(&app), 480.0);
        app.config.settings.layout.sidebar_width = 5;
        assert_eq!(sidebar_width(&app), 180.0);
    }

    #[test]
    fn a_minimised_window_does_not_replace_the_remembered_size() {
        let mut app = App::new(Config::default());
        let _ = update(
            &mut app,
            Message::WindowResized(iced::Size::new(1200.0, 800.0)),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::WindowResized(iced::Size::new(0.0, 0.0)),
            unused_runner(),
        );
        assert_eq!(app.window_size, Some(iced::Size::new(1200.0, 800.0)));
    }

    #[test]
    fn a_swap_does_not_bounce_back_under_the_pointer() {
        // Tabs of different widths: after a swap the pointer can sit over the
        // tab it just passed, which must not swap them straight back.
        let mut app = App::new(Config::default());
        let order = |app: &App| tab_order(&app.config.settings.layout.tab_order);
        let _ = update(
            &mut app,
            Message::TabDragStarted(Tab::Members),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::TabDragOver(Tab::Dependencies),
            unused_runner(),
        );
        assert_eq!(order(&app)[0], Tab::Members);
        let _ = update(
            &mut app,
            Message::TabDragOver(Tab::Dependencies),
            unused_runner(),
        );
        assert_eq!(order(&app)[0], Tab::Members, "no bounce");
        let _ = update(
            &mut app,
            Message::TabDragOver(Tab::Members),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::TabDragOver(Tab::Dependencies),
            unused_runner(),
        );
        assert_eq!(
            order(&app)[0],
            Tab::Dependencies,
            "a deliberate move back still works"
        );
    }

    #[test]
    fn the_close_button_hides_or_quits_as_set() {
        let mut app = App::new(Config::default());
        assert!(matches!(
            close_command(&app),
            crate::tray::TrayCommand::Hide
        ));
        app.config.settings.close_to_tray = false;
        assert!(matches!(
            close_command(&app),
            crate::tray::TrayCommand::Quit
        ));
    }

    #[test]
    fn the_close_choice_is_saved() {
        let mut app = App::new(Config::default());
        let task = update(
            &mut app,
            Message::CloseToTraySelected(false),
            unused_runner(),
        );
        assert!(!app.config.settings.close_to_tray);
        assert!(task.units() > 0);
    }

    #[test]
    fn a_chosen_pycharm_is_saved_and_automatic_clears_it() {
        let mut app = App::new(Config::default());
        let chosen = std::path::PathBuf::from("/opt/pycharm/bin/pycharm.sh");
        let task = update(
            &mut app,
            Message::PycharmPicked(Some(chosen.clone())),
            unused_runner(),
        );
        assert_eq!(app.config.settings.pycharm, Some(chosen.clone()));
        assert!(task.units() > 0, "the choice is persisted");

        // Cancelling the picker changes nothing.
        let _ = update(&mut app, Message::PycharmPicked(None), unused_runner());
        assert_eq!(app.config.settings.pycharm, Some(chosen));

        let task = update(&mut app, Message::PycharmAutomatic, unused_runner());
        assert_eq!(app.config.settings.pycharm, None);
        assert!(task.units() > 0);
    }

    #[test]
    fn the_detected_pycharm_is_kept_for_the_settings_panel() {
        let mut app = App::new(Config::default());
        assert_eq!(app.pycharm_detected, None, "not looked for yet");
        let found = std::path::PathBuf::from("/snap/bin/pycharm-professional");
        let _ = update(
            &mut app,
            Message::PycharmDetected(Some(found.clone())),
            unused_runner(),
        );
        assert_eq!(app.pycharm_detected, Some(Some(found)));
    }

    fn two_environments() -> App {
        use bombadil_core::model::{Environment, VenvLocation};
        let mut p = project("api");
        p.environments.push(Environment {
            location: VenvLocation::Custom {
                path: "/v/api-311".into(),
            },
            python: PythonPin::Unpinned,
        });
        App::new(Config {
            projects: vec![p],
            ..Config::default()
        })
    }

    #[test]
    fn a_project_can_be_pointed_at_another_environment_for_one_open() {
        use bombadil_core::model::VenvLocation;
        let app = two_environments();
        let other = VenvLocation::Custom {
            path: "/v/api-311".into(),
        };
        let pointed = pointed_at(&app.config.projects[0], &other).unwrap();
        assert_eq!(pointed.active_environment().unwrap().location, other);
        assert_eq!(
            app.config.projects[0].active,
            VenvLocation::Alongside,
            "the saved project is untouched"
        );
        assert!(
            pointed_at(
                &app.config.projects[0],
                &VenvLocation::Custom {
                    path: "/gone".into()
                }
            )
            .is_none(),
            "an environment that is not there is not opened"
        );
    }

    #[test]
    fn tray_opens_leave_the_config_alone_and_ignore_stale_ids() {
        use crate::tray::TrayCommand;
        use bombadil_core::model::VenvLocation;
        let mut app = two_environments();
        let before = app.config.clone();
        let id = app.config.projects[0].id;
        let other = VenvLocation::Custom {
            path: "/v/api-311".into(),
        };
        let task = update(
            &mut app,
            Message::Tray(TrayCommand::OpenPycharm {
                project: id,
                environment: other.clone(),
            }),
            unused_runner(),
        );
        assert!(task.units() > 0);
        assert_eq!(app.config, before);
        for stale in [
            TrayCommand::OpenTerminal {
                project: uuid::Uuid::new_v4(),
                environment: other.clone(),
            },
            TrayCommand::OpenPycharm {
                project: id,
                environment: VenvLocation::Custom {
                    path: "/gone".into(),
                },
            },
        ] {
            let task = update(&mut app, Message::Tray(stale), unused_runner());
            assert_eq!(task.units(), 0);
        }
    }

    #[test]
    fn the_tray_menu_lists_projects_and_their_environments() {
        use crate::tray::MenuEntry;
        let mut app = two_environments();
        app.entries[0].environments[0].python_version = Some("3.12.4".into());
        let menu = crate::tray::menu_entries(&app.entries);
        let MenuEntry::Submenu { label, children } = &menu[2] else {
            panic!("{menu:?}");
        };
        assert_eq!(label, "api");
        let labels: Vec<_> = children
            .iter()
            .filter_map(|c| match c {
                MenuEntry::Submenu { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            labels,
            [
                ".venv \u{b7} Python 3.12.4 (active)",
                "api-311 \u{b7} not created"
            ]
        );
    }

    #[test]
    fn hiding_closes_the_window_and_keeps_the_app() {
        // Wayland cannot hide a window (winit's `set_visible` is a no-op
        // there), so "to the tray" closes it and keeps the process.
        use crate::tray::TrayCommand;
        let mut app = App::new(Config::default());
        app.tray_is_running = true;
        app.window = Some(iced::window::Id::unique());
        let task = update(&mut app, Message::Tray(TrayCommand::Hide), unused_runner());
        assert_eq!(app.window, None);
        assert!(task.units() > 0, "the window is closed");
    }

    #[test]
    fn showing_with_no_window_opens_a_new_one() {
        use crate::tray::TrayCommand;
        let mut app = App::new(Config::default());
        app.tray_is_running = true;
        let task = update(&mut app, Message::Tray(TrayCommand::Show), unused_runner());
        assert!(task.units() > 0, "a window is opened");
        let id = iced::window::Id::unique();
        let _ = update(&mut app, Message::WindowOpened(id), unused_runner());
        assert_eq!(app.window, Some(id));
    }

    #[test]
    fn hiding_without_a_tray_quits_instead() {
        use crate::tray::TrayCommand;
        let mut app = App::new(Config::default());
        let id = iced::window::Id::unique();
        app.window = Some(id);
        let task = update(&mut app, Message::Tray(TrayCommand::Hide), unused_runner());
        assert!(task.units() > 0);
        assert_eq!(app.window, Some(id), "not hidden: there is no way back");
    }

    #[test]
    fn a_reopened_window_comes_back_at_its_last_size() {
        let mut app = App::new(Config::default());
        let _ = update(
            &mut app,
            Message::WindowResized(iced::Size::new(1000.0, 700.0)),
            unused_runner(),
        );
        assert_eq!(
            window_settings_now(&app).size,
            iced::Size::new(1000.0, 700.0)
        );
    }

    #[test]
    fn a_tray_terminal_opens_in_the_environment_it_named() {
        use bombadil_core::model::VenvLocation;
        let app = two_environments();
        let other = VenvLocation::Custom {
            path: "/v/api-311".into(),
        };
        let project = project_for_open(&app, 0, Some(&other)).unwrap();
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();
        for program in ["gnome-terminal", "open", "wt"] {
            runner.expect(
                program,
                &[],
                bombadil_core::process::CommandOutcome {
                    status: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                },
            );
        }
        let scratch = tempfile::tempdir().unwrap();
        let result = run_open_terminal(
            &TerminalChoice::Detected(host_terminal_kind()),
            &project,
            &app.config,
            &store,
            &runner,
            scratch.path(),
        );
        assert!(result.is_ok(), "got {result:?}");
        let calls = runner.calls();
        assert_eq!(
            calls[0].env.get("VIRTUAL_ENV").map(String::as_str),
            Some(std::path::Path::new("/v/api-311").to_str().unwrap()),
            "the named environment, not the active one"
        );
    }

    #[test]
    fn a_detection_that_lands_after_its_project_went_opens_nothing() {
        let mut app = two_environments();
        let gone = app.config.projects[0].id;
        app.config.projects = vec![project("other")];
        let task = update(
            &mut app,
            Message::OpenTerminalHereDetected(gone, vec![TerminalKind::Kitty], None),
            unused_runner(),
        );
        assert_eq!(task.units(), 0, "index 0 is a different project now");
    }

    #[test]
    fn upgrading_one_package_runs_once() {
        let mut app = one_project();
        let _ = update(
            &mut app,
            Message::DepUpgradeRequested(0, "fastapi".into()),
            unused_runner(),
        );
        assert!(matches!(
            app.busy.as_ref().map(|b| &b.action),
            Some(crate::busy::Action::Upgrade(Some(name))) if name == "fastapi"
        ));
        let again = update(
            &mut app,
            Message::DepUpgradeRequested(0, "httpx".into()),
            unused_runner(),
        );
        assert_eq!(again.units(), 0, "one uv command at a time");
    }

    #[test]
    fn upgrading_everything_asks_first() {
        use crate::shortcuts::Shortcut;
        let mut app = one_project();
        let _ = update(
            &mut app,
            Message::DepUpgradeAllRequested(0),
            unused_runner(),
        );
        assert_eq!(app.confirm_upgrade_all, Some(app.config.projects[0].id));
        assert!(app.busy.is_none());
        assert!(matches!(
            shortcut_message(&app, Shortcut::Escape),
            Some(Message::DepUpgradeAllCancelled)
        ));
        let _ = update(&mut app, Message::DepUpgradeAllCancelled, unused_runner());
        assert_eq!(app.confirm_upgrade_all, None);
        let _ = update(
            &mut app,
            Message::DepUpgradeAllRequested(0),
            unused_runner(),
        );
        let task = update(&mut app, Message::DepUpgradeAllConfirmed, unused_runner());
        assert!(task.units() > 0);
        assert_eq!(app.confirm_upgrade_all, None);
        assert!(matches!(
            app.busy.as_ref().map(|b| &b.action),
            Some(crate::busy::Action::Upgrade(None))
        ));
    }

    #[test]
    fn the_dependencies_tile_offers_sync_where_the_drift_shows() {
        let mut app = one_project();
        let header = detail::Header {
            label: "api".into(),
            path: "/p/api".into(),
            venv_status: detail::VenvStatus::Present,
            python_version: Some("3.12.4".into()),
            venv_path: None,
            git: None,
        };
        let tab = |version: Option<&str>| deps::DepsTab {
            groups: vec![deps::DepGroup {
                kind: deps::GroupKind::Main,
                rows: vec![deps::DepRow {
                    name: "a".into(),
                    constraint: Some(">=1".into()),
                    version: version.map(Into::into),
                    declared: true,
                }],
            }],
            graph: None,
            tree_note: None,
        };
        let sync_of = |app: &App| {
            summary_tiles(app, 0, &header, app.entries.first())
                .into_iter()
                .find(|t| t.label == "Dependencies")
                .and_then(|t| t.sync)
        };
        app.deps = Some((0, Ok(tab(None))));
        assert!(matches!(sync_of(&app), Some(Message::SyncRequested(0))));
        app.deps = Some((0, Ok(tab(Some("1.2")))));
        assert!(sync_of(&app).is_none(), "nothing to fix");
        app.deps = Some((0, Ok(tab(None))));
        app.busy = Some(crate::busy::Busy::new(
            uuid::Uuid::nil(),
            crate::busy::Action::Sync,
        ));
        assert!(sync_of(&app).is_none(), "not while busy");
    }

    #[test]
    fn find_goes_to_the_package_filter() {
        use crate::shortcuts::Shortcut;
        let mut app = one_project();
        app.tab = Tab::Scripts;
        assert!(matches!(
            shortcut_message(&app, Shortcut::Find),
            Some(Message::FindRequested)
        ));
        let task = update(&mut app, Message::FindRequested, unused_runner());
        assert_eq!(app.tab, Tab::Dependencies);
        assert!(task.units() > 0, "the field is focused");
    }

    #[test]
    fn escape_clears_the_package_filter_without_blocking_keys() {
        use crate::shortcuts::Shortcut;
        let mut app = one_project();
        app.selected = Some(0);
        let _ = update(
            &mut app,
            Message::DepFilterChanged("fast".into()),
            unused_runner(),
        );
        assert_eq!(app.dep_filter, "fast");
        assert!(matches!(
            shortcut_message(&app, Shortcut::Escape),
            Some(Message::DepFilterChanged(f)) if f.is_empty()
        ));
        assert!(
            shortcut_message(&app, Shortcut::Terminal).is_some(),
            "a filter is not an overlay"
        );
        let _ = update(&mut app, Message::ProjectSelected(0), unused_runner());
        assert!(app.dep_filter.is_empty(), "another project, a fresh list");
    }

    #[test]
    fn closing_preferences_with_unsaved_values_says_so() {
        let mut app = App::new(Config::default());
        app.config.global_env = vec![bombadil_core::model::EnvVar {
            key: "A".into(),
            value: "1".into(),
            secret: false,
        }];
        app.env_value_drafts
            .insert((env_editor::EnvScope::Global, 0), "2".into());
        app.preferences = Some(preferences::State::default());
        let _ = update(&mut app, Message::PreferencesClosed, unused_runner());
        assert_eq!(app.toasts.len(), 1);
        assert!(
            app.toasts[0].text.contains("1 environment variable value"),
            "{}",
            app.toasts[0].text
        );
        assert_eq!(app.env_value_drafts.len(), 1, "the draft is kept");
    }

    #[test]
    fn a_picked_package_folder_opens_the_dialog_and_a_wrong_one_says_why() {
        let mut app = one_project();
        let id = first_id(&app);
        let _ = update(
            &mut app,
            Message::AddLocalLoaded(
                id,
                "/code/notes".into(),
                Err("notes is not a Python package".into()),
            ),
            unused_runner(),
        );
        assert!(app.add_local.is_none());
        assert_eq!(app.toasts.len(), 1);
        let id = first_id(&app);
        let _ = update(
            &mut app,
            Message::AddLocalLoaded(id, "/code/mylib".into(), Ok(Some("mylib".into()))),
            unused_runner(),
        );
        let draft = app.add_local.as_ref().expect("dialog open");
        assert_eq!(draft.group, deps::GroupKind::Main);
        assert!(draft.editable, "editable by default");
        assert_eq!(draft.label(), "mylib");
    }

    #[test]
    fn adding_a_local_package_runs_once_and_esc_cancels() {
        use crate::shortcuts::Shortcut;
        let mut app = one_project();
        let id = first_id(&app);
        let _ = update(
            &mut app,
            Message::AddLocalLoaded(id, "/code/mylib".into(), Ok(Some("mylib".into()))),
            unused_runner(),
        );
        assert!(matches!(
            shortcut_message(&app, Shortcut::Escape),
            Some(Message::AddLocalCancelled)
        ));
        app.busy = Some(crate::busy::Busy::new(
            uuid::Uuid::nil(),
            crate::busy::Action::Sync,
        ));
        let refused = update(&mut app, Message::AddLocalConfirmed, unused_runner());
        assert_eq!(refused.units(), 0);
        assert!(app.add_local.is_some(), "the dialog waits");
        app.busy = None;
        let task = update(&mut app, Message::AddLocalConfirmed, unused_runner());
        assert!(task.units() > 0);
        assert!(app.add_local.is_none());
        assert!(matches!(
            app.busy.as_ref().map(|b| &b.action),
            Some(crate::busy::Action::AddLocal(name)) if name == "mylib"
        ));
    }

    #[test]
    fn a_scanned_folder_opens_one_offers_several_or_says_why() {
        use crate::shortcuts::Shortcut;
        let mut app = App::new(Config::default());
        let one = update(
            &mut app,
            Message::AddProjectFolderScanned(
                "/code/api".into(),
                (1, vec!["/code/api/pyproject.toml".into()]),
            ),
            unused_runner(),
        );
        assert!(one.units() > 0, "one manifest goes straight to the dialog");
        let several = vec![
            PathBuf::from("/code/api/pyproject.toml"),
            PathBuf::from("/code/web/pyproject.toml"),
        ];
        let _ = update(
            &mut app,
            Message::AddProjectFolderScanned("/code".into(), (2, several.clone())),
            unused_runner(),
        );
        assert_eq!(app.project_chooser, Some(several.clone()));
        assert!(matches!(
            shortcut_message(&app, Shortcut::Escape),
            Some(Message::ProjectChooserCancelled)
        ));
        let chosen = update(
            &mut app,
            Message::ProjectChosen(several[1].clone()),
            unused_runner(),
        );
        assert!(chosen.units() > 0);
        assert_eq!(app.project_chooser, None);
        let _ = update(
            &mut app,
            Message::AddProjectFolderScanned("/notes".into(), (0, vec![])),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::AddProjectFolderScanned("/code".into(), (2, vec![])),
            unused_runner(),
        );
        assert_eq!(app.toasts.len(), 2);
        assert!(
            app.toasts[0].text.contains("already"),
            "{}",
            app.toasts[0].text
        );
        assert!(
            app.toasts[1].text.contains("No pyproject.toml"),
            "{}",
            app.toasts[1].text
        );
    }

    #[test]
    fn a_project_already_added_is_not_added_again() {
        let mut app = one_project();
        let manifest = app.config.projects[0].pyproject_path.clone();
        let task = update(
            &mut app,
            Message::AddProjectManifestPicked(Some(manifest)),
            unused_runner(),
        );
        assert_eq!(task.units(), 0, "no dialog");
        assert_eq!(app.toasts.len(), 1);
        assert!(app.toasts[0].text.contains("already"));
    }

    fn test_uv() -> uv::ResolvedUv {
        uv::ResolvedUv {
            path: PathBuf::from("uv"),
            version: uv::UvVersion {
                major: 0,
                minor: 12,
                patch: 1,
            },
            origin: uv::UvOrigin::Configured,
            newer_than_bundled: false,
        }
    }

    fn argv(specs: Vec<bombadil_core::process::CommandSpec>) -> Vec<Vec<String>> {
        specs.into_iter().map(|s| s.args).collect()
    }

    #[test]
    fn dependency_commands_keep_the_pin_and_the_workspace_members() {
        use members_editor::SyncPlan;
        let uv = test_uv();
        let ctx = uv::command::ProjectContext {
            project_dir: PathBuf::from("/p"),
            venv_path: PathBuf::from("/p/.venv"),
        };
        let pin = PythonPin::Version("3.12".into());
        let upgrade = DepOp::Upgrade(Some("fastapi".into()));
        assert_eq!(
            argv(dep_commands(
                &uv,
                &ctx,
                &PythonPin::Unpinned,
                &SyncPlan::Plain,
                &upgrade
            )),
            [["sync", "--upgrade-package", "fastapi"]]
        );
        assert_eq!(
            argv(dep_commands(
                &uv,
                &ctx,
                &pin,
                &SyncPlan::AllPackages,
                &DepOp::Upgrade(None)
            )),
            [["sync", "--all-packages", "-p", "3.12", "--upgrade"]],
            "members stay installed and the pin holds"
        );
        let add = DepOp::Add(uv::command::add_path(
            &uv,
            &ctx,
            std::path::Path::new("../lib"),
            true,
            uv::command::DependencyGroup::Main,
        ));
        assert_eq!(
            argv(dep_commands(&uv, &ctx, &pin, &SyncPlan::Plain, &add)),
            [vec!["add", "--editable", "../lib", "-p", "3.12"]]
        );
        assert_eq!(
            argv(dep_commands(
                &uv,
                &ctx,
                &PythonPin::Unpinned,
                &SyncPlan::AllPackages,
                &add
            )),
            [
                vec!["add", "--editable", "../lib", "--no-sync"],
                vec!["sync", "--all-packages"]
            ],
            "a workspace adds without uv's own sync, then syncs its members"
        );
    }

    fn first_id(app: &App) -> uuid::Uuid {
        app.config.projects[0].id
    }

    fn two_projects() -> App {
        App::new(Config {
            projects: vec![project("a"), project("b")],
            ..Config::default()
        })
    }

    #[test]
    fn escape_closes_the_dialog_drawn_on_top() {
        use crate::shortcuts::Shortcut;
        let mut app = two_projects();
        app.add_project = Some(manifest_draft("[project]\nname = \"x\"\n"));
        app.add_local = Some(crate::add_local::Draft {
            project: first_id(&app),
            folder: "/code/lib".into(),
            name: None,
            groups: vec![deps::GroupKind::Main],
            group: deps::GroupKind::Main,
            editable: true,
        });
        assert!(matches!(
            shortcut_message(&app, Shortcut::Escape),
            Some(Message::AddLocalCancelled)
        ));
        app.add_local = None;
        app.project_chooser = Some(vec!["/c/a/pyproject.toml".into()]);
        assert!(matches!(
            shortcut_message(&app, Shortcut::Escape),
            Some(Message::ProjectChooserCancelled)
        ));
    }

    #[test]
    fn a_drop_while_a_dialog_is_open_is_ignored() {
        let mut app = two_projects();
        app.confirm_upgrade_all = Some(first_id(&app));
        let task = update(
            &mut app,
            Message::FileDropped("/code".into()),
            unused_runner(),
        );
        assert_eq!(task.units(), 0);
    }

    #[test]
    fn dialogs_follow_their_project_when_another_is_removed() {
        let mut app = two_projects();
        let b = app.config.projects[1].id;
        let _ = update(
            &mut app,
            Message::DepUpgradeAllRequested(1),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::AddLocalLoaded(b, "/code/lib".into(), Ok(Some("lib".into()))),
            unused_runner(),
        );
        app.config.projects.remove(0);
        let _ = update(&mut app, Message::AddLocalConfirmed, unused_runner());
        assert_eq!(
            app.busy.as_ref().map(|x| x.project_id),
            Some(b),
            "still b, not whatever is at index 1 now"
        );
    }

    #[test]
    fn a_manifest_registered_while_its_dialog_loaded_is_refused() {
        let mut app = one_project();
        let draft = add_project::draft_from(
            &app.config.projects[0].pyproject_path.clone(),
            "[project]\nname = \"api\"\n",
        );
        let _ = update(
            &mut app,
            Message::AddProjectManifestLoaded(draft),
            unused_runner(),
        );
        assert!(app.add_project.is_none());
        assert_eq!(app.toasts.len(), 1);
    }

    #[test]
    fn sync_is_not_offered_while_dependencies_load() {
        let mut app = one_project();
        app.deps = None;
        let header = detail::Header {
            label: "api".into(),
            path: "/p/api".into(),
            venv_status: detail::VenvStatus::Present,
            python_version: Some("3.12.4".into()),
            venv_path: None,
            git: None,
        };
        let tiles = summary_tiles(&app, 0, &header, app.entries.first());
        let deps_tile = tiles.iter().find(|t| t.label == "Dependencies").unwrap();
        assert!(deps_tile.sync.is_none(), "loading is not drift");
    }

    #[test]
    fn upgrade_all_waits_rather_than_closing_silently() {
        let mut app = one_project();
        let _ = update(
            &mut app,
            Message::DepUpgradeAllRequested(0),
            unused_runner(),
        );
        app.busy = Some(crate::busy::Busy::new(
            uuid::Uuid::nil(),
            crate::busy::Action::Sync,
        ));
        let _ = update(&mut app, Message::DepUpgradeAllConfirmed, unused_runner());
        assert!(app.confirm_upgrade_all.is_some(), "still asking");
    }

    #[test]
    fn the_add_project_toast_names_the_project_even_after_the_list_moved() {
        let mut app = two_projects();
        let b = app.config.projects[1].id;
        app.busy = Some(crate::busy::Busy::new(b, crate::busy::Action::AddProject));
        app.config.projects.remove(0); // b moves to index 0; 1 is now nothing
        let _ = update(
            &mut app,
            Message::AddProjectConfirmFinished {
                success: true,
                lines: Vec::new(),
                index: 1,
            },
            unused_runner(),
        );
        assert_eq!(app.toasts[0].text, "Added b");
    }

    #[test]
    fn a_tab_dragged_back_where_it_started_saves_nothing() {
        let mut app = App::new(Config::default());
        let _ = update(
            &mut app,
            Message::TabDragStarted(Tab::Members),
            unused_runner(),
        );
        // Left past Dependencies, off it, then back over it: home again.
        let _ = update(
            &mut app,
            Message::TabDragOver(Tab::Dependencies),
            unused_runner(),
        );
        assert_eq!(
            tab_order(&app.config.settings.layout.tab_order)[0],
            Tab::Members
        );
        let _ = update(
            &mut app,
            Message::TabDragOver(Tab::Members),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::TabDragOver(Tab::Dependencies),
            unused_runner(),
        );
        assert_eq!(
            tab_order(&app.config.settings.layout.tab_order),
            Tab::ALL.to_vec()
        );
        let end = update(&mut app, Message::DragEnded, unused_runner());
        assert_eq!(end.units(), 0, "nothing changed, nothing saved");
        assert!(
            app.config.settings.layout.tab_order.is_empty(),
            "the built-in order stays implicit"
        );
    }

    #[test]
    fn the_package_filter_belongs_to_the_project_and_the_tab_it_is_on() {
        use crate::shortcuts::Shortcut;
        let mut app = two_projects();
        app.dep_filter = "fast".into();
        app.tab = Tab::Scripts;
        assert!(
            shortcut_message(&app, Shortcut::Escape).is_none(),
            "Esc does not clear a filter that is not on screen"
        );
        let _ = update(
            &mut app,
            Message::RemoveProjectRequested(0),
            unused_runner(),
        );
        assert!(
            app.dep_filter.is_empty(),
            "the selection moved, so the list did"
        );
    }

    #[test]
    fn an_unmet_pin_turns_the_environment_tile_amber() {
        let mut app = one_project();
        app.entries[0].environments[0].python_version = Some("3.11.9".into());
        app.entries[0].environments[0].pin = PythonPin::Version("3.12".into());
        let header = detail::Header {
            label: "api".into(),
            path: "/p/api".into(),
            venv_status: detail::VenvStatus::Present,
            python_version: Some("3.11.9".into()),
            venv_path: None,
            git: None,
        };
        let tiles = summary_tiles(&app, 0, &header, app.entries.first());
        assert_eq!(tiles[0].state, theme::State::Drifted);
    }

    fn one_project() -> App {
        App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        })
    }

    #[test]
    fn selecting_a_project_whose_folder_is_gone_marks_it_and_reads_nothing() {
        let mut app = one_project();
        app.path_exists = |_| false;
        let task = update(&mut app, Message::ProjectSelected(0), unused_runner());
        let id = first_id(&app);
        assert!(app.missing.contains(&id));
        assert_eq!(
            task.units(),
            0,
            "no git or uv probe on a folder that isn't there"
        );
    }

    #[test]
    fn a_sync_on_a_missing_folder_starts_nothing() {
        let mut app = one_project();
        app.path_exists = |_| false;
        let _ = update(&mut app, Message::SyncRequested(0), unused_runner());
        assert!(app.busy.is_none());
        assert_eq!(
            app.toasts[0].text,
            "This project's folder isn't on this computer"
        );
    }

    fn bundle_with(
        projects: Vec<bombadil_core::model::Project>,
        secrets: bool,
    ) -> bombadil_core::backup::Bundle {
        let config = Config {
            projects,
            ..Config::default()
        };
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        if secrets {
            store
                .set(&bombadil_core::secrets::SecretKey::GitHubToken, "gh")
                .unwrap();
        }
        let out = bombadil_core::backup::export(
            &config,
            &store,
            if secrets { "correct horse" } else { "" },
            bombadil_core::backup::crypto::Kdf::FAST,
            std::path::Path::new("/p"),
        )
        .unwrap();
        bombadil_core::backup::read(&out.text).unwrap()
    }

    #[test]
    fn export_needs_matching_passphrases() {
        let mut app = one_project();
        app.backup.passphrase = "correct horse".into();
        app.backup.confirm = "correct horsf".into();
        let _ = update(&mut app, Message::BackupExportRequested, unused_runner());
        assert_eq!(app.toasts[0].text, "The passphrases don't match");
    }

    #[test]
    fn a_short_passphrase_is_refused() {
        let mut app = one_project();
        app.backup.passphrase = "short".into();
        app.backup.confirm = "short".into();
        let _ = update(&mut app, Message::BackupExportRequested, unused_runner());
        assert_eq!(
            app.toasts[0].text,
            "Use at least 8 characters for the passphrase"
        );
    }

    #[test]
    fn a_file_without_secrets_goes_straight_to_mapping() {
        let mut app = App::new(Config::default());
        let bundle = bundle_with(vec![project("api")], false);
        let _ = update(
            &mut app,
            Message::BackupImportRead("/x.bombadil".into(), Ok(bundle)),
            unused_runner(),
        );
        let import = app.backup.import.as_ref().unwrap();
        assert_eq!(
            import.secrets,
            Some(bombadil_core::backup::Secrets::default())
        );
        assert_eq!(import.maps.len(), 1);
    }

    #[test]
    fn a_wrong_passphrase_can_be_retried() {
        let mut app = App::new(Config::default());
        let bundle = bundle_with(vec![project("api")], true);
        let _ = update(
            &mut app,
            Message::BackupImportRead("/x.bombadil".into(), Ok(bundle)),
            unused_runner(),
        );
        assert_eq!(app.backup.import.as_ref().unwrap().secrets, None);
        let _ = update(
            &mut app,
            Message::BackupUnlocked(
                "/x.bombadil".into(),
                Err("the passphrase is wrong, or the file was changed".into()),
            ),
            unused_runner(),
        );
        let import = app.backup.import.as_ref().unwrap();
        assert!(import.error.is_some());
        assert_eq!(import.maps.len(), 1, "mapping kept");
        let _ = update(
            &mut app,
            Message::BackupUnlocked(
                "/x.bombadil".into(),
                Ok(bombadil_core::backup::Secrets::default()),
            ),
            unused_runner(),
        );
        let import = app.backup.import.as_ref().unwrap();
        assert!(import.error.is_none());
        assert!(import.secrets.is_some());
    }

    #[test]
    fn an_unlock_for_another_file_is_ignored() {
        let mut app = App::new(Config::default());
        let bundle = bundle_with(vec![project("api")], true);
        let _ = update(
            &mut app,
            Message::BackupImportRead("/b.bombadil".into(), Ok(bundle)),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::BackupUnlocked(
                "/a.bombadil".into(),
                Ok(bombadil_core::backup::Secrets::default()),
            ),
            unused_runner(),
        );
        assert_eq!(app.backup.import.as_ref().unwrap().secrets, None);
    }

    #[test]
    fn a_finished_import_becomes_the_running_config() {
        let mut app = one_project();
        let mut config = app.config.clone();
        config.projects.push(project("cli"));
        let report = bombadil_core::backup::import::Report {
            projects: 1,
            ..Default::default()
        };
        let _ = update(
            &mut app,
            Message::BackupImportFinished(Ok((config, report, None))),
            unused_runner(),
        );
        assert_eq!(app.config.projects.len(), 2);
        assert_eq!(app.entries.len(), 2, "the sidebar shows it");
        assert!(app.backup.import.is_none());
        assert_eq!(
            app.toasts[0].text,
            "Imported 1 project, 0 indexes, 0 secrets"
        );
    }

    #[test]
    fn a_finished_import_reselects_and_drops_tabs_read_for_the_old_list() {
        let mut app = one_project();
        app.selected = Some(0);
        app.deps = Some((0, Err("read for the old list".into())));
        app.dep_filter = "num".into();
        let config = Config {
            projects: vec![project("cli")],
            ..Config::default()
        };
        let report = bombadil_core::backup::import::Report::default();
        let task = update(
            &mut app,
            Message::BackupImportFinished(Ok((config, report, None))),
            unused_runner(),
        );
        assert!(app.deps.is_none());
        assert!(app.dep_filter.is_empty(), "selected the way a click does");
        assert!(task.units() > 0);
    }

    #[test]
    fn the_export_toast_counts_secrets_left_behind() {
        let mut app = one_project();
        let out = bombadil_core::backup::Exported {
            text: String::new(),
            secrets: 0,
            missing: vec!["index/corp".into()],
        };
        let _ = update(
            &mut app,
            Message::BackupExportFinished(Ok(out)),
            unused_runner(),
        );
        assert_eq!(
            app.toasts[0].text,
            "Exported 1 project, 0 indexes, 0 secrets. 1 secret wasn't in the keychain"
        );
    }

    #[test]
    fn an_import_reseeds_the_index_credential_rows() {
        let index = |name: &str| bombadil_core::model::Index {
            name: name.into(),
            url: "https://x/simple".into(),
            kind: bombadil_core::model::IndexKind::Extra,
            default_for_new_projects: false,
            auth: bombadil_core::model::IndexAuth::Token,
        };
        let mut app = App::new(Config {
            indexes: vec![index("corp-old")],
            ..Config::default()
        });
        app.env_value_drafts
            .insert((env_editor::EnvScope::Global, 0), "half-typed".into());
        let config = Config {
            indexes: vec![index("corp"), index("extra")],
            ..Config::default()
        };
        let report = bombadil_core::backup::import::Report::default();
        let _ = update(
            &mut app,
            Message::BackupImportFinished(Ok((config, report, None))),
            unused_runner(),
        );
        assert_eq!(app.index_credential_names, ["corp", "extra"]);
        assert_eq!(app.index_credential_drafts.len(), 2);
        assert_eq!(
            index_rename_on_commit(&app, 0),
            None,
            "an edit must not move corp-old's credential over the imported one"
        );
        assert!(app.env_value_drafts.is_empty());
    }

    #[test]
    fn unsaved_secrets_make_the_import_toast_stay() {
        let mut app = one_project();
        let report = bombadil_core::backup::import::Report {
            secrets_error: Some("no Secret Service".into()),
            ..Default::default()
        };
        let config = app.config.clone();
        let _ = update(
            &mut app,
            Message::BackupImportFinished(Ok((config, report, None))),
            unused_runner(),
        );
        assert_eq!(app.toasts[0].kind, crate::busy::ToastKind::Warning);
        assert!(app.toasts[0].text.contains("no Secret Service"));
    }

    #[test]
    fn a_replace_names_its_backup_and_missing_folders() {
        let mut app = one_project();
        let report = bombadil_core::backup::import::Report {
            projects: 1,
            not_found: vec!["cli".into()],
            ..Default::default()
        };
        let config = app.config.clone();
        let _ = update(
            &mut app,
            Message::BackupImportFinished(Ok((
                config,
                report,
                Some("/c/config.toml.bak-1".into()),
            ))),
            unused_runner(),
        );
        let text = &app.toasts[0].text;
        assert!(text.contains("1 project folder not found"), "{text}");
        assert!(text.contains("config.toml.bak-1"), "{text}");
    }

    #[test]
    fn cancelling_an_import_changes_nothing() {
        let mut app = one_project();
        let before = app.config.clone();
        let bundle = bundle_with(vec![project("cli")], false);
        let _ = update(
            &mut app,
            Message::BackupImportRead("/x.bombadil".into(), Ok(bundle)),
            unused_runner(),
        );
        let _ = update(&mut app, Message::BackupImportCancelled, unused_runner());
        assert!(app.backup.import.is_none());
        assert_eq!(app.config, before);
    }

    #[test]
    fn import_waits_for_a_mode() {
        let mut app = App::new(Config::default());
        let bundle = bundle_with(vec![project("api")], false);
        let _ = update(
            &mut app,
            Message::BackupImportRead("/x.bombadil".into(), Ok(bundle)),
            unused_runner(),
        );
        let _ = update(&mut app, Message::BackupImportConfirmed, unused_runner());
        assert!(
            app.backup.import.is_some(),
            "nothing ran without Merge or Replace"
        );
    }

    #[test]
    fn a_confirmed_recreate_on_a_missing_folder_starts_nothing() {
        let mut app = one_project();
        app.path_exists = |_| false;
        app.confirm_recreate = Some(context_menu::ConfirmRecreate {
            project_id: first_id(&app),
            label: "api".into(),
            venv_path: PathBuf::from("/p/api/.venv"),
        });
        let _ = update(&mut app, Message::RecreateVenvConfirmed, unused_runner());
        assert!(app.busy.is_none());
        assert!(app.confirm_recreate.is_none());
        assert_eq!(app.toasts[0].text, FOLDER_MISSING);
    }

    #[test]
    fn locate_points_the_project_at_the_new_file_and_saves() {
        let mut app = one_project();
        let id = first_id(&app);
        app.missing.insert(id);
        let new = PathBuf::from("/q/api/pyproject.toml");
        let task = update(
            &mut app,
            Message::LocatePicked(id, Some(new.clone())),
            unused_runner(),
        );
        assert_eq!(app.config.projects[0].pyproject_path, new);
        assert!(!app.missing.contains(&id));
        assert!(task.units() > 0, "the new path is saved");
        assert!(
            app.entries[0].environments[0].path.starts_with("/q/api"),
            "the cached venv path follows the project: {:?}",
            app.entries[0].environments[0].path
        );
    }

    #[test]
    fn the_sync_check_asks_about_the_sync_bombadil_would_run() {
        use members_editor::SyncPlan;
        let uv = uv::ResolvedUv {
            path: PathBuf::from("/uv"),
            origin: uv::UvOrigin::Bundled,
            version: uv::UvVersion::parse("uv 0.12.19").unwrap(),
            newer_than_bundled: false,
        };
        let ctx = uv::command::ProjectContext {
            project_dir: PathBuf::from("/p"),
            venv_path: PathBuf::from("/p/.venv"),
        };
        let pinned = PythonPin::Version("3.14".into());
        assert_eq!(
            sync_check_spec(&uv, &ctx, &pinned, &SyncPlan::AllPackages).args,
            [
                "sync",
                "--all-packages",
                "-p",
                "3.14",
                "--check",
                "--frozen",
                "--offline"
            ]
        );
        assert_eq!(
            sync_check_spec(&uv, &ctx, &PythonPin::Unpinned, &SyncPlan::Plain).args,
            ["sync", "--check", "--frozen", "--offline"]
        );
    }

    #[test]
    fn sync_checks_and_pythons_are_kept() {
        use bombadil_core::uv::results::SyncCheck;
        let mut app = one_project();
        let id = app.config.projects[0].id;
        let _ = update(
            &mut app,
            Message::SyncChecked {
                id,
                check: SyncCheck::NoEnvironment,
            },
            unused_runner(),
        );
        assert_eq!(app.sync_checks.get(&id), Some(&SyncCheck::NoEnvironment));
        let _ = update(
            &mut app,
            Message::PythonsListed(Some(vec!["3.14.7".into()])),
            unused_runner(),
        );
        assert_eq!(app.installed_pythons, Some(vec!["3.14.7".to_string()]));
        let texts: Vec<String> = project_findings(&app, 0)
            .into_iter()
            .map(|f| f.text)
            .collect();
        assert_eq!(texts, ["No environment yet"]);
    }

    #[test]
    fn a_sync_check_for_a_removed_project_is_dropped() {
        use bombadil_core::uv::results::SyncCheck;
        let mut app = one_project();
        let _ = update(
            &mut app,
            Message::SyncChecked {
                id: uuid::Uuid::new_v4(),
                check: SyncCheck::InSync,
            },
            unused_runner(),
        );
        assert!(app.sync_checks.is_empty());
    }

    #[test]
    fn a_busy_project_is_not_sync_checked() {
        let mut app = one_project();
        let id = app.config.projects[0].id;
        assert_eq!(health_round_targets(&app), vec![id]);
        app.busy = Some(crate::busy::Busy::new(id, crate::busy::Action::Sync));
        assert!(health_round_targets(&app).is_empty());
    }

    #[test]
    fn project_findings_read_the_default_environment() {
        let mut app = one_project();
        app.entries[0].requires_python = Some(">=3.12".into());
        app.entries[0].environments[0].python_version = Some("3.11.2".into());
        let texts: Vec<String> = project_findings(&app, 0)
            .into_iter()
            .map(|f| f.text)
            .collect();
        assert_eq!(texts, ["Python 3.11.2, but pyproject.toml needs >=3.12"]);
        assert!(
            project_findings(&app, 99).is_empty(),
            "out of range is nothing"
        );
    }

    #[test]
    fn the_environment_tile_carries_the_newer_python_note() {
        let mut app = one_project();
        app.entries[0].environments[0].active = true;
        app.entries[0].environments[0].python_version = Some("3.12.4".into());
        app.installed_pythons = Some(vec!["3.14.7".into()]);
        let header = detail::Header {
            label: "api".into(),
            path: "/p/api".into(),
            venv_status: detail::VenvStatus::Present,
            python_version: Some("3.12.4".into()),
            venv_path: None,
            git: None,
        };
        let env = summary_tiles(&app, 0, &header, app.entries.first())
            .into_iter()
            .find(|t| t.label == "Environment")
            .unwrap();
        assert_eq!(
            env.note.as_deref(),
            Some("Python 3.14 is installed and allowed; this environment uses 3.12")
        );
    }

    #[test]
    fn a_checks_tile_appears_only_with_a_pull_request() {
        use bombadil_core::github::{Check, CheckState, Lookup, PrStatus, PullRequest};
        let mut app = one_project();
        let header = detail::Header {
            label: "api".into(),
            path: "/p/api".into(),
            venv_status: detail::VenvStatus::Present,
            python_version: None,
            venv_path: None,
            git: None,
        };
        let checks = |app: &App| {
            summary_tiles(app, 0, &header, app.entries.first())
                .into_iter()
                .find(|t| t.label == "Checks")
        };
        assert!(checks(&app).is_none());
        let check = |state| Check {
            name: "t".into(),
            workflow: None,
            state,
            url: None,
            started: None,
            completed: None,
        };
        let status = PrStatus {
            pr: PullRequest {
                number: 12,
                title: "x".into(),
                url: "https://github.com/o/r/pull/12".into(),
                draft: false,
                head_branch: "f".into(),
                base_branch: "main".into(),
                head_sha: "abc".into(),
            },
            checks: vec![check(CheckState::Failed), check(CheckState::Passed)],
        };
        app.github.insert(
            app.config.projects[0].id,
            crate::github_tab::GitHubState {
                lookup: Lookup::Found(status),
                last_found: None,
                checked_at: std::time::Instant::now(),
            },
        );
        let tile = checks(&app).expect("a Checks tile");
        assert_eq!(tile.value, "1 of 2 failed");
        assert_eq!(tile.state, theme::State::Drifted);
        assert!(matches!(
            tile.press,
            Some(Message::TabSelected(Tab::GitHub))
        ));
    }

    #[test]
    fn shortcuts_name_the_selected_project_when_allowed() {
        use crate::shortcuts::Shortcut;
        let mut app = one_project();
        app.selected = None;
        for s in [Shortcut::Sync, Shortcut::Terminal, Shortcut::Refresh] {
            assert!(
                shortcut_message(&app, s).is_none(),
                "nothing selected: {s:?}"
            );
        }
        assert!(matches!(
            shortcut_message(&app, Shortcut::AddProject),
            Some(Message::AddProjectPickManifestRequested)
        ));
        app.selected = Some(0);
        assert!(matches!(
            shortcut_message(&app, Shortcut::Sync),
            Some(Message::SyncRequested(0))
        ));
        assert!(matches!(
            shortcut_message(&app, Shortcut::Terminal),
            Some(Message::OpenTerminalHereRequested(0))
        ));
        assert!(matches!(
            shortcut_message(&app, Shortcut::Refresh),
            Some(Message::RefreshRequested)
        ));
        app.selected = Some(5);
        assert!(
            shortcut_message(&app, Shortcut::Sync).is_none(),
            "a stale selection names nothing"
        );
    }

    #[test]
    fn shortcuts_that_start_uv_wait_while_busy() {
        use crate::shortcuts::Shortcut;
        let mut app = one_project();
        app.selected = Some(0);
        app.busy = Some(crate::busy::Busy::new(
            uuid::Uuid::nil(),
            crate::busy::Action::Sync,
        ));
        for s in [Shortcut::AddProject, Shortcut::Sync, Shortcut::Refresh] {
            assert!(shortcut_message(&app, s).is_none(), "{s:?}");
        }
        assert!(
            shortcut_message(&app, Shortcut::Terminal).is_some(),
            "a terminal starts no uv command"
        );
    }

    #[test]
    fn shortcuts_do_nothing_under_an_overlay() {
        use crate::shortcuts::Shortcut;
        let mut app = one_project();
        app.selected = Some(0);
        app.open_menu = Some(shell::TopMenu::Help);
        for s in [
            Shortcut::AddProject,
            Shortcut::Sync,
            Shortcut::Terminal,
            Shortcut::Refresh,
        ] {
            assert!(shortcut_message(&app, s).is_none(), "{s:?}");
        }
    }

    #[test]
    fn escape_closes_the_top_layer_only() {
        use crate::shortcuts::Shortcut;
        let mut app = App::new(Config::default());
        assert!(shortcut_message(&app, Shortcut::Escape).is_none());
        app.open_menu = Some(shell::TopMenu::Help);
        app.modal = Some(shell::Modal::About);
        assert!(matches!(
            shortcut_message(&app, Shortcut::Escape),
            Some(Message::ModalClosed)
        ));
        app.modal = None;
        assert!(matches!(
            shortcut_message(&app, Shortcut::Escape),
            Some(Message::MenuDismissed)
        ));
    }

    #[test]
    fn escape_cancels_a_version_edit_and_other_keys_still_work_beside_it() {
        use crate::shortcuts::Shortcut;
        let mut app = one_project();
        app.selected = Some(0);
        app.dep_edit = Some(deps::DepEdit {
            project: 0,
            name: "fastapi".into(),
            group: deps::GroupKind::Main,
            draft: "0.110".into(),
        });
        assert!(matches!(
            shortcut_message(&app, Shortcut::Escape),
            Some(Message::DepVersionEditCancelled)
        ));
        assert!(
            shortcut_message(&app, Shortcut::Terminal).is_some(),
            "an inline edit is not an overlay"
        );
    }

    #[test]
    fn a_shortcut_runs_what_its_button_runs() {
        let mut app = one_project();
        app.modal = Some(shell::Modal::About);
        let _ = update(
            &mut app,
            Message::Shortcut(crate::shortcuts::Shortcut::Escape),
            Arc::new(FakeCommandRunner::new()),
        );
        assert_eq!(app.modal, None);
    }

    #[test]
    fn refresh_rereads_the_selected_project() {
        let mut app = one_project();
        app.selected = None;
        let none = update(
            &mut app,
            Message::RefreshRequested,
            Arc::new(FakeCommandRunner::new()),
        );
        assert_eq!(none.units(), 0);
        app.selected = Some(0);
        let task = update(
            &mut app,
            Message::RefreshRequested,
            Arc::new(FakeCommandRunner::new()),
        );
        assert!(task.units() > 0);
    }

    #[test]
    fn frames_are_wanted_only_while_something_moves() {
        let mut app = App::new(Config::default());
        assert!(!animating(&app));
        app.busy = Some(crate::busy::Busy::new(
            uuid::Uuid::nil(),
            crate::busy::Action::Sync,
        ));
        assert!(animating(&app));
        app.busy = None;
        crate::busy::push(
            &mut app.toasts,
            crate::busy::ToastKind::Failure,
            "bad".into(),
            std::time::Instant::now(),
        );
        assert!(
            !animating(&app),
            "a failure toast has no countdown; redrawing for it burns a core"
        );
        crate::busy::push(
            &mut app.toasts,
            crate::busy::ToastKind::Success,
            "x".into(),
            std::time::Instant::now(),
        );
        assert!(animating(&app));
    }

    #[test]
    fn a_tick_moves_the_clock_and_drops_expired_toasts() {
        let mut app = App::new(Config::default());
        let now = std::time::Instant::now();
        crate::busy::push(
            &mut app.toasts,
            crate::busy::ToastKind::Success,
            "x".into(),
            now,
        );
        let later = now + crate::busy::SUCCESS_TOAST + std::time::Duration::from_millis(1);
        let _ = update(&mut app, Message::Tick(later), unused_runner());
        assert_eq!(app.now, later);
        assert!(app.toasts.is_empty());
    }

    #[test]
    fn a_running_command_blocks_a_second_one() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let fake = Arc::new(FakeCommandRunner::new());
        let _ = update(&mut app, Message::SyncRequested(0), fake.clone());
        assert!(!can_start(&app));
        assert_eq!(
            app.busy.as_ref().map(|b| b.action.clone()),
            Some(crate::busy::Action::Sync)
        );
        let second = update(&mut app, Message::SyncRequested(0), fake.clone());
        assert_eq!(second.units(), 0, "a second sync must not be dispatched");
    }

    #[test]
    fn a_timed_out_sync_releases_busy_and_says_so() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.busy = Some(crate::busy::Busy::new(
            app.config.projects[0].id,
            crate::busy::Action::Sync,
        ));
        let _ = update(
            &mut app,
            Message::SyncFinished {
                success: false,
                lines: vec!["sync timed out".into()],
            },
            unused_runner(),
        );
        assert!(can_start(&app));
        assert!(
            app.drawer.status.contains("Could not sync api"),
            "{}",
            app.drawer.status
        );
    }

    #[test]
    fn a_python_install_uv_refuses_says_why() {
        // It used to be a bare `bool`, and the caller threw even that away:
        // the dialog showed the old list again with no reason.
        let config = Config {
            settings: fake_uv_settings(),
            ..Config::default()
        };
        let runner = FakeCommandRunner::new();
        version_probe(&runner);
        runner.expect(
            "uv",
            &["python", "install", "3.99"],
            bombadil_core::process::CommandOutcome {
                status: Some(2),
                stdout: String::new(),
                stderr: "error: No download found for request: cpython-3.99".into(),
            },
        );
        let result = install_interpreter_blocking("3.99", &config, &runner);
        assert!(
            result
                .as_ref()
                .is_err_and(|e| e.contains("No download found")),
            "{result:?}"
        );
    }

    fn start_run(app: &mut App, args: &[&str]) -> Task<Message> {
        app.entries[0].environments[0].python_version = Some("3.12.0".into());
        let id = first_id(app);
        update(
            app,
            Message::RunRequested {
                project: id,
                label: "demo".into(),
                args: args.iter().map(|s| s.to_string()).collect(),
            },
            unused_runner(),
        )
    }

    #[test]
    fn a_created_project_opens_the_add_dialog_on_its_manifest() {
        let mut app = App::new(Config::default());
        let _ = update(&mut app, Message::NewProjectRequested, unused_runner());
        assert!(app.new_project.is_some());
        let manifest = PathBuf::from("/w/demo/pyproject.toml");
        let task = update(
            &mut app,
            Message::NewProjectCreated(Ok(manifest)),
            unused_runner(),
        );
        assert!(app.new_project.is_none());
        assert!(
            task.units() > 0,
            "the manifest is loaded for the Add project dialog"
        );
    }

    #[test]
    fn a_failed_create_keeps_the_dialog_and_says_why() {
        let mut app = App::new(Config::default());
        let _ = update(&mut app, Message::NewProjectRequested, unused_runner());
        app.creating_project = true;
        let _ = update(
            &mut app,
            Message::NewProjectCreated(Err("error: boom".into())),
            unused_runner(),
        );
        let draft = app.new_project.as_ref().unwrap();
        assert_eq!(draft.error.as_deref(), Some("error: boom"));
        assert!(!app.creating_project);
        assert!(app.config.projects.is_empty(), "nothing registered");
    }

    #[test]
    fn create_waits_for_a_valid_name() {
        let mut app = App::new(Config::default());
        let _ = update(&mut app, Message::NewProjectRequested, unused_runner());
        let _ = update(
            &mut app,
            Message::NewProjectName("bad name".into()),
            unused_runner(),
        );
        let task = update(&mut app, Message::NewProjectCreate, unused_runner());
        assert_eq!(task.units(), 0);
        assert!(!app.creating_project);
    }

    fn venv_of(app: &App) -> PathBuf {
        app.entries[0]
            .environments
            .iter()
            .find(|r| r.active)
            .unwrap()
            .path
            .clone()
    }

    fn deps_loaded_with(
        app: &mut App,
        installed: Vec<bombadil_core::uv::results::InstalledPackage>,
    ) -> Task<Message> {
        let fetch = DepsFetch {
            pyproject: Default::default(),
            installed,
            tree: no_tree(),
        };
        update(app, Message::DepsLoaded(0, Ok(fetch)), unused_runner())
    }

    fn installed(list: &[(&str, &str)]) -> Vec<bombadil_core::uv::results::InstalledPackage> {
        list.iter()
            .map(|(n, v)| bombadil_core::uv::results::InstalledPackage {
                name: n.to_string(),
                version: v.to_string(),
                editable_project_location: None,
            })
            .collect()
    }

    #[test]
    fn loading_dependencies_starts_a_check() {
        let mut app = one_project();
        app.selected = Some(0);
        let task = deps_loaded_with(&mut app, installed(&[("jinja2", "2.4.1")]));
        assert!(task.units() > 0, "the check was dispatched");
    }

    #[test]
    fn turning_the_check_off_sends_nothing() {
        let mut app = one_project();
        app.selected = Some(0);
        app.config.settings.check_vulnerabilities = false;
        assert!(!vuln_check_due(
            &app,
            &venv_of(&app),
            "x==1",
            std::time::Instant::now()
        ));
        let task = deps_loaded_with(&mut app, installed(&[("jinja2", "2.4.1")]));
        assert_eq!(task.units(), 0);
    }

    #[test]
    fn an_unchanged_environment_is_not_checked_twice_a_day() {
        let mut app = one_project();
        let venv = venv_of(&app);
        let now = std::time::Instant::now();
        let _ = update(
            &mut app,
            Message::VulnsChecked(venv.clone(), "x==1".into(), Ok(Default::default())),
            unused_runner(),
        );
        assert!(!vuln_check_due(&app, &venv, "x==1", now));
        assert!(
            vuln_check_due(&app, &venv, "x==2", now),
            "a changed environment is"
        );
        assert!(vuln_check_due(
            &app,
            &venv,
            "x==1",
            now + std::time::Duration::from_secs(25 * 3600)
        ));
    }

    #[test]
    fn findings_only_show_for_the_list_they_were_made_for() {
        let mut app = one_project();
        app.selected = Some(0);
        let venv = venv_of(&app);
        let _ = deps_loaded_with(&mut app, installed(&[("jinja2", "2.4.1")]));
        let finding = bombadil_core::osv::Finding {
            id: "GHSA-1".into(),
            summary: "x".into(),
            severity: None,
            fixed: Some("2.10.1".into()),
        };
        let key = vuln_key(&installed(&[("jinja2", "2.4.1")]));
        let _ = update(
            &mut app,
            Message::VulnsChecked(
                venv,
                key,
                Ok([("jinja2".to_string(), vec![finding])].into()),
            ),
            unused_runner(),
        );
        assert!(
            project_findings(&app, 0)
                .iter()
                .any(|f| f.text.contains("vulnerab"))
        );
        // Upgraded: the old answer no longer describes this environment.
        let _ = deps_loaded_with(&mut app, installed(&[("jinja2", "3.1.6")]));
        assert!(
            !project_findings(&app, 0)
                .iter()
                .any(|f| f.text.contains("vulnerab"))
        );
    }

    #[test]
    fn switching_the_check_off_forgets_its_findings() {
        let mut app = one_project();
        app.selected = Some(0);
        let venv = venv_of(&app);
        let list = installed(&[("jinja2", "2.4.1")]);
        let _ = deps_loaded_with(&mut app, list.clone());
        let finding = bombadil_core::osv::Finding {
            id: "GHSA-1".into(),
            summary: "x".into(),
            severity: None,
            fixed: None,
        };
        let _ = update(
            &mut app,
            Message::VulnsChecked(
                venv,
                vuln_key(&list),
                Ok([("jinja2".to_string(), vec![finding])].into()),
            ),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::CheckVulnerabilitiesSelected(false),
            unused_runner(),
        );
        assert!(
            !project_findings(&app, 0)
                .iter()
                .any(|f| f.text.contains("vulnerab"))
        );
    }

    #[test]
    fn local_projects_never_leave_the_machine() {
        let mut list = installed(&[("jinja2", "2.4.1"), ("acme-billing-internal", "0.1.0")]);
        list[1].editable_project_location = Some(PathBuf::from("/w/billing"));
        assert_eq!(
            vuln_packages(&list),
            [("jinja2".to_string(), "2.4.1".to_string())]
        );
        assert!(!vuln_key(&list).contains("acme"));
    }

    #[test]
    fn a_failed_check_is_retried_on_the_next_load() {
        let mut app = one_project();
        let venv = venv_of(&app);
        let _ = update(
            &mut app,
            Message::VulnsChecked(venv.clone(), "x==1".into(), Err("offline".into())),
            unused_runner(),
        );
        assert!(vuln_check_due(
            &app,
            &venv,
            "x==1",
            std::time::Instant::now()
        ));
    }

    #[test]
    fn findings_become_a_health_warning_and_a_failure_does_not() {
        let mut app = one_project();
        app.selected = Some(0);
        let venv = venv_of(&app);
        let list = installed(&[("jinja2", "2.4.1")]);
        let _ = deps_loaded_with(&mut app, list.clone());
        let finding = bombadil_core::osv::Finding {
            id: "GHSA-1".into(),
            summary: "x".into(),
            severity: None,
            fixed: Some("2".into()),
        };
        let _ = update(
            &mut app,
            Message::VulnsChecked(
                venv.clone(),
                vuln_key(&list),
                Ok([("jinja2".to_string(), vec![finding])].into()),
            ),
            unused_runner(),
        );
        assert!(
            project_findings(&app, 0)
                .iter()
                .any(|f| f.text == "1 package has a known vulnerability")
        );
        let _ = update(
            &mut app,
            Message::VulnsChecked(venv, vuln_key(&list), Err("offline".into())),
            unused_runner(),
        );
        assert!(
            !project_findings(&app, 0)
                .iter()
                .any(|f| f.text.contains("vulnerab"))
        );
    }

    #[test]
    fn clicking_a_package_opens_its_details_and_again_closes_them() {
        let mut app = one_project();
        app.selected = Some(0);
        let id = first_id(&app);
        let _ = deps_loaded_with(&mut app, installed(&[("jinja2", "2.4.1")]));
        let task = update(
            &mut app,
            Message::DepDetailsToggled(0, "jinja2".into()),
            unused_runner(),
        );
        assert!(matches!(&app.dep_details, Some((p, n, None)) if *p == id && n == "jinja2"));
        assert!(task.units() > 0, "the METADATA is read");
        let _ = update(
            &mut app,
            Message::DepDetailsToggled(0, "jinja2".into()),
            unused_runner(),
        );
        assert!(app.dep_details.is_none());
    }

    #[test]
    fn details_for_another_package_are_ignored_when_late() {
        let mut app = one_project();
        let id = first_id(&app);
        let _ = update(
            &mut app,
            Message::DepDetailsToggled(0, "a".into()),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::DepDetailsLoaded(id, "b".into(), Some(Default::default())),
            unused_runner(),
        );
        assert!(matches!(&app.dep_details, Some((_, n, None)) if n == "a"));
    }

    #[test]
    fn upgrade_to_the_fixed_version_installs_it() {
        let mut app = one_project();
        let task = update(
            &mut app,
            Message::DepPinRequested(0, "jinja2".into(), "2.10.1".into(), deps::GroupKind::Main),
            unused_runner(),
        );
        assert!(task.units() > 0);
        assert!(matches!(
            app.busy.as_ref().map(|b| &b.action),
            Some(crate::busy::Action::InstallVersion(r)) if r == "jinja2==2.10.1"
        ));
    }

    #[test]
    fn a_finished_sync_reports_what_changed() {
        let mut app = one_project();
        app.selected = Some(0);
        let id = first_id(&app);
        let _ = deps_loaded_with(
            &mut app,
            installed(&[("numpy", "2.1.0"), ("six", "1.16.0")]),
        );
        app.busy = Some(crate::busy::Busy::new(id, crate::busy::Action::Sync));
        let _ = update(
            &mut app,
            Message::SyncFinished {
                success: true,
                lines: vec![],
            },
            unused_runner(),
        );
        assert_eq!(app.toasts[0].text, "Synced api");
        let _ = deps_loaded_with(
            &mut app,
            installed(&[("numpy", "2.3.1"), ("polars", "1.9.0")]),
        );
        assert_eq!(
            app.toasts[0].text,
            "Synced api: numpy 2.1.0 → 2.3.1, added polars, removed six"
        );
        assert!(app.drawer.lines.iter().any(|l| l == "+ polars 1.9.0"));
        assert_eq!(
            app.last_change.as_ref().map(|(_, s)| s.as_str()),
            Some("numpy 2.1.0 → 2.3.1, added polars, removed six")
        );
    }

    #[test]
    fn a_reload_without_an_action_reports_nothing() {
        let mut app = one_project();
        app.selected = Some(0);
        let _ = deps_loaded_with(&mut app, installed(&[("a", "1")]));
        let _ = deps_loaded_with(&mut app, installed(&[("a", "2")]));
        assert!(app.toasts.is_empty());
        assert!(app.last_change.is_none());
    }

    #[test]
    fn a_failed_sync_reports_no_change() {
        let mut app = one_project();
        app.selected = Some(0);
        let id = first_id(&app);
        let _ = deps_loaded_with(&mut app, installed(&[("a", "1")]));
        app.busy = Some(crate::busy::Busy::new(id, crate::busy::Action::Sync));
        let _ = update(
            &mut app,
            Message::SyncFinished {
                success: false,
                lines: vec![],
            },
            unused_runner(),
        );
        assert!(!app.awaiting_change.contains_key(&id));
    }

    #[test]
    fn a_failure_toast_offers_show_output_once() {
        // The toast has its own "Show output"; the doctor's fix must not
        // be a second one.
        let mut app = one_project();
        let id = first_id(&app);
        app.busy = Some(crate::busy::Busy::new(id, crate::busy::Action::Sync));
        let _ = update(
            &mut app,
            Message::SyncFinished {
                success: false,
                lines: vec!["  × The build backend returned an error".into()],
            },
            unused_runner(),
        );
        assert_eq!(app.toasts[0].kind, crate::busy::ToastKind::Failure);
        assert_eq!(app.toasts[0].fix, None);
    }

    #[test]
    fn opening_the_disk_section_measures() {
        let mut app = one_project();
        let task = update(
            &mut app,
            Message::PreferencesSectionOpened(preferences::Section::Disk),
            unused_runner(),
        );
        assert!(task.units() > 0);
        assert!(app.disk.measuring);
    }

    #[test]
    fn a_delete_asks_first() {
        let mut app = one_project();
        let path = PathBuf::from("/central/old");
        let task = update(
            &mut app,
            Message::DiskDeleteRequested(crate::disk::Delete::Leftover(path.clone())),
            unused_runner(),
        );
        assert_eq!(task.units(), 0, "nothing deleted yet");
        assert_eq!(app.disk.confirm, Some(crate::disk::Delete::Leftover(path)));
        let _ = update(&mut app, Message::DiskDeleteCancelled, unused_runner());
        assert!(app.disk.confirm.is_none());
    }

    #[test]
    fn an_environment_in_use_is_not_deleted() {
        let mut app = one_project();
        let id = first_id(&app);
        let _ = start_run(&mut app, &["demo"]);
        let delete =
            crate::disk::Delete::Environment(id, VenvLocation::Alongside, "/p/.venv".into());
        let _ = update(
            &mut app,
            Message::DiskDeleteRequested(delete),
            unused_runner(),
        );
        assert!(app.disk.confirm.is_none());
        assert!(
            app.toasts[0].text.contains("is running"),
            "{}",
            app.toasts[0].text
        );
    }

    #[test]
    fn a_deleted_extra_environment_leaves_the_project() {
        let mut app = one_project();
        let id = first_id(&app);
        let extra = VenvLocation::Custom {
            path: "/envs/extra".into(),
        };
        app.config.projects[0]
            .environments
            .push(bombadil_core::model::Environment {
                location: extra.clone(),
                python: Default::default(),
            });
        let delete = crate::disk::Delete::Environment(id, extra.clone(), "/envs/extra".into());
        let task = update(
            &mut app,
            Message::DiskDeleted(delete, Ok(())),
            unused_runner(),
        );
        assert!(
            !app.config.projects[0]
                .environments
                .iter()
                .any(|e| e.location == extra)
        );
        assert!(task.units() > 0, "saved and measured again");
        assert!(app.toasts[0].text.starts_with("Deleted"));
    }

    #[test]
    fn pruning_the_cache_takes_the_one_command_slot() {
        let mut app = one_project();
        app.disk.cache = Some(("/c".into(), Some(100)));
        let task = update(&mut app, Message::CachePruneRequested, unused_runner());
        assert!(task.units() > 0);
        assert!(matches!(
            app.busy.as_ref().map(|b| &b.action),
            Some(crate::busy::Action::CachePrune)
        ));
        assert_eq!(app.disk.before_prune, Some(100));
    }

    #[test]
    fn a_remeasure_after_a_prune_says_what_it_freed() {
        let mut app = one_project();
        app.disk.before_prune = Some(3_000_000);
        let measured = crate::disk::State {
            cache: Some(("/c".into(), Some(1_741_825))),
            ..Default::default()
        };
        // Another measurement landing mid-prune is not the answer.
        let _ = update(
            &mut app,
            Message::DiskMeasured(measured.clone(), false),
            unused_runner(),
        );
        assert!(app.toasts.is_empty());
        assert_eq!(app.disk.before_prune, Some(3_000_000));
        let _ = update(
            &mut app,
            Message::DiskMeasured(measured, true),
            unused_runner(),
        );
        assert_eq!(app.toasts[0].text, "Freed 1.2 MB");
        assert!(app.disk.before_prune.is_none());
    }

    #[test]
    fn a_failed_prune_reports_no_space_freed() {
        let mut app = one_project();
        app.disk.before_prune = Some(100);
        app.busy = Some(crate::busy::Busy::new(
            uuid::Uuid::nil(),
            crate::busy::Action::CachePrune,
        ));
        let _ = update(
            &mut app,
            Message::SyncFinished {
                success: false,
                lines: vec!["error: no".into()],
            },
            unused_runner(),
        );
        assert!(app.disk.before_prune.is_none());
        assert!(app.toasts[0].fix.is_none(), "no project to fix it in");
        assert!(!app.last_failure.contains_key(&uuid::Uuid::nil()));
    }

    #[test]
    fn the_boot_sweep_is_a_background_task() {
        let mut app = two_projects();
        app.path_exists = |_| false;
        let task = sweep_missing(&app);
        assert!(task.units() > 0);
        assert!(
            app.missing.is_empty(),
            "the render thread never looks at the disk"
        );
    }

    #[test]
    fn focus_rechecks_folders_with_nothing_selected() {
        let mut app = two_projects();
        app.selected = None;
        let task = update(&mut app, Message::WindowFocused, unused_runner());
        assert!(task.units() > 0, "a re-mounted drive clears its warning");
    }

    #[test]
    fn a_sweep_marks_and_clears() {
        let mut app = two_projects();
        let id = first_id(&app);
        let _ = update(&mut app, Message::MissingSwept(vec![id]), unused_runner());
        assert!(app.missing.contains(&id));
        let _ = update(&mut app, Message::MissingSwept(vec![]), unused_runner());
        assert!(app.missing.is_empty());
    }

    #[test]
    fn a_sweep_ignores_removed_projects() {
        let mut app = one_project();
        let gone = uuid::Uuid::new_v4();
        let _ = update(&mut app, Message::MissingSwept(vec![gone]), unused_runner());
        assert!(app.missing.is_empty());
    }

    #[test]
    fn opening_find_indexes_in_the_background() {
        let mut app = one_project();
        let task = update(&mut app, Message::FindOpened, unused_runner());
        assert!(task.units() > 0);
        assert!(matches!(&app.find, Some((q, None)) if q.is_empty()));
    }

    #[test]
    fn picking_a_hit_selects_opens_and_filters() {
        let mut app = two_projects();
        app.find = Some(("req".into(), Some(Vec::new())));
        let id = app.config.projects[1].id;
        let task = update(
            &mut app,
            Message::FindPicked(id, "requests".into()),
            unused_runner(),
        );
        assert_eq!(app.selected, Some(1));
        assert_eq!(app.tab, Tab::Dependencies);
        assert_eq!(app.dep_filter, "requests");
        assert!(app.find.is_none());
        assert!(task.units() > 0, "the project is read as a click would");
    }

    #[test]
    fn the_vulnerability_switch_is_saved() {
        let mut app = App::new(Config::default());
        let task = update(
            &mut app,
            Message::CheckVulnerabilitiesSelected(false),
            unused_runner(),
        );
        assert!(!app.config.settings.check_vulnerabilities);
        assert!(task.units() > 0);
    }

    #[test]
    fn just_recipes_land_on_their_project_and_show_loads_the_source() {
        let mut app = one_project();
        app.selected = Some(0);
        let id = first_id(&app);
        let _ = update(
            &mut app,
            Message::JustLoaded(id, run_tab::JustState::NotInstalled),
            unused_runner(),
        );
        assert!(matches!(&app.just, Some((p, run_tab::JustState::NotInstalled)) if *p == id));
        let task = update(
            &mut app,
            Message::JustShowRequested("build".into()),
            unused_runner(),
        );
        assert!(task.units() > 0);
        let _ = update(
            &mut app,
            Message::JustShown("build".into(), Ok("build:\n    cargo build".into())),
            unused_runner(),
        );
        assert_eq!(
            app.just_shown.as_ref().map(|(n, _)| n.as_str()),
            Some("build")
        );
        let _ = update(
            &mut app,
            Message::JustShowRequested("build".into()),
            unused_runner(),
        );
        assert!(app.just_shown.is_none(), "Show again closes it");
    }

    #[test]
    fn choosing_where_just_comes_from_is_saved_and_rereads_the_recipes() {
        let mut app = one_project();
        app.selected = Some(0);
        app.tab = Tab::Run;
        let task = update(
            &mut app,
            Message::JustSourceSelected(bombadil_core::model::JustSource::Bundled),
            unused_runner(),
        );
        assert_eq!(
            app.config.settings.just_source,
            bombadil_core::model::JustSource::Bundled
        );
        assert!(matches!(&app.just, Some((_, run_tab::JustState::Loading))));
        assert!(task.units() > 0);
    }

    #[test]
    fn opening_the_run_tab_also_reads_the_recipes() {
        let mut app = one_project();
        app.selected = Some(0);
        app.tab = Tab::Run;
        let _ = update(&mut app, Message::TabSelected(Tab::Run), unused_runner());
        assert!(matches!(&app.just, Some((_, run_tab::JustState::Loading))));
    }

    #[test]
    fn saving_a_command_adds_it_and_persists() {
        let mut app = one_project();
        app.selected = Some(0);
        let _ = update(
            &mut app,
            Message::CommandDraftLabel("tests".into()),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::CommandDraftCommand("pytest -q".into()),
            unused_runner(),
        );
        let task = update(&mut app, Message::CommandSaved, unused_runner());
        assert_eq!(app.config.projects[0].commands[0].command, "pytest -q");
        assert_eq!(app.config.projects[0].commands[0].label, "tests");
        assert!(task.units() > 0, "persisted");
        assert_eq!(app.command_draft.command, "", "the form clears");
    }

    #[test]
    fn editing_replaces_in_place() {
        let mut app = one_project();
        app.selected = Some(0);
        app.config.projects[0].commands = vec![
            bombadil_core::model::SavedCommand {
                label: "a".into(),
                command: "x".into(),
            },
            bombadil_core::model::SavedCommand {
                label: "b".into(),
                command: "y".into(),
            },
        ];
        let _ = update(&mut app, Message::CommandEdit(0), unused_runner());
        assert_eq!(app.command_draft.command, "x");
        let _ = update(
            &mut app,
            Message::CommandDraftCommand("z".into()),
            unused_runner(),
        );
        let _ = update(&mut app, Message::CommandSaved, unused_runner());
        assert_eq!(app.config.projects[0].commands[0].command, "z");
        assert_eq!(app.config.projects[0].commands.len(), 2);
    }

    #[test]
    fn removing_a_command_persists() {
        let mut app = one_project();
        app.selected = Some(0);
        app.config.projects[0].commands = vec![bombadil_core::model::SavedCommand {
            label: "a".into(),
            command: "x".into(),
        }];
        let task = update(&mut app, Message::CommandRemoved(0), unused_runner());
        assert!(app.config.projects[0].commands.is_empty());
        assert!(task.units() > 0);
    }

    #[test]
    fn a_blank_command_is_not_saved() {
        let mut app = one_project();
        app.selected = Some(0);
        let _ = update(
            &mut app,
            Message::CommandDraftCommand("   ".into()),
            unused_runner(),
        );
        let task = update(&mut app, Message::CommandSaved, unused_runner());
        assert!(app.config.projects[0].commands.is_empty());
        assert_eq!(task.units(), 0);
    }

    #[test]
    fn a_picked_file_is_saved_and_run() {
        let mut app = one_project();
        app.entries[0].environments[0].python_version = Some("3.12.0".into());
        let id = first_id(&app);
        let file = app.config.projects[0]
            .pyproject_path
            .parent()
            .unwrap()
            .join("main.py");
        let task = update(
            &mut app,
            Message::RunFilePicked(id, Some(file)),
            unused_runner(),
        );
        assert_eq!(app.config.projects[0].commands[0].command, "python main.py");
        assert!(app.runs[&id].live());
        assert!(task.units() > 0);
    }

    #[test]
    fn clicking_the_run_tab_reads_the_entry_points() {
        // A click on a tab arrives as the start of a drag.
        let mut app = one_project();
        app.selected = Some(0);
        let task = update(&mut app, Message::TabDragStarted(Tab::Run), unused_runner());
        assert!(task.units() > 0);
    }

    #[test]
    fn opening_the_run_tab_reads_the_entry_points() {
        let mut app = one_project();
        app.selected = Some(0);
        let task = update(&mut app, Message::TabSelected(Tab::Run), unused_runner());
        assert!(task.units() > 0);
    }

    #[test]
    fn a_live_run_keeps_the_clock_ticking() {
        let mut app = one_project();
        assert!(!runs_live(&app));
        let id = first_id(&app);
        let _ = start_run(&mut app, &["demo"]);
        assert!(runs_live(&app), "the elapsed time has to move");
        let _ = update(
            &mut app,
            Message::RunFinished(id, run_tab::RunEnd::Exited(Some(0))),
            unused_runner(),
        );
        assert!(!runs_live(&app));
    }

    #[test]
    fn a_run_takes_the_projects_lane_not_the_busy_slot() {
        let mut app = one_project();
        let task = start_run(&mut app, &["demo"]);
        assert!(task.units() > 0);
        assert!(app.busy.is_none(), "another project can still sync");
        assert!(app.runs[&first_id(&app)].live());
    }

    #[test]
    fn a_second_run_of_the_same_project_is_refused() {
        let mut app = one_project();
        let _ = start_run(&mut app, &["demo"]);
        let task = start_run(&mut app, &["other"]);
        assert_eq!(task.units(), 0);
        assert_eq!(app.runs[&first_id(&app)].label, "demo");
    }

    #[test]
    fn a_run_cannot_start_while_its_project_syncs() {
        let mut app = one_project();
        app.busy = Some(crate::busy::Busy::new(
            first_id(&app),
            crate::busy::Action::Sync,
        ));
        app.entries[0].environments[0].python_version = Some("3.12.0".into());
        assert!(run_blocked(&app, 0).is_some());
        let task = start_run(&mut app, &["demo"]);
        assert_eq!(task.units(), 0);
        assert!(app.runs.is_empty());
    }

    #[test]
    fn a_run_waits_for_an_environment() {
        let app = one_project();
        assert_eq!(
            run_blocked(&app, 0),
            Some("Sync first to create the environment.")
        );
    }

    #[test]
    fn lines_and_the_end_land_on_the_run() {
        let mut app = one_project();
        let id = first_id(&app);
        let _ = start_run(&mut app, &["demo"]);
        let _ = update(&mut app, Message::RunLine(id, "hi".into()), unused_runner());
        let _ = update(
            &mut app,
            Message::RunFinished(id, run_tab::RunEnd::Exited(Some(0))),
            unused_runner(),
        );
        let run = &app.runs[&id];
        assert_eq!(run.lines, ["hi"]);
        assert!(!run.live());
    }

    #[test]
    fn stop_sets_the_cancel_flag() {
        let mut app = one_project();
        let id = first_id(&app);
        let _ = start_run(&mut app, &["demo"]);
        let _ = update(&mut app, Message::RunStopRequested(id), unused_runner());
        assert!(
            app.runs[&id]
                .cancel
                .load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    #[test]
    fn syncing_a_running_project_asks_first() {
        let mut app = one_project();
        let id = first_id(&app);
        let _ = start_run(&mut app, &["demo"]);
        let task = update(&mut app, Message::SyncRequested(0), unused_runner());
        assert_eq!(task.units(), 0);
        assert!(app.busy.is_none());
        assert!(
            matches!(&app.confirm_stop, Some((p, m)) if *p == id && matches!(**m, Message::SyncRequested(0)))
        );
        let _ = update(&mut app, Message::StopThenConfirmed, unused_runner());
        assert!(
            app.runs[&id]
                .cancel
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        assert!(app.confirm_stop.is_none());
        let task = update(
            &mut app,
            Message::RunFinished(id, run_tab::RunEnd::Stopped),
            unused_runner(),
        );
        assert!(task.units() > 0, "the queued sync was dispatched");
    }

    #[test]
    fn recreating_a_running_project_asks_first() {
        let mut app = one_project();
        let id = first_id(&app);
        let _ = start_run(&mut app, &["demo"]);
        let _ = update(&mut app, Message::RecreateVenvRequested(0), unused_runner());
        assert!(app.confirm_recreate.is_none());
        assert!(matches!(&app.confirm_stop, Some((p, _)) if *p == id));
    }

    #[test]
    fn removing_a_project_stops_its_run() {
        let mut app = one_project();
        let id = first_id(&app);
        let _ = start_run(&mut app, &["demo"]);
        let cancel = app.runs[&id].cancel.clone();
        let _ = update(
            &mut app,
            Message::RemoveProjectRequested(0),
            unused_runner(),
        );
        assert!(cancel.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!app.runs.contains_key(&id));
    }

    #[test]
    fn quitting_with_a_run_asks_then_stops_it() {
        let mut app = one_project();
        let id = first_id(&app);
        let _ = start_run(&mut app, &["demo"]);
        let _ = update(
            &mut app,
            Message::Tray(crate::tray::TrayCommand::Quit),
            unused_runner(),
        );
        assert!(app.confirm_quit, "asked before anything was stopped");
        assert!(
            !app.runs[&id]
                .cancel
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        let _ = update(&mut app, Message::QuitConfirmed, unused_runner());
        assert!(app.quitting);
        assert!(
            app.runs[&id]
                .cancel
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        let task = update(
            &mut app,
            Message::RunFinished(id, run_tab::RunEnd::Stopped),
            unused_runner(),
        );
        assert!(task.units() > 0, "exits once nothing is running");
    }

    #[test]
    fn quitting_from_the_tray_with_the_window_hidden_shows_the_question() {
        let mut app = one_project();
        let _ = start_run(&mut app, &["demo"]);
        app.window = None;
        let task = update(
            &mut app,
            Message::Tray(crate::tray::TrayCommand::Quit),
            unused_runner(),
        );
        assert!(app.confirm_quit);
        assert!(task.units() > 0, "a window opens to ask in");
    }

    #[test]
    fn quit_confirmed_after_the_run_ended_on_its_own_exits() {
        let mut app = one_project();
        let id = first_id(&app);
        let _ = start_run(&mut app, &["demo"]);
        let _ = update(
            &mut app,
            Message::Tray(crate::tray::TrayCommand::Quit),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::RunFinished(id, run_tab::RunEnd::Exited(Some(0))),
            unused_runner(),
        );
        let task = update(&mut app, Message::QuitConfirmed, unused_runner());
        assert!(task.units() > 0, "nothing left to wait for");
    }

    #[test]
    fn dependency_changes_wait_for_a_running_program() {
        // uv would replace files the running program holds open -- on
        // Windows that fails half way and leaves the environment mixed.
        let mut app = one_project();
        let _ = start_run(&mut app, &["demo"]);
        let task = start_upgrade(&mut app, 0, None, unused_runner());
        assert_eq!(task.units(), 0);
        assert!(app.busy.is_none());
        assert!(
            app.toasts[0].text.contains("is running"),
            "{}",
            app.toasts[0].text
        );
        app.toasts.clear();
        let task = add_requirement(
            &mut app,
            0,
            "six==1.16.0".into(),
            deps::GroupKind::Main,
            unused_runner(),
        );
        assert_eq!(task.units(), 0);
        assert!(app.busy.is_none());
        assert!(app.toasts[0].text.contains("is running"));
    }

    #[test]
    fn output_is_capped() {
        let mut app = one_project();
        let id = first_id(&app);
        let _ = start_run(&mut app, &["demo"]);
        for n in 0..run_tab::MAX_LINES + 5 {
            let _ = update(
                &mut app,
                Message::RunLine(id, n.to_string()),
                unused_runner(),
            );
        }
        let run = &app.runs[&id];
        assert_eq!(run.lines.len(), run_tab::MAX_LINES);
        assert_eq!(
            run.lines.last().unwrap(),
            &(run_tab::MAX_LINES + 4).to_string()
        );
    }

    #[test]
    fn a_run_streams_through_the_cancellable_path_and_reports_its_exit() {
        let project = project("api");
        let config = Config {
            projects: vec![project.clone()],
            settings: fake_uv_settings(),
            ..Config::default()
        };
        let runner = FakeCommandRunner::new();
        version_probe(&runner);
        runner.expect(
            "uv",
            &["run", "--no-sync", "demo"],
            bombadil_core::process::CommandOutcome {
                status: Some(3),
                stdout: "hello\n".into(),
                stderr: String::new(),
            },
        );
        let mut lines = Vec::new();
        let end = run_program(
            &project,
            &config,
            &["demo".into()],
            &bombadil_core::secrets::InMemorySecretStore::new(),
            &runner,
            &std::sync::atomic::AtomicBool::new(false),
            &mut |l| lines.push(l),
        );
        assert_eq!(end, run_tab::RunEnd::Exited(Some(3)));
        assert_eq!(lines, ["hello"]);
        assert_eq!(runner.cancellable_calls(), 1, "a run must be stoppable");
    }

    #[test]
    fn a_run_asks_python_for_unbuffered_utf8_output() {
        // Piped, Python holds its output until 8 KB or exit, and on Windows
        // writes cp1252 -- a progress line every second would show nothing.
        let mut project = project("api");
        project.env.push(bombadil_core::model::EnvVar {
            key: "PYTHONIOENCODING".into(),
            value: "latin-1".into(),
            secret: false,
        });
        let config = Config {
            projects: vec![project.clone()],
            settings: fake_uv_settings(),
            ..Config::default()
        };
        let runner = FakeCommandRunner::new();
        version_probe(&runner);
        runner.expect(
            "uv",
            &["run"],
            bombadil_core::process::CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        let _ = run_program(
            &project,
            &config,
            &["x".into()],
            &bombadil_core::secrets::InMemorySecretStore::new(),
            &runner,
            &std::sync::atomic::AtomicBool::new(false),
            &mut |_| {},
        );
        let run = runner
            .calls()
            .into_iter()
            .find(|c| c.args.first().map(String::as_str) == Some("run"))
            .unwrap();
        assert_eq!(
            run.env.get("PYTHONUNBUFFERED").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            run.env.get("PYTHONIOENCODING").map(String::as_str),
            Some("latin-1"),
            "the project's own choice wins"
        );
    }

    #[test]
    fn a_cancelled_run_ends_as_stopped() {
        let project = project("api");
        let config = Config {
            projects: vec![project.clone()],
            settings: fake_uv_settings(),
            ..Config::default()
        };
        let runner = FakeCommandRunner::new();
        version_probe(&runner);
        runner.expect(
            "uv",
            &["run"],
            bombadil_core::process::CommandOutcome {
                status: None,
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        let end = run_program(
            &project,
            &config,
            &["x".into()],
            &bombadil_core::secrets::InMemorySecretStore::new(),
            &runner,
            &std::sync::atomic::AtomicBool::new(true),
            &mut |_| {},
        );
        assert_eq!(end, run_tab::RunEnd::Stopped);
    }

    #[test]
    fn a_new_version_is_installed_with_uv_add_in_the_rows_own_group() {
        // Without the group flag, uv adds a second copy of the requirement to
        // `main` and the group keeps its old pin.
        let project = project("api");
        let config = Config {
            projects: vec![project.clone()],
            settings: fake_uv_settings(),
            ..Config::default()
        };
        let runner = FakeCommandRunner::new();
        version_probe(&runner);
        runner.expect(
            "uv",
            &["add", "pytest==8.0.0", "--group", "test"],
            bombadil_core::process::CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let group = deps::uv_group(&deps::GroupKind::Group("test".into()));
        let (success, _) = collect_lines(|on_line| {
            run_in_project(
                &project,
                &config,
                &|uv, ctx| vec![uv::command::add(uv, ctx, "pytest==8.0.0", group.clone())],
                &bombadil_core::secrets::InMemorySecretStore::new(),
                &runner,
                on_line,
            )
        });
        assert!(success, "{:?}", runner.calls());
    }

    #[test]
    fn a_version_uv_cannot_install_says_which() {
        let mut app = App::new(Config::default());
        app.drawer.expanded = false;
        app.busy = Some(crate::busy::Busy::new(
            uuid::Uuid::nil(),
            crate::busy::Action::InstallVersion("fastapi==0.1.0".into()),
        ));

        let _ = update(
            &mut app,
            Message::SyncFinished {
                success: false,
                lines: vec!["No solution found when resolving dependencies".into()],
            },
            unused_runner(),
        );

        assert!(
            app.drawer
                .status
                .contains("Could not install fastapi==0.1.0"),
            "{}",
            app.drawer.status
        );
        assert!(
            app.busy.is_none(),
            "the next plain sync must not inherit it"
        );
    }

    #[test]
    fn an_invalid_version_keeps_the_cell_open_and_runs_nothing() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.selected = Some(0);
        app.dep_edit = Some(deps::DepEdit {
            project: 0,
            name: "fastapi".into(),
            group: deps::GroupKind::Main,
            draft: ">=1".into(),
        });
        let fake = Arc::new(FakeCommandRunner::new());

        let task = update(&mut app, Message::DepVersionSubmitted, fake.clone());

        assert_eq!(task.units(), 0);
        assert!(
            app.dep_edit.is_some(),
            "the user has to be able to fix the typo"
        );
        assert!(can_start(&app));
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

        let (success, lines) =
            collect_lines(|on_line| run_sync(&project, &config, &store, &runner, on_line));
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
    fn a_pinned_project_syncs_with_that_interpreter() {
        let project = Project {
            environments: vec![bombadil_core::model::Environment {
                location: VenvLocation::Alongside,
                python: PythonPin::Version("3.12".into()),
            }],
            active: VenvLocation::Alongside,
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

        let (success, lines) =
            collect_lines(|on_line| run_sync(&project, &config, &store, &runner, on_line));

        assert!(success, "got {lines:?}");
        let sync = runner
            .calls()
            .into_iter()
            .find(|c| c.args.first().map(String::as_str) == Some("sync"))
            .expect("a sync must have run");
        assert!(
            sync.args.windows(2).any(|w| w == ["-p", "3.12"]),
            "the pin must reach uv; got {:?}",
            sync.args
        );
    }

    #[test]
    fn a_probe_result_lands_on_the_environment_it_probed() {
        // One project now has several probes in flight at once, and they land
        // in whatever order the filesystem answers. Matching by position
        // instead of by location would pile every result onto the first row --
        // so a 3.11 environment would report the 3.12 one's version, and the
        // drift glyph would describe the wrong directory.
        let second = VenvLocation::Custom {
            path: std::path::PathBuf::from("/envs/api-311"),
        };
        let mut app = App::new(Config {
            projects: vec![Project {
                environments: vec![
                    bombadil_core::model::Environment {
                        location: VenvLocation::Alongside,
                        python: PythonPin::Unpinned,
                    },
                    bombadil_core::model::Environment {
                        location: second.clone(),
                        python: PythonPin::Unpinned,
                    },
                ],
                active: VenvLocation::Alongside,
                ..project("api")
            }],
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::VenvProbed {
                index: 0,
                location: second.clone(),
                python_version: Some("3.11.9".to_string()),
                requires_python: None,
            },
            unused_runner(),
        );

        let rows = &app.entries[0].environments;
        assert_eq!(
            rows.iter()
                .find(|row| row.location == second)
                .and_then(|row| row.python_version.clone()),
            Some("3.11.9".to_string()),
            "the probed environment must carry its own result"
        );
        assert_eq!(
            rows[0].python_version, None,
            "and no other environment may pick it up"
        );
    }

    #[test]
    fn a_probe_for_an_environment_the_project_no_longer_has_is_dropped() {
        // The same staleness guard every indexed message here carries: an
        // environment can be removed while its probe is in flight.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::VenvProbed {
                index: 0,
                location: VenvLocation::Custom {
                    path: std::path::PathBuf::from("/gone"),
                },
                python_version: Some("3.11.9".to_string()),
                requires_python: None,
            },
            unused_runner(),
        );

        assert!(
            app.entries[0]
                .environments
                .iter()
                .all(|row| row.python_version.is_none()),
            "a result for an environment that is gone must land nowhere"
        );
    }

    #[test]
    fn a_projects_own_env_vars_and_indexes_are_reachable_from_its_tabs() {
        // The split this application settled on: a tab is what *this project*
        // has, the Settings menu is what every project has. Env vars and
        // indexes have both halves, so they appear in both places showing
        // different things.
        //
        // Absorbing the per-project halves into Preferences was reported
        // twice as the tab having gone missing, which is what this holds
        // against a future tidy-up.
        for tab in [Tab::EnvVars, Tab::Indexes, Tab::Scripts] {
            assert!(
                Tab::ALL.contains(&tab),
                "{tab:?} is a per-project surface and belongs in the tab bar"
            );
        }
        assert_eq!(
            Tab::ALL.len(),
            8,
            "Dependencies, Members, Env vars, Scripts, Indexes, GitHub, Doctor, Run -- and \
             Settings deliberately not, since it has no per-project half"
        );
    }

    #[test]
    fn adding_an_environment_creates_nothing_until_a_folder_is_picked() {
        // Reported as: pressing add "simply appearing a new env that I don't
        // know how it was created". An environment *is* its directory, so
        // choosing that directory is the act of creating it -- inventing a
        // name and adding it silently tells the user nothing about where it
        // went, and cannot express the case this exists for: pointing at an
        // environment already on disk.
        //
        // What this proves is that the press creates nothing on its own;
        // `EnvironmentAddFolderPicked` is the only arm that adds. That *the
        // dispatched task is a folder picker* is not provable here -- a
        // `Task` is opaque, and dispatching an invented path would look
        // identical -- so it is confirmed by hand instead. Named for the
        // claim it actually carries rather than the one I wanted it to.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let id = app.config.projects[0].id;
        let before = app.config.projects[0].environments.len();

        let task = update(&mut app, Message::EnvironmentAdded(id), unused_runner());

        assert!(task.units() > 0, "the press must dispatch something");
        assert_eq!(
            app.config.projects[0].environments.len(),
            before,
            "nothing may be created before the user has said where"
        );
        assert!(
            app.entries[0].environments.len() == before,
            "and nothing may appear in the tree either"
        );
    }

    #[test]
    fn cancelling_the_folder_picker_adds_nothing() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let id = app.config.projects[0].id;
        let before = app.config.projects[0].environments.len();

        let task = update(
            &mut app,
            Message::EnvironmentAddFolderPicked(id, None),
            unused_runner(),
        );

        assert_eq!(app.config.projects[0].environments.len(), before);
        assert_eq!(task.units(), 0);
    }

    #[test]
    fn the_chosen_folder_is_where_the_environment_goes() {
        let chosen = std::path::PathBuf::from("/home/me/envs/shared");
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let id = app.config.projects[0].id;

        let _ = update(
            &mut app,
            Message::EnvironmentAddFolderPicked(id, Some(chosen.clone())),
            unused_runner(),
        );

        assert_eq!(
            app.config.projects[0].environments.last().unwrap().location,
            VenvLocation::Custom {
                path: chosen.clone()
            },
            "the environment must live exactly where the user pointed"
        );
        assert!(
            app.entries[0]
                .environments
                .iter()
                .any(|row| row.path == chosen),
            "and the tree must show that directory"
        );
    }

    #[test]
    fn choosing_a_folder_this_project_already_uses_adds_nothing() {
        // Two environments cannot share a directory -- that is what
        // identifies them -- so this is a no-op rather than a duplicate row
        // the user would then have to work out how to remove.
        let chosen = std::path::PathBuf::from("/home/me/envs/shared");
        let mut app = App::new(Config {
            projects: vec![Project {
                environments: vec![bombadil_core::model::Environment {
                    location: VenvLocation::Custom {
                        path: chosen.clone(),
                    },
                    python: PythonPin::Unpinned,
                }],
                active: VenvLocation::Custom {
                    path: chosen.clone(),
                },
                ..project("api")
            }],
            ..Config::default()
        });
        let id = app.config.projects[0].id;

        let task = update(
            &mut app,
            Message::EnvironmentAddFolderPicked(id, Some(chosen)),
            unused_runner(),
        );

        assert_eq!(app.config.projects[0].environments.len(), 1);
        assert_eq!(task.units(), 0);
    }

    #[test]
    fn an_added_environment_appears_in_the_sidebar_at_once() {
        // `app.entries` is what the tree renders; `app.config` is what is
        // persisted. Writing only the second is the bug the whole-branch
        // review already caught once, in `App::projects`: the file on disk is
        // right, the screen is wrong, and restarting "fixes" it -- which is
        // exactly why manual testing misses it.
        //
        // Reported as "the add environment button is not working".
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let id = app.config.projects[0].id;
        let before = app.entries[0].environments.len();

        let _ = update(
            &mut app,
            Message::EnvironmentAddFolderPicked(id, Some(std::path::PathBuf::from("/envs/new"))),
            unused_runner(),
        );

        assert_eq!(
            app.config.projects[0].environments.len(),
            before + 1,
            "the config must carry it"
        );
        assert_eq!(
            app.entries[0].environments.len(),
            before + 1,
            "and the sidebar must show it without waiting for a restart"
        );
    }

    #[test]
    fn a_removed_environment_leaves_the_sidebar_at_once() {
        let spare = VenvLocation::Custom {
            path: std::path::PathBuf::from("/envs/spare"),
        };
        let mut app = App::new(Config {
            projects: vec![Project {
                environments: vec![
                    bombadil_core::model::Environment {
                        location: VenvLocation::Alongside,
                        python: PythonPin::Unpinned,
                    },
                    bombadil_core::model::Environment {
                        location: spare.clone(),
                        python: PythonPin::Unpinned,
                    },
                ],
                active: VenvLocation::Alongside,
                ..project("api")
            }],
            ..Config::default()
        });
        let id = app.config.projects[0].id;

        let _ = update(
            &mut app,
            Message::EnvironmentRemoveRequested(id, spare.clone()),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::EnvironmentRemovalConfirmed,
            unused_runner(),
        );

        assert_eq!(app.entries[0].environments.len(), 1);
        assert!(
            app.entries[0]
                .environments
                .iter()
                .all(|row| row.location != spare),
            "the removed environment must be gone from the tree too"
        );
    }

    #[test]
    fn rebuilding_the_sidebar_keeps_versions_already_probed() {
        // A rebuild that dropped them would blank every row to "not created"
        // on each add, and they would only come back when the probes landed.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let id = app.config.projects[0].id;
        set_probed_version(&mut app, 0, "3.12.13");

        let _ = update(
            &mut app,
            Message::EnvironmentAddFolderPicked(id, Some(std::path::PathBuf::from("/envs/new"))),
            unused_runner(),
        );

        assert_eq!(
            app.entries[0]
                .environments
                .iter()
                .find(|row| row.active)
                .and_then(|row| row.python_version.clone()),
            Some("3.12.13".to_string()),
            "an environment already probed must not blank on an unrelated add"
        );
    }

    #[test]
    fn a_new_environment_starts_from_the_global_pin() {
        // `Settings::python` stops being inherited at sync time and becomes
        // the pin a new environment is created with -- the changed meaning
        // this design gives it, written as a test so it cannot quietly revert.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            settings: Settings {
                python: PythonPin::Version("3.12".into()),
                ..Settings::default()
            },
            ..Config::default()
        });
        let id = app.config.projects[0].id;
        let before = app.config.projects[0].environments.len();

        let _ = update(
            &mut app,
            Message::EnvironmentAddFolderPicked(id, Some(std::path::PathBuf::from("/envs/new"))),
            unused_runner(),
        );

        assert_eq!(app.config.projects[0].environments.len(), before + 1);
        assert_eq!(
            app.config.projects[0].environments.last().unwrap().python,
            PythonPin::Version("3.12".into())
        );
    }

    #[test]
    fn the_last_environment_can_be_removed_and_the_project_stays() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let id = app.config.projects[0].id;
        let _ = update(
            &mut app,
            Message::EnvironmentRemoveRequested(id, VenvLocation::Alongside),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::EnvironmentRemovalConfirmed,
            unused_runner(),
        );
        assert_eq!(app.config.projects.len(), 1);
        assert!(app.config.projects[0].environments.is_empty());
        assert!(app.entries[0].environments.is_empty());
    }

    #[test]
    fn two_environments_of_one_project_never_share_a_directory() {
        // The location is what identifies an environment, so a collision
        // would make two rows the same row -- and whichever the user pressed,
        // Sync would build one directory.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let id = app.config.projects[0].id;

        for n in 0..3 {
            let _ = update(
                &mut app,
                Message::EnvironmentAddFolderPicked(
                    id,
                    Some(std::path::PathBuf::from(format!("/envs/api-{n}"))),
                ),
                unused_runner(),
            );
        }

        let locations: Vec<_> = app.config.projects[0]
            .environments
            .iter()
            .map(|environment| environment.location.clone())
            .collect();
        for (i, location) in locations.iter().enumerate() {
            assert!(
                !locations[i + 1..].contains(location),
                "two environments landed on {location:?}"
            );
        }
    }

    #[test]
    fn moving_an_environment_onto_a_taken_directory_is_refused() {
        // Rather than silently merging the two, which would lose one of them
        // along with the account of what is installed in it.
        let taken = VenvLocation::Custom {
            path: std::path::PathBuf::from("/envs/taken"),
        };
        let mut app = App::new(Config {
            projects: vec![Project {
                environments: vec![
                    bombadil_core::model::Environment {
                        location: VenvLocation::Alongside,
                        python: PythonPin::Unpinned,
                    },
                    bombadil_core::model::Environment {
                        location: taken.clone(),
                        python: PythonPin::Unpinned,
                    },
                ],
                active: VenvLocation::Alongside,
                ..project("api")
            }],
            ..Config::default()
        });
        let id = app.config.projects[0].id;

        let task = update(
            &mut app,
            Message::EnvironmentLocationSelected(id, VenvLocation::Alongside, taken.clone()),
            unused_runner(),
        );

        assert_eq!(task.units(), 0);
        assert_eq!(
            app.config.projects[0].environments.len(),
            2,
            "neither environment may be lost to the collision"
        );
        assert_eq!(
            app.config.projects[0].environments[0].location,
            VenvLocation::Alongside,
            "the one that was moved must stay where it is"
        );
    }

    #[test]
    fn removing_an_environment_asks_first() {
        // The same act as Recreate venv: an environment stops being one this
        // application keeps an account of.
        let spare = VenvLocation::Custom {
            path: std::path::PathBuf::from("/envs/spare"),
        };
        let mut app = App::new(Config {
            projects: vec![Project {
                environments: vec![
                    bombadil_core::model::Environment {
                        location: VenvLocation::Alongside,
                        python: PythonPin::Unpinned,
                    },
                    bombadil_core::model::Environment {
                        location: spare.clone(),
                        python: PythonPin::Unpinned,
                    },
                ],
                active: VenvLocation::Alongside,
                ..project("api")
            }],
            ..Config::default()
        });
        let id = app.config.projects[0].id;

        let task = update(
            &mut app,
            Message::EnvironmentRemoveRequested(id, spare),
            unused_runner(),
        );

        assert_eq!(task.units(), 0, "nothing may go before the user answers");
        assert_eq!(
            app.config.projects[0].environments.len(),
            2,
            "it must still be there until confirmed"
        );
        assert!(app.confirm_environment_removal.is_some());
    }

    #[test]
    fn removing_the_active_environment_moves_active_to_a_survivor() {
        // `active` names a location. Left pointing at the removed one it
        // would name nothing, and the sidebar's active marker would disagree
        // with what Sync builds.
        let spare = VenvLocation::Custom {
            path: std::path::PathBuf::from("/envs/spare"),
        };
        let mut app = App::new(Config {
            projects: vec![Project {
                environments: vec![
                    bombadil_core::model::Environment {
                        location: VenvLocation::Alongside,
                        python: PythonPin::Unpinned,
                    },
                    bombadil_core::model::Environment {
                        location: spare.clone(),
                        python: PythonPin::Unpinned,
                    },
                ],
                active: VenvLocation::Alongside,
                ..project("api")
            }],
            ..Config::default()
        });
        let id = app.config.projects[0].id;

        let _ = update(
            &mut app,
            Message::EnvironmentRemoveRequested(id, VenvLocation::Alongside),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::EnvironmentRemovalConfirmed,
            unused_runner(),
        );

        assert_eq!(app.config.projects[0].environments.len(), 1);
        assert_eq!(
            app.config.projects[0].active, spare,
            "active must name an environment that still exists"
        );
    }

    #[test]
    fn declining_a_removal_removes_nothing() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.confirm_environment_removal = Some(context_menu::ConfirmEnvironmentRemoval {
            project_id: app.config.projects[0].id,
            label: "api".into(),
            location: VenvLocation::Alongside,
            venv_path: std::path::PathBuf::from("/p/api/.venv"),
        });
        let before = app.config.projects[0].environments.len();

        let task = update(
            &mut app,
            Message::EnvironmentRemovalCancelled,
            unused_runner(),
        );

        assert_eq!(task.units(), 0);
        assert!(app.confirm_environment_removal.is_none());
        assert_eq!(app.config.projects[0].environments.len(), before);
    }

    #[test]
    fn collapsing_a_project_persists() {
        // A config field rather than view state: a project you collapse stays
        // collapsed across a restart.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        assert!(app.config.projects[0].expanded, "projects start expanded");

        let task = update(&mut app, Message::ProjectExpandToggled(0), unused_runner());

        assert!(!app.config.projects[0].expanded);
        assert!(!app.entries[0].expanded, "the sidebar must follow at once");
        assert!(task.units() > 0, "a persisted setting must be persisted");
    }

    #[test]
    fn activating_an_environment_moves_what_sync_acts_on() {
        let target = VenvLocation::Custom {
            path: std::path::PathBuf::from("/envs/api-311"),
        };
        let mut app = App::new(Config {
            projects: vec![Project {
                environments: vec![
                    bombadil_core::model::Environment {
                        location: VenvLocation::Alongside,
                        python: PythonPin::Unpinned,
                    },
                    bombadil_core::model::Environment {
                        location: target.clone(),
                        python: PythonPin::Version("3.11".into()),
                    },
                ],
                active: VenvLocation::Alongside,
                ..project("api")
            }],
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::EnvironmentActivated(0, target.clone()),
            unused_runner(),
        );

        assert_eq!(app.config.projects[0].active, target);
        assert_eq!(
            app.config.projects[0]
                .active_environment()
                .map(|e| e.python.clone()),
            Some(PythonPin::Version("3.11".into())),
            "what Sync pins from must move with it"
        );
        assert!(
            app.entries[0]
                .environments
                .iter()
                .any(|row| row.active && row.location == target),
            "exactly the pressed row must read as active in the sidebar"
        );
        assert_eq!(
            app.entries[0]
                .environments
                .iter()
                .filter(|row| row.active)
                .count(),
            1,
            "one active environment, never two"
        );
    }

    #[test]
    fn pressing_the_environment_a_project_already_uses_still_selects_it() {
        // Reported as: clicking an environment in one project then one in
        // another "is not changing (maybe because the second project only has
        // one virtual environment)". Exactly that. The row was pressable only
        // when it was *not* already the default, so a project with one
        // environment had a permanently dead row -- and since the press is
        // also what selects the project, that project could never be reached
        // by clicking its environment at all.
        let only = VenvLocation::Custom {
            path: std::path::PathBuf::from("/envs/web"),
        };
        let mut app = App::new(Config {
            projects: vec![
                project("api"),
                Project {
                    id: uuid::Uuid::from_u128(2),
                    environments: vec![bombadil_core::model::Environment {
                        location: only.clone(),
                        python: PythonPin::Unpinned,
                    }],
                    active: only.clone(),
                    ..project("web")
                },
            ],
            ..Config::default()
        });
        app.selected = Some(0);

        // The environment is already this project's default -- the press is
        // a no-op for `active` and the whole point for `selected`.
        let task = update(
            &mut app,
            Message::EnvironmentActivated(1, only.clone()),
            unused_runner(),
        );

        assert_eq!(
            app.selected,
            Some(1),
            "pressing a project's only environment must still reach that project"
        );
        assert_eq!(app.config.projects[1].active, only);
        assert!(task.units() > 0, "and re-read its dependencies");
    }

    #[test]
    fn activating_an_environment_selects_its_project_too() {
        // Reported as clicking between environments "not changing the
        // project, and dependencies and everything else". Pressing one of a
        // project's environments is a statement about that project; leaving
        // the selection where it was meant the header, the tabs and the
        // re-read all described a different one, so the press looked inert.
        let target = VenvLocation::Custom {
            path: std::path::PathBuf::from("/envs/web"),
        };
        let mut app = App::new(Config {
            projects: vec![
                project("api"),
                Project {
                    id: uuid::Uuid::from_u128(2),
                    environments: vec![bombadil_core::model::Environment {
                        location: target.clone(),
                        python: PythonPin::Unpinned,
                    }],
                    active: target.clone(),
                    ..project("web")
                },
            ],
            ..Config::default()
        });
        app.selected = Some(0);

        let task = update(
            &mut app,
            Message::EnvironmentActivated(1, target),
            unused_runner(),
        );

        assert_eq!(
            app.selected,
            Some(1),
            "the pressed environment's project must become the selected one"
        );
        assert!(
            task.units() > 0,
            "and its dependencies and members must be re-read"
        );
    }

    #[test]
    fn activating_an_environment_a_project_does_not_have_changes_nothing() {
        // `active` naming a location the project does not have would leave
        // every caller falling back to the first, which looks like the click
        // did something arbitrary.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let before = app.config.projects[0].active.clone();

        let task = update(
            &mut app,
            Message::EnvironmentActivated(
                0,
                VenvLocation::Custom {
                    path: std::path::PathBuf::from("/not/ours"),
                },
            ),
            unused_runner(),
        );

        assert_eq!(app.config.projects[0].active, before);
        assert_eq!(task.units(), 0);
    }

    #[test]
    fn sync_pins_from_the_active_environment_not_another_one() {
        // The property the whole feature rests on. A project with a 3.11 and
        // a 3.12 environment must build each with its own interpreter -- and
        // uv *deletes* an environment whose version differs from `-p`, so
        // reading the wrong one destroys the wrong directory.
        let target = VenvLocation::Custom {
            path: std::path::PathBuf::from("/envs/api-312"),
        };
        let project = Project {
            environments: vec![
                bombadil_core::model::Environment {
                    location: VenvLocation::Alongside,
                    python: PythonPin::Version("3.11".into()),
                },
                bombadil_core::model::Environment {
                    location: target.clone(),
                    python: PythonPin::Version("3.12".into()),
                },
            ],
            active: target,
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

        let (success, lines) =
            collect_lines(|on_line| run_sync(&project, &config, &store, &runner, on_line));

        assert!(success, "got {lines:?}");
        let sync = runner
            .calls()
            .into_iter()
            .find(|c| c.args.first().map(String::as_str) == Some("sync"))
            .expect("a sync must have run");
        assert!(
            sync.args.windows(2).any(|w| w == ["-p", "3.12"]),
            "the ACTIVE environment's pin must reach uv; got {:?}",
            sync.args
        );
        assert!(
            !sync.args.iter().any(|a| a == "3.11"),
            "the inactive environment's pin must not; got {:?}",
            sync.args
        );
    }

    #[test]
    fn an_unpinned_project_syncs_with_no_interpreter_argument() {
        // The pin is opt-in. An unpinned project's command must be what it
        // was before this feature existed -- every project is unpinned until
        // someone sets one, so this is the path almost everyone is on.
        let project = project("api");
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

        let _ = collect_lines(|on_line| run_sync(&project, &config, &store, &runner, on_line));

        let sync = runner
            .calls()
            .into_iter()
            .find(|c| c.args.first().map(String::as_str) == Some("sync"))
            .expect("a sync must have run");
        assert!(
            !sync.args.iter().any(|a| a == "-p"),
            "an unpinned project must not gain an interpreter argument; got {:?}",
            sync.args
        );
    }

    #[test]
    fn syncing_a_venv_that_is_off_its_pin_would_delete_it() {
        // Verified against uv 0.12.1: `uv sync -p 3.12` on a 3.13 venv prints
        // "Removed virtual environment at: .venv". Everything outside the
        // lock file is gone. That is the same class of action as Recreate
        // venv, which this application has always guarded.
        assert!(sync_would_delete(
            &PythonPin::Version("3.12".into()),
            Some("3.13.14")
        ));
    }

    #[test]
    fn syncing_a_venv_that_is_on_its_pin_deletes_nothing() {
        // `pyvenv.cfg` records `3.12` for a venv built with `-p 3.12`. Reading
        // that as a mismatch would put a confirmation in front of every
        // ordinary sync, and answering it would delete a working environment.
        assert!(!sync_would_delete(
            &PythonPin::Version("3.12.13".into()),
            Some("3.12")
        ));
    }

    #[test]
    fn syncing_without_a_pin_or_without_a_venv_deletes_nothing() {
        // No pin: uv is passed no `-p` and removes nothing. No venv: this is
        // the ordinary create path, and confirming the creation of something
        // that does not exist teaches the user to click through the one that
        // matters.
        assert!(!sync_would_delete(&PythonPin::Unpinned, Some("3.13.14")));
        assert!(!sync_would_delete(&PythonPin::Version("3.12".into()), None));
    }

    #[test]
    fn pressing_sync_on_a_venv_off_its_pin_asks_before_it_deletes() {
        // The whole point of the task: the press opens a confirmation instead
        // of dispatching, and dispatches nothing at all until it is answered.
        let project = Project {
            environments: vec![bombadil_core::model::Environment {
                location: VenvLocation::Alongside,
                python: PythonPin::Version("3.12".into()),
            }],
            active: VenvLocation::Alongside,
            ..project("api")
        };
        let mut app = App::new(Config {
            projects: vec![project],
            ..Config::default()
        });
        set_probed_version(&mut app, 0, "3.13.14");

        let task = update(&mut app, Message::SyncRequested(0), unused_runner());

        assert_eq!(task.units(), 0, "nothing may run before the user answers");
        assert!(can_start(&app), "the in-flight slot must not be taken");
        let confirm = app
            .confirm_pin_change
            .as_ref()
            .expect("the press must open the confirmation");
        assert_eq!(confirm.from, "3.13.14");
        assert_eq!(confirm.to, "3.12");
    }

    #[test]
    fn pressing_sync_on_a_venv_that_matches_its_pin_just_syncs() {
        // The other half: the guard must not stand in front of an ordinary
        // sync. A confirmation on every press is one the user stops reading.
        let project = Project {
            environments: vec![bombadil_core::model::Environment {
                location: VenvLocation::Alongside,
                python: PythonPin::Version("3.12".into()),
            }],
            active: VenvLocation::Alongside,
            ..project("api")
        };
        let mut app = App::new(Config {
            projects: vec![project],
            ..Config::default()
        });
        set_probed_version(&mut app, 0, "3.12");

        let task = update(&mut app, Message::SyncRequested(0), unused_runner());

        assert!(task.units() > 0, "the sync must dispatch");
        assert!(app.confirm_pin_change.is_none(), "nothing to confirm");
    }

    #[test]
    fn declining_the_pin_change_runs_nothing() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.confirm_pin_change = Some(context_menu::ConfirmPinChange {
            project_id: app.config.projects[0].id,
            label: "api".into(),
            venv_path: std::path::PathBuf::from("/p/api/.venv"),
            from: "3.13.14".into(),
            to: "3.12".into(),
        });

        let task = update(&mut app, Message::PinChangeCancelled, unused_runner());

        assert_eq!(task.units(), 0, "declining must not delete anything");
        assert!(app.confirm_pin_change.is_none());
        assert!(can_start(&app));
    }

    #[test]
    fn a_sync_runs_its_uv_command_through_stream_not_run() {
        // Spec §8 asks the drawer to stream live output. `CommandRunner::run`
        // calls `Command::output()`, which waits for the child to exit and
        // hands back one batch -- so a sync that used it could not stream
        // whatever the drawer did with the result.
        //
        // Asserted on the fake's stream counter, not on `Task::units()`:
        // several tests here use `units() > 0` to mean "a job was
        // dispatched", and that cannot tell which seam the job went through.
        let project = project("api");
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
                stdout: "Installed 3 packages".into(),
                stderr: String::new(),
            },
        );

        let (success, _lines) =
            collect_lines(|on_line| run_sync(&project, &config, &store, &runner, on_line));

        assert!(success);
        assert_eq!(
            runner.streaming_calls(),
            1,
            "the `uv sync` itself must go through `stream`; got {:?}",
            runner.calls()
        );
    }

    #[test]
    fn the_sync_stream_emits_one_message_per_line_then_a_terminal_one() {
        // The shape `update` depends on: lines arrive one at a time while the
        // command runs, and exactly one `SyncFinished` closes the run. A
        // wrapper that gathered the lines and sent them with the terminal
        // message would leave the drawer blank for the whole sync, which is
        // the bug this plan exists to fix.
        let got: Vec<Message> = futures::executor::block_on(
            sync_stream(
                |on_line| {
                    on_line("Resolved 3 packages".to_string());
                    on_line("Installed 3 packages".to_string());
                    (true, Vec::new())
                },
                "timed out",
                "failed",
            )
            .collect(),
        );

        let rendered: Vec<String> = got.iter().map(|m| format!("{m:?}")).collect();
        assert_eq!(got.len(), 3, "got {rendered:?}");
        assert!(
            matches!(&got[0], Message::SyncLine(line) if line == "Resolved 3 packages"),
            "got {rendered:?}"
        );
        assert!(
            matches!(&got[1], Message::SyncLine(line) if line == "Installed 3 packages"),
            "got {rendered:?}"
        );
        assert!(
            matches!(
                &got[2],
                Message::SyncFinished { success: true, lines } if lines.is_empty()
            ),
            "the terminal message must not repeat the lines already streamed; got {rendered:?}"
        );
    }

    #[test]
    fn a_streamed_line_is_redacted_before_it_becomes_a_message() {
        // Not "the drawer holds no secret" -- that is the test above, and it
        // would still pass if redaction happened at the end. `Message` derives
        // `Debug`, so a secret must never be in the string a `SyncLine` is
        // built from: this asserts on the line as `compose_and_stream` hands
        // it over, which is the moment before the message exists.
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
            "s3cr3t-streamed-value",
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
                stderr: "error: 401 for index (token: s3cr3t-streamed-value)".into(),
            },
        );

        let mut streamed = Vec::new();
        let (success, _lines) = run_sync(&project, &config, &store, &runner, &mut |line| {
            streamed.push(line)
        });

        assert!(!success);
        assert!(
            !streamed.is_empty(),
            "the failing line must reach the drawer, not be swallowed"
        );
        assert!(
            streamed
                .iter()
                .all(|l| !l.contains("s3cr3t-streamed-value")),
            "a keychain secret reached the callback unredacted: {streamed:?}"
        );
        assert!(
            streamed.iter().any(|l| l.contains("***")),
            "the line must still be shown, redacted; got {streamed:?}"
        );
    }

    #[test]
    fn a_finished_sync_refreshes_what_it_just_changed() {
        // A sync installs and removes packages and can create the environment
        // itself. Without a re-read the Dependencies tab goes on listing what
        // was installed *before* it, and the sidebar goes on saying "no env"
        // for an environment that now exists -- both wrong until restart.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            settings: fake_uv_settings(),
            ..Config::default()
        });
        app.selected = Some(0);
        app.deps = Some((0, Err("stale".to_string())));

        let task = update(
            &mut app,
            Message::SyncFinished {
                success: true,
                lines: Vec::new(),
            },
            unused_runner(),
        );

        assert!(
            task.units() > 0,
            "a finished sync must dispatch the re-reads it invalidated"
        );
        assert!(can_start(&app), "the slot must still be released");
    }

    #[test]
    fn a_failed_sync_refreshes_too() {
        // A failed sync is not an unchanged one: uv installs what it can
        // resolve before it hits the package it cannot, so the tab has to
        // describe the environment as it now is rather than as it was before
        // an attempt that partly landed.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            settings: fake_uv_settings(),
            ..Config::default()
        });
        app.selected = Some(0);

        let task = update(
            &mut app,
            Message::SyncFinished {
                success: false,
                lines: vec!["error: no solution found".to_string()],
            },
            unused_runner(),
        );

        assert!(task.units() > 0, "a failed sync must refresh as well");
    }

    #[test]
    fn a_finished_sync_with_nothing_selected_still_reprobes_the_sidebar() {
        // No selection means no tab to refresh, but the sidebar shows every
        // project and a sync can have created one of their environments.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            settings: fake_uv_settings(),
            ..Config::default()
        });
        app.selected = None;

        let task = update(
            &mut app,
            Message::SyncFinished {
                success: true,
                lines: Vec::new(),
            },
            unused_runner(),
        );

        assert!(
            task.units() > 0,
            "the venv probe covers every project, selected or not"
        );
    }

    #[test]
    fn a_failed_sync_keeps_a_streamed_transcript_visible() {
        // The streaming counterpart of
        // `a_failed_sync_keeps_the_transcript_visible`. That one hands the
        // transcript to `SyncFinished`; a streamed run's `SyncFinished`
        // carries no lines at all, because they arrived as `SyncLine` while
        // uv was running. The expand-on-failure check reads
        // `app.drawer.lines`, not the message -- if it had read the message,
        // streaming would have silently undone plan 3a's fix and every failed
        // sync would show "sync failed" with nothing under it.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        for message in [
            Message::SyncLine("Resolved 12 packages".to_string()),
            Message::SyncLine("error: no solution found".to_string()),
            Message::SyncFinished {
                success: false,
                lines: Vec::new(),
            },
        ] {
            let _ = update(&mut app, message, unused_runner());
        }

        // The drawer starts collapsed now; the transcript must still be
        // there for the toast's "Show output".

        assert_eq!(
            app.drawer.lines,
            vec!["Resolved 12 packages", "error: no solution found"],
            "the streamed lines must be kept, in arrival order"
        );
        assert_eq!(app.drawer.status, "sync failed");
    }

    #[test]
    fn a_line_arriving_mid_sync_is_shown_immediately() {
        // The drawer must not wait for `SyncFinished` to show anything: for
        // the length of a sync, what `SyncLine` has put there is all the user
        // has. (The cap and the drop-the-oldest rule are `push_line`'s, and
        // `drawer`'s own tests cover them -- this only proves `update` routes
        // through it.)
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::SyncLine("Resolved 12 packages".to_string()),
            unused_runner(),
        );

        // Collected whether or not the drawer is open, ready for when it is.
        assert_eq!(app.drawer.lines, vec!["Resolved 12 packages"]);
    }

    #[test]
    fn a_project_env_var_typed_this_session_reaches_the_very_next_sync() {
        // `App` used to hold a startup clone of `config.projects` alongside
        // `config.projects` itself. Every editor wrote the config; Sync, Open
        // terminal and Recreate venv read the clone -- so a variable typed
        // this session was simply absent from the environment uv ran in, an
        // index credential was never injected (a 401 the drawer could not
        // explain), and a pre-activate script never ran. Restarting "fixed"
        // all of it, which is exactly why manual testing never caught it.
        //
        // Nothing in `App` distinguishes the two: `syncing` holds the same
        // id and the drawer the same label either way. The resolution itself
        // is the last observable point, so this asserts on the `Project` the
        // press resolves and hands it to the same `run_sync` the dispatched
        // job runs.
        let config = Config {
            projects: vec![project("api")],
            settings: fake_uv_settings(),
            ..Config::default()
        };
        let mut app = App::new(config);

        for message in [
            Message::EnvVarAdded(env_editor::EnvScope::Project(0)),
            Message::EnvVarKeyChanged(env_editor::EnvScope::Project(0), 0, "MARKER".into()),
            Message::EnvVarValueChanged(env_editor::EnvScope::Project(0), 0, "set".into()),
            // Typing alone leaves the value in `App::env_value_drafts`; this
            // is the press that puts it in `Config`, where `envspec::compose`
            // reads it from.
            Message::EnvVarValueCommitted(env_editor::EnvScope::Project(0), 0),
        ] {
            let _ = update(&mut app, message, unused_runner());
        }

        let task = update(&mut app, Message::SyncRequested(0), unused_runner());
        assert!(task.units() > 0, "the Sync press must dispatch real work");

        let target = sync_target(&app, 0).expect("the press must resolve a project");
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

        let (success, lines) =
            collect_lines(|on_line| run_sync(&target, &app.config, &store, &runner, on_line));
        assert!(success, "got {lines:?}");

        let calls = runner.calls();
        let sync = calls
            .iter()
            .find(|c| c.args.iter().any(|a| a == "sync"))
            .expect("uv sync must have been run");
        assert_eq!(
            sync.env.get("MARKER").map(String::as_str),
            Some("set"),
            "a variable typed this session must be in the environment the very next sync \
             runs in, not only after a restart; got {:?}",
            sync.env.keys().collect::<Vec<_>>()
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

        let (success, lines) =
            collect_lines(|on_line| run_sync(&project, &config, &store, &runner, on_line));

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
            Message::AddProjectInterpretersLoaded(Ok(interpreters.clone())),
            unused_runner(),
        );

        assert_eq!(
            app.add_project.as_ref().map(|d| d.interpreters.clone()),
            Some(add_project::Interpreters::Loaded(interpreters))
        );
    }

    #[test]
    fn a_failed_interpreter_fetch_reaches_the_draft_as_a_failure_not_an_empty_list() {
        // The dead end this fix exists for: every failure used to arrive as
        // `vec![]`, which the dialog cannot tell apart from a fetch that has
        // not landed yet -- so it showed "(checking available
        // interpreters...)" forever, with no error and no way forward.
        let mut app = App::new(Config::default());
        app.add_project = Some(manifest_draft(""));

        let _ = update(
            &mut app,
            Message::AddProjectInterpretersLoaded(Err("no uv binary is available".to_string())),
            unused_runner(),
        );

        assert_eq!(
            app.add_project.as_ref().map(|d| d.interpreters.clone()),
            Some(add_project::Interpreters::Failed(
                "no uv binary is available".to_string()
            )),
            "a failed fetch must be representable on the draft, not collapse into Pending"
        );
    }

    #[test]
    fn an_unresolvable_uv_fails_the_interpreter_fetch_with_its_own_message() {
        // The producer half: `list_interpreters_blocking` is what could not
        // express failure before. A `Custom` pin the fake runner has no rule
        // for fails to probe -- `uv::resolve`'s actionable text is what has
        // to survive, the same discipline `run_sync` follows.
        let config = Config {
            settings: bombadil_core::model::Settings {
                uv_source: bombadil_core::model::UvSource::Custom {
                    path: PathBuf::from("/definitely/not/a/real/uv"),
                },
                ..Default::default()
            },
            ..Config::default()
        };

        let got = list_interpreters_blocking(&config, &FakeCommandRunner::new());

        let err = got.expect_err("an unresolvable uv must not look like an empty machine");
        assert!(
            err.contains("/definitely/not/a/real/uv"),
            "the resolve error's own text must survive; got {err}"
        );
    }

    #[test]
    fn unparseable_interpreter_output_fails_rather_than_reading_as_no_interpreters() {
        // uv ran and exited zero but this build cannot read what it printed
        // -- a version skew, not an empty machine, and the two must not look
        // the same to the user.
        let config = Config {
            settings: fake_uv_settings(),
            ..Config::default()
        };
        let runner = FakeCommandRunner::new();
        version_probe(&runner);
        runner.expect(
            "uv",
            &["python", "list", "--output-format", "json"],
            bombadil_core::process::CommandOutcome {
                status: Some(0),
                stdout: "not json at all".into(),
                stderr: String::new(),
            },
        );

        let got = list_interpreters_blocking(&config, &runner);

        assert!(
            got.is_err(),
            "unparseable output must be reported, not silently emptied; got {got:?}"
        );
    }

    #[test]
    fn changing_the_venv_choice_dispatches_an_adoption_probe_without_touching_disk_inline() {
        // Spec §8's "the dialog states that it will be adopted" needs the
        // probe to run *before* the user confirms, and it reads a
        // `pyvenv.cfg` -- so it goes through `job::run` like every other
        // filesystem read here, never inline in `update` and never in `view`.
        let mut app = App::new(Config::default());
        let mut draft = manifest_draft("");
        draft.venv_outcome = Some(add_project::VenvOutcome::Adopt {
            python_version: "3.9.18".to_string(),
        });
        app.add_project = Some(draft);

        let task = update(
            &mut app,
            Message::AddProjectVenvChoiceSelected(add_project::VenvChoice::Chosen(PathBuf::from(
                "/mnt/fast/envs/api",
            ))),
            unused_runner(),
        );

        assert!(
            task.units() > 0,
            "changing the venv choice must dispatch a fresh adopt-or-create probe"
        );
        assert_eq!(
            app.add_project
                .as_ref()
                .and_then(|d| d.venv_outcome.clone()),
            None,
            "the previous folder's statement must be cleared, not left on screen"
        );
    }

    #[test]
    fn opening_the_dialog_probes_the_default_venv_location_too() {
        // Accepting the default without touching the venv control is the
        // commonest path; leaving it unprobed would leave exactly that user
        // with no adoption statement.
        let mut app = App::new(Config::default());

        let task = update(
            &mut app,
            Message::AddProjectManifestLoaded(manifest_draft("")),
            unused_runner(),
        );

        assert!(
            task.units() > 1,
            "loading a manifest must dispatch both the interpreter fetch and the venv probe; \
             got {} unit(s)",
            task.units()
        );
    }

    #[test]
    fn a_landed_adoption_probe_reaches_the_open_draft() {
        let mut app = App::new(Config::default());
        app.add_project = Some(manifest_draft(""));
        // What `VenvChoice::SettingsDefault` resolves to for this manifest.
        let probed = PathBuf::from("/p/my-api/.venv");

        let _ = update(
            &mut app,
            Message::AddProjectVenvOutcomeProbed(
                probed,
                add_project::VenvOutcome::Adopt {
                    python_version: "3.9.18".to_string(),
                },
            ),
            unused_runner(),
        );

        assert_eq!(
            app.add_project
                .as_ref()
                .and_then(|d| d.venv_outcome.clone()),
            Some(add_project::VenvOutcome::Adopt {
                python_version: "3.9.18".to_string()
            })
        );
    }

    #[test]
    fn an_adoption_probe_for_a_folder_no_longer_chosen_is_dropped() {
        // The user changed their mind while the probe was in flight: an
        // older result landing last would describe a folder that is not the
        // one on screen.
        let mut app = App::new(Config::default());
        app.add_project = Some(manifest_draft(""));

        let _ = update(
            &mut app,
            Message::AddProjectVenvOutcomeProbed(
                PathBuf::from("/somewhere/else/.venv"),
                add_project::VenvOutcome::Adopt {
                    python_version: "3.9.18".to_string(),
                },
            ),
            unused_runner(),
        );

        assert_eq!(
            app.add_project
                .as_ref()
                .and_then(|d| d.venv_outcome.clone()),
            None,
            "a probe of a path the draft no longer resolves to must be dropped"
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
            Message::AddProjectInterpretersLoaded(Ok(vec![interpreter("3.12.4")])),
            unused_runner(),
        );

        assert!(app.add_project.is_none());
    }

    #[test]
    fn the_install_button_dispatches_without_blocking_when_a_version_can_be_suggested() {
        let mut app = App::new(Config::default());
        let mut draft =
            manifest_draft("[project]\nname = \"my-api\"\nrequires-python = \">=3.11\"\n");
        draft.interpreters = add_project::Interpreters::Loaded(vec![interpreter("3.9.18")]);
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
    fn quitting_saves_the_window_size() {
        let persist = Arc::new(FakeConfigPersist::default());
        let mut app = App::new(Config::default());
        app.persist = persist.clone();
        let _ = update(
            &mut app,
            Message::WindowResized(iced::Size::new(1100.4, 700.0)),
            unused_runner(),
        );
        assert!(persist.saved().is_empty(), "no write per resize");
        let _ = update(
            &mut app,
            Message::Tray(crate::tray::TrayCommand::Quit),
            unused_runner(),
        );
        let saved = persist.saved();
        assert_eq!(saved.len(), 1, "saved before the window goes");
        assert_eq!(saved[0].settings.layout.window, Some((1100, 700)));
        let _ = update(
            &mut app,
            Message::Tray(crate::tray::TrayCommand::Quit),
            unused_runner(),
        );
        assert_eq!(
            persist.saved().len(),
            1,
            "an unchanged size is not rewritten"
        );
    }

    #[test]
    fn a_remembered_size_never_makes_an_unusable_window() {
        use bombadil_core::model::Layout;
        let mut layout = Layout::default();
        assert_eq!(initial_size(&layout), None);
        layout.window = Some((10, 10));
        assert_eq!(initial_size(&layout), Some(iced::Size::new(640.0, 420.0)));
        layout.window = Some((1280, 800));
        assert_eq!(initial_size(&layout), Some(iced::Size::new(1280.0, 800.0)));
    }

    #[test]
    fn adding_a_project_resets_the_run_form_for_it() {
        // Selected without a click: the Run tab's form and entry points must
        // follow, or Save would write into the wrong project.
        let mut app = App::new(Config::default());
        app.tab = Tab::Run;
        app.command_draft = run_tab::CommandDraft {
            label: "x".into(),
            command: "y".into(),
            editing: Some(3),
        };
        app.add_project = Some(manifest_draft(
            "[project]\nname = \"my-api\"\nrequires-python = \">=3.11\"\n",
        ));
        let _ = update(&mut app, Message::AddProjectConfirmed, unused_runner());
        assert_eq!(app.command_draft.editing, None);
        assert!(app.command_draft.command.is_empty());
    }

    #[test]
    fn removing_the_selected_project_resets_the_run_form() {
        let mut app = two_projects();
        app.selected = Some(0);
        app.command_draft.editing = Some(0);
        let _ = update(
            &mut app,
            Message::RemoveProjectRequested(0),
            unused_runner(),
        );
        assert_eq!(
            app.command_draft.editing, None,
            "Save must not edit the next project"
        );
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
        assert_eq!(app.config.projects.len(), 1);
        assert_eq!(app.config.projects[0].label, "my-api");
        assert_eq!(app.entries.len(), 1);
        assert!(
            app.add_project.is_none(),
            "the dialog must close once confirmed"
        );
    }

    #[test]
    fn a_sync_pressed_right_after_confirming_the_dialog_dispatches_nothing() {
        // Confirming runs `uv sync -p <version>` and also selects the new
        // project, and the detail header always renders Sync -- so without
        // claiming the in-flight slot the user is one press away from two
        // concurrent `uv sync` on the same venv, interleaving into the one
        // shared drawer. That is why `App::busy` is a single slot.
        let mut app = App::new(Config::default());
        app.add_project = Some(manifest_draft(
            "[project]\nname = \"my-api\"\nrequires-python = \">=3.11\"\n",
        ));
        let fake = Arc::new(FakeCommandRunner::new());

        let confirm = update(&mut app, Message::AddProjectConfirmed, fake.clone());
        assert!(confirm.units() > 0, "confirming must dispatch work");

        let sync = update(&mut app, Message::SyncRequested(0), fake.clone());

        assert_eq!(
            sync.units(),
            0,
            "a Sync pressed while the confirm's own uv sync is in flight must dispatch nothing"
        );
        assert!(
            fake.calls().is_empty(),
            "no uv command may be issued by the ignored press; got {:?}",
            fake.calls()
        );
    }

    #[test]
    fn a_finished_confirm_releases_the_in_flight_slot() {
        // The other half: a guard that is never cleared is a deadlock, and
        // Sync would work exactly zero times after adding a project.
        let mut app = App::new(Config::default());
        app.add_project = Some(manifest_draft("[project]\nname = \"my-api\"\n"));

        let _ = update(&mut app, Message::AddProjectConfirmed, unused_runner());
        let _ = update(
            &mut app,
            Message::AddProjectConfirmFinished {
                success: true,
                lines: vec![],
                index: 0,
            },
            unused_runner(),
        );

        assert!(can_start(&app));
        let again = update(&mut app, Message::SyncRequested(0), unused_runner());
        assert!(
            again.units() > 0,
            "Sync must dispatch again once the confirm has finished"
        );
    }

    #[test]
    fn adding_a_project_waits_for_a_running_command() {
        // Its first `uv sync` is a uv command like any other. Starting it
        // over a running one swapped the busy slot, so the first command's
        // finish reported the new project's outcome.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let running = app.config.projects[0].id;
        app.busy = Some(crate::busy::Busy::new(running, crate::busy::Action::Sync));
        app.add_project = Some(manifest_draft(""));

        let task = update(&mut app, Message::AddProjectConfirmed, unused_runner());

        assert_eq!(task.units(), 0, "nothing may start while busy");
        assert_eq!(app.busy.as_ref().map(|b| b.project_id), Some(running));
        assert_eq!(app.config.projects.len(), 1);
        assert!(
            app.add_project.is_some(),
            "the dialog stays open so the user can confirm once it finishes"
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

    // --- The detail header's one-press terminal button ---

    #[test]
    fn the_header_terminal_button_dispatches_without_spawning_inline() {
        // That a configured default skips the probe entirely is
        // `context_menu::one_press_plan`'s claim, and its own tests are the
        // only place it can be proven -- both arms here return a `Task` and no
        // assertion available to this test can tell them apart. What this
        // covers is the part that is visible from here: the press dispatches,
        // and it does not spawn anything on the render thread.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            terminal_defaults: std::collections::BTreeMap::from([(
                Os::host(),
                TerminalChoice::Detected(TerminalKind::Xterm),
            )]),
            ..Config::default()
        });
        let fake = Arc::new(FakeCommandRunner::new());

        let task = update(
            &mut app,
            Message::OpenTerminalHereRequested(0),
            fake.clone(),
        );

        assert!(task.units() > 0, "the press must dispatch real work");
        assert!(
            fake.calls().is_empty(),
            "nothing may be spawned synchronously inside update; got {:?}",
            fake.calls()
        );
        assert!(
            !app.drawer.status.contains("failed"),
            "a configured default is not a failure; got {:?}",
            app.drawer.status
        );
    }

    #[test]
    fn the_header_terminal_button_reports_when_no_terminal_exists() {
        // The button has no menu to fall back to, so "nothing installed" has
        // to be said out loud. A press that silently did nothing would read as
        // a broken control, and this is something the user can act on.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let first = app.config.projects[0].id;
        let _ = update(
            &mut app,
            Message::OpenTerminalHereDetected(first, Vec::new(), None),
            unused_runner(),
        );

        assert!(
            app.drawer.lines.iter().any(|l| l.contains("no terminal")),
            "the drawer must say why nothing opened; got {:?}",
            app.drawer.lines
        );
        assert_eq!(app.drawer.status, "open terminal failed");
        assert!(app.drawer.expanded, "the reason must be on screen");
    }

    #[test]
    fn the_header_terminal_button_opens_the_first_detected_when_none_is_configured() {
        // The other half: something was found, so it opens rather than
        // complaining. Asserted on the drawer staying quiet, because a
        // dispatched `Task` cannot say which of the two arms produced it.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let first = app.config.projects[0].id;

        let task = update(
            &mut app,
            Message::OpenTerminalHereDetected(first, vec![TerminalKind::Kitty], None),
            unused_runner(),
        );

        assert!(task.units() > 0, "a detected terminal must be opened");
        assert!(
            app.drawer.lines.is_empty(),
            "nothing failed, so nothing belongs in the transcript; got {:?}",
            app.drawer.lines
        );
    }

    #[test]
    fn a_header_terminal_press_for_a_project_that_is_gone_does_nothing() {
        // Same staleness guard every indexed message here carries: the list
        // can shrink between the press and the probe landing.
        let mut app = App::new(Config::default());

        let task = update(
            &mut app,
            Message::OpenTerminalHereRequested(7),
            unused_runner(),
        );
        assert_eq!(task.units(), 0);

        let task = update(
            &mut app,
            Message::OpenTerminalHereDetected(
                uuid::Uuid::new_v4(),
                vec![TerminalKind::Kitty],
                None,
            ),
            unused_runner(),
        );
        assert_eq!(task.units(), 0);
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
        // The kind is derived from the host rather than hardcoded: a default
        // keyed to this OS but naming a terminal that cannot exist on it is
        // not a configuration the app could ever have written. This one never
        // reaches `argv::build`, so it passed everywhere either way -- but
        // leaving the pattern here is how it gets copied into a test that
        // does.
        let kind = host_terminal_kind();
        let config = Config {
            projects: vec![project("api")],
            terminal_defaults: std::collections::BTreeMap::from([(
                Os::host(),
                TerminalChoice::Detected(kind),
            )]),
            ..Config::default()
        };
        let app = App::new(config);
        assert_eq!(
            app.config.terminal_defaults.get(&Os::host()),
            Some(&TerminalChoice::Detected(kind)),
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
        assert_eq!(app.config.projects.len(), 1);
        assert_eq!(app.config.projects[0].label, "web");
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

        assert!(app.config.projects.is_empty());
        assert!(app.selected.is_none());
    }

    #[test]
    fn removing_a_project_never_touches_the_venv_on_disk() {
        // What this proves: the removal path runs and spawns nothing at all,
        // so it cannot reach uv or any other process.
        //
        // What it cannot prove any more: `venv_path::delete` now exists and
        // needs no `CommandRunner`, so a regression that called it from this
        // arm would leave `fake.calls()` empty and still destroy the user's
        // environment. Closing that in a GUI test would mean giving this test
        // a real venv on disk to watch, which the no-filesystem constraint
        // forbids -- the guard is the arm's own body, which touches no path.
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
            app.config.projects.len(),
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
            selected_label(&app),
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
        // One rule per host's first kind: `expect` matches on the program's
        // file stem, so exactly one of these can ever fire -- `gnome-terminal`
        // on Linux, `open -a Terminal` on macOS, `wt.exe` on Windows. Naming
        // all three keeps this test running the real path on every platform
        // instead of only the one CI happens to be generous with.
        for program in ["gnome-terminal", "open", "wt"] {
            runner.expect(
                program,
                &[],
                bombadil_core::process::CommandOutcome {
                    status: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                },
            );
        }

        // A scratch directory of this test's own: `terminal::open::open`
        // WRITES the shell re-assertion init file into whatever it is given,
        // and this test used to hand it `std::env::temp_dir()` by way of
        // `run_open_terminal` -- leaving files on the machine running the
        // suite, which is exactly what the no-filesystem constraint forbids.
        let scratch = tempfile::tempdir().unwrap();

        let result = run_open_terminal(
            &TerminalChoice::Detected(host_terminal_kind()),
            &project,
            &config,
            &store,
            &runner,
            scratch.path(),
        );

        assert!(result.is_ok(), "got {result:?}");
        let calls = runner.calls();
        assert_eq!(calls.len(), 1, "the terminal must actually be spawned");
        // Whether an init file is written at all depends on `$SHELL`, which
        // is not this test's to control -- but if one was, every reference to
        // it must live under `scratch`. "bombadil-terminal-init" is the real
        // temp directory's name and must appear nowhere.
        // `terminal::open::open` namespaces the init file by project id, so
        // the id is what identifies an init path among argv and a composed
        // environment that legitimately mentions plenty of other paths.
        let marker = project.id.to_string();
        let referenced: Vec<&String> = calls[0]
            .args
            .iter()
            .chain(calls[0].env.values())
            .filter(|v| v.contains(&marker))
            .collect();
        let scratch_prefix = scratch.path().to_string_lossy().into_owned();
        for value in referenced {
            assert!(
                value.starts_with(&scratch_prefix),
                "an init file was written outside the scratch dir {scratch_prefix}: {value}"
            );
        }
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

        let scratch = tempfile::tempdir().unwrap();

        let result = run_open_terminal(
            &TerminalChoice::Detected(host_terminal_kind()),
            &project("api"),
            &config,
            &store,
            &runner,
            scratch.path(),
        );

        let err =
            result.expect_err("a terminal that will not spawn must not be reported as opened");
        // With a hardcoded Linux kind this assertion passed on macOS and
        // Windows for the wrong reason: `argv::build` refused the kind before
        // anything was ever spawned, so spec §9's "the terminal has
        // disappeared" case went unexercised on exactly the two platforms
        // where it is least verified.
        assert!(
            !err.contains("not a terminal available"),
            "the failure must come from the spawn, not the wrong-OS guard; got {err}"
        );
    }

    // --- 7b: Recreate venv ---
    //
    // No test here touches the filesystem: `run_recreate_venv` takes the
    // delete as a parameter, the way `run_confirm_add_project` takes its
    // probe, so the destructive half is exercised as a recorded call. The
    // real deletion is `bombadil_core::venv_path::delete`, tested against
    // `tempfile` in the crate that owns it.

    /// Records every path it was asked to delete and answers with `outcome`.
    fn recording_delete(
        log: &std::sync::Mutex<Vec<PathBuf>>,
        outcome: Result<(), String>,
    ) -> impl Fn(&Path) -> Result<(), String> + '_ {
        move |path: &Path| {
            log.lock().unwrap().push(path.to_path_buf());
            outcome.clone()
        }
    }

    #[test]
    fn requesting_a_recreate_only_asks_for_confirmation_and_deletes_nothing_yet() {
        // The whole point of the confirmation. A press that went straight to
        // work would delete an environment the user has not agreed to lose,
        // and there is no undo.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let fake = Arc::new(FakeCommandRunner::new());
        let _ = update(&mut app, Message::ContextMenuOpened(0), fake.clone());

        let task = update(&mut app, Message::RecreateVenvRequested(0), fake.clone());

        assert_eq!(
            task.units(),
            0,
            "requesting a recreate must dispatch NOTHING until it is confirmed"
        );
        assert!(
            fake.calls().is_empty(),
            "nothing may run before the user confirms; got {:?}",
            fake.calls()
        );
        assert!(
            app.confirm_recreate.is_some(),
            "the confirmation must be on screen"
        );
        assert!(
            app.context_menu.is_none(),
            "the menu should close once an action is taken from it"
        );
    }

    #[test]
    fn the_confirmation_names_the_right_clicked_projects_own_venv_path() {
        // Two projects with the FIRST selected and the SECOND right-clicked:
        // a confirmation resolved against the selection would name -- and
        // then delete -- an environment the user never pointed at.
        let mut app = App::new(Config {
            projects: vec![project("api"), project("web")],
            ..Config::default()
        });
        assert_eq!(app.selected, Some(0));

        let _ = update(&mut app, Message::RecreateVenvRequested(1), unused_runner());

        let confirm = app.confirm_recreate.as_ref().expect("must be pending");
        assert_eq!(confirm.project_id, app.config.projects[1].id);
        assert_eq!(confirm.label, "web");
        assert_eq!(
            confirm.venv_path,
            PathBuf::from("/p/web/.venv"),
            "the confirmation must state the right-clicked project's resolved venv path"
        );
    }

    #[test]
    fn a_pending_recreate_never_deletes_a_project_the_user_did_not_name() {
        // Nothing here is modal, so the whole sequence is reachable with the
        // mouse: right-click B, pick Recreate venv, then right-click A and
        // pick Remove from list, then press Confirm. `RemoveProjectRequested`
        // clears `deps`, `members` and `confirm_member_sync` -- with a comment
        // naming exactly this hazard -- and used to leave `confirm_recreate`
        // holding an index that every later project had just shifted out of.
        // C's environment was what got deleted. `venv_path::delete`'s
        // refusals all still held; it was handed the wrong project.
        let mut app = App::new(Config {
            projects: vec![project("a"), project("b"), project("c")],
            ..Config::default()
        });
        let c_id = app.config.projects[2].id.to_string();

        let _ = update(&mut app, Message::RecreateVenvRequested(1), unused_runner());
        assert!(
            app.confirm_recreate.is_some(),
            "the confirmation for b must be pending"
        );

        let _ = update(
            &mut app,
            Message::RemoveProjectRequested(0),
            unused_runner(),
        );

        let task = update(&mut app, Message::RecreateVenvConfirmed, unused_runner());

        assert_ne!(
            busy_project(&app).as_deref(),
            Some(c_id.as_str()),
            "confirming a recreate for b must never delete c's environment"
        );
        assert_eq!(
            task.units(),
            0,
            "a confirmation whose list shifted underneath it must dispatch nothing"
        );
        assert!(can_start(&app));
    }

    #[test]
    fn a_recreate_confirmation_resolves_its_project_by_id_not_by_position() {
        // Defence in depth behind the clear-on-removal proven above, and the
        // half that makes the class unrepresentable rather than merely
        // unreached: with `RemoveProjectRequested` clearing the confirmation,
        // no state reachable today distinguishes an id from an index, so
        // without this the id would go untested and the next edit that moves
        // a project by some other route would put the defect straight back.
        let mut app = App::new(Config {
            projects: vec![project("a"), project("b")],
            ..Config::default()
        });
        let b_id = app.config.projects[1].id.to_string();

        let _ = update(&mut app, Message::RecreateVenvRequested(1), unused_runner());
        // The list moves under the open confirmation without going through
        // the removal path that clears it.
        app.config.projects.swap(0, 1);

        let _ = update(&mut app, Message::RecreateVenvConfirmed, unused_runner());

        assert_eq!(
            busy_project(&app).as_deref(),
            Some(b_id.as_str()),
            "the confirmation must act on the project it named, wherever that \
             project now sits"
        );
        assert!(
            app.drawer.status.contains('b'),
            "the running status must name it too; got {:?}",
            app.drawer.status
        );
    }

    #[test]
    fn cancelling_a_recreate_dispatches_nothing_and_clears_the_confirmation() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let fake = Arc::new(FakeCommandRunner::new());
        let _ = update(&mut app, Message::RecreateVenvRequested(0), fake.clone());

        let task = update(&mut app, Message::RecreateVenvCancelled, fake.clone());

        assert_eq!(task.units(), 0);
        assert!(fake.calls().is_empty(), "got {:?}", fake.calls());
        assert!(app.confirm_recreate.is_none());
    }

    #[test]
    fn confirming_a_recreate_dispatches_background_work_without_running_anything_inline() {
        // Deleting a directory tree and then running `uv sync` both block;
        // doing either inside `update` freezes the window.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let fake = Arc::new(FakeCommandRunner::new());
        let _ = update(&mut app, Message::RecreateVenvRequested(0), fake.clone());

        let task = update(&mut app, Message::RecreateVenvConfirmed, fake.clone());

        assert!(
            fake.calls().is_empty(),
            "runner.run must not be called synchronously inside update; got {:?}",
            fake.calls()
        );
        assert!(
            task.units() > 0,
            "a confirmed recreate must dispatch real background work"
        );
        assert!(
            app.confirm_recreate.is_none(),
            "the confirmation must close once it has been acted on"
        );
        assert!(
            !can_start(&app),
            "a recreate runs uv sync, so it must claim the single in-flight slot"
        );
    }

    #[test]
    fn a_recreate_deletes_the_projects_venv_and_then_rebuilds_it() {
        let project = project("api");
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
        let deleted = std::sync::Mutex::new(Vec::new());

        let (success, _lines) = collect_lines(|on_line| {
            run_recreate_venv(
                &project,
                &config,
                &store,
                &runner,
                on_line,
                recording_delete(&deleted, Ok(())),
            )
        });

        assert!(success);
        assert_eq!(
            deleted.lock().unwrap().as_slice(),
            [PathBuf::from("/p/api/.venv")],
            "exactly the project's own resolved venv path must be deleted, once"
        );
        assert!(
            runner
                .calls()
                .iter()
                .any(|c| c.args.iter().any(|a| a == "sync")),
            "the environment must be rebuilt after being deleted; got {:?}",
            runner.calls()
        );
    }

    #[test]
    fn a_recreate_whose_delete_is_refused_never_runs_uv_sync() {
        // Also the ordering proof: if the rebuild ran before the delete, a
        // refused delete would still leave a `uv sync` call recorded here.
        // And a refusal is not "just sync instead" -- that would silently do
        // something other than what the user confirmed.
        let project = project("api");
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
        let deleted = std::sync::Mutex::new(Vec::new());

        let (success, lines) = collect_lines(|on_line| {
            run_recreate_venv(
                &project,
                &config,
                &store,
                &runner,
                on_line,
                recording_delete(
                    &deleted,
                    Err("/p/api/.venv holds no pyvenv.cfg".to_string()),
                ),
            )
        });

        assert!(!success);
        assert!(
            runner.calls().is_empty(),
            "a refused delete must stop the recreate dead; got {:?}",
            runner.calls()
        );
        assert!(
            lines.iter().any(|l| l.contains("pyvenv.cfg")),
            "core's own refusal text must reach the user; got {lines:?}"
        );
    }

    #[test]
    fn a_recreate_whose_rebuild_fails_says_the_environment_is_gone() {
        // The worst outcome of this action, and the one the confirmation
        // warned about: the delete worked and the sync did not. "sync failed"
        // alone leaves the user unaware that their environment no longer
        // exists.
        let project = project("api");
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
                status: Some(1),
                stdout: String::new(),
                stderr: "error: no solution found".into(),
            },
        );
        let deleted = std::sync::Mutex::new(Vec::new());

        let (success, lines) = collect_lines(|on_line| {
            run_recreate_venv(
                &project,
                &config,
                &store,
                &runner,
                on_line,
                recording_delete(&deleted, Ok(())),
            )
        });

        assert!(!success);
        let joined = lines.join("\n");
        assert!(
            joined.contains("no solution found"),
            "uv's own reason must survive; got {joined}"
        );
        // Derived through the very function that built the path in the
        // message, not written as a `/`-separated literal: `venv_path::resolve`
        // assembles it with `PathBuf::join`, which separates with `\` on
        // Windows, so a literal compared against the *displayed* path passes
        // on Unix and fails on Windows for a message that was correct all
        // along. Deriving it keeps the whole path asserted -- the message has
        // to name which environment is gone, and a central-store venv is a
        // hashed directory the user has never seen.
        let expected = venv_path::resolve(
            &project,
            project
                .active_environment()
                .expect("this project has an environment"),
            &config.settings,
        )
        .expect("this project's venv path must resolve")
        .display()
        .to_string();
        assert!(
            joined.contains(&expected) && joined.contains("deleted"),
            "the user must be told the environment was deleted and not rebuilt; \
             expected {expected} to appear in {joined}"
        );
    }

    #[test]
    fn a_recreate_confirmed_while_a_sync_is_in_flight_dispatches_nothing() {
        // Same single-slot rule `SyncRequested` follows: one mutating uv
        // command at a time, one shared drawer.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let fake = Arc::new(FakeCommandRunner::new());
        let _ = update(&mut app, Message::SyncRequested(0), fake.clone());
        let _ = update(&mut app, Message::RecreateVenvRequested(0), fake.clone());

        let task = update(&mut app, Message::RecreateVenvConfirmed, fake.clone());

        assert_eq!(
            task.units(),
            0,
            "a recreate must not start while a sync is already running"
        );
        assert!(fake.calls().is_empty(), "got {:?}", fake.calls());
    }

    #[test]
    fn requesting_a_recreate_for_an_out_of_range_project_asks_nothing() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let task = update(
            &mut app,
            Message::RecreateVenvRequested(99),
            unused_runner(),
        );

        assert_eq!(task.units(), 0);
        assert!(app.confirm_recreate.is_none());
    }

    // --- 7c: Open folder ---

    #[test]
    fn opening_a_folder_never_calls_the_runner_synchronously() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let fake = Arc::new(FakeCommandRunner::new());
        let _ = update(&mut app, Message::ContextMenuOpened(0), fake.clone());

        let task = update(&mut app, Message::OpenFolderRequested(0), fake.clone());

        assert!(
            fake.calls().is_empty(),
            "runner.spawn must not be called synchronously inside update; got {:?}",
            fake.calls()
        );
        assert!(
            task.units() > 0,
            "opening a folder must dispatch real background work, not Task::none()"
        );
        assert!(
            app.context_menu.is_none(),
            "the menu should close once an action is taken from it"
        );
    }

    #[test]
    fn opening_a_folder_shows_the_project_directory_not_the_venv() {
        // Spec §8's Open folder is the folder the user recognises -- where
        // `pyproject.toml` lives, the same directory Open terminal opens in.
        // A central-store venv is a hashed directory somewhere else entirely.
        let runner = FakeCommandRunner::new();
        runner.expect(
            "xdg-open",
            &[],
            bombadil_core::process::CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let result = run_open_folder(Os::Linux, &project("api"), &runner);

        assert!(result.is_ok(), "got {result:?}");
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args, vec!["/p/api"]);
        assert_eq!(
            runner.blocking_calls(),
            0,
            "the file manager must be spawned, not waited on"
        );
    }

    #[test]
    fn opening_a_folder_uses_each_platforms_own_file_manager() {
        // `Os` is a parameter, never `cfg!`, exactly so all three are covered
        // wherever CI runs. A test that paired one hardcoded program with
        // `Os::host()` would silently cover one platform out of three.
        for (os, program) in [
            (Os::Linux, "xdg-open"),
            (Os::MacOs, "open"),
            (Os::Windows, "explorer"),
        ] {
            let runner = FakeCommandRunner::new();
            runner.expect(
                program,
                &[],
                bombadil_core::process::CommandOutcome {
                    status: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                },
            );

            run_open_folder(os, &project("api"), &runner)
                .unwrap_or_else(|e| panic!("{os:?} must open with {program}: {e}"));

            assert_eq!(
                runner.calls()[0].program.to_string_lossy(),
                program,
                "{os:?}"
            );
        }
    }

    #[test]
    fn a_project_whose_manifest_has_no_directory_reports_rather_than_opening_nothing() {
        let mut project = project("api");
        project.pyproject_path = PathBuf::from("pyproject.toml");
        let runner = FakeCommandRunner::new();

        let err = run_open_folder(Os::Linux, &project, &runner)
            .expect_err("there is no directory to open");

        assert!(
            runner.calls().is_empty(),
            "nothing may be spawned; got {:?}",
            runner.calls()
        );
        assert!(err.contains("pyproject.toml"), "got {err}");
    }

    #[test]
    fn setting_a_global_pin_writes_it_through_and_persists() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let persist = Arc::new(FakeConfigPersist::default());
        app.persist = persist.clone();

        let _ = update(
            &mut app,
            Message::PreferencesSectionOpened(preferences::Section::default()),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::PreferencesPythonPinChanged(preferences::Scope::Global, "3.12".into()),
            unused_runner(),
        );

        assert_eq!(
            app.config.settings.python,
            PythonPin::Version("3.12".into())
        );
        // Same shape every other settings test uses: the dispatched persist
        // is not driven by a unit test, so the config is saved directly to
        // prove it survives a restart.
        assert!(block_on(persist_config(
            app.config.clone(),
            persist.clone()
        )));
        assert_eq!(
            persist.saved()[0].settings.python,
            PythonPin::Version("3.12".into())
        );
    }

    #[test]
    fn an_emptied_pin_box_unpins_rather_than_pinning_to_nothing() {
        // Typing a version and clearing it must leave `Unpinned`. A
        // `Version("")` would reach uv as `-p ""` and be refused with an
        // error about an interpreter nobody asked for.
        let mut app = App::new(Config::default());

        let _ = update(
            &mut app,
            Message::PreferencesSectionOpened(preferences::Section::default()),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::PreferencesPythonPinChanged(preferences::Scope::Global, "3.12".into()),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::PreferencesPythonPinChanged(preferences::Scope::Global, "   ".into()),
            unused_runner(),
        );

        assert_eq!(app.config.settings.python, PythonPin::Unpinned);
    }

    #[test]
    fn a_project_pin_overrides_the_global_one_without_touching_it() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            settings: Settings {
                python: PythonPin::Version("3.12".into()),
                ..Settings::default()
            },
            ..Config::default()
        });
        let id = app.config.projects[0].id;

        let _ = update(
            &mut app,
            Message::PreferencesSectionOpened(preferences::Section::default()),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::PreferencesPythonPinChanged(preferences::Scope::Project(id), "3.11".into()),
            unused_runner(),
        );

        assert_eq!(
            app.config.projects[0]
                .active_environment()
                .map(|e| e.python.clone()),
            Some(PythonPin::Version("3.11".into()))
        );
        assert_eq!(
            app.config.settings.python,
            PythonPin::Version("3.12".into()),
            "a project override must not write into the global value"
        );
    }

    #[test]
    fn an_edit_in_a_scope_whose_project_is_gone_writes_nothing() {
        // Preferences can be open while the project it is showing is removed.
        // The edit must land nowhere rather than in whichever project took its
        // place -- the class of bug `ConfirmRecreate` carries a `Uuid` for --
        // and, just as importantly, not in the global settings.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let gone = uuid::Uuid::from_u128(99);

        let _ = update(
            &mut app,
            Message::PreferencesSectionOpened(preferences::Section::default()),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::PreferencesPythonPinChanged(preferences::Scope::Project(gone), "3.11".into()),
            unused_runner(),
        );

        assert_eq!(
            app.config.projects[0]
                .active_environment()
                .map(|e| e.python.clone()),
            Some(PythonPin::Unpinned)
        );
        assert_eq!(app.config.settings.python, PythonPin::Unpinned);
    }

    #[test]
    fn a_project_venv_location_writes_through_and_reprobes() {
        // Moving where an environment lives changes what the sidebar's venv
        // status describes, so the probe has to run again -- persisting alone
        // would leave the glyph describing the old location until restart.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let id = app.config.projects[0].id;

        let from = app.config.projects[0].active.clone();
        let task = update(
            &mut app,
            Message::EnvironmentLocationSelected(
                id,
                from,
                VenvLocation::Custom {
                    path: std::path::PathBuf::from("/envs/api"),
                },
            ),
            unused_runner(),
        );

        let moved = VenvLocation::Custom {
            path: std::path::PathBuf::from("/envs/api"),
        };
        assert_eq!(
            app.config.projects[0].environments[0].location, moved,
            "the environment itself must move"
        );
        assert_eq!(
            app.config.projects[0].active, moved,
            "and `active` with it, or it names a location that is gone"
        );
        assert!(
            task.units() > 0,
            "the change must dispatch a persist and a re-probe"
        );
    }

    #[test]
    fn a_cancelled_venv_folder_pick_leaves_the_location_alone() {
        // The picker returning `None` is a user who changed their mind. It
        // must not be read as "no folder", which would move the environment
        // to an empty path.
        let mut app = App::new(Config {
            projects: vec![Project {
                environments: vec![bombadil_core::model::Environment {
                    location: VenvLocation::Alongside,
                    python: PythonPin::Unpinned,
                }],
                active: VenvLocation::Alongside,
                ..project("api")
            }],
            ..Config::default()
        });
        let id = app.config.projects[0].id;

        let task = update(
            &mut app,
            Message::EnvironmentFolderPicked(id, VenvLocation::Alongside, None),
            unused_runner(),
        );

        assert_eq!(app.config.projects[0].active, VenvLocation::Alongside);
        assert_eq!(task.units(), 0, "cancelling must change nothing");
    }

    #[test]
    fn a_venv_edit_for_a_project_that_is_gone_writes_nothing() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        // Captured rather than hardcoded: the `project` helper picks a
        // location of its own, and asserting a literal here tests the helper
        // instead of the behaviour.
        let before = app.config.projects[0].environments[0].location.clone();
        let task = update(
            &mut app,
            Message::EnvironmentLocationSelected(
                uuid::Uuid::from_u128(99),
                VenvLocation::Alongside,
                VenvLocation::Custom {
                    path: std::path::PathBuf::from("/elsewhere"),
                },
            ),
            unused_runner(),
        );

        assert_eq!(app.config.projects[0].environments[0].location, before);
        assert_eq!(task.units(), 0);
    }

    #[test]
    fn a_project_uv_override_leaves_the_global_choice_alone() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            settings: Settings {
                uv_source: UvSource::Bundled,
                ..Settings::default()
            },
            ..Config::default()
        });
        let id = app.config.projects[0].id;

        let _ = update(
            &mut app,
            Message::PreferencesSectionOpened(preferences::Section::default()),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::PreferencesUvSourceOverridden(
                preferences::Scope::Project(id),
                UvSource::FromPath,
            ),
            unused_runner(),
        );

        assert_eq!(
            app.config.projects[0].uv_source,
            Override::Set(UvSource::FromPath)
        );
        assert_eq!(app.config.settings.uv_source, UvSource::Bundled);
    }

    #[test]
    fn a_project_terminal_override_is_per_os_and_leaves_the_others_alone() {
        // `Project::terminals` is a map for exactly this reason: an override
        // set on this machine must not decide what happens on another.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let id = app.config.projects[0].id;
        let other = match Os::host() {
            Os::Linux => Os::Windows,
            _ => Os::Linux,
        };

        let _ = update(
            &mut app,
            Message::PreferencesSectionOpened(preferences::Section::default()),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::PreferencesTerminalOverridden(
                preferences::Scope::Project(id),
                TerminalChoice::Detected(TerminalKind::Xterm),
            ),
            unused_runner(),
        );

        let terminals = &app.config.projects[0].terminals;
        assert_eq!(
            terminals.get(&Os::host()),
            Some(&TerminalChoice::Detected(TerminalKind::Xterm))
        );
        assert_eq!(
            terminals.get(&other),
            None,
            "an override must speak only for the OS it was set on"
        );
    }

    #[test]
    fn a_pin_set_here_reaches_the_confirmation_when_sync_would_delete() {
        // The end-to-end proof that Plan A's guard is wired to something a
        // user can actually reach. Until this task nothing could set a pin,
        // so `sync_would_delete` had never fired from a real press.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let id = app.config.projects[0].id;
        set_probed_version(&mut app, 0, "3.13.14");

        let _ = update(
            &mut app,
            Message::PreferencesSectionOpened(preferences::Section::default()),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::PreferencesPythonPinChanged(preferences::Scope::Project(id), "3.12".into()),
            unused_runner(),
        );
        let task = update(&mut app, Message::SyncRequested(0), unused_runner());

        assert_eq!(task.units(), 0, "nothing may run before the user answers");
        let confirm = app
            .confirm_pin_change
            .as_ref()
            .expect("a pin set in Preferences must reach the same guard");
        assert_eq!(confirm.from, "3.13.14");
        assert_eq!(confirm.to, "3.12");
    }

    #[test]
    fn preferences_opens_on_global_and_closes_the_menu() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::MenuToggled(shell::TopMenu::Help),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::PreferencesSectionOpened(preferences::Section::default()),
            unused_runner(),
        );

        let state = app.preferences.expect("Preferences must open");
        assert_eq!(state.scope, preferences::Scope::Global);
        assert!(
            app.open_menu.is_none(),
            "an open Help menu must close behind it -- two layers claiming the \
             pointer at once is how a press both dismisses and activates"
        );
    }

    #[test]
    fn switching_scope_keeps_the_section_you_were_reading() {
        // Scope and section are independent axes. Resetting the section on a
        // scope change would throw the user back to the top of the list every
        // time they compared a project against the global value -- which is
        // the main thing this screen is for.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let id = app.config.projects[0].id;

        let _ = update(
            &mut app,
            Message::PreferencesSectionOpened(preferences::Section::default()),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::PreferencesSectionSelected(preferences::Section::Python),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::PreferencesScopeSelected(preferences::Scope::Project(id)),
            unused_runner(),
        );

        let state = app.preferences.expect("still open");
        assert_eq!(state.scope, preferences::Scope::Project(id));
        assert_eq!(state.section, preferences::Section::Python);
    }

    #[test]
    fn closing_preferences_keeps_every_edit_made_in_it() {
        // There is no OK/Cancel here: every control writes straight through to
        // `Config` and persists, the same as the tabs it replaces. Closing is
        // not an undo, and must never read as one.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::PreferencesSectionOpened(preferences::Section::default()),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::SettingsUvSourceChoiceChanged(settings_editor::UvSourceChoice::Bundled),
            unused_runner(),
        );
        let _ = update(&mut app, Message::PreferencesClosed, unused_runner());

        assert!(app.preferences.is_none());
        assert_eq!(app.config.settings.uv_source, UvSource::Bundled);
    }

    #[test]
    fn the_bundled_licence_is_reachable_through_the_help_menu() {
        // Apache-2.0 section 4 and OFL 1.1 oblige the text to be reachable.
        // It is compiled into the executable and had no consumer at all until
        // the footer existed; moving that footer into a modal keeps the
        // obligation met only if this route actually opens. `shell`'s own
        // test holds the menu to still offering the entry; this holds the
        // press to still opening it.
        let mut app = App::new(Config::default());
        assert_eq!(app.modal, None, "no modal is open on start");

        let _ = update(
            &mut app,
            Message::MenuToggled(shell::TopMenu::Help),
            unused_runner(),
        );
        assert_eq!(
            app.open_menu,
            Some(shell::TopMenu::Help),
            "the Help menu must open"
        );

        let _ = update(
            &mut app,
            Message::ModalOpened(shell::Modal::Licences),
            unused_runner(),
        );
        assert_eq!(app.modal, Some(shell::Modal::Licences));
        assert!(
            app.open_menu.is_none(),
            "the menu must close behind the modal it opened; two layers \
             claiming the pointer at once is how a press both dismisses and \
             activates"
        );

        let _ = update(&mut app, Message::ModalClosed, unused_runner());
        assert_eq!(app.modal, None);
    }

    #[test]
    fn the_help_menu_closes_without_choosing_anything() {
        // A menu with no way out but choosing is a trap: the press that opened
        // it must close it, and so must a press anywhere else.
        let mut app = App::new(Config::default());

        let _ = update(
            &mut app,
            Message::MenuToggled(shell::TopMenu::Help),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::MenuToggled(shell::TopMenu::Help),
            unused_runner(),
        );
        assert!(
            app.open_menu.is_none(),
            "the Help button must toggle, not only open"
        );
        assert_eq!(app.modal, None, "dismissing must not open anything");

        let _ = update(
            &mut app,
            Message::MenuToggled(shell::TopMenu::Help),
            unused_runner(),
        );
        let _ = update(&mut app, Message::MenuDismissed, unused_runner());
        assert!(app.open_menu.is_none());
        assert_eq!(app.modal, None);
    }

    // --- Task 2: the Env vars tab ---
    //
    // Criterion 1 (a project variable overriding a global one is shown as
    // overriding, with both values visible) is covered by
    // `env_editor::overrides`'s own tests in `env_editor.rs`: it is a pure
    // function, and testing it directly there is a stronger proof than
    // re-deriving the same claim by driving `update`'s opaque `Task`.

    #[test]
    fn marking_a_variable_secret_writes_the_store_and_leaves_config_empty() {
        // Criterion 2, both halves the brief asks for in one test: the value
        // ends up in the store, and nothing resembling it is left in the
        // config that gets written to config.toml.
        let key = SecretKey::GlobalEnv {
            key: "TOKEN".into(),
        };
        let mut config = Config {
            global_env: vec![bombadil_core::model::EnvVar {
                key: "TOKEN".into(),
                value: "hunter2".into(),
                secret: false,
            }],
            ..Config::default()
        };

        // The in-memory half `update` runs synchronously before dispatching
        // the background job.
        let plain_value = env_editor::mark_secret(&mut config.global_env, 0)
            .expect("a non-secret row must yield its plaintext");
        assert_eq!(
            config.global_env[0].value, "",
            "got {:?}",
            config.global_env[0].value
        );
        assert!(config.global_env[0].secret);

        // The background half: never touches a real keychain, per the hard
        // security constraint -- `InMemorySecretStore` only.
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        let persist = FakeConfigPersist::default();
        let success = run_secret_write(&key, &plain_value, &config, &store, &persist);

        assert!(success);
        assert_eq!(
            store.get(&key).unwrap().as_deref(),
            Some("hunter2"),
            "the store must hold the value the user typed"
        );
        let saved = persist.saved();
        assert_eq!(
            saved.len(),
            1,
            "the config must be persisted alongside the write"
        );
        assert_eq!(
            saved[0].global_env[0].value, "",
            "the persisted config must never carry the secret's value"
        );
    }

    #[test]
    fn unmarking_a_variable_secret_deletes_the_keychain_entry() {
        // Criterion 3, for a project-scoped secret -- the account shape
        // (`SecretKey::ProjectEnv`) that `GlobalEnv` alone would not catch a
        // mix-up in.
        let project_id = uuid::Uuid::from_u128(9);
        let key = SecretKey::ProjectEnv {
            project_id,
            key: "TOKEN".into(),
        };
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        store.set(&key, "hunter2").unwrap();

        let mut proj = project("api");
        proj.id = project_id;
        proj.env = vec![bombadil_core::model::EnvVar {
            key: "TOKEN".into(),
            value: String::new(),
            secret: true,
        }];
        let mut config = Config {
            projects: vec![proj],
            ..Config::default()
        };

        let owed = env_editor::unmark_secret(&mut config.projects[0].env, 0);
        assert!(owed, "a secret row must report a delete as owed");
        assert!(!config.projects[0].env[0].secret);

        let persist = FakeConfigPersist::default();
        let success = run_secret_delete(&key, &config, &store, &persist);

        assert!(success);
        assert_eq!(
            store.get(&key).unwrap(),
            None,
            "the keychain entry must be gone, not orphaned"
        );
        assert!(
            !persist.saved()[0].projects[0].env[0].secret,
            "the persisted config must record the row as no longer secret"
        );
    }

    #[test]
    fn the_message_that_triggers_marking_secret_never_carries_the_value() {
        // Criterion 4, and the design question the brief asks to be
        // answered rather than guessed at: `EnvVarSecretToggled` is what a
        // real checkbox press sends into `update` to start the whole
        // mark-secret flow, and it carries only `scope` and `index` -- there
        // is no field for a value to hide in, so its derived `Debug` cannot
        // print one no matter what the row's plaintext was.
        let mut app = App::new(Config {
            global_env: vec![bombadil_core::model::EnvVar {
                key: "TOKEN".into(),
                value: "hunter2".into(),
                secret: false,
            }],
            ..Config::default()
        });

        let toggle = Message::EnvVarSecretToggled(env_editor::EnvScope::Global, 0);
        assert!(
            !format!("{toggle:?}").contains("hunter2"),
            "the message that triggers the write must not carry the value"
        );

        let _ = update(&mut app, toggle, unused_runner());

        // The message that reports the write back carries no value either --
        // it names the row and says whether the keychain took it.
        let written = Message::EnvVarSecretWritten {
            scope: env_editor::EnvScope::Global,
            index: 0,
            success: true,
        };
        assert!(!format!("{written:?}").contains("hunter2"));

        // Only once that arrives does the value leave `Config` -- so there is
        // nothing left in `app.config` for any later `Debug` render to print.
        let _ = update(&mut app, written, unused_runner());
        assert_eq!(app.config.global_env[0].value, "");
        assert!(app.config.global_env[0].secret);
    }

    #[test]
    fn adding_a_global_env_var_persists_through_config_persist() {
        // Criterion 5.
        let mut app = App::new(Config::default());
        let persist = Arc::new(FakeConfigPersist::default());
        app.persist = persist.clone();

        let task = update(
            &mut app,
            Message::EnvVarAdded(env_editor::EnvScope::Global),
            unused_runner(),
        );

        assert_eq!(
            app.config.global_env.len(),
            1,
            "in-memory must reflect the add the instant update returns"
        );
        assert!(
            task.units() > 0,
            "the add must dispatch a background persist"
        );
        assert!(
            persist.saved().is_empty(),
            "persist.save must not be called synchronously inside update"
        );

        // `Task` is opaque and never driven in a unit test in this file;
        // every other background call is proven by running its async half
        // directly instead, and this follows the same convention.
        let success = block_on(persist_config(app.config.clone(), persist.clone()));

        assert!(success);
        assert_eq!(
            persist.saved().last().unwrap().global_env.len(),
            1,
            "the persisted config must carry the edit, or it vanishes on restart"
        );
    }

    fn plain_var(key: &str, value: &str) -> bombadil_core::model::EnvVar {
        bombadil_core::model::EnvVar {
            key: key.into(),
            value: value.into(),
            secret: false,
        }
    }

    #[test]
    fn typing_a_value_reaches_neither_config_nor_config_persist() {
        // The defect this exists to catch: `EnvVarValueChanged` used to write
        // `Config` *and* dispatch a persist, so a PyPI token typed into that
        // box was written to `config.toml` once per character -- and the
        // store's atomic rename released every intermediate copy to free
        // blocks rather than overwriting it. The typing itself must reach
        // neither.
        let mut app = App::new(Config {
            global_env: vec![plain_var("TOKEN", "")],
            ..Config::default()
        });
        let persist = Arc::new(FakeConfigPersist::default());
        app.persist = persist.clone();

        // One message per character, exactly as iced sends them.
        let mut task_units = 0;
        for prefix in ["h", "hu", "hun", "hunt", "hunte", "hunter", "hunter2"] {
            let task = update(
                &mut app,
                Message::EnvVarValueChanged(env_editor::EnvScope::Global, 0, prefix.into()),
                unused_runner(),
            );
            task_units += task.units();
        }

        assert_eq!(
            task_units, 0,
            "typing must dispatch no work at all -- a dispatched persist is a write to config.toml"
        );
        assert_eq!(
            app.config.global_env[0].value, "",
            "the typed text must not be in Config, which is what any persist would write out"
        );
        assert!(
            !format!("{:?}", app.config).contains("hunter2"),
            "nothing a Debug render of Config could print may carry the typed text"
        );
        assert!(
            persist.saved().is_empty(),
            "no save may have happened, synchronously or otherwise"
        );
    }

    #[test]
    fn committing_a_typed_value_puts_it_in_config_and_persists_it() {
        // The other half: not persisting per keystroke must not mean the
        // value is quietly lost. Enter, or the row's save button, is what
        // commits it -- and then it does reach `config.toml`.
        let mut app = App::new(Config {
            global_env: vec![plain_var("HTTP_PROXY", "http://old")],
            ..Config::default()
        });
        let persist = Arc::new(FakeConfigPersist::default());
        app.persist = persist.clone();

        let _ = update(
            &mut app,
            Message::EnvVarValueChanged(env_editor::EnvScope::Global, 0, "http://new".into()),
            unused_runner(),
        );
        let task = update(
            &mut app,
            Message::EnvVarValueCommitted(env_editor::EnvScope::Global, 0),
            unused_runner(),
        );

        assert!(task.units() > 0, "the commit must dispatch a persist");
        assert_eq!(app.config.global_env[0].value, "http://new");
        assert!(
            persist.saved().is_empty(),
            "persist.save must not be called synchronously inside update"
        );

        // `Task` is opaque and never driven in a unit test in this file;
        // every other background call is proven by running its async half
        // directly instead, and this follows the same convention.
        let success = block_on(persist_config(app.config.clone(), persist.clone()));

        assert!(success);
        assert_eq!(
            persist.saved().last().unwrap().global_env[0].value,
            "http://new",
            "the persisted config must carry the committed value"
        );
    }

    #[test]
    fn a_typed_value_marked_secret_reaches_the_keychain_having_never_been_in_config() {
        // The workflow the whole editor is built around: type the value, then
        // tick "secret". The plaintext has to arrive in the `SecretStore` --
        // and the config that is saved alongside it must never have held it,
        // at any point in the sequence.
        let mut app = App::new(Config {
            global_env: vec![plain_var("TOKEN", "")],
            ..Config::default()
        });
        let persist = FakeConfigPersist::default();

        let _ = update(
            &mut app,
            Message::EnvVarValueChanged(env_editor::EnvScope::Global, 0, "hunter2".into()),
            unused_runner(),
        );
        let task = update(
            &mut app,
            Message::EnvVarSecretToggled(env_editor::EnvScope::Global, 0),
            unused_runner(),
        );
        assert!(task.units() > 0, "marking secret must dispatch its own job");

        // What `update` handed that job, derived the same way `update` derives
        // it. The `Task` is opaque, so this is the only way to see the config
        // that will actually reach disk -- and seeing it is the point: it is a
        // *copy*, and the plaintext must already be out of it.
        let Some(SecretToggle::Mark {
            candidate,
            plain_value,
        }) = plan_secret_toggle(
            &app.config,
            &app.env_value_drafts,
            env_editor::EnvScope::Global,
            0,
        )
        else {
            panic!("ticking secret on a typed row must plan a keychain write");
        };
        assert_eq!(
            plain_value, "hunter2",
            "the value handed to the keychain must be what the user typed"
        );
        assert_eq!(
            candidate.global_env[0].value, "",
            "and must already be out of the config that goes to disk with it"
        );
        assert!(candidate.global_env[0].secret);

        // The keychain half runs against the in-memory store the real job's
        // `KeyringSecretStore` stands in for.
        let key = SecretKey::GlobalEnv {
            key: "TOKEN".to_string(),
        };
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        assert!(run_secret_write(
            &key,
            &plain_value,
            &candidate,
            &store,
            &persist
        ));

        assert_eq!(
            store.get(&key).unwrap().as_deref(),
            Some("hunter2"),
            "the value the user typed must be what the keychain ends up holding"
        );
        assert!(
            persist
                .saved()
                .iter()
                .all(|c| !format!("{c:?}").contains("hunter2")),
            "no config that was ever handed to a persister may contain the plaintext"
        );

        // Now that it has landed, `app.config` catches up and the draft goes.
        let _ = update(
            &mut app,
            Message::EnvVarSecretWritten {
                scope: env_editor::EnvScope::Global,
                index: 0,
                success: true,
            },
            unused_runner(),
        );
        assert!(app.config.global_env[0].secret);
        assert_eq!(app.config.global_env[0].value, "");
        assert!(
            app.env_value_drafts.is_empty(),
            "the draft is redundant once the keychain holds the value"
        );
    }

    #[test]
    fn a_secret_write_the_keychain_refused_destroys_nothing() {
        // The data-loss path this sequencing exists to close. Marking a row
        // secret does not change a value, it destroys one: the plaintext is
        // blanked out of `Config` and handed to the job. Do that before the
        // keychain has accepted it and a refusal leaves the value nowhere at
        // all -- gone with the job, the row empty and flagged secret, and the
        // *next* unrelated env edit persisting that state, so the copy on
        // disk goes too.
        let mut app = App::new(Config {
            global_env: vec![plain_var("TOKEN", "hunter2")],
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::EnvVarSecretToggled(env_editor::EnvScope::Global, 0),
            unused_runner(),
        );
        assert_eq!(
            app.config.global_env[0].value, "hunter2",
            "nothing may be destroyed while the write is still in flight"
        );

        let _ = update(
            &mut app,
            Message::EnvVarSecretWritten {
                scope: env_editor::EnvScope::Global,
                index: 0,
                success: false,
            },
            unused_runner(),
        );

        assert_eq!(
            app.config.global_env[0].value, "hunter2",
            "a refused write must leave the value exactly where it was"
        );
        assert!(
            !app.config.global_env[0].secret,
            "and must not flag the row secret -- there is no keychain entry behind it"
        );
        assert!(
            app.drawer.status.contains("failed"),
            "the failure must still be reported; got {:?}",
            app.drawer.status
        );
    }

    #[test]
    fn unmarking_a_secret_the_keychain_refused_leaves_the_row_secret() {
        // Less costly than the mark direction -- nothing is destroyed -- but
        // the same speculative edit. A row that reads "not secret" while the
        // keychain still holds the entry and the config on disk still says
        // secret is a lie the next restart silently corrects.
        let mut app = App::new(Config {
            global_env: vec![bombadil_core::model::EnvVar {
                key: "TOKEN".into(),
                value: String::new(),
                secret: true,
            }],
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::EnvVarSecretToggled(env_editor::EnvScope::Global, 0),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::EnvVarSecretWritten {
                scope: env_editor::EnvScope::Global,
                index: 0,
                success: false,
            },
            unused_runner(),
        );

        assert!(
            app.config.global_env[0].secret,
            "a refused delete must leave the row matching what disk and the keychain still say"
        );
    }

    #[test]
    fn unmarking_a_secret_that_landed_clears_the_flag() {
        // The other half: the success path still actually applies. Without
        // this, an implementation that simply never applied the toggle would
        // pass every failure test above.
        let mut app = App::new(Config {
            global_env: vec![bombadil_core::model::EnvVar {
                key: "TOKEN".into(),
                value: String::new(),
                secret: true,
            }],
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::EnvVarSecretToggled(env_editor::EnvScope::Global, 0),
            unused_runner(),
        );
        let _ = update(
            &mut app,
            Message::EnvVarSecretWritten {
                scope: env_editor::EnvScope::Global,
                index: 0,
                success: true,
            },
            unused_runner(),
        );

        assert!(!app.config.global_env[0].secret);
    }

    // --- Task 3: the Scripts tab ---
    //
    // Reordering (criterion 1) and disable-not-delete (criterion 2) as pure
    // logic are covered by `scripts_editor`'s own tests -- `move_up`/
    // `move_down`/`toggle_enabled`/`execution_order` are pure functions, and
    // testing them directly there is a stronger proof than re-deriving the
    // same claim by driving `update`'s opaque `Task`. What belongs here is
    // the wiring: that the messages a real button press sends actually reach
    // those functions and actually persist.

    fn script(path: &str, enabled: bool) -> bombadil_core::model::Script {
        bombadil_core::model::Script {
            path: std::path::PathBuf::from(path),
            args: Vec::new(),
            enabled,
        }
    }

    #[test]
    fn requesting_to_add_a_script_dispatches_work_without_touching_config_synchronously() {
        // Criterion 3: the whole point of going through `pick_file`
        // (`rfd::AsyncFileDialog`) instead of adding a row inline is that
        // nothing about the new script is known -- there is no path -- until
        // the async picker resolves. A synchronous add here would mean the
        // handler bypassed the picker entirely, the exact freeze criterion 3
        // exists to prevent (a modal `rfd::FileDialog` on this same thread
        // would look identical from `update`'s side: config unchanged until
        // the call returns).
        let mut app = App::new(Config::default());

        let task = update(
            &mut app,
            Message::ScriptAddRequested(scripts_editor::ScriptScope::Global),
            unused_runner(),
        );

        assert!(
            task.units() > 0,
            "requesting to add a script must dispatch background work"
        );
        assert!(
            app.config.global_scripts.is_empty(),
            "nothing may be added to the config before the picker has answered; got {:?}",
            app.config.global_scripts
        );
    }

    #[test]
    fn a_picked_script_is_appended_and_persisted() {
        let mut app = App::new(Config::default());
        let persist = Arc::new(FakeConfigPersist::default());
        app.persist = persist.clone();

        let task = update(
            &mut app,
            Message::ScriptFilePicked(
                scripts_editor::ScriptScope::Global,
                Some(std::path::PathBuf::from("/etc/bombadil/pre.sh")),
            ),
            unused_runner(),
        );

        assert_eq!(app.config.global_scripts.len(), 1);
        assert_eq!(
            app.config.global_scripts[0].path,
            std::path::PathBuf::from("/etc/bombadil/pre.sh")
        );
        assert!(
            task.units() > 0,
            "the add must dispatch a background persist"
        );
        assert!(
            persist.saved().is_empty(),
            "persist.save must not be called synchronously inside update"
        );

        let success = block_on(persist_config(app.config.clone(), persist.clone()));
        assert!(success);
        assert_eq!(persist.saved().last().unwrap().global_scripts.len(), 1);
    }

    #[test]
    fn cancelling_the_script_picker_adds_nothing() {
        let mut app = App::new(Config::default());
        let _ = update(
            &mut app,
            Message::ScriptFilePicked(scripts_editor::ScriptScope::Global, None),
            unused_runner(),
        );
        assert!(app.config.global_scripts.is_empty());
    }

    #[test]
    fn disabling_a_script_through_update_leaves_it_in_the_config() {
        // Criterion 2, at the wiring level: the message a real checkbox press
        // sends must land on `toggle_enabled`, not on `remove_script`.
        let mut app = App::new(Config {
            global_scripts: vec![script("/etc/pre.sh", true)],
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::ScriptEnabledToggled(scripts_editor::ScriptScope::Global, 0),
            unused_runner(),
        );

        assert_eq!(
            app.config.global_scripts.len(),
            1,
            "a disabled script must still be in the config, not removed"
        );
        assert!(!app.config.global_scripts[0].enabled);
    }

    #[test]
    fn removing_a_script_through_update_actually_drops_it() {
        // The opposite mistake from the previous test: `ScriptRemoved` must
        // still really remove, so disable-not-delete is not proven by a
        // handler that never deletes anything at all.
        let mut app = App::new(Config {
            global_scripts: vec![script("/etc/pre.sh", true)],
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::ScriptRemoved(scripts_editor::ScriptScope::Global, 0),
            unused_runner(),
        );

        assert!(app.config.global_scripts.is_empty());
    }

    #[test]
    fn moving_a_script_down_through_update_persists_the_new_order() {
        // Criterion 1, at the wiring level.
        let mut app = App::new(Config {
            global_scripts: vec![script("/a.sh", true), script("/b.sh", true)],
            ..Config::default()
        });
        let persist = Arc::new(FakeConfigPersist::default());
        app.persist = persist.clone();

        let task = update(
            &mut app,
            Message::ScriptMovedDown(scripts_editor::ScriptScope::Global, 0),
            unused_runner(),
        );

        assert_eq!(
            app.config.global_scripts[0].path,
            std::path::PathBuf::from("/b.sh"),
            "the order must change the instant update returns; got {:?}",
            app.config.global_scripts
        );
        assert!(task.units() > 0);

        let success = block_on(persist_config(app.config.clone(), persist.clone()));
        assert!(success);
        assert_eq!(
            persist.saved().last().unwrap().global_scripts[0].path,
            std::path::PathBuf::from("/b.sh"),
            "the persisted config must carry the new order, or it reverts on restart"
        );
    }

    #[test]
    fn moving_a_project_scripts_row_never_touches_global_scripts() {
        // The reordering scoping the module doc argues for: a `Project`
        // scope's move must resolve to that project's own list, the same
        // guarantee `project_scope_addresses_that_projects_own_list_not_global`
        // proves for `scripts_mut` directly -- this is the same claim through
        // `update`'s real message.
        let mut proj = project("api");
        proj.scripts = vec![script("/p-a.sh", true), script("/p-b.sh", true)];
        let mut app = App::new(Config {
            global_scripts: vec![script("/g.sh", true)],
            projects: vec![proj],
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::ScriptMovedDown(scripts_editor::ScriptScope::Project(0), 0),
            unused_runner(),
        );

        assert_eq!(
            app.config.global_scripts,
            vec![script("/g.sh", true)],
            "a project-scope reorder must never touch global_scripts"
        );
        assert_eq!(
            app.config.projects[0].scripts[0].path,
            PathBuf::from("/p-b.sh")
        );
    }

    // --- Task 4: the Indexes tab ---
    //
    // The collision check (criterion 2) and the selection invariant
    // (criterion 3) are proven directly against `index_editor`'s own pure
    // functions in its own test module -- a stronger proof than re-deriving
    // the same claim by driving `update`'s opaque `Task`. What belongs here
    // is the wiring: that a real message reaches those functions, that a
    // credential save actually lands in the store and nowhere in `Config`
    // (criterion 1), and that nothing here can ever print one (criterion 4).

    fn index_with_auth(
        name: &str,
        auth: bombadil_core::model::IndexAuth,
    ) -> bombadil_core::model::Index {
        bombadil_core::model::Index {
            name: name.into(),
            url: format!("https://{name}.example/simple"),
            kind: bombadil_core::model::IndexKind::Extra,
            default_for_new_projects: false,
            auth,
        }
    }

    #[test]
    fn renaming_an_index_moves_its_credential_instead_of_orphaning_it() {
        // `SecretKey::Index` is keyed on the index's *name*. `set_name`
        // changed the name and touched nothing else, so the next sync's
        // `envspec::index::inject` looked under the new name, found nothing,
        // and failed with a 401 that nothing on screen explained -- while the
        // old entry stayed in the user's keychain with no way for the app to
        // name it again.
        let config = Config {
            indexes: vec![index_with_auth(
                "corp",
                bombadil_core::model::IndexAuth::Token,
            )],
            ..Config::default()
        };
        let from = SecretKey::Index {
            name: "internal".into(),
        };
        let to = SecretKey::Index {
            name: "corp".into(),
        };
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        store.set(&from, "hunter2").unwrap();
        let persist = FakeConfigPersist::default();

        let success = run_index_credential_rename(&from, &to, &config, &store, &persist);

        assert!(success);
        assert_eq!(
            store.get(&to).unwrap().as_deref(),
            Some("hunter2"),
            "the credential must follow the rename, or the next sync is a 401"
        );
        assert_eq!(
            store.get(&from).unwrap(),
            None,
            "the old account must not stay in the user's keychain forever"
        );
        assert_eq!(
            persist.saved().len(),
            1,
            "the renamed config must be persisted alongside the move"
        );
    }

    #[test]
    fn renaming_an_index_that_never_had_a_credential_still_persists() {
        // The commonest case by far -- an index typed for the first time,
        // one keystroke at a time. Nothing to move must not read as a
        // failure.
        let config = Config::default();
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        let persist = FakeConfigPersist::default();

        let success = run_index_credential_rename(
            &SecretKey::Index { name: "c".into() },
            &SecretKey::Index { name: "co".into() },
            &config,
            &store,
            &persist,
        );

        assert!(success);
        assert_eq!(persist.saved().len(), 1);
    }

    #[test]
    fn typing_an_index_name_neither_saves_the_config_nor_touches_the_keychain() {
        // `IndexNameChanged` used to persist *and* dispatch a credential
        // rename -- a `SecretStore::get` per character typed, on a machine
        // whose keychain may prompt. The typing itself must dispatch nothing.
        let mut app = App::new(Config {
            indexes: vec![index_with_auth(
                "corp",
                bombadil_core::model::IndexAuth::Token,
            )],
            ..Config::default()
        });
        let persist = Arc::new(FakeConfigPersist::default());
        app.persist = persist.clone();

        let mut task_units = 0;
        for prefix in ["c", "co", "cor", "corp", "corp-", "corp-e", "corp-eu"] {
            let task = update(
                &mut app,
                Message::IndexNameChanged(0, prefix.to_string()),
                unused_runner(),
            );
            task_units += task.units();
        }

        assert_eq!(
            task_units, 0,
            "typing a name must dispatch neither a persist nor a keychain job"
        );
        assert!(persist.saved().is_empty());
        assert_eq!(
            app.config.indexes[0].name, "corp-eu",
            "the box must still be responsive -- the edit is in memory"
        );
        assert_eq!(
            index_credential_key(&app, 0),
            Some(SecretKey::Index {
                name: "corp".into()
            }),
            "the credential has not moved yet, so the account it lives under must not have either"
        );
    }

    #[test]
    fn committing_a_renamed_index_moves_its_credential_once() {
        // The other half: one rename per edit instead of one per character,
        // and the entry still ends up under the new name.
        let mut app = App::new(Config {
            indexes: vec![index_with_auth(
                "internal",
                bombadil_core::model::IndexAuth::Token,
            )],
            ..Config::default()
        });
        for prefix in ["c", "co", "cor", "corp"] {
            let _ = update(
                &mut app,
                Message::IndexNameChanged(0, prefix.to_string()),
                unused_runner(),
            );
        }

        // The exact pair of accounts the commit will move between -- one move,
        // for the whole edit, not one per character.
        let (from, to) = index_rename_on_commit(&app, 0).expect("a renamed row owes a move");
        assert_eq!(
            from,
            SecretKey::Index {
                name: "internal".into()
            }
        );
        assert_eq!(
            to,
            SecretKey::Index {
                name: "corp".into()
            }
        );

        let task = update(&mut app, Message::IndexEditCommitted(0), unused_runner());

        assert!(task.units() > 0, "the commit must dispatch the rename job");
        assert_eq!(
            index_rename_on_commit(&app, 0),
            None,
            "the commit must record where the credential now lives, or the next one moves it again"
        );

        // The background half, against the in-memory store the real job's
        // `KeyringSecretStore` stands in for.
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        store.set(&from, "hunter2").unwrap();
        let persist = FakeConfigPersist::default();
        assert!(run_index_credential_rename(
            &from,
            &to,
            &app.config,
            &store,
            &persist
        ));
        assert_eq!(store.get(&to).unwrap().as_deref(), Some("hunter2"));
        assert_eq!(store.get(&from).unwrap(), None);
    }

    #[test]
    fn typing_an_index_url_does_not_save_but_committing_it_does() {
        // The write amplification half, on a field that carries no secret:
        // still one save per edit, not one per character -- and the edit is
        // not lost, because the commit is what saves it.
        let mut app = App::new(Config {
            indexes: vec![index_with_auth(
                "corp",
                bombadil_core::model::IndexAuth::None,
            )],
            ..Config::default()
        });
        let persist = Arc::new(FakeConfigPersist::default());
        app.persist = persist.clone();

        let typing = update(
            &mut app,
            Message::IndexUrlChanged(0, "https://corp.example/simple".to_string()),
            unused_runner(),
        );
        assert_eq!(typing.units(), 0, "typing a URL must dispatch no save");

        let task = update(&mut app, Message::IndexEditCommitted(0), unused_runner());
        assert!(task.units() > 0, "the commit must dispatch one");

        assert!(block_on(persist_config(
            app.config.clone(),
            persist.clone()
        )));
        assert_eq!(
            persist.saved().last().unwrap().indexes[0].url,
            "https://corp.example/simple",
            "the committed URL must reach the persisted config"
        );
    }

    #[test]
    fn removing_an_index_deletes_the_credential_it_leaves_behind() {
        // The other half of the same defect: `remove_index` never touched
        // `SecretKey::Index`, so a removed index's credential stayed in the
        // keychain forever with nothing left in the app able to name it.
        // The account has to be resolved *before* the row goes -- afterwards
        // there is no name to find it by.
        let mut app = App::new(Config {
            indexes: vec![index_with_auth(
                "corp",
                bombadil_core::model::IndexAuth::Token,
            )],
            ..Config::default()
        });
        let key = index_credential_key(&app, 0)
            .expect("a removed index must name the credential it owes a delete of");
        assert_eq!(
            key,
            SecretKey::Index {
                name: "corp".into()
            }
        );

        let store = bombadil_core::secrets::InMemorySecretStore::new();
        store.set(&key, "hunter2").unwrap();
        let persist = FakeConfigPersist::default();

        let task = update(&mut app, Message::IndexRemoved(0), unused_runner());
        assert!(task.units() > 0, "the removal must dispatch its own job");
        assert!(app.config.indexes.is_empty());

        // The background half, with the fake store the real job's
        // `KeyringSecretStore` stands in for.
        assert!(run_secret_delete(&key, &app.config, &store, &persist));
        assert_eq!(
            store.get(&key).unwrap(),
            None,
            "the credential must be gone, not orphaned"
        );
    }

    #[test]
    fn removing_a_secret_env_var_row_deletes_its_keychain_entry() {
        // The asymmetry the review found: `env_editor::unmark_secret` deletes,
        // but removing the row entirely did not -- so the value stayed in the
        // keychain forever, unreachable, while the row that named it was gone.
        let project_id = uuid::Uuid::from_u128(9);
        let mut proj = project("api");
        proj.id = project_id;
        proj.env = vec![bombadil_core::model::EnvVar {
            key: "TOKEN".into(),
            value: String::new(),
            secret: true,
        }];
        let mut app = App::new(Config {
            projects: vec![proj],
            ..Config::default()
        });

        // Resolved before the row goes: `SecretKey::ProjectEnv` is keyed on
        // the variable's name, which the removal is about to take away.
        let key = env_removal_secret_key(&app, env_editor::EnvScope::Project(0), 0)
            .expect("removing a secret row must name the entry it owes a delete of");
        assert_eq!(
            key,
            SecretKey::ProjectEnv {
                project_id,
                key: "TOKEN".into(),
            }
        );

        let store = bombadil_core::secrets::InMemorySecretStore::new();
        store.set(&key, "hunter2").unwrap();
        let persist = FakeConfigPersist::default();

        let task = update(
            &mut app,
            Message::EnvVarRemoved(env_editor::EnvScope::Project(0), 0),
            unused_runner(),
        );
        assert!(task.units() > 0, "the removal must dispatch its own job");
        assert!(app.config.projects[0].env.is_empty());

        assert!(run_secret_delete(&key, &app.config, &store, &persist));
        assert_eq!(store.get(&key).unwrap(), None);
    }

    #[test]
    fn removing_a_plain_env_var_row_owes_no_keychain_delete() {
        // The opposite mistake: an ordinary variable has no keychain entry,
        // and issuing a delete for one would be a keychain call per removal
        // on a machine that may have no keychain at all.
        let mut proj = project("api");
        proj.env = vec![bombadil_core::model::EnvVar {
            key: "HTTP_PROXY".into(),
            value: "http://proxy:8080".into(),
            secret: false,
        }];
        let app = App::new(Config {
            projects: vec![proj],
            ..Config::default()
        });

        assert!(env_removal_secret_key(&app, env_editor::EnvScope::Project(0), 0).is_none());
    }

    #[test]
    fn saving_an_index_credential_writes_the_store_and_leaves_config_untouched() {
        // Criterion 1: assert both halves -- the store holds the value the
        // user typed, and the persisted `Config` carries no trace of it
        // anywhere. `Index` has no plaintext field a credential could land in
        // even by accident (see `index_editor`'s module doc), so the
        // equality check below and the `Debug`-text check are both
        // belt-and-suspenders against a future field addition doing exactly
        // that, not just a claim that passes by the type's current shape.
        let idx = index_with_auth(
            "priv",
            bombadil_core::model::IndexAuth::Basic {
                username: "alice".into(),
            },
        );
        let mut app = App::new(Config {
            indexes: vec![idx.clone()],
            ..Config::default()
        });
        let persist = Arc::new(FakeConfigPersist::default());
        app.persist = persist.clone();
        app.index_credential_drafts = vec!["hunter2".to_string()];

        // What `update` is actually expected to hand the keychain job --
        // derived the same way `update` derives it (`index_editor::credential_save`),
        // not hardcoded here, so a mutation to either the key or the value it
        // builds is caught below rather than only by `index_editor`'s own
        // test of that function in isolation.
        let (key, value) =
            index_editor::credential_save(&app.config, &app.index_credential_drafts, 0)
                .expect("nothing here should block the save");
        assert_eq!(
            key,
            SecretKey::Index {
                name: "priv".into()
            },
            "a credential must be addressed by SecretKey::Index, not any other shape"
        );

        let task = update(
            &mut app,
            Message::IndexCredentialSaveRequested(0),
            unused_runner(),
        );

        assert!(
            task.units() > 0,
            "the keychain write must be dispatched as background work, not run inline"
        );
        assert_eq!(
            app.index_credential_drafts[0], "hunter2",
            "the draft must survive until the write lands -- it is the only copy"
        );
        assert!(
            persist.saved().is_empty(),
            "persist.save must not be called synchronously inside update"
        );

        // The background half: never touches a real keychain, per the hard
        // security constraint -- `InMemorySecretStore` only, driven through
        // `run_secret_write` directly rather than the real caller
        // (`mark_secret_job`), which always constructs a `KeyringSecretStore`.
        // Same substitution the existing env-var secret test makes for the
        // same reason.
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        let success = run_secret_write(&key, &value, &app.config, &store, persist.as_ref());

        assert!(success);
        assert_eq!(
            store.get(&key).unwrap().as_deref(),
            Some("hunter2"),
            "the store must hold the value the user typed"
        );

        let saved = persist.saved();
        assert_eq!(
            saved.len(),
            1,
            "the config must be persisted alongside the write"
        );
        assert_eq!(
            saved[0].indexes[0], idx,
            "the persisted index must be unchanged -- no field gained the credential"
        );
        let rendered = format!("{:?}", saved[0]);
        assert!(
            !rendered.contains("hunter2"),
            "the persisted config must never carry the credential, even in Debug; got:\n{rendered}"
        );

        // And now the write has landed, so the draft goes.
        let _ = update(
            &mut app,
            Message::IndexCredentialWritten {
                index: 0,
                success: true,
            },
            unused_runner(),
        );
        assert_eq!(
            app.index_credential_drafts[0], "",
            "once the keychain holds it, it must not stay in App state readable by a render"
        );
        assert_eq!(
            app.index_credential_names[0], "priv",
            "and the account it now lives under must be recorded"
        );
    }

    #[test]
    fn a_credential_the_keychain_refused_is_still_in_the_box() {
        // The user typed it and pressed save; the box is the only place that
        // value exists. Clearing it on the way to a keychain that then
        // refused would lose it outright, and the status line does not give
        // it back.
        //
        // `index_credential_names` matters just as much and is less visible:
        // recording the new account for a write that never happened points a
        // later rename commit at an entry the keychain has never heard of, so
        // the move finds nothing, and the real credential is left behind
        // under the old name.
        let mut app = App::new(Config {
            indexes: vec![index_with_auth(
                "renamed",
                bombadil_core::model::IndexAuth::Basic {
                    username: "alice".into(),
                },
            )],
            ..Config::default()
        });
        app.index_credential_drafts = vec!["hunter2".to_string()];
        app.index_credential_names = vec!["original".to_string()];

        let _ = update(
            &mut app,
            Message::IndexCredentialWritten {
                index: 0,
                success: false,
            },
            unused_runner(),
        );

        assert_eq!(
            app.index_credential_drafts[0], "hunter2",
            "a refused write must leave the typed credential where the user can retry it"
        );
        assert_eq!(
            app.index_credential_names[0], "original",
            "and must not claim the credential moved to an account that holds nothing"
        );
        assert!(
            app.drawer.status.contains("failed"),
            "the failure must still be reported; got {:?}",
            app.drawer.status
        );
    }

    #[test]
    fn a_colliding_index_name_dispatches_no_credential_save() {
        // Criterion 2: the message a real "save credential" press sends
        // never reaches the keychain when the row's current name collides
        // with another's credential variable -- caught here, before the
        // background job (and so before `inject`'s own later refusal) rather
        // than only at sync time.
        let a = index_with_auth("my-index", bombadil_core::model::IndexAuth::None);
        let b = index_with_auth(
            "my_index",
            bombadil_core::model::IndexAuth::Basic {
                username: "alice".into(),
            },
        );
        let mut app = App::new(Config {
            indexes: vec![a, b],
            ..Config::default()
        });
        app.index_credential_drafts = vec![String::new(), "hunter2".to_string()];
        let persist = Arc::new(FakeConfigPersist::default());
        app.persist = persist.clone();

        let task = update(
            &mut app,
            Message::IndexCredentialSaveRequested(1),
            unused_runner(),
        );

        assert_eq!(
            task.units(),
            0,
            "a colliding row must dispatch nothing, not attempt the write anyway"
        );
        assert_eq!(
            app.index_credential_drafts[1], "hunter2",
            "the draft must be left untouched since nothing was saved"
        );
        assert!(persist.saved().is_empty());
    }

    #[test]
    fn no_index_message_ever_prints_the_credential_in_debug() {
        // Criterion 4, at the `Message` level: every message this tab's
        // widgets can actually send, checked against the one string that
        // must never appear in any of their `Debug` renderings.
        let secret = "hunter2";
        let messages: Vec<Message> = vec![
            Message::IndexCredentialChanged(0, index_editor::CredentialInput(secret.to_string())),
            Message::IndexCredentialSaveRequested(0),
            Message::Persisted {
                what: "index change",
                success: true,
            },
            Message::Persisted {
                what: "index change",
                success: false,
            },
        ];
        for msg in &messages {
            let rendered = format!("{msg:?}");
            assert!(
                !rendered.contains(secret),
                "message must never print the credential; got {rendered:?}"
            );
        }
    }

    #[test]
    fn toggling_a_project_index_updates_only_that_projects_selection() {
        // Criterion 3, the wiring: the message a real checkbox press sends
        // reaches `index_editor::toggle_selected` on the right project's
        // `index_names`, and `Config::indexes` -- the shared configuration --
        // is provably still there afterwards.
        let idx = index_with_auth("priv", bombadil_core::model::IndexAuth::None);
        let mut proj = project("api");
        proj.index_names = vec!["priv".to_string()];
        let mut app = App::new(Config {
            indexes: vec![idx],
            projects: vec![proj],
            ..Config::default()
        });
        let persist = Arc::new(FakeConfigPersist::default());
        app.persist = persist.clone();

        let task = update(
            &mut app,
            Message::ProjectIndexToggled(0, "priv".to_string()),
            unused_runner(),
        );

        assert!(
            app.config.projects[0].index_names.is_empty(),
            "deselecting must remove the association"
        );
        assert_eq!(
            app.config.indexes.len(),
            1,
            "deselecting must never remove the configured index itself"
        );
        assert_eq!(app.config.indexes[0].name, "priv");
        assert!(
            task.units() > 0,
            "the change must dispatch a background persist"
        );
        assert!(persist.saved().is_empty());

        let success = block_on(persist_config(app.config.clone(), persist.clone()));
        assert!(success);
        assert!(
            persist.saved().last().unwrap().projects[0]
                .index_names
                .is_empty(),
            "the persisted config must carry the deselection"
        );
        assert_eq!(
            persist.saved().last().unwrap().indexes.len(),
            1,
            "the persisted config must still carry the configured index"
        );
    }

    #[test]
    fn adding_and_removing_an_index_keeps_the_credential_drafts_in_step() {
        let mut app = App::new(Config::default());

        let _ = update(&mut app, Message::IndexAdded, unused_runner());
        let _ = update(&mut app, Message::IndexAdded, unused_runner());
        assert_eq!(app.config.indexes.len(), 2);
        assert_eq!(
            app.index_credential_drafts.len(),
            2,
            "one draft slot per configured index"
        );

        let _ = update(
            &mut app,
            Message::IndexCredentialChanged(1, index_editor::CredentialInput("typing".into())),
            unused_runner(),
        );
        assert_eq!(app.index_credential_drafts[1], "typing");

        let _ = update(&mut app, Message::IndexRemoved(0), unused_runner());
        assert_eq!(app.config.indexes.len(), 1);
        assert_eq!(
            app.index_credential_drafts.len(),
            1,
            "removing a row must remove its draft slot too"
        );
    }

    // --- Task 5: the monorepo member checklist ---
    //
    // The five criteria (join on path, select-all-by-default, `effective`
    // decides, removal-warns-first, exclude-not-offered) are proven directly
    // against `members_editor`'s own pure functions in its own test module --
    // a stronger proof than re-deriving the same claims by driving `update`'s
    // opaque `Task`. What belongs here is the wiring: that selecting a
    // project fetches its members off the render thread, that a landed fetch
    // is staleness-guarded the same way `DepsLoaded` is, that a toggle
    // persists, and that the sync confirmation actually gates dispatch on
    // `members_editor::removal_warning`.

    fn workspace_kind(globs: &[&str]) -> bombadil_core::members::WorkspaceKind {
        bombadil_core::members::WorkspaceKind::Workspace {
            member_globs: globs.iter().map(|s| s.to_string()).collect(),
            exclude_globs: Vec::new(),
            root_project: None,
        }
    }

    fn discovered_member(
        path: &str,
        name: Option<&str>,
        is_workspace_member: bool,
    ) -> bombadil_core::members::DiscoveredMember {
        bombadil_core::members::DiscoveredMember {
            relative_path: std::path::PathBuf::from(path),
            name: name.map(String::from),
            is_workspace_member,
        }
    }

    fn installed_editable(location: &str) -> InstalledPackage {
        InstalledPackage {
            name: "whatever-uv-calls-it".to_string(),
            version: "0.1.0".to_string(),
            editable_project_location: Some(std::path::PathBuf::from(location)),
        }
    }

    #[test]
    fn selecting_a_project_fetches_its_members_through_job_run_not_inline() {
        let config = Config {
            projects: vec![project("api")],
            ..Config::default()
        };
        let mut app = App::new(config);
        let fake = Arc::new(FakeCommandRunner::new());

        let task = update(&mut app, Message::ProjectSelected(0), fake.clone());

        assert!(
            fake.calls().is_empty(),
            "the members fetch must not call the runner synchronously inside update; got {:?}",
            fake.calls()
        );
        assert!(
            task.units() > 0,
            "selecting a project must dispatch real background work, not Task::none()"
        );
    }

    #[test]
    fn a_members_fetch_result_is_stored_against_its_project_index() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let fetch = MembersFetch {
            kind: bombadil_core::members::WorkspaceKind::Loose,
            discovered: vec![discovered_member("packages/alpha", Some("alpha"), false)],
            installed: vec![],
        };

        let _ = update(&mut app, Message::MembersLoaded(0, fetch), unused_runner());

        let (index, stored) = app
            .members
            .as_ref()
            .expect("a landed fetch must populate app.members");
        assert_eq!(*index, 0);
        assert_eq!(stored.discovered.len(), 1);
    }

    #[test]
    fn a_stale_members_fetch_for_a_project_that_no_longer_exists_is_dropped() {
        let mut app = App::new(Config::default());
        let fetch = MembersFetch {
            kind: bombadil_core::members::WorkspaceKind::Loose,
            discovered: vec![],
            installed: vec![],
        };

        let _ = update(&mut app, Message::MembersLoaded(0, fetch), unused_runner());

        assert!(app.members.is_none());
    }

    #[test]
    fn removing_the_displayed_project_clears_its_stale_members_state() {
        let mut app = App::new(Config {
            projects: vec![project("api"), project("web")],
            ..Config::default()
        });
        app.members = Some((
            0,
            MembersFetch {
                kind: bombadil_core::members::WorkspaceKind::Loose,
                discovered: vec![],
                installed: vec![],
            },
        ));
        app.confirm_member_sync = Some(members_editor::ConfirmMemberSync {
            project_index: 0,
            label: "api".to_string(),
            warning: "would remove alpha".to_string(),
        });

        let _ = update(
            &mut app,
            Message::RemoveProjectRequested(0),
            unused_runner(),
        );

        assert!(app.members.is_none());
        assert!(app.confirm_member_sync.is_none());
    }

    #[test]
    fn toggling_a_member_updates_the_selection_in_memory_and_dispatches_a_persist() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.members = Some((
            0,
            MembersFetch {
                kind: workspace_kind(&["packages/*"]),
                discovered: vec![discovered_member("packages/alpha", Some("alpha"), true)],
                installed: vec![],
            },
        ));

        let task = update(
            &mut app,
            Message::MemberToggled(0, std::path::PathBuf::from("packages/alpha")),
            unused_runner(),
        );

        assert_eq!(
            app.config.projects[0].members.entries.len(),
            1,
            "the deselection must land in the project's own selection, in memory, synchronously"
        );
        assert!(!app.config.projects[0].members.entries[0].selected);
        assert!(
            task.units() > 0,
            "a selection change must dispatch a background persist"
        );
    }

    #[test]
    fn sync_requested_opens_a_confirmation_instead_of_syncing_when_something_would_be_removed() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.selected = Some(0);
        // alpha is installed editable, but deselected -- exactly criterion 4's case.
        app.config.projects[0].members = bombadil_core::model::MemberSelection {
            all_discovered: true,
            entries: vec![bombadil_core::model::MemberRef {
                path: std::path::PathBuf::from("packages/alpha"),
                origin: bombadil_core::model::MemberOrigin::Discovered,
                selected: false,
                name: None,
            }],
        };
        app.members = Some((
            0,
            MembersFetch {
                kind: workspace_kind(&["packages/*"]),
                discovered: vec![discovered_member("packages/alpha", Some("alpha"), true)],
                installed: vec![installed_editable("/p/api/packages/alpha")],
            },
        ));

        let task = update(&mut app, Message::MembersSyncRequested(0), unused_runner());

        let confirm = app
            .confirm_member_sync
            .as_ref()
            .expect("must open a confirmation before removing anything");
        assert!(confirm.warning.contains("alpha"), "got {confirm:?}");
        assert_eq!(
            task.units(),
            0,
            "nothing must be synced before the user confirms"
        );
        assert!(can_start(&app));
    }

    #[test]
    fn the_sync_button_in_a_workspace_root_installs_its_members() {
        // Plain `uv sync` in a workspace root installs *no* members --
        // verified against a real uv, which is why
        // `uv::command::sync_all_members` exists at all. The header's Sync
        // button ran the bare `["sync"]` anyway, so member-aware sync was
        // reachable only from the Members tab and the obvious button quietly
        // produced an environment with none of the workspace's own packages
        // in it.
        //
        // `app.members` is already populated by the time Sync is pressable:
        // `ProjectSelected` dispatches the deps and members fetches together.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            settings: fake_uv_settings(),
            ..Config::default()
        });
        app.members = Some((
            0,
            MembersFetch {
                kind: workspace_kind(&["packages/*"]),
                discovered: vec![discovered_member("packages/alpha", Some("alpha"), true)],
                installed: vec![],
            },
        ));

        let task = update(&mut app, Message::SyncRequested(0), unused_runner());
        assert!(task.units() > 0, "the Sync press must dispatch real work");
        assert!(
            app.drawer.status.contains("members"),
            "the dispatched job must be the member-aware one -- the plain path \
             names no members; got {:?}",
            app.drawer.status
        );

        let target = sync_target(&app, 0).expect("the press must resolve a project");
        let (kind, discovered) = members_context(&app, 0)
            .expect("a Sync press must route through this project's member-aware plan");

        let store = bombadil_core::secrets::InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();
        version_probe(&runner);
        runner.expect(
            "uv",
            &["sync", "--all-packages"],
            bombadil_core::process::CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let (success, lines) = collect_lines(|on_line| {
            run_members_sync(
                &target,
                &app.config,
                &kind,
                &discovered,
                &store,
                &runner,
                on_line,
            )
        });
        assert!(success, "got {lines:?}");

        let calls = runner.calls();
        assert!(
            calls
                .iter()
                .any(|c| c.args.iter().any(|a| a == "--all-packages")),
            "a workspace root's Sync must install its members; got {:?}",
            calls.iter().map(|c| c.args.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_ordinary_project_sync_is_left_exactly_as_it_was() {
        // The other half: `SyncPlan::Plain` is what makes routing every Sync
        // through the member machinery a no-op for a project that is not a
        // workspace at all. If this ever became `--all-packages` or
        // `--no-install-workspace`, every everyday sync would change shape.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            settings: fake_uv_settings(),
            ..Config::default()
        });
        // A landed fetch for a project that is not a workspace at all: the
        // case `SyncPlan::Plain` exists for, and the one that makes routing
        // every Sync through the member machinery safe.
        app.members = Some((
            0,
            MembersFetch {
                kind: bombadil_core::members::WorkspaceKind::Loose,
                discovered: vec![],
                installed: vec![],
            },
        ));
        let target = sync_target(&app, 0).expect("the press must resolve a project");

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

        assert!(
            members_context(&app, 0).is_none(),
            "a project whose plan is Plain must take the plain path, argv and \
             status line included"
        );
        let (success, lines) =
            collect_lines(|on_line| run_sync(&target, &app.config, &store, &runner, on_line));
        assert!(success, "got {lines:?}");

        let calls = runner.calls();
        let sync = calls
            .iter()
            .find(|c| c.args.iter().any(|a| a == "sync"))
            .expect("uv sync must have been run");
        assert_eq!(
            sync.args,
            vec!["sync".to_string()],
            "an ordinary project's sync must stay a bare `uv sync`"
        );
    }

    #[test]
    fn sync_requested_syncs_immediately_when_nothing_would_be_removed() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.selected = Some(0);
        app.members = Some((
            0,
            MembersFetch {
                kind: workspace_kind(&["packages/*"]),
                discovered: vec![discovered_member("packages/alpha", Some("alpha"), true)],
                installed: vec![],
            },
        ));

        let task = update(&mut app, Message::MembersSyncRequested(0), unused_runner());

        assert!(
            app.confirm_member_sync.is_none(),
            "nothing selected is being removed, so no confirmation is needed"
        );
        assert!(
            task.units() > 0,
            "syncing must be dispatched immediately when nothing would be removed"
        );
        assert!(!can_start(&app));
    }

    #[test]
    fn confirming_a_member_sync_dispatches_it_and_clears_the_confirmation() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.members = Some((
            0,
            MembersFetch {
                kind: workspace_kind(&["packages/*"]),
                discovered: vec![discovered_member("packages/alpha", Some("alpha"), true)],
                installed: vec![installed_editable("/p/api/packages/alpha")],
            },
        ));
        app.confirm_member_sync = Some(members_editor::ConfirmMemberSync {
            project_index: 0,
            label: "api".to_string(),
            warning: "would remove alpha".to_string(),
        });

        let task = update(&mut app, Message::MembersSyncConfirmed, unused_runner());

        assert!(app.confirm_member_sync.is_none());
        assert!(task.units() > 0, "confirming must dispatch the sync");
        assert!(!can_start(&app));
    }

    #[test]
    fn cancelling_a_member_sync_dispatches_nothing_and_clears_the_confirmation() {
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        app.confirm_member_sync = Some(members_editor::ConfirmMemberSync {
            project_index: 0,
            label: "api".to_string(),
            warning: "would remove alpha".to_string(),
        });

        let task = update(&mut app, Message::MembersSyncCancelled, unused_runner());

        assert!(app.confirm_member_sync.is_none());
        assert_eq!(task.units(), 0);
        assert!(can_start(&app));
    }

    // --- Task 6: the Settings tab ---

    fn version_outcome(line: &str) -> bombadil_core::process::CommandOutcome {
        bombadil_core::process::CommandOutcome {
            status: Some(0),
            stdout: line.into(),
            stderr: String::new(),
        }
    }

    // --- criterion 1: a `uv_source` change takes effect on the next
    // resolve, without restarting ---

    #[test]
    fn changing_uv_source_takes_effect_on_the_next_resolve_without_restarting() {
        // The bug this catches: a uv binary resolved once (at boot, or on
        // first use) and cached somewhere on `App`, so the UI and the next
        // `uv sync` disagree until the process restarts. There is no such
        // cache to poke directly -- `resolve_uv_for` takes `&Config` fresh on
        // every call -- so the proof is behavioural: resolve twice against
        // the SAME running `App`, with the settings change landing via
        // `update` in between, using two runners that each answer for only
        // one of the two paths. If the change had not really landed in
        // `app.config`, or `update` mutated some other copy of it, the
        // second resolve would be asked of a runner with no rule for the old
        // path and fail instead of succeeding against the new one.
        let mut app = App::new(Config {
            settings: Settings {
                uv_source: UvSource::Custom {
                    path: PathBuf::from("/bin/uv-old"),
                },
                ..Settings::default()
            },
            ..Config::default()
        });

        let old_runner = FakeCommandRunner::new();
        old_runner.expect("uv-old", &["--version"], version_outcome("uv 0.12.1"));
        let before =
            resolve_uv_for(&app.config, &old_runner).expect("the configured old source resolves");
        assert_eq!(before.path, PathBuf::from("/bin/uv-old"));

        // Simulates a validated "use this uv" press landing -- criterion 2's
        // own message is what a real change goes through; this test's
        // subject is what happens to *resolution* afterward, not the
        // validation itself.
        let _ = update(
            &mut app,
            Message::SettingsUvCustomValidated(Ok(uv::ResolvedUv {
                path: PathBuf::from("/bin/uv-new"),
                version: uv::UvVersion {
                    major: 0,
                    minor: 12,
                    patch: 1,
                },
                origin: uv::UvOrigin::Configured,
                newer_than_bundled: false,
            })),
            unused_runner(),
        );
        assert_eq!(
            app.config.settings.uv_source,
            UvSource::Custom {
                path: PathBuf::from("/bin/uv-new")
            },
            "the landed validation must write Settings::uv_source immediately"
        );

        let new_runner = FakeCommandRunner::new();
        new_runner.expect("uv-new", &["--version"], version_outcome("uv 0.12.1"));
        let after = resolve_uv_for(&app.config, &new_runner)
            .expect("the new source must resolve on the very next call, with no restart");
        assert_eq!(after.path, PathBuf::from("/bin/uv-new"));
    }

    // --- criterion 2: a custom uv binary is validated, through `job::run`,
    // before it is ever saved ---

    #[test]
    fn requesting_custom_uv_validation_never_probes_the_runner_synchronously() {
        let mut app = App::new(Config::default());
        app.uv_custom_path_draft = "/opt/uv".to_string();
        let fake = Arc::new(FakeCommandRunner::new());

        let task = update(
            &mut app,
            Message::SettingsUvCustomValidateRequested,
            fake.clone(),
        );

        assert!(
            fake.calls().is_empty(),
            "validating a uv binary must spawn it through job::run, not inline inside update; got {:?}",
            fake.calls()
        );
        assert!(
            task.units() > 0,
            "a validate request must dispatch real background work"
        );
    }

    #[test]
    fn a_custom_binary_below_the_minimum_version_is_refused_and_reported_as_such() {
        // Criterion 2's version check: `uv::resolve`'s own `TooOld` text,
        // not a second copy of the comparison.
        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.9.0"));

        let err = validate_uv_source_blocking(PathBuf::from("/opt/uv"), Config::default(), &runner)
            .expect_err("a version below MIN_UV_VERSION must be refused");

        assert!(
            err.contains("0.9.0"),
            "the offending version must be named; got {err}"
        );
        assert!(
            err.contains("0.12.1"),
            "the minimum it fell short of must be named too; got {err}"
        );
    }

    #[test]
    fn a_refused_custom_validation_never_writes_settings_or_persists() {
        // The exact bug criterion 2 exists to catch: a bad path saved before
        // it was ever confirmed to run, discovered later at spawn time far
        // from this screen. `SettingsUvCustomValidated(Err(_))` -- the only
        // message a failed validation can produce -- must leave `Settings`
        // and the persisted config exactly as they were.
        let original = UvSource::Bundled;
        let mut app = App::new(Config {
            settings: Settings {
                uv_source: original.clone(),
                ..Settings::default()
            },
            ..Config::default()
        });
        let persist = Arc::new(FakeConfigPersist::default());
        app.persist = persist.clone();

        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.9.0"));
        let err =
            validate_uv_source_blocking(PathBuf::from("/opt/uv"), app.config.clone(), &runner)
                .unwrap_err();

        let task = update(
            &mut app,
            Message::SettingsUvCustomValidated(Err(err)),
            unused_runner(),
        );

        assert_eq!(
            app.config.settings.uv_source, original,
            "a refused path must never reach Settings"
        );
        assert_eq!(
            task.units(),
            0,
            "a refusal must not dispatch a persist -- there is nothing new to save"
        );
        assert!(
            persist.saved().is_empty(),
            "a refused validation must never be persisted"
        );
    }

    #[test]
    fn an_accepted_custom_validation_writes_settings_and_persists_the_probed_path() {
        let mut app = App::new(Config::default());
        let persist = Arc::new(FakeConfigPersist::default());
        app.persist = persist.clone();

        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.12.1"));
        let resolved =
            validate_uv_source_blocking(PathBuf::from("/opt/uv"), app.config.clone(), &runner)
                .expect("uv 0.12.1 meets MIN_UV_VERSION");

        let _ = update(
            &mut app,
            Message::SettingsUvCustomValidated(Ok(resolved)),
            unused_runner(),
        );

        assert_eq!(
            app.config.settings.uv_source,
            UvSource::Custom {
                path: PathBuf::from("/opt/uv")
            }
        );

        let success = block_on(persist_config(app.config.clone(), persist.clone()));
        assert!(success);
        assert_eq!(
            persist.saved().last().unwrap().settings.uv_source,
            UvSource::Custom {
                path: PathBuf::from("/opt/uv")
            },
            "the accepted, probed path must be what is persisted"
        );
    }

    // --- criterion 3: `default_venv_location` changes the resolved path
    // for Default projects, and never moves or deletes an existing venv ---

    #[test]
    fn changing_the_default_venv_location_changes_the_resolved_path_for_a_default_project() {
        let project_at = |dir: &Path| Project {
            environments: vec![bombadil_core::model::Environment {
                location: bombadil_core::model::VenvLocation::Default,
                python: PythonPin::Unpinned,
            }],
            active: bombadil_core::model::VenvLocation::Default,
            pyproject_path: dir.join("pyproject.toml"),
            ..project("api")
        };
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(Config {
            projects: vec![project_at(dir.path())],
            settings: Settings {
                default_venv_location: bombadil_core::model::DefaultVenvLocation::Alongside,
                ..Settings::default()
            },
            ..Config::default()
        });
        let before = venv_path::resolve(
            &app.config.projects[0],
            app.config.projects[0]
                .active_environment()
                .expect("this project has an environment"),
            &app.config.settings,
        )
        .unwrap();
        assert_eq!(before, dir.path().join(".venv"));

        let central = tempfile::tempdir().unwrap();
        let _ = update(
            &mut app,
            Message::SettingsDefaultVenvLocationFolderPicked(Some(central.path().to_path_buf())),
            unused_runner(),
        );

        assert_eq!(
            app.config.settings.default_venv_location,
            bombadil_core::model::DefaultVenvLocation::Central {
                path: central.path().to_path_buf()
            }
        );
        let after = venv_path::resolve(
            &app.config.projects[0],
            app.config.projects[0]
                .active_environment()
                .expect("this project has an environment"),
            &app.config.settings,
        )
        .unwrap();
        assert_ne!(
            after, before,
            "the resolved path for a Default project must change with the setting"
        );
        assert!(
            after.starts_with(central.path()),
            "got {after:?}, expected it under {central:?}",
            central = central.path()
        );
    }

    #[test]
    fn changing_the_default_venv_location_reprobes_every_project() {
        // `entries[i].venv_exists` and `python_version` describe the location
        // that was resolved when the probe ran. Changing the default moves
        // where every `VenvLocation::Default` project resolves, so without a
        // re-probe the sidebar goes on describing an environment nothing is
        // going to use -- green ticks and Python versions for venvs at the
        // old path -- until the app is restarted.
        // `AddProjectConfirmFinished` already re-dispatches for exactly this
        // reason.
        let default_project = |label: &str| Project {
            environments: vec![bombadil_core::model::Environment {
                location: bombadil_core::model::VenvLocation::Default,
                python: PythonPin::Unpinned,
            }],
            active: bombadil_core::model::VenvLocation::Default,
            ..project(label)
        };
        let mut app = App::new(Config {
            projects: vec![default_project("api"), default_project("web")],
            ..Config::default()
        });

        let alongside = update(
            &mut app,
            Message::SettingsDefaultVenvLocationAlongsideSelected,
            unused_runner(),
        );
        assert_eq!(
            alongside.units(),
            8,
            "the persist, one venv, git and sync-check probe per project, and \
             one Python listing; got {}",
            alongside.units()
        );

        let picked = update(
            &mut app,
            Message::SettingsDefaultVenvLocationFolderPicked(Some(PathBuf::from("/central"))),
            unused_runner(),
        );
        assert_eq!(
            picked.units(),
            8,
            "picking a central folder must re-probe too; got {}",
            picked.units()
        );
    }

    #[test]
    fn changing_the_default_venv_location_never_touches_an_existing_venv_on_disk() {
        // The half of criterion 3 that matters most: Bombadil must never
        // move or delete a venv as a side effect of a settings change.
        // `venv_path::resolve` performs no filesystem access at all (see its
        // own doc), and the only call site for the crate's one destructive
        // operation, `venv_path::delete`, is `RecreateVenvConfirmed`, gated
        // on an explicit user confirmation -- nothing reachable from
        // `SettingsDefaultVenvLocation*` calls it. This does not take that on
        // faith: it puts a real venv on disk at the old location and checks
        // it is untouched after the setting changes.
        let dir = tempfile::tempdir().unwrap();
        let old_venv = dir.path().join(".venv");
        std::fs::create_dir_all(old_venv.join("lib")).unwrap();
        std::fs::write(
            old_venv.join("pyvenv.cfg"),
            "home = /usr/bin\nversion_info = 3.12.1\n",
        )
        .unwrap();
        std::fs::write(old_venv.join("lib").join("marker"), "still here").unwrap();

        let mut app = App::new(Config {
            projects: vec![Project {
                environments: vec![bombadil_core::model::Environment {
                    location: bombadil_core::model::VenvLocation::Default,
                    python: PythonPin::Unpinned,
                }],
                active: bombadil_core::model::VenvLocation::Default,
                pyproject_path: dir.path().join("pyproject.toml"),
                ..project("api")
            }],
            settings: Settings {
                default_venv_location: bombadil_core::model::DefaultVenvLocation::Alongside,
                ..Settings::default()
            },
            ..Config::default()
        });

        let central = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeCommandRunner::new());
        let _ = update(
            &mut app,
            Message::SettingsDefaultVenvLocationFolderPicked(Some(central.path().to_path_buf())),
            fake.clone(),
        );

        let new_path = venv_path::resolve(
            &app.config.projects[0],
            app.config.projects[0]
                .active_environment()
                .expect("this project has an environment"),
            &app.config.settings,
        )
        .unwrap();
        assert!(
            old_venv.exists() && old_venv.join("pyvenv.cfg").exists(),
            "the venv at the OLD resolved location must survive the settings change untouched"
        );
        assert_eq!(
            std::fs::read_to_string(old_venv.join("lib").join("marker")).unwrap(),
            "still here",
            "the old venv's contents must be byte-for-byte untouched, not just its directory"
        );
        assert!(
            !new_path.exists(),
            "nothing may be created at the new location either -- only the computed path changes, \
             the venv is neither moved nor recreated"
        );
        assert!(
            fake.calls().is_empty(),
            "a settings change must spawn no process at all; got {:?}",
            fake.calls()
        );
    }

    #[test]
    fn selecting_alongside_after_a_central_location_also_leaves_the_central_venv_alone() {
        // The mirror direction of the test above -- moving back toward
        // Alongside must be exactly as inert as moving away from it. Without
        // this, a fix that only guarded the Alongside-to-Central direction
        // would still pass the other test above and still be able to
        // silently strand or delete a venv on the way back.
        let central = tempfile::tempdir().unwrap();
        let central_venv = central.path().join("api-000000");
        // Not a real `central_dir_name` hash -- irrelevant here, since this
        // test never resolves through it; it only has to be *some* existing
        // directory under the old central root that must survive.
        std::fs::create_dir_all(central_venv.join("lib")).unwrap();
        std::fs::write(
            central_venv.join("pyvenv.cfg"),
            "home = /usr/bin\nversion_info = 3.12.1\n",
        )
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(Config {
            projects: vec![Project {
                environments: vec![bombadil_core::model::Environment {
                    location: bombadil_core::model::VenvLocation::Default,
                    python: PythonPin::Unpinned,
                }],
                active: bombadil_core::model::VenvLocation::Default,
                pyproject_path: dir.path().join("pyproject.toml"),
                ..project("api")
            }],
            settings: Settings {
                default_venv_location: bombadil_core::model::DefaultVenvLocation::Central {
                    path: central.path().to_path_buf(),
                },
                ..Settings::default()
            },
            ..Config::default()
        });

        let _ = update(
            &mut app,
            Message::SettingsDefaultVenvLocationAlongsideSelected,
            unused_runner(),
        );

        assert_eq!(
            app.config.settings.default_venv_location,
            bombadil_core::model::DefaultVenvLocation::Alongside
        );
        assert!(
            central_venv.join("pyvenv.cfg").exists(),
            "the venv left behind under the old central location must survive"
        );
    }

    #[test]
    fn picking_a_default_venv_folder_dispatches_no_filesystem_or_process_work_synchronously() {
        // "Choose a central folder..." opens the async picker; the choice
        // itself does not run through job::run (there is nothing blocking
        // about assigning a path), but this pins that the message that
        // requests the picker touches neither the runner nor `Settings`
        // until a folder actually comes back.
        let mut app = App::new(Config::default());
        let fake = Arc::new(FakeCommandRunner::new());
        let before = app.config.settings.default_venv_location.clone();

        let task = update(
            &mut app,
            Message::SettingsDefaultVenvLocationPickFolderRequested,
            fake.clone(),
        );

        assert_eq!(app.config.settings.default_venv_location, before);
        assert!(fake.calls().is_empty());
        assert!(task.units() > 0, "must open the async folder picker");
    }

    // --- Open in PyCharm ---

    /// A runner that answers the PATH probe and lets the launcher spawn
    /// through. Detection's filesystem half is exercised in
    /// `bombadil_core::ide`'s own tests against a described filesystem; here
    /// the real `Path::exists` runs against a home directory that has no
    /// PyCharm in it, so it always falls through to this.
    fn pycharm_on_path() -> FakeCommandRunner {
        let runner = FakeCommandRunner::new();
        let found = bombadil_core::process::CommandOutcome {
            status: Some(0),
            stdout: "/opt/pycharm\n".into(),
            stderr: String::new(),
        };
        runner.expect("which", &["pycharm"], found.clone());
        runner.expect("where", &["pycharm"], found.clone());
        runner.expect("pycharm", &[], found);
        runner
    }

    #[test]
    fn pycharm_opens_on_the_environment_the_other_two_buttons_act_on() {
        // The bug this exists to prevent: a project with several environments
        // whose PyCharm button opens a *different* one from the terminal
        // button sitting next to it and the Sync button next to that. Both
        // read `active_environment`, so both have to be asked here, and the
        // one that is NOT default has to be the wrong answer.
        let mut project = project("api");
        let elsewhere = bombadil_core::model::VenvLocation::Custom {
            path: std::path::PathBuf::from("/envs/api-312"),
        };
        project
            .environments
            .push(bombadil_core::model::Environment {
                location: elsewhere.clone(),
                python: PythonPin::Unpinned,
            });
        project.active = elsewhere;

        let runner = pycharm_on_path();
        run_open_pycharm(
            &project,
            &Config::default(),
            std::path::Path::new("/nonexistent-home"),
            None,
            &bombadil_core::secrets::InMemorySecretStore::new(),
            &runner,
        )
        .expect("a programmed spawn must succeed");

        let calls = runner.calls();
        let launch = calls
            .iter()
            .find(|c| c.program == *std::path::Path::new("/opt/pycharm"))
            .expect("PyCharm must be launched");
        assert_eq!(
            launch.env.get("VIRTUAL_ENV").map(String::as_str),
            Some("/envs/api-312"),
            "PyCharm must get the project's default environment, not its first"
        );
    }

    #[test]
    fn a_pycharm_chosen_in_settings_is_launched_without_detecting() {
        let dir = tempfile::tempdir().unwrap();
        let chosen = dir.path().join("pycharm.sh");
        std::fs::write(&chosen, "").unwrap();
        let mut config = Config::default();
        config.settings.pycharm = Some(chosen.clone());
        // Only the launch is programmed: detection would find no PyCharm.
        let runner = FakeCommandRunner::new();
        // Matched by file stem: `pycharm.sh` is `pycharm`.
        runner.expect(
            "pycharm",
            &["/p/api"],
            bombadil_core::process::CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        run_open_pycharm(
            &project("api"),
            &config,
            std::path::Path::new("/nonexistent-home"),
            None,
            &bombadil_core::secrets::InMemorySecretStore::new(),
            &runner,
        )
        .expect("the chosen launcher opens");
        let calls = runner.calls();
        assert!(calls.iter().any(|c| c.program == chosen), "{calls:?}");
        assert!(
            !calls
                .iter()
                .any(|c| c.program == *std::path::Path::new("which")),
            "a choice is not second-guessed by detection"
        );
    }

    #[test]
    fn a_chosen_pycharm_that_is_gone_says_where_to_fix_it() {
        let mut config = Config::default();
        config.settings.pycharm = Some("/nonexistent/pycharm.sh".into());
        let err = run_open_pycharm(
            &project("api"),
            &config,
            std::path::Path::new("/nonexistent-home"),
            None,
            &bombadil_core::secrets::InMemorySecretStore::new(),
            &pycharm_on_path(),
        )
        .expect_err("a missing choice is not silently replaced");
        assert!(err.contains("Settings"), "{err}");
    }

    #[test]
    fn pycharm_opens_the_project_folder() {
        let runner = pycharm_on_path();
        let warning = run_open_pycharm(
            &project("api"),
            &Config::default(),
            std::path::Path::new("/nonexistent-home"),
            None,
            &bombadil_core::secrets::InMemorySecretStore::new(),
            &runner,
        )
        .unwrap();
        // No config directory: the IDE still opens, and the user is told the
        // interpreter is theirs to set.
        assert!(warning.is_some_and(|w| w.contains("by hand")));

        let calls = runner.calls();
        let launch = calls
            .iter()
            .find(|c| c.program == *std::path::Path::new("/opt/pycharm"))
            .expect("PyCharm must be launched");
        assert_eq!(launch.args, vec!["/p/api".to_string()]);
    }

    #[test]
    fn no_pycharm_says_where_it_looked() {
        // "PyCharm was not found" on its own leaves a user who has PyCharm
        // installed with nothing to check. The paths are the whole message.
        let runner = FakeCommandRunner::new();

        let err = run_open_pycharm(
            &project("api"),
            &Config::default(),
            std::path::Path::new("/nonexistent-home"),
            None,
            &bombadil_core::secrets::InMemorySecretStore::new(),
            &runner,
        )
        .expect_err("nothing is installed, so this cannot succeed");

        assert!(err.contains("not found"), "got {err}");
        assert!(
            err.contains("Toolbox"),
            "the message must name the places that were tried; got {err}"
        );
        assert!(
            runner
                .calls()
                .iter()
                .all(|c| !c.program.to_string_lossy().contains("/opt/pycharm")),
            "a failed detection must not spawn a guessed path"
        );
    }

    #[test]
    fn pressing_the_pycharm_button_dispatches_background_work() {
        // Launching an IDE from `update` would block the render thread on a
        // process spawn -- the same rule every other launch in this file
        // follows.
        let mut app = App::new(Config {
            projects: vec![project("api")],
            ..Config::default()
        });
        let fake = Arc::new(FakeCommandRunner::new());

        let task = update(&mut app, Message::OpenPycharmRequested(0), fake.clone());

        assert!(task.units() > 0, "the launch must be background work");
        assert!(
            fake.calls().is_empty(),
            "nothing may be spawned synchronously inside update"
        );
    }

    #[test]
    fn a_pycharm_button_press_for_a_project_that_is_gone_does_nothing() {
        let mut app = App::new(Config::default());
        let fake = Arc::new(FakeCommandRunner::new());

        let task = update(&mut app, Message::OpenPycharmRequested(7), fake.clone());

        assert_eq!(task.units(), 0);
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn a_failed_pycharm_open_is_shown_rather_than_swallowed() {
        // The likely failure is "not found", which names something the user
        // can act on -- but only if the drawer is open to show it.
        let mut app = App::new(Config::default());
        app.drawer.expanded = false;

        let _ = update(
            &mut app,
            Message::OpenPycharmFinished(Err("PyCharm was not found. Looked in: ...".into())),
            unused_runner(),
        );

        assert!(app.drawer.expanded, "the user has to be able to read it");
        assert!(
            app.drawer.status.contains("failed"),
            "{:?}",
            app.drawer.status
        );
        assert!(
            app.drawer.lines.iter().any(|l| l.contains("not found")),
            "the transcript must carry the reason; got {:?}",
            app.drawer.lines
        );
    }

    fn with_project() -> (App, uuid::Uuid) {
        let mut config = Config::default();
        config.projects.push(project("api"));
        let id = config.projects[0].id;
        (App::new(config), id)
    }

    fn looked(app: &mut App, id: uuid::Uuid, lookup: bombadil_core::github::Lookup) {
        let _ = update(
            app,
            Message::GitHubLooked(vec![(id, lookup)]),
            unused_runner(),
        );
    }

    #[test]
    fn the_first_github_result_is_stored_and_silent() {
        use bombadil_core::github::{CheckState::*, Lookup};
        let (mut app, id) = with_project();
        looked(
            &mut app,
            id,
            Lookup::Found(crate::github_tab::tests::found("a", &[Running])),
        );
        assert!(app.toasts.is_empty());
        assert!(app.github[&id].last_found.is_some());
    }

    #[test]
    fn checks_finishing_raise_a_toast_once() {
        use bombadil_core::github::{CheckState::*, Lookup};
        let (mut app, id) = with_project();
        looked(
            &mut app,
            id,
            Lookup::Found(crate::github_tab::tests::found("a", &[Running])),
        );
        looked(
            &mut app,
            id,
            Lookup::Found(crate::github_tab::tests::found("a", &[Failed])),
        );
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(app.toasts[0].kind, crate::busy::ToastKind::Warning);
        assert!(app.toasts[0].text.starts_with("Checks failed: api"));
        looked(
            &mut app,
            id,
            Lookup::Found(crate::github_tab::tests::found("a", &[Failed])),
        );
        assert_eq!(app.toasts.len(), 1, "no repeat");
    }

    #[test]
    fn a_failed_lookup_keeps_the_last_result_and_never_notifies() {
        use bombadil_core::github::{CheckState::*, Lookup};
        let (mut app, id) = with_project();
        looked(
            &mut app,
            id,
            Lookup::Found(crate::github_tab::tests::found("a", &[Running])),
        );
        looked(&mut app, id, Lookup::Failed("offline".into()));
        assert!(app.github[&id].last_found.is_some());
        looked(
            &mut app,
            id,
            Lookup::Found(crate::github_tab::tests::found("a", &[Running])),
        );
        assert!(app.toasts.is_empty());
    }

    #[test]
    fn leaving_the_pr_forgets_it() {
        use bombadil_core::github::{CheckState::*, Lookup};
        let (mut app, id) = with_project();
        looked(
            &mut app,
            id,
            Lookup::Found(crate::github_tab::tests::found("a", &[Running])),
        );
        looked(&mut app, id, Lookup::DefaultBranch("main".into()));
        assert!(app.github[&id].last_found.is_none());
    }

    #[test]
    fn a_result_for_a_removed_project_is_dropped() {
        let (mut app, _) = with_project();
        let stranger = uuid::Uuid::new_v4();
        looked(&mut app, stranger, bombadil_core::github::Lookup::NotGit);
        assert!(!app.github.contains_key(&stranger));
    }

    #[test]
    fn refresh_marks_only_that_project_in_flight_and_a_result_clears_it() {
        let mut config = Config::default();
        config.projects.push(project("api"));
        config.projects.push(project("web"));
        let (a, b) = (config.projects[0].id, config.projects[1].id);
        let mut app = App::new(config);
        let _ = update(
            &mut app,
            Message::GitHubRefreshRequested(a),
            unused_runner(),
        );
        assert!(app.github_in_flight.contains(&a));
        assert!(!app.github_in_flight.contains(&b));
        // A second press while one is running starts nothing new.
        let _ = update(
            &mut app,
            Message::GitHubRefreshRequested(a),
            unused_runner(),
        );
        assert_eq!(app.github_in_flight.len(), 1);
        looked(&mut app, a, bombadil_core::github::Lookup::NotGit);
        assert!(app.github_in_flight.is_empty());
    }

    #[test]
    fn a_tick_looks_up_every_project_unless_watching_is_off_or_rate_limited() {
        let mut config = Config::default();
        config.projects.push(project("api"));
        config.projects.push(project("web"));
        let (a, b) = (config.projects[0].id, config.projects[1].id);
        let mut app = App::new(config);
        looked(
            &mut app,
            b,
            bombadil_core::github::Lookup::RateLimited {
                until: bombadil_core::github::now_unix() + 600,
            },
        );
        let _ = update(&mut app, Message::GitHubTick, unused_runner());
        assert!(app.github_in_flight.contains(&a));
        assert!(!app.github_in_flight.contains(&b));

        let (mut app, id) = with_project();
        app.config.settings.watch_github = false;
        let _ = update(&mut app, Message::GitHubTick, unused_runner());
        assert!(!app.github_in_flight.contains(&id));
    }

    #[test]
    fn the_github_tab_has_a_stable_key_and_comes_before_the_doctor_by_default() {
        assert_eq!(Tab::GitHub.key(), "github");
        let order = tab_order(&[]);
        let at = |t: Tab| order.iter().position(|x| *x == t).unwrap();
        assert_eq!(at(Tab::GitHub) + 1, at(Tab::Doctor));
    }

    #[test]
    fn turning_watching_off_is_saved() {
        let mut app = App::new(Config::default());
        let _ = update(
            &mut app,
            Message::GitHubWatchSelected(false),
            unused_runner(),
        );
        assert!(!app.config.settings.watch_github);
    }

    #[test]
    fn turning_watching_on_looks_up_at_once() {
        let (mut app, id) = with_project();
        app.config.settings.watch_github = false;
        let _ = update(
            &mut app,
            Message::GitHubWatchSelected(true),
            unused_runner(),
        );
        assert!(app.github_in_flight.contains(&id));
    }

    #[test]
    fn removing_a_project_forgets_its_github_state() {
        let (mut app, id) = with_project();
        looked(&mut app, id, bombadil_core::github::Lookup::NotGit);
        app.github_in_flight.insert(id);
        let _ = update(
            &mut app,
            Message::RemoveProjectRequested(0),
            unused_runner(),
        );
        assert!(!app.github.contains_key(&id));
        assert!(!app.github_in_flight.contains(&id));
    }

    #[test]
    fn a_refresh_while_watching_is_off_never_notifies() {
        use bombadil_core::github::{CheckState::*, Lookup};
        let (mut app, id) = with_project();
        app.config.settings.watch_github = false;
        let running = crate::github_tab::tests::found("a", &[Running]);
        looked(&mut app, id, Lookup::Found(running));
        looked(
            &mut app,
            id,
            Lookup::Found(crate::github_tab::tests::found("a", &[Failed])),
        );
        assert!(app.toasts.is_empty());
        assert!(app.github[&id].last_found.is_some(), "still shown");
    }

    #[test]
    fn projects_on_one_pull_request_notify_once() {
        use bombadil_core::github::{CheckState::*, Lookup};
        let found = crate::github_tab::tests::found;
        let mut config = Config::default();
        for label in ["api", "web", "cli"] {
            config.projects.push(project(label));
        }
        let ids: Vec<_> = config.projects.iter().map(|p| p.id).collect();
        let mut app = App::new(config);
        let _ = update(
            &mut app,
            Message::GitHubLooked(
                ids.iter()
                    .map(|id| (*id, Lookup::Found(found("a", &[Running]))))
                    .collect(),
            ),
            unused_runner(),
        );
        // Two in one batch, the third on its own later.
        let _ = update(
            &mut app,
            Message::GitHubLooked(
                ids[..2]
                    .iter()
                    .map(|id| (*id, Lookup::Found(found("a", &[Failed]))))
                    .collect(),
            ),
            unused_runner(),
        );
        looked(&mut app, ids[2], Lookup::Found(found("a", &[Failed])));
        assert_eq!(app.toasts.len(), 1, "{:?}", app.toasts);
        // A re-run of the same commit is news again.
        looked(&mut app, ids[0], Lookup::Found(found("a", &[Running])));
        assert_eq!(app.toasts.len(), 2);
    }

    #[test]
    fn a_failed_token_save_keeps_the_draft() {
        let mut app = App::new(Config::default());
        app.github_token_draft = "ghp_x".into();
        let _ = update(&mut app, Message::GitHubTokenSaved, unused_runner());
        let _ = update(
            &mut app,
            Message::GitHubTokenStored(Err("locked".into())),
            unused_runner(),
        );
        assert_eq!(app.github_token_draft, "ghp_x");
        let _ = update(
            &mut app,
            Message::GitHubTokenStored(Ok("Saved the GitHub token".into())),
            unused_runner(),
        );
        assert!(app.github_token_draft.is_empty());
    }

    #[test]
    fn one_hung_github_lookup_does_not_hold_up_the_others() {
        use bombadil_core::github::Lookup;
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let projects = vec![(a, PathBuf::from("/hung")), (b, PathBuf::from("/fine"))];
        let started = std::time::Instant::now();
        let got = futures::executor::block_on(look_up_each(
            projects,
            Duration::from_millis(100),
            |dir: PathBuf| {
                if dir.ends_with("hung") {
                    std::thread::sleep(Duration::from_secs(5));
                }
                Lookup::NotGit
            },
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(matches!(got[0], (id, Lookup::Failed(_)) if id == a));
        assert_eq!(got[1], (b, Lookup::NotGit));
    }

    #[test]
    fn choosing_a_github_account_saves_it_and_looks_up_again() {
        let (mut app, id) = with_project();
        app.github_source = Some("Using gh as work".into());
        let _ = update(
            &mut app,
            Message::GitHubAccountSelected("me".into()),
            unused_runner(),
        );
        assert_eq!(app.config.settings.github_account.as_deref(), Some("me"));
        assert_eq!(app.github_source, None, "rechecked");
        assert!(app.github_in_flight.contains(&id));
    }

    #[test]
    fn jargon_tabs_explain_their_term() {
        use crate::glossary::Term;
        assert_eq!(Tab::Dependencies.term(), Some(Term::Dependency));
        assert_eq!(Tab::Members.term(), Some(Term::Member));
        assert_eq!(Tab::EnvVars.term(), Some(Term::EnvVars));
        assert_eq!(Tab::Indexes.term(), Some(Term::Index));
        assert_eq!(Tab::Scripts.term(), None);
        assert_eq!(Tab::GitHub.term(), None);
    }

    #[test]
    fn adding_a_github_account_shows_its_code_then_selects_it() {
        use crate::github_tab::GitHubLogin;
        let mut app = App::new(Config::default());
        let _ = update(&mut app, Message::GitHubAddAccount, unused_runner());
        assert_eq!(app.github_login, Some(GitHubLogin::Waiting));
        let _ = update(
            &mut app,
            Message::GitHubLoginCode("AB12-CD34".into()),
            unused_runner(),
        );
        assert_eq!(
            app.github_login,
            Some(GitHubLogin::Code("AB12-CD34".into()))
        );
        let _ = update(
            &mut app,
            Message::GitHubLoginFinished(Ok("new".into())),
            unused_runner(),
        );
        assert_eq!(app.github_login, None);
        assert_eq!(app.config.settings.github_account.as_deref(), Some("new"));
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(app.toasts[0].kind, crate::busy::ToastKind::Success);
        assert!(app.toasts[0].text.contains("new"), "{}", app.toasts[0].text);
    }

    #[test]
    fn a_failed_login_warns_and_a_cancelled_one_stays_quiet() {
        let mut app = App::new(Config::default());
        let _ = update(&mut app, Message::GitHubAddAccount, unused_runner());
        let _ = update(
            &mut app,
            Message::GitHubLoginFinished(Err("expired_token".into())),
            unused_runner(),
        );
        assert_eq!(app.github_login, None);
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(app.toasts[0].kind, crate::busy::ToastKind::Warning);
        assert!(app.toasts[0].text.contains("expired_token"));

        let mut app = App::new(Config::default());
        let _ = update(&mut app, Message::GitHubAddAccount, unused_runner());
        let flag = app.github_login_cancel.clone().expect("a running login");
        let _ = update(&mut app, Message::GitHubLoginCancelled, unused_runner());
        assert_eq!(app.github_login, None);
        assert!(
            flag.load(std::sync::atomic::Ordering::SeqCst),
            "gh is stopped"
        );
        let _ = update(
            &mut app,
            Message::GitHubLoginFinished(Err("expired_token".into())),
            unused_runner(),
        );
        assert!(app.toasts.is_empty());
        assert_eq!(app.config.settings.github_account, None);
    }

    #[test]
    fn signing_in_with_the_browser_shows_its_code_then_uses_the_token() {
        use crate::github_tab::GitHubLogin;
        let mut app = App::new(Config::default());
        app.github_source = Some("Not signed in".into());
        let _ = update(&mut app, Message::GitHubBrowserSignIn, unused_runner());
        assert_eq!(app.github_login, Some(GitHubLogin::Waiting));
        let _ = update(&mut app, Message::GitHubAddAccount, unused_runner());
        assert!(app.github_login_cancel.is_some(), "one sign-in at a time");
        let _ = update(
            &mut app,
            Message::GitHubLoginCode("AB12-CD34".into()),
            unused_runner(),
        );
        assert_eq!(
            app.github_login,
            Some(GitHubLogin::Code("AB12-CD34".into()))
        );
        let _ = update(
            &mut app,
            Message::GitHubBrowserFinished(Ok("Signed in to GitHub".into())),
            unused_runner(),
        );
        assert_eq!(app.github_login, None);
        assert!(app.github_login_cancel.is_none());
        assert_eq!(app.github_source, None, "rechecked");
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(app.toasts[0].kind, crate::busy::ToastKind::Success);
    }

    #[test]
    fn a_failed_browser_sign_in_warns_and_a_cancelled_one_stays_quiet() {
        let mut app = App::new(Config::default());
        let _ = update(&mut app, Message::GitHubBrowserSignIn, unused_runner());
        let _ = update(
            &mut app,
            Message::GitHubBrowserFinished(Err("the sign-in was declined".into())),
            unused_runner(),
        );
        assert_eq!(app.github_login, None);
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(app.toasts[0].kind, crate::busy::ToastKind::Warning);
        assert!(app.toasts[0].text.contains("declined"));

        let mut app = App::new(Config::default());
        let _ = update(&mut app, Message::GitHubBrowserSignIn, unused_runner());
        let flag = app.github_login_cancel.clone().expect("a running sign-in");
        let _ = update(&mut app, Message::GitHubLoginCancelled, unused_runner());
        assert!(flag.load(std::sync::atomic::Ordering::SeqCst));
        let _ = update(
            &mut app,
            Message::GitHubBrowserFinished(Err("cancelled".into())),
            unused_runner(),
        );
        assert!(app.toasts.is_empty());
    }

    #[test]
    fn the_github_accounts_listed_are_kept_for_preferences() {
        let mut app = App::new(Config::default());
        let me = bombadil_core::github::gh::Account {
            login: "me".into(),
            active: true,
        };
        let _ = update(
            &mut app,
            Message::GitHubAccountsListed(vec![me.clone()]),
            unused_runner(),
        );
        assert_eq!(app.github_accounts, Some(vec![me]));
    }

    fn project(label: &str) -> bombadil_core::model::Project {
        bombadil_core::model::Project {
            id: uuid::Uuid::new_v4(),
            label: label.into(),
            pyproject_path: std::path::PathBuf::from(format!("/p/{label}/pyproject.toml")),
            environments: vec![bombadil_core::model::Environment {
                location: bombadil_core::model::VenvLocation::Alongside,
                python: PythonPin::Unpinned,
            }],
            active: bombadil_core::model::VenvLocation::Alongside,
            ..bombadil_core::model::Project::default()
        }
    }

    fn fail(app: &mut App, transcript: &str) {
        let id = app.config.projects[0].id;
        fail_with(app, id, crate::busy::Action::Sync, transcript);
    }

    fn fail_with(app: &mut App, id: uuid::Uuid, action: crate::busy::Action, transcript: &str) {
        app.busy = Some(crate::busy::Busy::new(id, action));
        drawer::start(&mut app.drawer, "syncing...".into());
        let _ = update(
            app,
            Message::SyncFinished {
                success: false,
                lines: transcript.lines().map(String::from).collect(),
            },
            unused_runner(),
        );
    }

    #[test]
    fn a_failure_is_diagnosed_in_plain_words() {
        let mut app = one_project();
        fail(
            &mut app,
            include_str!("../../bombadil-core/src/doctor/fixtures/no_version.txt"),
        );
        let id = app.config.projects[0].id;
        let (action, d) = app.last_failure.get(&id).expect("a diagnosis");
        assert_eq!(*action, crate::busy::Action::Sync);
        assert_eq!(d.sentence, "six has no version matching ==9.9");
        assert_eq!(app.toasts[0].text, "six has no version matching ==9.9");
        assert_eq!(
            app.toasts[0].fix,
            Some((
                id,
                bombadil_core::doctor::DoctorFix::LetUvPick("six".into())
            ))
        );
    }

    #[test]
    fn success_clears_the_diagnosis() {
        let mut app = one_project();
        fail(&mut app, "error: boom\n");
        let id = app.config.projects[0].id;
        app.busy = Some(crate::busy::Busy::new(id, crate::busy::Action::Sync));
        let _ = update(
            &mut app,
            Message::SyncFinished {
                success: true,
                lines: vec![],
            },
            unused_runner(),
        );
        assert!(app.last_failure.is_empty());
    }

    #[test]
    fn diagnoses_are_kept_per_project() {
        let mut app = App::new(Config {
            projects: vec![project("a"), project("b")],
            ..Config::default()
        });
        fail(&mut app, "error: first\n");
        let b = app.config.projects[1].id;
        fail_with(&mut app, b, crate::busy::Action::Sync, "error: second\n");
        assert_eq!(app.last_failure.len(), 2);
        let a = app.config.projects[0].id;
        assert_eq!(app.last_failure[&a].1.sentence, "uv couldn't finish: first");
        assert_eq!(
            app.last_failure[&b].1.sentence,
            "uv couldn't finish: second"
        );
    }

    #[test]
    fn a_failed_outdated_lookup_is_kept_as_its_reason() {
        let mut app = one_project();
        app.selected = Some(0);
        let _ = update(
            &mut app,
            Message::OutdatedLoaded(0, Err("offline".into())),
            unused_runner(),
        );
        assert_eq!(app.outdated, Some((0, Err("offline".into()))));
    }

    #[test]
    fn each_fix_maps_to_an_existing_action() {
        use bombadil_core::doctor::DoctorFix;
        let mut app = one_project();
        app.selected = Some(0);
        let id = first_id(&app);
        let _ = update(
            &mut app,
            Message::DoctorFixRequested(id, DoctorFix::OpenIndexes),
            unused_runner(),
        );
        assert_eq!(app.tab, Tab::Indexes);
        let _ = update(
            &mut app,
            Message::DoctorFixRequested(id, DoctorFix::ShowOutput),
            unused_runner(),
        );
        assert!(app.drawer.expanded);
        assert_eq!(
            fix_label(&DoctorFix::RequirePython(">=3.11".into())),
            "Require Python >=3.11"
        );
        assert_eq!(
            fix_label(&DoctorFix::LetUvPick("numpy".into())),
            "Let uv pick a numpy that fits"
        );
    }

    #[test]
    fn retry_runs_the_failed_sync_again() {
        use bombadil_core::doctor::DoctorFix;
        let mut app = one_project();
        fail(
            &mut app,
            include_str!("../../bombadil-core/src/doctor/fixtures/network.txt"),
        );
        let id = first_id(&app);
        let _ = update(
            &mut app,
            Message::DoctorFixRequested(id, DoctorFix::Retry),
            unused_runner(),
        );
        assert_eq!(
            app.busy.as_ref().map(|b| b.action.clone()),
            Some(crate::busy::Action::Sync)
        );
    }

    #[test]
    fn retry_reruns_an_upgrade() {
        use bombadil_core::doctor::DoctorFix;
        let mut app = one_project();
        let id = app.config.projects[0].id;
        fail_with(
            &mut app,
            id,
            crate::busy::Action::Upgrade(Some("six".into())),
            "error: boom\n",
        );
        let id = first_id(&app);
        let _ = update(
            &mut app,
            Message::DoctorFixRequested(id, DoctorFix::Retry),
            unused_runner(),
        );
        assert_eq!(
            app.busy.as_ref().map(|b| b.action.clone()),
            Some(crate::busy::Action::Upgrade(Some("six".into())))
        );
    }

    #[test]
    fn the_doctor_tab_has_a_stable_key_and_a_place() {
        assert_eq!(Tab::Doctor.key(), "doctor");
        assert_eq!(Tab::Doctor.label(), "Doctor");
        assert!(tab_order(&[]).contains(&Tab::Doctor));
        assert_eq!(
            Tab::ALL[Tab::ALL.len() - 2],
            Tab::Doctor,
            "then Run, the newest"
        );
    }

    #[test]
    fn update_pin_installs_the_latest() {
        let mut app = one_project();
        let _ = update(
            &mut app,
            Message::DoctorPinRequested(0, "six==1.17.0".into()),
            unused_runner(),
        );
        assert_eq!(
            app.busy.as_ref().map(|b| b.action.clone()),
            Some(crate::busy::Action::InstallVersion("six==1.17.0".into()))
        );
    }

    #[test]
    fn only_a_declared_package_is_replaced() {
        let mut app = one_project();
        let row = |name: &str, constraint: Option<&str>, declared| deps::DepRow {
            name: name.into(),
            constraint: constraint.map(Into::into),
            version: None,
            declared,
        };
        app.deps = Some((
            0,
            Ok(deps::DepsTab {
                groups: vec![deps::DepGroup {
                    kind: deps::GroupKind::Group("dev".into()),
                    rows: vec![row("Six", Some("==9.9"), true), row("certifi", None, false)],
                }],
                graph: None,
                tree_note: None,
            }),
        ));
        assert_eq!(
            declared_group(&app, 0, "six"),
            Some(deps::GroupKind::Group("dev".into()))
        );
        assert_eq!(
            declared_group(&app, 0, "six==1.17.0"),
            Some(deps::GroupKind::Group("dev".into()))
        );
        assert_eq!(
            declared_group(&app, 0, "certifi"),
            None,
            "installed, not declared"
        );
        assert_eq!(
            declared_group(&app, 1, "six"),
            None,
            "another project's tab"
        );
    }

    #[test]
    fn pressing_a_toasts_fix_dismisses_that_toast() {
        use bombadil_core::doctor::DoctorFix;
        let mut app = one_project();
        fail(
            &mut app,
            include_str!("../../bombadil-core/src/doctor/fixtures/network.txt"),
        );
        assert_eq!(app.toasts[0].fix, Some((first_id(&app), DoctorFix::Retry)));
        let id = first_id(&app);
        let _ = update(
            &mut app,
            Message::DoctorFixRequested(id, DoctorFix::Retry),
            unused_runner(),
        );
        assert!(app.toasts.iter().all(|t| t.fix.is_none()));
    }

    #[test]
    fn a_fix_while_busy_starts_nothing_and_keeps_its_toast() {
        use bombadil_core::doctor::DoctorFix;
        let mut app = one_project();
        let id = first_id(&app);
        fail(
            &mut app,
            include_str!("../../bombadil-core/src/doctor/fixtures/python.txt"),
        );
        app.busy = Some(crate::busy::Busy::new(id, crate::busy::Action::Sync));
        for fix in [
            DoctorFix::Retry,
            DoctorFix::LetUvPick("numpy".into()),
            DoctorFix::RequirePython(">=3.11".into()),
        ] {
            let task = update(
                &mut app,
                Message::DoctorFixRequested(id, fix),
                unused_runner(),
            );
            assert_eq!(task.units(), 0);
        }
        assert_eq!(
            app.busy.as_ref().map(|b| b.action.clone()),
            Some(crate::busy::Action::Sync)
        );
        assert!(app.toasts[0].fix.is_some(), "the toast stays for later");
    }

    #[test]
    fn a_toasts_fix_follows_its_project_when_the_list_changes() {
        let mut app = two_projects();
        let b = app.config.projects[1].id;
        fail_with(
            &mut app,
            b,
            crate::busy::Action::Sync,
            include_str!("../../bombadil-core/src/doctor/fixtures/network.txt"),
        );
        let (project, fix) = app.toasts[0].fix.clone().expect("a fix");
        app.config.projects.remove(0);
        app.entries.remove(0);
        let _ = update(
            &mut app,
            Message::DoctorFixRequested(project, fix),
            unused_runner(),
        );
        assert_eq!(app.busy.as_ref().map(|x| x.project_id), Some(b));
    }

    #[test]
    fn letting_uv_pick_after_a_failed_edit_adds_the_bare_name() {
        use bombadil_core::doctor::DoctorFix;
        let mut app = one_project();
        let id = first_id(&app);
        fail_with(
            &mut app,
            id,
            crate::busy::Action::InstallVersion("six==9.9".into()),
            include_str!("../../bombadil-core/src/doctor/fixtures/no_version.txt"),
        );
        let _ = update(
            &mut app,
            Message::DoctorFixRequested(id, DoctorFix::LetUvPick("six".into())),
            unused_runner(),
        );
        assert_eq!(
            app.busy.as_ref().map(|b| b.action.clone()),
            Some(crate::busy::Action::InstallVersion("six".into()))
        );
    }

    #[test]
    fn letting_uv_pick_after_a_failed_sync_edits_pyproject_first() {
        use bombadil_core::doctor::DoctorFix;
        let mut app = one_project();
        let id = first_id(&app);
        fail(
            &mut app,
            include_str!("../../bombadil-core/src/doctor/fixtures/no_version.txt"),
        );
        let task = update(
            &mut app,
            Message::DoctorFixRequested(id, DoctorFix::LetUvPick("six".into())),
            unused_runner(),
        );
        assert!(
            app.busy.is_none(),
            "no uv command until pyproject.toml is edited"
        );
        assert!(task.units() > 0);
    }

    #[test]
    fn a_stale_outdated_result_is_ignored() {
        let mut app = two_projects();
        app.selected = Some(1);
        let _ = update(
            &mut app,
            Message::OutdatedLoaded(0, Ok(vec![])),
            unused_runner(),
        );
        assert_eq!(app.outdated, None);
    }

    #[test]
    fn a_failed_local_add_offers_no_retry() {
        let mut app = one_project();
        let id = first_id(&app);
        fail_with(
            &mut app,
            id,
            crate::busy::Action::AddLocal("lib".into()),
            include_str!("../../bombadil-core/src/doctor/fixtures/network.txt"),
        );
        assert!(app.last_failure[&id].1.fixes.is_empty());
        assert_eq!(app.toasts[0].fix, None);
    }
}
