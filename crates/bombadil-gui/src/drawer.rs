//! The output drawer: the live transcript of a uv invocation.
//!
//! Spec §8: a collapsible drawer streams live uv output and reduces to a
//! single status line once idle. Spec §9: a non-zero exit shows a banner plus
//! the full transcript -- which means the transcript stored here must already
//! be safe to show, since nothing downstream re-checks it.

use crate::app::Message;
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

/// Renders the drawer. Deliberately dumb: all the logic worth testing lives
/// in `push_line` and `set_idle`.
pub fn view<'a>(drawer: &Drawer) -> iced::Element<'a, Message> {
    if drawer.expanded {
        let mut list = iced::widget::column![].spacing(2);
        for line in &drawer.lines {
            list = list.push(iced::widget::text(line.clone()));
        }
        iced::widget::scrollable(list).into()
    } else {
        iced::widget::text(drawer.status.clone()).into()
    }
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
}
