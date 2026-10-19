//! Where an imported project's folders are on this computer.

use crate::model::{Config, DefaultVenvLocation, VenvLocation};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderMap {
    /// As written on the exporting machine, which may use the other OS's
    /// separator.
    pub from: String,
    pub to: PathBuf,
}

/// Path components split on both separators, so a Windows path reads the
/// same on Unix and the reverse.
fn parts(path: &str) -> Vec<&str> {
    path.split(['/', '\\']).filter(|s| !s.is_empty()).collect()
}

/// The text before the first component: "/" for an absolute Unix path,
/// "" for `C:\...` (its drive is the first component).
fn root(path: &str) -> &str {
    if path.starts_with('/') { "/" } else { "" }
}

/// A drive letter (`C:`) first means a Windows path, joined with `\\`.
fn join(root: &str, parts: &[&str]) -> String {
    let sep = if parts.first().is_some_and(|p| p.ends_with(':')) {
        "\\"
    } else {
        "/"
    };
    format!("{root}{}", parts.join(sep))
}

/// Every folder an imported path lives in: a project's parent folder, a
/// custom venv's parent, a script's folder, the central venv folder. A folder
/// inside another listed one is dropped, so one row covers both.
pub fn suggest(config: &Config, old_home: &Path, new_home: &Path) -> Vec<FolderMap> {
    let mut folders: Vec<String> = Vec::new();
    let mut add = |path: &Path, up: usize| {
        let text = path.to_string_lossy();
        let p = parts(&text);
        if p.len() > up {
            folders.push(join(root(&text), &p[..p.len() - up]));
        }
    };
    for project in &config.projects {
        add(&project.pyproject_path, 2);
        for env in &project.environments {
            if let VenvLocation::Custom { path } = &env.location {
                add(path, 1);
            }
        }
        for script in &project.scripts {
            add(&script.path, 1);
        }
    }
    for script in &config.global_scripts {
        add(&script.path, 1);
    }
    if let DefaultVenvLocation::Central { path } = &config.settings.default_venv_location {
        add(path, 0);
    }
    folders.sort();
    folders.dedup();
    let kept: Vec<String> = folders
        .iter()
        .filter(|f| {
            !folders
                .iter()
                .any(|other| other != *f && parts(f).starts_with(&parts(other)))
        })
        .cloned()
        .collect();
    let old_home = old_home.to_string_lossy();
    kept.into_iter()
        .map(|from| {
            let to = match parts(&from).strip_prefix(parts(&old_home).as_slice()) {
                Some(rest) => rest.iter().fold(new_home.to_path_buf(), |p, c| p.join(c)),
                None => PathBuf::from(&from),
            };
            FolderMap { from, to }
        })
        .collect()
}

/// `path` with the first matching `from` swapped for its `to`, joined with
/// this machine's separator. A path under no `from` is returned as it was.
pub fn remap(path: &Path, maps: &[FolderMap]) -> PathBuf {
    let text = path.to_string_lossy();
    let p = parts(&text);
    for map in maps {
        if let Some(rest) = p.strip_prefix(parts(&map.from).as_slice()) {
            return rest.iter().fold(map.to.clone(), |acc, c| acc.join(c));
        }
    }
    path.to_path_buf()
}

pub fn apply(mut config: Config, maps: &[FolderMap]) -> Config {
    let venv = |location: &mut VenvLocation| {
        if let VenvLocation::Custom { path } = location {
            *path = remap(path, maps);
        }
    };
    for project in &mut config.projects {
        project.pyproject_path = remap(&project.pyproject_path, maps);
        project
            .environments
            .iter_mut()
            .for_each(|e| venv(&mut e.location));
        venv(&mut project.active);
        for script in &mut project.scripts {
            script.path = remap(&script.path, maps);
        }
    }
    for script in &mut config.global_scripts {
        script.path = remap(&script.path, maps);
    }
    if let DefaultVenvLocation::Central { path } = &mut config.settings.default_venv_location {
        *path = remap(path, maps);
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Environment, Project, Script, VenvLocation};
    use std::path::Path;

    fn project(path: &str) -> Project {
        Project {
            pyproject_path: path.into(),
            ..Default::default()
        }
    }

    #[test]
    fn suggestions_swap_the_old_home_for_the_new() {
        let c = Config {
            projects: vec![
                project("/home/a/code/api/pyproject.toml"),
                project("/home/a/code/web/pyproject.toml"),
                project("/srv/tools/pyproject.toml"),
            ],
            ..Default::default()
        };
        let maps = suggest(&c, Path::new("/home/a"), Path::new("/Users/b"));
        assert_eq!(
            maps,
            [
                FolderMap {
                    from: "/home/a/code".into(),
                    to: "/Users/b/code".into()
                },
                FolderMap {
                    from: "/srv".into(),
                    to: "/srv".into()
                },
            ]
        );
    }

    #[test]
    fn a_nested_folder_folds_into_its_parent() {
        let mut c = Config {
            projects: vec![project("/home/a/code/api/pyproject.toml")],
            ..Default::default()
        };
        c.projects[0].environments = vec![Environment {
            location: VenvLocation::Custom {
                path: "/home/a/code/venvs/api".into(),
            },
            python: Default::default(),
        }];
        let maps = suggest(&c, Path::new("/home/a"), Path::new("/home/b"));
        assert_eq!(maps.len(), 1, "{maps:?}");
    }

    #[test]
    fn remap_swaps_the_prefix() {
        let maps = [FolderMap {
            from: "/home/a/code".into(),
            to: "/home/b/src".into(),
        }];
        assert_eq!(
            remap(Path::new("/home/a/code/api/pyproject.toml"), &maps),
            Path::new("/home/b/src/api/pyproject.toml")
        );
        assert_eq!(
            remap(Path::new("/opt/x"), &maps),
            Path::new("/opt/x"),
            "unmapped stays"
        );
        assert_eq!(
            remap(Path::new("/home/a/codex/y"), &maps),
            Path::new("/home/a/codex/y"),
            "a prefix is whole components, not characters"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_windows_path_maps_onto_a_unix_folder() {
        let maps = [FolderMap {
            from: r"C:\Users\a\code".into(),
            to: "/home/b/code".into(),
        }];
        assert_eq!(
            remap(Path::new(r"C:\Users\a\code\api\pyproject.toml"), &maps),
            Path::new("/home/b/code/api/pyproject.toml")
        );
    }

    #[test]
    fn apply_rewrites_every_path_field() {
        let maps = [FolderMap {
            from: "/home/a".into(),
            to: "/home/b".into(),
        }];
        let mut c = Config::default();
        let mut p = project("/home/a/api/pyproject.toml");
        p.environments = vec![Environment {
            location: VenvLocation::Custom {
                path: "/home/a/venvs/api".into(),
            },
            python: Default::default(),
        }];
        p.active = VenvLocation::Custom {
            path: "/home/a/venvs/api".into(),
        };
        p.scripts = vec![Script {
            path: "/home/a/pre.sh".into(),
            args: vec![],
            enabled: true,
        }];
        c.projects = vec![p];
        c.global_scripts = vec![Script {
            path: "/home/a/g.sh".into(),
            args: vec![],
            enabled: true,
        }];
        c.settings.default_venv_location = crate::model::DefaultVenvLocation::Central {
            path: "/home/a/.venvs".into(),
        };
        let c = apply(c, &maps);
        let p = &c.projects[0];
        assert_eq!(p.pyproject_path, Path::new("/home/b/api/pyproject.toml"));
        assert_eq!(
            p.environments[0].location,
            VenvLocation::Custom {
                path: "/home/b/venvs/api".into()
            }
        );
        assert_eq!(
            p.active,
            VenvLocation::Custom {
                path: "/home/b/venvs/api".into()
            }
        );
        assert_eq!(p.scripts[0].path, Path::new("/home/b/pre.sh"));
        assert_eq!(c.global_scripts[0].path, Path::new("/home/b/g.sh"));
        assert_eq!(
            c.settings.default_venv_location,
            crate::model::DefaultVenvLocation::Central {
                path: "/home/b/.venvs".into()
            }
        );
    }
}
