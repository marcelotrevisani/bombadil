//! The attribution texts Apache-2.0 section 4 obliges: `bombadil-uv-bin`
//! embeds a real uv binary, so a distributed build of Bombadil must carry
//! uv's name, licence and version somewhere reachable in the UI -- and the
//! licence *text* with it.
//!
//! This module owns the *text*; `shell` owns where it appears. It used to be
//! an expandable footer at the bottom of every frame; it is now Help >
//! Bundled licences, which satisfies the same obligation -- reachable, not
//! unavoidable -- without spending the base of the window on boilerplate.
//!
//! The text is `bombadil_uv_bin::UV_LICENSE`, compiled into the executable by
//! `include_str!`. That is the copy that always ships: `UV-LICENSE-APACHE`
//! exists in the source tree but is not distributed next to the binary, so a
//! footer telling the user to go read that file pointed at nothing. An
//! obligation is that the text is reachable, not that it is beautiful.
//!
//! # The two bundled fonts
//!
//! `theme.rs` embeds Atkinson Hyperlegible Next and IBM Plex Mono directly
//! into the executable with `include_bytes!`. Both are SIL Open Font
//! License, Version 1.1, which -- like Apache-2.0 -- requires the licence to
//! accompany the font it covers. Shipping the font without it is the same
//! class of problem as shipping uv without its notice, so this module
//! carries both OFL texts the same way it carries uv's: compiled in with
//! `include_str!`, reachable from the same menu entry.

/// A short notice naming uv, its licence and the exact version bundled in
/// this build. Rendered in the About modal, above the control that opens the
/// full text.
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
/// thing the Licences modal actually renders.
pub fn license() -> &'static str {
    bombadil_uv_bin::UV_LICENSE
}

/// A short notice naming both bundled fonts and their licence, mirroring
/// [`attribution`] for uv.
pub fn font_attribution() -> &'static str {
    "Uses Atkinson Hyperlegible Next and IBM Plex Mono, both licensed under \
     the SIL Open Font License, Version 1.1 (OFL-1.1)."
}

/// Atkinson Hyperlegible Next's full OFL 1.1 text, compiled into this
/// executable from the same file `theme.rs` ships the font bytes alongside.
pub fn atkinson_license() -> &'static str {
    include_str!("../fonts/OFL-AtkinsonHyperlegibleNext.txt")
}

/// IBM Plex Mono's full OFL 1.1 text, compiled into this executable from the
/// same file `theme.rs` ships the font bytes alongside.
pub fn plex_license() -> &'static str {
    include_str!("../fonts/OFL-IBMPlexMono.txt")
}

/// Lucide's licence, as published: ISC for Lucide, MIT for the icons it
/// derives from Feather. Both halves bind, so the whole file ships.
pub fn lucide_license() -> &'static str {
    include_str!("../LUCIDE-LICENSE")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lucide_notice_carries_both_its_licences() {
        // ISC for Lucide, and MIT for the Feather icons it derives from --
        // several of which (terminal, check, trash-2) ship here.
        let text = lucide_license();
        assert!(text.contains("ISC License"), "the ISC notice is missing");
        assert!(
            text.contains("Cole Bemis"),
            "the Feather MIT notice is missing"
        );
    }

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
        // into the executable with no consumer at all. `shell::modal_layer`
        // renders exactly what this returns.
        let text = license();
        assert!(text.contains("Apache License"), "got {text}");
        assert!(
            text.contains("TERMS AND CONDITIONS"),
            "the full text must ship, not just a title line"
        );
    }

    #[test]
    fn the_font_attribution_names_both_fonts_and_ofl() {
        // OFL 1.1, like Apache-2.0 section 4, obliges the notice to
        // accompany the font. Embedding the bytes without this is the same
        // class of violation as the uv case above.
        let text = font_attribution();
        assert!(text.contains("Atkinson Hyperlegible Next"), "got {text}");
        assert!(text.contains("IBM Plex Mono"), "got {text}");
        assert!(
            text.contains("OFL-1.1") || text.contains("Open Font License"),
            "got {text}"
        );
    }

    #[test]
    fn both_font_licence_texts_are_reachable_from_the_ui() {
        // Mirrors `the_full_licence_text_is_reachable_from_the_ui`: the
        // Licences modal renders exactly what these return, so a font shipped
        // without a consumer for its licence text would be caught the same
        // way uv's was.
        let atkinson = atkinson_license();
        assert!(atkinson.contains("SIL OPEN FONT LICENSE"), "got {atkinson}");
        assert!(atkinson.contains("Version 1.1"), "got {atkinson}");

        let plex = plex_license();
        assert!(plex.contains("SIL OPEN FONT LICENSE"), "got {plex}");
        assert!(plex.contains("Version 1.1"), "got {plex}");
    }

    #[test]
    fn the_font_licences_are_not_the_same_text_twice() {
        // Each font has its own copyright holder in the preamble; shipping
        // one file twice under two names would misattribute the other.
        assert_ne!(atkinson_license(), plex_license());
    }
}
