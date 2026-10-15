//! The system tray, and the one rule that keeps it from stranding the user.
//!
//! # The close button hides, and says so in Preferences
//!
//! The X keeps Bombadil in the tray by default, because the tray menu is a
//! way to use it: open a terminal or PyCharm for any project's environment
//! without the window. It once did this silently and was reported as "the
//! exit did nothing", so the behaviour is a visible switch (Preferences >
//! Appearance, "Closing the window"), and the tray's Quit always quits.
//!
//! # Never hide without somewhere to come back from
//!
//! Hiding is only reversible if something can un-hide it: a process with no
//! window and no tray icon has no way back short of `kill`. [`start`] returns
//! `None` when there is no tray, rather than a `Tray` that quietly does
//! nothing, because a silent no-op tray is exactly the shape that produces
//! that state.
//!
//! The X sends `Hide`, so [`window_action`]'s guard is load-bearing: with no
//! tray running, `Hide` becomes a close.
//!
//! "Hide" closes the window rather than hiding it: Wayland has no way to hide
//! a window (winit's `set_visible` is a no-op there), which is how the first
//! version of this left the X doing nothing at all. The app runs as an iced
//! daemon, so the process outlives its window, and Show opens a new one.
//!
//! # Why two implementations
//!
//! Linux uses [`ksni`], which speaks StatusNotifierItem over D-Bus in pure
//! Rust. That is the protocol GNOME and KDE actually use -- the X11 system
//! tray protocol does not exist on Wayland -- and it costs this build no GTK
//! and no X11. `tray-icon`, the obvious single-crate answer, enables `gtk` and
//! `libxdo` by default, which would put libgtk-3-dev, libxdo-dev and
//! libayatana-appindicator3-dev in front of every Linux contributor and every
//! CI job, to compile an application that otherwise draws every pixel itself.
//!
//! macOS and Windows have no such protocol and no lighter option, so they use
//! `tray-icon`, which needs no GTK there.
//!
//! # What is verified, and where
//!
//! The Linux path was run against a real GNOME/Wayland session with Ubuntu's
//! appindicator extension. **The macOS and Windows paths have been compiled
//! and never run** -- see [`start`]'s own notes on what could differ.

/// What a press on the tray asks the application to do.
///
/// Deliberately not `Toggle`: the tray thread does not know whether the window
/// is currently hidden, and a toggle computed from a stale answer shows a
/// window the user just hid. `update` knows, so `update` decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayCommand {
    /// Bring the window back and focus it.
    Show,
    /// Put the window away, leaving this process running.
    Hide,
    /// Quit for real.
    Quit,
    /// Open a terminal for one environment of one project. By id and
    /// location, never position: the menu may be older than the list.
    OpenTerminal {
        project: uuid::Uuid,
        environment: bombadil_core::model::VenvLocation,
    },
    /// Open PyCharm with one environment of one project as its interpreter.
    OpenPycharm {
        project: uuid::Uuid,
        environment: bombadil_core::model::VenvLocation,
    },
}

/// The tray menu, independent of the toolkit that draws it: both
/// implementations build from this, so both show the same thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    Item {
        label: String,
        command: TrayCommand,
    },
    Disabled(String),
    Separator,
    Submenu {
        label: String,
        children: Vec<MenuEntry>,
    },
}

/// Show, then a submenu per project (a submenu per environment, or the
/// actions directly when there is only one), then Quit. "Hide" is not
/// here: the window's close button does that. Built from the sidebar's
/// entries, so it names environments exactly as the window does.
pub fn menu_entries(entries: &[crate::sidebar::Entry]) -> Vec<MenuEntry> {
    use crate::sidebar::EnvironmentRow;
    let actions = |project: uuid::Uuid, row: &EnvironmentRow| {
        vec![
            MenuEntry::Item {
                label: "Open terminal".into(),
                command: TrayCommand::OpenTerminal {
                    project,
                    environment: row.location.clone(),
                },
            },
            MenuEntry::Item {
                label: "Open in PyCharm".into(),
                command: TrayCommand::OpenPycharm {
                    project,
                    environment: row.location.clone(),
                },
            },
        ]
    };
    // Its directory name, its Python, and whether it is the active one.
    let label = |row: &EnvironmentRow| {
        let name = row
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| row.path.display().to_string());
        let python = match &row.python_version {
            Some(version) => format!("Python {version}"),
            None => "not created".to_string(),
        };
        let active = if row.active { " (active)" } else { "" };
        format!("{name} \u{b7} {python}{active}")
    };
    let mut menu = vec![
        MenuEntry::Item {
            label: "Show Bombadil".into(),
            command: TrayCommand::Show,
        },
        MenuEntry::Separator,
    ];
    for entry in entries {
        let children = match entry.environments.as_slice() {
            [] => vec![MenuEntry::Disabled("No environment".into())],
            [only] => actions(entry.id, only),
            several => several
                .iter()
                .map(|row| MenuEntry::Submenu {
                    label: label(row),
                    children: actions(entry.id, row),
                })
                .collect(),
        };
        menu.push(MenuEntry::Submenu {
            label: entry.label.clone(),
            children,
        });
    }
    menu.push(MenuEntry::Separator);
    menu.push(MenuEntry::Item {
        label: "Quit".into(),
        command: TrayCommand::Quit,
    });
    menu
}

/// What a [`TrayCommand`] means for the window.
///
/// Separate from the command because the two are not the same question: the
/// tray asks for `Hide`, and whether that is a hide or a close depends on
/// something only the application knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    /// Restore the window and focus it.
    Show,
    /// Hide the window, leaving the process running.
    Hide,
    /// Close the window, which ends the application.
    Close,
    /// Leave the window as it is: the command opens something else.
    Stay,
}

/// The rule that keeps a hidden window reachable.
///
/// `Hide` becomes `Close` when no tray is running: hiding is only reversible
/// if something can un-hide it, and with no tray icon there is nothing to
/// click.
///
/// A free function rather than a `match` inside `update` because `update`
/// returns an opaque `Task` -- there is no way to assert from outside it
/// whether the window was hidden or closed, so the decision has to be
/// somewhere a test can see it.
pub fn window_action(command: TrayCommand, tray_is_running: bool) -> WindowAction {
    match command {
        TrayCommand::Show => WindowAction::Show,
        TrayCommand::Hide if tray_is_running => WindowAction::Hide,
        // Both the trayless close button and a real Quit.
        TrayCommand::Hide | TrayCommand::Quit => WindowAction::Close,
        TrayCommand::OpenTerminal { .. } | TrayCommand::OpenPycharm { .. } => WindowAction::Stay,
    }
}

/// A running tray. **Dropping this removes the icon**, so the caller has to
/// hold it for as long as the application runs.
pub struct Tray {
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos", target_os = "windows")),
        allow(dead_code) // Held for its `Drop`.
    )]
    inner: Inner,
}

impl Tray {
    /// Shows `menu`. Called by the update loop only when it changed.
    #[cfg(target_os = "linux")]
    pub fn set_menu(&self, entries: Vec<MenuEntry>) {
        // Thread-safe: ksni re-reads `menu()` on its own thread.
        self.inner.0.update(|menu| menu.entries = entries);
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn set_menu(&self, entries: Vec<MenuEntry>) {
        let (menu, ids) = native::menu(entries);
        // Extended, not replaced: a menu rebuilt while it is open still has
        // its old items on screen, and a press on one must still resolve.
        // Commands carry ids, so an old item can never act on the wrong
        // project. ponytail: grows by one menu's worth per rebuild; prune if
        // a session ever rebuilds thousands of times.
        if let Ok(mut commands) = self.inner.commands.lock() {
            commands.extend(ids);
        }
        self.inner.icon.set_menu(Some(Box::new(menu)));
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    pub fn set_menu(&self, entries: Vec<MenuEntry>) {
        let _ = entries;
    }
}

/// The icon the tray shows, as ARGB32 in network byte order -- what
/// StatusNotifierItem's pixmap property is defined as.
///
/// Converted from the same `icon-64.rgba` the window icon uses rather than
/// shipping a second copy: one asset, two byte orders, and no way for the two
/// icons to drift apart.
#[cfg(target_os = "linux")]
fn argb32(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for [r, g, b, a] in rgba.as_chunks::<4>().0 {
        out.extend_from_slice(&[*a, *r, *g, *b]);
    }
    out
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{MenuEntry, TrayCommand, argb32};

    /// StatusNotifierItem menus read `_` as an access-key marker and drop
    /// it; doubled, it shows as itself. `my_api` must not read `myapi`.
    pub fn literal(label: &str) -> String {
        label.replace('_', "__")
    }

    pub fn item(entry: MenuEntry) -> ksni::MenuItem<Menu> {
        use ksni::menu::{MenuItem, StandardItem, SubMenu};
        match entry {
            MenuEntry::Item { label, command } => StandardItem {
                label: literal(&label),
                activate: Box::new(move |this: &mut Menu| {
                    let _ = this.commands.send(command.clone());
                }),
                ..Default::default()
            }
            .into(),
            MenuEntry::Disabled(label) => StandardItem {
                label: literal(&label),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuEntry::Separator => MenuItem::Separator,
            MenuEntry::Submenu { label, children } => SubMenu {
                label: literal(&label),
                submenu: children.into_iter().map(item).collect(),
                ..Default::default()
            }
            .into(),
        }
    }
    use std::sync::mpsc::Sender;

    pub struct Handle(#[allow(dead_code)] pub ksni::blocking::Handle<Menu>);

    pub struct Menu {
        pub commands: Sender<TrayCommand>,
        pub entries: Vec<MenuEntry>,
        pub rgba: &'static [u8],
        pub side: i32,
    }

    impl ksni::Tray for Menu {
        fn id(&self) -> String {
            // The same word as the window's `app_id` and the desktop entry's
            // basename, so a shell that groups the tray item with the window
            // has something to group on.
            "bombadil".into()
        }

        fn title(&self) -> String {
            "Bombadil".into()
        }

        // Deliberately no `icon_name`. Pointing at the icon theme would make
        // the tray depend on `just install-desktop` having been run, and a
        // host that prefers a name it cannot resolve draws nothing at all.
        // The pixmap is always right and needs nothing installed.
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            vec![ksni::Icon {
                width: self.side,
                height: self.side,
                data: argb32(self.rgba),
            }]
        }

        /// A left click. The convention every tray application follows, and
        /// the one users try first.
        fn activate(&mut self, _x: i32, _y: i32) {
            let _ = self.commands.send(TrayCommand::Show);
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            self.entries.iter().cloned().map(item).collect()
        }
    }
}

#[cfg(target_os = "linux")]
type Inner = linux::Handle;

#[cfg(any(target_os = "macos", target_os = "windows"))]
type Inner = native::Handle;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
type Inner = ();

/// Start the tray, or `None` when this desktop has none.
///
/// `rgba` is the window icon's own pixels, `side` its width and height.
///
/// **`None` is a real answer, not a failure to report.** A machine with no
/// StatusNotifierItem host is an ordinary machine; the application runs
/// exactly as it did before, with a close button that closes. What the caller
/// must not do is hide the window anyway -- see the module doc.
///
/// # Platform notes
///
/// Linux: run against GNOME/Wayland with Ubuntu's appindicator extension.
/// A GNOME session *without* an appindicator extension has no host, so this
/// returns `None` and the window keeps its ordinary close button.
///
/// macOS and Windows: **compiled, never run.** `tray_icon::TrayIcon` must be
/// built on the thread that owns the event loop, which is why this is called
/// from `main` before `iced::application(..).run()` rather than from a
/// background thread. The specific untested risk is macOS: `NSStatusItem`
/// wants a running `NSApplication`, and winit creates one when the event loop
/// starts -- which is after this returns. If the icon does not appear there,
/// that ordering is the first thing to check.
pub fn start(
    rgba: &'static [u8],
    side: u32,
) -> Option<(Tray, std::sync::mpsc::Receiver<TrayCommand>)> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let inner = build(sender, rgba, side)?;
    Some((Tray { inner }, receiver))
}

#[cfg(target_os = "linux")]
fn build(
    commands: std::sync::mpsc::Sender<TrayCommand>,
    rgba: &'static [u8],
    side: u32,
) -> Option<Inner> {
    use ksni::blocking::TrayMethods;

    // `spawn` fails when nothing on the session bus implements
    // StatusNotifierWatcher -- a desktop with no tray. That is the `None` the
    // whole module is shaped around, not an error worth reporting.
    linux::Menu {
        commands,
        entries: menu_entries(&[]),
        rgba,
        side: side as i32,
    }
    .spawn()
    .ok()
    .map(linux::Handle)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native {
    use super::{MenuEntry, TrayCommand};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tray_icon::menu::{IsMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};

    /// Which command each item of the current menu sends. Replaced on every
    /// rebuild; read by the menu event handler on whatever thread it runs.
    pub type Commands = Arc<Mutex<HashMap<MenuId, TrayCommand>>>;

    pub struct Handle {
        pub icon: tray_icon::TrayIcon,
        pub commands: Commands,
    }

    /// Windows reads `&` as an access-key marker; doubled, it is itself.
    fn literal(label: &str) -> String {
        if cfg!(target_os = "windows") {
            label.replace('&', "&&")
        } else {
            label.to_string()
        }
    }

    fn build(entry: MenuEntry, ids: &mut HashMap<MenuId, TrayCommand>) -> Box<dyn IsMenuItem> {
        match entry {
            MenuEntry::Item { label, command } => {
                let item = MenuItem::new(literal(&label), true, None);
                ids.insert(item.id().clone(), command);
                Box::new(item)
            }
            MenuEntry::Disabled(label) => Box::new(MenuItem::new(literal(&label), false, None)),
            MenuEntry::Separator => Box::new(PredefinedMenuItem::separator()),
            MenuEntry::Submenu { label, children } => {
                let sub = Submenu::new(literal(&label), true);
                for child in children {
                    let _ = sub.append(build(child, ids).as_ref());
                }
                Box::new(sub)
            }
        }
    }

    /// The menu for `entries`, and which command each item sends.
    pub fn menu(entries: Vec<MenuEntry>) -> (Menu, HashMap<MenuId, TrayCommand>) {
        let menu = Menu::new();
        let mut ids = HashMap::new();
        for entry in entries {
            let _ = menu.append(build(entry, &mut ids).as_ref());
        }
        (menu, ids)
    }
}

/// macOS and Windows. **Compiled, never run** -- see [`start`].
///
/// `tray-icon` objects belong to the main thread. This is called from `run`
/// before the event loop, and [`Tray::set_menu`] from iced's `update`, which
/// runs on that same thread.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn build(
    commands: std::sync::mpsc::Sender<TrayCommand>,
    rgba: &'static [u8],
    side: u32,
) -> Option<Inner> {
    use tray_icon::menu::MenuEvent;
    use tray_icon::{MouseButton, TrayIconEvent};

    let icon = tray_icon::Icon::from_rgba(rgba.to_vec(), side, side).ok()?;
    let (menu, ids) = native::menu(menu_entries(&[]));
    let ids: native::Commands = std::sync::Arc::new(std::sync::Mutex::new(ids));

    let tray = tray_icon::TrayIconBuilder::new()
        .with_tooltip("Bombadil")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        // Windows: a left click does nothing, the right click opens the menu,
        // and a double click opens the window (below) -- the platform's
        // convention. macOS opens the menu on any click regardless.
        .with_menu_on_left_click(cfg!(target_os = "macos"))
        .build()
        .ok()?;

    // `tray-icon` reports presses through process-global handlers rather than
    // a callback per item; both translate into the same `TrayCommand`s the
    // Linux path sends.
    let menu_commands = commands.clone();
    let menu_ids = ids.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let command = menu_ids
            .lock()
            .ok()
            .and_then(|ids| ids.get(&event.id).cloned());
        if let Some(command) = command {
            let _ = menu_commands.send(command);
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        } = event
        {
            let _ = commands.send(TrayCommand::Show);
        }
    }));

    Some(native::Handle {
        icon: tray,
        commands: ids,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn build(
    _commands: std::sync::mpsc::Sender<TrayCommand>,
    _rgba: &'static [u8],
    _side: u32,
) -> Option<Inner> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(label: &str, envs: &[&str]) -> crate::sidebar::Entry {
        crate::sidebar::Entry {
            id: uuid::Uuid::nil(),
            label: label.into(),
            requires_python: None,
            environments: envs
                .iter()
                .map(|e| crate::sidebar::EnvironmentRow {
                    location: bombadil_core::model::VenvLocation::Custom { path: e.into() },
                    path: e.into(),
                    python_version: None,
                    pin: bombadil_core::model::PythonPin::Unpinned,
                    active: false,
                })
                .collect(),
            expanded: false,
            git: None,
        }
    }

    fn shape(entries: &[MenuEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| match e {
                MenuEntry::Item { label, .. } => label.clone(),
                MenuEntry::Disabled(label) => format!("({label})"),
                MenuEntry::Separator => "--".into(),
                MenuEntry::Submenu { label, children } => {
                    format!("{label}[{}]", shape(children).join(","))
                }
            })
            .collect()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn underscores_in_names_survive_the_linux_menu() {
        assert_eq!(linux::literal("my_api"), "my__api");
        assert_eq!(linux::literal("plain"), "plain");
    }

    #[test]
    fn the_menu_nests_projects_and_environments() {
        let menu = menu_entries(&[
            project("api", &["a", "b"]),
            project("worker", &["w"]),
            project("empty", &[]),
        ]);
        assert_eq!(
            shape(&menu),
            [
                "Show Bombadil",
                "--",
                "api[a \u{b7} not created[Open terminal,Open in PyCharm],\
                 b \u{b7} not created[Open terminal,Open in PyCharm]]",
                "worker[Open terminal,Open in PyCharm]",
                "empty[(No environment)]",
                "--",
                "Quit",
            ]
        );
    }

    #[test]
    fn a_menu_press_names_the_environment_it_was_built_for() {
        let menu = menu_entries(&[project("worker", &["w"])]);
        let MenuEntry::Submenu { children, .. } = &menu[2] else {
            panic!("{:?}", shape(&menu));
        };
        let MenuEntry::Item { command, .. } = &children[0] else {
            panic!();
        };
        assert_eq!(
            *command,
            TrayCommand::OpenTerminal {
                project: uuid::Uuid::nil(),
                environment: bombadil_core::model::VenvLocation::Custom { path: "w".into() },
            }
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rgba_becomes_argb_in_network_byte_order() {
        // StatusNotifierItem defines the pixmap as ARGB32, network byte order.
        // Getting this wrong does not fail -- it draws the icon in the wrong
        // colours with the alpha channel read as red, which looks like a
        // corrupt asset rather than a byte-order bug.
        let rgba = [0x11, 0x22, 0x33, 0xFF, 0x44, 0x55, 0x66, 0x80];

        assert_eq!(
            argb32(&rgba),
            vec![0xFF, 0x11, 0x22, 0x33, 0x80, 0x44, 0x55, 0x66]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn every_pixel_survives_the_conversion() {
        // A conversion that dropped or added a pixel would shift every
        // subsequent row, which reads as a sheared image.
        let rgba: Vec<u8> = (0..64 * 64 * 4).map(|i| (i % 251) as u8).collect();

        assert_eq!(argb32(&rgba).len(), rgba.len());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_trailing_partial_pixel_is_dropped_rather_than_read_out_of_bounds() {
        // `chunks_exact` is the guard. Nothing should ever hand this a
        // ragged buffer, but reading past one would be a panic in a thread
        // the user cannot see.
        let ragged = [0x11, 0x22, 0x33, 0xFF, 0x44, 0x55];

        assert_eq!(argb32(&ragged), vec![0xFF, 0x11, 0x22, 0x33]);
    }

    #[test]
    fn hiding_without_a_tray_closes_instead() {
        // The safety property. Hiding with no tray icon to restore from
        // leaves a process with no window and no way back short of `kill` --
        // and the close button is the most likely way to reach it, since a
        // trayless desktop still has one.
        assert_eq!(
            window_action(TrayCommand::Hide, false),
            WindowAction::Close,
            "with no tray, the close button must close"
        );
    }

    #[test]
    fn hiding_with_a_tray_actually_hides() {
        // The other half. Without this, an implementation that always closed
        // would pass the test above and the feature would not exist.
        assert_eq!(
            window_action(TrayCommand::Hide, true),
            WindowAction::Hide,
            "with a tray, the close button must hide"
        );
    }

    #[test]
    fn quit_closes_whether_or_not_a_tray_is_running() {
        // Quit is only reachable *from* the tray, so the `false` case should
        // not arise -- but an implementation that made Quit conditional on
        // the same flag as Hide would leave the one control that is supposed
        // to end the application doing nothing.
        for running in [true, false] {
            assert_eq!(
                window_action(TrayCommand::Quit, running),
                WindowAction::Close,
                "Quit must quit (tray running: {running})"
            );
        }
    }

    #[test]
    fn showing_is_unconditional() {
        // Show arrives only from a tray press, so a tray is running by
        // definition; gating it on the flag would be a way for the window to
        // become unreachable if that flag were ever wrong.
        for running in [true, false] {
            assert_eq!(
                window_action(TrayCommand::Show, running),
                WindowAction::Show
            );
        }
    }

    #[test]
    fn the_three_commands_are_distinct() {
        // `Show` and `Hide` are separate on purpose: a `Toggle` computed on
        // the tray thread would be computed from a stale idea of whether the
        // window is visible.
        assert_ne!(TrayCommand::Show, TrayCommand::Hide);
        assert_ne!(TrayCommand::Show, TrayCommand::Quit);
        assert_ne!(TrayCommand::Hide, TrayCommand::Quit);
    }
}
