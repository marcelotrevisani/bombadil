use crate::error::StoreError;
use std::path::{Path, PathBuf};

/// The directory holding `config.toml`.
///
/// This is whatever the `directories` crate's `ProjectDirs::config_dir()`
/// returns for `("", "", "bombadil")`, verified against `directories` 6.0.0:
/// `~/.config/bombadil` on Linux, `~/Library/Application Support/bombadil` on
/// macOS, and `%APPDATA%\bombadil\config` on Windows — note the extra
/// `config` path segment Windows gets that the other two platforms don't.
pub fn default_config_dir() -> Result<PathBuf, StoreError> {
    directories::ProjectDirs::from("", "", "bombadil")
        .map(|dirs| dirs.config_dir().to_path_buf())
        .ok_or(StoreError::NoHomeDirectory)
}

/// The config file inside a given directory. Separate from
/// [`default_config_dir`] so tests can point at a temporary directory.
pub fn config_file_in(dir: &Path) -> PathBuf {
    dir.join("config.toml")
}

/// The directory holding larger, non-roaming data: the extracted bundled uv
/// binary and anything else too big or too local-machine-specific to belong
/// in the config directory.
///
/// This is a *different* directory from [`default_config_dir`]. It is
/// whatever `directories`' `ProjectDirs::data_local_dir()` returns for
/// `("", "", "bombadil")`, verified against `directories` 6.0.0:
/// `~/.local/share/bombadil` on Linux, `~/Library/Application Support/bombadil`
/// on macOS, and `%LOCALAPPDATA%\bombadil\data` on Windows — notably
/// `%LOCALAPPDATA%`, not the `%APPDATA%` that [`default_config_dir`] uses, so
/// this data does not roam with a Windows user's profile.
pub fn default_data_dir() -> Result<PathBuf, StoreError> {
    directories::ProjectDirs::from("", "", "bombadil")
        .map(|dirs| dirs.data_local_dir().to_path_buf())
        .ok_or(StoreError::NoHomeDirectory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_directory_resolves_on_this_platform() {
        let dir = default_config_dir().expect("every supported platform has a config dir");
        // Not `ends_with("bombadil")`: on Windows the real path is
        // `%APPDATA%\bombadil\config`, so only a `bombadil` component
        // somewhere in the path is true on all three platforms.
        assert!(
            dir.components().any(|c| c.as_os_str() == "bombadil"),
            "got {dir:?}"
        );
    }

    #[test]
    fn a_data_directory_resolves_on_this_platform() {
        let dir = default_data_dir().expect("every supported platform has a data dir");
        // Same shape as the config-dir test above: assert a `bombadil`
        // component is present, not an exact string, since the path differs
        // per platform (and per XDG_DATA_HOME on Linux).
        assert!(
            dir.components().any(|c| c.as_os_str() == "bombadil"),
            "got {dir:?}"
        );
    }

    #[test]
    fn the_data_directory_differs_from_the_config_directory() {
        // The whole point of adding a separate function: uv's extracted
        // binary must not land inside the config directory.
        let config = default_config_dir().unwrap();
        let data = default_data_dir().unwrap();
        assert_ne!(config, data, "config and data dirs must not coincide");
    }

    #[test]
    fn the_config_file_is_named_config_toml() {
        let file = config_file_in(std::path::Path::new("/somewhere"));
        assert_eq!(file, std::path::Path::new("/somewhere/config.toml"));
    }
}
