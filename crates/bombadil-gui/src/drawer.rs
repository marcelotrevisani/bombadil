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
    /// Whether the transcript is on screen.
    ///
    /// **Only the user changes this.** It used to be driven by the output
    /// itself -- a line arriving opened the drawer, a command finishing
    /// closed it -- which threw the transcript away at the exact moment
    /// there was something worth reading, and re-opened it under a user who
    /// had deliberately closed it.
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
            // Closed: a success says so in its toast; a failure opens the
            // drawer itself (`set_idle`). Opening it is one press and stays
            // open.
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
    // Deliberately does *not* open the drawer. A user who closed it meant it,
    // and output arriving is not a reason to overrule them -- the status line
    // still shows a command is running.
    drawer.lines.push(redact(line.to_string(), secrets));
    if drawer.lines.len() > MAX_LINES {
        let overflow = drawer.lines.len() - MAX_LINES;
        // Drains from the front: keeps the tail, the end where an error
        // actually shows up.
        drawer.lines.drain(0..overflow);
    }
}

/// Set the status line. Called once a uv invocation finishes, whether it
/// succeeded or not -- `status` is the caller's summary either way.
///
/// Deliberately does *not* collapse the drawer. It used to, which meant a
/// successful sync's transcript vanished the instant it finished -- the one
/// moment the user had a reason to read it. Whether the transcript is on
/// screen is the user's choice now, and nothing but their press changes it.
pub fn set_idle(drawer: &mut Drawer, status: impl Into<String>) {
    drawer.status = status.into();
    // A failure opens the drawer: its reason is the transcript. A success
    // leaves it open or closed as it was.
    if is_failure(&drawer.status) {
        drawer.expanded = true;
    }
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
    // Matched where Bombadil's own wording puts the marker, never anywhere
    // in the string: a finished success names its project ("Synced
    // failed-jobs"), and a label must not be able to spend `boot`.
    !status.ends_with("...")
        && (status.starts_with("Could not")
            || status.ends_with(" failed")
            || status.contains(" failed to persist")
            || status.ends_with(" failed unexpectedly"))
}

/// Renders the drawer: a `bark` surface, collapsed to one status line while
/// A header that is always present -- the status, a line count, and the
/// control that opens or closes the transcript -- with the transcript itself
/// in Plex at `DATA` beneath it while open. A failed command's status renders
/// in `boot` either way, per spec §9. Deliberately dumb beyond that: all the
/// logic worth testing lives in `push_line`, `set_idle` and `is_failure`.
/// `busy_label`, while a command runs, replaces the status line: it is the
/// live "Syncing api… 12s", which no stored status could keep current.
pub fn view<'a>(drawer: &Drawer, busy_label: Option<String>) -> iced::Element<'a, Message> {
    let failed = busy_label.is_none() && is_failure(&drawer.status);
    let status = busy_label.unwrap_or_else(|| drawer.status.clone());
    let status_colour = if failed {
        theme::Swatch::Boot
    } else {
        theme::Swatch::Slate
    };

    // The header is always there: the status, how many lines are behind it,
    // and the one control that opens or closes them. A drawer whose only
    // affordance was appearing on its own gave the user no way to say "not
    // now" -- or, once it had closed itself, "actually, show me".
    let toggle = iced::widget::button(
        iced::widget::row![
            iced::widget::text(if drawer.expanded {
                "\u{25be}"
            } else {
                "\u{25b8}"
            })
            .size(theme::BODY),
            iced::widget::text(status)
                .size(theme::BODY)
                .style(theme::tinted(status_colour)),
        ]
        .spacing(theme::SPACE_1)
        .align_y(iced::Alignment::Center),
    )
    .on_press(Message::DrawerToggled)
    .padding(0.0)
    .style(theme::button_bare(status_colour));

    let count: iced::Element<'a, Message> = if drawer.lines.is_empty() {
        iced::widget::Space::new().into()
    } else {
        // So a closed drawer still says there is something to open. Without
        // it, "sync succeeded" and "sync succeeded, and here are 400 lines
        // explaining what it did" look identical.
        iced::widget::text(format!("{} lines", drawer.lines.len()))
            .size(theme::LABEL)
            .style(theme::text_muted)
            .into()
    };

    let header = iced::widget::row![
        toggle,
        iced::widget::Space::new().width(iced::Length::Fill),
        count,
    ]
    .spacing(theme::SPACE_2)
    .align_y(iced::Alignment::Center);

    let mut content = iced::widget::column![header].spacing(theme::SPACE_1);

    if drawer.expanded {
        let mut list = iced::widget::column![].spacing(theme::SPACE_1);
        for line in &drawer.lines {
            list = list.push(
                iced::widget::text(line.clone())
                    .font(theme::FONT_DATA)
                    .size(theme::DATA),
            );
        }
        // Bounded in height, not in content: a large resolution prints
        // thousands of lines, and an unbounded transcript would push the whole
        // application off the top of the window. `push_line` caps how many are
        // kept; this caps how much of the window they occupy, and the rest
        // scrolls.
        //
        // `anchor_bottom` is what makes it follow the output: the scroll offset
        // is measured from the *end*, so appending a line moves the content
        // rather than the view, and the newest line is always the one on
        // screen. It also does the right thing when the user scrolls up to read
        // something -- their offset from the end is what is held, so the view
        // stays put instead of being yanked back down by every arriving line.
        // A snap-to-end task on each line would have fought them for the
        // scrollbar.
        content = content.push(
            iced::widget::scrollable(list)
                .height(theme::DRAWER_MAX_HEIGHT)
                .anchor_bottom(),
        );
    }

    // Full width, not sized to its own content: the drawer is the base of the
    // window, and a status line shrunk to the word "idle" read as a stray
    // chip floating in the middle of the canvas.
    iced::widget::container(content)
        .width(iced::Length::Fill)
        .padding([theme::SPACE_2, theme::SPACE_3])
        .style(theme::surface)
        .into()
}

/// A fresh transcript for a command starting, with the drawer left open or
/// closed exactly as the user had it.
pub fn start(drawer: &mut Drawer, status: String) {
    drawer.lines.clear();
    drawer.status = status;
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
    fn finishing_a_command_keeps_its_transcript_on_screen() {
        // It used to collapse here, which threw the output away at the exact
        // moment the user had a reason to read it -- a successful sync's
        // transcript existed only while it was still running.
        // Open, as a user who wants to watch has it.
        let mut drawer = Drawer {
            expanded: true,
            ..Drawer::default()
        };
        push_line(&mut drawer, "resolving dependencies...", &[]);

        set_idle(&mut drawer, "sync succeeded");

        assert!(
            drawer.expanded,
            "a finished command must leave its output where the user can read it"
        );
        assert_eq!(drawer.status, "sync succeeded");
        assert_eq!(drawer.lines.len(), 1, "and the transcript must survive");
    }

    #[test]
    fn output_arriving_does_not_reopen_a_drawer_the_user_closed() {
        // The other half of "the user decides". A closed drawer that reopened
        // itself the moment a line arrived would be a control that does not
        // hold, and the status line already says a command is running.
        let mut drawer = Drawer {
            expanded: false,
            ..Drawer::default()
        };

        push_line(&mut drawer, "resolving dependencies...", &[]);
        set_idle(&mut drawer, "sync succeeded");

        assert!(!drawer.expanded, "only the user opens it");
        assert_eq!(
            drawer.lines.len(),
            1,
            "the transcript is still collected while closed, ready to show"
        );
    }

    #[test]
    fn a_new_drawer_starts_collapsed() {
        // Every finish raises a toast, and a failure's toast opens the output;
        // an empty transcript open by default only took room from the tab.
        assert!(!Drawer::default().expanded);
    }

    #[test]
    fn a_failure_opens_the_drawer_and_a_success_leaves_it_alone() {
        // At the user's request: the reason a command failed is shown, not
        // left behind a toast's button.
        let mut drawer = Drawer::default();
        set_idle(&mut drawer, "Synced api");
        assert!(!drawer.expanded, "a success changes nothing");
        set_idle(&mut drawer, "Could not sync api");
        assert!(drawer.expanded, "a failure opens it");
        let mut open = Drawer {
            expanded: true,
            ..Drawer::default()
        };
        set_idle(&mut open, "Synced api");
        assert!(open.expanded, "and a success does not close it either");
    }

    #[test]
    fn starting_a_command_keeps_the_drawer_as_the_user_left_it() {
        let mut drawer = Drawer {
            lines: vec!["old".into()],
            expanded: true,
            status: "idle".into(),
        };
        start(&mut drawer, "syncing...".into());
        assert!(drawer.expanded, "opened by the user, stays open");
        assert!(drawer.lines.is_empty(), "a fresh transcript");
        assert_eq!(drawer.status, "syncing...");
        drawer.expanded = false;
        start(&mut drawer, "upgrading...".into());
        assert!(!drawer.expanded, "closed stays closed");
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
            // `busy::Action::failed`'s wording.
            "Could not sync api",
            "Could not install fastapi==0.110",
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
            // Finished successes name the project now (`busy::Action::done`).
            "Synced failed-migrations",
            "Recreated failed-migrations",
            "Installed failed-pkg==1.0",
        ] {
            assert!(
                !is_failure(status),
                "{status:?} is a sync in flight, not a failure"
            );
        }
    }
}
