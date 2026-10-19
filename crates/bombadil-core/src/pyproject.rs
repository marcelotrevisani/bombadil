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

/// The names under `[project.scripts]`, sorted: what `uv run <name>` runs.
/// Empty when the file has none or does not parse.
pub fn entry_points(text: &str) -> Vec<String> {
    #[derive(Deserialize, Default)]
    struct Project {
        #[serde(default)]
        scripts: BTreeMap<String, String>,
    }
    #[derive(Deserialize, Default)]
    struct Root {
        #[serde(default)]
        project: Project,
    }
    toml::from_str::<Root>(text)
        .map(|root| root.project.scripts.into_keys().collect())
        .unwrap_or_default()
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

/// `text` with `[project] requires-python` set to `spec`; every other byte
/// kept. Errors when the file does not parse or has no `[project]` table.
pub fn set_requires_python(text: &str, spec: &str) -> Result<String, PyProjectError> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| PyProjectError::Edit(e.to_string()))?;
    let project = doc
        .get_mut("project")
        .and_then(|p| p.as_table_mut())
        .ok_or_else(|| PyProjectError::Edit("there is no [project] table".into()))?;
    match project
        .get_mut("requires-python")
        .and_then(|v| v.as_value_mut())
    {
        Some(value) => {
            // Without the old decor the space before a trailing comment goes.
            let decor = value.decor().clone();
            *value = toml_edit::Value::from(spec);
            *value.decor_mut() = decor;
        }
        None => {
            project.insert("requires-python", toml_edit::value(spec));
        }
    }
    Ok(doc.to_string())
}

/// `text` with every requirement on `package` stripped of its version
/// specifier, keeping its extras, markers and comments; `None` when no
/// dependency list declares it. Looks in `[project] dependencies`,
/// `[project.optional-dependencies]` and `[dependency-groups]`.
pub fn loosen_requirement(text: &str, package: &str) -> Result<Option<String>, PyProjectError> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| PyProjectError::Edit(e.to_string()))?;
    let key = normalised(package);
    let mut changed = false;
    let mut loosen = |list: &mut toml_edit::Item| {
        let Some(array) = list.as_array_mut() else {
            return;
        };
        for value in array.iter_mut() {
            let Some(requirement) = value.as_str() else {
                continue;
            };
            let (name, rest) = split_name(requirement.trim());
            if normalised(name) != key {
                continue;
            }
            let loose = loosened(name, rest);
            let decor = value.decor().clone();
            *value = toml_edit::Value::from(loose);
            *value.decor_mut() = decor;
            changed = true;
        }
    };
    if let Some(project) = doc.get_mut("project").and_then(|p| p.as_table_like_mut()) {
        if let Some(list) = project.get_mut("dependencies") {
            loosen(list);
        }
        if let Some(extras) = project
            .get_mut("optional-dependencies")
            .and_then(|e| e.as_table_like_mut())
        {
            for (_, list) in extras.iter_mut() {
                loosen(list);
            }
        }
    }
    if let Some(groups) = doc
        .get_mut("dependency-groups")
        .and_then(|g| g.as_table_like_mut())
    {
        for (_, list) in groups.iter_mut() {
            loosen(list);
        }
    }
    Ok(changed.then(|| doc.to_string()))
}

fn normalised(name: &str) -> String {
    name.to_lowercase().replace(['_', '.'], "-")
}

/// A requirement's leading name and everything after it.
fn split_name(requirement: &str) -> (&str, &str) {
    let end = requirement
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .unwrap_or(requirement.len());
    requirement.split_at(end)
}

/// `name` with `rest`'s extras and markers but no version specifier. A
/// direct reference (`name @ url`) is left as it is.
fn loosened(name: &str, rest: &str) -> String {
    let (head, markers) = match rest.split_once(';') {
        Some((head, markers)) => (head, Some(markers.trim())),
        None => (rest, None),
    };
    if head.trim_start().starts_with('@') {
        return format!("{name}{rest}");
    }
    let extras = head
        .trim_start()
        .strip_prefix('[')
        .and_then(|e| e.split_once(']'))
        .map(|(e, _)| format!("[{e}]"))
        .unwrap_or_default();
    match markers {
        Some(markers) => format!("{name}{extras}; {markers}"),
        None => format!("{name}{extras}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_points_are_the_script_names() {
        let text =
            "[project]\nname = \"a\"\n\n[project.scripts]\nzeta = \"a:z\"\nalpha = \"a:main\"\n";
        assert_eq!(entry_points(text), ["alpha", "zeta"]);
        assert!(entry_points("[project]\nname = \"a\"\n").is_empty());
        assert!(entry_points("not toml [").is_empty());
    }

    #[test]
    fn raising_requires_python_keeps_the_rest_of_the_file() {
        let text = "# my project\n[project]\nname = \"x\"  # the name\nrequires-python = \">=3.8\"  # min\ndependencies = [\n  \"six\",  # pinned later\n]\n\n[tool.uv]\ndev-dependencies = []\n";
        let out = set_requires_python(text, ">=3.11").unwrap();
        assert_eq!(out, text.replace("\">=3.8\"", "\">=3.11\""));
    }

    #[test]
    fn loosening_drops_only_the_version_specifier() {
        let text = "[project]\nname = \"x\"\ndependencies = [\n  \"Six==9.9\",  # pinned\n  \"uvicorn[standard]>=0.3; sys_platform != 'win32'\",\n  \"httpx\",\n]\n";
        assert_eq!(
            loosen_requirement(text, "six").unwrap().as_deref(),
            Some(text.replace("Six==9.9", "Six").as_str())
        );
        assert_eq!(
            loosen_requirement(text, "uvicorn").unwrap().as_deref(),
            Some(
                text.replace(">=0.3; sys_platform", "; sys_platform")
                    .as_str()
            )
        );
    }

    #[test]
    fn loosening_finds_groups_and_extras() {
        let text = "[project]\nname = \"x\"\n\n[project.optional-dependencies]\nweb = [\"six_x>=2\"]\n\n[dependency-groups]\ndev = [\"pytest==9.9\"]\n";
        assert_eq!(
            loosen_requirement(text, "six.x").unwrap().as_deref(),
            Some(text.replace("six_x>=2", "six_x").as_str())
        );
        assert_eq!(
            loosen_requirement(text, "pytest").unwrap().as_deref(),
            Some(text.replace("pytest==9.9", "pytest").as_str())
        );
    }

    #[test]
    fn an_undeclared_package_is_not_loosened() {
        let text = "[project]\nname = \"x\"\ndependencies = [\"six\"]\n";
        assert_eq!(loosen_requirement(text, "numpy").unwrap(), None);
        assert!(loosen_requirement("not toml [", "six").is_err());
    }

    #[test]
    fn requires_python_is_added_when_missing() {
        let out = set_requires_python("[project]\nname = \"x\"\n", ">=3.11").unwrap();
        assert!(out.contains("requires-python = \">=3.11\""), "{out}");
        assert!(out.starts_with("[project]\nname = \"x\"\n"));
    }

    #[test]
    fn without_a_project_table_nothing_is_written() {
        assert!(set_requires_python("[tool.uv]\n", ">=3.11").is_err());
        assert!(set_requires_python("not toml [", ">=3.11").is_err());
    }

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
