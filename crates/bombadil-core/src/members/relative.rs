use std::path::{Component, Path, PathBuf};

/// The path form to hand `uv add --editable`, which uv writes verbatim into
/// `[tool.uv.sources]`.
///
/// Prefers a relative path, because `pyproject.toml` is shared: an absolute
/// path baked into it works only on the machine that added it. Falls back to
/// the target unchanged when no relative path exists — different Windows
/// drives, or a target that is already relative.
///
/// Pure: operates on path components and never touches the filesystem, so it
/// works for paths that do not exist yet. Inputs are assumed lexically
/// normal — no `..` components — since purity means no canonicalisation
/// happens here to clean them up first; a caller holding a path that may
/// contain `..` must normalise it before calling.
///
/// Note: The fallback for differing `Component::Prefix` values (Windows drive
/// letters) is reachable only on Windows. On Unix, all absolute paths share
/// one `RootDir` component which cancels during prefix-stripping, so the check
/// at line 35-40 does not apply. This is intentional and correct — it is the
/// detection mechanism for an unrelateable pair.
pub fn path_for_manifest(project_dir: &Path, target: &Path) -> PathBuf {
    if !target.is_absolute() || !project_dir.is_absolute() {
        return target.to_path_buf();
    }

    let mut from = project_dir.components().peekable();
    let mut to = target.components().peekable();

    // Drop the shared prefix.
    while let (Some(a), Some(b)) = (from.peek(), to.peek()) {
        if a == b {
            from.next();
            to.next();
        } else {
            break;
        }
    }

    // A different prefix component means different roots — on Windows, a
    // different drive. No relative path exists.
    let from: Vec<Component<'_>> = from.collect();
    let to: Vec<Component<'_>> = to.collect();
    if from
        .iter()
        .chain(to.iter())
        .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
    {
        return target.to_path_buf();
    }

    let mut relative = PathBuf::new();
    for _ in &from {
        relative.push("..");
    }
    for component in &to {
        relative.push(component.as_os_str());
    }

    if relative.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        relative
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn a_child_of_the_project_is_relative() {
        assert_eq!(
            path_for_manifest(
                Path::new("/work/repo"),
                Path::new("/work/repo/packages/alpha")
            ),
            PathBuf::from("packages/alpha")
        );
    }

    #[test]
    fn a_sibling_checkout_walks_up_and_back_down() {
        // The case that matters: a shared library checked out beside the repo.
        assert_eq!(
            path_for_manifest(Path::new("/work/repo"), Path::new("/work/shared-lib")),
            PathBuf::from("../shared-lib")
        );
    }

    #[test]
    fn a_distant_relative_is_still_expressed_relatively() {
        assert_eq!(
            path_for_manifest(Path::new("/a/b/c/d"), Path::new("/a/x/y")),
            PathBuf::from("../../../x/y")
        );
    }

    #[test]
    fn the_project_itself_is_the_current_directory() {
        assert_eq!(
            path_for_manifest(Path::new("/work/repo"), Path::new("/work/repo")),
            PathBuf::from(".")
        );
    }

    #[test]
    fn a_relative_project_dir_is_returned_unchanged() {
        // Nothing sensible to compute against, so the target is handed to uv as
        // given. NOTE: this does NOT exercise the Prefix/RootDir branch — it
        // short-circuits on the relative project_dir before reaching it.
        let target = Path::new("/elsewhere/lib");
        assert_eq!(
            path_for_manifest(Path::new("relative/project"), target),
            target
        );
    }

    #[test]
    fn a_relative_target_is_returned_unchanged() {
        // Nothing sensible to compute against; hand it to uv as given.
        assert_eq!(
            path_for_manifest(Path::new("/work/repo"), Path::new("../lib")),
            PathBuf::from("../lib")
        );
    }

    #[cfg(windows)]
    #[test]
    fn paths_on_different_windows_drives_stay_absolute() {
        // The branch at line 35-40 is unreachable on Unix: every absolute Unix
        // path shares one RootDir component, which cancels in the
        // prefix-stripping loop. Only a differing drive Prefix leaves one
        // behind. This runs on the windows-latest CI job.
        let target = Path::new(r"D:\shared\lib");
        assert_eq!(
            path_for_manifest(Path::new(r"C:\work\repo"), target),
            target
        );
    }

    #[cfg(windows)]
    #[test]
    fn paths_on_the_same_windows_drive_are_relative() {
        // The positive counterpart: same drive must still relativise, or the
        // fallback above would be masking a total failure to compute anything.
        assert_eq!(
            path_for_manifest(Path::new(r"C:\work\repo"), Path::new(r"C:\work\shared-lib")),
            PathBuf::from("..").join("shared-lib")
        );
    }
}
