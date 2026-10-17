//! The token path: GitHub's REST API with a personal access token from the
//! keychain, for machines without a signed-in `gh`. github.com only --
//! Enterprise hosts are reached through `gh`.

use super::{Check, CheckState, Lookup, PrStatus, PullRequest, now_unix, redact};
use serde_json::Value;

const API: &str = "https://api.github.com";
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub body: String,
    pub remaining: Option<u64>,
    /// Unix seconds when the rate limit resets.
    pub reset: Option<i64>,
    /// Seconds to wait, from `retry-after` (secondary rate limits).
    pub retry_after: Option<i64>,
}

/// One authenticated GET. Behind a trait so no test reaches the network.
pub trait Fetch: Send + Sync {
    fn get(&self, url: &str, token: &str) -> Result<Response, String>;
}

pub struct UreqFetch(ureq::Agent);

impl UreqFetch {
    /// Trusts what the machine trusts, as the uv download does
    /// (`bombadil-uv-bin/build.rs`), and hands every status back as data.
    pub fn new() -> Self {
        Self(
            ureq::Agent::config_builder()
                .timeout_global(Some(TIMEOUT))
                .http_status_as_error(false)
                .tls_config(
                    ureq::tls::TlsConfig::builder()
                        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                        .build(),
                )
                .build()
                .new_agent(),
        )
    }
}

impl Default for UreqFetch {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetch for UreqFetch {
    fn get(&self, url: &str, token: &str) -> Result<Response, String> {
        let mut response = self
            .0
            .get(url)
            .header("Authorization", &format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "bombadil")
            .call()
            .map_err(|e| e.to_string())?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let remaining = header("x-ratelimit-remaining").and_then(|v| v.parse().ok());
        let reset = header("x-ratelimit-reset").and_then(|v| v.parse().ok());
        let retry_after = header("retry-after").and_then(|v| v.parse().ok());
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|e| e.to_string())?;
        Ok(Response {
            status,
            body,
            remaining,
            reset,
            retry_after,
        })
    }
}

/// `origin`'s owner and repository, when it is on github.com.
pub fn owner_repo(remote: &str) -> Option<(String, String)> {
    let remote = remote.trim();
    let path = [
        "https://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ]
    .iter()
    .find_map(|prefix| remote.strip_prefix(prefix))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    (!owner.is_empty() && !repo.is_empty() && !repo.contains('/'))
        .then(|| (owner.to_string(), repo.to_string()))
}

/// Percent-encodes everything but unreserved characters and `/`, which
/// GitHub accepts literally in `head=` and branch names use constantly.
pub fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn lookup(owner: &str, repo: &str, branch: &str, token: &str, fetch: &dyn Fetch) -> Lookup {
    match lookup_inner(owner, repo, branch, token, fetch) {
        Ok(found) | Err(found) => found,
    }
}

// `Err` is an early answer, the same type as `Ok`; boxing it buys nothing.
#[allow(clippy::result_large_err)]
fn lookup_inner(
    owner: &str,
    repo: &str,
    branch: &str,
    token: &str,
    fetch: &dyn Fetch,
) -> Result<Lookup, Lookup> {
    let get = |path: String| -> Result<Value, Lookup> {
        let r = fetch
            .get(&format!("{API}{path}"), token)
            .map_err(|e| Lookup::Failed(redact(&e, token)))?;
        match r.status {
            200 => serde_json::from_str(&r.body)
                .map_err(|_| Lookup::Failed("GitHub gave an unexpected answer".into())),
            401 => Err(Lookup::TokenRejected),
            403 | 429 if r.remaining == Some(0) => Err(Lookup::RateLimited {
                until: r.reset.unwrap_or_else(|| now_unix() + 600),
            }),
            // A secondary rate limit: GitHub says how long, or a minute.
            403 | 429 if r.retry_after.is_some() => Err(Lookup::RateLimited {
                until: now_unix() + r.retry_after.unwrap_or(60),
            }),
            429 => Err(Lookup::RateLimited {
                until: now_unix() + 60,
            }),
            404 => Err(Lookup::Failed(format!(
                "{owner}/{repo} was not found, or the token cannot see it"
            ))),
            code => Err(Lookup::Failed(format!("GitHub answered HTTP {code}"))),
        }
    };
    let text = |v: &Value, key: &str| {
        v[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    let origin = get(format!("/repos/{owner}/{repo}"))?;
    let default = text(&origin, "default_branch").unwrap_or_default();
    if default == branch {
        return Ok(Lookup::DefaultBranch(default));
    }
    // A fork's pull requests usually live on its parent; look there first.
    let mut repos: Vec<String> = text(&origin["parent"], "full_name").into_iter().collect();
    repos.push(format!("{owner}/{repo}"));
    let mut found = None;
    for full in repos {
        let base = format!("/repos/{full}");
        let pulls = match get(format!(
            "{base}/pulls?head={owner}:{}&state=open",
            encode(branch)
        )) {
            Ok(pulls) => pulls,
            // A token scoped to the fork cannot see the parent: try the fork.
            Err(Lookup::Failed(_)) if full != format!("{owner}/{repo}") => continue,
            Err(other) => return Err(other),
        };
        if let Some(pr) = pulls.as_array().and_then(|a| a.first()) {
            found = Some((base, pr.clone()));
            break;
        }
    }
    let Some((base, pr)) = found else {
        return Ok(Lookup::NoPullRequest(branch.to_string()));
    };
    let pr = &pr;
    let pr = PullRequest {
        number: pr["number"].as_u64().unwrap_or(0),
        title: text(pr, "title").unwrap_or_default(),
        url: text(pr, "html_url").unwrap_or_default(),
        draft: pr["draft"].as_bool().unwrap_or(false),
        head_branch: text(&pr["head"], "ref").unwrap_or_default(),
        base_branch: text(&pr["base"], "ref").unwrap_or_default(),
        head_sha: text(&pr["head"], "sha").unwrap_or_default(),
    };
    let sha = encode(&pr.head_sha);
    // Every page, up to a thousand: a big matrix passes a hundred easily.
    let all = |path: String, key: &str| -> Result<Vec<Value>, Lookup> {
        let mut items = Vec::new();
        for page in 1..=10 {
            let body = get(format!("{path}?per_page=100&page={page}"))?;
            let batch = body[key].as_array().cloned().unwrap_or_default();
            let total = body["total_count"].as_u64().unwrap_or(0) as usize;
            let short = batch.len() < 100;
            items.extend(batch);
            if short || items.len() >= total {
                break;
            }
        }
        Ok(items)
    };
    let runs = all(format!("{base}/commits/{sha}/check-runs"), "check_runs")?;
    let statuses = all(format!("{base}/commits/{sha}/status"), "statuses")?;
    let mut checks: Vec<Check> = runs
        .iter()
        .map(|c| Check {
            name: text(c, "name").unwrap_or_default(),
            workflow: None,
            state: CheckState::from_github(c["status"].as_str(), c["conclusion"].as_str()),
            url: text(c, "details_url").or_else(|| text(c, "html_url")),
            started: text(c, "started_at"),
            completed: text(c, "completed_at"),
        })
        .collect();
    checks.extend(statuses.iter().map(|s| Check {
        name: text(s, "context").unwrap_or_default(),
        workflow: None,
        state: CheckState::from_github(s["state"].as_str(), None),
        url: text(s, "target_url"),
        started: None,
        completed: None,
    }));
    Ok(Lookup::Found(PrStatus { pr, checks }))
}

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;
    use std::sync::Mutex;

    /// Answers by URL substring, records every URL and token it was asked with.
    #[derive(Default)]
    pub(crate) struct FakeFetch {
        pub answers: Vec<(&'static str, Result<Response, String>)>,
        pub asked: Mutex<Vec<(String, String)>>,
    }
    impl Fetch for FakeFetch {
        fn get(&self, url: &str, token: &str) -> Result<Response, String> {
            self.asked.lock().unwrap().push((url.into(), token.into()));
            self.answers
                .iter()
                .find(|(part, _)| url.contains(part))
                .map(|(_, r)| r.clone())
                .unwrap_or_else(|| Err(format!("unexpected {url}")))
        }
    }
    pub(crate) fn ok(body: &str) -> Result<Response, String> {
        Ok(Response {
            status: 200,
            body: body.into(),
            remaining: Some(4999),
            reset: Some(0),
            retry_after: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::{FakeFetch, ok};
    use super::*;

    fn status(code: u16, remaining: Option<u64>, reset: Option<i64>) -> Result<Response, String> {
        Ok(Response {
            status: code,
            body: "{}".into(),
            remaining,
            reset,
            retry_after: None,
        })
    }
    const REPO: &str = r#"{"default_branch":"main"}"#;
    const PULLS: &str = r#"[{"number":9,"title":"Add x","html_url":"https://github.com/o/r/pull/9",
        "draft":true,"head":{"ref":"feat/x","sha":"abc"},"base":{"ref":"main"}}]"#;
    const RUNS: &str = r#"{"check_runs":[{"name":"test","status":"completed","conclusion":"failure",
        "details_url":"https://github.com/o/r/actions/runs/1/job/2","html_url":"https://x",
        "started_at":"2026-10-16T19:57:23Z","completed_at":"2026-10-16T19:58:22Z"}]}"#;
    const STATUSES: &str =
        r#"{"statuses":[{"context":"ci/legacy","state":"pending","target_url":""}]}"#;

    #[test]
    fn remotes_on_github_name_owner_and_repo() {
        let want = Some(("o".to_string(), "r".to_string()));
        assert_eq!(owner_repo("https://github.com/o/r.git\n"), want);
        assert_eq!(owner_repo("https://github.com/o/r"), want);
        assert_eq!(owner_repo("git@github.com:o/r.git"), want);
        assert_eq!(owner_repo("ssh://git@github.com/o/r.git"), want);
        assert_eq!(owner_repo("https://gitlab.com/o/r.git"), None);
        assert_eq!(owner_repo("https://github.com/o"), None);
    }

    #[test]
    fn a_pr_and_its_checks_are_read_with_the_token() {
        let fetch = FakeFetch {
            answers: vec![
                ("/pulls?", ok(PULLS)),
                ("/check-runs", ok(RUNS)),
                ("/status", ok(STATUSES)),
                ("/repos/o/r", ok(REPO)),
            ],
            ..Default::default()
        };
        let Lookup::Found(found) = lookup("o", "r", "feat/x", "tok", &fetch) else {
            panic!("found")
        };
        assert_eq!(found.pr.number, 9);
        assert!(found.pr.draft);
        assert_eq!(found.pr.head_sha, "abc");
        assert_eq!(found.checks[0].state, CheckState::Failed);
        assert!(
            found.checks[0]
                .url
                .as_deref()
                .unwrap()
                .contains("/actions/runs/1")
        );
        assert_eq!(found.checks[1].name, "ci/legacy");
        assert_eq!(found.checks[1].state, CheckState::Queued);
        assert_eq!(found.checks[1].url, None);
        let asked = fetch.asked.lock().unwrap();
        assert_eq!(asked[0].0, "https://api.github.com/repos/o/r");
        assert_eq!(
            asked[1].0,
            "https://api.github.com/repos/o/r/pulls?head=o:feat/x&state=open"
        );
        assert_eq!(
            asked[2].0,
            "https://api.github.com/repos/o/r/commits/abc/check-runs?per_page=100&page=1"
        );
        assert_eq!(
            asked[3].0,
            "https://api.github.com/repos/o/r/commits/abc/status?per_page=100&page=1"
        );
        assert!(asked.iter().all(|(_, token)| token == "tok"));
    }

    #[test]
    fn branch_names_are_percent_encoded() {
        assert_eq!(encode("feat/a b#1+&"), "feat/a%20b%231%2B%26");
    }

    #[test]
    fn the_default_branch_and_no_pr_stop_early() {
        let fetch = FakeFetch {
            answers: vec![("/repos/o/r", ok(REPO))],
            ..Default::default()
        };
        assert_eq!(
            lookup("o", "r", "main", "t", &fetch),
            Lookup::DefaultBranch("main".into())
        );
        let fetch = FakeFetch {
            answers: vec![("/pulls?", ok("[]")), ("/repos/o/r", ok(REPO))],
            ..Default::default()
        };
        assert_eq!(
            lookup("o", "r", "feat/x", "t", &fetch),
            Lookup::NoPullRequest("feat/x".into())
        );
    }

    #[test]
    fn http_failures_map_to_reasons() {
        let one = |answer| FakeFetch {
            answers: vec![("/repos/o/r", answer)],
            ..Default::default()
        };
        assert_eq!(
            lookup("o", "r", "b", "t", &one(status(401, None, None))),
            Lookup::TokenRejected
        );
        assert_eq!(
            lookup(
                "o",
                "r",
                "b",
                "t",
                &one(status(403, Some(0), Some(1_900_000_000)))
            ),
            Lookup::RateLimited {
                until: 1_900_000_000
            }
        );
        assert!(
            matches!(lookup("o", "r", "b", "t", &one(status(404, Some(10), None))),
            Lookup::Failed(m) if m.contains("o/r"))
        );
        assert!(
            matches!(lookup("o", "r", "b", "t", &one(status(502, Some(10), None))),
            Lookup::Failed(m) if m.contains("502"))
        );
    }

    #[test]
    fn secondary_rate_limits_wait_as_told() {
        let one = |answer| FakeFetch {
            answers: vec![("/repos/o/r", answer)],
            ..Default::default()
        };
        let now = now_unix();
        let mut told = status(403, Some(4000), None);
        if let Ok(r) = &mut told {
            r.retry_after = Some(120);
        }
        assert!(matches!(
            lookup("o", "r", "b", "t", &one(told)),
            Lookup::RateLimited { until } if (now + 119..=now + 130).contains(&until)
        ));
        assert!(matches!(
            lookup("o", "r", "b", "t", &one(status(429, None, None))),
            Lookup::RateLimited { until } if until > now
        ));
    }

    #[test]
    fn every_page_of_checks_is_read() {
        let run = r#"{"name":"n","status":"queued","conclusion":null}"#;
        let page = |n: usize| {
            format!(
                r#"{{"total_count":101,"check_runs":[{}]}}"#,
                vec![run; n].join(",")
            )
        };
        let fetch = FakeFetch {
            answers: vec![
                ("/pulls?", ok(PULLS)),
                ("/check-runs?per_page=100&page=1", ok(&page(100))),
                ("/check-runs?per_page=100&page=2", ok(&page(1))),
                ("/status", ok(STATUSES)),
                ("/repos/o/r", ok(REPO)),
            ],
            ..Default::default()
        };
        let Lookup::Found(found) = lookup("o", "r", "feat/x", "t", &fetch) else {
            panic!("found")
        };
        assert_eq!(found.checks.len(), 102, "101 runs and 1 status");
    }

    #[test]
    fn a_fork_finds_its_pull_request_on_the_parent() {
        let fork = r#"{"default_branch":"main","fork":true,"parent":{"full_name":"up/r"}}"#;
        let fetch = FakeFetch {
            answers: vec![
                ("/repos/up/r/pulls?head=o:feat/x", ok(PULLS)),
                ("/repos/up/r/commits/abc/check-runs", ok(RUNS)),
                ("/repos/up/r/commits/abc/status", ok(STATUSES)),
                ("/repos/o/r", ok(fork)),
            ],
            ..Default::default()
        };
        assert!(matches!(
            lookup("o", "r", "feat/x", "t", &fetch),
            Lookup::Found(found) if found.pr.number == 9
        ));
    }

    #[test]
    fn a_parent_the_token_cannot_see_falls_back_to_the_fork() {
        let fork = r#"{"default_branch":"main","fork":true,"parent":{"full_name":"up/r"}}"#;
        let fetch = FakeFetch {
            answers: vec![
                ("/repos/up/r", status(404, Some(10), None)),
                ("/repos/o/r/pulls?", ok(PULLS)),
                ("/check-runs", ok(RUNS)),
                ("/status", ok(STATUSES)),
                ("/repos/o/r", ok(fork)),
            ],
            ..Default::default()
        };
        assert!(matches!(
            lookup("o", "r", "feat/x", "t", &fetch),
            Lookup::Found(found) if found.pr.number == 9
        ));
    }

    #[test]
    fn the_token_never_reaches_an_error() {
        let fetch = FakeFetch {
            answers: vec![(
                "/repos/o/r",
                Err("connect failed for Bearer ghp_secret".into()),
            )],
            ..Default::default()
        };
        let Lookup::Failed(message) = lookup("o", "r", "b", "ghp_secret", &fetch) else {
            panic!("failed")
        };
        assert!(!message.contains("ghp_secret"), "{message}");
    }
}
