//! The output drawer: the live transcript of a uv invocation.
//!
//! Spec §8: a collapsible drawer streams live uv output and reduces to a
//! single status line once idle. Spec §9: a non-zero exit shows a banner plus
//! the full transcript -- which means the transcript stored here must already
//! be safe to show, since nothing downstream re-checks it.
//!
//! # How `view` knows a command failed
//!
//! `Drawer` carries no `failed: bool` -- every failure status `app::update`
//! ever sets (`"sync failed"`, `"open terminal failed"`, `"env var change
//! failed to persist"`, ...) already contains the word "failed", and every
//! success status (`"sync succeeded"`, `"project added"`, `"idle"`) never
//! does. [`is_failure`] reads that existing convention rather than adding a
//! field every one of those call sites in `app.rs` would then have to set
//! explicitly.
//!
//! The in-progress statuses are the exception, and the reason [`is_failure`]
//! is not a bare `contains`: `"syncing api..."`, `"recreating api..."` and
//! `"syncing members for api..."` all interpolate a *project label*, which
//! the user chose. A project named `failed-migrations` would make a healthy
//! sync's status line read as a failure. All three end in `"..."`, the
//! in-progress marker, so [`is_failure`] excludes those first and the label
//! can never reach the substring test.

use crate::app::Message;
use crate::theme;
use bombadil_core::envspec::redact;

/// How many lines the drawer keeps. A `uv sync` with a large resolution can
/// print thousands of lines; an unbounded `Vec<String>` would grow until the
/// app is killed. The cap drops the OLDEST lines on overflow, not the
/// newest -- a failure is almost always visible at the tail of the output,
/// so that is the end that has to survive.
const MAX_LINES: usize = 500;

/// Everything the output drawer shows.
#[derive(Debug)]
pub struct Drawer {
    /// Accumulated output lines, oldest first, capped at [`MAX_LINES`].
    /// Every line here has already been through [`push_line`]'s redaction --
    /// nothing populates this field any other way, so reading it directly
    /// (as a failure transcript does) is exactly as safe as going through
    /// `view`.
    pub lines: Vec<String>,
    /// True while a command is streaming output; false once it is idle. Only
    /// `view` is expected to read this to decide what to render -- it is
    /// public so `update` can drive it directly, the same way `app.entries`
    /// is mutated in place elsewhere in this crate.
    pub expanded: bool,
    /// The single line `view` shows once `expanded` is false.
    pub status: String,
}

impl Default for Drawer {
    /// A drawer with nothing to show yet: collapsed, no output, idle. Not
    /// derived -- `status` starts as a real word, not the empty string
    /// `#[derive(Default)]` would give it, so `view` never has to render a
    /// blank status line before the first command has run.
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            expanded: false,
            status: "idle".to_string(),
        }
    }
}

/// Append one line of output, redacting `secrets` out of it first and
/// dropping the oldest line if the buffer is already at [`MAX_LINES`].
///
/// Redaction happens here -- on the way in -- rather than in `view`, so
/// nothing that reads `lines` directly (a failure banner showing the full
/// transcript, per spec §9) can end up seeing a value `view` would have
/// scrubbed. Reuses `bombadil_core::envspec::redact`, the same function
/// `compose` uses to keep a keychain secret out of a failing script's error,
/// rather than a second implementation that could drift from it.
pub fn push_line(drawer: &mut Drawer, line: &str, secrets: &[String]) {
    drawer.expanded = true;
    drawer.lines.push(redact(line.to_string(), secrets));
    if drawer.lines.len() > MAX_LINES {
        let overflow = drawer.lines.len() - MAX_LINES;
        // Drains from the front: keeps the tail, the end where an error
        // actually shows up.
        drawer.lines.drain(0..overflow);
    }
}

/// Mark the drawer idle: collapse it and set the single status line `view`
/// shows in its place. Called once a uv invocation finishes, whether it
/// succeeded or not -- `status` is the caller's summary either way.
pub fn set_idle(drawer: &mut Drawer, status: impl Into<String>) {
    drawer.expanded = false;
    drawer.status = status.into();
}

/// Whether `status` names a failure -- see the module doc for the convention
/// this reads rather than re-derives.
///
/// The `ends_with("...")` guard is the whole point of this function having a
/// second clause. Three statuses interpolate a project's own label
/// (`format!("syncing {}...", project.label)` and friends in `app::update`),
/// and that label comes from `[project].name` or a directory name -- so a
/// project called `failed-migrations` would otherwise paint its status line
/// `boot` for the whole of a perfectly successful sync. `boot` is the one
/// colour this application cannot afford to spend on a non-failure (see
/// `theme`'s module doc), and user-controlled data must not be able to spend
/// it. `"..."` is the in-progress convention, and every status that carries
/// user data is in progress.
///
/// ponytail: still a string convention, not a typed flag, and it now has two
/// ceilings rather than one. The harmless one: a future *failure* worded
/// without "failed" misses the banner. The costly one: a future status that
/// interpolates user data and does *not* end in `"..."` puts the rationed
/// colour back within reach of a project name. Upgrade to a
/// `Drawer::failed: bool` set at each `app::update` call site if either
/// bites -- the second one first.
fn is_failure(status: &str) -> bool {
    !status.ends_with("...") && status.contains("failed")
}

/// Renders the drawer: a `bark` surface, collapsed to one status line while
/// idle, the full transcript in Plex at `DATA` while expanded. A failed
/// command's status renders in `boot` either way -- the collapsed line for a
/// persist failure with no transcript to show, and a banner above the
/// transcript for a failed command, per spec §9. Deliberately dumb beyond
/// that: all the logic worth testing lives in `push_line`, `set_idle` and
/// `is_failure`.
pub fn view<'a>(drawer: &Drawer) -> iced::Element<'a, Message> {
    let failed = is_failure(&drawer.status);
    let status_colour = if failed { theme::BOOT } else { theme::SLATE };

    let content: iced::Element<'a, Message> = if drawer.expanded {
        let mut list = iced::widget::column![].spacing(theme::SPACE_1);
        for line in &drawer.lines {
            list = list.push(
                iced::widget::text(line.clone())
                    .font(theme::FONT_DATA)
                    .size(theme::DATA),
            );
        }
        iced::widget::column![
            iced::widget::text(drawer.status.clone())
                .size(theme::BODY)
                .color(status_colour),
            // Bounded: a large resolution prints thousands of lines, and an
            // unbounded transcript would push the whole application off the
            // top of the window. `push_line` already caps how many are kept;
            // this caps how much of the window they may occupy.
            iced::widget::scrollable(list).height(theme::DRAWER_MAX_HEIGHT),
        ]
        .spacing(theme::SPACE_1)
        .into()
    } else {
        iced::widget::text(drawer.status.clone())
            .size(theme::BODY)
            .color(status_colour)
            .into()
    };

    // Full width, not sized to its own content: the drawer is the base of the
    // window, and a status line shrunk to the word "idle" read as a stray
    // chip floating in the middle of the canvas.
    iced::widget::container(content)
        .width(iced::Length::Fill)
        .padding([theme::SPACE_2, theme::SPACE_3])
        .style(theme::surface)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_accumulate_in_order() {
        let mut drawer = Drawer::default();
        push_line(&mut drawer, "first", &[]);
        push_line(&mut drawer, "second", &[]);
        push_line(&mut drawer, "third", &[]);

        assert_eq!(drawer.lines, vec!["first", "second", "third"]);
    }

    #[test]
    fn the_buffer_is_bounded_and_keeps_the_most_recent_lines() {
        // A test asserting only the length would pass against an
        // implementation that dropped the newest lines instead of the
        // oldest -- the wrong end, since the user needs the tail, where an
        // error would show up. Assert both the cap and which lines survived.
        let mut drawer = Drawer::default();
        let pushed = MAX_LINES + 10;
        for i in 0..pushed {
            push_line(&mut drawer, &format!("line-{i}"), &[]);
        }

        assert_eq!(drawer.lines.len(), MAX_LINES, "got {}", drawer.lines.len());
        assert_eq!(
            drawer.lines.first().map(String::as_str),
            Some("line-10"),
            "the oldest 10 lines must have been dropped; got {:?}",
            drawer.lines.first()
        );
        assert_eq!(
            drawer.lines.last().map(String::as_str),
            Some(format!("line-{}", pushed - 1)).as_deref(),
            "the most recently pushed line must survive; got {:?}",
            drawer.lines.last()
        );
    }

    #[test]
    fn idle_collapses_to_a_single_status_line() {
        let mut drawer = Drawer::default();
        push_line(&mut drawer, "resolving dependencies...", &[]);
        assert!(
            drawer.expanded,
            "streaming output must expand the drawer before it can collapse"
        );

        set_idle(&mut drawer, "sync succeeded");

        assert!(!drawer.expanded, "an idle drawer must collapse");
        assert_eq!(drawer.status, "sync succeeded");
        assert!(
            !drawer.status.is_empty(),
            "the collapsed drawer must show a real status, not an empty line"
        );
    }

    #[test]
    fn a_secret_never_reaches_the_drawer() {
        // The assertions below read `drawer.lines` directly, the same way a
        // failure transcript (spec §9) would -- never through `view`. An
        // implementation that redacted only in `view` would leave the raw
        // secret sitting in `lines` and still fail this test.
        let mut drawer = Drawer::default();
        let secrets = vec!["s3cr3t-value".to_string()];

        push_line(
            &mut drawer,
            "+ curl -H 'Authorization: Bearer s3cr3t-value'",
            &secrets,
        );

        assert!(
            drawer.lines.iter().all(|l| !l.contains("s3cr3t-value")),
            "a secret leaked into the stored lines: {:?}",
            drawer.lines
        );
        assert!(drawer.lines[0].contains("***"), "got {:?}", drawer.lines[0]);
    }

    // --- is_failure: the convention `boot` rationing on the drawer depends on ---

    #[test]
    fn every_failure_status_app_update_sets_is_recognised() {
        for status in [
            "sync failed",
            "add project failed",
            "open terminal failed",
            "open folder failed",
            "remove failed to persist",
            "env var change failed to persist",
            "script change failed to persist",
            "index change failed to persist",
            "member selection failed to persist",
            "settings change failed to persist",
        ] {
            assert!(is_failure(status), "{status:?} must read as a failure");
        }
    }

    #[test]
    fn a_success_or_in_progress_status_is_not_a_failure() {
        for status in [
            "idle",
            "sync succeeded",
            "project added",
            "syncing api...",
            "recreating api...",
        ] {
            assert!(!is_failure(status), "{status:?} must not read as a failure");
        }
    }

    #[test]
    fn a_project_label_containing_failed_does_not_paint_a_healthy_sync_boot() {
        // The label is user data -- `[project].name`, or the directory name.
        // These are the exact strings `app::update` builds at the three
        // `format!` call sites that interpolate one. `boot` is rationed to
        // "this needs you"; a project called `failed-migrations` spending it
        // on an entire successful sync is the one way user data could take
        // the rationing away, and the whole system depends on it not doing
        // so.
        for status in [
            "syncing failed-migrations...",
            "recreating failed-migrations...",
            "syncing members for failed-migrations...",
            // The pathological case: the label is the marker itself.
            "syncing failed...",
        ] {
            assert!(
                !is_failure(status),
                "{status:?} is a sync in flight, not a failure"
            );
        }
    }
}
