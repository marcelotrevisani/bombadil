//! The single owner of colour, type and spacing for the whole GUI. No other
//! module hardcodes a colour, a font size or a spacing value -- a value the
//! rest of the crate needs belongs here, not inline at the call site.
//!
//! # Where the palette comes from
//!
//! Tolkien describes Tom Bombadil in exactly two colours -- "bright blue his
//! jacket was, and his boots were yellow" -- and he is a keeper of a bounded
//! domain who owns nothing he tends. That is what this application does: it
//! never owns a virtual environment, it keeps an accurate account of one.
//! [`JACKET`] is the interactive colour (selection, focus, links, the
//! primary action). [`BOOT`] means *this needs you* and nothing else --
//! see below. [`INK`] is a dark green-cast ground rather than neutral grey,
//! because the domain is earth, not chrome.
//!
//! # `BOOT` is rationed
//!
//! `BOOT` appears only where the user must look: a venv that does not exist,
//! a dependency declared but not installed, a member that syncing will
//! remove, and the confirm button of a destructive action. If it appears
//! anywhere else it stops meaning anything -- the whole system depends on it
//! staying rare, which is why [`only_the_drifted_state_uses_the_attention_colour`]
//! is the most important test in this module.
//!
//! # The state glyph is the only symbolic vocabulary
//!
//! [`state_glyph`] is the single mapping from [`State`] to a glyph and a
//! colour, used in every row of every pane: sidebar projects, dependency
//! rows, member rows, index rows. Two states rendering identically would
//! make the column meaningless, so every distinct [`State`] gets a distinct
//! glyph *and* a distinct colour -- never one without the other.
//!
//! # Type
//!
//! Two faces, both chosen for a functional reason rather than a mood:
//!
//! - **Atkinson Hyperlegible Next** ([`FONT_PROSE`], [`FONT_PROSE_SEMIBOLD`])
//!   for UI prose and labels. Designed by the Braille Institute to maximise
//!   character disambiguation -- this application is *read*, not skimmed,
//!   and a misread version string or path is a real failure.
//! - **IBM Plex Mono** ([`FONT_DATA`]) for every value the user compares
//!   character by character: versions, constraints, paths, environment
//!   variable keys, index URLs. The distinction is semantic: prose is
//!   Atkinson, data is Plex.
//!
//! Both are SIL OFL 1.1; the licence text for both ships in
//! [`crate::about`], alongside uv's Apache-2.0 notice, per the licence's own
//! requirement that it accompany the font.
//!
//! # Spacing
//!
//! A 4pt base ([`SPACE_BASE`]). Every spacing constant is a whole multiple
//! of it, so nothing in the layout sits off the grid by a stray pixel.
//!
//! # Why parts of this module still look unused
//!
//! `bombadil-gui` is a binary crate, so an unused `pub` item is flagged as
//! dead code with no external crate to excuse it. Task 2 (the sidebar) has
//! now pulled in `SLATE`, `JACKET`, `PARCHMENT`, `BODY`, `DATA`, `SPACE_1`,
//! `SPACE_2`, `SIDEBAR_WIDTH`, `FONT_DATA`, `FONT_PROSE_SEMIBOLD` and
//! [`state_glyph`] itself, so the blanket allow Task 1 left on the whole
//! module is gone. What remains genuinely unused -- `BARK`, `LABEL`,
//! `TITLE`, `DISPLAY`, `SPACE_3`, `SPACE_4` -- carries its own narrow
//! `#[allow(dead_code)]` naming the task that consumes it, so this list
//! shrinks item by item as tasks 3 through 5 land instead of vanishing all
//! at once.

use iced::font::Weight;
use iced::{Color, Font, Theme, color};

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// Ground: near-black with a green cast, not neutral grey.
pub const INK: Color = color!(0x1B1D1A);
/// Raised surfaces -- sidebar, drawer, rows.
#[allow(dead_code, reason = "consumed by task 5 (drawer surface)")]
pub const BARK: Color = color!(0x242723);
/// Primary text on [`INK`]; warm, never pure white.
pub const PARCHMENT: Color = color!(0xE8E4D9);
/// Secondary text, labels, disabled.
pub const SLATE: Color = color!(0x7C8279);
/// Interactive: selection, focus, links, primary action.
pub const JACKET: Color = color!(0x4A82C4);
/// Attention only: drift, absent, a destructive confirm. Rationed -- see the
/// module docs.
pub const BOOT: Color = color!(0xE3B23C);
/// Confirmation: installed, present, success.
pub const MOSS: Color = color!(0x4E7A57);

/// Builds the application's [`Theme`] from the palette above via
/// [`Theme::custom`], the mechanism iced 0.14 provides for exactly this.
pub fn theme() -> Theme {
    Theme::custom(
        "Bombadil".to_string(),
        iced::theme::Palette {
            background: INK,
            text: PARCHMENT,
            primary: JACKET,
            success: MOSS,
            // Both warning and danger read as `BOOT`: the plan makes no
            // distinction between "needs attention" and "about to destroy
            // something" at the palette level -- both get the one rationed
            // colour, and destructive confirms additionally get sole use of
            // it on screen (task 5).
            warning: BOOT,
            danger: BOOT,
        },
    )
}

// ---------------------------------------------------------------------------
// Type scale (points)
// ---------------------------------------------------------------------------

/// Secondary text, disabled labels.
#[allow(dead_code, reason = "consumed by tasks 4-5 (data panes, editors)")]
pub const LABEL: f32 = 11.0;
/// All Plex-set data: versions, constraints, paths, keys, URLs.
pub const DATA: f32 = 12.0;
/// Default prose size.
pub const BODY: f32 = 13.0;
/// Section and pane titles.
#[allow(
    dead_code,
    reason = "consumed by task 3 (tab bar, detail header titles)"
)]
pub const TITLE: f32 = 17.0;
/// The project label in the detail header -- the largest thing on screen.
#[allow(dead_code, reason = "consumed by task 3 (detail header)")]
pub const DISPLAY: f32 = 22.0;

// ---------------------------------------------------------------------------
// Spacing (points, 4pt grid)
// ---------------------------------------------------------------------------

/// The grid unit every other spacing constant is a multiple of.
pub const SPACE_BASE: f32 = 4.0;
/// Tightest spacing: within a row, icon to label.
pub const SPACE_1: f32 = SPACE_BASE;
/// Between related rows.
pub const SPACE_2: f32 = SPACE_BASE * 2.0;
/// Between unrelated groups within a pane.
#[allow(
    dead_code,
    reason = "consumed by tasks 3-5 (panes with multiple groups)"
)]
pub const SPACE_3: f32 = SPACE_BASE * 4.0;
/// Between panes and major sections.
#[allow(dead_code, reason = "consumed by tasks 3-5 (pane and section layout)")]
pub const SPACE_4: f32 = SPACE_BASE * 6.0;

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// Fixed sidebar width, so the dependency table keeps the full remaining
/// width instead of the sidebar's own content deciding the split.
pub const SIDEBAR_WIDTH: f32 = 240.0;

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

/// Atkinson Hyperlegible Next, regular weight -- all UI prose and labels.
pub const FONT_PROSE: Font = Font::with_name("Atkinson Hyperlegible Next");
/// Atkinson Hyperlegible Next, semibold weight -- titles and the selected
/// sidebar row.
pub const FONT_PROSE_SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..FONT_PROSE
};
/// IBM Plex Mono, regular weight -- all data the user compares character by
/// character.
pub const FONT_DATA: Font = Font::with_name("IBM Plex Mono");

/// Embedded font bytes, registered with the renderer in `main.rs` via
/// `.font(...)`. SIL OFL 1.1; licence text carried in `about.rs`.
pub const ATKINSON_REGULAR: &[u8] = include_bytes!("../fonts/AtkinsonHyperlegibleNext-Regular.ttf");
pub const ATKINSON_SEMIBOLD: &[u8] =
    include_bytes!("../fonts/AtkinsonHyperlegibleNext-SemiBold.ttf");
pub const PLEX_MONO_REGULAR: &[u8] = include_bytes!("../fonts/IBMPlexMono-Regular.ttf");

// ---------------------------------------------------------------------------
// The state glyph
// ---------------------------------------------------------------------------

/// The three states the glyph column can report. Shared across every pane --
/// sidebar projects, dependency rows, member rows, index rows -- so the user
/// learns the vocabulary once and it holds everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Present and agreeing: the venv exists, the package is installed at
    /// the declared constraint, the member is installed editable.
    Present,
    /// Known but absent: registered without an environment, declared but
    /// not installed, discovered but deselected.
    Absent,
    /// Present but not agreeing: installed outside the constraint, a member
    /// syncing will remove, a venv whose Python does not satisfy
    /// `requires-python`.
    Drifted,
}

/// One glyph, rendered in one colour. Never construct these ad hoc at a call
/// site -- always go through [`state_glyph`], so the mapping stays in one
/// place.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    pub glyph: char,
    pub colour: Color,
}

/// The one mapping from domain [`State`] to the glyph and colour that
/// represents it. Every pane's row-building code calls this rather than
/// picking a glyph or colour itself.
pub fn state_glyph(state: State) -> Glyph {
    match state {
        State::Present => Glyph {
            glyph: '\u{25cf}', // ● filled
            colour: MOSS,
        },
        State::Absent => Glyph {
            glyph: '\u{25cb}', // ○ hollow
            colour: SLATE,
        },
        State::Drifted => Glyph {
            glyph: '\u{25cd}', // ◍ ringed
            colour: BOOT,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_state_gets_a_distinct_glyph_and_colour() {
        // The glyph column is the application's only symbolic vocabulary, so
        // two states rendering identically would make it meaningless.
        let present = state_glyph(State::Present);
        let absent = state_glyph(State::Absent);
        let drift = state_glyph(State::Drifted);
        assert_ne!(present.glyph, absent.glyph);
        assert_ne!(absent.glyph, drift.glyph);
        assert_ne!(present.glyph, drift.glyph);
        assert_ne!(present.colour, absent.colour);
        assert_ne!(absent.colour, drift.colour);
        assert_ne!(present.colour, drift.colour);
    }

    #[test]
    fn only_the_drifted_state_uses_the_attention_colour() {
        // `boot` is rationed: if it appears where nothing is wrong it stops
        // meaning anything.
        assert_eq!(state_glyph(State::Drifted).colour, BOOT);
        assert_ne!(state_glyph(State::Present).colour, BOOT);
        assert_ne!(state_glyph(State::Absent).colour, BOOT);
    }

    #[test]
    fn the_type_scale_is_strictly_ordered() {
        // A scale whose steps collide gives no hierarchy at all.
        let steps = [LABEL, DATA, BODY, TITLE, DISPLAY];
        for pair in steps.windows(2) {
            assert!(
                pair[0] < pair[1],
                "{:?} must be smaller than {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn spacing_is_a_multiple_of_the_base() {
        for s in [SPACE_1, SPACE_2, SPACE_3, SPACE_4] {
            assert_eq!(s % SPACE_BASE, 0.0, "{s} is off the 4pt grid");
        }
    }
}
