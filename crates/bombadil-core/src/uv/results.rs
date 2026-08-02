use crate::error::ResultsError;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The `uv tree --format json` schema version this code was written against.
/// uv marks the schema unstable, so anything else degrades rather than fails.
pub const TESTED_TREE_SCHEMA: &str = "preview";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    /// Present when uv installed this package in editable mode, naming the
    /// directory it was installed from. Absent for ordinary dependencies.
    #[serde(default)]
    pub editable_project_location: Option<PathBuf>,
}

impl InstalledPackage {
    /// Whether this package is a dev-mode install rather than an ordinary
    /// dependency. Distinguishing the two is the point of the members list.
    pub fn is_editable(&self) -> bool {
        self.editable_project_location.is_some()
    }
}

pub fn parse_pip_list(json: &str) -> Result<Vec<InstalledPackage>, ResultsError> {
    serde_json::from_str(json).map_err(|source| ResultsError::Json {
        command: "uv pip list",
        source,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Interpreter {
    pub key: String,
    pub version: String,
    /// `None` when uv can download this interpreter but it is not installed.
    #[serde(default)]
    pub path: Option<PathBuf>,
    pub implementation: String,
}

impl Interpreter {
    /// An interpreter present on this machine, as opposed to one uv offers to
    /// download.
    pub fn is_installed(&self) -> bool {
        self.path.is_some()
    }
}

pub fn parse_python_list(json: &str) -> Result<Vec<Interpreter>, ResultsError> {
    serde_json::from_str(json).map_err(|source| ResultsError::Json {
        command: "uv python list",
        source,
    })
}

/// One node in the resolved dependency graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreePackage {
    pub id: String,
    pub name: String,
    pub version: String,
    /// Ids of this package's direct dependencies. Look each up in
    /// `DependencyGraph::packages`.
    pub dependencies: Vec<String>,
}

/// A workspace member, as uv enumerates them — globs already expanded.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TreeMember {
    pub name: String,
    pub path: PathBuf,
    pub id: String,
}

/// The resolved graph: entry points plus every node, keyed by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyGraph {
    pub roots: Vec<String>,
    pub members: Vec<TreeMember>,
    pub packages: BTreeMap<String, TreePackage>,
}

impl DependencyGraph {
    /// The direct dependencies of a package, resolved from ids to nodes.
    /// Ids with no entry in `packages` are skipped rather than erroring —
    /// uv's schema is `preview`, and a dangling id must not take out the view.
    ///
    /// One level deep and safe as it stands, but Python dependency graphs
    /// contain cycles: a caller that recurses through this must carry its own
    /// visited set to avoid looping forever.
    pub fn dependencies_of(&self, id: &str) -> Vec<&TreePackage> {
        self.packages
            .get(id)
            .map(|pkg| {
                pkg.dependencies
                    .iter()
                    .filter_map(|dep_id| self.packages.get(dep_id))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// What came back from `uv tree`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeOutcome {
    Parsed(DependencyGraph),
    /// uv produced a schema we have not been tested against. The caller shows
    /// direct dependencies only rather than guessing at an unknown shape.
    UnsupportedSchema {
        found: String,
    },
}

#[derive(Deserialize)]
struct RawSchema {
    /// Optional so that a schema object which is present but restructured by a
    /// future uv degrades like any other unrecognised schema, instead of
    /// failing deserialization of the whole document.
    #[serde(default)]
    version: Option<String>,
}

#[derive(Deserialize)]
struct RawRootRef {
    id: String,
}

#[derive(Deserialize)]
struct RawDependencyRef {
    id: String,
}

/// A `resolution` entry. `name`/`version` are optional because uv's
/// `resolution` map also carries a non-package pseudo-entry for the
/// workspace itself (`"kind": "workspace"`), which has neither. Such entries
/// are dropped when building `DependencyGraph::packages` rather than failing
/// the whole parse.
#[derive(Deserialize)]
struct RawPackage {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    dependencies: Vec<RawDependencyRef>,
}

#[derive(Deserialize)]
struct RawTree {
    #[serde(default)]
    schema: Option<RawSchema>,
    #[serde(default)]
    roots: Vec<RawRootRef>,
    #[serde(default)]
    members: Vec<TreeMember>,
    #[serde(default)]
    resolution: BTreeMap<String, RawPackage>,
}

pub fn parse_tree(json: &str) -> Result<TreeOutcome, ResultsError> {
    let raw: RawTree = serde_json::from_str(json).map_err(|source| ResultsError::Json {
        command: "uv tree",
        source,
    })?;

    match raw.schema {
        Some(schema) if schema.version.as_deref() == Some(TESTED_TREE_SCHEMA) => {
            let packages = raw
                .resolution
                .into_iter()
                .filter_map(|(id, pkg)| {
                    let name = pkg.name?;
                    let version = pkg.version?;
                    let dependencies = pkg.dependencies.into_iter().map(|d| d.id).collect();
                    Some((
                        id.clone(),
                        TreePackage {
                            id,
                            name,
                            version,
                            dependencies,
                        },
                    ))
                })
                .collect();

            Ok(TreeOutcome::Parsed(DependencyGraph {
                roots: raw.roots.into_iter().map(|r| r.id).collect(),
                members: raw.members,
                packages,
            }))
        }
        Some(RawSchema { version: Some(v) }) => Ok(TreeOutcome::UnsupportedSchema { found: v }),
        Some(RawSchema { version: None }) => Ok(TreeOutcome::UnsupportedSchema {
            found: "<no version field>".to_string(),
        }),
        None => Ok(TreeOutcome::UnsupportedSchema {
            found: "<absent>".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIP_LIST: &str = include_str!("../../tests/fixtures/uv-pip-list.json");
    const PYTHON_LIST: &str = include_str!("../../tests/fixtures/uv-python-list.json");
    const TREE: &str = include_str!("../../tests/fixtures/uv-tree.json");

    #[test]
    fn reads_installed_packages() {
        let got = parse_pip_list(PIP_LIST).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].name, "idna");
        assert_eq!(got[0].version, "3.18");
    }

    #[test]
    fn an_editable_install_reports_where_it_came_from() {
        // This is what distinguishes a dev-installed member from an ordinary
        // dependency that happens to share its name.
        let got = parse_pip_list(PIP_LIST).unwrap();

        let ordinary = got.iter().find(|p| p.name == "idna").unwrap();
        assert!(!ordinary.is_editable());
        assert_eq!(ordinary.editable_project_location, None);

        let editable = got.iter().find(|p| p.name == "beta").unwrap();
        assert!(editable.is_editable());
        assert_eq!(
            editable.editable_project_location.as_deref(),
            Some(std::path::Path::new("/work/repo/packages/beta"))
        );
    }

    #[test]
    fn an_empty_pip_list_is_not_an_error() {
        assert!(parse_pip_list("[]").unwrap().is_empty());
    }

    #[test]
    fn malformed_pip_list_json_is_an_error() {
        assert!(parse_pip_list("not json").is_err());
    }

    #[test]
    fn reads_interpreters_and_distinguishes_installed_from_downloadable() {
        let got = parse_python_list(PYTHON_LIST).unwrap();
        assert_eq!(got.len(), 2);

        // path: null means uv could download it but it is not on this machine.
        assert!(
            !got[0].is_installed(),
            "3.15.0b4 has no path and is not installed"
        );
        assert_eq!(got[0].version, "3.15.0b4");

        assert!(got[1].is_installed(), "3.12.4 has a path and is installed");
        assert_eq!(
            got[1].path.as_deref(),
            Some(std::path::Path::new("/usr/bin/python3.12"))
        );
        assert_eq!(got[1].implementation, "cpython");
    }

    fn parsed_graph() -> DependencyGraph {
        match parse_tree(TREE).unwrap() {
            TreeOutcome::Parsed(graph) => graph,
            other => panic!("expected Parsed, got {other:?}"),
        }
    }

    #[test]
    fn reads_the_tree_when_the_schema_is_the_one_we_tested() {
        let graph = parsed_graph();
        assert!(!graph.roots.is_empty());
        assert!(!graph.packages.is_empty());
    }

    #[test]
    fn dependencies_of_a_root_resolves_its_direct_dependencies_by_name() {
        let graph = parsed_graph();
        let root = &graph.roots[0];
        let names: Vec<&str> = graph
            .dependencies_of(root)
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, vec!["httpx"], "got {names:?}");
    }

    #[test]
    fn a_transitive_chain_can_be_walked_two_levels() {
        let graph = parsed_graph();
        let httpx = graph
            .packages
            .values()
            .find(|p| p.name == "httpx")
            .expect("httpx is in the fixture");
        let httpcore = graph
            .dependencies_of(&httpx.id)
            .into_iter()
            .find(|p| p.name == "httpcore")
            .expect("httpx depends on httpcore");
        let h11_present = graph
            .dependencies_of(&httpcore.id)
            .iter()
            .any(|p| p.name == "h11");
        assert!(h11_present, "httpcore should depend on h11");
    }

    #[test]
    fn dependencies_of_an_unknown_id_returns_empty_rather_than_panicking() {
        let graph = parsed_graph();
        assert!(graph.dependencies_of("does-not-exist@nowhere").is_empty());
    }

    #[test]
    fn an_unknown_tree_schema_degrades_instead_of_failing() {
        // uv declares this schema unstable. A future uv must not make the app
        // unusable -- the GUI falls back to direct dependencies only.
        let json = r#"{"schema":{"version":"v2"},"roots":[]}"#;
        match parse_tree(json).unwrap() {
            TreeOutcome::UnsupportedSchema { found } => assert_eq!(found, "v2"),
            other => panic!("expected UnsupportedSchema, got {other:?}"),
        }
    }

    #[test]
    fn a_tree_with_no_schema_field_degrades_too() {
        match parse_tree(r#"{"roots":[]}"#).unwrap() {
            TreeOutcome::UnsupportedSchema { found } => assert_eq!(found, "<absent>"),
            other => panic!("expected UnsupportedSchema, got {other:?}"),
        }
    }

    #[test]
    fn malformed_tree_json_is_an_error() {
        assert!(parse_tree("{{{").is_err());
    }

    #[test]
    fn a_schema_object_without_a_version_degrades_too() {
        // A future uv could restructure the schema object while still emitting the
        // key. That must degrade like any unknown schema, not kill the app.
        match parse_tree(r#"{"schema":{},"roots":[]}"#).unwrap() {
            TreeOutcome::UnsupportedSchema { found } => assert_eq!(found, "<no version field>"),
            other => panic!("expected UnsupportedSchema, got {other:?}"),
        }
    }

    #[test]
    fn valid_json_of_the_wrong_shape_is_an_error() {
        // Degrading is for unrecognised *schemas*, not for input that is not a
        // tree document at all.
        assert!(
            parse_tree(r#""string""#).is_err(),
            "string should not deserialize as tree"
        );
        assert!(
            parse_tree("123").is_err(),
            "number should not deserialize as tree"
        );
        assert!(
            parse_pip_list("true").is_err(),
            "boolean should not deserialize as pip list"
        );
        assert!(
            parse_pip_list("null").is_err(),
            "null should not deserialize as pip list"
        );
    }
}
