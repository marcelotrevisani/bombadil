//! The GitHub tab: this project's open pull request and its checks, and the
//! words used when they start and finish.

use crate::app::Message;
use crate::busy::ToastKind;
use crate::icons::Icon;
use crate::theme;
use bombadil_core::github::{Check, CheckState, Lookup, PrStatus, now_unix, transition::Event};
use std::time::Instant;

/// What the last lookup said, and the last pull request it found -- kept
/// through a failed lookup so a flaky network never empties the tab.
#[derive(Debug, Clone)]
pub struct GitHubState {
    pub lookup: Lookup,
    pub last_found: Option<PrStatus>,
    pub checked_at: Instant,
}

/// The toast kind, title and body for an event. The same words go to the
/// desktop notification.
pub fn notice(project: &str, status: &PrStatus, event: &Event) -> (ToastKind, String, String) {
    let pr = format!("#{} {}", status.pr.number, status.pr.title);
    match event {
        Event::Started { total } => (
            ToastKind::Success,
            format!("Checks started: {project}"),
            format!("{pr}, {total} checks on {}", status.pr.head_branch),
        ),
        Event::Finished(c) if c.failed == 0 => (
            ToastKind::Success,
            format!("Checks passed: {project}"),
            format!("{pr}, all {} passed", c.total),
        ),
        Event::Finished(c) => (
            ToastKind::Warning,
            format!("Checks failed: {project}"),
            format!("{pr}, {} of {} failed", c.failed, c.total),
        ),
    }
}

/// Check and status links come from any CI integration; only web pages are
/// handed to the system opener.
pub fn openable(url: &str) -> bool {
    url.starts_with("https://")
}

/// Where Preferences' "Add account" is: started, then showing gh's code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHubLogin {
    Waiting,
    Code(String),
}

pub fn reason(lookup: &Lookup, watching: bool) -> Option<String> {
    use Lookup::*;
    if !watching {
        return Some("Watching is off in Preferences.".into());
    }
    Some(match lookup {
        NotGit => "Not a git repository.".into(),
        NotGitHub => "No GitHub remote (origin isn't on GitHub).".into(),
        DefaultBranch(b) => format!("On {b}, the repository's default branch. Nothing to watch."),
        Detached => "Detached HEAD.".into(),
        NoPullRequest(b) => format!("No open pull request for {b}."),
        NeedsAuth => {
            "Sign in to see pull requests: add an account or a token in Preferences \u{203a} GitHub."
                .into()
        }
        TokenRejected => "GitHub rejected the saved token.".into(),
        RateLimited { until } => format!(
            "Rate limited, retrying after {:02}:{:02} UTC.",
            until.rem_euclid(86_400) / 3_600,
            until.rem_euclid(3_600) / 60
        ),
        Failed(e) => format!("Could not reach GitHub: {e}"),
        AccountGone(login) => {
            format!("{login} is no longer signed in to gh. Choose an account in Preferences.")
        }
        Found(_) => return None,
    })
}

pub fn summary(status: &PrStatus) -> String {
    let c = status.counts();
    if c.total == 0 {
        "No checks reported".into()
    } else if c.done < c.total {
        format!("Checks running: {} of {} done", c.done, c.total)
    } else if c.failed > 0 {
        format!("{} of {} checks failed", c.failed, c.total)
    } else if c.skipped > 0 {
        format!("All {} checks passed, {} skipped", c.total, c.skipped)
    } else {
        format!("All {} checks passed", c.total)
    }
}

/// How long ago the last lookup landed, for the summary line.
pub fn checked(secs: u64) -> String {
    match secs {
        0 => "checked just now".into(),
        1..60 => format!("checked {secs} s ago"),
        _ => format!("checked {} min ago", secs / 60),
    }
}

pub fn duration(check: &Check, now: i64) -> Option<String> {
    let started = bombadil_core::github::unix_seconds(check.started.as_deref()?)?;
    match check
        .completed
        .as_deref()
        .and_then(bombadil_core::github::unix_seconds)
    {
        Some(done) => {
            let s = (done - started).max(0);
            Some(if s < 60 {
                format!("{s} s")
            } else {
                format!("{} min", s / 60)
            })
        }
        None => Some(format!("running {} min", ((now - started).max(0)) / 60)),
    }
}

fn state_icon<'a>(state: CheckState) -> iced::Element<'a, Message> {
    let (icon, swatch) = match state {
        CheckState::Failed => (Icon::X, theme::Swatch::Boot),
        CheckState::Running => (Icon::Refresh, theme::Swatch::Parchment),
        CheckState::Queued => (Icon::Ellipsis, theme::Swatch::Slate),
        CheckState::Passed => (Icon::Check, theme::Swatch::Moss),
        CheckState::Skipped => (Icon::Check, theme::Swatch::Slate),
    };
    crate::icons::icon(icon, 16.0, swatch)
}

/// The tab for one project. `state` is `None` until its first lookup lands.
pub fn view<'a>(
    project_id: uuid::Uuid,
    state: Option<&GitHubState>,
    watching: bool,
    in_flight: bool,
) -> iced::Element<'a, Message> {
    let refresh = iced::widget::button(
        iced::widget::text(if in_flight {
            "Checking\u{2026}"
        } else {
            "Refresh"
        })
        .size(theme::BODY),
    )
    .on_press_maybe((!in_flight).then_some(Message::GitHubRefreshRequested(project_id)))
    .padding([theme::SPACE_1, theme::SPACE_3])
    .style(theme::button_secondary);

    let mut column = iced::widget::column![].spacing(theme::SPACE_3);
    let line = match state {
        None if watching => Some("Looking for a pull request\u{2026}".to_string()),
        None => reason(&Lookup::NotGit, false),
        Some(s) => reason(&s.lookup, watching),
    };
    if let Some(line) = line {
        column = column.push(
            iced::widget::row![
                iced::widget::text(line)
                    .size(theme::BODY)
                    .style(theme::text_muted),
                iced::widget::Space::new().width(iced::Length::Fill),
                refresh,
            ]
            .align_y(iced::Alignment::Center),
        );
        // A hiccup keeps the last pull request on screen below its reason.
        let Some(found) = state.and_then(|s| s.last_found.as_ref()) else {
            return column.into();
        };
        return column.push(pr_view(found, None)).into();
    }
    let found = state
        .and_then(|s| s.last_found.as_ref())
        .expect("Found keeps last_found");
    let ago = state.map(|s| s.checked_at.elapsed().as_secs()).unwrap_or(0);
    column
        .push(pr_view(found, Some((ago, refresh.into()))))
        .into()
}

fn pr_view<'a>(
    status: &PrStatus,
    refresh: Option<(u64, iced::Element<'a, Message>)>,
) -> iced::Element<'a, Message> {
    let pr = &status.pr;
    let header = iced::widget::row![
        iced::widget::text(format!("#{} {}", pr.number, pr.title))
            .font(theme::FONT_PROSE_SEMIBOLD)
            .size(theme::TITLE),
        theme::badge(if pr.draft { "Draft" } else { "Open" }),
        iced::widget::Space::new().width(iced::Length::Fill),
        iced::widget::button(iced::widget::text("Open pull request").size(theme::BODY))
            .on_press(Message::GitHubOpen(pr.url.clone()))
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(theme::button_primary),
    ]
    .spacing(theme::SPACE_2)
    .align_y(iced::Alignment::Center);

    let short_sha: String = pr.head_sha.chars().take(7).collect();
    let mut summary_row = iced::widget::row![
        iced::widget::text(summary(status)).size(theme::BODY),
        iced::widget::text(format!(
            "{} \u{2192} {} \u{00b7} {short_sha}",
            pr.head_branch, pr.base_branch
        ))
        .font(theme::FONT_DATA)
        .size(theme::DATA)
        .style(theme::text_muted),
        iced::widget::Space::new().width(iced::Length::Fill),
    ]
    .spacing(theme::SPACE_2)
    .align_y(iced::Alignment::Center);
    if let Some((ago, refresh)) = refresh {
        summary_row = summary_row
            .push(
                iced::widget::text(checked(ago))
                    .size(theme::LABEL)
                    .style(theme::text_muted),
            )
            .push(refresh);
    }

    let now = now_unix();
    let mut checks = status.checks.clone();
    checks.sort_by(|a, b| a.state.cmp(&b.state).then_with(|| a.name.cmp(&b.name)));
    let mut rows = iced::widget::column![].spacing(theme::SPACE_1);
    for check in checks {
        let mut row = iced::widget::row![
            state_icon(check.state),
            iced::widget::text(check.name.clone()).size(theme::BODY),
        ]
        .spacing(theme::SPACE_2)
        .align_y(iced::Alignment::Center);
        if let Some(workflow) = &check.workflow {
            row = row.push(
                iced::widget::text(workflow.clone())
                    .size(theme::LABEL)
                    .style(theme::text_muted),
            );
        }
        row = row.push(iced::widget::Space::new().width(iced::Length::Fill));
        if let Some(d) = duration(&check, now) {
            row = row.push(
                iced::widget::text(d)
                    .font(theme::FONT_DATA)
                    .size(theme::DATA)
                    .style(theme::text_muted),
            );
        }
        if let Some(url) = &check.url {
            row = row.push(
                iced::widget::button(iced::widget::text("Open").size(theme::LABEL))
                    .on_press(Message::GitHubOpen(url.clone()))
                    .padding([0.0, theme::SPACE_2])
                    .style(theme::button_secondary),
            );
        }
        rows = rows.push(row);
    }
    iced::widget::column![header, summary_row, theme::hairline_row(), rows]
        .spacing(theme::SPACE_3)
        .into()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use bombadil_core::github::{Check, CheckState, Counts, PullRequest};

    pub(crate) fn found(sha: &str, states: &[CheckState]) -> PrStatus {
        PrStatus {
            pr: PullRequest {
                number: 12,
                title: "Fix the thing".into(),
                url: "https://github.com/o/r/pull/12".into(),
                draft: false,
                head_branch: "feat/x".into(),
                base_branch: "main".into(),
                head_sha: sha.into(),
            },
            checks: states
                .iter()
                .map(|s| Check {
                    name: "c".into(),
                    workflow: None,
                    state: *s,
                    url: None,
                    started: None,
                    completed: None,
                })
                .collect(),
        }
    }

    #[test]
    fn notices_say_what_happened_to_which_pr() {
        let s = found("a", &[CheckState::Running; 9]);
        assert_eq!(
            notice("api", &s, &Event::Started { total: 9 }),
            (
                ToastKind::Success,
                "Checks started: api".into(),
                "#12 Fix the thing, 9 checks on feat/x".into()
            )
        );
        let passed = Counts {
            total: 9,
            done: 9,
            passed: 9,
            skipped: 0,
            failed: 0,
        };
        assert_eq!(
            notice("api", &s, &Event::Finished(passed)),
            (
                ToastKind::Success,
                "Checks passed: api".into(),
                "#12 Fix the thing, all 9 passed".into()
            )
        );
        let failed = Counts {
            total: 9,
            done: 9,
            passed: 7,
            skipped: 0,
            failed: 2,
        };
        assert_eq!(
            notice("api", &s, &Event::Finished(failed)),
            (
                ToastKind::Warning,
                "Checks failed: api".into(),
                "#12 Fix the thing, 2 of 9 failed".into()
            )
        );
    }

    #[test]
    fn every_lookup_but_found_has_a_reason() {
        use bombadil_core::github::Lookup::*;
        assert_eq!(reason(&NotGit, true).unwrap(), "Not a git repository.");
        assert_eq!(
            reason(&NotGitHub, true).unwrap(),
            "No GitHub remote (origin isn't on GitHub)."
        );
        assert_eq!(
            reason(&DefaultBranch("main".into()), true).unwrap(),
            "On main, the repository's default branch. Nothing to watch."
        );
        assert_eq!(reason(&Detached, true).unwrap(), "Detached HEAD.");
        assert_eq!(
            reason(&NoPullRequest("feat/x".into()), true).unwrap(),
            "No open pull request for feat/x."
        );
        assert_eq!(
            reason(&TokenRejected, true).unwrap(),
            "GitHub rejected the saved token."
        );
        assert_eq!(
            reason(
                &RateLimited {
                    until: 1_792_180_702
                },
                true
            )
            .unwrap(),
            "Rate limited, retrying after 19:58 UTC."
        );
        assert_eq!(
            reason(&Failed("x".into()), true).unwrap(),
            "Could not reach GitHub: x"
        );
        assert_eq!(
            reason(&NotGit, false).unwrap(),
            "Watching is off in Preferences."
        );
        assert_eq!(reason(&Found(found("a", &[])), true), None);
    }

    #[test]
    fn the_summary_reads_running_passed_or_failed() {
        use CheckState::*;
        assert_eq!(
            summary(&found("a", &[Passed, Running, Queued])),
            "Checks running: 1 of 3 done"
        );
        assert_eq!(
            summary(&found("a", &[Passed, Passed])),
            "All 2 checks passed"
        );
        assert_eq!(
            summary(&found("a", &[Passed, Skipped])),
            "All 2 checks passed, 1 skipped"
        );
        assert_eq!(
            summary(&found("a", &[Passed, Failed, Failed])),
            "2 of 3 checks failed"
        );
        assert_eq!(summary(&found("a", &[])), "No checks reported");
    }

    #[test]
    fn durations_are_whole_minutes_or_seconds() {
        let mut c = found("a", &[CheckState::Passed]).checks.remove(0);
        c.started = Some("2026-10-16T19:57:23Z".into());
        c.completed = Some("2026-10-16T19:58:22Z".into());
        assert_eq!(duration(&c, 0).as_deref(), Some("59 s"));
        c.completed = None;
        let now = bombadil_core::github::unix_seconds("2026-10-16T20:00:30Z").unwrap();
        assert_eq!(duration(&c, now).as_deref(), Some("running 3 min"));
        c.started = None;
        assert_eq!(duration(&c, now), None);
    }

    #[test]
    fn signing_in_points_at_preferences() {
        assert_eq!(
            reason(&Lookup::NeedsAuth, true).unwrap(),
            "Sign in to see pull requests: add an account or a token in Preferences \u{203a} GitHub."
        );
    }

    #[test]
    fn a_chosen_account_that_left_gh_says_so() {
        assert_eq!(
            reason(&Lookup::AccountGone("me".into()), true).unwrap(),
            "me is no longer signed in to gh. Choose an account in Preferences."
        );
    }

    #[test]
    fn checked_reads_in_seconds_then_minutes() {
        assert_eq!(checked(0), "checked just now");
        assert_eq!(checked(42), "checked 42 s ago");
        assert_eq!(checked(118), "checked 1 min ago");
        assert_eq!(checked(3600), "checked 60 min ago");
    }

    #[test]
    fn only_https_links_are_opened() {
        assert!(openable("https://github.com/o/r/pull/1"));
        assert!(!openable("file:///etc/passwd"));
        assert!(!openable("http://ci.example/1"));
        assert!(!openable("javascript:alert(1)"));
    }
}
