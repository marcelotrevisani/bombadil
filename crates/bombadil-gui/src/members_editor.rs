//! The Members tab (spec §5a): every `pyproject.toml` discovered below a
//! project's own manifest, with a checkbox for whether it installs in
//! editable ("dev") mode.
//!
//! Three uv facts, verified against a real uv and not to be re-derived,
//! shape everything here:
//!
//! - Plain `uv sync` in a workspace installs NO members. "Everything
//!   selected" is specifically `--all-packages` (`uv::command::sync_all_members`).
//! - Re-syncing with a smaller `--package` set REMOVES the members no longer
//!   named. Deselecting a member in this tab and syncing is therefore not
//!   cosmetic -- it uninstalls something -- so [`removal_warning`] exists to
//!   say so before the user confirms (criterion 4).
//! - A member excluded by `[tool.uv.workspace] exclude` cannot be passed to
//!   `--package` at all: uv refuses and aborts the *whole* sync, not just
//!   that member. [`candidates`] is what keeps an excluded member off this
//!   tab entirely (criterion 5) -- `DiscoveredMember::is_workspace_member`
//!   is already `false` for it (`members::discover`'s own contract), so this
//!   is a filter, not a second exclusion check.
//!
//! # Why this module never re-derives selection from `MemberSelection` by hand
//!
//! `bombadil_core::members::effective` is the one place the precedence
//! between a saved deselection and "select everything discovered" is
//! decided (its own doc spells out the rule). [`build`] and [`toggle`] both
//! call it rather than inlining `selection.all_discovered` themselves
//! (criterion 3) -- this project has already shipped that rule twice once,
//! as prose in a doc comment and as GUI logic that drifted from it.
//!
//! # Why the installed join is on path, not name
//!
//! `InstalledPackage::editable_project_location` is the directory uv
//! installed the package from -- a path, not a name. A discovered member
//! with no `[project].name` still has a path, so [`is_installed`] joins on
//! that (criterion 1). Joining on name instead would silently drop every
//! nameless member from the "already installed" state -- exactly the
//! monorepo case the plan explicitly supports, since a directory can hold a
//! `pyproject.toml` with no `[project]` table at all (tooling configuration
//! only) and still be a real, previously-`uv add --editable`'d sibling.

use crate::app::Message;
use crate::theme;
use bombadil_core::members::{self, DiscoveredMember, WorkspaceKind};
use bombadil_core::model::{MemberOrigin, MemberRef, MemberSelection};
use bombadil_core::uv::results::InstalledPackage;
use std::path::{Path, PathBuf};

/// One checklist row: a discovered member with the saved selection and the
/// installed state applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRow {
    /// Relative to the project directory for a discovered member, absolute
    /// for one added from outside the tree.
    pub path: PathBuf,
    /// `None` when the manifest declares no `[project].name` -- such a
    /// member can never be passed to `--package` (`members::selected_package_names`
    /// already filters it out), so the checklist shows it but disabled.
    pub name: Option<String>,
    pub origin: MemberOrigin,
    pub selected: bool,
    /// Whether uv currently reports this member installed in editable mode,
    /// joined on path (criterion 1) -- see the module doc.
    pub installed: bool,
}

/// Everything the Members tab shows for one project.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MembersTab {
    pub rows: Vec<MemberRow>,
}

/// Which discovered members are ever offered as checklist rows (criterion 5).
///
/// For a declared workspace, only members the workspace's own `members`
/// globs select and its `exclude` list does not remove --
/// `DiscoveredMember::is_workspace_member`, computed once by
/// `members::discover` itself. Offering an excluded one would let the user
/// select it, and passing an excluded member to `--package` fails and
/// aborts the *entire* sync, not just that member.
///
/// For a loose project (no `[tool.uv.workspace]` at all)
/// `is_workspace_member` is `false` on every discovered entry by
/// `members::discover`'s own contract, so filtering by it here would hide
/// every candidate. A loose tree has no workspace to exclude anyone from, so
/// every discovered `pyproject.toml` is offered.
fn candidates(discovered: &[DiscoveredMember], kind: &WorkspaceKind) -> Vec<DiscoveredMember> {
    match kind {
        WorkspaceKind::Workspace { .. } => discovered
            .iter()
            .filter(|m| m.is_workspace_member)
            .cloned()
            .collect(),
        WorkspaceKind::Loose => discovered.to_vec(),
    }
}

/// Whether `member_path` (relative for a discovered member, absolute for one
/// added from outside the tree) is currently installed in editable mode.
///
/// Joined on path, never on name (criterion 1) -- see the module doc for why
/// a name-keyed join would silently drop every nameless member.
pub fn is_installed(
    project_dir: &Path,
    member_path: &Path,
    installed: &[InstalledPackage],
) -> bool {
    let absolute = if member_path.is_absolute() {
        member_path.to_path_buf()
    } else {
        project_dir.join(member_path)
    };
    installed
        .iter()
        .any(|pkg| pkg.editable_project_location.as_deref() == Some(absolute.as_path()))
}

/// Builds the checklist: every offered member (criterion 5), with the saved
/// selection applied through `members::effective` -- the one call site for
/// that precedence rule (criterion 3) -- and joined against what uv reports
/// installed (criterion 1). All-selected-by-default (criterion 2) falls out
/// of `effective` itself: a fresh `MemberSelection::default()` has
/// `all_discovered: true` and no entries, which `effective` reports as every
/// discovered member selected.
pub fn build(
    project_dir: &Path,
    selection: &MemberSelection,
    kind: &WorkspaceKind,
    discovered: &[DiscoveredMember],
    installed: &[InstalledPackage],
) -> MembersTab {
    let candidates = candidates(discovered, kind);
    let effective = members::effective(selection, &candidates);
    let rows = effective
        .into_iter()
        .map(|m| MemberRow {
            installed: is_installed(project_dir, &m.path, installed),
            path: m.path,
            name: m.name,
            origin: m.origin,
            selected: m.selected,
        })
        .collect();
    MembersTab { rows }
}

/// Rows syncing right now would remove (criterion 4): installed in editable
/// mode, but no longer selected. A row that was never installed carries no
/// removal risk even if deselected -- there is nothing there for uv to drop.
pub fn will_remove(rows: &[MemberRow]) -> Vec<&MemberRow> {
    rows.iter().filter(|r| r.installed && !r.selected).collect()
}

/// The label a row is identified by in [`removal_warning`] and the view:
/// its name, or its path when it has none.
fn display_label(row: &MemberRow) -> String {
    row.name
        .clone()
        .unwrap_or_else(|| row.path.display().to_string())
}

/// The sentence a sync confirmation states before anything runs (criterion
/// 4): which members would be removed, and why. `None` when nothing would be
/// removed, so the caller can skip confirmation and sync straight away --
/// same shape as `deps::DepsTab::tree_note`, which is also only `Some` when
/// there is something to say.
pub fn removal_warning(rows: &[MemberRow]) -> Option<String> {
    let removed = will_remove(rows);
    if removed.is_empty() {
        return None;
    }
    let names: Vec<String> = removed.iter().map(|r| display_label(r)).collect();
    Some(format!(
        "Syncing now will remove the following from the environment, since they are no longer selected: {}.",
        names.join(", ")
    ))
}

/// Flips one row's selection (a checkbox press).
///
/// Reads the row's *current effective* state through `members::effective`
/// rather than re-deriving it from `selection.all_discovered` by hand
/// (criterion 3), then records the flip as an explicit entry so it survives
/// rediscovery -- the same precedence `members::effective`'s own doc
/// describes. A path with no matching candidate (a stale press, or a path
/// that discovery no longer reports) is a no-op.
pub fn toggle(
    selection: &mut MemberSelection,
    discovered: &[DiscoveredMember],
    kind: &WorkspaceKind,
    path: &Path,
) {
    let candidates = candidates(discovered, kind);
    let effective = members::effective(selection, &candidates);
    let Some(current) = effective.iter().find(|m| m.path == path) else {
        return;
    };
    let new_selected = !current.selected;
    if let Some(entry) = selection.entries.iter_mut().find(|e| e.path == path) {
        entry.selected = new_selected;
    } else {
        selection.entries.push(MemberRef {
            path: path.to_path_buf(),
            origin: current.origin,
            selected: new_selected,
            name: current.name.clone(),
        });
    }
}

/// Which `uv sync` shape a project's current selection maps to.
///
/// `Plain` covers an ordinary, non-monorepo project: no candidates and
/// nothing saved in `entries` either, so there is nothing to select and the
/// member machinery must not touch the sync command at all. `AllPackages`
/// is "select everything" expressed the way a real uv actually requires it
/// (`--all-packages`, never a hand-assembled list of every name -- plain
/// `uv sync` installs no members and there is no reason to believe listing
/// every member by hand behaves identically to uv's own all-packages path).
/// Anything less than everything is `Named`, exactly the packages `uv sync
/// --package ...` should install -- and, since it omits whatever was just
/// deselected, exactly what makes deselecting one and re-syncing remove it
/// (criterion 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncPlan {
    Plain,
    AllPackages,
    Named(Vec<String>),
}

pub fn sync_plan(
    selection: &MemberSelection,
    kind: &WorkspaceKind,
    discovered: &[DiscoveredMember],
) -> SyncPlan {
    let candidates = candidates(discovered, kind);
    if candidates.is_empty() && selection.entries.is_empty() {
        return SyncPlan::Plain;
    }
    let effective = members::effective(selection, &candidates);
    if !effective.is_empty() && effective.iter().all(|m| m.selected) {
        SyncPlan::AllPackages
    } else {
        SyncPlan::Named(members::selected_package_names(&effective))
    }
}

/// The pending "sync would remove members" confirmation (criterion 4),
/// mirroring `context_menu::ConfirmRecreate`'s shape: opened by a press,
/// resolved only by `Message::MembersSyncConfirmed` or
/// `Message::MembersSyncCancelled`, nothing dispatched until the user
/// answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmMemberSync {
    pub project_index: usize,
    pub label: String,
    /// Captured when the confirmation opened, from the same `removal_warning`
    /// call that decided one was needed -- so the sentence the user agrees
    /// to and the sync that runs after describe the same selection, not two
    /// derivations that could drift apart between the press and the confirm.
    pub warning: String,
}

/// Maps one member row to the glyph state it should render.
///
/// - Not installed: [`theme::State::Absent`], whether it is currently
///   selected (nothing installed yet for a next sync to disagree with) or
///   deselected (discovered, deselected -- the plain case).
/// - Installed and still selected: [`theme::State::Present`] -- installed
///   editable, and the next sync agrees.
/// - Installed but no longer selected: [`theme::State::Drifted`] -- exactly
///   the row [`will_remove`] flags: syncing now would uninstall it. Kept as
///   the same underlying fact `will_remove`/`removal_warning` already prove,
///   not a second derivation of "would removing this be a surprise."
pub fn state(row: &MemberRow) -> theme::State {
    if !row.installed {
        theme::State::Absent
    } else if row.selected {
        theme::State::Present
    } else {
        theme::State::Drifted
    }
}

/// Renders the pending sync confirmation. Deliberately dumb, like
/// `context_menu::confirm_view`: the sentence worth testing is
/// `removal_warning`'s.
pub fn confirm_view<'a>(confirm: &ConfirmMemberSync) -> iced::Element<'a, Message> {
    iced::widget::column![
        iced::widget::text(format!("{}\n\n{}", confirm.label, confirm.warning)),
        iced::widget::button(iced::widget::text("Sync anyway"))
            .on_press(Message::MembersSyncConfirmed),
        iced::widget::button(iced::widget::text("Cancel")).on_press(Message::MembersSyncCancelled),
    ]
    .spacing(theme::SPACE_2)
    .into()
}

/// Renders one member row: the state glyph, then the checkbox whose label is
/// the member's name or path -- data, compared character by character like
/// any other path in this application, so it renders in Plex rather than the
/// checkbox's default prose font. The row sits on a `bark` raised surface,
/// the same treatment every row in every pane gets.
///
/// The "cannot be installed" note is a separate widget rather than a suffix
/// on the checkbox's own label: appended, it would inherit the checkbox's
/// Plex, and it is a sentence about the member, not a value to compare.
fn member_row<'a>(row: &MemberRow, project_index: usize) -> iced::Element<'a, Message> {
    let glyph = theme::state_glyph(state(row));

    let path = row.path.clone();
    let checkbox = iced::widget::checkbox(row.selected)
        .label(display_label(row))
        .font(theme::FONT_DATA)
        .text_size(theme::DATA)
        .on_toggle_maybe(
            row.name
                .is_some()
                .then_some(move |_| Message::MemberToggled(project_index, path.clone())),
        );

    let mut line = iced::widget::row![
        iced::widget::text(glyph.glyph.to_string())
            .size(theme::BODY)
            .color(glyph.colour)
            .width(theme::GLYPH_COLUMN_WIDTH),
        checkbox,
    ]
    .spacing(theme::SPACE_1);

    if row.name.is_none() {
        line = line.push(
            iced::widget::text("(no [project].name -- cannot be installed)")
                .font(theme::FONT_PROSE)
                .size(theme::DATA)
                .color(theme::SLATE),
        );
    }

    iced::widget::container(line)
        .padding(theme::SPACE_1)
        .style(|_theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(theme::BARK)),
            ..iced::widget::container::Style::default()
        })
        .into()
}

/// Renders the checklist. Deliberately dumb, like every other `view` in this
/// crate: all the logic worth testing lives in the functions above.
pub fn view<'a>(tab: &MembersTab, project_index: usize) -> iced::Element<'a, Message> {
    let mut column = iced::widget::column![
        iced::widget::text("Members")
            .font(theme::FONT_PROSE_SEMIBOLD)
            .size(theme::TITLE)
    ]
    .spacing(theme::SPACE_2);

    if tab.rows.is_empty() {
        column = column.push(
            iced::widget::text("No additional pyproject.toml files found below this project.")
                .size(theme::BODY)
                .color(theme::SLATE),
        );
    }

    for row in &tab.rows {
        column = column.push(member_row(row, project_index));
    }

    if let Some(warning) = removal_warning(&tab.rows) {
        // One of `BOOT`'s rationed cases by name: a member syncing will
        // remove.
        column = column.push(
            iced::widget::text(warning)
                .size(theme::BODY)
                .color(theme::BOOT),
        );
    }

    column = column.push(
        iced::widget::button(iced::widget::text("Sync members"))
            .on_press(Message::MembersSyncRequested(project_index)),
    );

    column.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovered(path: &str, name: Option<&str>, is_workspace_member: bool) -> DiscoveredMember {
        DiscoveredMember {
            relative_path: PathBuf::from(path),
            name: name.map(String::from),
            is_workspace_member,
        }
    }

    fn pkg(path: &str) -> InstalledPackage {
        InstalledPackage {
            name: "irrelevant-for-the-join".to_string(),
            version: "1.0".to_string(),
            editable_project_location: Some(PathBuf::from(path)),
        }
    }

    fn workspace(globs: &[&str], exclude: &[&str]) -> WorkspaceKind {
        WorkspaceKind::Workspace {
            member_globs: globs.iter().map(|s| s.to_string()).collect(),
            exclude_globs: exclude.iter().map(|s| s.to_string()).collect(),
        }
    }

    // --- criterion 1: joined on PATH, not name ---

    #[test]
    fn a_nameless_member_is_still_reported_installed_when_its_path_matches() {
        // The exact case the monorepo plan supports: a pyproject.toml with no
        // [project] table, previously installed editable, must not be
        // silently dropped by a name-keyed join -- it has no name to key on.
        let installed = vec![pkg("/work/repo/tooling")];
        let got = is_installed(Path::new("/work/repo"), Path::new("tooling"), &installed);
        assert!(got, "a nameless member must still join on its path");
    }

    #[test]
    fn a_named_member_at_a_different_path_is_not_reported_installed() {
        let installed = vec![pkg("/work/repo/packages/other")];
        let got = is_installed(
            Path::new("/work/repo"),
            Path::new("packages/alpha"),
            &installed,
        );
        assert!(!got);
    }

    #[test]
    fn an_added_members_absolute_path_is_used_as_is() {
        let installed = vec![pkg("/work/shared-lib")];
        let got = is_installed(
            Path::new("/work/repo"),
            Path::new("/work/shared-lib"),
            &installed,
        );
        assert!(
            got,
            "an absolute member path must not be re-joined onto project_dir"
        );
    }

    #[test]
    fn build_marks_a_nameless_discovered_member_installed_via_the_path_join() {
        let discovered_members = [discovered("tooling", None, true)];
        let installed = vec![pkg("/work/repo/tooling")];
        let tab = build(
            Path::new("/work/repo"),
            &MemberSelection::default(),
            &workspace(&["*"], &[]),
            &discovered_members,
            &installed,
        );
        let row = tab.rows.first().expect("one row");
        assert_eq!(row.name, None);
        assert!(
            row.installed,
            "a nameless member's installed state must still be reported, joined by path"
        );
    }

    // --- criterion 2: all discovered members selected by default ---

    #[test]
    fn every_discovered_member_is_selected_by_default() {
        let discovered_members = [
            discovered("packages/alpha", Some("alpha"), true),
            discovered("packages/beta", Some("beta"), true),
        ];
        let tab = build(
            Path::new("/work/repo"),
            &MemberSelection::default(),
            &workspace(&["packages/*"], &[]),
            &discovered_members,
            &[],
        );
        assert_eq!(tab.rows.len(), 2, "got {:?}", tab.rows);
        assert!(
            tab.rows.iter().all(|r| r.selected),
            "a fresh selection must not leave anything deselected -- an empty \
             default means uv sync installs no members at all; got {:?}",
            tab.rows
        );
    }

    // --- criterion 3: members::effective decides, not a second copy of the rule ---

    #[test]
    fn an_explicit_deselection_survives_even_though_all_discovered_is_true() {
        // The precedence `members::effective` documents: a deliberate
        // deselection wins over "select everything discovered." If `build`
        // reimplemented the rule as `selected: selection.all_discovered`
        // instead of calling `effective`, this row would come back selected.
        let discovered_members = [discovered("packages/alpha", Some("alpha"), true)];
        let selection = MemberSelection {
            all_discovered: true,
            entries: vec![MemberRef {
                path: PathBuf::from("packages/alpha"),
                origin: MemberOrigin::Discovered,
                selected: false,
                name: None,
            }],
        };
        let tab = build(
            Path::new("/work/repo"),
            &selection,
            &workspace(&["packages/*"], &[]),
            &discovered_members,
            &[],
        );
        assert!(!tab.rows[0].selected, "got {:?}", tab.rows);
    }

    #[test]
    fn toggle_reads_the_current_effective_state_before_flipping_it() {
        let discovered_members = [discovered("packages/alpha", Some("alpha"), true)];
        let kind = workspace(&["packages/*"], &[]);
        let mut selection = MemberSelection::default();

        toggle(
            &mut selection,
            &discovered_members,
            &kind,
            Path::new("packages/alpha"),
        );

        let tab = build(
            Path::new("/work/repo"),
            &selection,
            &kind,
            &discovered_members,
            &[],
        );
        assert!(
            !tab.rows[0].selected,
            "toggling a selected-by-default member must deselect it"
        );

        toggle(
            &mut selection,
            &discovered_members,
            &kind,
            Path::new("packages/alpha"),
        );
        let tab = build(
            Path::new("/work/repo"),
            &selection,
            &kind,
            &discovered_members,
            &[],
        );
        assert!(tab.rows[0].selected, "toggling again must reselect it");
    }

    // --- criterion 4: deselecting and syncing removes it, and the UI says so first ---

    #[test]
    fn a_deselected_installed_member_is_reported_as_something_syncing_will_remove() {
        let rows = vec![MemberRow {
            path: PathBuf::from("packages/alpha"),
            name: Some("alpha".to_string()),
            origin: MemberOrigin::Discovered,
            selected: false,
            installed: true,
        }];
        let warning = removal_warning(&rows).expect("must warn before the user confirms");
        assert!(warning.contains("alpha"), "got {warning:?}");
    }

    #[test]
    fn a_deselected_member_that_was_never_installed_is_not_reported_as_removed() {
        // No false alarm: nothing there for uv to drop.
        let rows = vec![MemberRow {
            path: PathBuf::from("packages/alpha"),
            name: Some("alpha".to_string()),
            origin: MemberOrigin::Discovered,
            selected: false,
            installed: false,
        }];
        assert!(removal_warning(&rows).is_none());
    }

    #[test]
    fn a_selected_installed_member_is_not_reported_as_removed() {
        let rows = vec![MemberRow {
            path: PathBuf::from("packages/alpha"),
            name: Some("alpha".to_string()),
            origin: MemberOrigin::Discovered,
            selected: true,
            installed: true,
        }];
        assert!(removal_warning(&rows).is_none());
    }

    #[test]
    fn deselecting_a_member_shrinks_the_named_sync_plan_to_omit_it() {
        // The real mechanism criterion 4 leans on: re-syncing with a smaller
        // --package set removes whatever is no longer named. If sync_plan
        // did not actually drop a deselected member's name, this would still
        // list it and the sync would not remove anything.
        let discovered_members = [
            discovered("packages/alpha", Some("alpha"), true),
            discovered("packages/beta", Some("beta"), true),
        ];
        let kind = workspace(&["packages/*"], &[]);
        let mut selection = MemberSelection::default();
        toggle(
            &mut selection,
            &discovered_members,
            &kind,
            Path::new("packages/alpha"),
        );

        match sync_plan(&selection, &kind, &discovered_members) {
            SyncPlan::Named(names) => {
                assert_eq!(names, vec!["beta".to_string()], "got {names:?}");
            }
            other => panic!("expected Named, got {other:?}"),
        }
    }

    #[test]
    fn selecting_everything_syncs_with_all_packages_not_a_hand_assembled_list() {
        let discovered_members = [
            discovered("packages/alpha", Some("alpha"), true),
            discovered("packages/beta", Some("beta"), true),
        ];
        let kind = workspace(&["packages/*"], &[]);
        let selection = MemberSelection::default();

        assert_eq!(
            sync_plan(&selection, &kind, &discovered_members),
            SyncPlan::AllPackages
        );
    }

    #[test]
    fn an_ordinary_project_with_no_members_at_all_needs_no_member_aware_sync() {
        let selection = MemberSelection::default();
        assert_eq!(
            sync_plan(&selection, &WorkspaceKind::Loose, &[]),
            SyncPlan::Plain
        );
    }

    // --- criterion 5: an excluded member is not offered ---

    #[test]
    fn an_excluded_workspace_member_never_becomes_a_row() {
        let discovered_members = [
            discovered("packages/alpha", Some("alpha"), true),
            // Excluded: members::discover already computed is_workspace_member
            // false for it, the same as classifies_workspace_members_against_the_globs
            // proves at the discover layer.
            discovered("packages/skipme", Some("skipme"), false),
        ];
        let tab = build(
            Path::new("/work/repo"),
            &MemberSelection::default(),
            &workspace(&["packages/*"], &["packages/skipme"]),
            &discovered_members,
            &[],
        );
        let names: Vec<Option<String>> = tab.rows.iter().map(|r| r.name.clone()).collect();
        assert_eq!(
            names,
            vec![Some("alpha".to_string())],
            "an excluded member must never be offered -- passing it to --package \
             would abort the entire sync; got {names:?}"
        );
    }

    #[test]
    fn an_excluded_member_never_appears_in_the_package_list_even_with_a_stale_selection_entry() {
        // Defence in depth: even if a stale "selected" entry exists for a
        // path that is now excluded (e.g. the workspace's exclude list
        // changed after the entry was saved), it must not resurrect the
        // member into the --package list. `alpha` is deselected so the plan
        // takes the `Named` branch rather than `AllPackages`, which is what
        // makes a leaked `skipme` name observable at all.
        let discovered_members = [
            discovered("packages/alpha", Some("alpha"), true),
            discovered("packages/beta", Some("beta"), true),
            discovered("packages/skipme", Some("skipme"), false),
        ];
        let kind = workspace(&["packages/*"], &["packages/skipme"]);
        let selection = MemberSelection {
            all_discovered: true,
            entries: vec![
                MemberRef {
                    path: PathBuf::from("packages/alpha"),
                    origin: MemberOrigin::Discovered,
                    selected: false,
                    name: Some("alpha".to_string()),
                },
                MemberRef {
                    path: PathBuf::from("packages/skipme"),
                    origin: MemberOrigin::Discovered,
                    selected: true,
                    name: Some("skipme".to_string()),
                },
            ],
        };

        match sync_plan(&selection, &kind, &discovered_members) {
            SyncPlan::Named(names) => {
                assert_eq!(names, vec!["beta".to_string()], "got {names:?}");
            }
            other => panic!("expected Named, got {other:?}"),
        }
    }

    #[test]
    fn a_loose_project_offers_every_discovered_manifest_despite_is_workspace_member_being_false() {
        // members::discover always reports is_workspace_member: false for a
        // loose project (there is no workspace to be a member of) -- filtering
        // on that flag the same way the Workspace branch does would hide
        // every candidate for the common non-workspace monorepo case.
        let discovered_members = [
            discovered("services/api", Some("api"), false),
            discovered("services/worker", Some("worker"), false),
        ];
        let tab = build(
            Path::new("/work/repo"),
            &MemberSelection::default(),
            &WorkspaceKind::Loose,
            &discovered_members,
            &[],
        );
        assert_eq!(tab.rows.len(), 2, "got {:?}", tab.rows);
    }

    // --- state: the glyph mapping for one member row ---

    fn member_row(selected: bool, installed: bool) -> MemberRow {
        MemberRow {
            path: PathBuf::from("packages/alpha"),
            name: Some("alpha".to_string()),
            origin: MemberOrigin::Discovered,
            selected,
            installed,
        }
    }

    #[test]
    fn a_selected_installed_member_is_present() {
        assert_eq!(state(&member_row(true, true)), theme::State::Present);
    }

    #[test]
    fn a_discovered_deselected_never_installed_member_is_absent() {
        assert_eq!(state(&member_row(false, false)), theme::State::Absent);
    }

    #[test]
    fn a_selected_not_yet_installed_member_is_absent() {
        // Mirrors deps' "declared, not installed": nothing installed yet for
        // a next sync to disagree with.
        assert_eq!(state(&member_row(true, false)), theme::State::Absent);
    }

    #[test]
    fn a_deselected_but_still_installed_member_is_drifted() {
        // The exact case `will_remove`/`removal_warning` exist to flag:
        // syncing now would uninstall this member.
        assert_eq!(state(&member_row(false, true)), theme::State::Drifted);
    }
}
