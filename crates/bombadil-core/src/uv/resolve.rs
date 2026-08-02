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
///
/// `locate` resolves a bare program name (e.g. `"uv"`) to an absolute path,
/// the way a shell's PATH search would. `Auto` and `FromPath` call it once
/// and probe *that* absolute path, so the resolved [`ResolvedUv::path`] is
/// always verifiable and always the exact binary that will later be run —
/// never the literal string `"uv"`, which a venv's `bin`/`Scripts` prepended
/// to `PATH` could otherwise cause to resolve to a different binary at spawn
/// time than the one just probed. `Bundled` and `Custom` never call it:
/// `Custom` already has an explicit path.
pub fn resolve(
    source: &UvSource,
    runner: &dyn CommandRunner,
    locate: &dyn Fn(&str) -> Option<PathBuf>,
    bundled: &dyn Fn() -> Result<PathBuf, String>,
) -> Result<ResolvedUv, UvResolveError> {
    match source {
        UvSource::Bundled => use_bundled(bundled),

        UvSource::Auto => match locate("uv") {
            None => use_bundled(bundled),
            Some(on_path) => match probe(&on_path, runner) {
                Some(version) if version >= MIN_UV_VERSION => {
                    Ok(resolved(on_path, version, UvOrigin::Path))
                }
                _ => use_bundled(bundled),
            },
        },

        UvSource::FromPath => match locate("uv") {
            None => Err(UvResolveError::PathUvUnavailable),
            Some(on_path) => match probe(&on_path, runner) {
                Some(version) if version >= MIN_UV_VERSION => {
                    Ok(resolved(on_path, version, UvOrigin::Path))
                }
                Some(version) => Err(UvResolveError::TooOld {
                    path: on_path,
                    found: version.to_string(),
                    minimum: MIN_UV_VERSION.to_string(),
                }),
                None => Err(UvResolveError::PathUvUnavailable),
            },
        },

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

/// Find an executable on PATH, the way a shell would.
///
/// Kept separate from `resolve` so resolution stays testable without
/// depending on what happens to be installed on the machine running the tests.
pub fn locate_on_path(program: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var).find_map(|dir| candidate(&dir, program))
}

#[cfg(unix)]
fn candidate(dir: &Path, program: &str) -> Option<PathBuf> {
    let path = dir.join(program);
    path.is_file().then_some(path)
}

#[cfg(windows)]
fn candidate(dir: &Path, program: &str) -> Option<PathBuf> {
    // `uv` on Windows is `uv.exe`: a bare-name probe would find nothing, so
    // try every extension PATHEXT lists, the way cmd.exe resolves a command.
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    pathext.split(';').find_map(|ext| {
        let path = dir.join(format!("{program}{ext}"));
        path.is_file().then_some(path)
    })
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

    type Locator = Box<dyn Fn(&str) -> Option<PathBuf>>;

    fn bundled_ok() -> Box<dyn Fn() -> Result<PathBuf, String>> {
        Box::new(|| Ok(PathBuf::from("/data/uv/0.12.1-abc/uv")))
    }

    /// A locator that always finds uv at a fixed absolute path — distinct
    /// from the bare string `"uv"`, so tests can tell the two apart.
    fn locate_found() -> Locator {
        Box::new(|_| Some(PathBuf::from("/usr/bin/uv")))
    }

    fn locate_missing() -> Locator {
        Box::new(|_| None)
    }

    /// A locator that fails the test if called at all, for branches that
    /// must never consult PATH.
    fn locate_must_not_be_called() -> Locator {
        Box::new(|program| panic!("locate must not be called, but was called with {program:?}"))
    }

    #[test]
    fn auto_prefers_a_path_uv_that_meets_the_minimum() {
        let runner = FakeCommandRunner::new();
        runner.expect(
            "uv",
            &["--version"],
            version_outcome("uv 0.12.1 (x86_64-unknown-linux-gnu)"),
        );

        let got = resolve(&UvSource::Auto, &runner, &*locate_found(), &*bundled_ok()).unwrap();

        assert_eq!(got.origin, UvOrigin::Path);
        assert_eq!(
            got.path,
            PathBuf::from("/usr/bin/uv"),
            "the resolved path must be the absolute path the locator returned, not the literal \"uv\""
        );
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

        let got = resolve(&UvSource::Auto, &runner, &*locate_found(), &*bundled_ok()).unwrap();

        assert_eq!(got.origin, UvOrigin::Bundled);
        assert_eq!(got.path, PathBuf::from("/data/uv/0.12.1-abc/uv"));
    }

    #[test]
    fn auto_falls_back_to_bundled_when_no_uv_is_on_path() {
        // The locator reports no uv on PATH, so Auto must fall back without
        // ever attempting to probe anything.
        let runner = FakeCommandRunner::new();

        let got = resolve(&UvSource::Auto, &runner, &*locate_missing(), &*bundled_ok()).unwrap();

        assert_eq!(got.origin, UvOrigin::Bundled);
        assert!(
            runner.calls().is_empty(),
            "Auto must not probe when locate finds nothing; got {:?}",
            runner.calls()
        );
    }

    #[test]
    fn auto_flags_a_path_uv_newer_than_the_bundled_one() {
        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.99.0 (x)"));

        let got = resolve(&UvSource::Auto, &runner, &*locate_found(), &*bundled_ok()).unwrap();

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

        let got = resolve(
            &UvSource::Bundled,
            &runner,
            &*locate_must_not_be_called(),
            &*bundled_ok(),
        )
        .unwrap();

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

        let err = resolve(
            &UvSource::FromPath,
            &runner,
            &*locate_missing(),
            &*bundled_ok(),
        )
        .unwrap_err();

        assert!(
            matches!(err, UvResolveError::PathUvUnavailable),
            "an explicit pin must never silently fall back to bundled; got {err:?}"
        );
    }

    #[test]
    fn from_path_fails_loudly_when_uv_is_too_old() {
        let runner = FakeCommandRunner::new();
        runner.expect("uv", &["--version"], version_outcome("uv 0.9.0 (x)"));

        let err = resolve(
            &UvSource::FromPath,
            &runner,
            &*locate_found(),
            &*bundled_ok(),
        )
        .unwrap_err();

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

        let got = resolve(
            &UvSource::FromPath,
            &runner,
            &*locate_found(),
            &*bundled_ok(),
        )
        .unwrap();

        assert_eq!(got.origin, UvOrigin::Path);
        assert_eq!(
            got.path,
            PathBuf::from("/usr/bin/uv"),
            "the resolved path must be the absolute path the locator returned, not the literal \"uv\""
        );
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
        let got = resolve(
            &source,
            &runner,
            &*locate_must_not_be_called(),
            &*bundled_ok(),
        )
        .unwrap();

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

        let err = resolve(
            &source,
            &runner,
            &*locate_must_not_be_called(),
            &*bundled_ok(),
        )
        .unwrap_err();

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
        let err = resolve(
            &source,
            &runner,
            &*locate_must_not_be_called(),
            &*bundled_ok(),
        )
        .unwrap_err();

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

        let err = resolve(
            &UvSource::Bundled,
            &runner,
            &*locate_must_not_be_called(),
            &*failing,
        )
        .unwrap_err();

        assert!(
            matches!(err, UvResolveError::BundledUnavailable { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn locate_on_path_returns_none_for_a_program_that_cannot_exist() {
        assert_eq!(locate_on_path("definitely-not-a-real-program-xyzzy"), None);
    }

    #[test]
    #[cfg(unix)]
    fn locate_on_path_finds_a_program_known_to_exist_on_this_platform() {
        // `sh` is POSIX-mandated and present on every unix this workspace
        // targets (Linux, macOS), so this is robust without being fragile.
        // No equivalent guarantee exists on Windows, so this test is
        // unix-only rather than attempting a fragile cross-platform pick.
        assert!(locate_on_path("sh").is_some());
    }
}
