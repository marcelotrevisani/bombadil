//! Bombadil's iced front end.
//!
//! # Conventions these modules follow
//!
//! - `view` is deliberately dumb and untested. Logic worth a test lives in a
//!   free function that `view` calls.
//! - Runners, secret stores, config persisters and filesystem probes are
//!   **parameters**, never constructed inline, so tests stay hermetic.
//! - Everything that spawns a process or blocks on I/O goes through
//!   [`job::run`], never `update` directly.
//! - A secret must never enter a `Message`, which derives `Debug`.
//!
//! ## Never compare a `display()`ed path against a string literal
//!
//! `PathBuf::join` uses `\` on Windows, so `"/p/api/.venv"` never equals a
//! joined `/p/api\.venv`. Derive the expected value through the same function
//! production uses, or compare `Path`s — `Path` comparison is component-based
//! and accepts either separator.
//!
//! Two CI failures on this project came from a platform assumption hiding in an
//! assertion rather than in the code, invisible on Linux and macOS both times.

mod about;
mod add_project;
mod app;
mod banner;
mod context_menu;
mod deps;
mod detail;
mod drawer;
mod env_editor;
mod index_editor;
mod interpreter;
mod job;
mod members_editor;
mod preferences;
mod scripts_editor;
mod settings_editor;
mod shell;
mod sidebar;
mod theme;

use bombadil_core::model::Config;
use bombadil_core::process::{CommandRunner, SystemCommandRunner};
use bombadil_core::store::{Loaded, Store, paths};
use std::sync::Arc;

/// The icon's raw pixels, 64x64 sRGB RGBA.
///
/// Raw rather than a PNG because `iced::window::icon::from_rgba` wants exactly
/// this, and decoding a PNG for it would mean pulling an image decoder into a
/// binary that otherwise renders every pixel itself. `just icon` regenerates
/// this and every PNG beside it from `assets/icon.svg`, which is the file to
/// edit -- this one is a build product that happens to be committed.
const ICON_RGBA: &[u8] = include_bytes!("../../../assets/icon-64.rgba");
const ICON_SIDE: u32 = 64;

/// The window icon, or `None` if the bytes will not make one.
///
/// A missing icon is not worth refusing to start over: the window manager
/// falls back to its own placeholder and everything else works. `expect` here
/// would turn a bad asset into an application that cannot open at all.
fn window_icon() -> Option<iced::window::Icon> {
    iced::window::icon::from_rgba(ICON_RGBA.to_vec(), ICON_SIDE, ICON_SIDE).ok()
}

/// The name the desktop expects this application to answer to: the basename of
/// `assets/bombadil.desktop`, the `Icon=` key inside it, and the file installed
/// into the hicolor icon theme all have to be this same word.
const APP_ID: &str = "bombadil";

/// The window, with an icon set two different ways because desktops disagree
/// about where an icon comes from.
///
/// `icon` is the one a window carries itself. X11, Windows and macOS use it.
///
/// **GNOME on Wayland ignores it entirely.** There is no protocol request for
/// "here is my icon" that it honours; the shell resolves a window's icon by
/// matching its `app_id` against the basename of an installed `.desktop` file
/// and reading that file's `Icon=` key. So a Wayland session shows the
/// generic placeholder no matter what bytes this binary holds, until
/// `just install-desktop` puts the entry and the themed PNGs in place. Setting
/// `application_id` is the half that has to live in the binary; the other half
/// cannot.
fn window_settings() -> iced::window::Settings {
    #[allow(unused_mut)]
    let mut settings = iced::window::Settings {
        icon: window_icon(),
        ..Default::default()
    };
    // Linux-only because `platform_specific` is a different type per platform.
    // winit maps this to the Wayland `app_id` and to the X11 `WM_CLASS`, which
    // is also what the desktop entry's `StartupWMClass` has to match.
    #[cfg(target_os = "linux")]
    {
        settings.platform_specific.application_id = APP_ID.to_string();
    }
    settings
}

fn main() -> iced::Result {
    let (loaded, persist) = load_config();
    // `update` takes the runner as a parameter rather than storing it on
    // `App` so a test can substitute a `FakeCommandRunner`; here it is real.
    let runner: Arc<dyn CommandRunner> = Arc::new(SystemCommandRunner);

    iced::application(
        move || {
            let (mut state, task) = app::boot(loaded.clone());
            // `App::new` defaults `persist` to `NullPersist` so tests never
            // need a `Store`; the real one, opened alongside `loaded` below,
            // replaces it here for the actual app.
            state.persist = persist.clone();
            (state, task)
        },
        move |state: &mut app::App, message: app::Message| {
            app::update(state, message, runner.clone())
        },
        app::view,
    )
    .title("Bombadil")
    .window(window_settings())
    // Both weights of Atkinson Hyperlegible Next share one font family, so
    // the renderer picks the right face by `Font::weight`; Plex Mono is
    // registered separately for the data typeface. Licences for both ship
    // in `about.rs`, alongside uv's.
    .font(theme::ATKINSON_REGULAR)
    .font(theme::ATKINSON_SEMIBOLD)
    .font(theme::PLEX_MONO_REGULAR)
    .default_font(theme::FONT_PROSE)
    .theme(theme::theme())
    .run()
}

/// Loads the persisted config, starting from defaults rather than refusing to
/// open when loading does not go cleanly, and returns the store to save
/// through -- both come from opening the same file, so they belong together.
///
/// Returns the whole `Loaded`, not just its `Config`: `Unreadable` and
/// `ReadOnly` both carry defaults, so a caller that keeps only the config
/// cannot tell a corrupt file from an empty one and neither can the user.
/// `app::boot` turns the variant into a banner.
///
/// `Store::open` never returns an error: a missing config file already loads
/// as `Loaded::Fresh(Config::default())` and a corrupt one already loads as
/// `Loaded::Unreadable`, both carrying usable defaults. The only genuine
/// failure at this stage is not being able to locate a config directory at
/// all (no resolvable home directory), reported below as `Unreadable` --
/// which is what it is, from the user's point of view, and gets the same
/// banner rather than the same silence. There is no file to open a `Store`
/// on in that case, so confirming the add-project dialog gets
/// `app::NullPersist` instead, which reports the same failure again rather
/// than pretending to have saved.
fn load_config() -> (Loaded, Arc<dyn app::ConfigPersist>) {
    match paths::default_config_dir() {
        Ok(dir) => {
            let path = paths::config_file_in(&dir);
            let (store, loaded) = Store::open(path);
            (loaded, Arc::new(store))
        }
        Err(err) => (
            Loaded::Unreadable {
                config: Config::default(),
                message: err.to_string(),
            },
            Arc::new(app::NullPersist),
        ),
    }
}
