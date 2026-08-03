use super::{EnvVar, MemberSelection, Script};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use uuid::Uuid;

/// Where a project's virtual environment lives.
///
/// `#[derive(Default)]` with `#[default]`, never a hand-written `impl`:
/// clippy's `derivable_impls` is denied in CI.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VenvLocation {
    /// Follow `Settings::default_venv_location`.
    #[default]
    Default,
    /// `<project dir>/.venv`.
    Alongside,
    /// A directory the user picked explicitly.
    Custom { path: PathBuf },
}

/// A registered project. Identity is the `id`, not the path, so moving or
/// renaming a project directory never orphans its variables and scripts.
///
/// `Default` exists so a new field can be added without editing every
/// construction site -- there are dozens across both crates, and a
/// mechanical edit of all of them is where a behavioural change hides. The
/// default `id` is the nil UUID, which is right for what a default `Project`
/// is: a test scaffold, not a project anyone registered.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub label: String,
    pub pyproject_path: PathBuf,
    /// References `Index::name` values in `Config::indexes`.
    #[serde(default)]
    pub index_names: Vec<String>,
    pub venv: VenvLocation,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    #[serde(default)]
    pub scripts: Vec<Script>,
    #[serde(default)]
    pub members: MemberSelection,
    /// This project's interpreter pin, or the global one.
    #[serde(default)]
    pub python: super::Override<super::PythonPin>,
    /// Which uv runs this project, or the global choice. A project that has
    /// to stay on an older uv is the case this exists for.
    #[serde(default)]
    pub uv_source: super::Override<super::UvSource>,
    /// Which terminal this project opens in, per OS. Empty means "follow
    /// `Config::terminal_defaults`".
    ///
    /// A map rather than an `Override<TerminalChoice>`, mirroring the global
    /// field: the terminals that exist differ per platform, so a project
    /// override of iTerm2 must not make the project unopenable on Linux.
    #[serde(default)]
    pub terminals: BTreeMap<crate::terminal::Os, super::TerminalChoice>,
}
