//! Keeping a machine tidy: how much space environments and caches take,
//! which environment folders nothing uses any more, and what a project's
//! lock file says is installed.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The total length of the files under `path`. Links are never followed --
/// a venv's link to its interpreter, or a loop, would count something that
/// isn't this folder's -- and entries that can't be read are skipped.
pub fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.file_type().is_symlink() {
        return 0;
    }
    if !meta.is_dir() {
        return meta.len();
    }
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

/// `412.3 MB`: binary units, one decimal, as file managers show them.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `path` as the filesystem resolves it -- links followed, case and `..`
/// settled -- or as written when it can't be resolved.
fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether `folder` must never be treated as a leftover: it is a project
/// (it has a `pyproject.toml`), or a registered environment or project
/// folder is it or lies inside it. Compared resolved, so one folder written
/// two ways -- through a link, or in another case -- is still one folder.
pub fn in_use(folder: &Path, registered: &[PathBuf]) -> bool {
    if folder.join("pyproject.toml").exists() {
        return true;
    }
    let folder = resolved(folder);
    registered.iter().any(|r| resolved(r).starts_with(&folder))
}

/// The virtual environments directly inside `central` that nothing uses:
/// what a removed project left behind. `registered` holds every registered
/// environment path and every project folder.
pub fn leftovers(central: &Path, registered: &[PathBuf]) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(central) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
                && crate::venv_path::probe(p).is_some()
                && !in_use(p, registered)
        })
        .collect();
    found.sort();
    found
}

/// The packages a `uv.lock` pins, as `(name, version)`. `None` when the file
/// can't be read as a lock, so a caller can tell "nothing" from "unknown".
pub fn lock_packages(text: &str) -> Option<Vec<(String, String)>> {
    #[derive(Deserialize)]
    struct Package {
        name: String,
        #[serde(default)]
        version: String,
    }
    #[derive(Deserialize)]
    struct Lock {
        #[serde(default)]
        package: Vec<Package>,
    }
    let lock: Lock = toml::from_str(text).ok()?;
    Some(
        lock.package
            .into_iter()
            .map(|p| (p.name, p.version))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn venv(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("pyvenv.cfg"), "version_info = 3.12.0\n").unwrap();
    }

    #[test]
    fn sizes_add_up_file_lengths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), vec![0u8; 100]).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("b"), vec![0u8; 50]).unwrap();
        assert_eq!(dir_size(dir.path()), 150);
        assert_eq!(dir_size(&dir.path().join("missing")), 0);
    }

    #[cfg(unix)]
    #[test]
    fn the_walk_never_follows_links() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("big"), vec![0u8; 10_000]).unwrap();
        std::fs::write(dir.path().join("a"), vec![0u8; 10]).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("out")).unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
        assert!(dir_size(dir.path()) < 1_000, "{}", dir_size(dir.path()));
    }

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(432_330_342), "412.3 MB");
        assert_eq!(format_size(1_288_490_189), "1.2 GB");
    }

    #[test]
    fn leftovers_are_unregistered_venvs() {
        let central = tempfile::tempdir().unwrap();
        venv(&central.path().join("a"));
        venv(&central.path().join("b"));
        std::fs::create_dir(central.path().join("c")).unwrap();
        let found = leftovers(central.path(), &[central.path().join("a")]);
        assert_eq!(found, [central.path().join("b")]);
        assert!(leftovers(&central.path().join("nope"), &[]).is_empty());
    }

    #[test]
    fn registered_folders_are_not_leftovers() {
        let central = tempfile::tempdir().unwrap();
        venv(&central.path().join("a"));
        assert!(leftovers(central.path(), &[central.path().join("a")]).is_empty());
    }

    #[test]
    fn a_project_folder_is_never_a_leftover() {
        // `python -m venv .` once run inside a project leaves a pyvenv.cfg
        // in the project folder itself.
        let central = tempfile::tempdir().unwrap();
        let tool = central.path().join("tool");
        venv(&tool);
        std::fs::write(tool.join("main.py"), "print()").unwrap();
        assert!(
            leftovers(central.path(), &[tool.join(".venv")]).is_empty(),
            "holds a registered env"
        );
        std::fs::write(tool.join("pyproject.toml"), "[project]\nname='tool'\n").unwrap();
        assert!(leftovers(central.path(), &[]).is_empty(), "is a project");
    }

    #[cfg(unix)]
    #[test]
    fn a_registered_env_reached_through_a_link_is_not_a_leftover() {
        let real = tempfile::tempdir().unwrap();
        venv(&real.path().join("foo"));
        let holder = tempfile::tempdir().unwrap();
        let link = holder.path().join("link");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        assert!(leftovers(&link, &[real.path().join("foo")]).is_empty());
    }

    #[test]
    fn a_lock_lists_its_packages() {
        let text = "version = 1\n\n[[package]]\nname = \"certifi\"\nversion = \"2026.7.22\"\n\n[[package]]\nname = \"pipeline\"\nversion = \"0.1.0\"\nsource = { virtual = \".\" }\n";
        assert_eq!(
            lock_packages(text).unwrap(),
            [
                ("certifi".to_string(), "2026.7.22".to_string()),
                ("pipeline".to_string(), "0.1.0".to_string())
            ]
        );
    }

    #[test]
    fn an_unreadable_lock_is_none() {
        assert!(lock_packages("not [ toml").is_none());
        assert_eq!(lock_packages("version = 1\n").unwrap(), []);
    }
}
