use super::{EnvVar, Environment, MemberSelection, PythonPin, Script};
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub label: String,
    pub pyproject_path: PathBuf,
    /// References `Index::name` values in `Config::indexes`.
    #[serde(default)]
    pub index_names: Vec<String>,
    /// Every environment configured for this project.
    ///
    /// Never empty after a load, because migration synthesises one from the
    /// version-1 `venv` field -- but it can be emptied by removing the last,
    /// so `active_environment` still answers `None`.
    #[serde(default)]
    pub environments: Vec<Environment>,
    /// Which environment Sync, Open terminal, Recreate and the sidebar glyph
    /// act on.
    ///
    /// The location rather than an index, for the reason `ConfirmRecreate`
    /// documents at length: an index is only meaningful against the list as
    /// it stood when it was captured, and this list is edited while the
    /// application is running.
    #[serde(default)]
    pub active: VenvLocation,
    /// Whether this project's environments are shown in the sidebar tree.
    ///
    /// Persisted, so a project you collapse stays collapsed across a restart.
    /// Defaults to *expanded*: after migration every project has exactly one
    /// environment, and a collapsed default would hide the thing the tree
    /// exists to show. `#[serde(default)]` on a `bool` is `false`, which is
    /// why this names a function instead.
    #[serde(default = "expanded_by_default")]
    pub expanded: bool,
    /// Version 1's single location. Read only by the migration, never
    /// written again -- leaving it live would mean two places a location
    /// could be set and two answers to which is current.
    #[serde(default, rename = "venv", skip_serializing)]
    pub legacy_venv: Option<VenvLocation>,
    /// Version 1's project-level pin, which now belongs to an environment.
    /// Same read-once treatment as `legacy_venv`.
    #[serde(default, rename = "python", skip_serializing)]
    pub legacy_python: Option<super::Override<PythonPin>>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    #[serde(default)]
    pub scripts: Vec<Script>,
    /// Saved Run-tab commands, in the order shown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<super::SavedCommand>,
    #[serde(default)]
    pub members: MemberSelection,
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

fn expanded_by_default() -> bool {
    true
}

impl Default for Project {
    /// Hand-written because `expanded` defaults to `true`, which a derive
    /// cannot express. Clippy's `derivable_impls` does not fire: this is not
    /// what a derive would produce.
    fn default() -> Self {
        Self {
            id: Uuid::nil(),
            label: String::new(),
            pyproject_path: PathBuf::new(),
            index_names: Vec::new(),
            environments: Vec::new(),
            active: VenvLocation::Default,
            expanded: expanded_by_default(),
            legacy_venv: None,
            legacy_python: None,
            env: Vec::new(),
            scripts: Vec::new(),
            commands: Vec::new(),
            members: MemberSelection::default(),
            uv_source: super::Override::Inherit,
            terminals: BTreeMap::new(),
        }
    }
}

impl Project {
    /// The environment everything acts on.
    ///
    /// Falls back to the first when `active` names a location no longer in
    /// the list -- which removing an environment can leave behind. One
    /// fallback here rather than one per caller: sync, the terminal launcher
    /// and the drift glyph must not each invent their own recovery and then
    /// disagree about which environment is current.
    pub fn active_environment(&self) -> Option<&Environment> {
        self.environments
            .iter()
            .find(|environment| environment.location == self.active)
            .or_else(|| self.environments.first())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_commands_round_trip_and_are_omitted_when_empty() {
        let mut p = Project::default();
        let empty = toml::to_string(&p).unwrap();
        assert!(!empty.contains("commands"), "{empty}");
        p.commands.push(crate::model::SavedCommand {
            label: "tests".into(),
            command: "pytest -q".into(),
        });
        let back: Project = toml::from_str(&toml::to_string(&p).unwrap()).unwrap();
        assert_eq!(back.commands, p.commands);
    }
}
