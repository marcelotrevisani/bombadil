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

/// Light, dark, or whatever the desktop is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    /// Follow the OS, live.
    #[default]
    System,
    Light,
    Dark,
}

/// Where the window and its panels were left, so they come back that way.
/// Whole pixels: they are what a person can see, and keep `Settings` `Eq`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub sidebar_width: u32,
    pub sidebar_collapsed: bool,
    /// The window's last inner size; `None` lets the toolkit choose.
    pub window: Option<(u32, u32)>,
    /// Tab keys in the user's order; empty is the built-in order.
    pub tab_order: Vec<String>,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            sidebar_width: 240,
            sidebar_collapsed: false,
            window: None,
            tab_order: Vec::new(),
        }
    }
}

/// Application-wide settings.
///
/// Later plans add fields (terminal defaults). Because every field is
/// `#[serde(default)]`, adding one needs no schema migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub uv_source: UvSource,
    #[serde(default)]
    pub default_venv_location: DefaultVenvLocation,
    /// The interpreter new environments are built with, and the one Sync
    /// asserts. `Unpinned` leaves the choice to uv and the manifest's
    /// `requires-python`, which is what happened before this existed.
    #[serde(default)]
    pub python: super::PythonPin,
    /// Appearance. `System` follows the desktop's light/dark preference.
    #[serde(default)]
    pub theme: ThemeChoice,
    #[serde(default)]
    pub layout: Layout,
    /// Whether the window's close button leaves Bombadil running in the tray.
    /// The GUI still quits when there is no tray to come back from.
    #[serde(default = "yes")]
    pub close_to_tray: bool,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            uv_source: UvSource::default(),
            default_venv_location: DefaultVenvLocation::default(),
            python: super::PythonPin::default(),
            theme: ThemeChoice::default(),
            layout: Layout::default(),
            close_to_tray: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_written_before_layout_existed_loads_the_defaults() {
        let settings: Settings = toml::from_str("theme = \"dark\"\n").unwrap();
        assert_eq!(settings.layout.sidebar_width, 240);
        assert!(!settings.layout.sidebar_collapsed);
        assert_eq!(settings.layout.window, None);
        assert!(settings.layout.tab_order.is_empty());
    }

    #[test]
    fn closing_keeps_bombadil_in_the_tray_unless_told_otherwise() {
        let settings: Settings = toml::from_str("theme = \"dark\"\n").unwrap();
        assert!(settings.close_to_tray);
        let off: Settings = toml::from_str("close_to_tray = false\n").unwrap();
        assert!(!off.close_to_tray);
        assert!(Settings::default().close_to_tray);
    }

    #[test]
    fn a_partial_layout_fills_in_the_rest() {
        let settings: Settings = toml::from_str("[layout]\nsidebar_collapsed = true\n").unwrap();
        assert!(settings.layout.sidebar_collapsed);
        assert_eq!(settings.layout.sidebar_width, 240);
    }

    #[test]
    fn a_config_written_before_themes_existed_loads_as_system() {
        let settings: Settings = toml::from_str("[uv_source]\nkind = \"auto\"\n").unwrap();
        assert_eq!(settings.theme, ThemeChoice::System);
    }

    #[test]
    fn the_theme_choice_round_trips_in_snake_case() {
        let settings = Settings {
            theme: ThemeChoice::Light,
            ..Settings::default()
        };
        let text = toml::to_string(&settings).unwrap();
        assert!(text.contains("theme = \"light\""), "{text}");
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), settings);
    }
}
