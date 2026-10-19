//! The one uv command that may run at a time, and the words for it.
//!
//! Pure on purpose: `app::update` decides, views render, and everything worth
//! a test is here.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Sync,
    /// The `name==version` requirement being installed.
    InstallVersion(String),
    /// The first sync of a project just added.
    AddProject,
    /// A sync that rebuilds the venv on a different interpreter.
    PinChange,
    Recreate,
    MembersSync,
    /// One package by name, or every package.
    Upgrade(Option<String>),
    /// A local package, by its name or folder name.
    AddLocal(String),
    /// `uv cache prune`: no project of its own.
    CachePrune,
}

#[derive(Debug, Clone)]
pub struct Busy {
    pub project_id: uuid::Uuid,
    pub action: Action,
    pub started: Instant,
}

impl Busy {
    pub fn new(project_id: uuid::Uuid, action: Action) -> Self {
        Self {
            project_id,
            action,
            started: Instant::now(),
        }
    }

    /// "Syncing api… 12s". Seconds appear from the first whole second.
    pub fn label(&self, project_label: Option<&str>, now: Instant) -> String {
        let secs = now.saturating_duration_since(self.started).as_secs();
        let base = self.action.present(project_label);
        if secs == 0 {
            format!("{base}…")
        } else {
            format!("{base}… {secs}s")
        }
    }
}

/// `" api"` or nothing, so a missing project degrades the sentence instead of
/// breaking it.
fn named(project_label: Option<&str>) -> String {
    project_label.map(|l| format!(" {l}")).unwrap_or_default()
}

/// `" of api"` or nothing, for the members wording.
fn of(project_label: Option<&str>) -> String {
    project_label
        .map(|l| format!(" of {l}"))
        .unwrap_or_default()
}

impl Action {
    fn present(&self, p: Option<&str>) -> String {
        let n = named(p);
        match self {
            Action::Sync => format!("Syncing{n}"),
            Action::InstallVersion(req) => format!("Installing {req}"),
            Action::AddProject => format!("Setting up{n}"),
            Action::PinChange => format!("Rebuilding{n}"),
            Action::Recreate => format!("Recreating{n}"),
            Action::MembersSync => format!("Syncing members{}", of(p)),
            Action::Upgrade(Some(name)) => format!("Upgrading {name}"),
            Action::Upgrade(None) => format!("Upgrading all packages{}", of(p)),
            Action::AddLocal(name) => format!("Adding {name}"),
            Action::CachePrune => "Cleaning uv's cache".into(),
        }
    }

    /// The sentence a successful finish reports.
    pub fn done(&self, p: Option<&str>) -> String {
        let n = named(p);
        match self {
            Action::Sync => format!("Synced{n}"),
            Action::InstallVersion(req) => format!("Installed {req}"),
            Action::AddProject => format!("Added{n}"),
            Action::PinChange => format!("Rebuilt{n}"),
            Action::Recreate => format!("Recreated{n}"),
            Action::MembersSync => format!("Synced members{}", of(p)),
            Action::Upgrade(Some(name)) => format!("Upgraded {name}"),
            Action::Upgrade(None) => format!("Upgraded all packages{}", of(p)),
            Action::AddLocal(name) => format!("Added {name}"),
            Action::CachePrune => "Cleaned uv's cache".into(),
        }
    }

    /// The sentence a failed finish reports.
    pub fn failed(&self, p: Option<&str>) -> String {
        let n = named(p);
        match self {
            Action::Sync => format!("Could not sync{n}"),
            Action::InstallVersion(req) => format!("Could not install {req}"),
            Action::AddProject => format!("Could not set up{n}"),
            Action::PinChange => format!("Could not rebuild{n}"),
            Action::Recreate => format!("Could not recreate{n}"),
            Action::MembersSync => format!("Could not sync members{}", of(p)),
            Action::Upgrade(Some(name)) => format!("Could not upgrade {name}"),
            Action::Upgrade(None) => format!("Could not upgrade the packages{}", of(p)),
            Action::AddLocal(name) => format!("Could not add {name}"),
            Action::CachePrune => "Could not clean uv's cache".into(),
        }
    }
}

/// The least time between two focus-triggered re-reads.
pub const FOCUS_DEBOUNCE: Duration = Duration::from_secs(2);

/// How long a success stays on screen. Failures stay until dismissed.
pub const SUCCESS_TOAST: Duration = Duration::from_secs(4);
/// More than this and the oldest is dropped.
pub const MAX_TOASTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    /// A command failed: stays, and offers its output.
    Failure,
    /// Something to act on that is not a failed command: stays, no output.
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    /// `None` until dismissed: a failure must not vanish unread.
    pub expires: Option<Instant>,
    /// The doctor's first fix for a failure, for the project with this id.
    pub fix: Option<(uuid::Uuid, bombadil_core::doctor::DoctorFix)>,
}

/// Newest first; the oldest beyond [`MAX_TOASTS`] is dropped.
pub fn push(toasts: &mut Vec<Toast>, kind: ToastKind, text: String, now: Instant) {
    let expires = (kind == ToastKind::Success).then(|| now + SUCCESS_TOAST);
    toasts.insert(
        0,
        Toast {
            kind,
            text,
            expires,
            fix: None,
        },
    );
    toasts.truncate(MAX_TOASTS);
}

/// Drops every toast whose time has passed. Failures have none.
pub fn expire(toasts: &mut Vec<Toast>, now: Instant) {
    toasts.retain(|t| t.expires.is_none_or(|at| now < at));
}

/// The spinner's angle in radians: one full turn per second since `started`.
pub fn spin(started: Instant, now: Instant) -> f32 {
    now.saturating_duration_since(started).as_secs_f32().fract() * std::f32::consts::TAU
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrades_and_local_adds_say_what_they_do() {
        let one = Action::Upgrade(Some("fastapi".into()));
        assert_eq!(one.present(Some("api")), "Upgrading fastapi");
        assert_eq!(one.done(Some("api")), "Upgraded fastapi");
        assert_eq!(one.failed(Some("api")), "Could not upgrade fastapi");
        let all = Action::Upgrade(None);
        assert_eq!(all.done(Some("api")), "Upgraded all packages of api");
        let local = Action::AddLocal("mylib".into());
        assert_eq!(local.present(None), "Adding mylib");
        assert_eq!(local.failed(None), "Could not add mylib");
    }

    fn busy(action: Action) -> Busy {
        Busy::new(uuid::Uuid::nil(), action)
    }

    #[test]
    fn label_names_the_action_and_project_and_counts_seconds() {
        let b = busy(Action::Sync);
        assert_eq!(b.label(Some("api"), b.started), "Syncing api…");
        assert_eq!(
            b.label(Some("api"), b.started + Duration::from_millis(12_400)),
            "Syncing api… 12s"
        );
    }

    #[test]
    fn every_action_has_its_own_wording() {
        let now_of = |a: Action| {
            let b = busy(a);
            b.label(Some("api"), b.started)
        };
        assert_eq!(
            now_of(Action::InstallVersion("fastapi==0.110".into())),
            "Installing fastapi==0.110…"
        );
        assert_eq!(now_of(Action::AddProject), "Setting up api…");
        assert_eq!(now_of(Action::PinChange), "Rebuilding api…");
        assert_eq!(now_of(Action::Recreate), "Recreating api…");
        assert_eq!(now_of(Action::MembersSync), "Syncing members of api…");
    }

    #[test]
    fn label_without_a_project_name() {
        // The project was removed while its command ran.
        let b = busy(Action::Sync);
        assert_eq!(b.label(None, b.started), "Syncing…");
    }

    #[test]
    fn a_success_toast_expires_and_a_failure_does_not() {
        let now = Instant::now();
        let mut t = Vec::new();
        push(&mut t, ToastKind::Success, "Synced api".into(), now);
        push(&mut t, ToastKind::Failure, "Could not sync api".into(), now);
        assert_eq!(t[0].kind, ToastKind::Failure, "newest first");
        assert_eq!(t[0].expires, None);
        assert_eq!(t[1].expires, Some(now + SUCCESS_TOAST));
    }

    #[test]
    fn toasts_are_capped_at_three() {
        let now = Instant::now();
        let mut t = Vec::new();
        for i in 0..5 {
            push(&mut t, ToastKind::Success, format!("{i}"), now);
        }
        assert_eq!(t.len(), MAX_TOASTS);
        assert_eq!(t[0].text, "4", "the oldest are the ones dropped");
    }

    #[test]
    fn tick_expires_successes_only() {
        let now = Instant::now();
        let mut t = Vec::new();
        push(&mut t, ToastKind::Success, "ok".into(), now);
        push(&mut t, ToastKind::Failure, "bad".into(), now);
        expire(&mut t, now + SUCCESS_TOAST + Duration::from_millis(1));
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].kind, ToastKind::Failure);
    }

    #[test]
    fn tick_never_expires_a_failure() {
        let now = Instant::now();
        let mut t = Vec::new();
        push(&mut t, ToastKind::Failure, "bad".into(), now);
        expire(&mut t, now + Duration::from_secs(3600));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn the_spinner_turns_once_a_second() {
        let start = Instant::now();
        assert_eq!(spin(start, start), 0.0);
        let quarter = spin(start, start + Duration::from_millis(250));
        assert!(
            (quarter - std::f32::consts::FRAC_PI_2).abs() < 1e-3,
            "{quarter}"
        );
    }

    #[test]
    fn cleaning_the_cache_has_its_own_words() {
        assert_eq!(Action::CachePrune.present(None), "Cleaning uv's cache");
        assert_eq!(Action::CachePrune.done(None), "Cleaned uv's cache");
        assert_eq!(
            Action::CachePrune.failed(None),
            "Could not clean uv's cache"
        );
    }

    #[test]
    fn finish_sentences() {
        assert_eq!(Action::Sync.done(Some("api")), "Synced api");
        assert_eq!(Action::Sync.failed(Some("api")), "Could not sync api");
        let install = Action::InstallVersion("fastapi==0.110".into());
        assert_eq!(install.done(Some("api")), "Installed fastapi==0.110");
        assert_eq!(install.failed(None), "Could not install fastapi==0.110");
        assert_eq!(Action::Recreate.done(Some("api")), "Recreated api");
        assert_eq!(Action::AddProject.done(Some("api")), "Added api");
        assert_eq!(
            Action::PinChange.failed(Some("api")),
            "Could not rebuild api"
        );
        assert_eq!(
            Action::MembersSync.done(Some("api")),
            "Synced members of api"
        );
    }
}
