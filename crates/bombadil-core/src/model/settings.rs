use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where new virtual environments go when a project says `VenvLocation::Default`.
///
/// Use `#[derive(Default)]` with `#[default]`, not a hand-written `impl Default`:
/// clippy's `derivable_impls` lint rejects the manual form, and CI denies warnings.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DefaultVenvLocation {
    /// `<project dir>/.venv`, which is what uv does on its own.
    #[default]
    Alongside,
    /// One shared directory holding `<project-dir-name>-<hash>` subdirectories.
    Central { path: PathBuf },
}

/// Which uv binary to run. Consumed in plan 2.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UvSource {
    /// uv from `PATH` if it is at least `MIN_UV_VERSION`, otherwise the bundled copy.
    #[default]
    Auto,
    /// Always the embedded copy.
    Bundled,
    /// Always uv from `PATH`; fails loudly rather than falling back.
    FromPath,
    /// A specific binary; fails loudly if missing.
    Custom { path: PathBuf },
}

/// Application-wide settings.
///
/// Later plans add fields (theme, terminal defaults). Because every field is
/// `#[serde(default)]`, adding one needs no schema migration.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub uv_source: UvSource,
    #[serde(default)]
    pub default_venv_location: DefaultVenvLocation,
}
