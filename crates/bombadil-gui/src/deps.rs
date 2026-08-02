//! The Dependencies tab: declared constraints joined with what is actually
//! installed, grouped the way `pyproject.toml` groups them.
//!
//! Spec §8: rows grouped by `main`, each dependency group, and each
//! optional-dependencies extra. Columns: package, declared constraint,
//! installed version. Direct dependencies expand to reveal their transitive
//! tree.
//!
//! Everything here is pure and takes already-parsed data as parameters --
//! `pyproject::PyProject`, `Vec<InstalledPackage>`, `TreeOutcome` -- so it is
//! testable without touching a keychain, a process or the filesystem. `app.rs`
//! is what fetches those off the render thread, through `job::run`.

use bombadil_core::pyproject::{GroupEntry, PyProject};
use bombadil_core::uv::results::{DependencyGraph, InstalledPackage, TreeOutcome, TreePackage};
use std::collections::BTreeSet;

/// Which bucket a row belongs to. `Main` is `[project.dependencies]`; `Group`
/// is a PEP 735 entry under `[dependency-groups]`; `Extra` is an entry under
/// `[project.optional-dependencies]`. Kept distinct rather than flattened --
/// spec §8 groups rows this way specifically so the tab shows which
/// dependencies come from where, and collapsing them into one list would
/// destroy exactly that information (criterion 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupKind {
    Main,
    Group(String),
    Extra(String),
}

/// One row: a package name, its declared constraint (if declared), and its
/// installed version (if installed). At least one of `constraint`/`version`
/// is always `Some` -- a row with neither would describe nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepRow {
    pub name: String,
    /// The version specifier text as written (e.g. `">=0.115"`), or `None`
    /// for an unconstrained requirement (`"typer"`) *and* for a package that
    /// is installed but not declared in this group at all.
    pub constraint: Option<String>,
    /// The installed version, or `None` when the package is declared but not
    /// installed -- exactly the packages a user needs to sync (criterion 1).
    pub version: Option<String>,
    /// False only for a leftover: installed, but not declared anywhere in
    /// `main`, any group or any extra (criterion 2).
    pub declared: bool,
}

/// One group's rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepGroup {
    pub kind: GroupKind,
    pub rows: Vec<DepRow>,
}

/// Everything the Dependencies tab shows for one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepsTab {
    pub groups: Vec<DepGroup>,
    /// The resolved graph, when `uv tree` produced one this build understands
    /// -- `transitive_dependencies` below walks it to expand a direct
    /// dependency's row. `None` both when there is no tree data at all (no
    /// venv yet, the command failed) and when uv emitted a schema this build
    /// was not tested against; either way the tab still shows every row
    /// above, direct dependencies only (criterion 4).
    pub graph: Option<DependencyGraph>,
    /// Set only for `TreeOutcome::UnsupportedSchema`, so the tab can say why
    /// the transitive tree is unavailable rather than rendering an
    /// unexplained empty expand affordance (criterion 4). `None` for the
    /// ordinary "no tree fetched yet" case, which needs no explanation.
    pub tree_note: Option<String>,
}

/// Splits a PEP 508 requirement into its package name and declared
/// constraint text, dropping any environment marker (after `;`) and any
/// extras (`[...]`) -- neither belongs in the constraint column.
///
/// This is *not* a second `pyproject.toml` parser: `pyproject::parse`
/// already turned the manifest into `Vec<String>` requirement strings: this
/// only splits one requirement string into its two display columns.
fn split_requirement(requirement: &str) -> (String, Option<String>) {
    let before_marker = requirement.split(';').next().unwrap_or(requirement).trim();
    let name_end = before_marker
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .unwrap_or(before_marker.len());
    let name = before_marker[..name_end].to_string();
    let rest = before_marker[name_end..].trim();
    let rest = match rest.strip_prefix('[') {
        Some(after_bracket) => match after_bracket.find(']') {
            Some(end) => after_bracket[end + 1..].trim(),
            None => "",
        },
        None => rest,
    };
    let constraint = if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    };
    (name, constraint)
}

/// Joins one group's declared requirement strings against what is installed,
/// by name (criterion 1). Matching is case-insensitive: PyPI package names
/// are, by PEP 503, case-insensitive and `pip`/`uv` normalise them.
fn join_rows(requirements: &[String], installed: &[InstalledPackage]) -> Vec<DepRow> {
    requirements
        .iter()
        .map(|requirement| {
            let (name, constraint) = split_requirement(requirement);
            let version = installed
                .iter()
                .find(|pkg| pkg.name.eq_ignore_ascii_case(&name))
                .map(|pkg| pkg.version.clone());
            DepRow {
                name,
                constraint,
                version,
                declared: true,
            }
        })
        .collect()
}

/// Every requirement string a PEP 735 group carries, including those pulled
/// in through `include-group` -- resolved transitively so a group that only
/// re-exports another one still shows that group's packages (spec's
/// "package in two groups appears under both" reads the other way too: a
/// group that includes another must show what it includes).
///
/// `visited` guards against a cycle: a `pyproject.toml` where two groups
/// include each other is invalid, but this must degrade rather than loop
/// forever building the tab.
fn resolve_group<'a>(
    pyproject: &'a PyProject,
    group: &'a str,
    visited: &mut BTreeSet<&'a str>,
) -> Vec<String> {
    let mut out = Vec::new();
    if !visited.insert(group) {
        return out;
    }
    let Some(entries) = pyproject.dependency_groups.get(group) else {
        return out;
    };
    for entry in entries {
        match entry {
            GroupEntry::Requirement(requirement) => out.push(requirement.clone()),
            GroupEntry::IncludeGroup(other) => out.extend(resolve_group(pyproject, other, visited)),
        }
    }
    out
}

/// Builds the Dependencies tab from a parsed manifest, what is installed, and
/// (if fetched) the resolved tree.
///
/// `tree` is `None` when no tree data has been fetched at all -- no venv yet,
/// or the `uv tree` process itself failed to run -- as opposed to
/// `Some(TreeOutcome::UnsupportedSchema { .. })`, which means uv ran and
/// answered with a schema this build does not understand (criterion 4). Both
/// leave `graph` empty, but only the second sets `tree_note`: an absent venv
/// needs no explanation, an unrecognised schema does.
pub fn build(
    pyproject: &PyProject,
    installed: &[InstalledPackage],
    tree: Option<TreeOutcome>,
) -> DepsTab {
    let mut declared_anywhere: BTreeSet<String> = BTreeSet::new();
    let mut groups = Vec::new();

    let main_rows = join_rows(&pyproject.dependencies, installed);
    for row in &main_rows {
        declared_anywhere.insert(row.name.to_ascii_lowercase());
    }

    for name in pyproject.dependency_groups.keys() {
        let mut visited = BTreeSet::new();
        let requirements = resolve_group(pyproject, name, &mut visited);
        let rows = join_rows(&requirements, installed);
        for row in &rows {
            declared_anywhere.insert(row.name.to_ascii_lowercase());
        }
        groups.push(DepGroup {
            kind: GroupKind::Group(name.clone()),
            rows,
        });
    }

    for (name, requirements) in &pyproject.optional_dependencies {
        let rows = join_rows(requirements, installed);
        for row in &rows {
            declared_anywhere.insert(row.name.to_ascii_lowercase());
        }
        groups.push(DepGroup {
            kind: GroupKind::Extra(name.clone()),
            rows,
        });
    }

    // Leftovers: installed but not declared under main, any group or any
    // extra (criterion 2). They live under `main` -- there is nowhere else
    // in a PEP 735/optional-dependencies model for an undeclared package to
    // belong, and `main` is what a bare `uv sync` would otherwise leave
    // untouched, which is exactly what a leftover or a transitive-only
    // install is.
    let mut main_rows = main_rows;
    for pkg in installed {
        if !declared_anywhere.contains(&pkg.name.to_ascii_lowercase()) {
            main_rows.push(DepRow {
                name: pkg.name.clone(),
                constraint: None,
                version: Some(pkg.version.clone()),
                declared: false,
            });
        }
    }

    groups.insert(
        0,
        DepGroup {
            kind: GroupKind::Main,
            rows: main_rows,
        },
    );

    let (graph, tree_note) = match tree {
        None => (None, None),
        Some(TreeOutcome::Parsed(graph)) => (Some(graph), None),
        Some(TreeOutcome::UnsupportedSchema { found }) => (
            None,
            Some(format!(
                "uv returned a dependency tree in an unrecognised format ({found}); showing direct dependencies only."
            )),
        ),
    };

    DepsTab {
        groups,
        graph,
        tree_note,
    }
}

/// Every package transitively reachable from `start_id` in `graph`, each
/// appearing once even under a diamond dependency. This is what a direct
/// dependency's row expands to (spec §8).
///
/// Walks `DependencyGraph::dependencies_of` with its own visited set, per
/// that method's own warning: Python dependency graphs contain cycles.
pub fn transitive_dependencies<'a>(
    graph: &'a DependencyGraph,
    start_id: &str,
) -> Vec<&'a TreePackage> {
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    let mut out = Vec::new();
    let mut stack = vec![start_id];
    visited.insert(start_id);
    while let Some(id) = stack.pop() {
        for dep in graph.dependencies_of(id) {
            if visited.insert(dep.id.as_str()) {
                out.push(dep);
                stack.push(&dep.id);
            }
        }
    }
    out
}

/// Finds a package's id in the graph by name, so a row (which only carries a
/// name) can be expanded through [`transitive_dependencies`]. `None` when the
/// package is not in the graph at all -- e.g. an extra that was not included
/// in the `uv tree` invocation that produced it.
pub fn find_id_by_name<'a>(graph: &'a DependencyGraph, name: &str) -> Option<&'a str> {
    graph
        .packages
        .values()
        .find(|pkg| pkg.name.eq_ignore_ascii_case(name))
        .map(|pkg| pkg.id.as_str())
}

/// Renders the tab. Deliberately dumb, like every other `view` in this crate
/// -- all the logic worth testing lives in `build`, `transitive_dependencies`
/// and `find_id_by_name` above. A direct dependency's transitive tree, when
/// `tab.graph` has one, is shown beneath it unconditionally rather than
/// behind a click -- `App` carries no per-row expand state, so "expand" here
/// means "the tree is rendered," not an interaction to wire up later.
pub fn view<'a>(tab: &DepsTab) -> iced::Element<'a, crate::app::Message> {
    let mut column = iced::widget::column![].spacing(12);

    if let Some(note) = &tab.tree_note {
        column = column.push(iced::widget::text(note.clone()));
    }

    for group in &tab.groups {
        let title = match &group.kind {
            GroupKind::Main => "main".to_string(),
            GroupKind::Group(name) => format!("group: {name}"),
            GroupKind::Extra(name) => format!("extra: {name}"),
        };
        let mut rows = iced::widget::column![iced::widget::text(title).size(16)].spacing(2);
        for row in &group.rows {
            let constraint = row.constraint.as_deref().unwrap_or("-");
            let version = row.version.as_deref().unwrap_or("(not installed)");
            let suffix = if row.declared { "" } else { " (not declared)" };
            rows = rows.push(iced::widget::text(format!(
                "{}  {constraint}  {version}{suffix}",
                row.name
            )));

            if let Some(graph) = &tab.graph
                && let Some(id) = find_id_by_name(graph, &row.name)
            {
                for dep in transitive_dependencies(graph, id) {
                    rows = rows.push(
                        iced::widget::text(format!("    \u{2514} {} {}", dep.name, dep.version))
                            .size(13),
                    );
                }
            }
        }
        column = column.push(rows);
    }

    column.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn pkg(name: &str, version: &str) -> InstalledPackage {
        InstalledPackage {
            name: name.to_string(),
            version: version.to_string(),
            editable_project_location: None,
        }
    }

    fn pyproject() -> PyProject {
        PyProject {
            name: Some("my-api".into()),
            requires_python: Some(">=3.12".into()),
            dependencies: vec!["fastapi>=0.115".into(), "httpx~=0.27".into()],
            optional_dependencies: BTreeMap::from([
                ("cli".to_string(), vec!["typer".to_string()]),
                (
                    "postgres".to_string(),
                    vec!["asyncpg>=0.29".to_string(), "sqlalchemy>=2".to_string()],
                ),
            ]),
            dependency_groups: BTreeMap::from([
                (
                    "test".to_string(),
                    vec![
                        GroupEntry::Requirement("pytest>=8".into()),
                        GroupEntry::Requirement("pytest-cov".into()),
                    ],
                ),
                (
                    "dev".to_string(),
                    vec![
                        GroupEntry::IncludeGroup("test".into()),
                        GroupEntry::Requirement("ruff".into()),
                    ],
                ),
            ]),
        }
    }

    // --- split_requirement ---

    #[test]
    fn a_constrained_requirement_splits_name_from_constraint() {
        assert_eq!(
            split_requirement("fastapi>=0.115"),
            ("fastapi".to_string(), Some(">=0.115".to_string()))
        );
    }

    #[test]
    fn an_unconstrained_requirement_has_no_constraint_text() {
        assert_eq!(split_requirement("typer"), ("typer".to_string(), None));
    }

    #[test]
    fn extras_and_markers_are_stripped_from_the_constraint_column() {
        assert_eq!(
            split_requirement("typer[all]>=1.0; python_version < \"3.13\""),
            ("typer".to_string(), Some(">=1.0".to_string()))
        );
    }

    // --- criterion 1: declared and installed joined by name ---

    #[test]
    fn a_declared_and_installed_package_shows_both() {
        let installed = vec![pkg("fastapi", "0.115.2")];
        let tab = build(&pyproject(), &installed, None);
        let main = &tab.groups[0];
        let row = main.rows.iter().find(|r| r.name == "fastapi").unwrap();
        assert_eq!(row.constraint.as_deref(), Some(">=0.115"));
        assert_eq!(row.version.as_deref(), Some("0.115.2"));
    }

    #[test]
    fn a_declared_but_uninstalled_package_shows_its_constraint_with_no_version() {
        // The exact bug criterion 1 exists to catch: dropping this row hides
        // precisely the packages a user needs to `uv sync`.
        let installed: Vec<InstalledPackage> = vec![];
        let tab = build(&pyproject(), &installed, None);
        let main = &tab.groups[0];
        let row = main
            .rows
            .iter()
            .find(|r| r.name == "httpx")
            .expect("declared-but-uninstalled must still be a row");
        assert_eq!(row.constraint.as_deref(), Some("~=0.27"));
        assert_eq!(row.version, None);
        assert!(row.declared);
    }

    // --- criterion 2: installed but not declared is still shown ---

    #[test]
    fn an_installed_but_undeclared_package_is_shown_marked_as_such() {
        let installed = vec![pkg("fastapi", "0.115.2"), pkg("anyio", "4.14.2")];
        let tab = build(&pyproject(), &installed, None);
        let main = &tab.groups[0];
        let row = main
            .rows
            .iter()
            .find(|r| r.name == "anyio")
            .expect("an undeclared installed package must still be a row");
        assert!(
            !row.declared,
            "anyio is not declared anywhere in the manifest"
        );
        assert_eq!(row.version.as_deref(), Some("4.14.2"));
        assert_eq!(row.constraint, None);
    }

    #[test]
    fn a_package_declared_in_an_extra_is_not_treated_as_a_leftover_in_main() {
        // The undeclared check has to look across every group and extra, not
        // just `main` -- otherwise every extra's own packages would double
        // up as fake "leftovers".
        let installed = vec![pkg("typer", "0.12.0")];
        let tab = build(&pyproject(), &installed, None);
        let main = &tab.groups[0];
        assert!(
            !main.rows.iter().any(|r| r.name == "typer"),
            "typer is declared under the cli extra, not main; got {:?}",
            main.rows
        );
    }

    // --- criterion 3: main, groups and extras stay distinct ---

    #[test]
    fn main_groups_and_extras_are_separate_and_a_shared_package_appears_in_both() {
        let tab = build(&pyproject(), &[], None);

        let kinds: Vec<&GroupKind> = tab.groups.iter().map(|g| &g.kind).collect();
        assert!(kinds.contains(&&GroupKind::Main));
        assert!(kinds.contains(&&GroupKind::Group("test".to_string())));
        assert!(kinds.contains(&&GroupKind::Group("dev".to_string())));
        assert!(kinds.contains(&&GroupKind::Extra("cli".to_string())));
        assert!(kinds.contains(&&GroupKind::Extra("postgres".to_string())));

        // "dev" includes "test" via include-group, so pytest-cov -- declared
        // directly only under "test" -- must appear under "dev" too.
        let test_group = tab
            .groups
            .iter()
            .find(|g| g.kind == GroupKind::Group("test".to_string()))
            .unwrap();
        let dev_group = tab
            .groups
            .iter()
            .find(|g| g.kind == GroupKind::Group("dev".to_string()))
            .unwrap();
        assert!(test_group.rows.iter().any(|r| r.name == "pytest-cov"));
        assert!(
            dev_group.rows.iter().any(|r| r.name == "pytest-cov"),
            "dev includes test, so pytest-cov must appear under dev too; got {:?}",
            dev_group.rows
        );
        // Flattening would merge test's rows into main or lose the group
        // boundary; distinctness means main has neither.
        let main = &tab.groups[0];
        assert!(!main.rows.iter().any(|r| r.name == "pytest-cov"));
    }

    #[test]
    fn a_cyclic_include_group_terminates_instead_of_looping_forever() {
        let pyproject = PyProject {
            dependency_groups: BTreeMap::from([
                (
                    "a".to_string(),
                    vec![GroupEntry::IncludeGroup("b".to_string())],
                ),
                (
                    "b".to_string(),
                    vec![GroupEntry::IncludeGroup("a".to_string())],
                ),
            ]),
            ..Default::default()
        };
        let tab = build(&pyproject, &[], None);
        // Must simply finish -- an infinite loop here would hang the test
        // (and, in the real app, the render thread once this feeds `view`).
        assert_eq!(tab.groups.len(), 3); // main + a + b
    }

    // --- criterion 4: UnsupportedSchema shows direct dependencies only, and says why ---

    #[test]
    fn an_unsupported_schema_still_shows_every_declared_row() {
        let installed = vec![pkg("fastapi", "0.115.2")];
        let tab = build(
            &pyproject(),
            &installed,
            Some(TreeOutcome::UnsupportedSchema {
                found: "v2".to_string(),
            }),
        );
        let main = &tab.groups[0];
        assert!(
            main.rows.iter().any(|r| r.name == "fastapi"),
            "direct dependencies must still be listed"
        );
        assert!(
            main.rows.iter().any(|r| r.name == "httpx"),
            "direct dependencies must still be listed"
        );
    }

    #[test]
    fn an_unsupported_schema_carries_no_graph_and_says_why() {
        let tab = build(
            &pyproject(),
            &[],
            Some(TreeOutcome::UnsupportedSchema {
                found: "v2".to_string(),
            }),
        );
        assert!(
            tab.graph.is_none(),
            "no transitive expansion without a graph uv answered with"
        );
        let note = tab
            .tree_note
            .expect("the tab must say why the tree is unavailable");
        assert!(
            note.contains("v2"),
            "the reason must name the schema uv actually returned; got {note:?}"
        );
    }

    #[test]
    fn no_tree_fetched_yet_carries_no_note() {
        // Distinct from UnsupportedSchema: no venv yet (or the command
        // hasn't run) needs no explanation, unlike an uv that answered with a
        // schema this build does not understand.
        let tab = build(&pyproject(), &[], None);
        assert!(tab.graph.is_none());
        assert!(tab.tree_note.is_none());
    }

    #[test]
    fn a_parsed_tree_carries_the_graph_and_no_note() {
        let graph = DependencyGraph {
            roots: vec!["root".to_string()],
            members: vec![],
            packages: BTreeMap::new(),
        };
        let tab = build(&pyproject(), &[], Some(TreeOutcome::Parsed(graph.clone())));
        assert_eq!(tab.graph, Some(graph));
        assert!(tab.tree_note.is_none());
    }

    // --- transitive_dependencies ---

    fn graph_with_chain() -> DependencyGraph {
        // root -> httpx -> httpcore -> h11
        //                           -> certifi
        let mut packages = BTreeMap::new();
        packages.insert(
            "root".to_string(),
            TreePackage {
                id: "root".into(),
                name: "root".into(),
                version: "0.1.0".into(),
                dependencies: vec!["httpx".into()],
            },
        );
        packages.insert(
            "httpx".to_string(),
            TreePackage {
                id: "httpx".into(),
                name: "httpx".into(),
                version: "0.28.1".into(),
                dependencies: vec!["httpcore".into()],
            },
        );
        packages.insert(
            "httpcore".to_string(),
            TreePackage {
                id: "httpcore".into(),
                name: "httpcore".into(),
                version: "1.0.9".into(),
                dependencies: vec!["h11".into(), "certifi".into()],
            },
        );
        packages.insert(
            "h11".to_string(),
            TreePackage {
                id: "h11".into(),
                name: "h11".into(),
                version: "0.16.0".into(),
                dependencies: vec![],
            },
        );
        packages.insert(
            "certifi".to_string(),
            TreePackage {
                id: "certifi".into(),
                name: "certifi".into(),
                version: "2026.7.22".into(),
                dependencies: vec![],
            },
        );
        DependencyGraph {
            roots: vec!["root".to_string()],
            members: vec![],
            packages,
        }
    }

    #[test]
    fn transitive_dependencies_walks_the_whole_chain() {
        let graph = graph_with_chain();
        let names: BTreeSet<&str> = transitive_dependencies(&graph, "httpx")
            .into_iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(
            names,
            BTreeSet::from(["httpcore", "h11", "certifi"]),
            "httpx's whole transitive tree must be reachable, not just its direct dependency"
        );
    }

    #[test]
    fn transitive_dependencies_does_not_include_the_start_node_itself() {
        let graph = graph_with_chain();
        let ids: Vec<&str> = transitive_dependencies(&graph, "httpx")
            .into_iter()
            .map(|p| p.id.as_str())
            .collect();
        assert!(!ids.contains(&"httpx"));
    }

    #[test]
    fn a_diamond_dependency_is_not_duplicated() {
        let mut graph = graph_with_chain();
        // Add a second edge from root straight to certifi, so certifi is
        // reachable two ways -- via httpcore and directly.
        graph
            .packages
            .get_mut("root")
            .unwrap()
            .dependencies
            .push("certifi".to_string());

        let certifi_count = transitive_dependencies(&graph, "root")
            .into_iter()
            .filter(|p| p.name == "certifi")
            .count();
        assert_eq!(
            certifi_count, 1,
            "a diamond dependency must appear once, not once per path"
        );
    }

    #[test]
    fn find_id_by_name_locates_a_package_case_insensitively() {
        let graph = graph_with_chain();
        assert_eq!(find_id_by_name(&graph, "HTTPX"), Some("httpx"));
        assert_eq!(find_id_by_name(&graph, "nonexistent"), None);
    }
}
