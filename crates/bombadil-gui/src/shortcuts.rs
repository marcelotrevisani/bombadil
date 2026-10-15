//! Bombadil's keyboard shortcuts: which keys, and what each is called.
//! Whether one may run right now is `app::shortcut_message`'s call, since
//! that reads the application state.

use iced::event::Status;
use iced::keyboard::{Key, Modifiers, key::Named, key::Physical};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    AddProject,
    Sync,
    Terminal,
    Refresh,
    Escape,
    ToggleSidebar,
    Find,
}

/// `key` after the command modifier as the platform names it: iced's
/// `command()` is Ctrl everywhere but macOS, where it is ⌘.
macro_rules! with_mod {
    ($key:literal) => {
        if cfg!(target_os = "macos") {
            concat!("\u{2318}+", $key)
        } else {
            concat!("Ctrl+", $key)
        }
    };
}

/// The one table: the shortcuts card and every tooltip naming a key read it.
pub const SHORTCUTS: [(Shortcut, &str, &str); 7] = [
    (Shortcut::AddProject, with_mod!("N"), "Add a project"),
    (
        Shortcut::Sync,
        with_mod!("Enter"),
        "Sync the selected project",
    ),
    (
        Shortcut::Terminal,
        with_mod!("T"),
        "Open a terminal for the selected project",
    ),
    (
        Shortcut::Refresh,
        "F5",
        "Re-read the selected project from disk",
    ),
    (Shortcut::Escape, "Esc", "Close whatever is on top"),
    (Shortcut::Find, with_mod!("F"), "Filter the packages"),
    (
        Shortcut::ToggleSidebar,
        with_mod!("B"),
        "Collapse or expand the sidebar",
    ),
];

/// The keys for `which`, as the table spells them.
pub fn keys(which: Shortcut) -> &'static str {
    SHORTCUTS
        .iter()
        .find(|(s, _, _)| *s == which)
        .map_or("", |(_, keys, _)| keys)
}

/// The shortcut `key` is, if any. A key a widget already handled (typing in
/// a field) is the widget's -- except Esc, which a focused field takes only
/// to unfocus itself; passing it on closes the edit in one press, not two.
/// Letters are matched by where they are on the keyboard when the layout is
/// not Latin (`to_latin`), so Ctrl+T is Ctrl+T on a Russian layout too.
pub fn shortcut(
    key: &Key,
    physical: Physical,
    modifiers: Modifiers,
    status: Status,
) -> Option<Shortcut> {
    if let Key::Named(Named::Escape) = key {
        return Some(Shortcut::Escape);
    }
    if status == Status::Captured {
        return None;
    }
    let only_command = modifiers.command() && !modifiers.shift() && !modifiers.alt();
    let latin = key.to_latin(physical);
    match key {
        Key::Named(Named::F5) if modifiers.is_empty() => Some(Shortcut::Refresh),
        Key::Named(Named::Enter) if only_command => Some(Shortcut::Sync),
        _ if only_command && latin == Some('n') => Some(Shortcut::AddProject),
        _ if only_command && latin == Some('t') => Some(Shortcut::Terminal),
        _ if only_command && latin == Some('b') => Some(Shortcut::ToggleSidebar),
        _ if only_command && latin == Some('f') => Some(Shortcut::Find),
        _ => None,
    }
}

/// A tooltip naming `which`'s keys only where they act: on the selected
/// project's controls, not on another row the pointer happens to be over.
pub fn hint(text: &str, which: Shortcut, acts_here: bool) -> String {
    if acts_here {
        format!("{text} ({})", keys(which))
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::event::Status;
    use iced::keyboard::{Key, Modifiers, key::Named, key::Physical};

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }

    fn unknown() -> Physical {
        Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified)
    }

    /// For keys whose physical position does not matter to the test.
    fn sc(key: &Key, modifiers: Modifiers, status: Status) -> Option<Shortcut> {
        shortcut(key, unknown(), modifiers, status)
    }

    #[test]
    fn letter_shortcuts_work_on_non_latin_layouts() {
        // On a Russian layout Ctrl+T arrives as "е" on the T key.
        let t = Physical::Code(iced::keyboard::key::Code::KeyT);
        assert_eq!(
            shortcut(&ch("е"), t, Modifiers::COMMAND, Status::Ignored),
            Some(Shortcut::Terminal)
        );
    }

    #[test]
    fn each_key_maps_to_its_shortcut() {
        let cmd = Modifiers::COMMAND;
        let ignored = Status::Ignored;
        assert_eq!(sc(&ch("n"), cmd, ignored), Some(Shortcut::AddProject));
        assert_eq!(sc(&ch("N"), cmd | Modifiers::SHIFT, ignored), None);
        assert_eq!(
            sc(&Key::Named(Named::Enter), cmd, ignored),
            Some(Shortcut::Sync)
        );
        assert_eq!(sc(&ch("t"), cmd, ignored), Some(Shortcut::Terminal));
        assert_eq!(sc(&ch("b"), cmd, ignored), Some(Shortcut::ToggleSidebar));
        assert_eq!(sc(&ch("f"), cmd, ignored), Some(Shortcut::Find));
        assert_eq!(
            sc(&Key::Named(Named::F5), Modifiers::empty(), ignored),
            Some(Shortcut::Refresh)
        );
        assert_eq!(
            sc(&Key::Named(Named::Escape), Modifiers::empty(), ignored),
            Some(Shortcut::Escape)
        );
    }

    #[test]
    fn a_hint_names_its_key_only_where_the_key_acts() {
        // The keys act on the selected project; a hovered other row's
        // tooltip must not teach them as its own.
        assert_eq!(hint("Sync", Shortcut::Sync, false), "Sync");
        assert_eq!(
            hint("Sync", Shortcut::Sync, true),
            format!("Sync ({})", keys(Shortcut::Sync))
        );
    }

    #[test]
    fn letters_need_the_command_modifier() {
        let none = Modifiers::empty();
        assert_eq!(sc(&ch("n"), none, Status::Ignored), None);
        assert_eq!(sc(&Key::Named(Named::Enter), none, Status::Ignored), None);
    }

    #[test]
    fn a_key_a_field_took_is_not_a_shortcut_except_escape() {
        let cmd = Modifiers::COMMAND;
        assert_eq!(sc(&Key::Named(Named::Enter), cmd, Status::Captured), None);
        assert_eq!(
            sc(&Key::Named(Named::F5), Modifiers::empty(), Status::Captured),
            None
        );
        assert_eq!(
            sc(
                &Key::Named(Named::Escape),
                Modifiers::empty(),
                Status::Captured
            ),
            Some(Shortcut::Escape)
        );
    }

    #[test]
    fn the_table_lists_every_shortcut_once() {
        for s in [
            Shortcut::AddProject,
            Shortcut::Sync,
            Shortcut::Terminal,
            Shortcut::Refresh,
            Shortcut::Escape,
            Shortcut::ToggleSidebar,
            Shortcut::Find,
        ] {
            let n = SHORTCUTS.iter().filter(|(t, _, _)| *t == s).count();
            assert_eq!(n, 1, "{s:?}");
        }
        let sync = keys(Shortcut::Sync);
        assert!(sync == "Ctrl+Enter" || sync == "\u{2318}+Enter", "{sync}");
    }
}
