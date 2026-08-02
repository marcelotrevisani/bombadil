mod app;
mod banner;
mod detail;
mod drawer;
mod job;
mod sidebar;

use bombadil_core::model::Config;
use bombadil_core::process::{CommandRunner, SystemCommandRunner};
use bombadil_core::store::{Store, paths};
use std::sync::Arc;

fn main() -> iced::Result {
    let config = load_config();
    // `update` takes the runner as a parameter rather than storing it on
    // `App` so a test can substitute a `FakeCommandRunner`; here it is real.
    let runner: Arc<dyn CommandRunner> = Arc::new(SystemCommandRunner);

    iced::application(
        move || app::boot(config.clone()),
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
/// `Store::open` never returns an error: a missing config file already loads
/// as `Loaded::Fresh(Config::default())` and a corrupt one already loads as
/// `Loaded::Unreadable(Config::default())`, both carrying usable defaults.
/// The only genuine failure at this stage is not being able to locate a
/// config directory at all (no resolvable home directory), which is the one
/// case handled explicitly below.
fn load_config() -> Config {
    match paths::default_config_dir() {
        Ok(dir) => {
            let path = paths::config_file_in(&dir);
            let (_store, loaded) = Store::open(path);
            loaded.config().clone()
        }
        Err(_) => Config::default(),
    }
}
