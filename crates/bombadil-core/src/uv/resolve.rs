use super::{BUNDLED_UV_VERSION, MIN_UV_VERSION, UvVersion};
use crate::error::UvResolveError;
use crate::model::UvSource;
use crate::process::{CommandRunner, CommandSpec};
use std::path::{Path, PathBuf};

/// Where the uv we are about to run came from. Surfaced in the UI and in
/// failure reports, so version skew is visible rather than mysterious.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UvOrigin {
    Bundled,
    Path,
    Configured,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedUv {
    pub path: PathBuf,
    pub version: UvVersion,
    pub origin: UvOrigin,
    /// True when this uv is newer than the one we ship and therefore newer
    /// than anything this code has been tested against.
    pub newer_than_bundled: bool,
}

/// Ask a uv binary for its version. `None` when it cannot be run or its output
/// cannot be parsed — both mean "unusable", and the caller decides whether
/// that is fatal.
fn probe(path: &Path, runner: &dyn CommandRunner) -> Option<UvVersion> {
    let spec = CommandSpec::new(path).arg("--version");
    let outcome = runner.run(&spec).ok()?;
    if !outcome.success() {
        return None;
    }
    UvVersion::parse(&outcome.stdout)
}

fn resolved(path: PathBuf, version: UvVersion, origin: UvOrigin) -> ResolvedUv {
    ResolvedUv {
        path,
        version,
        origin,
        newer_than_bundled: version > BUNDLED_UV_VERSION,
    }
}

fn use_bundled(
    bundled: &dyn Fn() -> Result<PathBuf, String>,
) -> Result<ResolvedUv, UvResolveError> {
    let path = bundled().map_err(|message| UvResolveError::BundledUnavailable { message })?;
    Ok(resolved(path, BUNDLED_UV_VERSION, UvOrigin::Bundled))
}

/// Decide which uv binary to run.
///
/// `Auto` prefers a new-enough uv from PATH and otherwise falls back to the
/// bundled copy. `FromPath` and `Custom` are explicit pins: they fail loudly
/// rather than falling back, because silently ignoring a user's pin hides the
/// very problem they need to see.
pub fn resolve(
    source: &UvSource,
    runner: &dyn CommandRunner,
    bundled: &dyn Fn() -> Result<PathBuf, String>,
) -> Result<ResolvedUv, UvResolveError> {
    match source {
        UvSource::Bundled => use_bundled(bundled),

        UvSource::Auto => {
            let on_path = PathBuf::from("uv");
            match probe(&on_path, runner) {
                Some(version) if version >= MIN_UV_VERSION => {
                    Ok(resolved(on_path, version, UvOrigin::Path))
                }
                _ => use_bundled(bundled),
            }
        }

        UvSource::FromPath => {
            let on_path = PathBuf::from("uv");
            match probe(&on_path, runner) {
                Some(version) if version >= MIN_UV_VERSION => {
                    Ok(resolved(on_path, version, UvOrigin::Path))
                }
                Some(version) => Err(UvResolveError::TooOld {
                    path: on_path,
                    found: version.to_string(),
                    minimum: MIN_UV_VERSION.to_string(),
                }),
                None => Err(UvResolveError::PathUvUnavailable),
            }
        }

        UvSource::Custom { path } => match probe(path, runner) {
            Some(version) if version >= MIN_UV_VERSION => {
                Ok(resolved(path.clone(), version, UvOrigin::Configured))
            }
            Some(version) => Err(UvResolveError::TooOld {
                path: path.clone(),
                found: version.to_string(),
                minimum: MIN_UV_VERSION.to_string(),
            }),
            None => Err(UvResolveError::ConfiguredUnavailable { path: path.clone() }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{CommandOutcome, FakeCommandRunner};
    use std::path::PathBuf;

    fn version_outcome(line: &str) -> CommandOutcome {
        CommandOutcome {
            status: Some(0),
            stdout: line.into(),
            stderr: String::new(),
        }
    }

    fn bundled_ok() -> Box<dyn Fn() -> Result<PathBuf, String>> {
        Box::new(|| Ok(PathBuf::from("/data/uv/0.12.1-abc/uv")))
    }

    #[test]
    fn auto_prefers_a_path_uv_that_meets_the_minimum() {
        let runner = FakeCommandRunner::new();
        runner.expect(
            "uv",
            &["--version"],
            version_outcome("uv 0.12.1 (x86_64-unknown-linux-gnu)"),
        );

        let got = resolve(&UvSource::Auto, &runner, &*bundled_ok()).unwrap();

        assert_eq!(got.origin, UvOrigin::Path);
        assert_eq!(
            got.version,
            UvVersion {
                major: 0,
                minor: 12,
                patch: 1
            }
        );
        assert!(!got.newer_than_bundled);
    }

    #[test]
    fn auto_falls_back_to_bundled_when_path_uv_is_too_old() {
        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.9.0 (x)"));

        let got = resolve(&UvSource::Auto, &runner, &*bundled_ok()).unwrap();

        assert_eq!(got.origin, UvOrigin::Bundled);
        assert_eq!(got.path, PathBuf::from("/data/uv/0.12.1-abc/uv"));
    }

    #[test]
    fn auto_falls_back_to_bundled_when_no_uv_is_on_path() {
        // The fake errors on unprogrammed calls, which is exactly what a
        // missing binary looks like.
        let runner = FakeCommandRunner::new();

        let got = resolve(&UvSource::Auto, &runner, &*bundled_ok()).unwrap();

        assert_eq!(got.origin, UvOrigin::Bundled);
    }

    #[test]
    fn auto_flags_a_path_uv_newer_than_the_bundled_one() {
        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.99.0 (x)"));

        let got = resolve(&UvSource::Auto, &runner, &*bundled_ok()).unwrap();

        assert_eq!(got.origin, UvOrigin::Path);
        assert!(
            got.newer_than_bundled,
            "a uv newer than the one we test against must be visibly flagged"
        );
    }

    #[test]
    fn bundled_never_consults_path() {
        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.99.0 (x)"));

        let got = resolve(&UvSource::Bundled, &runner, &*bundled_ok()).unwrap();

        assert_eq!(got.origin, UvOrigin::Bundled);
        assert_eq!(got.version, BUNDLED_UV_VERSION);
        assert!(
            runner.calls().is_empty(),
            "Bundled must never probe PATH; got {:?}",
            runner.calls()
        );
    }

    #[test]
    fn from_path_fails_loudly_when_uv_is_missing() {
        let runner = FakeCommandRunner::new();

        let err = resolve(&UvSource::FromPath, &runner, &*bundled_ok()).unwrap_err();

        assert!(
            matches!(err, UvResolveError::PathUvUnavailable),
            "an explicit pin must never silently fall back to bundled; got {err:?}"
        );
    }

    #[test]
    fn from_path_fails_loudly_when_uv_is_too_old() {
        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.9.0 (x)"));

        let err = resolve(&UvSource::FromPath, &runner, &*bundled_ok()).unwrap_err();

        assert!(matches!(err, UvResolveError::TooOld { .. }), "got {err:?}");
    }

    #[test]
    fn from_path_uses_a_uv_that_meets_the_minimum() {
        // Without this, swapping the two Some(version) arms in the FromPath
        // branch would leave the whole suite green.
        let runner = FakeCommandRunner::new();
        runner.expect(
            "uv",
            &["--version"],
            version_outcome("uv 0.12.1 (x86_64-unknown-linux-gnu)"),
        );

        let got = resolve(&UvSource::FromPath, &runner, &*bundled_ok()).unwrap();

        assert_eq!(got.origin, UvOrigin::Path);
        assert_eq!(
            got.version,
            UvVersion {
                major: 0,
                minor: 12,
                patch: 1
            }
        );
        assert!(!got.newer_than_bundled);
    }

    #[test]
    fn a_configured_binary_is_used_and_reports_its_own_version() {
        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.13.0 (x)"));

        let source = UvSource::Custom {
            path: PathBuf::from("/opt/uv/bin/uv"),
        };
        let got = resolve(&source, &runner, &*bundled_ok()).unwrap();

        assert_eq!(got.origin, UvOrigin::Configured);
        assert_eq!(got.path, PathBuf::from("/opt/uv/bin/uv"));
        assert_eq!(
            got.version,
            UvVersion {
                major: 0,
                minor: 13,
                patch: 0
            }
        );
    }

    #[test]
    fn a_configured_binary_that_is_missing_fails_loudly() {
        let runner = FakeCommandRunner::new();
        let source = UvSource::Custom {
            path: PathBuf::from("/opt/uv/bin/uv"),
        };

        let err = resolve(&source, &runner, &*bundled_ok()).unwrap_err();

        assert!(
            matches!(err, UvResolveError::ConfiguredUnavailable { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn a_configured_binary_that_is_too_old_fails_loudly() {
        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.9.0 (x)"));

        let source = UvSource::Custom {
            path: PathBuf::from("/opt/uv/bin/uv"),
        };
        let err = resolve(&source, &runner, &*bundled_ok()).unwrap_err();

        match err {
            UvResolveError::TooOld { path, found, .. } => {
                assert_eq!(path, PathBuf::from("/opt/uv/bin/uv"));
                assert_eq!(found, "0.9.0");
            }
            other => panic!("expected TooOld, got {other:?}"),
        }
    }

    #[test]
    fn a_failed_bundled_extraction_is_reported() {
        let runner = FakeCommandRunner::new();
        let failing: Box<dyn Fn() -> Result<PathBuf, String>> =
            Box::new(|| Err("disk full".to_string()));

        let err = resolve(&UvSource::Bundled, &runner, &*failing).unwrap_err();

        assert!(
            matches!(err, UvResolveError::BundledUnavailable { .. }),
            "got {err:?}"
        );
    }
}
