use crate::error::ResultsError;
use serde::Deserialize;
use std::path::PathBuf;

/// The `uv tree --format json` schema version this code was written against.
/// uv marks the schema unstable, so anything else degrades rather than fails.
pub const TESTED_TREE_SCHEMA: &str = "preview";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TreeRoot {
    pub id: String,
}

/// What came back from `uv tree`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeOutcome {
    Parsed(Vec<TreeRoot>),
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
struct RawTree {
    #[serde(default)]
    schema: Option<RawSchema>,
    #[serde(default)]
    roots: Vec<TreeRoot>,
}

pub fn parse_tree(json: &str) -> Result<TreeOutcome, ResultsError> {
    let raw: RawTree = serde_json::from_str(json).map_err(|source| ResultsError::Json {
        command: "uv tree",
        source,
    })?;

    match raw.schema {
        Some(schema) if schema.version.as_deref() == Some(TESTED_TREE_SCHEMA) => {
            Ok(TreeOutcome::Parsed(raw.roots))
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
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "idna");
        assert_eq!(got[0].version, "3.18");
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

    #[test]
    fn reads_the_tree_when_the_schema_is_the_one_we_tested() {
        match parse_tree(TREE).unwrap() {
            TreeOutcome::Parsed(roots) => {
                assert_eq!(roots.len(), 2);
                assert!(roots[0].id.starts_with("jsonprobe"));
            }
            other => panic!("expected Parsed, got {other:?}"),
        }
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
