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
mod scripts_editor;
mod settings_editor;
mod sidebar;
mod theme;

use bombadil_core::model::Config;
use bombadil_core::process::{CommandRunner, SystemCommandRunner};
use bombadil_core::store::{Loaded, Store, paths};
use std::sync::Arc;

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
