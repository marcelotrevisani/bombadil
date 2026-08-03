//! The system tray, and the one rule that keeps it from stranding the user.
//!
//! # Never hide without somewhere to come back from
//!
//! Hiding the window is only reversible if something can un-hide it. If the
//! tray failed to start -- no StatusNotifierItem host on the bus, a desktop
//! with no tray at all -- and the close button hid the window anyway, the
//! result is a process with no window, no tray icon and no way back short of
//! `kill`. So [`start`] returns `None` when there is no tray, and the caller
//! must treat `None` as "the close button closes, as it always did".
//!
//! That is why this returns an `Option` rather than a `Tray` that quietly does
//! nothing: a silent no-op tray is exactly the shape that produces the
//! stranded process.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// Bring the window back and focus it.
    Show,
    /// Put the window away, leaving this process running.
    Hide,
    /// Quit for real.
    Quit,
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
}

/// The rule that keeps a hidden window reachable.
///
/// `Hide` becomes `Close` when no tray is running. That is the whole safety
/// property of this module: hiding is only reversible if something can
/// un-hide it, and with no tray icon there is nothing to click. A user who
/// pressed the close button on a trayless desktop gets what every close button
/// has always done, rather than a process with no window and no way back.
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
    }
}

/// A running tray. **Dropping this removes the icon**, so the caller has to
/// hold it for as long as the application runs.
pub struct Tray {
    #[allow(dead_code)] // Held for its `Drop`, never read.
    inner: Inner,
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
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&[px[3], px[0], px[1], px[2]]);
    }
    out
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{TrayCommand, argb32};
    use std::sync::mpsc::Sender;

    pub struct Handle(#[allow(dead_code)] pub ksni::blocking::Handle<Menu>);

    pub struct Menu {
        pub commands: Sender<TrayCommand>,
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
            use ksni::menu::{MenuItem, StandardItem};
            vec![
                StandardItem {
                    label: "Show Bombadil".into(),
                    activate: Box::new(|this: &mut Self| {
                        let _ = this.commands.send(TrayCommand::Show);
                    }),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "Hide".into(),
                    activate: Box::new(|this: &mut Self| {
                        let _ = this.commands.send(TrayCommand::Hide);
                    }),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: "Quit".into(),
                    activate: Box::new(|this: &mut Self| {
                        let _ = this.commands.send(TrayCommand::Quit);
                    }),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }
}

#[cfg(target_os = "linux")]
type Inner = linux::Handle;

#[cfg(any(target_os = "macos", target_os = "windows"))]
type Inner = tray_icon::TrayIcon;

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
        rgba,
        side: side as i32,
    }
    .spawn()
    .ok()
    .map(linux::Handle)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn build(
    commands: std::sync::mpsc::Sender<TrayCommand>,
    rgba: &'static [u8],
    side: u32,
) -> Option<Inner> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};

    let icon = tray_icon::Icon::from_rgba(rgba.to_vec(), side, side).ok()?;

    let show = MenuItem::new("Show Bombadil", true, None);
    let hide = MenuItem::new("Hide", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let (show_id, hide_id, quit_id) = (show.id().clone(), hide.id().clone(), quit.id().clone());

    let menu = Menu::new();
    menu.append_items(&[&show, &hide, &PredefinedMenuItem::separator(), &quit])
        .ok()?;

    let tray = tray_icon::TrayIconBuilder::new()
        .with_tooltip("Bombadil")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .build()
        .ok()?;

    // `tray-icon` reports menu presses through a process-global channel rather
    // than a callback on the item, so one thread translates them into the same
    // `TrayCommand`s the Linux path sends. Ends on its own when the receiver
    // is dropped at exit.
    std::thread::spawn(move || {
        while let Ok(event) = MenuEvent::receiver().recv() {
            let command = if event.id == show_id {
                TrayCommand::Show
            } else if event.id == hide_id {
                TrayCommand::Hide
            } else if event.id == quit_id {
                TrayCommand::Quit
            } else {
                continue;
            };
            if commands.send(command).is_err() {
                break;
            }
        }
    });

    Some(tray)
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
