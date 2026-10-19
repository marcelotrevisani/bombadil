//! What needs attention in a project, in one plain sentence each. The
//! detail header and the sidebar badge both read `findings`.

use crate::interpreter::{satisfies_pin, version_satisfies};
use bombadil_core::git::GitStatus;
use bombadil_core::github::PrStatus;
use bombadil_core::model::PythonPin;
use bombadil_core::uv::results::SyncCheck;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Warning,
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fix {
    Sync,
    OpenPullRequest(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub level: Level,
    pub text: String,
    pub fix: Option<Fix>,
}

/// What is known about one project's default environment. `None` means not
/// known (not probed, probe failed, not applicable): it never yields a finding.
pub struct Input<'a> {
    pub python: Option<&'a str>,
    pub requires_python: Option<&'a str>,
    pub pin: &'a PythonPin,
    pub sync: Option<&'a SyncCheck>,
    pub git: Option<&'a GitStatus>,
    pub pr: Option<&'a PrStatus>,
    pub installed: Option<&'a [String]>,
    /// The project's `pyproject.toml` isn't on this computer.
    pub missing: bool,
}

fn warning(text: String, fix: Option<Fix>) -> Finding {
    Finding {
        level: Level::Warning,
        text,
        fix,
    }
}

fn note(text: String) -> Finding {
    Finding {
        level: Level::Note,
        text,
        fix: None,
    }
}

pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `(major, minor)` of a stable release; `None` for pre-releases and
/// anything unreadable.
fn minor(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0");
    patch
        .chars()
        .all(|c| c.is_ascii_digit())
        .then_some((major, minor))
}

pub fn findings(input: &Input) -> Vec<Finding> {
    // Nothing else can be checked on a folder that isn't there.
    if input.missing {
        return vec![warning(
            "This project's folder isn't on this computer".into(),
            None,
        )];
    }
    let mut out = Vec::new();
    if input.sync == Some(&SyncCheck::NoEnvironment) {
        out.push(warning("No environment yet".into(), Some(Fix::Sync)));
    } else if let Some(python) = input.python {
        if !version_satisfies(python, input.requires_python) {
            let needs = input.requires_python.unwrap_or_default();
            out.push(warning(
                format!("Python {python}, but pyproject.toml needs {needs}"),
                Some(Fix::Sync),
            ));
        } else if let PythonPin::Version(pinned) = input.pin
            && !satisfies_pin(python, input.pin)
        {
            out.push(warning(
                format!("Python {python}, but this environment is pinned to {pinned}"),
                Some(Fix::Sync),
            ));
        } else if let (Some(current), Some(installed)) = (minor(python), input.installed) {
            let newest = installed
                .iter()
                .filter(|v| {
                    version_satisfies(v, input.requires_python) && satisfies_pin(v, input.pin)
                })
                .filter_map(|v| minor(v))
                .filter(|m| *m > current)
                .max();
            if let Some((major, newer)) = newest {
                out.push(note(format!(
                    "Python {major}.{newer} is installed and allowed; this environment uses {}.{}",
                    current.0, current.1
                )));
            }
        }
        if let Some(SyncCheck::WouldReplace { python: next }) = input.sync {
            // No fix offered: rebuilding on another Python is the user's
            // call, not a button to press without reading this.
            let text = match next {
                Some(next) => {
                    format!(
                        "Sync would rebuild this environment on Python {next} (it uses {python})"
                    )
                }
                None => "Sync would rebuild this environment".to_string(),
            };
            out.push(warning(text, None));
        }
        if let Some(SyncCheck::Behind {
            missing,
            changed,
            extra,
        }) = input.sync
        {
            let parts: Vec<String> = [
                (missing, "missing"),
                (changed, "to change"),
                (extra, "extra"),
            ]
            .into_iter()
            .filter(|(n, _)| **n > 0)
            .map(|(n, what)| format!("{n} {what}"))
            .collect();
            let text = match parts.is_empty() {
                true => "Out of step with uv.lock".to_string(),
                false => format!("Out of step with uv.lock: {}", parts.join(", ")),
            };
            out.push(warning(text, Some(Fix::Sync)));
        }
    }
    if let Some(git) = input.git {
        if git.ahead > 0 {
            out.push(note(format!(
                "{} not pushed",
                plural(git.ahead, "commit", "commits")
            )));
        }
        if git.behind > 0 {
            out.push(note(format!(
                "{} to pull",
                plural(git.behind, "commit", "commits")
            )));
        }
    }
    if let Some(status) = input.pr {
        let counts = status.counts();
        let number = status.pr.number;
        if counts.failed > 0 {
            out.push(warning(
                format!(
                    "{} of {} checks failed on #{number}",
                    counts.failed, counts.total
                ),
                Some(Fix::OpenPullRequest(status.pr.url.clone())),
            ));
        } else if counts.done < counts.total {
            out.push(note(format!("Checks running on #{number}")));
        }
    }
    out.sort_by_key(|f| f.level);
    out
}

pub fn has_warning(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.level == Level::Warning)
}

/// Every finding's sentence, one per line, for the sidebar badge.
pub fn tooltip(findings: &[Finding]) -> String {
    findings
        .iter()
        .map(|f| f.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bombadil_core::git::GitStatus;
    use bombadil_core::model::PythonPin;
    use bombadil_core::uv::results::SyncCheck;

    fn input<'a>(pin: &'a PythonPin) -> Input<'a> {
        Input {
            python: Some("3.12.4"),
            requires_python: Some(">=3.12"),
            pin,
            sync: Some(&SyncCheck::InSync),
            git: None,
            pr: None,
            installed: None,
            missing: false,
        }
    }

    #[test]
    fn a_missing_folder_is_a_health_warning() {
        let pin = PythonPin::Unpinned;
        let f = findings(&Input {
            missing: true,
            ..input(&pin)
        });
        assert_eq!(texts(&f), ["This project's folder isn't on this computer"]);
        assert_eq!(f[0].level, Level::Warning);
    }
    fn texts(f: &[Finding]) -> Vec<&str> {
        f.iter().map(|f| f.text.as_str()).collect()
    }

    #[test]
    fn a_healthy_project_has_nothing_to_say() {
        assert!(findings(&input(&PythonPin::Unpinned)).is_empty());
    }

    #[test]
    fn no_environment_is_the_only_warning() {
        let pin = PythonPin::Unpinned;
        let f = findings(&Input {
            python: None,
            sync: Some(&SyncCheck::NoEnvironment),
            installed: Some(&["3.14.7".to_string()]),
            ..input(&pin)
        });
        assert_eq!(texts(&f), ["No environment yet"]);
        assert_eq!(f[0].level, Level::Warning);
        assert_eq!(f[0].fix, Some(Fix::Sync));
    }

    #[test]
    fn unprobed_state_says_nothing() {
        let pin = PythonPin::Unpinned;
        let f = findings(&Input {
            python: None,
            sync: None,
            ..input(&pin)
        });
        assert!(f.is_empty());
    }

    #[test]
    fn python_outside_requires_python_warns_with_sync() {
        let pin = PythonPin::Unpinned;
        let f = findings(&Input {
            python: Some("3.11.9"),
            ..input(&pin)
        });
        assert_eq!(
            texts(&f),
            ["Python 3.11.9, but pyproject.toml needs >=3.12"]
        );
        assert_eq!(f[0].fix, Some(Fix::Sync));
    }

    #[test]
    fn python_off_its_pin_warns() {
        let pin = PythonPin::Version("3.13".into());
        let f = findings(&input(&pin));
        assert_eq!(
            texts(&f),
            ["Python 3.12.4, but this environment is pinned to 3.13"]
        );
    }

    #[test]
    fn a_newer_allowed_python_is_a_quiet_note() {
        let pin = PythonPin::Unpinned;
        let installed = [
            "3.12.9".to_string(),
            "3.13.2".to_string(),
            "3.14.7".to_string(),
        ];
        let f = findings(&Input {
            installed: Some(&installed),
            ..input(&pin)
        });
        assert_eq!(
            texts(&f),
            ["Python 3.14 is installed and allowed; this environment uses 3.12"]
        );
        assert_eq!((f[0].level, f[0].fix.clone()), (Level::Note, None));
    }

    #[test]
    fn pre_releases_are_never_suggested() {
        let pin = PythonPin::Unpinned;
        let installed = [
            "3.15.0rc2".to_string(),
            "3.14.0a1".to_string(),
            "3.12.9".to_string(),
        ];
        assert!(
            findings(&Input {
                installed: Some(&installed),
                ..input(&pin)
            })
            .is_empty()
        );
    }

    #[test]
    fn a_newer_patch_of_the_same_minor_is_not_news() {
        let pin = PythonPin::Unpinned;
        let installed = ["3.12.9".to_string()];
        assert!(
            findings(&Input {
                installed: Some(&installed),
                ..input(&pin)
            })
            .is_empty()
        );
    }

    #[test]
    fn no_requires_python_still_respects_the_pin() {
        let pin = PythonPin::Version("3.12".into());
        let installed = ["3.14.7".to_string()];
        let f = findings(&Input {
            requires_python: None,
            installed: Some(&installed),
            ..input(&pin)
        });
        assert!(f.is_empty(), "{f:?}");
    }

    #[test]
    fn a_fit_warning_suppresses_the_newer_note() {
        let pin = PythonPin::Unpinned;
        let installed = ["3.14.7".to_string()];
        let f = findings(&Input {
            python: Some("3.11.9"),
            installed: Some(&installed),
            ..input(&pin)
        });
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].level, Level::Warning);
    }

    #[test]
    fn being_behind_the_lock_lists_only_what_is_there() {
        let pin = PythonPin::Unpinned;
        let behind = SyncCheck::Behind {
            missing: 3,
            changed: 0,
            extra: 1,
        };
        let f = findings(&Input {
            sync: Some(&behind),
            ..input(&pin)
        });
        assert_eq!(texts(&f), ["Out of step with uv.lock: 3 missing, 1 extra"]);
        assert_eq!(f[0].fix, Some(Fix::Sync));
        let zero = SyncCheck::Behind {
            missing: 0,
            changed: 0,
            extra: 0,
        };
        let f = findings(&Input {
            sync: Some(&zero),
            ..input(&pin)
        });
        assert_eq!(texts(&f), ["Out of step with uv.lock"]);
        let unknown = SyncCheck::Unknown;
        assert!(
            findings(&Input {
                sync: Some(&unknown),
                ..input(&pin)
            })
            .is_empty()
        );
    }

    #[test]
    fn a_rebuild_on_another_python_says_so_without_counts() {
        let pin = PythonPin::Unpinned;
        let replace = SyncCheck::WouldReplace {
            python: Some("3.13.15".into()),
        };
        let f = findings(&Input {
            sync: Some(&replace),
            ..input(&pin)
        });
        assert_eq!(
            texts(&f),
            ["Sync would rebuild this environment on Python 3.13.15 (it uses 3.12.4)"]
        );
        assert_eq!((f[0].level, f[0].fix.clone()), (Level::Warning, None));
        let unnamed = SyncCheck::WouldReplace { python: None };
        let f = findings(&Input {
            sync: Some(&unnamed),
            ..input(&pin)
        });
        assert_eq!(texts(&f), ["Sync would rebuild this environment"]);
    }

    #[test]
    fn commits_to_push_and_pull_are_notes() {
        let pin = PythonPin::Unpinned;
        let git = GitStatus {
            ahead: 2,
            behind: 1,
            ..GitStatus::default()
        };
        let f = findings(&Input {
            git: Some(&git),
            ..input(&pin)
        });
        assert_eq!(texts(&f), ["2 commits not pushed", "1 commit to pull"]);
        assert!(f.iter().all(|f| f.level == Level::Note));
        assert!(!has_warning(&f), "notes never raise the badge");
    }

    #[test]
    fn failed_checks_warn_and_running_ones_note() {
        use bombadil_core::github::{Check, CheckState, PrStatus, PullRequest};
        let check = |state| Check {
            name: "t".into(),
            workflow: None,
            state,
            url: None,
            started: None,
            completed: None,
        };
        let pr = |checks| PrStatus {
            pr: PullRequest {
                number: 12,
                title: "x".into(),
                url: "https://github.com/o/r/pull/12".into(),
                draft: false,
                head_branch: "f".into(),
                base_branch: "main".into(),
                head_sha: "abc".into(),
            },
            checks,
        };
        let pin = PythonPin::Unpinned;
        let failed = pr(vec![check(CheckState::Failed), check(CheckState::Passed)]);
        let f = findings(&Input {
            pr: Some(&failed),
            ..input(&pin)
        });
        assert_eq!(texts(&f), ["1 of 2 checks failed on #12"]);
        assert_eq!(
            f[0].fix,
            Some(Fix::OpenPullRequest(
                "https://github.com/o/r/pull/12".into()
            ))
        );
        let running = pr(vec![check(CheckState::Running), check(CheckState::Passed)]);
        let f = findings(&Input {
            pr: Some(&running),
            ..input(&pin)
        });
        assert_eq!(texts(&f), ["Checks running on #12"]);
        assert_eq!(f[0].level, Level::Note);
        let passed = pr(vec![check(CheckState::Passed)]);
        assert!(
            findings(&Input {
                pr: Some(&passed),
                ..input(&pin)
            })
            .is_empty()
        );
    }

    #[test]
    fn warnings_come_before_notes() {
        // The newer-Python note is found before the sync warning; the
        // warning must still come first.
        let pin = PythonPin::Unpinned;
        let installed = ["3.14.7".to_string()];
        let behind = SyncCheck::Behind {
            missing: 1,
            changed: 0,
            extra: 0,
        };
        let f = findings(&Input {
            installed: Some(&installed),
            sync: Some(&behind),
            ..input(&pin)
        });
        assert_eq!(f[0].level, Level::Warning);
        assert_eq!(f[1].level, Level::Note);
        assert_eq!(
            tooltip(&f),
            "Out of step with uv.lock: 1 missing\n\
             Python 3.14 is installed and allowed; this environment uses 3.12"
        );
    }
}
