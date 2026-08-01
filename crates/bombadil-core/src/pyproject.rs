use crate::error::PyProjectError;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// One entry in a PEP 735 dependency group: either a requirement string or a
/// reference to another group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupEntry {
    Requirement(String),
    IncludeGroup(String),
}

/// The parts of `pyproject.toml` Bombadil displays. Everything else is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PyProject {
    pub name: Option<String>,
    pub requires_python: Option<String>,
    pub dependencies: Vec<String>,
    pub optional_dependencies: BTreeMap<String, Vec<String>>,
    pub dependency_groups: BTreeMap<String, Vec<GroupEntry>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawGroupEntry {
    Requirement(String),
    IncludeGroup {
        #[serde(rename = "include-group")]
        include_group: String,
    },
}

#[derive(Deserialize, Default)]
struct RawProject {
    name: Option<String>,
    #[serde(rename = "requires-python")]
    requires_python: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default, rename = "optional-dependencies")]
    optional_dependencies: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize, Default)]
struct RawRoot {
    #[serde(default)]
    project: RawProject,
    #[serde(default, rename = "dependency-groups")]
    dependency_groups: BTreeMap<String, Vec<RawGroupEntry>>,
}

/// Parse the contents of a `pyproject.toml`.
pub fn parse(text: &str) -> Result<PyProject, PyProjectError> {
    let raw: RawRoot = toml::from_str(text).map_err(|source| PyProjectError::Parse { source })?;

    let dependency_groups = raw
        .dependency_groups
        .into_iter()
        .map(|(group, entries)| {
            let entries = entries
                .into_iter()
                .map(|e| match e {
                    RawGroupEntry::Requirement(r) => GroupEntry::Requirement(r),
                    RawGroupEntry::IncludeGroup { include_group } => {
                        GroupEntry::IncludeGroup(include_group)
                    }
                })
                .collect();
            (group, entries)
        })
        .collect();

    Ok(PyProject {
        name: raw.project.name,
        requires_python: raw.project.requires_python,
        dependencies: raw.project.dependencies,
        optional_dependencies: raw.project.optional_dependencies,
        dependency_groups,
    })
}

/// Read and parse a `pyproject.toml` from disk.
pub fn load(path: &Path) -> Result<PyProject, PyProjectError> {
    let text = std::fs::read_to_string(path).map_err(|source| PyProjectError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&text).map_err(|err| match err {
        PyProjectError::Parse { source } => PyProjectError::ParseAt {
            path: path.to_path_buf(),
            source,
        },
        // parse() only ever returns Parse; keep other variants as-is so this
        // stays correct if that ever changes.
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/realistic_pyproject.toml");

    #[test]
    fn reads_name_and_requires_python() {
        let p = parse(FIXTURE).unwrap();
        assert_eq!(p.name.as_deref(), Some("my-api"));
        assert_eq!(p.requires_python.as_deref(), Some(">=3.12"));
    }

    #[test]
    fn reads_direct_dependencies_in_declaration_order() {
        let p = parse(FIXTURE).unwrap();
        assert_eq!(p.dependencies, vec!["fastapi>=0.115", "httpx~=0.27"]);
    }

    #[test]
    fn reads_optional_dependency_extras() {
        let p = parse(FIXTURE).unwrap();
        assert_eq!(p.optional_dependencies["cli"], vec!["typer"]);
        assert_eq!(
            p.optional_dependencies["postgres"],
            vec!["asyncpg>=0.29", "sqlalchemy>=2"]
        );
    }

    #[test]
    fn reads_pep735_groups_including_group_inclusion() {
        let p = parse(FIXTURE).unwrap();
        assert_eq!(
            p.dependency_groups["test"],
            vec![
                GroupEntry::Requirement("pytest>=8".into()),
                GroupEntry::Requirement("pytest-cov".into()),
            ]
        );
        assert_eq!(
            p.dependency_groups["dev"],
            vec![
                GroupEntry::IncludeGroup("test".into()),
                GroupEntry::Requirement("ruff".into()),
            ]
        );
    }

    #[test]
    fn an_almost_empty_file_is_not_an_error() {
        let p = parse("[project]\nname = \"x\"\n").unwrap();
        assert_eq!(p.name.as_deref(), Some("x"));
        assert!(p.dependencies.is_empty());
        assert!(p.requires_python.is_none());
    }

    #[test]
    fn a_file_with_no_project_table_still_parses() {
        // Valid for a uv workspace root that only carries [tool.uv].
        let p = parse("[tool.uv]\npackage = false\n").unwrap();
        assert!(p.name.is_none());
    }

    #[test]
    fn invalid_toml_is_an_error_naming_the_problem() {
        let err = parse("[project\nname =").unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn load_reads_and_parses_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pyproject.toml");
        std::fs::write(&path, FIXTURE).unwrap();

        let p = load(&path).unwrap();
        assert_eq!(p.name.as_deref(), Some("my-api"));
    }

    #[test]
    fn load_on_a_missing_file_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");

        let err = load(&path).unwrap_err();
        match err {
            PyProjectError::Io { path: p, .. } => assert_eq!(p, path),
            other => panic!("expected Io, got {other:?}"),
        }
    }

    #[test]
    fn load_on_a_malformed_file_names_the_path_that_failed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pyproject.toml");
        std::fs::write(&path, "[project\nname =").unwrap();

        let err = load(&path).unwrap_err();
        assert!(
            err.to_string()
                .contains(&path.to_string_lossy().to_string()),
            "got {err}"
        );
    }
}
