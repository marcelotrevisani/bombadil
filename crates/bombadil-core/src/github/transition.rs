//! When a pull request's checks start and finish, decided from two lookups.

use super::{Counts, PrStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Started { total: usize },
    Finished(Counts),
}

/// The event between two lookups of the same project, if any.
///
/// A `prev` for a different pull request or branch is no `prev` at all: what
/// it knew says nothing about this one, and comparing them would announce a
/// "finished" that never happened. `None` -- the first lookup since launch --
/// is silent for the same reason, and so opening Bombadil does not fire one
/// notification per project.
pub fn event(prev: Option<&PrStatus>, next: &PrStatus) -> Option<Event> {
    let prev =
        prev.filter(|p| p.pr.number == next.pr.number && p.pr.head_branch == next.pr.head_branch)?;
    let (before, now) = (prev.counts(), next.counts());
    // No checks yet counts as pending: a run that registers and finishes
    // between two polls still announces its result.
    let pending_before = before.done < before.total || before.total == 0;
    let pending_now = now.done < now.total;
    let finished = now.total > 0 && !pending_now;

    if prev.pr.head_sha != next.pr.head_sha {
        // A push. Its checks may already have run start to finish between
        // two polls; then the result is what is worth saying.
        return if pending_now {
            Some(Event::Started { total: now.total })
        } else if finished {
            Some(Event::Finished(now))
        } else {
            None
        };
    }
    if (before.total == 0 || !pending_before) && pending_now {
        // A re-run, or checks registering after the commit was first seen.
        Some(Event::Started { total: now.total })
    } else if pending_before && finished {
        Some(Event::Finished(now))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::CheckState::*;
    use crate::github::tests::status_with;

    #[test]
    fn the_first_lookup_never_notifies() {
        assert_eq!(event(None, &status_with("a", &[Running])), None);
        assert_eq!(event(None, &status_with("a", &[Passed])), None);
    }

    #[test]
    fn a_new_commit_with_checks_running_starts() {
        let prev = status_with("a", &[Passed]);
        let next = status_with("b", &[Running, Queued]);
        assert_eq!(event(Some(&prev), &next), Some(Event::Started { total: 2 }));
    }

    #[test]
    fn a_new_commit_already_finished_between_polls_finishes() {
        let prev = status_with("a", &[Passed]);
        let next = status_with("b", &[Passed, Failed]);
        assert!(matches!(
            event(Some(&prev), &next),
            Some(Event::Finished(c)) if c.failed == 1 && c.total == 2
        ));
    }

    #[test]
    fn checks_appearing_later_start() {
        let prev = status_with("a", &[]);
        let next = status_with("a", &[Queued]);
        assert_eq!(event(Some(&prev), &next), Some(Event::Started { total: 1 }));
    }

    #[test]
    fn a_rerun_on_the_same_commit_starts() {
        let prev = status_with("a", &[Passed, Failed]);
        let next = status_with("a", &[Passed, Running]);
        assert_eq!(event(Some(&prev), &next), Some(Event::Started { total: 2 }));
    }

    #[test]
    fn pending_to_all_done_finishes_with_counts() {
        let prev = status_with("a", &[Passed, Running]);
        let passed = status_with("a", &[Passed, Skipped]);
        let failed = status_with("a", &[Passed, Failed]);
        assert!(matches!(event(Some(&prev), &passed), Some(Event::Finished(c)) if c.failed == 0));
        assert!(matches!(event(Some(&prev), &failed), Some(Event::Finished(c)) if c.failed == 1));
    }

    #[test]
    fn nothing_changed_is_silent() {
        let running = status_with("a", &[Running]);
        let done = status_with("a", &[Passed]);
        assert_eq!(event(Some(&running), &running), None);
        assert_eq!(event(Some(&done), &done), None);
    }

    #[test]
    fn a_different_pr_or_branch_counts_as_a_first_lookup() {
        let prev = status_with("a", &[Running]);
        let mut other_pr = status_with("a", &[Passed]);
        other_pr.pr.number = 8;
        let mut other_branch = status_with("a", &[Passed]);
        other_branch.pr.head_branch = "feat/y".into();
        assert_eq!(event(Some(&prev), &other_pr), None);
        assert_eq!(event(Some(&prev), &other_branch), None);
    }

    #[test]
    fn checks_registering_and_finishing_between_polls_finish() {
        let prev = status_with("a", &[]);
        let next = status_with("a", &[Passed]);
        assert!(matches!(event(Some(&prev), &next), Some(Event::Finished(c)) if c.total == 1));
        // A repository with no CI stays silent.
        assert_eq!(event(Some(&prev), &prev), None);
    }
}
