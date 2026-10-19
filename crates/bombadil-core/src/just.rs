//! A project's `just` recipes: found by asking `just` itself, run like any
//! other command on the Run tab.

use crate::model::JustSource;
use crate::process::CommandSpec;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub default: Option<String>,
    /// `*rest`: any number of words, none required.
    pub optional_many: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    pub name: String,
    /// The comment above the recipe.
    pub doc: Option<String>,
    pub params: Vec<Param>,
}

#[derive(Deserialize)]
struct RawParam {
    name: String,
    #[serde(default)]
    default: Option<serde_json::Value>,
    #[serde(default)]
    kind: String,
}

#[derive(Deserialize)]
struct RawRecipe {
    name: String,
    #[serde(default)]
    doc: Option<String>,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    parameters: Vec<RawParam>,
}

#[derive(Deserialize)]
struct RawDump {
    #[serde(default)]
    recipes: BTreeMap<String, RawRecipe>,
}

/// The public recipes in `just --dump --dump-format json`, by name.
pub fn parse_dump(json: &str) -> Result<Vec<Recipe>, String> {
    let dump: RawDump = serde_json::from_str(json)
        .map_err(|e| format!("just's recipe list was unreadable: {e}"))?;
    Ok(dump
        .recipes
        .into_values()
        .filter(|r| !r.private)
        .map(|r| Recipe {
            name: r.name,
            doc: r.doc.filter(|d| !d.trim().is_empty()),
            params: r
                .parameters
                .into_iter()
                .map(|p| Param {
                    name: p.name,
                    // A default is an expression; a plain string is shown as
                    // it is, anything else as just wrote it.
                    default: p.default.and_then(|v| match v {
                        serde_json::Value::Null => None,
                        serde_json::Value::String(s) => Some(s),
                        other => Some(other.to_string()),
                    }),
                    optional_many: p.kind == "star",
                })
                .collect(),
        })
        .collect())
}

/// Why `args` can't run `recipe` yet, naming the parameters still needed.
pub fn missing_args(recipe: &Recipe, args: &[String]) -> Option<String> {
    let needed: Vec<&str> = recipe
        .params
        .iter()
        .enumerate()
        .filter(|(i, p)| p.default.is_none() && !p.optional_many && *i >= args.len())
        .map(|(_, p)| p.name.as_str())
        .collect();
    (!needed.is_empty()).then(|| format!("Needs: {}", needed.join(", ")))
}

/// The justfile in `dir`, under any of the names just accepts.
pub fn find_justfile(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.is_file()
                && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    matches!(n.to_ascii_lowercase().as_str(), "justfile" | ".justfile")
                })
        })
}

/// The `just` that will run, and whether it is the bundled copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub bundled: bool,
}

/// Which `just` to run: `on_path` is what a PATH lookup found, `bundled`
/// extracts the embedded copy (only called when it is the answer).
/// `Ok(None)` means `FromPath` and there is none.
pub fn resolve(
    source: JustSource,
    on_path: Option<PathBuf>,
    bundled: &dyn Fn() -> Result<PathBuf, String>,
) -> Result<Option<Found>, String> {
    match (source, on_path) {
        (JustSource::Auto | JustSource::FromPath, Some(path)) => Ok(Some(Found {
            path,
            bundled: false,
        })),
        (JustSource::FromPath, None) => Ok(None),
        (JustSource::Auto | JustSource::Bundled, _) => bundled().map(|path| {
            Some(Found {
                path,
                bundled: true,
            })
        }),
    }
}

/// `just --dump --dump-format json` in `dir`.
pub fn dump(just: &Path, dir: &Path) -> CommandSpec {
    CommandSpec::new(just)
        .cwd(dir)
        .args(["--dump", "--dump-format", "json"])
}

/// `just --show <recipe>` in `dir`: the recipe's source.
pub fn show(just: &Path, dir: &Path, recipe: &str) -> CommandSpec {
    CommandSpec::new(just).cwd(dir).args(["--show", recipe])
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = r#"{"recipes":{
      "build":{"name":"build","doc":"Build the whole workspace.","private":false,"parameters":[]},
      "greet":{"name":"greet","doc":null,"private":false,"parameters":[
         {"name":"name","default":"world","kind":"singular"},
         {"name":"who","default":null,"kind":"singular"},
         {"name":"rest","default":null,"kind":"star"}]},
      "_hidden":{"name":"_hidden","doc":null,"private":true,"parameters":[]}
    },"modules":{}}"#;

    #[test]
    fn recipes_come_from_the_dump_without_private_ones() {
        let recipes = parse_dump(DUMP).unwrap();
        let names: Vec<&str> = recipes.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["build", "greet"]);
        assert_eq!(
            recipes[0].doc.as_deref(),
            Some("Build the whole workspace.")
        );
        assert_eq!(recipes[1].params[0].default.as_deref(), Some("world"));
        assert!(parse_dump("not json").is_err());
    }

    #[test]
    fn required_parameters_must_be_given() {
        let greet = parse_dump(DUMP).unwrap().remove(1);
        // `name` has a default, `rest` is variadic and optional: only `who`
        // is required, and it comes second, so two words are needed.
        assert_eq!(missing_args(&greet, &[]).as_deref(), Some("Needs: who"));
        assert_eq!(
            missing_args(&greet, &["Ana".into()]).as_deref(),
            Some("Needs: who")
        );
        assert_eq!(missing_args(&greet, &["Ana".into(), "Bo".into()]), None);
        let build = parse_dump(DUMP).unwrap().remove(0);
        assert_eq!(missing_args(&build, &[]), None);
    }

    #[test]
    fn the_justfile_is_found_whatever_its_case() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_justfile(dir.path()), None);
        std::fs::write(dir.path().join("Justfile"), "x:\n  echo\n").unwrap();
        assert_eq!(find_justfile(dir.path()), Some(dir.path().join("Justfile")));
        let hidden = tempfile::tempdir().unwrap();
        std::fs::write(hidden.path().join(".justfile"), "x:\n  echo\n").unwrap();
        assert_eq!(
            find_justfile(hidden.path()),
            Some(hidden.path().join(".justfile"))
        );
    }

    #[test]
    fn commands_run_in_the_project_folder() {
        let dir = std::path::Path::new("/p/api");
        let just = std::path::Path::new("/tools/just");
        let dump = dump(just, dir);
        assert_eq!(dump.program, just);
        assert_eq!(dump.args, ["--dump", "--dump-format", "json"]);
        assert_eq!(dump.cwd.as_deref(), Some(dir));
        let show = show(just, dir, "build");
        assert_eq!(show.program, just);
        assert_eq!(show.args, ["--show", "build"]);
    }

    fn bundled() -> Result<PathBuf, String> {
        Ok(PathBuf::from("bundled-just"))
    }

    fn mine() -> Found {
        Found {
            path: PathBuf::from("my-just"),
            bundled: false,
        }
    }

    fn ours() -> Found {
        Found {
            path: PathBuf::from("bundled-just"),
            bundled: true,
        }
    }

    #[test]
    fn auto_prefers_just_on_path_then_the_bundled_one() {
        let on_path = Some(PathBuf::from("my-just"));
        assert_eq!(
            resolve(JustSource::Auto, on_path, &bundled),
            Ok(Some(mine()))
        );
        assert_eq!(resolve(JustSource::Auto, None, &bundled), Ok(Some(ours())));
    }

    #[test]
    fn bundled_ignores_path_and_from_path_never_falls_back() {
        let on_path = Some(PathBuf::from("my-just"));
        assert_eq!(
            resolve(JustSource::Bundled, on_path, &bundled),
            Ok(Some(ours()))
        );
        assert_eq!(resolve(JustSource::FromPath, None, &bundled), Ok(None));
    }
}
