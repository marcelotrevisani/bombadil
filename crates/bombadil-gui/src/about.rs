//! The attribution surface Apache-2.0 section 4 obliges: `bombadil-uv-bin`
//! embeds a real uv binary, so a distributed build of Bombadil must carry
//! uv's name, licence and version somewhere reachable in the UI -- and the
//! licence *text* with it.
//!
//! The text is `bombadil_uv_bin::UV_LICENSE`, compiled into the executable by
//! `include_str!`. That is the copy that always ships: `UV-LICENSE-APACHE`
//! exists in the source tree but is not distributed next to the binary, so a
//! footer telling the user to go read that file pointed at nothing. An
//! expandable region here is the whole of the fix -- the obligation is that
//! the text is reachable, not that it is beautiful.

use crate::app::Message;

/// A short notice naming uv, its licence and the exact version bundled in
/// this build. Rendered as a footer line by [`view`], above the control that
/// reveals the full text.
///
/// `bombadil_uv_bin::BUNDLED_VERSION` is only known at `bombadil-uv-bin`'s own
/// build time (its build script sets the env var `env!` reads), so it cannot
/// be spliced into a `concat!` literal here -- this crate's build never sees
/// that env var. A `OnceLock<String>` formats it once per process instead.
pub fn attribution() -> &'static str {
    use std::sync::OnceLock;
    static TEXT: OnceLock<String> = OnceLock::new();
    TEXT.get_or_init(|| {
        format!(
            "Bundles uv {}, licensed under the Apache License, Version 2.0 (Apache-2.0).",
            bombadil_uv_bin::BUNDLED_VERSION
        )
    })
}

/// uv's full licence text, as compiled into this executable.
///
/// A function rather than a re-exported `const` so this module is the single
/// place the UI asks for the licence, and so the test below is testing the
/// thing `view` actually renders.
pub fn license() -> &'static str {
    bombadil_uv_bin::UV_LICENSE
}

/// The footer: the attribution line, a control to reveal the bundled
/// licence, and the licence itself while `expanded`.
///
/// Deliberately dumb, like every other `view` in this crate: the state lives
/// on `App` and the toggle is a message, so what is worth testing (that the
/// text is reachable at all, and that the toggle flips) is tested without a
/// window.
pub fn view<'a>(expanded: bool) -> iced::Element<'a, Message> {
    let label = if expanded {
        "Hide bundled licence"
    } else {
        "Bundled licence (uv, Apache-2.0)"
    };
    let mut column = iced::widget::column![
        iced::widget::text(attribution()).size(11),
        iced::widget::button(iced::widget::text(label).size(11))
            .on_press(Message::AboutLicenseToggled),
    ]
    .spacing(4);

    if expanded {
        column = column.push(iced::widget::scrollable(
            iced::widget::text(license()).size(11),
        ));
    }

    column.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_attribution_names_uv_and_its_licence() {
        // Apache-2.0 section 4 obliges a distributed work to carry the notice.
        // Embedding uv without this is a licence violation, not a missing nicety.
        let text = attribution();
        assert!(text.contains("uv"), "got {text}");
        assert!(
            text.contains("Apache-2.0") || text.contains("Apache License"),
            "got {text}"
        );
    }

    #[test]
    fn the_attribution_names_the_bundled_version() {
        // A notice that does not say which version was shipped is not much of
        // a notice, and the version is already a compile-time constant.
        let text = attribution();
        assert!(
            text.contains(bombadil_uv_bin::BUNDLED_VERSION),
            "must name the bundled version; got {text}"
        );
    }

    #[test]
    fn the_attribution_does_not_send_the_user_to_a_file_that_is_not_shipped() {
        // `UV-LICENSE-APACHE` is in the source tree and nowhere near the
        // installed binary. Pointing at it satisfied nobody: the user cannot
        // open it, and the obligation is to *provide* the text.
        let text = attribution();
        assert!(
            !text.contains("UV-LICENSE"),
            "the notice must not reference an undistributed file; got {text}"
        );
    }

    #[test]
    fn the_full_licence_text_is_reachable_from_the_ui() {
        // The point of the fix: `bombadil_uv_bin::UV_LICENSE` was compiled
        // into the executable with no consumer at all. `view` renders exactly
        // what this returns.
        let text = license();
        assert!(text.contains("Apache License"), "got {text}");
        assert!(
            text.contains("TERMS AND CONDITIONS"),
            "the full text must ship, not just a title line"
        );
    }
}
