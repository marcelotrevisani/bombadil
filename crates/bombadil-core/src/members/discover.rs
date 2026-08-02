use super::{WorkspaceKind, glob_matches};
use std::path::{Path, PathBuf};

/// How far below the project directory discovery walks.
///
/// Deep enough for `services/api/` and one level of nesting beyond it,
/// shallow enough that a pathological tree cannot make Bombadil crawl a disk.
pub const MAX_DISCOVERY_DEPTH: usize = 4;

/// Directories never worth walking into: virtual environments, VCS metadata,
/// dependency caches and build output. A `pyproject.toml` inside any of these
/// belongs to something else.
pub const SKIPPED_DIRS: &[&str] = &[
    ".venv",
    "venv",
    ".git",
    "node_modules",
    "__pycache__",
    "build",
    "dist",
    "target",
    ".tox",
    ".mypy_cache",
];

/// One `pyproject.toml` found below a registered project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredMember {
    /// Directory holding the manifest, relative to the project directory.
    pub relative_path: PathBuf,
    /// `[project].name`, absent when the manifest declares no project.
    pub name: Option<String>,
    /// Whether the declared workspace globs cover this path. Always false for
    /// a loose project, which has no workspace to be a member of.
    pub is_workspace_member: bool,
}

/// Find the packages belonging to a registered project.
///
/// Never installs or modifies anything — the result is a list of candidates
/// for the user to confirm. A missing or unreadable directory yields an empty
/// list rather than an error: discovery is not the place to fail an app.
///
/// Results are sorted by path, because `read_dir` order is not stable and the
/// UI list would otherwise reshuffle between runs.
pub fn discover(project_dir: &Path, kind: &WorkspaceKind) -> Vec<DiscoveredMember> {
    let mut found = Vec::new();
    walk(project_dir, project_dir, 0, &mut found);
    found.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

    if let WorkspaceKind::Workspace { member_globs } = kind {
        for member in &mut found {
            let as_str = member.relative_path.to_string_lossy().replace('\\', "/");
            member.is_workspace_member = member_globs.iter().any(|g| glob_matches(g, &as_str));
        }
    }

    found
}

fn walk(root: &Path, dir: &Path, depth: usize, found: &mut Vec<DiscoveredMember>) {
    if depth > MAX_DISCOVERY_DEPTH {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if SKIPPED_DIRS.contains(&name.as_ref()) || name.starts_with('.') {
            continue;
        }

        let manifest = path.join("pyproject.toml");
        if manifest.is_file()
            && let Ok(relative) = path.strip_prefix(root)
        {
            let name = std::fs::read_to_string(&manifest)
                .ok()
                .and_then(|text| crate::pyproject::parse(&text).ok())
                .and_then(|parsed| parsed.name);
            found.push(DiscoveredMember {
                relative_path: relative.to_path_buf(),
                name,
                is_workspace_member: false,
            });
        }

        walk(root, &path, depth + 1, found);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tree of `pyproject.toml` files under a temp dir.
    fn tree(paths: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, name) in paths {
            let full = dir.path().join(rel);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(
                &full,
                format!("[project]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
            )
            .unwrap();
        }
        dir
    }

    #[test]
    fn finds_nested_packages_and_ignores_the_root_itself() {
        let dir = tree(&[
            ("pyproject.toml", "root"),
            ("packages/alpha/pyproject.toml", "alpha"),
            ("services/api/pyproject.toml", "api"),
        ]);

        let found = discover(dir.path(), &WorkspaceKind::Loose);

        let paths: Vec<_> = found.iter().map(|m| m.relative_path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                std::path::PathBuf::from("packages/alpha"),
                std::path::PathBuf::from("services/api"),
            ],
            "the root's own pyproject.toml is not a member of its own list, \
             and results must be sorted for a stable UI order"
        );
        assert_eq!(found[0].name.as_deref(), Some("alpha"));
    }

    #[test]
    fn skips_virtualenvs_and_build_output() {
        let dir = tree(&[
            ("packages/alpha/pyproject.toml", "alpha"),
            (".venv/lib/site-packages/evil/pyproject.toml", "evil"),
            ("node_modules/pkg/pyproject.toml", "nope"),
            ("build/tmp/pyproject.toml", "nope2"),
            ("target/debug/pyproject.toml", "nope3"),
        ]);

        let found = discover(dir.path(), &WorkspaceKind::Loose);

        assert_eq!(found.len(), 1, "got {found:?}");
        assert_eq!(found[0].name.as_deref(), Some("alpha"));
    }

    #[test]
    fn stops_at_the_depth_cap() {
        // Deeper than MAX_DISCOVERY_DEPTH so a pathological tree cannot make
        // discovery crawl a whole disk.
        let dir = tree(&[("a/b/c/d/e/f/pyproject.toml", "deep")]);
        assert!(discover(dir.path(), &WorkspaceKind::Loose).is_empty());
    }

    #[test]
    fn classifies_workspace_members_against_the_globs() {
        let dir = tree(&[
            ("packages/alpha/pyproject.toml", "alpha"),
            ("tools/helper/pyproject.toml", "helper"),
        ]);
        let kind = WorkspaceKind::Workspace {
            member_globs: vec!["packages/*".to_string()],
        };

        let found = discover(dir.path(), &kind);

        let alpha = found
            .iter()
            .find(|m| m.name.as_deref() == Some("alpha"))
            .unwrap();
        let helper = found
            .iter()
            .find(|m| m.name.as_deref() == Some("helper"))
            .unwrap();
        assert!(alpha.is_workspace_member);
        assert!(
            !helper.is_workspace_member,
            "a package outside the declared globs is present in the tree but is \
             not a workspace member, and uv sync --package would reject it"
        );
    }

    #[test]
    fn everything_is_a_member_when_the_project_is_loose() {
        let dir = tree(&[("packages/alpha/pyproject.toml", "alpha")]);
        let found = discover(dir.path(), &WorkspaceKind::Loose);
        assert!(!found[0].is_workspace_member);
    }

    #[test]
    fn a_manifest_without_a_project_name_is_still_reported() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("tooling");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join("pyproject.toml"),
            "[tool.black]\nline-length = 88\n",
        )
        .unwrap();

        let found = discover(dir.path(), &WorkspaceKind::Loose);

        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].name, None,
            "reported, but not selectable by --package"
        );
    }

    #[test]
    fn a_missing_directory_yields_nothing_rather_than_failing() {
        let found = discover(
            std::path::Path::new("/no/such/place/at/all"),
            &WorkspaceKind::Loose,
        );
        assert!(found.is_empty());
    }
}
