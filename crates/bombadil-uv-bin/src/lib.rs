//! The uv binary, embedded.
//!
//! Separate from `bombadil-core` so that crate's tests neither link tens of
//! megabytes nor need the network.

use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The uv release embedded in this build.
pub const BUNDLED_VERSION: &str = "0.12.1";

/// uv's license text, redistributed with the embedded binary as Apache-2.0
/// section 4 requires. Surface this in an About dialog.
pub const UV_LICENSE: &str = include_str!("../UV-LICENSE-APACHE");

const EXE_NAME: &str = env!("BOMBADIL_UV_EXE_NAME");
const UV_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/", env!("BOMBADIL_UV_EXE_NAME")));

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("could not write the bundled uv to {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where the extracted binary lives, keyed by version and content hash so a
/// new build never reuses an old extraction.
pub fn extraction_path(data_dir: &Path) -> PathBuf {
    let digest = hex(&Sha256::digest(UV_BYTES));
    data_dir
        .join("uv")
        .join(format!("{BUNDLED_VERSION}-{}", &digest[..12]))
        .join(EXE_NAME)
}

/// Write the embedded uv to disk if it is not already there with the right
/// contents, and return its path.
///
/// Re-extracts when the file is missing or its hash does not match, so a
/// truncated or tampered extraction repairs itself.
///
/// Writes via a temp file in the same directory, chmods it, then renames it
/// into place — so a concurrent reader sees either no file or the complete,
/// correctly-permissioned one, never a partial write or a not-yet-executable
/// one.
pub fn ensure_extracted(data_dir: &Path) -> Result<PathBuf, ExtractError> {
    let path = extraction_path(data_dir);

    if let Ok(existing) = std::fs::read(&path)
        && Sha256::digest(&existing) == Sha256::digest(UV_BYTES)
    {
        return Ok(path);
    }

    let parent = path.parent().expect("extraction path always has a parent");
    std::fs::create_dir_all(parent).map_err(|source| ExtractError::Io {
        path: parent.to_path_buf(),
        source,
    })?;

    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|source| ExtractError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let tmp_path = tmp.path().to_path_buf();

    tmp.write_all(UV_BYTES).map_err(|source| ExtractError::Io {
        path: tmp_path.clone(),
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o755))
            .map_err(|source| ExtractError::Io {
                path: tmp_path.clone(),
                source,
            })?;
    }

    tmp.persist(&path).map_err(|e| ExtractError::Io {
        path: tmp_path,
        source: e.error,
    })?;

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_binary_is_not_empty() {
        assert!(UV_BYTES.len() > 1_000_000, "got {} bytes", UV_BYTES.len());
    }

    #[test]
    fn the_uv_license_is_present_and_looks_like_apache_2() {
        assert!(!UV_LICENSE.is_empty());
        assert!(UV_LICENSE.contains("Apache License"), "got:\n{UV_LICENSE}");
    }

    #[test]
    fn extraction_writes_a_runnable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = ensure_extracted(dir.path()).unwrap();

        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap().len(), UV_BYTES.len());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "extracted uv must be executable");
        }
    }

    #[test]
    fn a_second_extraction_reuses_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let first = ensure_extracted(dir.path()).unwrap();
        let second = ensure_extracted(dir.path()).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn extraction_is_atomic_no_temp_file_survives_and_permissions_are_already_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = ensure_extracted(dir.path()).unwrap();

        // The file at its final path exists and is already executable: a
        // reader that finds it at all never observes an unpersisted or
        // not-yet-chmod'd state, because both happen on the temp file before
        // the rename that makes it visible at `path`.
        assert!(path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "extracted uv must be executable");
        }

        // No leftover NamedTempFile survives in the extraction directory: a
        // rename that silently became a copy would leave two entries here.
        let extraction_dir = path.parent().unwrap();
        let entries: Vec<_> = std::fs::read_dir(extraction_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "expected only the extracted binary, found {entries:?}"
        );
    }

    #[test]
    fn a_corrupted_extraction_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let path = ensure_extracted(dir.path()).unwrap();
        std::fs::write(&path, b"truncated").unwrap();

        ensure_extracted(dir.path()).unwrap();

        assert_eq!(std::fs::read(&path).unwrap().len(), UV_BYTES.len());
    }
}
