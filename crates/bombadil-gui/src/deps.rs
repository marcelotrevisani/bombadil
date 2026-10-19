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

use crate::interpreter;
use crate::theme;

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
    /// -- `tree_lines` below walks it to expand a direct
    /// dependency's row. `None` both when uv emitted a schema this build was
    /// not tested against and when the tree could not be read at all; either
    /// way the tab still shows every row above, direct dependencies only
    /// (criterion 4).
    pub graph: Option<DependencyGraph>,
    /// Why the transitive tree is unavailable, when it is: an unrecognised
    /// schema (`TreeOutcome::UnsupportedSchema`), or uv's own text for a tree
    /// that could not be read. Either way the tab says so rather than
    /// rendering an unexplained empty expand affordance (criterion 4).
    /// `None` only when there is a graph, which needs no explanation.
    pub tree_note: Option<String>,
}

/// A version being typed into one row's version cell. Opened by a double
/// click on a declared row, submitted with Enter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepEdit {
    /// The project index the row belongs to, so an edit never follows the
    /// selection onto a different project's tab.
    pub project: usize,
    pub name: String,
    pub group: GroupKind,
    pub draft: String,
}

/// Which of the tab's two layouts is showing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DepsView {
    /// Declared dependencies by group, each with its resolved tree beneath.
    #[default]
    Tree,
    /// Every package, declared or not, once each, alphabetically.
    All,
}

/// One package in the All view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatRow {
    pub row: DepRow,
    /// Where it is declared, which is where a version change is written.
    /// `Main` for a package declared nowhere: pinning a transitive package
    /// in uv means declaring it.
    pub group: GroupKind,
}

/// Every package the tab knows of, once each: the declared row wins over a
/// leftover, and the first group that declares it over later ones.
pub fn all_packages(tab: &DepsTab) -> Vec<FlatRow> {
    let mut seen: std::collections::BTreeMap<String, FlatRow> = Default::default();
    for group in &tab.groups {
        for row in &group.rows {
            let key = canonical_name(&row.name);
            let better = seen
                .get(&key)
                .is_none_or(|existing| row.declared && !existing.row.declared);
            if better {
                seen.insert(
                    key,
                    FlatRow {
                        row: row.clone(),
                        group: group.kind.clone(),
                    },
                );
            }
        }
    }
    seen.into_values().collect()
}

/// A package's subtree, depth first, as `(depth, package, repeated)`.
/// `repeated` marks a package already shown higher in this subtree; its
/// children are not listed again, which is also what ends a cycle.
pub fn tree_lines<'a>(
    graph: &'a DependencyGraph,
    start_id: &str,
) -> Vec<(usize, &'a TreePackage, bool)> {
    fn walk<'a>(
        graph: &'a DependencyGraph,
        id: &str,
        depth: usize,
        seen: &mut BTreeSet<String>,
        out: &mut Vec<(usize, &'a TreePackage, bool)>,
    ) {
        for dep in graph.dependencies_of(id) {
            let repeated = !seen.insert(dep.id.clone());
            out.push((depth, dep, repeated));
            if !repeated {
                walk(graph, &dep.id, depth + 1, seen, out);
            }
        }
    }
    let mut seen = BTreeSet::from([start_id.to_string()]);
    let mut out = Vec::new();
    walk(graph, start_id, 0, &mut seen, &mut out);
    out
}

/// The id of the version cell's input, for focusing it when an edit opens.
pub const VERSION_INPUT: &str = "dep-version";
/// The filter field's id, for Ctrl+F.
pub const FILTER_INPUT: &str = "dep-filter";

/// The `name==version` requirement `uv add` gets for `draft`, or why not.
///
/// A leading `==` is accepted because people type it. Anything that is not a
/// single PEP 440 version is refused here instead of being handed to uv:
/// `uv add` takes a whole requirement, so `1.0; os_name == 'nt'` or `>=1,<2`
/// would land in `pyproject.toml` as something the user never asked for.
pub fn pinned_requirement(name: &str, draft: &str) -> Result<String, String> {
    let version = draft.trim();
    let version = version.strip_prefix("==").unwrap_or(version).trim();
    if version.is_empty() {
        return Err(format!("type a version for {name}"));
    }
    let allowed =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '*' | '+' | '!' | '-' | '_');
    if !version.chars().all(allowed) || !version.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(format!(
            "\"{version}\" is not a version; type one like 2.31.0"
        ));
    }
    Ok(format!("{name}=={version}"))
}

/// The `uv add` flags that keep a requirement in the group its row came from.
/// A package name as PyPI compares them: case, `_` and `.` do not matter.
pub fn normalise(name: &str) -> String {
    name.to_lowercase().replace(['_', '.'], "-")
}

/// What the doctor says beside a row with a newer release, if anything.
pub fn outdated_note(
    row: &DepRow,
    outdated: &[bombadil_core::uv::results::Outdated],
) -> Option<String> {
    let found = outdated
        .iter()
        .find(|o| normalise(&o.name) == normalise(&row.name))?;
    match row
        .constraint
        .as_deref()
        .and_then(|c| c.trim().strip_prefix("=="))
    {
        Some(pinned) => Some(format!(
            "pinned to {}; {} exists",
            pinned.trim(),
            found.latest
        )),
        None => Some(format!("{} available", found.latest)),
    }
}

fn join_notes(notes: [Option<String>; 2]) -> Option<String> {
    let notes: Vec<String> = notes.into_iter().flatten().collect();
    (!notes.is_empty()).then(|| notes.join(" \u{b7} "))
}

pub fn uv_group(kind: &GroupKind) -> bombadil_core::uv::command::DependencyGroup {
    use bombadil_core::uv::command::DependencyGroup;
    match kind {
        GroupKind::Main => DependencyGroup::Main,
        GroupKind::Group(name) => DependencyGroup::Named(name.clone()),
        GroupKind::Extra(name) => DependencyGroup::Optional(name.clone()),
    }
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
/// A package name in PEP 503 canonical form: lowercased, with every run of
/// `-`, `_` or `.` collapsed to a single `-`.
///
/// Python treats `calc_service`, `calc-service` and `Calc.Service` as one
/// package, and the two sides of this tab spell it differently: a
/// `pyproject.toml` declares whatever the author typed, while `uv pip list`
/// reports the canonical form. Comparing them case-insensitively is not
/// enough -- `calc_service` and `calc-service` differ by a character that is
/// not a case difference.
///
/// The symptom was a monorepo's own members, every one of them installed
/// editable, showing as "not installed" in the Dependencies tab: their
/// declared names carry underscores, and uv reports them with hyphens.
fn canonical_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut previous_was_separator = false;
    for character in name.chars() {
        if matches!(character, '-' | '_' | '.') {
            // A *run* collapses to one `-`, per PEP 503's `[-_.]+`.
            if !previous_was_separator {
                out.push('-');
                previous_was_separator = true;
            }
        } else {
            out.extend(character.to_lowercase());
            previous_was_separator = false;
        }
    }
    out
}

fn join_rows(requirements: &[String], installed: &[InstalledPackage]) -> Vec<DepRow> {
    requirements
        .iter()
        .map(|requirement| {
            let (name, constraint) = split_requirement(requirement);
            let canonical = canonical_name(&name);
            let version = installed
                .iter()
                .find(|pkg| canonical_name(&pkg.name) == canonical)
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
/// the resolved tree.
///
/// The three things `tree` can say must stay distinguishable, and each gets
/// its own note (criterion 4): a graph this build understands, uv answering
/// with a schema it does not (`TreeOutcome::UnsupportedSchema`), and the tree
/// not being readable at all -- `Err`, carrying uv's own text. The last one
/// used to arrive as the same `None` as "nothing fetched yet", so a real JSON
/// parse error rendered as an ordinary tab with no tree and nothing said.
///
/// There is no "not fetched" case here on purpose: this is only ever built
/// from a fetch that got far enough to run `uv tree`, and one that did not
/// never reaches a `DepsTab` at all -- `app::update` keeps the whole fetch's
/// `Err` instead.
pub fn build(
    pyproject: &PyProject,
    installed: &[InstalledPackage],
    tree: Result<TreeOutcome, String>,
) -> DepsTab {
    let mut declared_anywhere: BTreeSet<String> = BTreeSet::new();
    let mut groups = Vec::new();

    let main_rows = join_rows(&pyproject.dependencies, installed);
    for row in &main_rows {
        declared_anywhere.insert(canonical_name(&row.name));
    }

    for name in pyproject.dependency_groups.keys() {
        let mut visited = BTreeSet::new();
        let requirements = resolve_group(pyproject, name, &mut visited);
        let rows = join_rows(&requirements, installed);
        for row in &rows {
            declared_anywhere.insert(canonical_name(&row.name));
        }
        groups.push(DepGroup {
            kind: GroupKind::Group(name.clone()),
            rows,
        });
    }

    for (name, requirements) in &pyproject.optional_dependencies {
        let rows = join_rows(requirements, installed);
        for row in &rows {
            declared_anywhere.insert(canonical_name(&row.name));
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
        if !declared_anywhere.contains(&canonical_name(&pkg.name)) {
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
        Ok(TreeOutcome::Parsed(graph)) => (Some(graph), None),
        Ok(TreeOutcome::UnsupportedSchema { found }) => (
            None,
            Some(format!(
                "uv returned a dependency tree in an unrecognised format ({found}); showing direct dependencies only."
            )),
        ),
        Err(cause) => (
            None,
            Some(format!(
                "could not read the dependency tree ({cause}); showing direct dependencies only."
            )),
        ),
    };

    DepsTab {
        groups,
        graph,
        tree_note,
    }
}

/// Finds a package's id in the graph by name, so a row (which only carries a
/// name) can be expanded through [`tree_lines`]. `None` when the
/// package is not in the graph at all -- e.g. an extra that was not included
/// in the `uv tree` invocation that produced it.
pub fn find_id_by_name<'a>(graph: &'a DependencyGraph, name: &str) -> Option<&'a str> {
    graph
        .packages
        .values()
        .find(|pkg| pkg.name.eq_ignore_ascii_case(name))
        .map(|pkg| pkg.id.as_str())
}

/// Whether `name` passes the filter: a case-insensitive substring; an empty
/// filter passes everything.
pub fn name_matches(name: &str, filter: &str) -> bool {
    let filter = filter.trim();
    filter.is_empty() || name.to_lowercase().contains(&filter.to_lowercase())
}

/// The tree view's rows under `filter`: a declared row stays when it, or
/// anything beneath it in the resolved graph, matches -- otherwise a match
/// three levels down would have nowhere to show. Groups left empty go.
pub fn filter_tree(tab: &DepsTab, filter: &str) -> DepsTab {
    let keep = |row: &DepRow| {
        name_matches(&row.name, filter)
            || tab.graph.as_ref().is_some_and(|graph| {
                find_id_by_name(graph, &row.name).is_some_and(|id| {
                    tree_lines(graph, id)
                        .iter()
                        .any(|(_, dep, _)| name_matches(&dep.name, filter))
                })
            })
    };
    DepsTab {
        groups: tab
            .groups
            .iter()
            .map(|group| DepGroup {
                kind: group.kind.clone(),
                rows: group.rows.iter().filter(|r| keep(r)).cloned().collect(),
            })
            .filter(|group| !group.rows.is_empty())
            .collect(),
        graph: tab.graph.clone(),
        tree_note: tab.tree_note.clone(),
    }
}

/// What the tab shows instead of rows when the fetch itself failed: no venv
/// to ask, no resolvable uv, a `uv pip list` that exited non-zero, output this
/// build cannot parse.
///
/// The alternative is what this used to do -- degrade to an empty `installed`
/// list, so every declared package rendered as `"(not installed)"`. That is a
/// positive claim about the environment when the truth is that we could not
/// ask it anything, and it is indistinguishable from a genuinely empty venv.
/// `cause` is uv's own text wherever there is any, the same discipline
/// `app::list_interpreters_blocking` follows.
pub fn unavailable_message(cause: &str) -> String {
    format!("could not read this project's environment: {cause}")
}

/// Maps one dependency row to the glyph state it should render.
///
/// - Not installed: [`theme::State::Absent`] -- declared, not installed
///   (criterion 1's exact "needs a `uv sync`" row), and equally a leftover
///   with no constraint to violate in the first place has nothing to be
///   absent *from*, so this only ever fires for a genuinely uninstalled
///   package.
/// - Installed, and the installed version satisfies the declared
///   constraint (or there is no constraint to check, e.g. an undeclared
///   leftover or a bare `"typer"` requirement): [`theme::State::Present`].
/// - Installed, but outside the declared constraint:
///   [`theme::State::Drifted`] -- a package installed at a version the
///   manifest forbids is a different problem from one that is missing, and
///   this is the state that says so.
///
/// Reuses `interpreter::version_satisfies` rather than a second comparator.
/// A dependency constraint (`">=0.27"`, `"~=1.2"`, ...) is the same PEP 440
/// specifier grammar `requires-python` uses, so the same parser applies
/// directly -- no forked rule, no second parser. That includes its fallback
/// direction: a constraint this parser cannot read (PEP 440's `<`, `<=`,
/// `!=`, `===`, an epoch, a local version) is treated as satisfied, not as a
/// mismatch, so a working package is never marked drifted just because its
/// constraint used syntax outside the subset that parser handles.
pub fn state(row: &DepRow) -> theme::State {
    match &row.version {
        None => theme::State::Absent,
        Some(version) => {
            if interpreter::version_satisfies(version, row.constraint.as_deref()) {
                theme::State::Present
            } else {
                theme::State::Drifted
            }
        }
    }
}

/// Renders one dependency row: the state glyph, then name, constraint and
/// version in fixed-width columns so the mono-set constraints and versions
/// stack vertically across the whole tab, not just within one group --
/// `theme::DEP_NAME_COLUMN_WIDTH`/`DEP_CONSTRAINT_COLUMN_WIDTH` are the grid
/// every row in every group shares. The row itself sits on a `bark` raised
/// surface over the pane's `ink` ground, the same treatment every row in
/// every pane gets.
///
/// Package names are data too (the plan's own instruction: a user compares
/// them character by character), so the name column is Plex like the
/// constraint and version columns. Only the placeholder words a real value
/// is missing -- `"not installed"`, `"(not declared)"` -- switch to the
/// prose face, the same exception `sidebar::view` already makes for
/// `"no env"`.
fn dep_row<'a>(
    row: &DepRow,
    group: &GroupKind,
    editing: Option<&DepEdit>,
    enabled: bool,
    project: usize,
    note: Option<String>,
) -> iced::Element<'a, crate::app::Message> {
    let glyph = theme::state_glyph(state(row));

    let name = iced::widget::text(row.name.clone())
        .font(theme::FONT_DATA)
        .size(theme::DATA)
        .width(theme::DEP_NAME_COLUMN_WIDTH);

    let constraint: iced::Element<'_, crate::app::Message> = match &row.constraint {
        Some(constraint) => iced::widget::text(constraint.clone())
            .font(theme::FONT_DATA)
            .size(theme::DATA)
            .style(theme::text_muted)
            .into(),
        None => iced::widget::text("-")
            .font(theme::FONT_PROSE)
            .size(theme::DATA)
            .style(theme::text_muted)
            .into(),
    };
    let constraint = iced::widget::container(constraint).width(theme::DEP_CONSTRAINT_COLUMN_WIDTH);

    let version: iced::Element<'_, crate::app::Message> = match &row.version {
        Some(version) => iced::widget::text(version.clone())
            .font(theme::FONT_DATA)
            .size(theme::DATA)
            .into(),
        None => iced::widget::text("not installed")
            .font(theme::FONT_PROSE)
            .size(theme::DATA)
            .style(theme::text_muted)
            .into(),
    };
    // Any row: on a package declared nowhere, `uv add` declares it in
    // `main`, which is how uv pins a transitive version.
    let version: iced::Element<'_, crate::app::Message> = match editing {
        Some(edit) if edit.name == row.name && edit.group == *group => iced::widget::row![
            iced::widget::text_input("version", &edit.draft)
                .style(crate::theme::text_field)
                .id(VERSION_INPUT)
                .on_input(crate::app::Message::DepVersionDraftChanged)
                .on_submit_maybe(enabled.then_some(crate::app::Message::DepVersionSubmitted))
                .font(theme::FONT_DATA)
                .size(theme::DATA)
                .width(140),
            iced::widget::button(iced::widget::text("cancel").size(theme::LABEL))
                .on_press(crate::app::Message::DepVersionEditCancelled)
                .padding([0.0, theme::SPACE_1])
                .style(theme::button_quiet),
        ]
        .spacing(theme::SPACE_1)
        .align_y(iced::Alignment::Center)
        .into(),
        _ => {
            let start = crate::app::Message::DepVersionEditStarted {
                name: row.name.clone(),
                group: group.clone(),
                current: row.version.clone().unwrap_or_default(),
            };
            let cell = iced::widget::mouse_area(version).on_double_click(start.clone());
            if enabled {
                // The pencil is the visible way in; double-click stays.
                // A slot is kept beside the version so the pencil never
                // covers it.
                // Upgrade only where something is installed to upgrade.
                let upgrade = row.version.is_some().then(|| {
                    crate::icons::icon_button(
                        crate::icons::Icon::ArrowUp,
                        format!("Upgrade {} within its constraint", row.name),
                        Some(crate::app::Message::DepUpgradeRequested(
                            project,
                            row.name.clone(),
                        )),
                    )
                });
                iced::widget::hover(
                    iced::widget::row![cell, iced::widget::Space::new().width(64)],
                    iced::widget::container(
                        iced::widget::row![crate::icons::icon_button(
                            crate::icons::Icon::Pencil,
                            "Change version (or double-click)",
                            Some(start),
                        )]
                        .push(upgrade),
                    )
                    .width(iced::Length::Fill)
                    .align_x(iced::alignment::Horizontal::Right),
                )
            } else {
                cell.into()
            }
        }
    };

    let mut line = iced::widget::row![
        iced::widget::text(glyph.glyph.to_string())
            .size(theme::BODY)
            .style(theme::tinted(glyph.swatch))
            .width(theme::GLYPH_COLUMN_WIDTH),
        name,
        constraint,
        // A fixed column, so a note after it starts at the same place on
        // every row instead of wherever the version text happened to end.
        iced::widget::container(version).width(theme::DEP_VERSION_COLUMN_WIDTH),
    ]
    .push(note.map(|note| {
        iced::widget::text(note)
            .size(theme::LABEL)
            .style(theme::text_muted)
    }))
    .spacing(theme::SPACE_1)
    .align_y(iced::Alignment::Center);

    if !row.declared {
        line = line.push(
            iced::widget::text("(not declared)")
                .font(theme::FONT_PROSE)
                .size(theme::DATA)
                .style(theme::text_muted),
        );
    }

    // Full width: hugging its content, each row's band ended wherever its
    // version text did, a ragged edge down the list.
    iced::widget::container(line)
        .padding(theme::SPACE_1)
        .width(iced::Length::Fill)
        .style(|t| iced::widget::container::Style {
            border: iced::Border {
                radius: theme::RADIUS.into(),
                ..iced::Border::default()
            },
            ..theme::surface(t)
        })
        .into()
}

/// Renders the tab, or -- for a fetch that failed -- [`unavailable_message`]
/// alone, with no rows at all: a row claiming a version, or the absence of
/// one, would be a claim this fetch is in no position to make.
///
/// Deliberately dumb, like every other `view` in this crate: all the logic
/// worth testing lives in `build`, `state`, `unavailable_message`,
/// `tree_lines`, `all_packages` and `find_id_by_name`. A direct dependency's
/// transitive tree, when `tab.graph` has one, is shown beneath it
/// unconditionally rather than behind a click -- `App` carries no per-row
/// expand state, so "expand" here means "the tree is rendered," not an
/// interaction to wire up later.
#[allow(clippy::too_many_arguments)]
pub fn view<'a>(
    fetched: &Result<DepsTab, String>,
    editing: Option<&DepEdit>,
    mode: DepsView,
    filter: &str,
    project: usize,
    waiting: Option<&str>,
    no_environment: bool,
    outdated: &[bombadil_core::uv::results::Outdated],
) -> iced::Element<'a, crate::app::Message> {
    use crate::app::Message;
    let enabled = waiting.is_none();
    use crate::icons::{self, Icon};
    let labelled = |which: Icon, label: &'static str| {
        iced::widget::row![
            icons::icon(which, 14.0, theme::Swatch::Parchment),
            iced::widget::text(label).size(theme::BODY),
        ]
        .spacing(theme::SPACE_1)
        .align_y(iced::Alignment::Center)
    };
    let switch = |label: &'static str, this: DepsView| {
        iced::widget::button(iced::widget::text(label).size(theme::BODY))
            .on_press(Message::DepsViewSelected(this))
            .padding([theme::SPACE_1, theme::SPACE_2])
            .style(theme::button_choice(mode == this))
    };
    // Recreate here, beside the drift it answers; Sync is the header's primary
    // action and the Dependencies tile's, so a third copy here only crowded
    // the toolbar onto two lines. Recreate keeps its confirmation.
    let toolbar = iced::widget::row![
        switch("Tree", DepsView::Tree),
        switch("All packages", DepsView::All),
        iced::widget::text_input("Filter packages", filter)
            .style(crate::theme::text_field)
            .id(FILTER_INPUT)
            .on_input(Message::DepFilterChanged)
            .size(theme::BODY)
            .width(180),
        iced::widget::button(labelled(Icon::Folder, "Add local package\u{2026}"))
            .on_press(Message::AddLocalRequested(project))
            .padding([theme::SPACE_1, theme::SPACE_2])
            .style(theme::button_secondary),
        crate::glossary::wait_tip(
            iced::widget::button(labelled(Icon::ArrowUp, "Upgrade all"))
                .on_press_maybe(enabled.then_some(Message::DepUpgradeAllRequested(project)))
                .padding([theme::SPACE_1, theme::SPACE_2])
                .style(theme::button_secondary),
            waiting,
            crate::glossary::Term::Upgrade,
        ),
        crate::glossary::wait_tip(
            iced::widget::button(labelled(Icon::Rotate, "Recreate venv"))
                .on_press_maybe(enabled.then_some(Message::RecreateVenvRequested(project)))
                .padding([theme::SPACE_1, theme::SPACE_2])
                .style(theme::button_secondary),
            waiting,
            crate::glossary::Term::Environment,
        ),
    ]
    .spacing(theme::SPACE_1)
    .align_y(iced::Alignment::Center)
    // Wraps rather than clipping: with the filter and the add/upgrade
    // actions it is wider than a narrow window, and the last button used to
    // be cut off at the edge.
    .wrap()
    .vertical_spacing(theme::SPACE_1);

    let body: iced::Element<'a, Message> = match (fetched, mode) {
        // Not a failure to report but a step not taken yet; the header's
        // Sync takes it.
        (Err(_), _) if no_environment => {
            iced::widget::text("No environment yet. Sync creates it from pyproject.toml.")
                .size(theme::BODY)
                .style(theme::text_muted)
                .into()
        }
        (Err(cause), _) => iced::widget::text(unavailable_message(cause))
            .size(theme::BODY)
            .into(),
        (Ok(tab), DepsView::Tree) => tree_view(
            &filter_tree(tab, filter),
            editing,
            enabled,
            project,
            outdated,
        ),
        (Ok(tab), DepsView::All) => all_view(tab, filter, editing, enabled, project, outdated),
    };
    let headed = fetched.is_ok();
    iced::widget::column![toolbar]
        .push(headed.then(|| column_headings(mode)))
        .push(body)
        .spacing(theme::SPACE_3)
        .into()
}

/// Every package once, with where it is declared. Double-click a version to
/// change it.
fn all_view<'a>(
    tab: &DepsTab,
    filter: &str,
    editing: Option<&DepEdit>,
    enabled: bool,
    project: usize,
    outdated: &[bombadil_core::uv::results::Outdated],
) -> iced::Element<'a, crate::app::Message> {
    let mut rows = iced::widget::column![
        iced::widget::text(
            "hover a version to change or upgrade it; a package declared nowhere is pinned in main"
        )
        .size(theme::LABEL)
        .style(theme::text_muted)
    ]
    .spacing(theme::SPACE_1);
    for flat in all_packages(tab)
        .into_iter()
        .filter(|flat| name_matches(&flat.row.name, filter))
    {
        // Inside the row, beside the version: outside it, the full-width row
        // pushed it to the pane's far edge, under the scroll bar.
        let label = match &flat.group {
            GroupKind::Group(name) => Some(format!("group: {name}")),
            GroupKind::Extra(name) => Some(format!("extra: {name}")),
            GroupKind::Main => None,
        }
        .filter(|_| flat.row.declared);
        let label = join_notes([label, outdated_note(&flat.row, outdated)]);
        rows = rows.push(dep_row(
            &flat.row,
            &flat.group,
            editing,
            enabled,
            project,
            label,
        ));
    }
    rows.into()
}

/// Names the three columns once, above the rows, so `-` reads as "no
/// constraint" rather than as a glitch, and which number is which needs no
/// guessing.
fn column_headings<'a>(mode: DepsView) -> iced::Element<'a, crate::app::Message> {
    let heading = |label: &'a str| {
        iced::widget::text(label)
            .size(theme::LABEL)
            .style(theme::text_muted)
    };
    iced::widget::container(
        iced::widget::row![
            iced::widget::Space::new().width(theme::GLYPH_COLUMN_WIDTH),
            heading("Package").width(theme::DEP_NAME_COLUMN_WIDTH),
            heading("Constraint").width(theme::DEP_CONSTRAINT_COLUMN_WIDTH),
            heading("Installed").width(theme::DEP_VERSION_COLUMN_WIDTH),
        ]
        // Only the All view has a fourth column: where each package is
        // declared.
        .push((mode == DepsView::All).then(|| heading("Declared in")))
        .spacing(theme::SPACE_1),
    )
    .padding([0.0, theme::SPACE_1])
    .into()
}

/// How far a transitive line at `depth` sits right of its parent row.
fn indent(depth: usize) -> f32 {
    16.0 * depth as f32
}

/// Declared dependencies by group, each with its resolved tree indented
/// beneath it.
fn tree_view<'a>(
    tab: &DepsTab,
    editing: Option<&DepEdit>,
    enabled: bool,
    project: usize,
    outdated: &[bombadil_core::uv::results::Outdated],
) -> iced::Element<'a, crate::app::Message> {
    let mut column = iced::widget::column![].spacing(theme::SPACE_3);

    if let Some(note) = &tab.tree_note {
        // Not one of `BOOT`'s rationed cases (a missing venv, a declared but
        // uninstalled dependency, a member syncing will remove, a
        // destructive confirm) -- an unreadable tree still shows every
        // direct dependency's row above, so this is informational, not an
        // alarm.
        column = column.push(
            iced::widget::text(note.clone())
                .size(theme::BODY)
                .style(theme::text_muted),
        );
    }

    for group in &tab.groups {
        let title = match &group.kind {
            GroupKind::Main => "main".to_string(),
            GroupKind::Group(name) => format!("group: {name}"),
            GroupKind::Extra(name) => format!("extra: {name}"),
        };
        let mut rows = iced::widget::column![
            iced::widget::text(title)
                .font(theme::FONT_PROSE)
                .size(theme::LABEL)
                .style(theme::text_muted)
        ]
        .spacing(theme::SPACE_1);
        for row in &group.rows {
            rows = rows.push(dep_row(
                row,
                &group.kind,
                editing,
                enabled,
                project,
                outdated_note(row, outdated),
            ));
            if !row.declared {
                // A leftover's place is the All view; the tree is what the
                // manifest asks for.
                continue;
            }

            if let Some(graph) = &tab.graph
                && let Some(id) = find_id_by_name(graph, &row.name)
            {
                for (depth, dep, repeated) in tree_lines(graph, id) {
                    // Wrapped in the same `padding(SPACE_1)` container
                    // `dep_row` uses (minus its `bark` fill -- a transitive
                    // row is not a surface of its own). Without it the tree
                    // sat 4pt to the left of the row it belongs to, which
                    // reads as a different column rather than as a child.
                    rows = rows.push(
                        iced::widget::container(
                            iced::widget::row![
                                iced::widget::Space::new()
                                    .width(theme::GLYPH_COLUMN_WIDTH + indent(depth)),
                                // Narrowed by the indent, so every version
                                // lands in the installed column at any depth
                                // instead of stepping right with the tree.
                                iced::widget::text(format!("\u{2514} {}", dep.name))
                                    .font(theme::FONT_DATA)
                                    .size(theme::LABEL)
                                    .style(theme::text_muted)
                                    .width(
                                        (theme::DEP_NAME_COLUMN_WIDTH
                                            + theme::SPACE_1
                                            + theme::DEP_CONSTRAINT_COLUMN_WIDTH
                                            - indent(depth))
                                        .max(theme::DEP_CONSTRAINT_COLUMN_WIDTH),
                                    ),
                                iced::widget::text(if repeated {
                                    format!("{} (shown above)", dep.version)
                                } else {
                                    dep.version.clone()
                                })
                                .font(theme::FONT_DATA)
                                .size(theme::LABEL)
                                .style(theme::text_muted),
                            ]
                            .spacing(theme::SPACE_1),
                        )
                        .padding(theme::SPACE_1),
                    );
                }
            }
        }
        column = column.push(rows);
    }

    column.into()
}

/// The "Upgrade all" confirmation: it rewrites much of `uv.lock`, so it is
/// asked, not done.
pub fn confirm_upgrade_all_view<'a>(label: &str) -> iced::Element<'a, crate::app::Message> {
    use crate::app::Message;
    iced::widget::container(
        iced::widget::column![
            iced::widget::text(format!(
                "Upgrade every package of {label} to the newest versions its \
                 constraints allow?\n\nThis rewrites uv.lock."
            ))
            .size(theme::BODY),
            iced::widget::row![
                iced::widget::button(iced::widget::text("Upgrade all").size(theme::BODY))
                    .on_press(Message::DepUpgradeAllConfirmed)
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_primary),
                iced::widget::button(iced::widget::text("Cancel").size(theme::BODY))
                    .on_press(Message::DepUpgradeAllCancelled)
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_quiet),
            ]
            .spacing(theme::SPACE_2),
        ]
        .spacing(theme::SPACE_3),
    )
    .width(theme::MODAL_WIDTH)
    .padding(theme::SPACE_4)
    .style(theme::panel)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outdated_notes_match_normalised_names() {
        use bombadil_core::uv::results::Outdated;
        let outdated = vec![
            Outdated {
                name: "Typing_Extensions".into(),
                version: "4.1".into(),
                latest: "4.16".into(),
            },
            Outdated {
                name: "six".into(),
                version: "1.16.0".into(),
                latest: "1.17.0".into(),
            },
        ];
        let row = |name: &str, constraint: Option<&str>| DepRow {
            name: name.into(),
            constraint: constraint.map(Into::into),
            version: Some("x".into()),
            declared: true,
        };
        assert_eq!(
            outdated_note(&row("typing-extensions", Some(">=4")), &outdated).as_deref(),
            Some("4.16 available")
        );
        assert_eq!(
            outdated_note(&row("six", Some("==1.16.0")), &outdated).as_deref(),
            Some("pinned to 1.16.0; 1.17.0 exists")
        );
        assert_eq!(outdated_note(&row("httpx", None), &outdated), None);
    }

    #[test]
    fn a_typed_version_becomes_an_exact_pin() {
        assert_eq!(
            pinned_requirement("fastapi", " 0.110.0 ").unwrap(),
            "fastapi==0.110.0"
        );
        assert_eq!(
            pinned_requirement("fastapi", "==0.110.0").unwrap(),
            "fastapi==0.110.0"
        );
        assert_eq!(
            pinned_requirement("torch", "2.3.0+cpu").unwrap(),
            "torch==2.3.0+cpu"
        );
    }

    #[test]
    fn anything_but_one_version_is_refused_before_uv_sees_it() {
        // Each of these would otherwise be written into pyproject.toml.
        for draft in [
            "",
            "  ",
            ">=1,<2",
            "1.0; os_name == 'nt'",
            "1.0 --dev",
            "latest",
        ] {
            assert!(
                pinned_requirement("x", draft).is_err(),
                "{draft:?} was accepted"
            );
        }
    }

    #[test]
    fn a_member_declared_with_underscores_matches_the_hyphenated_install() {
        // The reported bug, in one assertion. A monorepo declares its own
        // members as `calc_service` and uv reports them as `calc-service`, so
        // every editable member showed as "not installed" -- the Dependencies
        // tab telling the user to sync an environment that was already right.
        let rows = join_rows(
            &["calc_service>=15.0.0".to_string()],
            &[InstalledPackage {
                name: "calc-service".into(),
                version: "15.2.28".into(),
                // Editable, which is what a workspace member is: the exact
                // shape the report came from.
                editable_project_location: Some("/repo/lib/calc_service".into()),
            }],
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].version.as_deref(),
            Some("15.2.28"),
            "an underscore and a hyphen are the same package to Python"
        );
        assert_eq!(state(&rows[0]), theme::State::Present);
    }

    #[test]
    fn a_matched_member_does_not_also_appear_as_an_undeclared_leftover() {
        // The other half, and the one that would have left the tab listing
        // the package twice: `declared_anywhere` has to be keyed the same way
        // the join is, or a member matched above comes back as a leftover.
        let pyproject = PyProject {
            dependencies: vec!["calc_service>=15.0.0".to_string()],
            ..PyProject::default()
        };
        let installed = vec![InstalledPackage {
            name: "calc-service".into(),
            version: "15.2.28".into(),
            editable_project_location: Some("/repo/lib/calc_service".into()),
        }];

        let tab = build(&pyproject, &installed, Err("no tree".to_string()));

        let named: Vec<&DepRow> = tab
            .groups
            .iter()
            .flat_map(|group| group.rows.iter())
            .filter(|row| canonical_name(&row.name) == "calc-service")
            .collect();
        assert_eq!(
            named.len(),
            1,
            "the package must appear once, not once declared and once as a \
             leftover; got {named:?}"
        );
        assert!(named[0].declared);
    }

    #[test]
    fn canonical_names_follow_pep_503() {
        // Lowercase, and every run of `-`, `_` or `.` collapsed to one `-`.
        for (raw, want) in [
            ("calc_service", "calc-service"),
            ("calc-service", "calc-service"),
            ("Calc.Service", "calc-service"),
            ("calc__service", "calc-service"),
            ("calc_-.service", "calc-service"),
            ("ThornOfficialConfig", "thornofficialconfig"),
            ("zope.interface", "zope-interface"),
        ] {
            assert_eq!(canonical_name(raw), want, "{raw}");
        }
    }

    #[test]
    fn two_genuinely_different_packages_do_not_collapse_together() {
        // The normalisation must not be so eager that it matches things
        // Python would keep apart. Only `-`, `_` and `.` are equivalent.
        assert_ne!(
            canonical_name("calc_service"),
            canonical_name("calcservice")
        );
        assert_ne!(
            canonical_name("calc_service"),
            canonical_name("calc_services")
        );
    }

    use std::collections::BTreeMap;

    /// A tree result for a test that is not about the tree: `Err`, since
    /// every fetch that reaches `build` at all has run `uv tree` and got
    /// either an answer or a reason.
    fn no_tree() -> Result<TreeOutcome, String> {
        Err("this test is not about the tree".to_string())
    }

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
        let tab = build(&pyproject(), &installed, no_tree());
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
        let tab = build(&pyproject(), &installed, no_tree());
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
        let tab = build(&pyproject(), &installed, no_tree());
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
        let tab = build(&pyproject(), &installed, no_tree());
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
        let tab = build(&pyproject(), &[], no_tree());

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
        let tab = build(&pyproject, &[], no_tree());
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
            Ok(TreeOutcome::UnsupportedSchema {
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
            Ok(TreeOutcome::UnsupportedSchema {
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

    // --- a fetch that failed outright: named, never rendered as rows ---

    #[test]
    fn an_unavailable_environment_is_reported_by_its_cause() {
        let message = unavailable_message("error: No virtual environment found at /p/api/.venv");

        assert!(
            message.contains("No virtual environment found at /p/api/.venv"),
            "the cause uv gave must survive into what the user reads; got {message:?}"
        );
        assert!(
            !message.contains("not installed"),
            "a fetch that could not ask must not claim anything about what is installed; \
             got {message:?}"
        );
    }

    #[test]
    fn an_unreadable_tree_says_so_rather_than_looking_like_no_tree_at_all() {
        // The literal repeat of the defect one line up in `fetch_deps_blocking`:
        // `parse_tree(..).ok()` threw a real JSON parse error into the same
        // `None` as "nothing fetched yet", so a tree uv answered with and this
        // build could not read rendered as an ordinary tab with no tree and
        // nothing said about it.
        let tab = build(
            &pyproject(),
            &[],
            Err("could not read uv's dependency tree: expected value at line 1".to_string()),
        );

        assert!(tab.graph.is_none());
        let note = tab
            .tree_note
            .expect("a tree that could not be read must be explained, not silently absent");
        assert!(
            note.contains("expected value at line 1"),
            "the note must carry the actual reason; got {note:?}"
        );
    }

    #[test]
    fn an_unreadable_tree_is_not_confused_with_an_unrecognised_schema() {
        // Both leave `graph` empty and both say something, but they say
        // different things: `deps::build`'s own doc requires the two to stay
        // distinguishable.
        let unreadable = build(&pyproject(), &[], Err("uv tree failed".to_string()));
        let unsupported = build(
            &pyproject(),
            &[],
            Ok(TreeOutcome::UnsupportedSchema {
                found: "v2".to_string(),
            }),
        );

        assert_ne!(unreadable.tree_note, unsupported.tree_note);
        assert!(
            !unreadable
                .tree_note
                .unwrap()
                .contains("unrecognised format"),
            "a tree that never parsed must not be reported as a schema this build has not seen"
        );
    }

    #[test]
    fn a_parsed_tree_carries_the_graph_and_no_note() {
        let graph = DependencyGraph {
            roots: vec!["root".to_string()],
            members: vec![],
            packages: BTreeMap::new(),
        };
        let tab = build(&pyproject(), &[], Ok(TreeOutcome::Parsed(graph.clone())));
        assert_eq!(tab.graph, Some(graph));
        assert!(tab.tree_note.is_none());
    }

    // --- tree_lines ---

    fn declared(name: &str) -> DepRow {
        DepRow {
            name: name.into(),
            constraint: None,
            version: Some("1.0".into()),
            declared: true,
        }
    }

    #[test]
    fn a_filter_matches_names_regardless_of_case() {
        assert!(name_matches("HTTPX", "tpx"));
        assert!(name_matches("httpx", ""));
        assert!(!name_matches("httpx", "core"));
    }

    #[test]
    fn the_tree_filter_keeps_a_parent_for_a_matching_descendant() {
        let tab = DepsTab {
            groups: vec![DepGroup {
                kind: GroupKind::Main,
                rows: vec![declared("httpx"), declared("rich")],
            }],
            graph: Some(graph_with_chain()),
            tree_note: None,
        };
        let names = |t: &DepsTab| {
            t.groups
                .iter()
                .flat_map(|g| g.rows.iter().map(|r| r.name.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&filter_tree(&tab, "h11")),
            ["httpx"],
            "h11 is under httpx"
        );
        assert_eq!(names(&filter_tree(&tab, "ric")), ["rich"]);
        assert!(
            filter_tree(&tab, "zzz").groups.is_empty(),
            "empty groups go"
        );
        assert_eq!(names(&filter_tree(&tab, "")), ["httpx", "rich"]);
    }

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

    fn lines(graph: &DependencyGraph, id: &str) -> Vec<(usize, String, bool)> {
        tree_lines(graph, id)
            .into_iter()
            .map(|(depth, p, repeated)| (depth, p.name.clone(), repeated))
            .collect()
    }

    #[test]
    fn the_tree_is_nested_by_depth_and_excludes_its_root() {
        assert_eq!(
            lines(&graph_with_chain(), "httpx"),
            vec![
                (0, "httpcore".into(), false),
                (1, "h11".into(), false),
                (1, "certifi".into(), false),
            ]
        );
    }

    #[test]
    fn a_diamond_is_expanded_once_and_marked_where_it_repeats() {
        let mut graph = graph_with_chain();
        graph
            .packages
            .get_mut("root")
            .unwrap()
            .dependencies
            .push("certifi".into());
        let got = lines(&graph, "root");
        let certifi: Vec<_> = got.iter().filter(|(_, n, _)| n == "certifi").collect();
        assert_eq!(certifi.len(), 2, "shown on both paths: {got:?}");
        assert_eq!(
            certifi.iter().filter(|(_, _, repeated)| !repeated).count(),
            1
        );
    }

    #[test]
    fn a_cycle_ends_instead_of_recursing_forever() {
        let mut graph = graph_with_chain();
        graph
            .packages
            .get_mut("h11")
            .unwrap()
            .dependencies
            .push("httpx".into());
        let got = lines(&graph, "httpx");
        assert!(got.contains(&(2, "httpx".into(), true)), "{got:?}");
    }

    #[test]
    fn all_packages_lists_each_once_preferring_where_it_is_declared() {
        let row = |name: &str, declared: bool| DepRow {
            name: name.into(),
            constraint: None,
            version: Some("1".into()),
            declared,
        };
        let tab = DepsTab {
            groups: vec![
                DepGroup {
                    kind: GroupKind::Main,
                    rows: vec![
                        row("requests", true),
                        row("pytest", false),
                        row("idna", false),
                    ],
                },
                DepGroup {
                    kind: GroupKind::Group("test".into()),
                    rows: vec![row("pytest", true)],
                },
            ],
            graph: None,
            tree_note: None,
        };
        let got: Vec<(String, GroupKind, bool)> = all_packages(&tab)
            .into_iter()
            .map(|f| (f.row.name, f.group, f.row.declared))
            .collect();
        assert_eq!(
            got,
            vec![
                ("idna".into(), GroupKind::Main, false),
                ("pytest".into(), GroupKind::Group("test".into()), true),
                ("requests".into(), GroupKind::Main, true),
            ]
        );
    }

    #[test]
    fn find_id_by_name_locates_a_package_case_insensitively() {
        let graph = graph_with_chain();
        assert_eq!(find_id_by_name(&graph, "HTTPX"), Some("httpx"));
        assert_eq!(find_id_by_name(&graph, "nonexistent"), None);
    }

    // --- state: the glyph mapping for one dependency row ---

    fn row(constraint: Option<&str>, version: Option<&str>) -> DepRow {
        DepRow {
            name: "httpx".to_string(),
            constraint: constraint.map(String::from),
            version: version.map(String::from),
            declared: true,
        }
    }

    #[test]
    fn a_declared_and_installed_package_satisfying_its_constraint_is_present() {
        assert_eq!(
            state(&row(Some(">=0.27"), Some("0.27.2"))),
            theme::State::Present
        );
    }

    #[test]
    fn a_declared_but_uninstalled_package_is_absent() {
        assert_eq!(state(&row(Some(">=0.27"), None)), theme::State::Absent);
    }

    #[test]
    fn an_installed_package_outside_its_declared_constraint_is_drifted() {
        // The bug this glyph exists to catch: installed at a version the
        // manifest forbids is a different problem from missing entirely.
        assert_eq!(
            state(&row(Some(">=0.27"), Some("0.26.0"))),
            theme::State::Drifted
        );
    }

    #[test]
    fn an_installed_package_with_no_declared_constraint_is_present() {
        // A leftover -- installed but not declared anywhere -- has nothing
        // to violate, so it reads as agreeing rather than drifted.
        assert_eq!(state(&row(None, Some("4.14.2"))), theme::State::Present);
    }

    #[test]
    fn an_unparseable_constraint_falls_back_to_present_not_drifted() {
        // Same fallback direction as `interpreter::version_satisfies` and
        // `sidebar::state`: a constraint this parser cannot read must never
        // read as "does not satisfy". Marking a working package drifted on a
        // parse failure would be a false alarm.
        //
        // The installed version must *violate* the constraint it cannot
        // parse. `("<3.13", "3.9.18")` -- what this asserted first --
        // discriminates nothing: 3.9.18 satisfies `<3.13`, so it reads
        // Present whether the fallback fired or `<` were implemented
        // tomorrow. `<3.5` can only read Present because the parse failed.
        assert_eq!(
            state(&row(Some("<3.5"), Some("3.9.18"))),
            theme::State::Present
        );
        assert_eq!(
            state(&row(Some("not a specifier"), Some("3.9.18"))),
            theme::State::Present
        );
    }
}
