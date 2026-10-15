//! The desktop entry Bombadil writes for itself on Linux, every start.
//!
//! GNOME on Wayland ignores the icon a window carries and takes it from the
//! `.desktop` file whose basename matches the window's `app_id` (see
//! `window_settings`). GLib drops an entry whose `Exec` program does not
//! exist, so one written once by `just install-desktop` breaks quietly as
//! soon as the binary moves or is renamed, and the window goes back to the
//! placeholder icon. Writing it from the running binary keeps `Exec` true
//! for every way of installing Bombadil, `pip` and `cargo install` included.

use std::path::{Path, PathBuf};

const ICON_PNG: &[u8] = include_bytes!("../assets/icon-256.png");

/// Writes the entry and its icon under `$XDG_DATA_HOME`, unless a package
/// already installed one system-wide. Best effort: failing to write leaves an
/// iconless window and is never a reason not to start.
pub fn install(app_id: &str) {
    let system = ["/usr/share/applications", "/usr/local/share/applications"];
    if system
        .iter()
        .any(|d| Path::new(d).join(format!("{app_id}.desktop")).exists())
    {
        return;
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    let (Some(data), Ok(exe)) = (data, std::env::current_exe()) else {
        return;
    };
    for (path, content) in files(&data, &exe, app_id) {
        if let Err(error) = write_if_changed(&path, &content) {
            eprintln!("bombadil: could not write {}: {error}", path.display());
        }
    }
}

/// What [`install`] writes, and where.
fn files(data: &Path, exe: &Path, app_id: &str) -> [(PathBuf, Vec<u8>); 2] {
    // An absolute `Icon=`: a bare name goes through the hicolor theme cache,
    // which does not list a file added after the cache was built.
    let icon = data.join(format!("icons/hicolor/256x256/apps/{app_id}.png"));
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Bombadil\n\
         GenericName=Virtual environment manager\n\
         Comment=A desktop keeper for uv virtual environments\n\
         Exec=\"{}\"\nIcon={}\nTerminal=false\nCategories=Development;IDE;\n\
         Keywords=python;uv;virtualenv;venv;\nStartupWMClass={app_id}\n",
        exe.display(),
        icon.display(),
    );
    [
        (icon, ICON_PNG.to_vec()),
        (
            data.join(format!("applications/{app_id}.desktop")),
            entry.into_bytes(),
        ),
    ]
}

/// Skips an identical file, so the shell is not told to reload on every start.
fn write_if_changed(path: &Path, content: &[u8]) -> std::io::Result<()> {
    if std::fs::read(path).is_ok_and(|old| old == content) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moved_binary_rewrites_exec() {
        // The bug this exists for: an entry pointing at a binary that is gone
        // is dropped by GLib, and the window loses its icon with no message.
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("applications/bombadil.desktop");
        for exe in ["/old/bombadil-gui", "/new/bombadil"] {
            for (path, content) in files(dir.path(), Path::new(exe), "bombadil") {
                write_if_changed(&path, &content).unwrap();
            }
        }
        let text = std::fs::read_to_string(entry).unwrap();
        assert!(text.contains("Exec=\"/new/bombadil\""), "{text}");
        assert!(text.contains("StartupWMClass=bombadil"));
        let icon = dir.path().join("icons/hicolor/256x256/apps/bombadil.png");
        assert!(text.contains(&format!("Icon={}", icon.display())), "{text}");
        assert_eq!(std::fs::read(icon).unwrap(), ICON_PNG);
    }
}
