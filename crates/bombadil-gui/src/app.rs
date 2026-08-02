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
use crate::scripts_editor;
use crate::settings_editor;
use crate::sidebar;
use bombadil_core::envspec::{self, EnvInputs};
use bombadil_core::model::{
    Config, DefaultVenvLocation, Project, Settings, TerminalChoice, UvSource,
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
    /// The pending Recreate venv confirmation (spec §8), if any. Deleting an
    /// environment cannot be undone, so the press that starts the action only
    /// fills this in -- `RecreateVenvConfirmed` is the only message that
    /// reaches `venv_path::delete`.
    pub confirm_recreate: Option<context_menu::ConfirmRecreate>,
    /// Whether the footer is showing the bundled uv licence in full.
    /// Apache-2.0 section 4 obliges the text to be reachable, and
    /// `bombadil_uv_bin::UV_LICENSE` is compiled in -- this is what reaches
    /// it. Collapsed by default: it is thousands of words of boilerplate,
    /// and the obligation is that the user *can* read it.
    pub about_expanded: bool,
    /// The Dependencies tab (spec §8) for whichever project it was last
    /// fetched for, paired with that project's index so a result that
    /// arrives after the selection has moved on is never shown against the
    /// wrong project -- the same staleness guard `entries`/`VenvProbed` uses.
    /// `None` before the first fetch lands.
    pub deps: Option<(usize, deps::DepsTab)>,
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
    Members,
    EnvVars,
    Scripts,
    Indexes,
    Settings,
}

impl Tab {
    /// Every tab, in the order the selector renders them.
    pub const ALL: [Tab; 6] = [
        Tab::Dependencies,
        Tab::Members,
        Tab::EnvVars,
        Tab::Scripts,
        Tab::Indexes,
        Tab::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Dependencies => "Dependencies",
            Tab::Members => "Members",
            Tab::EnvVars => "Env vars",
            Tab::Scripts => "Scripts",
            Tab::Indexes => "Indexes",
            Tab::Settings => "Settings",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    ProjectSelected(usize),
    /// A tab in the detail area's selector was pressed (spec §8).
    TabSelected(Tab),
    /// A background venv probe finished for `projects[usize]`. Carries the
    /// Python version on success; `None` covers "no venv", a probe that
    /// timed out, and a probe that failed -- all three degrade to hollow.
    VenvProbed(usize, Option<String>),
    /// The background fetch for the Dependencies tab finished for
    /// `projects[usize]` -- criterion 5: dispatched through `job::run`,
    /// never run inline, because `uv tree` on a large project is slow.
    /// Carries the index for the same staleness reason `VenvProbed` does:
    /// the user can select a different project before this lands.
    DepsLoaded(usize, DepsFetch),
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
    /// The background open finished. `Ok(())` on a successful spawn; `Err`
    /// carries text `terminal::open::open` has already redacted of any
    /// secret the composed environment resolved.
    OpenTerminalFinished(Result<(), String>),
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
    /// The background persist for a removal finished.
    RemoveProjectFinished {
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
    /// "Open folder" was picked for `projects[usize]` (spec §8): show the
    /// project's directory in the platform's file manager. Non-destructive,
    /// and dispatched as background work like every other process spawn here.
    OpenFolderRequested(usize),
    /// The background open finished. `Err` carries the spawn failure --
    /// nothing composed reaches this path, so there is nothing to redact.
    OpenFolderFinished(Result<(), String>),
    /// The footer's licence control was pressed: show or hide the bundled uv
    /// licence text. Apache-2.0 section 4 obliges the text to be reachable,
    /// and this is what reaches it.
    AboutLicenseToggled,
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
    /// The background half of an env var edit finished: a plain edit's
    /// persist, or a secret toggle's keychain write/delete plus persist.
    EnvVarPersistFinished {
        success: bool,
    },
    /// The Scripts tab's "Add script" button was pressed for `scope` (spec
    /// §6, Task 3): open the async file picker for the script to run.
    /// Dispatches `pick_script` through `Task::perform` rather than adding a
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
    /// The background half of a script edit finished: persisting the config.
    ScriptPersistFinished {
        success: bool,
    },
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
    /// The background half of an index edit finished: a plain field edit's
    /// persist, a credential save's keychain write plus persist, or a
    /// project's selection toggle's persist.
    IndexPersistFinished {
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
    /// The background half of a member selection edit: persisting the config.
    MembersPersistFinished {
        success: bool,
    },
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
    /// The background half of a Settings persist (a `uv_source` choice, or a
    /// validated custom binary) finished.
    SettingsPersistFinished {
        success: bool,
    },
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
            config,
            selected,
            entries,
            drawer: drawer::Drawer::default(),
            load_problems: Vec::new(),
            syncing: None,
            add_project: None,
            persist: Arc::new(NullPersist),
            context_menu: None,
            confirm_recreate: None,
            about_expanded: false,
            deps: None,
            index_credential_drafts,
            index_credential_names,
            env_value_drafts: env_editor::ValueDrafts::new(),
            members: None,
            confirm_member_sync: None,
            uv_custom_path_draft,
            uv_custom_validation: None,
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
    let mut probes = vec![probe_every_venv(&app)];
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
            .filter_map(|(i, project)| {
                let path = venv_path::resolve(project, &app.config.settings).ok()?;
                Some(Task::perform(probe_venv(path), move |version| {
                    Message::VenvProbed(i, version)
                }))
            })
            .collect::<Vec<_>>(),
    )
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

/// What background loading produced for the Dependencies tab (spec §8):
/// the parsed manifest, what `uv pip list` reports installed, and (when it
/// could be read) the resolved tree. `deps::build` -- pure, tested on its
/// own -- turns this into the tab; nothing here does that joining itself.
#[derive(Debug, Clone)]
pub struct DepsFetch {
    pyproject: PyProject,
    installed: Vec<InstalledPackage>,
    /// `None` covers both "no venv to ask yet" and "the command failed to
    /// run" -- neither is uv answering with a schema this build does not
    /// understand, which is the one case `deps::build` explains to the user.
    /// See its own doc for why the two must stay distinguishable.
    tree: Option<TreeOutcome>,
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
    let venv_path = venv_path::resolve(project, &config.settings).ok()?;
    Some(uv::command::ProjectContext {
        project_dir: project_dir.to_path_buf(),
        venv_path,
    })
}

/// The blocking half of a Dependencies fetch: read the manifest, then (if a
/// uv can be resolved and the project has a venv to ask about) run `uv pip
/// list` and `uv tree`. Every failure along the way -- an unreadable
/// manifest, no resolvable uv, a command that exits non-zero, output this
/// build cannot parse -- degrades to an empty/absent result rather than
/// erroring: a project that has never been synced has no installed packages
/// and no tree yet, which is a normal state (the same "registered without a
/// venv" state `sidebar` already treats as ordinary), not a failure to
/// report.
fn fetch_deps_blocking(
    project: &Project,
    config: &Config,
    runner: &dyn CommandRunner,
) -> DepsFetch {
    let pyproject = bombadil_core::pyproject::load(&project.pyproject_path).unwrap_or_default();

    let Some(resolved_uv) = resolve_uv_for(config, runner).ok() else {
        return DepsFetch {
            pyproject,
            installed: Vec::new(),
            tree: None,
        };
    };
    let Some(ctx) = deps_command_context(project, config) else {
        return DepsFetch {
            pyproject,
            installed: Vec::new(),
            tree: None,
        };
    };

    let installed = runner
        .run(&uv::command::pip_list(&resolved_uv, &ctx))
        .ok()
        .filter(|outcome| outcome.success())
        .and_then(|outcome| uv::results::parse_pip_list(&outcome.stdout).ok())
        .unwrap_or_default();

    let tree = runner
        .run(&uv::command::tree(&resolved_uv, &ctx, DEPS_TREE_DEPTH))
        .ok()
        .filter(|outcome| outcome.success())
        .and_then(|outcome| uv::results::parse_tree(&outcome.stdout).ok());

    DepsFetch {
        pyproject,
        installed,
        tree,
    }
}

/// Runs a Dependencies fetch off the render thread via `job::run` --
/// criterion 5: `uv tree` on a large project is slow, and `CommandRunner::run`
/// blocks until the child exits, so calling `fetch_deps_blocking` from
/// `update` would freeze the window exactly like `sync_project` would.
/// `SYNC_TIMEOUT`, not `VENV_PROBE_TIMEOUT`, for the same reason `sync_project`
/// uses it: this can run a full uv resolve, not a sub-millisecond file read.
async fn load_deps(project: Project, config: Config, runner: Arc<dyn CommandRunner>) -> DepsFetch {
    match job::run(
        move || fetch_deps_blocking(&project, &config, runner.as_ref()),
        SYNC_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(fetch) => fetch,
        job::JobResult::TimedOut | job::JobResult::Failed => DepsFetch {
            pyproject: PyProject::default(),
            installed: Vec::new(),
            tree: None,
        },
    }
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
    Task::perform(load_deps(project, config, runner), move |fetch| {
        Message::DepsLoaded(index, fetch)
    })
}

/// The blocking half of a Members fetch (spec §5a, Task 5): classify the
/// project's own manifest, walk its tree for other `pyproject.toml` files,
/// then (if a uv can be resolved and the project has a venv to ask about)
/// run `uv pip list`. Mirrors `fetch_deps_blocking`'s degrade-don't-error
/// discipline: an unreadable manifest, no resolvable uv, a project with no
/// venv yet -- all leave `discovered`/`installed` empty rather than failing,
/// since a project that has never been synced or has no siblings at all is a
/// normal state for this tab, not an error to report.
fn fetch_members_blocking(
    project: &Project,
    config: &Config,
    runner: &dyn CommandRunner,
) -> MembersFetch {
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
            return Task::batch([
                fetch_deps_task(
                    i,
                    app.config.projects[i].clone(),
                    app.config.clone(),
                    runner.clone(),
                ),
                fetch_members_task(
                    i,
                    app.config.projects[i].clone(),
                    app.config.clone(),
                    runner,
                ),
            ]);
        }
        Message::ProjectSelected(_) => {}
        Message::TabSelected(tab) => {
            app.tab = tab;
        }
        Message::VenvProbed(i, version) if i < app.entries.len() => {
            app.entries[i].venv_exists = version.is_some();
            app.entries[i].python_version = version;
        }
        // A stale probe result for a project that no longer exists.
        Message::VenvProbed(_, _) => {}
        Message::DepsLoaded(i, fetch) if i < app.config.projects.len() => {
            let tab = deps::build(&fetch.pyproject, &fetch.installed, fetch.tree);
            app.deps = Some((i, tab));
        }
        // A stale fetch for a project that no longer exists, or that the
        // user has since moved away from -- `view` only ever reads
        // `app.deps` when its index still matches the current selection.
        Message::DepsLoaded(_, _) => {}
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
        Message::SyncRequested(i) => {
            app.context_menu = None;
            if app.syncing.is_none()
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
            if let Some(draft) = app.add_project.take() {
                let project = add_project::to_project(&draft);
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
                    label: project.label.clone(),
                    venv_exists: false,
                    python_version: None,
                });
                let index = app.config.projects.len() - 1;
                app.selected = Some(index);
                // This path runs `uv sync -p <version>` too, so it claims the
                // same single in-flight slot `SyncRequested` does. Without
                // it, confirming the dialog -- which also selects the new
                // project, and the detail header always renders Sync --
                // leaves the user one press away from two concurrent
                // `uv sync` on the same venv, interleaving into the one
                // shared drawer. See `App::syncing`'s own doc.
                app.syncing = Some(project.id.to_string());

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
            // Releases the slot `AddProjectConfirmed` claimed. Never
            // clearing it would make the guard a deadlock: Sync would work
            // exactly zero times after adding a project.
            app.syncing = None;
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
            if let Some(project) = app.config.projects.get(index).cloned()
                && let Ok(path) = venv_path::resolve(&project, &app.config.settings)
            {
                return Task::perform(probe_venv(path), move |version| {
                    Message::VenvProbed(index, version)
                });
            }
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
        Message::RemoveProjectRequested(i) if i < app.config.projects.len() => {
            // In-memory first, synchronously -- the same discipline
            // `AddProjectConfirmed` uses, so the sidebar reflects the removal
            // the instant `update` returns rather than only once the
            // background persist lands.
            app.config.projects.remove(i);
            app.entries.remove(i);
            // Indices shift on removal; `deps` is tagged with one, and a
            // shifted-but-uncleared tag would show a stale tab against
            // whichever project now sits at that index rather than the one
            // it was actually fetched for. `ProjectSelected` refetches it
            // the next time a selection lands.
            app.deps = None;
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
            return Task::perform(persist_config(config, persist), |success| {
                Message::RemoveProjectFinished { success }
            });
        }
        Message::RemoveProjectRequested(_) => {}
        Message::RemoveProjectFinished { success } => {
            if !success {
                drawer::set_idle(&mut app.drawer, "remove failed to persist".to_string());
            }
        }
        // Opens the confirmation and nothing else. The menu closes either
        // way, like every other action taken from it.
        Message::RecreateVenvRequested(i) => {
            app.context_menu = None;
            if let Some(project) = app.config.projects.get(i)
                && let Ok(venv_path) = venv_path::resolve(project, &app.config.settings)
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
        Message::RecreateVenvConfirmed => {
            // `take` unconditionally: whether or not the work is dispatched,
            // the user has answered the question and the dialog is done.
            // Re-resolved by id, not by the index the press carried: the
            // list can have shifted underneath the open confirmation (see
            // `ConfirmRecreate`), and a project that no longer exists
            // resolves to nothing rather than to whoever took its place.
            if let Some(confirm) = app.confirm_recreate.take()
                && app.syncing.is_none()
                && let Some(project) = app
                    .config
                    .projects
                    .iter()
                    .find(|p| p.id == confirm.project_id)
                    .cloned()
            {
                let config = app.config.clone();
                app.syncing = Some(project.id.to_string());
                app.drawer = drawer::Drawer {
                    status: format!("recreating {}...", project.label),
                    ..drawer::Drawer::default()
                };
                // Reuses `SyncFinished`: a recreate ends in the same `uv sync`
                // with the same single-slot release, the same transcript and
                // the same expand-on-failure. What it does NOT reuse is the
                // status text's meaning -- `run_recreate_venv` appends a line
                // saying the environment was deleted and not rebuilt, which
                // is the part "sync failed" cannot express on its own.
                return Task::perform(
                    recreate_venv(project, config, runner),
                    |(success, lines)| Message::SyncFinished { success, lines },
                );
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
            if !app.drawer.lines.is_empty() {
                app.drawer.expanded = true;
            }
        }
        Message::AboutLicenseToggled => {
            app.about_expanded = !app.about_expanded;
        }
        Message::EnvVarAdded(scope) => {
            if let Some(vars) = env_editor::vars_mut(&mut app.config, scope) {
                env_editor::add_var(vars);
            }
            return persist_env_edit(app);
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
                    Message::EnvVarPersistFinished { success }
                });
            }
            return persist_env_edit(app);
        }
        Message::EnvVarKeyChanged(scope, index, key) => {
            if let Some(vars) = env_editor::vars_mut(&mut app.config, scope) {
                env_editor::set_key(vars, index, key);
            }
            return persist_env_edit(app);
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
                return persist_env_edit(app);
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
            let Some(vars) = env_editor::vars_mut(&mut app.config, scope) else {
                return Task::none();
            };
            let Some(key_name) = vars.get(index).map(|var| var.key.clone()) else {
                return Task::none();
            };
            let secret_key = env_editor::secret_key(scope, project_id, &key_name);

            // In-memory first, synchronously -- the same discipline every
            // other edit in this file uses -- then the keychain call and the
            // persist that must agree with it happen together in one
            // background job.
            // Takes the plaintext from the row's draft -- where, on this tab's
            // own workflow, it is the only copy, `Config` never having seen it
            // -- or from `Config` for a value committed earlier.
            if let Some(plain_value) =
                env_editor::take_secret_value(vars, &mut app.env_value_drafts, scope, index)
            {
                let config = app.config.clone();
                let persist = app.persist.clone();
                return Task::perform(
                    mark_secret_job(secret_key, plain_value, config, persist),
                    |success| Message::EnvVarPersistFinished { success },
                );
            }
            if env_editor::unmark_secret(vars, index) {
                let config = app.config.clone();
                let persist = app.persist.clone();
                return Task::perform(unmark_secret_job(secret_key, config, persist), |success| {
                    Message::EnvVarPersistFinished { success }
                });
            }
        }
        Message::EnvVarPersistFinished { success } => {
            if !success {
                drawer::set_idle(
                    &mut app.drawer,
                    "env var change failed to persist".to_string(),
                );
            }
        }
        Message::ScriptAddRequested(scope) => {
            return Task::perform(pick_script(), move |path| {
                Message::ScriptFilePicked(scope, path)
            });
        }
        // Cancelled -- nothing chosen, nothing to add.
        Message::ScriptFilePicked(_, None) => {}
        Message::ScriptFilePicked(scope, Some(path)) => {
            if let Some(scripts) = scripts_editor::scripts_mut(&mut app.config, scope) {
                scripts_editor::add_script(scripts, path);
            }
            return persist_script_edit(app);
        }
        Message::ScriptRemoved(scope, index) => {
            if let Some(scripts) = scripts_editor::scripts_mut(&mut app.config, scope) {
                scripts_editor::remove_script(scripts, index);
            }
            return persist_script_edit(app);
        }
        Message::ScriptEnabledToggled(scope, index) => {
            if let Some(scripts) = scripts_editor::scripts_mut(&mut app.config, scope) {
                scripts_editor::toggle_enabled(scripts, index);
            }
            return persist_script_edit(app);
        }
        Message::ScriptMovedUp(scope, index) => {
            if let Some(scripts) = scripts_editor::scripts_mut(&mut app.config, scope) {
                scripts_editor::move_up(scripts, index);
            }
            return persist_script_edit(app);
        }
        Message::ScriptMovedDown(scope, index) => {
            if let Some(scripts) = scripts_editor::scripts_mut(&mut app.config, scope) {
                scripts_editor::move_down(scripts, index);
            }
            return persist_script_edit(app);
        }
        Message::ScriptPersistFinished { success } => {
            if !success {
                drawer::set_idle(
                    &mut app.drawer,
                    "script change failed to persist".to_string(),
                );
            }
        }
        Message::IndexAdded => {
            index_editor::add_index(&mut app.config.indexes);
            app.index_credential_drafts.push(String::new());
            app.index_credential_names.push(String::new());
            return persist_index_edit(app);
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
                    Message::IndexPersistFinished { success }
                });
            }
            return persist_index_edit(app);
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
                // Cleared the instant it is handed off -- criterion 4's other
                // angle: this must not keep sitting in `App` state, readable
                // by a later render, any longer than typing takes.
                app.index_credential_drafts[i] = String::new();
                // `credential_save` resolved the account from the row's
                // *current* name, and the job below persists that same config,
                // so this is now where the credential lives -- recorded, or a
                // later commit would try to rename an entry that has already
                // moved and overwrite it with the old one.
                if let (Some(index), Some(slot)) = (
                    app.config.indexes.get(i),
                    app.index_credential_names.get_mut(i),
                ) {
                    *slot = index.name.clone();
                }
                let config = app.config.clone();
                let persist = app.persist.clone();
                return Task::perform(
                    mark_secret_job(key, plain_value, config, persist),
                    |success| Message::IndexPersistFinished { success },
                );
            }
        }
        Message::IndexPersistFinished { success } => {
            if !success {
                drawer::set_idle(
                    &mut app.drawer,
                    "index change failed to persist".to_string(),
                );
            }
        }
        Message::ProjectIndexToggled(project_index, name) => {
            if let Some(project) = app.config.projects.get_mut(project_index) {
                index_editor::toggle_selected(&mut project.index_names, &name);
            }
            return persist_index_edit(app);
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
                return persist_members_edit(app);
            }
        }
        Message::MembersPersistFinished { success } => {
            if !success {
                drawer::set_idle(
                    &mut app.drawer,
                    "member selection failed to persist".to_string(),
                );
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
                } else if app.syncing.is_none() {
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
                && app.syncing.is_none()
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
            return persist_settings_edit(app);
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
            return persist_settings_edit(app);
        }
        // A refused path -- including one below `MIN_UV_VERSION`, reported
        // through `UvResolveError::TooOld`'s own text -- only updates what
        // the tab shows. `Settings::uv_source` is untouched, so the bad path
        // never reaches `config.toml`.
        Message::SettingsUvCustomValidated(Err(message)) => {
            app.uv_custom_validation = Some(Err(message));
        }
        Message::SettingsPersistFinished { success } => {
            if !success {
                drawer::set_idle(
                    &mut app.drawer,
                    "settings change failed to persist".to_string(),
                );
            }
        }
        Message::SettingsDefaultVenvLocationAlongsideSelected => {
            settings_editor::apply_default_venv_location(
                &mut app.config.settings,
                DefaultVenvLocation::Alongside,
            );
            // Re-probed, not just persisted: the sidebar's venv status
            // describes wherever each project resolved when it was last
            // probed, and this just moved that. See `probe_every_venv`.
            return Task::batch([persist_settings_edit(app), probe_every_venv(app)]);
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
            return Task::batch([persist_settings_edit(app), probe_every_venv(app)]);
        }
    }
    Task::none()
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
            |success| Message::IndexPersistFinished { success },
        );
    }
    persist_index_edit(app)
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
    var.secret
        .then(|| env_editor::secret_key(scope, project_id, &var.key))
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
/// for a member selection edit (a checkbox toggle). Mirrors
/// `persist_env_edit`/`persist_script_edit`/`persist_index_edit` exactly --
/// this never touches the keychain, so a plain `persist_config` is the whole
/// job.
fn persist_members_edit(app: &App) -> Task<Message> {
    let config = app.config.clone();
    let persist = app.persist.clone();
    Task::perform(persist_config(config, persist), |success| {
        Message::MembersPersistFinished { success }
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
    let config = app.config.clone();
    app.syncing = Some(project.id.to_string());
    app.drawer = drawer::Drawer {
        status: format!("syncing members for {}...", project.label),
        ..drawer::Drawer::default()
    };
    // Reuses `SyncFinished`: this is still an ordinary `uv sync` under the
    // hood, with the same single-slot release, transcript handling and
    // expand-on-failure.
    Task::perform(
        members_sync(project, config, kind, discovered, runner),
        |(success, lines)| Message::SyncFinished { success, lines },
    )
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

/// Opens the async file picker for a pre-activate script (criterion 3). Same
/// reasoning as `pick_pyproject`: the blocking `rfd::FileDialog` must never
/// be used, since a modal dialog on the render thread would freeze the
/// window for as long as the user takes to answer it.
async fn pick_script() -> Option<PathBuf> {
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
        Ok(outcome) => Err(match outcome.stderr.trim() {
            "" => "uv python list failed".to_string(),
            stderr => stderr.to_string(),
        }),
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
    match job::run(
        move || list_interpreters_blocking(&config, runner.as_ref()),
        SYNC_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(result) => result,
        job::JobResult::TimedOut => Err("listing interpreters timed out".to_string()),
        job::JobResult::Failed => Err("listing interpreters failed unexpectedly".to_string()),
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
) -> Result<Vec<Interpreter>, String> {
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
) -> (bool, Vec<String>) {
    let Some(project_dir) = project
        .pyproject_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    else {
        return (false, vec!["no project directory to run uv in".to_string()]);
    };
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
    let spec = match members_editor::sync_plan(&project.members, kind, discovered) {
        members_editor::SyncPlan::Plain => uv::command::sync(&resolved_uv, &ctx),
        members_editor::SyncPlan::AllPackages => uv::command::sync_all_members(&resolved_uv, &ctx),
        members_editor::SyncPlan::Named(names) => {
            uv::command::sync_members(&resolved_uv, &ctx, &names)
        }
    };
    compose_and_run(spec, project, config, &venv_path, store, runner)
}

/// Runs a members-aware sync off the render thread -- same shape as
/// `sync_project`, including `SYNC_TIMEOUT`, since this is also a real `uv
/// sync` under the hood.
async fn members_sync(
    project: Project,
    config: Config,
    kind: bombadil_core::members::WorkspaceKind,
    discovered: Vec<bombadil_core::members::DiscoveredMember>,
    runner: Arc<dyn CommandRunner>,
) -> (bool, Vec<String>) {
    match job::run(
        move || {
            run_members_sync(
                &project,
                &config,
                &kind,
                &discovered,
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
    delete: impl Fn(&Path) -> Result<(), String>,
) -> (bool, Vec<String>) {
    let venv_path = match venv_path::resolve(project, &config.settings) {
        Ok(path) => path,
        Err(err) => return (false, vec![err.to_string()]),
    };

    if let Err(err) = delete(&venv_path) {
        return (false, vec![err, "nothing was deleted".to_string()]);
    }

    let (success, mut lines) = run_sync(project, config, store, runner);
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

/// Runs a recreate off the render thread -- same shape as `sync_project`,
/// including `SYNC_TIMEOUT`, since the rebuild half *is* a `uv sync`.
async fn recreate_venv(
    project: Project,
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> (bool, Vec<String>) {
    match job::run(
        move || {
            let settings = config.settings.clone();
            let target = &project;
            run_recreate_venv(
                &project,
                &config,
                &KeyringSecretStore::new(),
                runner.as_ref(),
                |path| venv_path::delete(target, &settings, path).map_err(|err| err.to_string()),
            )
        },
        SYNC_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(result) => result,
        job::JobResult::TimedOut => (
            false,
            vec!["recreating the environment timed out".to_string()],
        ),
        job::JobResult::Failed => (
            false,
            vec!["recreating the environment failed unexpectedly".to_string()],
        ),
    }
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
    let venv_path = venv_path::resolve(project, &config.settings).map_err(|e| e.to_string())?;
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

/// Runs off the render thread via `job::run` -- same shape as `sync_project`.
async fn open_terminal(
    choice: TerminalChoice,
    project: Project,
    config: Config,
    runner: Arc<dyn CommandRunner>,
) -> Result<(), String> {
    match job::run(
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
    )
    .await
    {
        job::JobResult::Done(result) => result,
        job::JobResult::TimedOut => Err("opening the terminal timed out".to_string()),
        job::JobResult::Failed => Err("opening the terminal failed unexpectedly".to_string()),
    }
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
    match job::run(
        move || run_open_folder(os, &project, runner.as_ref()),
        VENV_PROBE_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(result) => result,
        job::JobResult::TimedOut => Err("opening the folder timed out".to_string()),
        job::JobResult::Failed => Err("opening the folder failed unexpectedly".to_string()),
    }
}

/// The blocking half of persisting a config, with nothing else to do: just a
/// save. Its own function, mirroring `run_confirm_add_project`, so a test can
/// exercise it directly with a `FakeConfigPersist` instead of through
/// `update`'s opaque `Task`. Shared by every caller that only needs a plain
/// save -- a removal, and the plain (non-secret-toggling) env var edits.
fn run_persist(config: &Config, persist: &dyn ConfigPersist) -> bool {
    persist.save(config).is_ok()
}

/// Dispatches a background persist of `app.config` as it stands right now
/// (criterion 5). Shared by every plain env var edit (add, remove, rename,
/// retype a non-secret value) -- none of them touch the keychain, so a plain
/// `persist_config` is the whole job, unlike `EnvVarSecretToggled`, which
/// also has a keychain call that must land in the same background step.
fn persist_env_edit(app: &App) -> Task<Message> {
    let config = app.config.clone();
    let persist = app.persist.clone();
    Task::perform(persist_config(config, persist), |success| {
        Message::EnvVarPersistFinished { success }
    })
}

/// Dispatches a background persist of `app.config` as it stands right now,
/// for a script edit (add, remove, toggle enabled, reorder). Mirrors
/// `persist_env_edit` exactly -- none of these edits touch the keychain, so a
/// plain `persist_config` is the whole job.
fn persist_script_edit(app: &App) -> Task<Message> {
    let config = app.config.clone();
    let persist = app.persist.clone();
    Task::perform(persist_config(config, persist), |success| {
        Message::ScriptPersistFinished { success }
    })
}

/// Dispatches a background persist of `app.config` as it stands right now,
/// for an index edit that does not touch the keychain (add, remove, a field
/// edit, a project's selection toggle). Mirrors `persist_env_edit` and
/// `persist_script_edit` exactly.
///
/// `IndexCredentialSaveRequested` does *not* go through this: a credential
/// save's persist has to land in the same background job as the keychain
/// write, the same pairing `EnvVarSecretToggled` needs -- see
/// `mark_secret_job`, which it dispatches directly instead.
fn persist_index_edit(app: &App) -> Task<Message> {
    let config = app.config.clone();
    let persist = app.persist.clone();
    Task::perform(persist_config(config, persist), |success| {
        Message::IndexPersistFinished { success }
    })
}

/// Dispatches a background persist of `app.config` as it stands right now,
/// for a Settings edit (spec §5, Task 6) that needs no keychain call --
/// every one of them, since neither `uv_source` nor `default_venv_location`
/// carries a secret. Mirrors `persist_env_edit`/`persist_script_edit`/
/// `persist_index_edit` exactly.
fn persist_settings_edit(app: &App) -> Task<Message> {
    let config = app.config.clone();
    let persist = app.persist.clone();
    Task::perform(persist_config(config, persist), |success| {
        Message::SettingsPersistFinished { success }
    })
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
    match job::run(
        move || validate_uv_source_blocking(path, config, runner.as_ref()),
        SYNC_TIMEOUT,
    )
    .await
    {
        job::JobResult::Done(result) => result,
        job::JobResult::TimedOut => Err("validating uv timed out".to_string()),
        job::JobResult::Failed => Err("validating uv failed unexpectedly".to_string()),
    }
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
            let header = detail::header(project, &|_| entry.and_then(|e| e.python_version.clone()));
            detail::view(&header, i)
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
            Some((i, tab)) if Some(*i) == app.selected => deps::view(tab),
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
                    members_editor::view(&tab, *i)
                }
                None => iced::widget::text("").into(),
            },
            _ => iced::widget::text("").into(),
        },
        // Global variables are always shown; the project half only once a
        // project is selected -- the same `app.selected` `env_editor::view`
        // needs to know which project's `env` to show alongside them.
        Tab::EnvVars => env_editor::view(&app.config, app.selected, &app.env_value_drafts),
        // Same shape as the Env vars tab: global scripts always shown, the
        // project's own scripts once one is selected.
        Tab::Scripts => scripts_editor::view(&app.config, app.selected),
        // Every configured index, plus (once a project is selected) that
        // project's own selection among them (spec §7, Task 4).
        Tab::Indexes => index_editor::view(&app.config, app.selected, &app.index_credential_drafts),
        // Which uv binary to run and where new venvs go (spec §5, Task 6).
        // `resolved_default_venv_paths` recomputes fresh from `app.config` on
        // every render, the same "recompute, don't cache" trick the Env vars
        // and Indexes tabs already rely on, which is what makes criterion 3
        // visible the instant a location change lands, with no restart.
        Tab::Settings => settings_editor::view(
            &app.config.settings,
            &app.config.projects,
            &app.uv_custom_path_draft,
            app.uv_custom_validation.as_ref(),
        ),
    };

    // The current tab's own button is disabled rather than hidden -- the
    // same "no press needed, no press possible" treatment `index_editor`'s
    // auth buttons and `scripts_editor`'s move buttons give a control whose
    // action is already the state.
    let mut tab_bar = iced::widget::row![].spacing(4);
    for tab in Tab::ALL {
        tab_bar = tab_bar.push(
            iced::widget::button(iced::widget::text(tab.label()))
                .on_press_maybe((tab != app.tab).then_some(Message::TabSelected(tab))),
        );
    }

    let detail_and_drawer =
        iced::widget::column![detail, tab_bar, pane, drawer::view(&app.drawer)].spacing(8);

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

    // The Recreate venv confirmation (spec §8). Rendered from the resolved
    // path captured when it opened, not re-resolved here: the sentence the
    // user agrees to and the path that gets deleted must be the same one.
    let confirm_recreate_view: iced::Element<'_, Message> = match &app.confirm_recreate {
        Some(confirm) => context_menu::confirm_view(confirm),
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
    let footer = crate::about::view(app.about_expanded);
    // The dialog, the context menu and both confirmations sit *above* the
    // scrollable, not inside it. Nothing here is modal, so a confirmation
    // rendered after the body is a confirmation the user has to scroll to --
    // and with a real dependency tree that meant thousands of pixels below
    // the viewport, in a column that did not scroll at all. Each is an empty
    // `text("")` when closed, so they cost nothing when there is nothing to
    // answer.
    iced::widget::column![
        banner::view(&problems),
        add_project_dialog,
        context_menu_view,
        confirm_recreate_view,
        confirm_member_sync_view,
        iced::widget::scrollable(body)
            .width(iced::Length::Fill)
            .height(iced::Length::Fill),
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
            Message::TabSelected(Tab::Indexes),
            unused_runner(),
        );

        assert_eq!(app.tab, Tab::Indexes);
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
        app.deps = Some((0, deps::build(&PyProject::default(), &[], None)));
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
            tree: None,
        };

        let _ = update(&mut app, Message::DepsLoaded(0, fetch), unused_runner());

        let (index, tab) = app
            .deps
            .as_ref()
            .expect("a landed fetch must populate app.deps");
        assert_eq!(*index, 0);
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
            tree: None,
        };

        let _ = update(&mut app, Message::DepsLoaded(0, fetch), unused_runner());

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
        app.deps = Some((0, deps::build(&PyProject::default(), &[], None)));

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
            !app.entries[0].venv_exists,
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
            app.syncing.as_deref(),
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
        assert!(app.syncing.is_none());
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

        let (success, lines) = run_sync(&target, &app.config, &store, &runner);
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
        // shared drawer. `App::syncing`'s own doc names both consequences as
        // the reason the slot exists.
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

        assert!(app.syncing.is_none());
        let again = update(&mut app, Message::SyncRequested(0), unused_runner());
        assert!(
            again.units() > 0,
            "Sync must dispatch again once the confirm has finished"
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
            app.syncing.as_deref(),
            Some(c_id.as_str()),
            "confirming a recreate for b must never delete c's environment"
        );
        assert_eq!(
            task.units(),
            0,
            "a confirmation whose list shifted underneath it must dispatch nothing"
        );
        assert!(app.syncing.is_none());
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
            app.syncing.as_deref(),
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
            app.syncing.is_some(),
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

        let (success, _lines) = run_recreate_venv(
            &project,
            &config,
            &store,
            &runner,
            recording_delete(&deleted, Ok(())),
        );

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

        let (success, lines) = run_recreate_venv(
            &project,
            &config,
            &store,
            &runner,
            recording_delete(
                &deleted,
                Err("/p/api/.venv holds no pyvenv.cfg".to_string()),
            ),
        );

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

        let (success, lines) = run_recreate_venv(
            &project,
            &config,
            &store,
            &runner,
            recording_delete(&deleted, Ok(())),
        );

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
        let expected = venv_path::resolve(&project, &config.settings)
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
    fn the_bundled_licence_can_be_shown_and_hidden_again() {
        // Apache-2.0 section 4 obliges the text to be reachable. It is
        // compiled into the executable (`bombadil_uv_bin::UV_LICENSE`) and
        // had no consumer at all; this flag is what `view` renders it from,
        // so a toggle that never flips leaves the obligation unmet.
        let mut app = App::new(Config::default());
        assert!(!app.about_expanded, "the licence starts collapsed");

        let _ = update(&mut app, Message::AboutLicenseToggled, unused_runner());
        assert!(app.about_expanded);

        let _ = update(&mut app, Message::AboutLicenseToggled, unused_runner());
        assert!(!app.about_expanded);
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

        // The value has now actually moved out of `Config` (see the
        // previous test for where it went) -- so there is nothing left in
        // `app.config` for any later `Debug` render to print either.
        assert_eq!(app.config.global_env[0].value, "");
        assert!(app.config.global_env[0].secret);

        let finished = Message::EnvVarPersistFinished { success: true };
        assert!(!format!("{finished:?}").contains("hunter2"));
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
        assert!(
            app.env_value_drafts.is_empty(),
            "the toggle must have taken the draft -- if it is still here, the value \
             `update` handed the keychain job did not come from what the user typed"
        );

        // The keychain half runs against the in-memory store the real job's
        // `KeyringSecretStore` stands in for. Which plaintext
        // `env_editor::take_secret_value` yields for this state is proven
        // directly in `env_editor`'s own tests; what is proven here is that
        // `update` reaches it and leaves nothing behind.
        let key = SecretKey::GlobalEnv {
            key: "TOKEN".to_string(),
        };
        let store = bombadil_core::secrets::InMemorySecretStore::new();
        assert!(run_secret_write(
            &key,
            "hunter2",
            &app.config,
            &store,
            &persist
        ));

        assert_eq!(
            store.get(&key).unwrap().as_deref(),
            Some("hunter2"),
            "the value the user typed must be what the keychain ends up holding"
        );
        assert!(app.config.global_env[0].secret);
        assert_eq!(app.config.global_env[0].value, "");
        assert!(
            persist
                .saved()
                .iter()
                .all(|c| !format!("{c:?}").contains("hunter2")),
            "no config that was ever handed to a persister may contain the plaintext"
        );
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
        // Criterion 3: the whole point of going through `pick_script`
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
            app.index_credential_drafts[0], "",
            "the draft must be cleared the instant it is handed off"
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
            Message::IndexPersistFinished { success: true },
            Message::IndexPersistFinished { success: false },
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
        assert!(app.syncing.is_none());
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

        let (success, lines) =
            run_members_sync(&target, &app.config, &kind, &discovered, &store, &runner);
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
        let (success, lines) = run_sync(&target, &app.config, &store, &runner);
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
        assert!(app.syncing.is_some());
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
        assert!(app.syncing.is_some());
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
        assert!(app.syncing.is_none());
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
            venv: bombadil_core::model::VenvLocation::Default,
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
        let before = venv_path::resolve(&app.config.projects[0], &app.config.settings).unwrap();
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
        let after = venv_path::resolve(&app.config.projects[0], &app.config.settings).unwrap();
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
            venv: bombadil_core::model::VenvLocation::Default,
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
            3,
            "the persist plus one venv probe per project; got {}",
            alongside.units()
        );

        let picked = update(
            &mut app,
            Message::SettingsDefaultVenvLocationFolderPicked(Some(PathBuf::from("/central"))),
            unused_runner(),
        );
        assert_eq!(
            picked.units(),
            3,
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
                venv: bombadil_core::model::VenvLocation::Default,
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

        let new_path = venv_path::resolve(&app.config.projects[0], &app.config.settings).unwrap();
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
                venv: bombadil_core::model::VenvLocation::Default,
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
