use crate::error::StoreError;
use std::path::{Path, PathBuf};

/// The directory holding `config.toml`.
///
/// `~/.config/bombadil` on Linux, `~/Library/Application Support/bombadil` on
/// macOS, `%APPDATA%\bombadil` on Windows.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_directory_resolves_on_this_platform() {
        let dir = default_config_dir().expect("every supported platform has a config dir");
        assert!(dir.ends_with("bombadil"), "got {dir:?}");
    }

    #[test]
    fn the_config_file_is_named_config_toml() {
        let file = config_file_in(std::path::Path::new("/somewhere"));
        assert_eq!(file, std::path::Path::new("/somewhere/config.toml"));
    }
}
