//! A branch's open pull request on GitHub and the state of its CI checks,
//! read only. Nothing here writes to GitHub.
//!
//! `lookup` answers one question per project -- "what is this branch's open
//! pull request, and how are its checks doing?" -- as a [`Lookup`] that always
//! carries a reason, so the GitHub tab is never blank without saying why.
//! `gh` is asked first; when it is missing or signed out, a token from the
//! keychain is used against the REST API instead.

pub mod gh;
pub mod rest;
pub mod transition;

use crate::process::{CommandRunner, CommandSpec};
use crate::secrets::{SecretKey, SecretStore};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub draft: bool,
    pub head_branch: String,
    pub base_branch: String,
    pub head_sha: String,
}

/// Declared in display order: failures first, then what is still moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckState {
    Failed,
    Running,
    Queued,
    Passed,
    Skipped,
}

impl CheckState {
    /// GitHub's vocabulary in either case: `gh` (GraphQL) says `SUCCESS`,
    /// REST says `success`. A legacy commit status has only a `state`, passed
    /// here as `status`.
    pub fn from_github(status: Option<&str>, conclusion: Option<&str>) -> CheckState {
        let upper = |s: Option<&str>| s.filter(|s| !s.is_empty()).map(str::to_ascii_uppercase);
        match upper(conclusion).as_deref() {
            Some("SUCCESS" | "NEUTRAL") => return CheckState::Passed,
            Some("SKIPPED") => return CheckState::Skipped,
            // FAILURE, TIMED_OUT, CANCELLED, ACTION_REQUIRED, STARTUP_FAILURE, STALE.
            Some(_) => return CheckState::Failed,
            None => {}
        }
        match upper(status).as_deref() {
            Some("IN_PROGRESS") => CheckState::Running,
            Some("SUCCESS" | "COMPLETED") => CheckState::Passed,
            Some("FAILURE" | "ERROR") => CheckState::Failed,
            _ => CheckState::Queued,
        }
    }

    pub fn done(self) -> bool {
        !matches!(self, CheckState::Running | CheckState::Queued)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    /// The Actions workflow, when GitHub says (`gh` does; REST does not).
    pub workflow: Option<String>,
    pub state: CheckState,
    pub url: Option<String>,
    /// RFC 3339, UTC, as GitHub sent them.
    pub started: Option<String>,
    pub completed: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrStatus {
    pub pr: PullRequest,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub total: usize,
    pub done: usize,
    pub passed: usize,
    pub skipped: usize,
    pub failed: usize,
}

impl PrStatus {
    pub fn counts(&self) -> Counts {
        let mut counts = Counts {
            total: self.checks.len(),
            ..Counts::default()
        };
        for check in &self.checks {
            match check.state {
                CheckState::Passed => counts.passed += 1,
                CheckState::Skipped => counts.skipped += 1,
                CheckState::Failed => counts.failed += 1,
                CheckState::Running | CheckState::Queued => {}
            }
        }
        counts.done = counts.passed + counts.skipped + counts.failed;
        counts
    }
}

/// What one lookup found. Every variant but `Found` is a reason the tab shows
/// instead of a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    NotGit,
    NotGitHub,
    Detached,
    DefaultBranch(String),
    NoPullRequest(String),
    NeedsAuth,
    TokenRejected,
    /// Unix seconds when GitHub will answer again.
    RateLimited {
        until: i64,
    },
    Found(PrStatus),
    Failed(String),
    /// The account chosen in Preferences is no longer signed in to `gh`.
    AccountGone(String),
}

/// `YYYY-MM-DDTHH:MM:SSZ` to Unix seconds. GitHub always sends UTC with `Z`;
/// anything else is `None` rather than a guess.
pub fn unix_seconds(stamp: &str) -> Option<i64> {
    let (date, time) = stamp.strip_suffix('Z')?.split_once('T')?;
    let mut d = date.splitn(3, '-').map(str::parse::<i64>);
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let mut t = time.splitn(3, ':').map(str::parse::<i64>);
    let (h, min, s) = (t.next()?.ok()?, t.next()?.ok()?, t.next()?.ok()?);
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3_600 + min * 60 + s)
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `text` with every occurrence of `secret` replaced, so an error message can
/// never carry the token it was made with.
pub fn redact(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        text.to_string()
    } else {
        text.replace(secret, "***")
    }
}

/// This project's pull request and checks, or why there is nothing to show.
/// Blocking: call off the render thread.
pub fn lookup(
    dir: &Path,
    runner: &dyn CommandRunner,
    store: &dyn SecretStore,
    fetch: &dyn rest::Fetch,
    account: Option<&str>,
) -> Lookup {
    let Some(git) = crate::git::status(dir, runner) else {
        return Lookup::NotGit;
    };
    if git.detached {
        return Lookup::Detached;
    }
    // The account chosen in Preferences, or gh's active one. A chosen account
    // that left gh is said, never quietly swapped for whoever is active.
    let as_user = match account {
        Some(login) => match gh::token_for(runner, login) {
            Some(token) => Some(token),
            None => return Lookup::AccountGone(login.to_string()),
        },
        None => None,
    };
    let remote = branch_remote(dir, &git.branch, runner);
    let url = remote_url(dir, &remote, runner);
    let head_owner = url.as_deref().and_then(remote_owner);
    if let gh::Gh::Answered(found) = gh::lookup(
        dir,
        &git.branch,
        head_owner.as_deref(),
        as_user.as_deref(),
        runner,
    ) {
        return found;
    }
    let Some((owner, repo)) = url.as_deref().and_then(rest::owner_repo) else {
        return Lookup::NotGitHub;
    };
    match token(store) {
        Some(token) => rest::lookup(&owner, &repo, &git.branch, &token, fetch),
        // Nobody to ask, but git itself knows the default branch.
        None if is_local_default(dir, &remote, &git.branch, runner) => {
            Lookup::DefaultBranch(git.branch)
        }
        None => Lookup::NeedsAuth,
    }
}

/// Preferences' one-line answer to "how is Bombadil reaching GitHub?".
pub fn source(
    runner: &dyn CommandRunner,
    store: &dyn SecretStore,
    account: Option<&str>,
) -> String {
    if let Some(login) = account {
        return match gh::token_for(runner, login) {
            Some(_) => format!("Using gh as {login}"),
            None => format!("{login} is no longer signed in to gh"),
        };
    }
    match gh::login(runner) {
        Some(user) => format!("Using gh (logged in as {user})"),
        None if token(store).is_some() => "Using saved token".to_string(),
        None => "Not signed in".to_string(),
    }
}

fn token(store: &dyn SecretStore) -> Option<String> {
    let token = store.get(&SecretKey::GitHubToken).ok()??;
    Some(token.trim().to_string()).filter(|t| !t.is_empty())
}

/// The remote the branch tracks, else `origin`.
fn branch_remote(dir: &Path, branch: &str, runner: &dyn CommandRunner) -> String {
    let key = format!("branch.{branch}.remote");
    let spec = CommandSpec::new("git")
        .args(["config", "--get", key.as_str()])
        .cwd(dir);
    runner
        .run(&spec)
        .ok()
        .filter(|o| o.success())
        .map(|o| o.stdout.trim().to_string())
        .filter(|r| !r.is_empty() && r != ".")
        .unwrap_or_else(|| "origin".into())
}

fn remote_url(dir: &Path, remote: &str, runner: &dyn CommandRunner) -> Option<String> {
    let spec = CommandSpec::new("git")
        .args(["-c", "core.fsmonitor=false", "remote", "get-url", remote])
        .cwd(dir);
    let out = runner.run(&spec).ok().filter(|o| o.success())?;
    Some(out.stdout.trim().to_string())
}

/// The owner in any host's `owner/repo` remote, Enterprise included.
fn remote_owner(url: &str) -> Option<String> {
    let path = url.trim().trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.rsplit(['/', ':']);
    let (_repo, owner) = (parts.next()?, parts.next()?);
    Some(owner.to_string()).filter(|o| !o.is_empty() && !o.contains('@'))
}

/// Whether `branch` is the default as git last heard it (`<remote>/HEAD`);
/// `main` or `master` when the clone never recorded one.
fn is_local_default(dir: &Path, remote: &str, branch: &str, runner: &dyn CommandRunner) -> bool {
    let head = format!("refs/remotes/{remote}/HEAD");
    let spec = CommandSpec::new("git")
        .args(["symbolic-ref", "--short", head.as_str()])
        .cwd(dir);
    match runner.run(&spec).ok().filter(|o| o.success()) {
        Some(out) => out.stdout.trim().strip_prefix(&format!("{remote}/")) == Some(branch),
        None => matches!(branch, "main" | "master"),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn github_states_map_in_either_case() {
        use CheckState::*;
        let cases: &[(Option<&str>, Option<&str>, CheckState)] = &[
            (Some("COMPLETED"), Some("SUCCESS"), Passed),
            (Some("completed"), Some("neutral"), Passed),
            (Some("COMPLETED"), Some("SKIPPED"), Skipped),
            (Some("COMPLETED"), Some("FAILURE"), Failed),
            (Some("completed"), Some("timed_out"), Failed),
            (Some("COMPLETED"), Some("CANCELLED"), Failed),
            (Some("COMPLETED"), Some("ACTION_REQUIRED"), Failed),
            (Some("COMPLETED"), Some("STARTUP_FAILURE"), Failed),
            (Some("IN_PROGRESS"), None, Running),
            (Some("in_progress"), Some(""), Running),
            (Some("QUEUED"), None, Queued),
            (Some("WAITING"), None, Queued),
            (Some("REQUESTED"), None, Queued),
            (Some("PENDING"), None, Queued),
            // StatusContext `state`, passed as status with no conclusion.
            (Some("SUCCESS"), None, Passed),
            (Some("FAILURE"), None, Failed),
            (Some("ERROR"), None, Failed),
            (Some("EXPECTED"), None, Queued),
            (None, None, Queued),
        ];
        for (status, conclusion, want) in cases {
            assert_eq!(
                CheckState::from_github(*status, *conclusion),
                *want,
                "{status:?} {conclusion:?}"
            );
        }
    }

    fn check(state: CheckState) -> Check {
        Check {
            name: "c".into(),
            workflow: None,
            state,
            url: None,
            started: None,
            completed: None,
        }
    }

    pub(crate) fn status_with(sha: &str, states: &[CheckState]) -> PrStatus {
        PrStatus {
            pr: PullRequest {
                number: 7,
                title: "Fix it".into(),
                url: "https://github.com/o/r/pull/7".into(),
                draft: false,
                head_branch: "feat/x".into(),
                base_branch: "main".into(),
                head_sha: sha.into(),
            },
            checks: states.iter().copied().map(check).collect(),
        }
    }

    #[test]
    fn counts_split_done_passed_skipped_failed() {
        use CheckState::*;
        let counts = status_with("a", &[Passed, Skipped, Failed, Running, Queued]).counts();
        assert_eq!(
            counts,
            Counts {
                total: 5,
                done: 3,
                passed: 1,
                skipped: 1,
                failed: 1
            }
        );
    }

    #[test]
    fn github_timestamps_become_unix_seconds() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        // `date -u -d 2026-10-16T19:58:22Z +%s`
        assert_eq!(unix_seconds("2026-10-16T19:58:22Z"), Some(1_792_180_702));
        assert_eq!(unix_seconds("2024-02-29T12:00:00Z"), Some(1_709_208_000));
        assert_eq!(unix_seconds("not a time"), None);
        assert_eq!(unix_seconds("2026-10-16T19:58:22+01:00"), None);
    }

    #[test]
    fn redact_hides_every_occurrence_and_ignores_an_empty_secret() {
        assert_eq!(redact("a ghp_x b ghp_x", "ghp_x"), "a *** b ***");
        assert_eq!(redact("plain", ""), "plain");
    }

    mod combined {
        use crate::github::rest::{Response, tests_support::FakeFetch};
        use crate::github::*;
        use crate::process::{CommandOutcome, FakeCommandRunner};
        use crate::secrets::{InMemorySecretStore, SecretKey, SecretStore};
        use std::path::Path;

        fn git(stdout: &str) -> CommandOutcome {
            CommandOutcome {
                status: Some(0),
                stdout: stdout.into(),
                stderr: String::new(),
            }
        }
        const ON_FEAT: &str = "# branch.oid abc\n# branch.head feat/x\n";

        #[test]
        fn not_a_repository_and_detached_head_ask_nobody() {
            let runner = FakeCommandRunner::new();
            let none = FakeFetch::default();
            let store = InMemorySecretStore::new();
            // git unprogrammed: fails, which is "not a repository".
            assert_eq!(
                lookup(Path::new("/p"), &runner, &store, &none, None),
                Lookup::NotGit
            );
            let runner = FakeCommandRunner::new();
            runner.expect(
                "git",
                &["status"],
                git("# branch.oid abc\n# branch.head (detached)\n"),
            );
            assert_eq!(
                lookup(Path::new("/p"), &runner, &store, &none, None),
                Lookup::Detached
            );
            assert!(none.asked.lock().unwrap().is_empty());
        }

        #[test]
        fn without_gh_a_github_remote_and_no_token_needs_auth() {
            let runner = FakeCommandRunner::new();
            runner.expect("git", &["status"], git(ON_FEAT));
            runner.expect("git", &["get-url"], git("https://github.com/o/r.git\n"));
            let store = InMemorySecretStore::new();
            assert_eq!(
                lookup(
                    Path::new("/p"),
                    &runner,
                    &store,
                    &FakeFetch::default(),
                    None
                ),
                Lookup::NeedsAuth
            );
        }

        #[test]
        fn without_gh_a_non_github_remote_is_not_github() {
            let runner = FakeCommandRunner::new();
            runner.expect("git", &["status"], git(ON_FEAT));
            runner.expect("git", &["get-url"], git("https://gitlab.com/o/r.git\n"));
            let store = InMemorySecretStore::new();
            store.set(&SecretKey::GitHubToken, "t").unwrap();
            assert_eq!(
                lookup(
                    Path::new("/p"),
                    &runner,
                    &store,
                    &FakeFetch::default(),
                    None
                ),
                Lookup::NotGitHub
            );
        }

        #[test]
        fn without_gh_the_saved_token_is_used() {
            let runner = FakeCommandRunner::new();
            runner.expect("git", &["status"], git(ON_FEAT));
            runner.expect("git", &["get-url"], git("git@github.com:o/r.git\n"));
            let store = InMemorySecretStore::new();
            store.set(&SecretKey::GitHubToken, " tok \n").unwrap();
            let fetch = FakeFetch {
                answers: vec![(
                    "/repos/o/r",
                    Ok(Response {
                        status: 200,
                        body: r#"{"default_branch":"feat/x"}"#.into(),
                        remaining: None,
                        reset: None,
                        retry_after: None,
                    }),
                )],
                ..Default::default()
            };
            assert_eq!(
                lookup(Path::new("/p"), &runner, &store, &fetch, None),
                Lookup::DefaultBranch("feat/x".into())
            );
            assert_eq!(fetch.asked.lock().unwrap()[0].1, "tok", "trimmed");
        }

        #[test]
        fn signed_out_the_default_branch_is_still_known_locally() {
            let store = InMemorySecretStore::new();
            let none = FakeFetch::default();
            let runner = FakeCommandRunner::new();
            runner.expect("git", &["status"], git("# branch.head trunk\n"));
            runner.expect("git", &["get-url"], git("https://github.com/o/r.git\n"));
            runner.expect("git", &["symbolic-ref"], git("origin/trunk\n"));
            assert_eq!(
                lookup(Path::new("/p"), &runner, &store, &none, None),
                Lookup::DefaultBranch("trunk".into())
            );
            // No origin/HEAD recorded: main and master are taken as default.
            let runner = FakeCommandRunner::new();
            runner.expect("git", &["status"], git("# branch.head master\n"));
            runner.expect("git", &["get-url"], git("https://github.com/o/r.git\n"));
            assert_eq!(
                lookup(Path::new("/p"), &runner, &store, &none, None),
                Lookup::DefaultBranch("master".into())
            );
        }

        #[test]
        fn the_branch_is_looked_up_where_it_is_pushed() {
            let runner = FakeCommandRunner::new();
            runner.expect("git", &["status"], git(ON_FEAT));
            runner.expect("git", &["branch.feat/x.remote"], git("mine\n"));
            runner.expect(
                "git",
                &["get-url", "mine"],
                git("git@github.com:me/r.git\n"),
            );
            runner.expect(
                "git",
                &["get-url", "origin"],
                git("git@github.com:o/r.git\n"),
            );
            let store = InMemorySecretStore::new();
            store.set(&SecretKey::GitHubToken, "t").unwrap();
            let fetch = FakeFetch::default();
            let _ = lookup(Path::new("/p"), &runner, &store, &fetch, None);
            assert!(
                fetch.asked.lock().unwrap()[0].0.ends_with("/repos/me/r"),
                "{:?}",
                fetch.asked
            );
        }

        #[test]
        fn a_chosen_account_that_left_gh_is_said_and_not_replaced() {
            let runner = FakeCommandRunner::new();
            runner.expect("git", &["status"], git(ON_FEAT));
            runner.expect("git", &["get-url"], git("https://github.com/o/r.git\n"));
            let store = InMemorySecretStore::new();
            store.set(&SecretKey::GitHubToken, "t").unwrap();
            let fetch = FakeFetch::default();
            assert_eq!(
                lookup(Path::new("/p"), &runner, &store, &fetch, Some("me")),
                Lookup::AccountGone("me".into())
            );
            assert!(fetch.asked.lock().unwrap().is_empty(), "no fallback");
            assert_eq!(
                source(&runner, &store, Some("me")),
                "me is no longer signed in to gh"
            );
            runner.expect("gh", &["auth", "token"], git("gho_x\n"));
            assert_eq!(source(&runner, &store, Some("me")), "Using gh as me");
        }

        #[test]
        fn the_source_line_names_gh_then_token_then_nothing() {
            let runner = FakeCommandRunner::new();
            runner.expect("gh", &["api", "user"], git("me\n"));
            let store = InMemorySecretStore::new();
            assert_eq!(source(&runner, &store, None), "Using gh (logged in as me)");
            let runner = FakeCommandRunner::new();
            assert_eq!(source(&runner, &store, None), "Not signed in");
            store.set(&SecretKey::GitHubToken, "t").unwrap();
            assert_eq!(source(&runner, &store, None), "Using saved token");
        }
    }
}
