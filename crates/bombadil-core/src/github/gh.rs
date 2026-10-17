//! The `gh` path: GitHub as the user's own `gh` sees it -- their login, their
//! hosts (Enterprise included), their SSO -- with no token in Bombadil.

use super::{Check, CheckState, Lookup, PrStatus, PullRequest, now_unix};
use crate::process::{CommandRunner, CommandSpec};
use serde::Deserialize;
use std::path::Path;

pub const PR_FIELDS: &str = "number,title,url,isDraft,headRefName,baseRefName,headRefOid,\
     headRepositoryOwner,statusCheckRollup";

/// How long to leave a rate-limited project alone when `gh` does not say.
const RATE_LIMIT_BACKOFF: i64 = 600;

#[derive(Debug, PartialEq, Eq)]
pub enum Gh {
    Answered(Lookup),
    /// Not installed, or not signed in: try the token instead.
    Unavailable,
}

enum Reply {
    Ok(String),
    Unavailable,
    Failed(String),
}

fn run(dir: &Path, runner: &dyn CommandRunner, token: Option<&str>, args: &[&str]) -> Reply {
    let mut spec = CommandSpec::new("gh").args(args.iter().copied()).cwd(dir);
    if let Some(token) = token {
        spec = spec.env("GH_TOKEN", token);
    }
    match runner.run(&spec) {
        Err(_) => Reply::Unavailable,
        Ok(out) if out.success() => Reply::Ok(out.stdout),
        Ok(out) => Reply::Failed(out.stderr.trim().to_string()),
    }
}

/// Order matters: gh's "not a GitHub host" message also says `gh auth login`.
fn failure(stderr: String) -> Gh {
    if stderr.contains("known GitHub host") || stderr.contains("no git remotes") {
        Gh::Answered(Lookup::NotGitHub)
    } else if stderr.contains("gh auth login") {
        Gh::Unavailable
    } else if stderr.to_ascii_lowercase().contains("rate limit") {
        Gh::Answered(Lookup::RateLimited {
            until: now_unix() + RATE_LIMIT_BACKOFF,
        })
    } else {
        Gh::Answered(Lookup::Failed(stderr))
    }
}

/// `head_owner` is who the branch is pushed to: `--head` matches the branch
/// name alone, so a same-named branch on someone else's fork would match too.
pub fn lookup(
    dir: &Path,
    branch: &str,
    head_owner: Option<&str>,
    token: Option<&str>,
    runner: &dyn CommandRunner,
) -> Gh {
    let repo = match run(
        dir,
        runner,
        token,
        &["repo", "view", "--json", "defaultBranchRef"],
    ) {
        Reply::Ok(out) => out,
        Reply::Unavailable => return Gh::Unavailable,
        Reply::Failed(stderr) => return failure(stderr),
    };
    let Some(default) = parse_default_branch(&repo) else {
        return Gh::Answered(Lookup::Failed("gh gave an unexpected answer".into()));
    };
    if default == branch {
        return Gh::Answered(Lookup::DefaultBranch(default));
    }
    let args = [
        "pr", "list", "--head", branch, "--state", "open", "--limit", "30", "--json", PR_FIELDS,
    ];
    match run(dir, runner, token, &args) {
        Reply::Ok(out) => Gh::Answered(match parse_pr_list(&out, head_owner) {
            Some(Some(status)) => Lookup::Found(status),
            Some(None) => Lookup::NoPullRequest(branch.to_string()),
            None => Lookup::Failed("gh gave an unexpected answer".into()),
        }),
        Reply::Unavailable => Gh::Unavailable,
        Reply::Failed(stderr) => failure(stderr),
    }
}

/// A `gh auth` command that sees gh's own accounts, not a token from the
/// environment: gh refuses `auth login` while `GH_TOKEN` is set and reports
/// only that token's account, and it treats an empty value as unset.
fn auth(args: &[&str]) -> CommandSpec {
    CommandSpec::new("gh")
        .args(args.iter().copied())
        .env("GH_TOKEN", "")
        .env("GITHUB_TOKEN", "")
}

/// One account `gh` is signed in to on github.com.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub login: String,
    pub active: bool,
}

/// The github.com accounts `gh` is signed in to; empty when `gh` is missing.
/// Other hosts are left out: the chosen account's token is for github.com.
pub fn accounts(runner: &dyn CommandRunner) -> Vec<Account> {
    #[derive(Deserialize)]
    struct Status {
        hosts: std::collections::HashMap<String, Vec<Entry>>,
    }
    #[derive(Deserialize)]
    struct Entry {
        login: String,
        active: bool,
        state: String,
    }
    let spec = auth(&["auth", "status", "--json", "hosts"]);
    // gh exits 1 when any account has a problem but still prints the JSON.
    let Ok(out) = runner.run(&spec) else {
        return Vec::new();
    };
    serde_json::from_str::<Status>(&out.stdout)
        .ok()
        .and_then(|mut s| s.hosts.remove("github.com"))
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.state == "success")
        .map(|e| Account {
            login: e.login,
            active: e.active,
        })
        .collect()
}

/// `login`'s token, from gh's own store. Never logged.
pub fn token_for(runner: &dyn CommandRunner, login: &str) -> Option<String> {
    let spec = auth(&["auth", "token", "--hostname", "github.com", "--user", login]);
    let out = runner.run(&spec).ok().filter(|o| o.success())?;
    Some(out.stdout.trim().to_string()).filter(|t| !t.is_empty())
}

/// The code from gh's "First copy your one-time code: XXXX-XXXX" line.
pub fn one_time_code(line: &str) -> Option<String> {
    let code = line.split_once("one-time code:")?.1.trim();
    (!code.is_empty()).then(|| code.to_string())
}

/// Signs a new account in through the browser, handing the one-time code to
/// `on_code` as soon as gh prints it, and returns the account's login.
///
/// `gh auth login` makes the new account gh's active one everywhere; the
/// account that was active in `before` is switched back afterwards, so only
/// Bombadil's choice changes. Blocking until the browser step is done.
pub fn add_account(
    runner: &dyn CommandRunner,
    before: &[Account],
    on_code: &mut dyn FnMut(String),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<String, String> {
    let spec = auth(&[
        "auth",
        "login",
        "--web",
        "--hostname",
        "github.com",
        "--git-protocol",
        "https",
        "--skip-ssh-key",
    ]);
    let out = runner
        .stream_cancellable(
            &spec,
            &mut |line| {
                if let Some(code) = one_time_code(line.text()) {
                    on_code(code);
                }
            },
            cancel,
        )
        .map_err(|_| "gh is not installed".to_string())?;
    if cancel.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("cancelled".into());
    }
    if !out.success() {
        let why = out.stderr.lines().last().unwrap_or("").trim();
        return Err(if why.is_empty() {
            "gh could not sign in".into()
        } else {
            why.to_string()
        });
    }
    let after = accounts(runner);
    let added = after
        .iter()
        .find(|a| !before.iter().any(|b| b.login == a.login))
        .or_else(|| after.iter().find(|a| a.active))
        .map(|a| a.login.clone())
        .ok_or_else(|| "gh signed in, but lists no github.com account".to_string())?;
    if let Some(previous) = before.iter().find(|a| a.active)
        && previous.login != added
    {
        let switch = auth(&[
            "auth",
            "switch",
            "--hostname",
            "github.com",
            "--user",
            previous.login.as_str(),
        ]);
        let _ = runner.run(&switch);
    }
    Ok(added)
}

/// The signed-in user, or `None` when `gh` is missing or signed out.
pub fn login(runner: &dyn CommandRunner) -> Option<String> {
    let spec = CommandSpec::new("gh").args(["api", "user", "--jq", ".login"]);
    let out = runner.run(&spec).ok().filter(|o| o.success())?;
    Some(out.stdout.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn parse_default_branch(json: &str) -> Option<String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Repo {
        default_branch_ref: Branch,
    }
    #[derive(Deserialize)]
    struct Branch {
        name: String,
    }
    serde_json::from_str::<Repo>(json)
        .ok()
        .map(|r| r.default_branch_ref.name)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPr {
    number: u64,
    title: String,
    url: String,
    is_draft: bool,
    head_ref_name: String,
    base_ref_name: String,
    head_ref_oid: String,
    head_repository_owner: Option<Owner>,
    status_check_rollup: Option<Vec<GhCheck>>,
}

#[derive(Deserialize)]
struct Owner {
    login: String,
}

/// `CheckRun` and `StatusContext` share one list with different fields.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhCheck {
    name: Option<String>,
    workflow_name: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    details_url: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    context: Option<String>,
    state: Option<String>,
    target_url: Option<String>,
}

fn some(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.is_empty())
}

/// The first pull request whose head is on `head_owner`'s repository, or the
/// first at all when the owner is unknown.
pub fn parse_pr_list(json: &str, head_owner: Option<&str>) -> Option<Option<PrStatus>> {
    let prs: Vec<GhPr> = serde_json::from_str(json).ok()?;
    let ours = |pr: &GhPr| match (head_owner, &pr.head_repository_owner) {
        (Some(want), Some(owner)) => owner.login.eq_ignore_ascii_case(want),
        _ => true,
    };
    Some(prs.into_iter().find(ours).map(|pr| {
        PrStatus {
            checks: pr
                .status_check_rollup
                .unwrap_or_default()
                .into_iter()
                .map(|c| match c.context {
                    Some(context) => Check {
                        name: context,
                        workflow: None,
                        state: CheckState::from_github(c.state.as_deref(), None),
                        url: some(c.target_url),
                        started: None,
                        completed: None,
                    },
                    None => Check {
                        name: c.name.unwrap_or_default(),
                        workflow: some(c.workflow_name),
                        state: CheckState::from_github(
                            c.status.as_deref(),
                            c.conclusion.as_deref(),
                        ),
                        url: some(c.details_url),
                        started: some(c.started_at),
                        completed: some(c.completed_at),
                    },
                })
                .collect(),
            pr: PullRequest {
                number: pr.number,
                title: pr.title,
                url: pr.url,
                draft: pr.is_draft,
                head_branch: pr.head_ref_name,
                base_branch: pr.base_ref_name,
                head_sha: pr.head_ref_oid,
            },
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::CheckState;
    use crate::process::{CommandOutcome, FakeCommandRunner};
    use std::sync::atomic::AtomicBool;

    // Trimmed from `gh pr list --head chore/v0.3.4 ... --json` on this repo,
    // 2026-10-17, plus one legacy StatusContext entry.
    const PR_LIST: &str = r#"[{"baseRefName":"main","headRefName":"chore/v0.3.4",
      "headRefOid":"5772ff82339d1d15098536a8c58ae679dec42e9f","isDraft":false,"number":52,
      "title":"chore: v0.3.4","url":"https://github.com/marcelotrevisani/bombadil/pull/52",
      "statusCheckRollup":[
        {"__typename":"CheckRun","completedAt":"2026-10-16T19:58:22Z","conclusion":"SUCCESS",
         "detailsUrl":"https://github.com/marcelotrevisani/bombadil/actions/runs/1/job/2",
         "name":"check (ubuntu-latest)","startedAt":"2026-10-16T19:57:23Z","status":"COMPLETED",
         "workflowName":"CI"},
        {"__typename":"CheckRun","completedAt":null,"conclusion":"","detailsUrl":"",
         "name":"hygiene","startedAt":"2026-10-16T19:57:22Z","status":"IN_PROGRESS",
         "workflowName":""},
        {"__typename":"StatusContext","context":"ci/legacy","state":"FAILURE",
         "targetUrl":"https://ci.example/1"}
      ]}]"#;

    fn ok(stdout: &str) -> CommandOutcome {
        CommandOutcome {
            status: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }
    fn err(code: i32, stderr: &str) -> CommandOutcome {
        CommandOutcome {
            status: Some(code),
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }
    const DEFAULT: &str = r#"{"defaultBranchRef":{"name":"main"}}"#;
    fn dir() -> &'static Path {
        Path::new("/p")
    }

    #[test]
    fn a_pr_list_parses_both_kinds_of_check() {
        let status = parse_pr_list(PR_LIST, None).unwrap().unwrap();
        assert_eq!(status.pr.number, 52);
        assert_eq!(
            status.pr.head_sha,
            "5772ff82339d1d15098536a8c58ae679dec42e9f"
        );
        assert!(!status.pr.draft);
        let [run, running, legacy] = &status.checks[..] else {
            panic!("3 checks")
        };
        assert_eq!(run.workflow.as_deref(), Some("CI"));
        assert_eq!(run.state, CheckState::Passed);
        assert!(
            run.url
                .as_deref()
                .unwrap()
                .contains("/actions/runs/1/job/2")
        );
        // Empty strings are GitHub's "none", not values.
        assert_eq!(running.workflow, None);
        assert_eq!(running.url, None);
        assert_eq!(running.state, CheckState::Running);
        assert_eq!(legacy.name, "ci/legacy");
        assert_eq!(legacy.state, CheckState::Failed);
        assert_eq!(legacy.url.as_deref(), Some("https://ci.example/1"));
    }

    #[test]
    fn an_empty_list_is_no_pr_and_garbage_is_unparseable() {
        assert_eq!(parse_pr_list("[]", None), Some(None));
        assert_eq!(parse_pr_list("{", None), None);
        let null_rollup = r#"[{"baseRefName":"main","headRefName":"b","headRefOid":"s",
          "isDraft":true,"number":1,"title":"t","url":"u","statusCheckRollup":null}]"#;
        assert!(
            parse_pr_list(null_rollup, None)
                .unwrap()
                .unwrap()
                .checks
                .is_empty()
        );
        assert_eq!(parse_default_branch(DEFAULT), Some("main".into()));
    }

    #[test]
    fn a_feature_branch_asks_for_its_open_pr() {
        let runner = FakeCommandRunner::new();
        runner.expect("gh", &["repo", "view"], ok(DEFAULT));
        runner.expect("gh", &["pr", "list"], ok(PR_LIST));
        let Gh::Answered(Lookup::Found(status)) =
            lookup(dir(), "chore/v0.3.4", None, None, &runner)
        else {
            panic!("found")
        };
        assert_eq!(status.pr.number, 52);
        let calls = runner.calls();
        assert_eq!(
            calls[0].args,
            ["repo", "view", "--json", "defaultBranchRef"]
        );
        assert_eq!(
            calls[1].args,
            [
                "pr",
                "list",
                "--head",
                "chore/v0.3.4",
                "--state",
                "open",
                "--limit",
                "30",
                "--json",
                PR_FIELDS
            ]
        );
        assert_eq!(calls[1].cwd.as_deref(), Some(dir()));
    }

    #[test]
    fn a_same_named_branch_from_another_fork_is_not_ours() {
        let two = r#"[
          {"baseRefName":"main","headRefName":"fix","headRefOid":"s1","isDraft":false,
           "number":1,"title":"theirs","url":"u1","statusCheckRollup":[],
           "headRepositoryOwner":{"login":"stranger"}},
          {"baseRefName":"main","headRefName":"fix","headRefOid":"s2","isDraft":false,
           "number":2,"title":"ours","url":"u2","statusCheckRollup":[],
           "headRepositoryOwner":{"login":"Me"}}]"#;
        let ours = parse_pr_list(two, Some("me")).unwrap().unwrap();
        assert_eq!(ours.pr.number, 2, "owners compare without case");
        assert_eq!(parse_pr_list(two, Some("nobody")), Some(None));
        assert_eq!(parse_pr_list(two, None).unwrap().unwrap().pr.number, 1);
    }

    #[test]
    fn the_default_branch_stops_before_asking_for_prs() {
        let runner = FakeCommandRunner::new();
        runner.expect("gh", &["repo", "view"], ok(DEFAULT));
        assert_eq!(
            lookup(dir(), "main", None, None, &runner),
            Gh::Answered(Lookup::DefaultBranch("main".into()))
        );
        assert_eq!(runner.calls().len(), 1);
    }

    #[test]
    fn no_open_pr_says_which_branch() {
        let runner = FakeCommandRunner::new();
        runner.expect("gh", &["repo", "view"], ok(DEFAULT));
        runner.expect("gh", &["pr", "list"], ok("[]"));
        assert_eq!(
            lookup(dir(), "feat/x", None, None, &runner),
            Gh::Answered(Lookup::NoPullRequest("feat/x".into()))
        );
    }

    #[test]
    fn gh_missing_or_signed_out_is_unavailable() {
        // Unprogrammed: the fake returns Err, as a missing binary does.
        assert_eq!(
            lookup(dir(), "feat/x", None, None, &FakeCommandRunner::new()),
            Gh::Unavailable
        );
        let runner = FakeCommandRunner::new();
        runner.expect(
            "gh",
            &["repo", "view"],
            err(
                4,
                "To get started with GitHub CLI, please run:  gh auth login\n",
            ),
        );
        assert_eq!(
            lookup(dir(), "feat/x", None, None, &runner),
            Gh::Unavailable
        );
    }

    #[test]
    fn a_non_github_remote_is_not_github_even_though_gh_mentions_auth_login() {
        // Verbatim from gh 2.x against a gitlab.com origin, 2026-10-17.
        let runner = FakeCommandRunner::new();
        runner.expect("gh", &["repo", "view"], err(1,
            "none of the git remotes configured for this repository point to a known GitHub host. \
             To tell gh about a new GitHub host, please use `gh auth login`\n"));
        assert_eq!(
            lookup(dir(), "feat/x", None, None, &runner),
            Gh::Answered(Lookup::NotGitHub)
        );
    }

    #[test]
    fn rate_limits_and_other_failures_are_answers() {
        let runner = FakeCommandRunner::new();
        runner.expect(
            "gh",
            &["repo", "view"],
            err(1, "GraphQL: API rate limit exceeded for user"),
        );
        assert!(matches!(
            lookup(dir(), "feat/x", None, None, &runner),
            Gh::Answered(Lookup::RateLimited { until }) if until > crate::github::now_unix()
        ));
        let runner = FakeCommandRunner::new();
        runner.expect("gh", &["repo", "view"], ok(DEFAULT));
        runner.expect("gh", &["pr", "list"], err(1, "HTTP 502: Bad Gateway\n"));
        assert_eq!(
            lookup(dir(), "feat/x", None, None, &runner),
            Gh::Answered(Lookup::Failed("HTTP 502: Bad Gateway".into()))
        );
    }

    #[test]
    fn login_reads_the_signed_in_user() {
        let runner = FakeCommandRunner::new();
        runner.expect("gh", &["api", "user"], ok("marcelotrevisani\n"));
        assert_eq!(login(&runner), Some("marcelotrevisani".into()));
        assert_eq!(login(&FakeCommandRunner::new()), None);
    }

    // From `gh auth status --json hosts` with gh 2.92, 2026-10-17, renamed.
    const STATUS: &str = r#"{"hosts":{"github.com":[
      {"active":false,"host":"github.com","login":"me","state":"success"},
      {"active":true,"host":"github.com","login":"work","state":"success"},
      {"active":false,"host":"github.com","login":"stale","state":"error"}],
      "ghe.example":[{"active":true,"host":"ghe.example","login":"corp","state":"success"}]}}"#;

    fn account(login: &str, active: bool) -> Account {
        Account {
            login: login.into(),
            active,
        }
    }

    #[test]
    fn accounts_are_the_signed_in_github_com_ones() {
        let runner = FakeCommandRunner::new();
        runner.expect("gh", &["auth", "status"], ok(STATUS));
        assert_eq!(
            accounts(&runner),
            vec![account("me", false), account("work", true)]
        );
        assert_eq!(
            runner.calls()[0].args,
            ["auth", "status", "--json", "hosts"]
        );
        assert!(accounts(&FakeCommandRunner::new()).is_empty(), "no gh");
    }

    #[test]
    fn an_account_s_token_is_asked_for_by_name() {
        let runner = FakeCommandRunner::new();
        runner.expect("gh", &["auth", "token"], ok("gho_x\n"));
        assert_eq!(token_for(&runner, "me").as_deref(), Some("gho_x"));
        assert_eq!(
            runner.calls()[0].args,
            ["auth", "token", "--hostname", "github.com", "--user", "me"]
        );
        assert_eq!(token_for(&FakeCommandRunner::new(), "me"), None);
    }

    #[test]
    fn a_chosen_account_s_token_goes_to_every_gh_call() {
        let runner = FakeCommandRunner::new();
        runner.expect("gh", &["repo", "view"], ok(DEFAULT));
        runner.expect("gh", &["pr", "list"], ok("[]"));
        let _ = lookup(dir(), "feat/x", None, Some("gho_x"), &runner);
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert!(
            calls
                .iter()
                .all(|c| c.env.get("GH_TOKEN").map(String::as_str) == Some("gho_x"))
        );
    }

    #[test]
    fn the_one_time_code_is_read_from_gh_s_prompt() {
        assert_eq!(
            one_time_code("! First copy your one-time code: AB12-CD34").as_deref(),
            Some("AB12-CD34")
        );
        assert_eq!(one_time_code("Open this URL to continue"), None);
    }

    #[test]
    fn adding_an_account_shows_the_code_and_switches_gh_back() {
        let runner = FakeCommandRunner::new();
        runner.expect(
            "gh",
            &["auth", "login"],
            err(
                0,
                "! First copy your one-time code: AB12-CD34\nOpen this URL\n",
            ),
        );
        runner.expect(
            "gh",
            &["auth", "status"],
            ok(r#"{"hosts":{"github.com":[
              {"active":false,"login":"work","state":"success"},
              {"active":true,"login":"new","state":"success"}]}}"#),
        );
        runner.expect("gh", &["auth", "switch"], ok(""));
        let mut codes = Vec::new();
        let added = add_account(
            &runner,
            &[account("work", true)],
            &mut |c| codes.push(c),
            &AtomicBool::new(false),
        );
        assert_eq!(added, Ok("new".to_string()));
        assert_eq!(codes, ["AB12-CD34"]);
        let calls = runner.calls();
        assert_eq!(
            calls[0].args,
            [
                "auth",
                "login",
                "--web",
                "--hostname",
                "github.com",
                "--git-protocol",
                "https",
                "--skip-ssh-key"
            ]
        );
        assert_eq!(
            calls.last().unwrap().args,
            [
                "auth",
                "switch",
                "--hostname",
                "github.com",
                "--user",
                "work"
            ]
        );
    }

    #[test]
    fn account_commands_ignore_tokens_in_the_environment() {
        // gh refuses `auth login` while GH_TOKEN is set, and lists only the
        // environment's account; empty values are what gh treats as unset.
        let runner = FakeCommandRunner::new();
        runner.expect("gh", &["auth", "login"], ok(""));
        runner.expect("gh", &["auth", "status"], ok(STATUS));
        runner.expect("gh", &["auth", "token"], ok("t\n"));
        runner.expect("gh", &["auth", "switch"], ok(""));
        let _ = add_account(
            &runner,
            &[account("other", true)],
            &mut |_| {},
            &AtomicBool::new(false),
        );
        let _ = token_for(&runner, "me");
        let calls = runner.calls();
        assert_eq!(calls.len(), 4, "login, status, switch, token");
        for call in calls {
            for key in ["GH_TOKEN", "GITHUB_TOKEN"] {
                assert_eq!(
                    call.env.get(key).map(String::as_str),
                    Some(""),
                    "{key} in {:?}",
                    call.args
                );
            }
        }
    }

    #[test]
    fn a_cancelled_login_stops_there() {
        let runner = FakeCommandRunner::new();
        runner.expect("gh", &["auth", "login"], err(1, "killed\n"));
        let added = add_account(
            &runner,
            &[account("work", true)],
            &mut |_| {},
            &AtomicBool::new(true),
        );
        assert_eq!(added, Err("cancelled".to_string()));
        assert_eq!(runner.calls().len(), 1, "no status, no switch");
    }

    #[test]
    fn a_failed_login_is_an_error_and_switches_nothing() {
        let runner = FakeCommandRunner::new();
        runner.expect("gh", &["auth", "login"], err(1, "expired_token\n"));
        let added = add_account(
            &runner,
            &[account("work", true)],
            &mut |_| {},
            &AtomicBool::new(false),
        );
        assert!(matches!(added, Err(m) if m.contains("expired_token")));
        assert!(
            !runner
                .calls()
                .iter()
                .any(|c| c.args.contains(&"switch".to_string()))
        );
    }
}
