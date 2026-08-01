use crate::error::StoreError;
use std::io::Write;
use std::path::Path;

/// Write `contents` to `path` such that a concurrent reader sees either the old
/// file or the new one, never a partial write.
///
/// The temporary file is created in the *same directory* as the target so the
/// final rename cannot cross a filesystem boundary. On Windows `persist` maps to
/// `MoveFileEx` with `MOVEFILE_REPLACE_EXISTING`, so an existing target is
/// replaced rather than causing an error.
///
/// A bare filename (no directory component) resolves against the current
/// directory, same as any relative path — it is not rejected by the
/// `BadPath` guard below, which only fires when there is no parent at all.
pub fn write_atomic(path: &Path, contents: &str) -> Result<(), StoreError> {
    let dir = path.parent().ok_or_else(|| StoreError::BadPath {
        path: path.to_path_buf(),
    })?;

    std::fs::create_dir_all(dir).map_err(|source| StoreError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|source| StoreError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    tmp.write_all(contents.as_bytes())
        .map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;

    tmp.as_file().sync_all().map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    tmp.persist(path).map_err(|e| StoreError::Io {
        path: path.to_path_buf(),
        source: e.error,
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_new_file_including_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested").join("config.toml");

        write_atomic(&target, "schema_version = 1\n").unwrap();

        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "schema_version = 1\n"
        );
    }

    #[test]
    fn overwrites_an_existing_file_completely() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");

        write_atomic(&target, "a-much-longer-first-write\n").unwrap();
        write_atomic(&target, "short\n").unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "short\n");
    }

    #[test]
    fn leaves_no_temporary_files_behind() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");

        write_atomic(&target, "one\n").unwrap();
        write_atomic(&target, "two\n").unwrap();

        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "expected only config.toml, found {entries:?}"
        );
    }
}
