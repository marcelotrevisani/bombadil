use crate::error::VenvPathError;
use crate::model::{DefaultVenvLocation, Project, Settings, VenvLocation};
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
pub fn resolve(project: &Project, settings: &Settings) -> Result<PathBuf, VenvPathError> {
    let project_dir = project
        .pyproject_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| VenvPathError::NoProjectDirectory {
            pyproject_path: project.pyproject_path.clone(),
        })?;

    Ok(match &project.venv {
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

#[cfg(test)]
mod tests {
    use super::{central_dir_name, probe, resolve};
    use crate::model::{DefaultVenvLocation, Project, Settings, UvSource, VenvLocation};
    use std::path::{Path, PathBuf};
    use uuid::Uuid;

    fn project(venv: VenvLocation) -> Project {
        Project {
            id: Uuid::nil(),
            label: "my-api".into(),
            pyproject_path: PathBuf::from("/proj/my-api/pyproject.toml"),
            index_names: vec![],
            venv,
            env: vec![],
            scripts: vec![],
            members: Default::default(),
        }
    }

    fn settings(default_venv_location: DefaultVenvLocation) -> Settings {
        Settings {
            uv_source: UvSource::Auto,
            default_venv_location,
        }
    }

    #[test]
    fn alongside_puts_the_venv_next_to_the_pyproject() {
        let got = resolve(
            &project(VenvLocation::Alongside),
            &settings(DefaultVenvLocation::Alongside),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/proj/my-api/.venv"));
    }

    #[test]
    fn a_custom_location_is_used_verbatim() {
        let got = resolve(
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
        let got = resolve(
            &project(VenvLocation::Default),
            &settings(DefaultVenvLocation::Alongside),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/proj/my-api/.venv"));
    }

    #[test]
    fn default_follows_the_central_setting_and_names_by_directory_plus_hash() {
        let got = resolve(
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
        assert!(resolve(&p, &settings(DefaultVenvLocation::Alongside)).is_err());
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
}
