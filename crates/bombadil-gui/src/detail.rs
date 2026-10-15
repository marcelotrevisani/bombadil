//! The detail pane header: identifies the selected project, its path, venv
//! status and Python version, and offers the Sync action.
//!
//! Spec §8: the header carries the project name, its path, venv status,
//! Python version, and a Sync action. A project registered without a venv is
//! a normal state (see `sidebar`) -- creating the environment is how the user
//! recovers from it, so the Sync action is never gated on the venv already
//! existing. `view` does not branch on `venv_status` to decide whether to
//! show the Sync button; it is always rendered, which is what keeps this
//! criterion true by construction rather than by a check someone could
//! accidentally remove.

use crate::app::Message;
use crate::icons::{self, Icon};
use crate::shortcuts::{self, Shortcut};
use crate::theme;
use bombadil_core::model::Project;
use std::path::{Path, PathBuf};

/// Whether the project's virtual environment exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenvStatus {
    Present,
    Absent,
}

/// Everything the detail pane header shows for the selected project.
pub struct Header {
    pub label: String,
    /// The project's directory -- what a user recognises -- never the
    /// `pyproject.toml` file inside it.
    pub path: PathBuf,
    pub venv_status: VenvStatus,
    pub python_version: Option<String>,
    /// Where the *active* environment resolves to.
    ///
    /// Shown beneath the project's own path, because a project can have
    /// several environments now and the header describes the one Sync and
    /// Open terminal act on -- which the project path alone cannot say. `None`
    /// for a project with no environments at all.
    pub venv_path: Option<PathBuf>,
    /// Set by the caller from the probed sidebar entry.
    pub git: Option<bombadil_core::git::GitStatus>,
}

/// Builds the header for `project`.
///
/// `venv_probe` is a parameter rather than a filesystem call, so this is
/// testable without touching disk. The real caller in `app::view` answers
/// from `App`'s already-probed `entries` cache rather than touching disk
/// again on every frame.
///
/// `venv_path` likewise comes from the caller: resolving it needs `Settings`,
/// which this function deliberately does not take -- one resolver, called
/// where the config already is.
pub fn header(
    project: &Project,
    venv_path: Option<PathBuf>,
    venv_probe: &dyn Fn(&Project) -> Option<String>,
) -> Header {
    let python_version = venv_probe(project);
    let venv_status = if python_version.is_some() {
        VenvStatus::Present
    } else {
        VenvStatus::Absent
    };

    Header {
        label: project.label.clone(),
        path: project_dir(project),
        venv_status,
        python_version,
        venv_path,
        git: None,
    }
}

/// One line for the header: `main · ↑1 ↓2 · 2 modified`, or `main → fork/trunk · clean`
/// when the upstream has another name.
pub fn git_summary(git: &bombadil_core::git::GitStatus) -> String {
    let mut parts = vec![match (&git.upstream, git.detached) {
        (_, true) => format!("detached at {}", git.branch),
        // `origin/<same name>` is the usual case; naming the branch twice
        // only wrapped the tile.
        (Some(upstream), false)
            if upstream
                .split_once('/')
                .is_some_and(|(_, name)| name == git.branch) =>
        {
            git.branch.clone()
        }
        (Some(upstream), false) => format!("{} \u{2192} {upstream}", git.branch),
        (None, false) => format!("{} (no upstream)", git.branch),
    }];
    let ahead_behind: Vec<String> = [("\u{2191}", git.ahead), ("\u{2193}", git.behind)]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(arrow, n)| format!("{arrow}{n}"))
        .collect();
    if !ahead_behind.is_empty() {
        parts.push(ahead_behind.join(" "));
    }
    let changes: Vec<String> = [
        (git.staged, "staged"),
        (git.modified, "modified"),
        (git.untracked, "untracked"),
        (git.conflicted, "conflicted"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, kind)| format!("{n} {kind}"))
    .collect();
    parts.push(if changes.is_empty() {
        "clean".to_string()
    } else {
        changes.join(", ")
    });
    parts.join(" \u{b7} ")
}

/// One tile of the header's summary card.
pub struct Tile {
    pub label: &'static str,
    pub state: theme::State,
    pub value: String,
    /// Where pressing the tile goes, if anywhere.
    pub press: Option<Message>,
    /// A Sync button inside the tile, where the drift it fixes is shown.
    pub sync: Option<Message>,
}

/// The Environment tile's line. A drifted environment says which
/// `requires-python` its Python misses, so the fix is in the sentence.
pub fn env_value(
    status: VenvStatus,
    python: Option<&String>,
    state: theme::State,
    requires_python: Option<&str>,
) -> String {
    match (status, python) {
        // `header` derives Present from a probed version, so Present always
        // carries one; the `None` case reads as no environment too.
        (VenvStatus::Absent, _) | (VenvStatus::Present, None) => "no environment".to_string(),
        (VenvStatus::Present, Some(version)) => match (state, requires_python) {
            (theme::State::Drifted, Some(req)) => {
                format!("Python {version} does not satisfy {req}")
            }
            _ => format!("Python {version}"),
        },
    }
}

/// The state that needs the most attention: drifted, then absent, then
/// present (`State`'s declaration order). Nothing at all is present.
pub fn worst(states: impl IntoIterator<Item = theme::State>) -> theme::State {
    states.into_iter().max().unwrap_or(theme::State::Present)
}

/// The Dependencies tile: counts, zero ones left out, and the worst glyph.
/// A package declared in two groups is one package.
pub fn deps_summary(tab: &crate::deps::DepsTab) -> (theme::State, String) {
    let mut by_name: std::collections::BTreeMap<&str, theme::State> = Default::default();
    for row in tab.groups.iter().flat_map(|g| &g.rows) {
        let state = crate::deps::state(row);
        let slot = by_name.entry(row.name.as_str()).or_insert(state);
        *slot = worst([*slot, state]);
    }
    let count = |want: theme::State| by_name.values().filter(|s| **s == want).count();
    let installed = by_name.len() - count(theme::State::Absent);
    let parts: Vec<String> = [
        (installed, "installed"),
        (count(theme::State::Drifted), "drifted"),
        (count(theme::State::Absent), "missing"),
    ]
    .into_iter()
    .enumerate()
    .filter(|(i, (n, _))| *i == 0 || *n > 0)
    .map(|(_, (n, what))| format!("{n} {what}"))
    .collect();
    (worst(by_name.values().copied()), parts.join(" \u{b7} "))
}

/// The Members tile, for a project that has members: how many of the wanted
/// ones are installed. Drifted when a sync would remove one; absent when a
/// wanted one is missing. A deselected member that is not installed is
/// exactly as asked.
pub fn members_summary(
    rows: &[crate::members_editor::MemberRow],
) -> Option<(theme::State, String)> {
    if rows.is_empty() {
        return None;
    }
    let wanted = rows.iter().filter(|r| r.selected).count();
    let installed = rows.iter().filter(|r| r.selected && r.installed).count();
    let state = if rows.iter().any(|r| r.installed && !r.selected) {
        theme::State::Drifted
    } else if installed < wanted {
        theme::State::Absent
    } else {
        theme::State::Present
    };
    Some((state, format!("{installed} of {wanted} installed")))
}

/// The project's directory: `pyproject_path`'s parent, or the path itself
/// when it has none (a bare filename with no directory component) -- the
/// same fallback `venv_path::resolve` uses for the same edge case.
fn project_dir(project: &Project) -> PathBuf {
    project
        .pyproject_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| project.pyproject_path.clone())
}

/// Renders the header. Deliberately dumb: all the logic worth testing lives
/// in `header` -- this crate's convention is that `view` itself carries no
/// test, since it returns an opaque `Element` no test can drive. The Sync
/// button is unconditional -- it does not read `venv_status` to decide
/// whether to render at all.
///
/// Two rows, both right-anchored: the project label (prose, `DISPLAY` --
/// the largest thing on screen) sits across from the Python version, and
/// the path beneath it sits across from Sync. The project label and Sync's
/// own label are the only words here, so both stay in the prose face at
/// their default colour; the path and the version are read character by
/// character, so both are Plex at `DATA` in `slate` -- the same rule
/// `sidebar` already applies to a project's path and version, held here too.
///
/// Sync is `theme::button_primary` -- the one filled control in this view,
/// and the reason the tokens live in `theme` rather than here: an unstyled
/// iced button also paints from the palette's primary, but it puts parchment
/// on it, which measures 3.1:1. `button_primary` uses ink text for 5.4:1.
///
/// `index` is the project's position in `App::config.projects`, carried into
/// `Message::SyncRequested` so one message shape serves this button and the
/// context menu's Sync entry -- see that message's own doc for why resolving
/// against the selection instead is a bug.
/// `busy` is the running command, if any: its label, the spinner angle, and
/// whether it is this project's.
pub fn view<'a>(
    header: &Header,
    index: usize,
    busy: Option<(String, f32, bool)>,
    tiles: Vec<Tile>,
) -> iced::Element<'a, Message> {
    // The one primary on this surface, so it keeps its label beside the icon.
    // While a command runs it is disabled: the running one shows the spinner
    // and what it is doing; any other says what it is waiting for. Disabled
    // primary has no fill, so its icon turns `slate` -- `ink` would vanish.
    let sync_row = |glyph: iced::Element<'a, Message>, label: String| {
        iced::widget::row![glyph, iced::widget::text(label).size(theme::BODY)]
            .spacing(theme::SPACE_1)
            .align_y(iced::Alignment::Center)
    };
    let sync: iced::Element<'a, Message> = match busy {
        None => theme::tip(
            iced::widget::button(sync_row(
                icons::icon(Icon::Refresh, 14.0, theme::Swatch::Ink),
                "Sync".to_string(),
            ))
            .on_press(Message::SyncRequested(index))
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(theme::button_primary),
            format!("Sync ({})", shortcuts::keys(Shortcut::Sync)),
            iced::widget::tooltip::Position::Left,
        ),
        Some((label, angle, true)) => iced::widget::button(sync_row(
            icons::spinner(angle, 14.0, theme::Swatch::Slate),
            label,
        ))
        .padding([theme::SPACE_1, theme::SPACE_3])
        .style(theme::button_primary)
        .into(),
        Some((label, _, false)) => iced::widget::tooltip(
            iced::widget::button(sync_row(
                icons::icon(Icon::Refresh, 14.0, theme::Swatch::Slate),
                "Sync".to_string(),
            ))
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(theme::button_primary),
            iced::widget::container(
                iced::widget::text(format!("Wait for {label} to finish")).size(theme::LABEL),
            )
            .padding(theme::SPACE_1)
            .style(theme::panel),
            iced::widget::tooltip::Position::Left,
        )
        .into(),
    };

    // Lucide icons with tooltips. PyCharm gets a generic "code" mark: a
    // JetBrains logo is not ours to draw. Its tooltip names the interpreter
    // because that is the part a user would otherwise go and set by hand.
    let terminal = icons::icon_button(
        Icon::Terminal,
        format!(
            "Open a terminal here, with the environment activated ({})",
            shortcuts::keys(Shortcut::Terminal)
        ),
        Some(Message::OpenTerminalHereRequested(index)),
    );
    let folder = icons::icon_button(
        Icon::Folder,
        "Open the project folder",
        Some(Message::OpenFolderRequested(index)),
    );
    let pycharm = icons::icon_button(
        Icon::Code,
        "Open in PyCharm, in this folder, with this environment as the interpreter",
        Some(Message::OpenPycharmRequested(index)),
    );

    let title_row = iced::widget::row![
        iced::widget::text(header.label.clone())
            .font(theme::FONT_PROSE_SEMIBOLD)
            .size(theme::DISPLAY),
        iced::widget::Space::new().width(iced::Length::Fill),
        folder,
        pycharm,
        terminal,
        sync,
    ]
    .spacing(theme::SPACE_2)
    .align_y(iced::Alignment::Center);

    // Status first, in one card; drift shows here without opening a tab.
    let mut card = iced::widget::row![].spacing(theme::SPACE_4);
    for tile in tiles {
        let glyph = theme::state_glyph(tile.state);
        let body = iced::widget::column![
            iced::widget::text(tile.label)
                .size(theme::LABEL)
                .style(theme::text_muted),
            iced::widget::row![
                theme::tip(
                    iced::widget::text(glyph.glyph.to_string())
                        .size(theme::BODY)
                        .style(theme::tinted(glyph.swatch)),
                    theme::state_meaning(tile.state),
                    iced::widget::tooltip::Position::Bottom,
                ),
                iced::widget::text(tile.value).size(theme::BODY),
            ]
            .spacing(theme::SPACE_1)
            .align_y(iced::Alignment::Center),
        ]
        .spacing(theme::SPACE_1);
        let body = iced::widget::row![body]
            .push(tile.sync.map(|message| {
                iced::widget::button(iced::widget::text("Sync").size(theme::LABEL))
                    .on_press(message)
                    .padding([0.0, theme::SPACE_2])
                    .style(theme::button_primary)
            }))
            .spacing(theme::SPACE_2)
            .align_y(iced::Alignment::End);
        card = card.push(match tile.press {
            Some(message) => iced::Element::from(
                iced::widget::button(body)
                    .on_press(message)
                    .padding(0)
                    .style(theme::button_bare(theme::Swatch::Parchment)),
            ),
            None => body.into(),
        });
    }
    let card = iced::widget::container(card)
        .width(iced::Length::Fill)
        .padding([theme::SPACE_2, theme::SPACE_3])
        .style(|t| iced::widget::container::Style {
            border: iced::Border {
                radius: theme::RADIUS_SURFACE.into(),
                ..iced::Border::default()
            },
            ..theme::surface(t)
        });

    // Reference, not status: where the project and its environment live.
    let path = |text: String| {
        iced::widget::text(text)
            .font(theme::FONT_DATA)
            .size(theme::DATA)
            .style(theme::text_muted)
    };
    let mut paths = iced::widget::column![path(header.path.display().to_string())];
    if let Some(venv) = &header.venv_path {
        paths = paths.push(path(format!("environment {}", venv.display())));
    }

    iced::widget::column![title_row, card, paths.spacing(theme::SPACE_1)]
        .spacing(theme::SPACE_2)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::theme::State;

    fn dep(name: &str, constraint: Option<&str>, version: Option<&str>) -> crate::deps::DepRow {
        crate::deps::DepRow {
            name: name.into(),
            constraint: constraint.map(Into::into),
            version: version.map(Into::into),
            declared: true,
        }
    }

    fn tab(rows: Vec<crate::deps::DepRow>) -> crate::deps::DepsTab {
        crate::deps::DepsTab {
            groups: vec![crate::deps::DepGroup {
                kind: crate::deps::GroupKind::Main,
                rows,
            }],
            graph: None,
            tree_note: None,
        }
    }

    #[test]
    fn the_environment_tile_says_why_it_drifted() {
        let v = Some("3.11.9".to_string());
        assert_eq!(
            env_value(
                VenvStatus::Present,
                v.as_ref(),
                State::Present,
                Some(">=3.11")
            ),
            "Python 3.11.9"
        );
        assert_eq!(
            env_value(
                VenvStatus::Present,
                v.as_ref(),
                State::Drifted,
                Some(">=3.12")
            ),
            "Python 3.11.9 does not satisfy >=3.12"
        );
        assert_eq!(
            env_value(VenvStatus::Absent, None, State::Absent, None),
            "no environment"
        );
    }

    #[test]
    fn the_worst_state_wins() {
        assert_eq!(worst([]), State::Present);
        assert_eq!(worst([State::Present, State::Absent]), State::Absent);
        assert_eq!(
            worst([State::Absent, State::Drifted, State::Present]),
            State::Drifted
        );
    }

    #[test]
    fn the_dependencies_tile_counts_what_needs_attention() {
        let all_good = tab(vec![dep("a", Some(">=1"), Some("1.2"))]);
        assert_eq!(
            deps_summary(&all_good),
            (State::Present, "1 installed".to_string())
        );
        let mixed = tab(vec![
            dep("a", Some(">=1"), Some("1.2")),
            dep("b", Some(">=2"), Some("1.0")),
            dep("c", Some(">=1"), None),
        ]);
        assert_eq!(
            deps_summary(&mixed),
            (
                State::Drifted,
                "2 installed \u{b7} 1 drifted \u{b7} 1 missing".to_string()
            )
        );
        let mut twice = mixed.clone();
        twice.groups.push(twice.groups[0].clone());
        assert_eq!(
            deps_summary(&twice),
            deps_summary(&mixed),
            "a package in two groups counts once"
        );
    }

    fn member(selected: bool, installed: bool) -> crate::members_editor::MemberRow {
        crate::members_editor::MemberRow {
            path: "m".into(),
            name: Some("m".into()),
            origin: bombadil_core::model::MemberOrigin::Discovered,
            selected,
            installed,
        }
    }

    #[test]
    fn the_members_tile_counts_the_wanted_ones() {
        assert_eq!(members_summary(&[]), None, "no members, no tile");
        assert_eq!(
            members_summary(&[member(true, true), member(false, false)]),
            Some((State::Present, "1 of 1 installed".to_string())),
            "a deselected, uninstalled member is not a problem"
        );
        assert_eq!(
            members_summary(&[member(true, true), member(true, false)]),
            Some((State::Absent, "1 of 2 installed".to_string()))
        );
        assert_eq!(
            members_summary(&[member(true, true), member(false, true)]).map(|s| s.0),
            Some(State::Drifted),
            "a sync would remove one"
        );
    }

    #[test]
    fn the_git_line_says_branch_divergence_and_changes() {
        use bombadil_core::git::GitStatus;
        let git = GitStatus {
            branch: "main".into(),
            upstream: Some("origin/trunk".into()),
            ahead: 1,
            behind: 2,
            modified: 3,
            untracked: 1,
            ..GitStatus::default()
        };
        assert_eq!(
            git_summary(&git),
            "main \u{2192} origin/trunk \u{b7} \u{2191}1 \u{2193}2 \u{b7} 3 modified, 1 untracked"
        );
        let clean = GitStatus {
            branch: "abc12345".into(),
            detached: true,
            ..GitStatus::default()
        };
        assert_eq!(git_summary(&clean), "detached at abc12345 \u{b7} clean");
    }

    #[test]
    fn a_branch_tracking_its_namesake_does_not_repeat_the_name() {
        // `feature/x -> origin/feature/x` spent a second, often wrapping,
        // copy of a long branch name to say only "it has an upstream".
        use bombadil_core::git::GitStatus;
        let git = GitStatus {
            branch: "feature/long-name".into(),
            upstream: Some("origin/feature/long-name".into()),
            ..GitStatus::default()
        };
        assert_eq!(git_summary(&git), "feature/long-name \u{b7} clean");
        let elsewhere = GitStatus {
            branch: "main".into(),
            upstream: Some("origin/trunk".into()),
            ..GitStatus::default()
        };
        assert_eq!(
            git_summary(&elsewhere),
            "main \u{2192} origin/trunk \u{b7} clean"
        );
    }

    #[test]
    fn the_header_carries_the_active_environments_path() {
        // A project can have several environments, so "which one do Sync and
        // Open terminal act on" is a question the project's own path cannot
        // answer -- and the two buttons sitting on that row make it urgent.
        let path = PathBuf::from("/home/me/envs/api-312");
        let header = header(&project("api"), Some(path.clone()), &|_| {
            Some("3.12.1".into())
        });

        assert_eq!(header.venv_path, Some(path));
        assert_ne!(
            header.venv_path.as_ref(),
            Some(&header.path),
            "the environment path is not the project path -- showing one twice \
             would say nothing"
        );
    }

    #[test]
    fn a_project_with_no_environments_carries_no_path() {
        // Reachable between removing the last environment and adding another.
        // `None` rather than the project directory, which would claim an
        // environment lives somewhere it does not.
        let header = header(&project("api"), None, &|_| None);
        assert_eq!(header.venv_path, None);
    }

    #[test]
    fn a_project_with_a_venv_shows_its_python_version() {
        let header = header(&project("api"), None, &|_| Some("3.12.1".into()));
        assert_eq!(header.python_version.as_deref(), Some("3.12.1"));
        assert_eq!(header.venv_status, VenvStatus::Present);
    }

    #[test]
    fn a_project_without_a_venv_reports_no_environment() {
        // Registered-without-a-venv is a normal state (see `sidebar`);
        // whether Sync is still offered for it is `update`'s concern, not
        // `header`'s -- covered in `app.rs` where the Sync message is
        // dispatched independently of venv state.
        let header = header(&project("api"), None, &|_| None);
        assert_eq!(header.python_version, None);
        assert_eq!(header.venv_status, VenvStatus::Absent);
    }

    #[test]
    fn the_displayed_path_is_the_project_directory_not_the_manifest_file() {
        // A user recognises the directory they added, not the manifest file
        // inside it.
        let header = header(&project("api"), None, &|_| None);
        assert_eq!(header.path, PathBuf::from("/p/api"));
        assert_ne!(header.path, PathBuf::from("/p/api/pyproject.toml"));
    }

    fn project(label: &str) -> bombadil_core::model::Project {
        bombadil_core::model::Project {
            id: uuid::Uuid::new_v4(),
            label: label.into(),
            pyproject_path: std::path::PathBuf::from(format!("/p/{label}/pyproject.toml")),
            environments: vec![bombadil_core::model::Environment {
                location: bombadil_core::model::VenvLocation::Alongside,
                python: bombadil_core::model::PythonPin::Unpinned,
            }],
            active: bombadil_core::model::VenvLocation::Alongside,
            ..bombadil_core::model::Project::default()
        }
    }
}
