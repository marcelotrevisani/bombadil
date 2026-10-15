//! A project's git branch and working-tree status, for display only.
//!
//! One `git status --porcelain=v2 --branch` answers everything shown: the
//! branch, its upstream, ahead/behind and the change counts. The porcelain
//! format is git's stable, machine-readable one, so this parses that rather
//! than linking a git library. `--no-optional-locks` keeps a background probe
//! from taking `index.lock` out from under a `git commit` the user is running.

use crate::process::{CommandRunner, CommandSpec};
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitStatus {
    /// The branch, or the short commit hash when `detached`.
    pub branch: String,
    pub detached: bool,
    /// e.g. `origin/main`.
    pub upstream: Option<String>,
    /// Commits ahead of / behind the upstream, as of the last fetch.
    pub ahead: usize,
    pub behind: usize,
    pub staged: usize,
    pub modified: usize,
    pub untracked: usize,
    pub conflicted: usize,
}

impl GitStatus {
    pub fn is_clean(&self) -> bool {
        self.staged + self.modified + self.untracked + self.conflicted == 0
    }
}

/// The status of the repository containing `dir`, or `None` when it is not in
/// one, git is not installed, or git failed -- all of which the UI shows the
/// same way, by showing nothing.
pub fn status(dir: &Path, runner: &dyn CommandRunner) -> Option<GitStatus> {
    let spec = CommandSpec::new("git")
        // A repository's own config can name an fsmonitor program, which
        // `git status` would run; the repository is not trusted.
        .args([
            "-c",
            "core.fsmonitor=false",
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "--branch",
        ])
        .cwd(dir);
    let outcome = runner.run(&spec).ok()?;
    outcome.success().then(|| parse(&outcome.stdout))
}

/// Parses `git status --porcelain=v2 --branch` output.
pub fn parse(output: &str) -> GitStatus {
    let mut status = GitStatus::default();
    let mut oid = String::new();
    for line in output.lines() {
        if let Some(header) = line.strip_prefix("# branch.") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "oid" => oid = value.to_string(),
                "head" => status.branch = value.to_string(),
                "upstream" => status.upstream = Some(value.to_string()),
                "ab" => {
                    for part in value.split_whitespace() {
                        if let Some(n) = part.strip_prefix('+') {
                            status.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            status.behind = n.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let mut fields = line.splitn(3, ' ');
        match (fields.next(), fields.next()) {
            (Some("?"), _) => status.untracked += 1,
            (Some("u"), _) => status.conflicted += 1,
            (Some("1" | "2"), Some(xy)) => {
                let mut xy = xy.chars();
                if xy.next().is_some_and(|x| x != '.') {
                    status.staged += 1;
                }
                if xy.next().is_some_and(|y| y != '.') {
                    status.modified += 1;
                }
            }
            _ => {}
        }
    }
    if status.branch == "(detached)" {
        status.detached = true;
        status.branch = oid.chars().take(8).collect();
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{CommandOutcome, FakeCommandRunner};

    #[test]
    fn branch_upstream_divergence_and_every_kind_of_change() {
        let out = "\
# branch.oid 1234567890abcdef
# branch.head feat/x
# branch.upstream origin/feat/x
# branch.ab +2 -3
1 M. N... 100644 100644 100644 a b staged.py
1 .M N... 100644 100644 100644 a b modified.py
1 MM N... 100644 100644 100644 a b both.py
2 R. N... 100644 100644 100644 a b R100 new.py\told.py
u UU N... 100644 100644 100644 100644 a b c conflict.py
? new.txt
";
        assert_eq!(
            parse(out),
            GitStatus {
                branch: "feat/x".into(),
                detached: false,
                upstream: Some("origin/feat/x".into()),
                ahead: 2,
                behind: 3,
                staged: 3,
                modified: 2,
                untracked: 1,
                conflicted: 1,
            }
        );
    }

    #[test]
    fn a_detached_head_shows_the_short_hash() {
        let s = parse("# branch.oid 1234567890abcdef\n# branch.head (detached)\n");
        assert!(s.detached);
        assert_eq!(s.branch, "12345678");
        assert!(s.is_clean());
        assert_eq!(s.upstream, None);
    }

    #[test]
    fn a_fresh_repo_names_its_unborn_branch() {
        let s = parse("# branch.oid (initial)\n# branch.head main\n");
        assert_eq!(s.branch, "main");
        assert!(!s.detached);
    }

    #[test]
    fn outside_a_repository_there_is_no_status() {
        let runner = FakeCommandRunner::new();
        runner.expect(
            "git",
            &[
                "-c",
                "core.fsmonitor=false",
                "--no-optional-locks",
                "status",
                "--porcelain=v2",
                "--branch",
            ],
            CommandOutcome {
                status: Some(128),
                stdout: String::new(),
                stderr: "fatal: not a git repository".into(),
            },
        );
        assert_eq!(status(Path::new("/tmp"), &runner), None);
        // And with no git at all, the probe fails rather than the app.
        assert_eq!(status(Path::new("/tmp"), &FakeCommandRunner::new()), None);
    }

    #[test]
    fn a_repositorys_fsmonitor_command_is_never_run() {
        // `core.fsmonitor` in a repo's own config names a program `git
        // status` runs; Bombadil runs status on every project it lists.
        let runner = FakeCommandRunner::new();
        runner.expect(
            "git",
            &[
                "-c",
                "core.fsmonitor=false",
                "--no-optional-locks",
                "status",
                "--porcelain=v2",
                "--branch",
            ],
            CommandOutcome {
                status: Some(0),
                stdout: "# branch.head main\n".into(),
                stderr: String::new(),
            },
        );
        assert_eq!(
            status(Path::new("/tmp"), &runner).map(|s| s.branch),
            Some("main".to_string())
        );
    }
}
