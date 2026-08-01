//! The uv binary, embedded.
//!
//! Separate from `bombadil-core` so that crate's tests neither link tens of
//! megabytes nor need the network.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The uv release embedded in this build.
pub const BUNDLED_VERSION: &str = "0.12.1";

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
    std::fs::write(&path, UV_BYTES).map_err(|source| ExtractError::Io {
        path: path.clone(),
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).map_err(
            |source| ExtractError::Io {
                path: path.clone(),
                source,
            },
        )?;
    }

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
    fn a_corrupted_extraction_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let path = ensure_extracted(dir.path()).unwrap();
        std::fs::write(&path, b"truncated").unwrap();

        ensure_extracted(dir.path()).unwrap();

        assert_eq!(std::fs::read(&path).unwrap().len(), UV_BYTES.len());
    }
}
