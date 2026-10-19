//! Known vulnerabilities in installed packages, from osv.dev. Only package
//! names and versions are sent.

use serde_json::{Value, json};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

const BATCH_URL: &str = "https://api.osv.dev/v1/querybatch";
const VULN_URL: &str = "https://api.osv.dev/v1/vulns/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vuln {
    pub id: String,
    pub summary: String,
    pub severity: Option<String>,
    /// Every PyPI `fixed` event across the advisory's version ranges.
    pub fixed: Vec<String>,
    /// Other ids for the same issue: GHSA and PYSEC both publish most.
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub id: String,
    pub summary: String,
    pub severity: Option<String>,
    /// The lowest release that fixes it above the installed one.
    pub fixed: Option<String>,
}

pub trait Http: Send + Sync {
    fn post_json(&self, url: &str, body: &str) -> Result<String, String>;
    fn get(&self, url: &str) -> Result<String, String>;
}

impl Http for crate::github::rest::UreqFetch {
    fn post_json(&self, url: &str, body: &str) -> Result<String, String> {
        self.agent()
            .post(url)
            .header("Content-Type", "application/json")
            .header("User-Agent", "bombadil")
            .send(body)
            .map_err(|e| e.to_string())?
            .body_mut()
            .read_to_string()
            .map_err(|e| e.to_string())
    }
    fn get(&self, url: &str) -> Result<String, String> {
        self.agent()
            .get(url)
            .header("User-Agent", "bombadil")
            .call()
            .map_err(|e| e.to_string())?
            .body_mut()
            .read_to_string()
            .map_err(|e| e.to_string())
    }
}

/// ponytail: release segments only (leading digits of each dotted part),
/// not full PEP 440; enough to order fixes and upgrades.
fn segments(v: &str) -> Vec<u64> {
    v.split('.')
        .map_while(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect()
}

pub fn version_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (segments(a), segments(b));
    for i in 0..a.len().max(b.len()) {
        match a.get(i).unwrap_or(&0).cmp(b.get(i).unwrap_or(&0)) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    Ordering::Equal
}

pub fn parse_batch(body: &str, expected: usize) -> Result<Vec<Vec<String>>, String> {
    let v: Value =
        serde_json::from_str(body).map_err(|_| "osv.dev sent an unexpected answer".to_string())?;
    let results = v["results"]
        .as_array()
        .ok_or("osv.dev sent an unexpected answer")?;
    if results.len() != expected {
        return Err("osv.dev answered for a different number of packages".into());
    }
    Ok(results
        .iter()
        .map(|r| {
            r["vulns"]
                .as_array()
                .map(|vs| {
                    vs.iter()
                        .filter_map(|x| x["id"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect())
}

pub fn parse_vuln(body: &str) -> Result<Vuln, String> {
    let v: Value =
        serde_json::from_str(body).map_err(|_| "osv.dev sent an unexpected answer".to_string())?;
    let id = v["id"]
        .as_str()
        .ok_or("osv.dev sent an unexpected answer")?
        .to_string();
    let fixed = v["affected"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["package"]["ecosystem"] == "PyPI")
        .flat_map(|a| a["ranges"].as_array().into_iter().flatten())
        // A GIT range's `fixed` is a commit hash, not a release.
        .filter(|r| r["type"] != "GIT")
        .flat_map(|r| r["events"].as_array().into_iter().flatten())
        .filter_map(|e| e["fixed"].as_str().map(str::to_string))
        .collect();
    // PYSEC records often have no summary; the first line of the details
    // says the same thing.
    let summary = v["summary"]
        .as_str()
        .or_else(|| v["details"].as_str().and_then(|d| d.lines().next()))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&id)
        .to_string();
    let aliases = v["aliases"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a.as_str().map(str::to_string))
        .collect();
    Ok(Vuln {
        summary,
        aliases,
        severity: v["database_specific"]["severity"]
            .as_str()
            .map(str::to_string),
        id,
        fixed,
    })
}

pub fn fixed_for(vuln: &Vuln, installed: &str) -> Option<String> {
    vuln.fixed
        .iter()
        .filter(|f| version_cmp(f, installed) == Ordering::Greater)
        .min_by(|a, b| version_cmp(a, b))
        .cloned()
}

/// The one version that fixes every advisory with a published fix: the
/// highest of their fixes. `None` when none has a fix.
pub fn fix_all(findings: &[Finding]) -> Option<String> {
    findings
        .iter()
        .filter_map(|f| f.fixed.as_deref())
        .max_by(|a, b| version_cmp(a, b))
        .map(str::to_string)
}

pub fn check(
    http: &dyn Http,
    packages: &[(String, String)],
    known: &mut HashMap<String, Vuln>,
) -> Result<BTreeMap<String, Vec<Finding>>, String> {
    if packages.is_empty() {
        return Ok(BTreeMap::new());
    }
    let queries: Vec<Value> = packages
        .iter()
        .map(|(name, version)| json!({"package": {"name": name, "ecosystem": "PyPI"}, "version": version}))
        .collect();
    let body = json!({ "queries": queries }).to_string();
    let ids = parse_batch(&http.post_json(BATCH_URL, &body)?, packages.len())?;
    let mut found = BTreeMap::new();
    for ((name, version), mut ids) in packages.iter().zip(ids) {
        let mut findings = Vec::new();
        // One issue is usually published as both a GHSA and a PYSEC record;
        // GHSA first, since it carries a summary and a severity, and every
        // record whose id is already covered is the same issue again.
        ids.sort_by_key(|id| !id.starts_with("GHSA-"));
        let mut covered = std::collections::HashSet::new();
        for id in ids {
            if !known.contains_key(&id) {
                let vuln = parse_vuln(&http.get(&format!("{VULN_URL}{id}"))?)?;
                known.insert(id.clone(), vuln);
            }
            let vuln = &known[&id];
            if covered.contains(&vuln.id) || vuln.aliases.iter().any(|a| covered.contains(a)) {
                continue;
            }
            covered.insert(vuln.id.clone());
            covered.extend(vuln.aliases.iter().cloned());
            findings.push(Finding {
                id: vuln.id.clone(),
                summary: vuln.summary.clone(),
                severity: vuln.severity.clone(),
                fixed: fixed_for(vuln, version),
            });
        }
        if !findings.is_empty() {
            found.insert(name.clone(), findings);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    const BATCH: &str = r#"{"results":[{"vulns":[{"id":"GHSA-1","modified":"x"},{"id":"GHSA-2","modified":"x"}]},{}]}"#;
    const VULN: &str = r#"{"id":"GHSA-1","summary":"Sandbox escape","database_specific":{"severity":"HIGH"},
        "affected":[{"package":{"ecosystem":"PyPI","name":"jinja2"},"ranges":[
          {"type":"ECOSYSTEM","events":[{"introduced":"0"},{"fixed":"2.10.1"}]},
          {"type":"ECOSYSTEM","events":[{"introduced":"3.0.0"},{"fixed":"3.1.4"}]}]},
          {"package":{"ecosystem":"npm","name":"x"},"ranges":[{"type":"SEMVER","events":[{"fixed":"9.9.9"}]}]}]}"#;

    #[test]
    fn a_batch_answer_is_ids_per_query() {
        assert_eq!(
            parse_batch(BATCH, 2).unwrap(),
            vec![vec!["GHSA-1".to_string(), "GHSA-2".into()], vec![]]
        );
        assert!(
            parse_batch(BATCH, 3).is_err(),
            "a short answer is not trusted"
        );
        assert!(parse_batch("nope", 1).is_err());
    }

    #[test]
    fn a_vuln_keeps_only_pypi_fixes() {
        let v = parse_vuln(VULN).unwrap();
        assert_eq!(v.summary, "Sandbox escape");
        assert_eq!(v.severity.as_deref(), Some("HIGH"));
        assert_eq!(v.fixed, ["2.10.1", "3.1.4"]);
    }

    #[test]
    fn fixed_is_the_lowest_fix_above_the_installed_version() {
        let v = parse_vuln(VULN).unwrap();
        assert_eq!(fixed_for(&v, "2.4.1").as_deref(), Some("2.10.1"));
        assert_eq!(fixed_for(&v, "3.0.2").as_deref(), Some("3.1.4"));
        assert_eq!(fixed_for(&v, "3.1.4"), None, "nothing above");
        let none = Vuln { fixed: vec![], ..v };
        assert_eq!(fixed_for(&none, "1.0"), None);
    }

    #[test]
    fn git_commits_are_not_versions_and_details_stand_in_for_a_summary() {
        let body = r#"{"id":"PYSEC-1","aliases":["CVE-1","GHSA-1"],"details":"Pillow before 7.1.0 has a bug.\nMore.",
            "affected":[{"package":{"ecosystem":"PyPI","name":"pillow"},"ranges":[
              {"type":"GIT","events":[{"introduced":"0"},{"fixed":"7abc123"}]},
              {"type":"ECOSYSTEM","events":[{"introduced":"0"},{"fixed":"7.1.0"}]}]}]}"#;
        let v = parse_vuln(body).unwrap();
        assert_eq!(v.fixed, ["7.1.0"]);
        assert_eq!(v.summary, "Pillow before 7.1.0 has a bug.");
        assert_eq!(v.aliases, ["CVE-1", "GHSA-1"]);
    }

    struct TwoRecords;
    impl Http for TwoRecords {
        fn post_json(&self, _: &str, _: &str) -> Result<String, String> {
            Ok(r#"{"results":[{"vulns":[{"id":"PYSEC-1"},{"id":"GHSA-1"}]}]}"#.into())
        }
        fn get(&self, url: &str) -> Result<String, String> {
            Ok(if url.ends_with("PYSEC-1") {
                r#"{"id":"PYSEC-1","aliases":["CVE-1","GHSA-1"],"affected":[]}"#.into()
            } else {
                r#"{"id":"GHSA-1","aliases":["CVE-1","PYSEC-1"],"summary":"Escape","database_specific":{"severity":"HIGH"},"affected":[]}"#.into()
            })
        }
    }

    #[test]
    fn one_issue_published_twice_is_counted_once() {
        let found = check(
            &TwoRecords,
            &[("jinja2".into(), "2.4.1".into())],
            &mut HashMap::new(),
        )
        .unwrap();
        assert_eq!(found["jinja2"].len(), 1, "{:?}", found["jinja2"]);
        assert_eq!(found["jinja2"][0].id, "GHSA-1", "the record with a summary");
    }

    #[test]
    fn one_upgrade_fixes_every_fixable_advisory() {
        let f = |fixed: Option<&str>| Finding {
            id: "x".into(),
            summary: String::new(),
            severity: None,
            fixed: fixed.map(str::to_string),
        };
        assert_eq!(
            fix_all(&[
                f(Some("3.1.3")),
                f(Some("3.1.6")),
                f(None),
                f(Some("2.10.1"))
            ])
            .as_deref(),
            Some("3.1.6")
        );
        assert_eq!(fix_all(&[f(None)]), None);
    }

    #[test]
    fn versions_compare_by_release_segments() {
        use std::cmp::Ordering::*;
        assert_eq!(version_cmp("2.10.1", "2.9"), Greater);
        assert_eq!(version_cmp("1.0", "1.0.0"), Equal);
        assert_eq!(version_cmp("1.2", "1.10"), Less);
    }

    struct FakeHttp {
        posts: Mutex<Vec<String>>,
        gets: Mutex<Vec<String>>,
        fail: bool,
    }

    impl Http for FakeHttp {
        fn post_json(&self, _url: &str, body: &str) -> Result<String, String> {
            self.posts.lock().unwrap().push(body.to_string());
            if self.fail {
                return Err("offline".into());
            }
            Ok(r#"{"results":[{"vulns":[{"id":"GHSA-1"}]},{}]}"#.into())
        }
        fn get(&self, url: &str) -> Result<String, String> {
            self.gets.lock().unwrap().push(url.to_string());
            Ok(VULN.into())
        }
    }

    fn fake(fail: bool) -> FakeHttp {
        FakeHttp {
            posts: Mutex::new(vec![]),
            gets: Mutex::new(vec![]),
            fail,
        }
    }

    #[test]
    fn check_reports_vulnerable_packages_and_caches_details() {
        let http = fake(false);
        let packages = vec![
            ("jinja2".to_string(), "2.4.1".to_string()),
            ("six".into(), "1.16.0".into()),
        ];
        let mut known = HashMap::new();
        let found = check(&http, &packages, &mut known).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found["jinja2"][0].fixed.as_deref(), Some("2.10.1"));
        assert!(http.posts.lock().unwrap()[0].contains(r#""name":"jinja2""#));
        assert!(http.posts.lock().unwrap()[0].contains(r#""ecosystem":"PyPI""#));
        let _ = check(&http, &packages, &mut known).unwrap();
        assert_eq!(
            http.gets.lock().unwrap().len(),
            1,
            "a known id is not fetched again"
        );
    }

    #[test]
    fn check_fails_cleanly_offline() {
        assert!(
            check(
                &fake(true),
                &[("a".into(), "1".into())],
                &mut HashMap::new()
            )
            .is_err()
        );
    }

    #[test]
    fn nothing_installed_sends_nothing() {
        let http = fake(false);
        assert!(check(&http, &[], &mut HashMap::new()).unwrap().is_empty());
        assert!(http.posts.lock().unwrap().is_empty());
    }
}
