use serde::Deserialize;

/// Whether a registered project is a uv workspace, and if so what it declares
/// as members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceKind {
    Workspace {
        member_globs: Vec<String>,
        exclude_globs: Vec<String>,
        /// The root's own `[project].name`; `None` for a virtual workspace.
        /// A member-aware sync has to name it, or uv leaves it out.
        root_project: Option<String>,
    },
    Loose,
}

#[derive(Deserialize)]
struct RawWorkspace {
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

#[derive(Deserialize)]
struct RawUv {
    workspace: Option<RawWorkspace>,
}

#[derive(Deserialize)]
struct RawTool {
    uv: Option<RawUv>,
}

#[derive(Deserialize)]
struct RawProject {
    name: Option<String>,
}

#[derive(Deserialize)]
struct RawRoot {
    tool: Option<RawTool>,
    project: Option<RawProject>,
}

/// Classify a root `pyproject.toml`.
///
/// Unparseable input is reported as `Loose` rather than an error: discovery
/// must never be the thing that makes the app unusable, and a malformed
/// manifest already surfaces through `pyproject::load`.
pub fn parse_workspace(pyproject_text: &str) -> WorkspaceKind {
    let raw: RawRoot = match toml::from_str(pyproject_text) {
        Ok(raw) => raw,
        Err(_) => return WorkspaceKind::Loose,
    };

    let root_project = raw.project.and_then(|p| p.name);
    match raw.tool.and_then(|t| t.uv).and_then(|u| u.workspace) {
        Some(ws) => WorkspaceKind::Workspace {
            member_globs: ws.members,
            exclude_globs: ws.exclude,
            root_project,
        },
        None => WorkspaceKind::Loose,
    }
}

/// Match a uv workspace member pattern against a candidate path.
///
/// Handles the only forms uv workspaces use in practice: a literal path, a
/// trailing `*` matching exactly one segment, and `**` matching any depth.
/// Both arguments are forward-slash-joined relative paths, so this is
/// platform-independent.
pub fn glob_matches(pattern: &str, candidate: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return candidate
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1);
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return match candidate.strip_prefix(prefix) {
            Some(rest) => rest.starts_with('/') && rest.len() > 1 && !rest[1..].contains('/'),
            None => false,
        };
    }
    pattern == candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_that_is_a_project_is_named() {
        let kind = parse_workspace(
            "[project]\nname = \"pipeline\"\n\n[tool.uv.workspace]\nmembers = [\"packages/*\"]\n",
        );
        assert!(matches!(
            kind,
            WorkspaceKind::Workspace { root_project: Some(ref n), .. } if n == "pipeline"
        ));
        let virtual_root = parse_workspace("[tool.uv.workspace]\nmembers = [\"packages/*\"]\n");
        assert!(matches!(
            virtual_root,
            WorkspaceKind::Workspace {
                root_project: None,
                ..
            }
        ));
    }

    #[test]
    fn a_workspace_root_reports_its_member_globs() {
        let text = r#"
[project]
name = "root"
version = "0.1.0"

[tool.uv.workspace]
members = ["packages/*", "services/*"]
"#;
        match parse_workspace(text) {
            WorkspaceKind::Workspace { member_globs, .. } => {
                assert_eq!(member_globs, vec!["packages/*", "services/*"]);
            }
            other => panic!("expected Workspace, got {other:?}"),
        }
    }

    #[test]
    fn a_workspace_root_reports_its_exclude_globs() {
        let text = r#"
[project]
name = "root"
version = "0.1.0"

[tool.uv.workspace]
members = ["packages/*"]
exclude = ["packages/skipme"]
"#;
        match parse_workspace(text) {
            WorkspaceKind::Workspace { exclude_globs, .. } => {
                assert_eq!(exclude_globs, vec!["packages/skipme"]);
            }
            other => panic!("expected Workspace, got {other:?}"),
        }
    }

    #[test]
    fn a_plain_project_is_loose() {
        let text = "[project]\nname = \"solo\"\nversion = \"0.1.0\"\n";
        assert_eq!(parse_workspace(text), WorkspaceKind::Loose);
    }

    #[test]
    fn a_workspace_table_with_no_members_key_is_still_a_workspace() {
        // uv allows an empty workspace table; it is a workspace with no globs,
        // not a loose project, and treating it as loose would make Bombadil
        // start adding editable path dependencies to a real workspace.
        let text = "[project]\nname = \"root\"\nversion = \"0.1.0\"\n\n[tool.uv.workspace]\n";
        match parse_workspace(text) {
            WorkspaceKind::Workspace { member_globs, .. } => assert!(member_globs.is_empty()),
            other => panic!("expected Workspace, got {other:?}"),
        }
    }

    #[test]
    fn unparseable_toml_is_treated_as_loose() {
        // Discovery must never be the thing that makes the app unusable; a
        // malformed root manifest surfaces elsewhere, through pyproject::load.
        assert_eq!(parse_workspace("{{{ not toml"), WorkspaceKind::Loose);
    }

    #[test]
    fn a_tool_uv_table_without_a_workspace_key_is_loose() {
        // The common shape for a plain project: `[tool.uv]` present (e.g. to
        // set `package = false`) but no `[tool.uv.workspace]` table. If this
        // regressed, every non-workspace project would be misclassified as a
        // workspace.
        let text =
            "[project]\nname = \"solo\"\nversion = \"0.1.0\"\n\n[tool.uv]\npackage = false\n";
        assert_eq!(parse_workspace(text), WorkspaceKind::Loose);
    }

    #[test]
    fn a_single_star_matches_one_segment_only() {
        assert!(glob_matches("packages/*", "packages/alpha"));
        assert!(!glob_matches("packages/*", "packages/alpha/nested"));
        assert!(!glob_matches("packages/*", "services/alpha"));
        assert!(!glob_matches("packages/*", "packages"));
    }

    #[test]
    fn a_double_star_matches_any_depth() {
        assert!(glob_matches("packages/**", "packages/alpha"));
        assert!(glob_matches("packages/**", "packages/alpha/nested/deep"));
        assert!(!glob_matches("packages/**", "services/alpha"));
    }

    #[test]
    fn a_literal_pattern_matches_exactly() {
        assert!(glob_matches("packages/alpha", "packages/alpha"));
        assert!(!glob_matches("packages/alpha", "packages/beta"));
        assert!(!glob_matches("packages/alpha", "packages/alpha/nested"));
    }
}
