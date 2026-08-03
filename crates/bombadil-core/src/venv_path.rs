use crate::error::{VenvDeleteError, VenvPathError};
use crate::model::{DefaultVenvLocation, Environment, Project, Settings, VenvLocation};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Characters of the project directory name kept as-is; anything else becomes '-'.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The folder name a project gets inside the central venv store:
/// `<sanitized directory name>-<first 6 hex of SHA-256 of the directory path>`.
///
/// The hash keeps two unrelated projects that happen to share a directory name
/// from colliding, while the prefix keeps the folder recognisable.
///
/// The hash is separator- and case-sensitive: it hashes the path's raw bytes,
/// not a normalized form, so `C:\proj` and `c:/proj` resolve to different
/// venvs even though some tools would treat them as the same location.
pub fn central_dir_name(project_dir: &Path) -> String {
    let stem = project_dir
        .file_name()
        .map(|n| sanitize(&n.to_string_lossy()))
        .unwrap_or_else(|| "project".to_string());

    // `as_encoded_bytes`, not `to_string_lossy().as_bytes()`: lossy
    // conversion maps invalid UTF-8 to U+FFFD, so two distinct non-UTF-8
    // paths could lossy-convert to the same string and collide here.
    let digest = Sha256::digest(project_dir.as_os_str().as_encoded_bytes());
    let hash: String = digest.iter().take(3).map(|b| format!("{b:02x}")).collect();

    format!("{stem}-{hash}")
}

/// Where this project's virtual environment lives.
///
/// Performs no filesystem access, so it is safe to call for projects whose
/// directory no longer exists.
pub fn resolve(
    project: &Project,
    environment: &Environment,
    settings: &Settings,
) -> Result<PathBuf, VenvPathError> {
    let project_dir = project
        .pyproject_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| VenvPathError::NoProjectDirectory {
            pyproject_path: project.pyproject_path.clone(),
        })?;

    Ok(match &environment.location {
        VenvLocation::Custom { path } => path.clone(),
        VenvLocation::Alongside => project_dir.join(".venv"),
        VenvLocation::Default => match &settings.default_venv_location {
            DefaultVenvLocation::Alongside => project_dir.join(".venv"),
            DefaultVenvLocation::Central { path } => path.join(central_dir_name(project_dir)),
        },
    })
}

/// The Python version a virtual environment reports about itself, read from
/// `pyvenv.cfg`.
///
/// Every venv uv creates has one, so its presence is the cheapest reliable
/// test that a directory really is a virtual environment rather than an
/// empty folder someone pointed us at.
///
/// `pyvenv.cfg` is `key = value` lines. uv writes `version_info`; the
/// stdlib `venv` module writes `version`. Both are accepted, `version`
/// preferred if a file somehow carries both. A missing file, a missing key,
/// or a malformed file all yield `None` -- this reads paths the caller
/// chose, which may not be venvs at all, so it must never panic.
pub fn probe(venv: &Path) -> Option<String> {
    let text = std::fs::read_to_string(venv.join("pyvenv.cfg")).ok()?;

    let mut version = None;
    let mut version_info = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "version" => version = Some(value.to_string()),
            "version_info" => version_info = Some(value.to_string()),
            _ => {}
        }
    }
    version.or(version_info)
}

/// Delete `path`, but only if it really is `project`'s virtual environment.
///
/// This is the only destructive filesystem operation in the crate, so it
/// refuses far more than it accepts. Four things must hold before anything is
/// removed, and each failure is a distinct [`VenvDeleteError`] so the caller
/// can say *why* rather than "could not delete":
///
/// 1. `path` is exactly where [`resolve`] puts this project's environment.
///    Equality, not a prefix test: a caller must not be able to hand in an
///    arbitrary path, and no caller has a reason to delete part of a venv.
/// 2. `path` is a real directory, checked with `symlink_metadata` so a
///    symlink is refused rather than followed. [`probe`] *does* follow
///    symlinks, so this check has to come first -- otherwise a link pointing
///    at someone else's environment would satisfy check 3.
/// 3. `path` holds a `pyvenv.cfg` [`probe`] can read a version out of. Same
///    test the rest of the crate uses to decide something is a virtual
///    environment at all: uv writes `version_info`, the stdlib writes
///    `version`, and a directory with neither is not an environment however
///    plausible its name.
/// 4. The removal itself succeeded.
///
/// Deliberately *not* handled: a `pyvenv.cfg` whose `home` points somewhere
/// unexpected, and a project configured with `VenvLocation::Custom` pointing
/// at a directory the user cares about -- if they wrote a `pyvenv.cfg` into
/// it, it is a virtual environment by every test uv itself applies.
pub fn delete(
    project: &Project,
    environment: &Environment,
    settings: &Settings,
    path: &Path,
) -> Result<(), VenvDeleteError> {
    let expected = resolve(project, environment, settings)?;
    if path != expected {
        return Err(VenvDeleteError::OutsideProject {
            path: path.to_path_buf(),
            expected,
        });
    }

    // `symlink_metadata`, never `metadata`: a symlink at the venv path must
    // be refused, not followed. A path that does not exist lands here too,
    // which is the honest answer -- there is no environment to delete.
    if !std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()) {
        return Err(VenvDeleteError::NotADirectory {
            path: path.to_path_buf(),
        });
    }

    if probe(path).is_none() {
        return Err(VenvDeleteError::NotAVenv {
            path: path.to_path_buf(),
        });
    }

    std::fs::remove_dir_all(path).map_err(|source| VenvDeleteError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::{central_dir_name, delete, probe, resolve};
    use crate::error::{VenvDeleteError, VenvPathError};
    use crate::model::{
        DefaultVenvLocation, Environment, Project, PythonPin, Settings, UvSource, VenvLocation,
    };
    use std::path::{Path, PathBuf};

    fn project(venv: VenvLocation) -> Project {
        Project {
            label: "my-api".into(),
            pyproject_path: PathBuf::from("/proj/my-api/pyproject.toml"),
            environments: vec![Environment {
                location: venv.clone(),
                ..Environment::default()
            }],
            active: venv,
            ..Project::default()
        }
    }

    /// `resolve` for a project's active environment -- the shape every test
    /// here used before environments became a list.
    fn resolve_active(project: &Project, settings: &Settings) -> Result<PathBuf, VenvPathError> {
        let environment = project.active_environment().cloned().unwrap_or_default();
        resolve(project, &environment, settings)
    }

    /// `delete` for a project's active environment, likewise.
    fn delete_active(
        project: &Project,
        settings: &Settings,
        path: &Path,
    ) -> Result<(), VenvDeleteError> {
        let environment = project.active_environment().cloned().unwrap_or_default();
        delete(project, &environment, settings, path)
    }

    fn settings(default_venv_location: DefaultVenvLocation) -> Settings {
        Settings {
            uv_source: UvSource::Auto,
            default_venv_location,
            python: PythonPin::default(),
        }
    }

    #[test]
    fn alongside_puts_the_venv_next_to_the_pyproject() {
        let got = resolve_active(
            &project(VenvLocation::Alongside),
            &settings(DefaultVenvLocation::Alongside),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/proj/my-api/.venv"));
    }

    #[test]
    fn a_custom_location_is_used_verbatim() {
        let got = resolve_active(
            &project(VenvLocation::Custom {
                path: PathBuf::from("/mnt/fast/envs/api"),
            }),
            &settings(DefaultVenvLocation::Central {
                path: PathBuf::from("/ignored"),
            }),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/mnt/fast/envs/api"));
    }

    #[test]
    fn default_follows_the_alongside_setting() {
        let got = resolve_active(
            &project(VenvLocation::Default),
            &settings(DefaultVenvLocation::Alongside),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/proj/my-api/.venv"));
    }

    #[test]
    fn default_follows_the_central_setting_and_names_by_directory_plus_hash() {
        let got = resolve_active(
            &project(VenvLocation::Default),
            &settings(DefaultVenvLocation::Central {
                path: PathBuf::from("/home/t/.venvs"),
            }),
        )
        .unwrap();

        let name = got.file_name().unwrap().to_string_lossy().into_owned();
        assert!(got.starts_with("/home/t/.venvs"), "got {got:?}");
        assert!(name.starts_with("my-api-"), "got {name}");
        assert_eq!(
            name.len(),
            "my-api-".len() + 6,
            "hash must be 6 chars: {name}"
        );
        assert!(
            name["my-api-".len()..]
                .chars()
                .all(|c| c.is_ascii_hexdigit()),
            "got {name}"
        );
    }

    #[test]
    fn two_projects_with_the_same_directory_name_do_not_collide() {
        let a = central_dir_name(Path::new("/work/alpha/api"));
        let b = central_dir_name(Path::new("/work/beta/api"));
        assert!(a.starts_with("api-") && b.starts_with("api-"));
        assert_ne!(
            a, b,
            "different projects named 'api' must get different folders"
        );
    }

    #[test]
    fn the_same_path_always_produces_the_same_name() {
        assert_eq!(
            central_dir_name(Path::new("/work/alpha/api")),
            central_dir_name(Path::new("/work/alpha/api")),
        );
    }

    #[test]
    fn awkward_directory_characters_are_replaced() {
        let name = central_dir_name(Path::new("/work/my project (v2)"));
        let stem = name.rsplit_once('-').unwrap().0;
        assert_eq!(stem, "my-project--v2-");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'),
            "got {name}"
        );
    }

    #[test]
    fn a_pyproject_path_with_no_parent_is_an_error() {
        let mut p = project(VenvLocation::Default);
        p.pyproject_path = PathBuf::from("pyproject.toml");
        assert!(resolve_active(&p, &settings(DefaultVenvLocation::Alongside)).is_err());
    }

    #[test]
    fn a_real_uv_venv_yields_its_version() {
        // Confirmed against a real `uv venv`: it writes `version_info`, not
        // `version` -- `pyvenv.cfg` looked like:
        //   home = /home/trevisani/miniforge3/bin
        //   implementation = CPython
        //   uv = 0.12.1
        //   version_info = 3.13.14
        //   include-system-site-packages = false
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyvenv.cfg"),
            "home = /usr/bin\nimplementation = CPython\nuv = 0.12.1\nversion_info = 3.13.14\ninclude-system-site-packages = false\n",
        )
        .unwrap();
        assert_eq!(probe(dir.path()).as_deref(), Some("3.13.14"));
    }

    #[test]
    fn a_stdlib_venv_yields_its_version() {
        // Confirmed against a real `python -m venv`: it writes `version`,
        // not `version_info`.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyvenv.cfg"),
            "home = /usr/bin\ninclude-system-site-packages = false\nversion = 3.13.14\nexecutable = /usr/bin/python3.13\n",
        )
        .unwrap();
        assert_eq!(probe(dir.path()).as_deref(), Some("3.13.14"));
    }

    #[test]
    fn version_is_preferred_over_version_info_when_both_are_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyvenv.cfg"),
            "version = 3.11.0\nversion_info = 3.13.14\n",
        )
        .unwrap();
        assert_eq!(probe(dir.path()).as_deref(), Some("3.11.0"));
    }

    #[test]
    fn a_missing_pyvenv_cfg_yields_none() {
        // The directory exists but was never a venv -- must not panic and
        // must not be mistaken for one.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(probe(dir.path()), None);
    }

    #[test]
    fn a_missing_venv_directory_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(probe(&dir.path().join("does-not-exist")), None);
    }

    #[test]
    fn a_pyvenv_cfg_with_no_version_key_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        assert_eq!(probe(dir.path()), None);
    }

    #[test]
    fn a_malformed_pyvenv_cfg_yields_none_rather_than_panicking() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyvenv.cfg"),
            "not a key value file at all\n\0\0\0",
        )
        .unwrap();
        assert_eq!(probe(dir.path()), None);
    }

    #[test]
    fn a_real_uv_venv_created_on_disk_probes_correctly_then_none_once_the_file_is_gone() {
        // The mutation this test earns its place against: a probe that
        // returns Some unconditionally, or that trusts the directory's mere
        // existence instead of reading pyvenv.cfg.
        let dir = tempfile::tempdir().unwrap();
        let venv = dir.path().join(".venv");
        std::fs::create_dir_all(&venv).unwrap();
        std::fs::write(
            venv.join("pyvenv.cfg"),
            "home = /usr/bin\nversion_info = 3.12.7\n",
        )
        .unwrap();
        assert_eq!(probe(&venv).as_deref(), Some("3.12.7"));

        std::fs::remove_file(venv.join("pyvenv.cfg")).unwrap();
        assert_eq!(probe(&venv), None);
    }

    // --- delete: the crate's only destructive operation ---
    //
    // Every test below asserts BOTH the returned outcome and what is still on
    // disk afterwards. A refusal that returns `Err` after having already
    // removed the directory would pass an outcome-only assertion, which on
    // this function is the exact failure that costs a user their environment.

    /// A project whose venv resolves to `<dir>/proj/.venv`, so a refusal can
    /// be tested against a path inside the same tempdir.
    fn project_in(dir: &Path) -> Project {
        Project {
            pyproject_path: dir.join("proj").join("pyproject.toml"),
            ..project(VenvLocation::Alongside)
        }
    }

    /// A directory shaped like a real venv: `pyvenv.cfg` plus something
    /// underneath it, so a delete that only unlinked the marker file would
    /// leave evidence.
    fn make_venv(path: &Path) {
        std::fs::create_dir_all(path.join("lib")).unwrap();
        std::fs::write(
            path.join("pyvenv.cfg"),
            "home = /usr/bin\nversion_info = 3.13.1\n",
        )
        .unwrap();
        std::fs::write(path.join("lib").join("marker"), "x").unwrap();
    }

    #[test]
    fn a_real_venv_at_the_projects_resolved_location_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let project = project_in(dir.path());
        let settings = settings(DefaultVenvLocation::Alongside);
        let venv = resolve_active(&project, &settings).unwrap();
        make_venv(&venv);

        delete_active(&project, &settings, &venv).expect("a real venv must be removable");

        assert!(!venv.exists(), "{venv:?} must be gone");
        assert!(
            dir.path().join("proj").exists(),
            "only the venv may be removed, not the project directory around it"
        );
    }

    #[test]
    fn a_directory_without_a_pyvenv_cfg_is_refused_and_survives() {
        // The refusal that matters most: a user who set the venv location to
        // their source folder, or to a directory that merely looks like an
        // environment. A plausible name is not evidence.
        let dir = tempfile::tempdir().unwrap();
        let project = project_in(dir.path());
        let settings = settings(DefaultVenvLocation::Alongside);
        let venv = resolve_active(&project, &settings).unwrap();
        std::fs::create_dir_all(venv.join("src")).unwrap();
        std::fs::write(venv.join("src").join("main.py"), "print(1)").unwrap();

        let err = delete_active(&project, &settings, &venv).unwrap_err();

        assert!(
            matches!(err, VenvDeleteError::NotAVenv { .. }),
            "got {err:?}"
        );
        assert!(
            venv.join("src").join("main.py").exists(),
            "nothing may be removed when the path is not a virtual environment"
        );
    }

    #[test]
    fn a_path_outside_the_projects_resolved_location_is_refused_and_survives() {
        // Deliberately a REAL venv: only the location check can refuse it, so
        // this cannot pass off the pyvenv.cfg check by accident.
        let dir = tempfile::tempdir().unwrap();
        let project = project_in(dir.path());
        let settings = settings(DefaultVenvLocation::Alongside);
        let elsewhere = dir.path().join("someone-elses-venv");
        make_venv(&elsewhere);

        let err = delete_active(&project, &settings, &elsewhere).unwrap_err();

        assert!(
            matches!(err, VenvDeleteError::OutsideProject { .. }),
            "got {err:?}"
        );
        assert!(
            elsewhere.join("pyvenv.cfg").exists(),
            "a path this project does not own must not be deleted"
        );
    }

    #[test]
    fn a_path_that_does_not_exist_is_refused_rather_than_reported_as_deleted() {
        // Recreate-venv on a project that never had one arrives here. It must
        // read as "there is no environment", not as a successful delete.
        let dir = tempfile::tempdir().unwrap();
        let project = project_in(dir.path());
        let settings = settings(DefaultVenvLocation::Alongside);
        let venv = resolve_active(&project, &settings).unwrap();

        let err = delete_active(&project, &settings, &venv).unwrap_err();

        assert!(
            matches!(err, VenvDeleteError::NotADirectory { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn a_venv_path_that_is_a_file_is_refused_and_survives() {
        let dir = tempfile::tempdir().unwrap();
        let project = project_in(dir.path());
        let settings = settings(DefaultVenvLocation::Alongside);
        let venv = resolve_active(&project, &settings).unwrap();
        std::fs::create_dir_all(venv.parent().unwrap()).unwrap();
        std::fs::write(&venv, "not a directory").unwrap();

        let err = delete_active(&project, &settings, &venv).unwrap_err();

        assert!(
            matches!(err, VenvDeleteError::NotADirectory { .. }),
            "got {err:?}"
        );
        assert!(venv.exists(), "the file must survive");
    }

    #[cfg(unix)]
    #[test]
    fn a_venv_path_that_is_a_symlink_is_refused_and_its_target_survives() {
        // `probe` follows symlinks, so a symlink pointing at a real venv --
        // or at a home directory containing one -- passes the pyvenv.cfg test
        // while naming something the project does not own. The check has to
        // be on the link itself, which means `symlink_metadata`, not
        // `metadata`.
        //
        // unix-only: creating a symlink on Windows needs a privilege CI does
        // not have. The guard itself is not platform-specific.
        let dir = tempfile::tempdir().unwrap();
        let project = project_in(dir.path());
        let settings = settings(DefaultVenvLocation::Alongside);
        let venv = resolve_active(&project, &settings).unwrap();
        let target = dir.path().join("real-venv");
        make_venv(&target);
        std::fs::create_dir_all(venv.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &venv).unwrap();
        assert!(probe(&venv).is_some(), "the symlink must probe as a venv");

        let err = delete_active(&project, &settings, &venv).unwrap_err();

        assert!(
            matches!(err, VenvDeleteError::NotADirectory { .. }),
            "got {err:?}"
        );
        assert!(
            target.join("pyvenv.cfg").exists(),
            "the symlink's target must not be touched"
        );
        assert!(
            std::fs::symlink_metadata(&venv).is_ok(),
            "the symlink itself must not be removed either"
        );
    }

    #[test]
    fn a_project_with_no_directory_is_refused_before_anything_is_touched() {
        let mut project = project(VenvLocation::Alongside);
        project.pyproject_path = PathBuf::from("pyproject.toml");
        let err = delete_active(
            &project,
            &settings(DefaultVenvLocation::Alongside),
            Path::new(".venv"),
        )
        .unwrap_err();
        assert!(matches!(err, VenvDeleteError::Resolve(_)), "got {err:?}");
    }

    #[test]
    fn every_refusal_names_the_path_it_refused() {
        // The whole reason these are distinct variants: the GUI hands the
        // rendered message straight to the drawer, and "could not delete"
        // with no path is not something a user can act on.
        let dir = tempfile::tempdir().unwrap();
        let project = project_in(dir.path());
        let settings = settings(DefaultVenvLocation::Alongside);
        let venv = resolve_active(&project, &settings).unwrap();

        std::fs::create_dir_all(&venv).unwrap();
        let not_a_venv = delete_active(&project, &settings, &venv)
            .unwrap_err()
            .to_string();
        assert!(
            not_a_venv.contains(&venv.display().to_string()),
            "got {not_a_venv}"
        );

        let elsewhere = dir.path().join("elsewhere");
        let outside = delete_active(&project, &settings, &elsewhere)
            .unwrap_err()
            .to_string();
        assert!(
            outside.contains(&elsewhere.display().to_string()),
            "got {outside}"
        );
        assert!(
            outside.contains(&venv.display().to_string()),
            "the refusal must also name where the venv actually is; got {outside}"
        );
    }
}
