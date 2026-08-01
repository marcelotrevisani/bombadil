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

#[cfg(test)]
mod tests {
    use super::{central_dir_name, resolve};
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
}
