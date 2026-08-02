use super::{EnvVar, MemberSelection, Script};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Where a project's virtual environment lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VenvLocation {
    /// Follow `Settings::default_venv_location`.
    Default,
    /// `<project dir>/.venv`.
    Alongside,
    /// A directory the user picked explicitly.
    Custom { path: PathBuf },
}

/// A registered project. Identity is the `id`, not the path, so moving or
/// renaming a project directory never orphans its variables and scripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}
