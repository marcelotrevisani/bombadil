mod about;
mod add_project;
mod app;
mod banner;
mod context_menu;
mod deps;
mod detail;
mod drawer;
mod interpreter;
mod job;
mod sidebar;

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
