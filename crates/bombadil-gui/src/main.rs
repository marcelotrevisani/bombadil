mod about;
mod add_project;
mod app;
mod banner;
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
    let loaded = load_config();
    // `update` takes the runner as a parameter rather than storing it on
    // `App` so a test can substitute a `FakeCommandRunner`; here it is real.
    let runner: Arc<dyn CommandRunner> = Arc::new(SystemCommandRunner);

    iced::application(
        move || app::boot(loaded.clone()),
        move |state: &mut app::App, message: app::Message| {
            app::update(state, message, runner.clone())
        },
        app::view,
    )
    .title("Bombadil")
    .run()
}

/// Loads the persisted config, starting from defaults rather than refusing to
/// open when loading does not go cleanly.
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
/// banner rather than the same silence.
fn load_config() -> Loaded {
    match paths::default_config_dir() {
        Ok(dir) => {
            let path = paths::config_file_in(&dir);
            let (_store, loaded) = Store::open(path);
            loaded
        }
        Err(err) => Loaded::Unreadable {
            config: Config::default(),
            message: err.to_string(),
        },
    }
}
