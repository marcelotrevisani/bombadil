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
/// works for paths that do not exist yet.
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
    fn paths_with_no_common_root_stay_absolute() {
        // On Windows this is a different drive letter; there is no relative
        // path between C:\ and D:\, so the manifest has to carry an absolute
        // one even though that is not portable.
        let target = Path::new("/elsewhere/lib");
        let got = path_for_manifest(Path::new("relative/project"), target);
        assert_eq!(got, target);
    }

    #[test]
    fn a_relative_target_is_returned_unchanged() {
        // Nothing sensible to compute against; hand it to uv as given.
        assert_eq!(
            path_for_manifest(Path::new("/work/repo"), Path::new("../lib")),
            PathBuf::from("../lib")
        );
    }
}
