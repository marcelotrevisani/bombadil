//! The uv and just binaries, embedded.
//!
//! Separate from `bombadil-core` so that crate's tests neither link tens of
//! megabytes nor need the network.

use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The uv release embedded in this build.
pub const BUNDLED_VERSION: &str = env!("BOMBADIL_UV_VERSION");

/// uv's license text, redistributed with the embedded binary as Apache-2.0
/// section 4 requires. Surface this in an About dialog.
pub const UV_LICENSE: &str = include_str!("../UV-LICENSE-APACHE");

/// The just release embedded in this build.
pub const JUST_BUNDLED_VERSION: &str = env!("BOMBADIL_JUST_VERSION");

/// just's license text (CC0 1.0), shown beside uv's.
pub const JUST_LICENSE: &str = include_str!("../JUST-LICENSE-CC0");

const UV_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/", env!("BOMBADIL_UV_EXE_NAME")));
const JUST_BYTES: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/",
    env!("BOMBADIL_JUST_EXE_NAME")
));

/// One embedded program and where it goes under the data directory.
struct Embedded {
    folder: &'static str,
    version: &'static str,
    exe_name: &'static str,
    bytes: &'static [u8],
    digest: &'static OnceLock<Vec<u8>>,
}

static UV_DIGEST: OnceLock<Vec<u8>> = OnceLock::new();
static JUST_DIGEST: OnceLock<Vec<u8>> = OnceLock::new();

const UV: Embedded = Embedded {
    folder: "uv",
    version: BUNDLED_VERSION,
    exe_name: env!("BOMBADIL_UV_EXE_NAME"),
    bytes: UV_BYTES,
    digest: &UV_DIGEST,
};

const JUST: Embedded = Embedded {
    folder: "just",
    version: JUST_BUNDLED_VERSION,
    exe_name: env!("BOMBADIL_JUST_EXE_NAME"),
    bytes: JUST_BYTES,
    digest: &JUST_DIGEST,
};

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("could not write the bundled program to {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Embedded {
    /// The embedded binary's digest, hashed once per process rather than on
    /// every call: the bytes are a multi-megabyte `const`, so they cannot
    /// change within a process, and re-hashing uv on every call added up to
    /// well over 100 MB of hashing per app launch. This is deliberately
    /// distinct from the digest of the *extracted* file on disk in
    /// `ensure`, which must stay live: self-repair of a truncated or
    /// tampered extraction depends on that one being recomputed every time.
    fn digest(&self) -> &'static [u8] {
        self.digest
            .get_or_init(|| Sha256::digest(self.bytes).to_vec())
    }

    fn path(&self, data_dir: &Path) -> PathBuf {
        let digest = hex(self.digest());
        data_dir
            .join(self.folder)
            .join(format!("{}-{}", self.version, &digest[..12]))
            .join(self.exe_name)
    }

    fn ensure(&self, data_dir: &Path) -> Result<PathBuf, ExtractError> {
        let path = self.path(data_dir);

        if let Ok(existing) = std::fs::read(&path)
            && Sha256::digest(&existing).as_slice() == self.digest()
        {
            return Ok(path);
        }

        // `path` always joins a file name onto a directory, so there is a
        // parent; an error rather than a panic if that ever stops being so.
        let Some(parent) = path.parent() else {
            return Err(ExtractError::Io {
                path: path.clone(),
                source: std::io::Error::other("the extraction path has no parent directory"),
            });
        };
        std::fs::create_dir_all(parent).map_err(|source| ExtractError::Io {
            path: parent.to_path_buf(),
            source,
        })?;

        let mut tmp =
            tempfile::NamedTempFile::new_in(parent).map_err(|source| ExtractError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        let tmp_path = tmp.path().to_path_buf();

        tmp.write_all(self.bytes)
            .map_err(|source| ExtractError::Io {
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
}

/// Where the extracted uv lives, keyed by version and content hash so a new
/// build never reuses an old extraction.
pub fn extraction_path(data_dir: &Path) -> PathBuf {
    UV.path(data_dir)
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
    UV.ensure(data_dir)
}

/// [`ensure_extracted`] for the embedded just, under `<data_dir>/just/`.
pub fn ensure_just_extracted(data_dir: &Path) -> Result<PathBuf, ExtractError> {
    JUST.ensure(data_dir)
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
    fn the_just_license_is_cc0() {
        assert!(
            JUST_LICENSE.contains("CC0 1.0 Universal"),
            "got:\n{JUST_LICENSE}"
        );
    }

    #[test]
    fn just_is_extracted_beside_uv_not_over_it() {
        let dir = tempfile::tempdir().unwrap();
        let uv = ensure_extracted(dir.path()).unwrap();
        let just = ensure_just_extracted(dir.path()).unwrap();

        assert_ne!(uv.parent(), just.parent());
        assert!(just.starts_with(dir.path().join("just")));
        assert_eq!(std::fs::read(&just).unwrap(), JUST_BYTES);
        assert_eq!(std::fs::read(&uv).unwrap().len(), UV_BYTES.len());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&just).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "extracted just must be executable");
        }
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
