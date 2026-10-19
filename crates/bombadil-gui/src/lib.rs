//! Bombadil's iced front end.
//!
//! A library with one public item, [`run`]. The binary users install is the
//! `bombadil` crate, which exists to be a name worth typing and does nothing
//! but call it: `cargo install` only installs binaries defined *in* the
//! package being installed, never in its dependencies, so a meta crate has to
//! carry its own `main` and something to put in it.
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
mod add_local;
mod add_project;
mod app;
mod banner;
mod busy;
mod context_menu;
mod deps;
#[cfg(target_os = "linux")]
mod desktop_entry;
mod detail;
mod doctor_tab;
mod drawer;
mod env_editor;
mod github_tab;
mod glossary;
mod health;
mod icons;
mod index_editor;
mod interpreter;
mod job;
mod members_editor;
mod preferences;
mod scripts_editor;
mod settings_editor;
mod shell;
mod shortcuts;
mod sidebar;
mod theme;
mod tray;

use bombadil_core::model::Config;
use bombadil_core::process::{CommandRunner, SystemCommandRunner};
use bombadil_core::store::{Loaded, Store, paths};
use std::sync::Arc;

/// The icon's raw pixels, 64x64 sRGB RGBA.
///
/// Raw rather than a PNG because `iced::window::icon::from_rgba` wants exactly
/// this, and decoding a PNG for it would mean pulling an image decoder into a
/// binary that otherwise renders every pixel itself. `just icon` regenerates
/// it, and every PNG in the repo's own `assets/`, from `assets/icon.svg` --
/// the file to edit; this one is a build product that happens to be committed.
///
/// Inside this crate rather than beside the SVG it comes from, because
/// `cargo package` only takes files under the crate root: an
/// `include_bytes!("../../../assets/...")` packages cleanly and then fails to
/// compile for whoever installs it. The repo's `assets/` keeps the sources and
/// the PNGs that only the repo and the desktop entry need.
const ICON_RGBA: &[u8] = include_bytes!("../assets/icon-64.rgba");
const ICON_SIDE: u32 = 64;

/// The window icon, or `None` if the bytes will not make one.
///
/// A missing icon is not worth refusing to start over: the window manager
/// falls back to its own placeholder and everything else works. `expect` here
/// would turn a bad asset into an application that cannot open at all.
fn window_icon() -> Option<iced::window::Icon> {
    iced::window::icon::from_rgba(ICON_RGBA.to_vec(), ICON_SIDE, ICON_SIDE).ok()
}

/// Sets the Dock icon on macOS, where the window icon above is ignored.
///
/// A bundled `.app` already has one from its `Info.plist`; this is for the
/// bare binary `cargo install` and `pip install` put on the PATH, which the
/// Dock would otherwise show as a generic executable. Padded to Apple's grid
/// (see `scripts/render_assets.py`), so it sits level with its neighbours.
/// Called when a window opens, not at startup: winit creates `NSApplication`
/// as its own subclass and only sets the activation policy once launching
/// finishes, and changing the policy can put the default tile back.
#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use objc2::AnyThread;
    const PNG: &[u8] = include_bytes!("../assets/icon-macos.png");
    let Some(main_thread) = objc2::MainThreadMarker::new() else {
        return;
    };
    let data = objc2_foundation::NSData::with_bytes(PNG);
    let Some(image) = objc2_app_kit::NSImage::initWithData(objc2_app_kit::NSImage::alloc(), &data)
    else {
        return;
    };
    let app = objc2_app_kit::NSApplication::sharedApplication(main_thread);
    // SAFETY: on the main thread (checked above), with a valid image.
    unsafe { app.setApplicationIconImage(Some(&image)) };
}

/// The name the desktop expects this application to answer to: the basename of
/// `assets/bombadil.desktop`, the `Icon=` key inside it, and the file installed
/// into the hicolor icon theme all have to be this same word.
///
/// `cfg`-gated because its only reader is, and a constant that exists on a
/// platform nothing reads it from is dead code -- which this repo denies. The
/// same class of mistake as comparing a `display()`ed path to a literal:
/// invisible on the machine it was written on, a CI failure on the other two.
#[cfg(target_os = "linux")]
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
/// generic placeholder no matter what bytes this binary holds, until a
/// matching entry exists -- which `run` writes through `desktop_entry` on
/// every start.
fn window_settings(layout: &bombadil_core::model::Layout) -> iced::window::Settings {
    let defaults = iced::window::Settings::default();
    #[allow(unused_mut)]
    let mut settings = iced::window::Settings {
        icon: window_icon(),
        // Where the user left it, never smaller than a usable window.
        size: app::initial_size(layout).unwrap_or(defaults.size),
        // The close button is intercepted so the decision lives in `update`:
        // to the tray or quit, per Preferences (`close_to_tray`), and quit
        // regardless when there is no tray to come back from.
        exit_on_close_request: false,
        ..defaults
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

/// The tray's command channel, parked where a plain `fn` can reach it.
///
/// `Subscription::run_with` takes a function *pointer*, not a closure, so the
/// receiver cannot be captured and has to live somewhere global. `Option`
/// because a `Receiver` can only be consumed once: the first subscription
/// takes it, and any later one gets an empty stream rather than a panic --
/// a tray that stops working beats an application that stops running.
static TRAY_COMMANDS: std::sync::OnceLock<
    std::sync::Mutex<Option<std::sync::mpsc::Receiver<tray::TrayCommand>>>,
> = std::sync::OnceLock::new();

/// A `GitHubTick` every two minutes. iced's `time::every` needs the tokio or
/// smol backend and Bombadil runs the thread-pool one, so a sleeping thread
/// feeds the stream instead; when the subscription is dropped (watching
/// turned off) the next send fails and the thread ends.
fn github_ticker() -> iced::Subscription<app::Message> {
    iced::Subscription::run_with((), |()| {
        let (mut sender, receiver) = futures::channel::mpsc::channel(1);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(120));
                if sender.try_send(app::Message::GitHubTick).is_err() && sender.is_closed() {
                    break;
                }
            }
        });
        receiver
    })
}

/// Turns the tray's commands into application messages.
///
/// A `std::sync::mpsc::Receiver` is not a `Stream`, and blocking on one would
/// stall the executor -- the same problem `job::stream` already has, solved
/// the same way: a thread blocks on the receiver and forwards into a channel
/// iced can await.
fn tray_subscription() -> iced::Subscription<app::Message> {
    iced::Subscription::run_with((), |()| {
        let (mut sender, receiver) = futures::channel::mpsc::channel(8);
        let taken = TRAY_COMMANDS
            .get()
            .and_then(|slot| slot.lock().ok()?.take());
        if let Some(commands) = taken {
            std::thread::spawn(move || {
                while let Ok(command) = commands.recv() {
                    if sender.try_send(app::Message::Tray(command)).is_err() {
                        break;
                    }
                }
            });
        }
        receiver
    })
}

/// Runs the application. Returns when the last window closes.
///
/// Everything this needs -- the config, the persister, the runner, the tray --
/// is built in here rather than taken as a parameter, because there is exactly
/// one caller and it is a `main` that should hold no opinions.
pub fn run() -> iced::Result {
    #[cfg(target_os = "linux")]
    desktop_entry::install(APP_ID);
    let (loaded, persist) = load_config();
    let window = window_settings(&loaded.config().settings.layout);
    // `update` takes the runner as a parameter rather than storing it on
    // `App` so a test can substitute a `FakeCommandRunner`; here it is real.
    let runner: Arc<dyn CommandRunner> = Arc::new(SystemCommandRunner);

    // Started here, before the event loop, and held for the whole of `main`:
    // `TrayIcon` must be built on the thread that owns the event loop, and
    // dropping either handle removes the icon.
    //
    // `None` is an ordinary answer -- a desktop with no StatusNotifierItem
    // host has no tray, which is not a failure and not worth a message. What
    // it must do is reach `App::tray_is_running`, because that is what decides
    // whether the close button hides the window or closes it. Hiding with no
    // tray to restore from leaves a process the user cannot reach.
    let tray = tray::start(ICON_RGBA, ICON_SIDE);
    let tray_is_running = tray.is_some();
    // Owned by the update closure below, which lives as long as the event
    // loop -- so the icon does too.
    let tray = std::rc::Rc::new(tray.map(|(handle, commands)| {
        let _ = TRAY_COMMANDS.set(std::sync::Mutex::new(Some(commands)));
        handle
    }));

    // A daemon, not an application: an application exits when its last
    // window closes, and "to the tray" has to close the window -- Wayland
    // cannot hide one. The window is opened here, and again from the tray.
    iced::daemon(
        move || {
            let (mut state, task) = app::boot(loaded.clone());
            state.window_settings = window.clone();
            // `App::new` defaults `persist` to `NullPersist` so tests never
            // need a `Store`; the real one, opened alongside `loaded` below,
            // replaces it here for the actual app.
            state.persist = persist.clone();
            state.tray_is_running = tray_is_running;
            let open = app::open_window(&state);
            (state, iced::Task::batch([task, open]))
        },
        move |state: &mut app::App, message: app::Message| {
            #[cfg(target_os = "macos")]
            if matches!(message, app::Message::WindowOpened(_)) {
                set_dock_icon();
            }
            // Animation frames and pointer moves change no project, and come
            // sixty times a second; the menu is not rebuilt for them.
            let moves_nothing = matches!(
                message,
                app::Message::Tick(_) | app::Message::SidebarDragged(_)
            );
            let task = app::update(state, message, runner.clone());
            // The tray menu follows the projects; sent only when it changed.
            if !moves_nothing && let Some(tray) = tray.as_ref() {
                let model = tray::menu_entries(&state.entries);
                if model != state.tray_menu {
                    tray.set_menu(model.clone());
                    state.tray_menu = model;
                }
            }
            task
        },
        view,
    )
    .title("Bombadil")
    .subscription(|state: &app::App| {
        iced::Subscription::batch([
            iced::window::close_requests().map(|_| app::Message::CloseRequested),
            tray_subscription(),
            iced::system::theme_changes().map(app::Message::SystemThemeChanged),
            iced::window::events().filter_map(window_event),
            iced::event::listen_with(key_event),
            if state.config.settings.watch_github {
                github_ticker()
            } else {
                iced::Subscription::none()
            },
            // The pointer only while something is being dragged.
            if app::dragging(state) {
                iced::event::listen_with(drag_event)
            } else {
                iced::Subscription::none()
            },
            // Frames only while something moves; idle, nothing redraws.
            if app::animating(state) {
                iced::window::frames().map(app::Message::Tick)
            } else {
                iced::Subscription::none()
            },
        ])
    })
    // Both weights of Atkinson Hyperlegible Next share one font family, so
    // the renderer picks the right face by `Font::weight`; Plex Mono is
    // registered separately for the data typeface. Licences for both ship
    // in `about.rs`, alongside uv's.
    .font(theme::ATKINSON_REGULAR)
    .font(theme::ATKINSON_SEMIBOLD)
    .font(theme::PLEX_MONO_REGULAR)
    // Lucide's icon font, subset to the glyphs in `icons`.
    .font(icons::FONT_BYTES)
    .default_font(theme::FONT_PROSE)
    .theme(|state: &app::App, _window| theme::theme(state.theme_mode()))
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

/// A key press as a shortcut, if it is one. `status` says whether a widget
/// (a focused field) already handled it; `shortcuts::shortcut` decides.
fn key_event(
    event: iced::Event,
    status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<app::Message> {
    match event {
        // `repeat: false`: a held key fires once -- holding Ctrl+T must
        // open one terminal, and holding Esc close one layer.
        iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key,
            physical_key,
            modifiers,
            repeat: false,
            ..
        }) => {
            shortcuts::shortcut(&key, physical_key, modifiers, status).map(app::Message::Shortcut)
        }
        _ => None,
    }
}

/// The pointer during a drag: where it is, and the release that ends it --
/// wherever that lands, so a drag can never be left stuck on.
fn drag_event(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<app::Message> {
    match event {
        iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
            Some(app::Message::SidebarDragged(position.x))
        }
        iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
            Some(app::Message::DragEnded)
        }
        _ => None,
    }
}

/// The one window's view. A named `fn` because a closure cannot state that
/// the element borrows from `state`.
fn view(state: &app::App, _window: iced::window::Id) -> iced::Element<'_, app::Message> {
    app::view(state)
}

/// The window events the app acts on: regaining focus, and a new size.
fn window_event((_id, event): (iced::window::Id, iced::window::Event)) -> Option<app::Message> {
    match event {
        iced::window::Event::Focused => Some(app::Message::WindowFocused),
        iced::window::Event::Resized(size) => Some(app::Message::WindowResized(size)),
        iced::window::Event::FileDropped(path) => Some(app::Message::FileDropped(path)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::{Event, Key, Location, Modifiers, key};

    fn ctrl_t(repeat: bool) -> iced::Event {
        iced::Event::Keyboard(Event::KeyPressed {
            key: Key::Character("t".into()),
            modified_key: Key::Character("t".into()),
            physical_key: key::Physical::Code(key::Code::KeyT),
            location: Location::Standard,
            modifiers: Modifiers::COMMAND,
            text: None,
            repeat,
        })
    }

    #[test]
    fn a_held_key_fires_once() {
        // Auto-repeat would open a terminal per repeat, or peel every layer.
        let window = iced::window::Id::unique();
        assert!(key_event(ctrl_t(false), iced::event::Status::Ignored, window).is_some());
        assert!(key_event(ctrl_t(true), iced::event::Status::Ignored, window).is_none());
    }
}
