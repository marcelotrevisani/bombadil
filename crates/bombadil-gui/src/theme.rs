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
//! remove, the confirm button of a destructive action, a failed command's
//! status in the drawer, and a config problem in the banner. If it appears
//! anywhere else it stops meaning anything -- the whole system depends on it
//! staying rare, which is why [`only_the_drifted_state_uses_the_attention_colour`]
//! is the most important test in this module.
//!
//! Every one of those six passes the same test: the thing painted `BOOT`
//! does not render at all when nothing is wrong. `banner::view` returns an
//! empty `text` for an empty problem list; `drawer::is_failure` is false for
//! every non-failure status -- including one carrying a user-chosen project
//! label, which is the one way user data could otherwise paint `BOOT` on a
//! healthy sync.
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
//! dead code with no external crate to excuse it. Task 2 (the sidebar) pulled
//! in `SLATE`, `JACKET`, `PARCHMENT`, `BODY`, `DATA`, `SPACE_1`, `SPACE_2`,
//! `SIDEBAR_WIDTH`, `FONT_DATA`, `FONT_PROSE_SEMIBOLD` and [`state_glyph`]
//! itself. Task 3 (the detail header and tab bar) pulled in `DISPLAY`,
//! `TITLE`, `SPACE_3`, `SPACE_4` and `TAB_UNDERLINE`. Task 4 (the data
//! panes) pulled in `BARK` (every dependency/member/index row is a raised
//! surface over the pane's `INK` ground), `LABEL` (the "main"/"group: x"/
//! "extra: x" section headings), `GLYPH_COLUMN_WIDTH`,
//! `DEP_NAME_COLUMN_WIDTH` and `DEP_CONSTRAINT_COLUMN_WIDTH`. Task 5 (the
//! editors, drawer and destructive states) added no new token -- every
//! constant it needed already existed -- but did give `INK` its first
//! consumer outside this module: `context_menu::confirm_view`'s "Delete and
//! recreate" button paints its own text in `INK`, since `PARCHMENT` reads
//! poorly against `BOOT`'s yellow. Nothing in this module remains unused as
//! of Task 5. The branch-review round added [`labeled_value`] (lifted out of
//! `settings_editor`, now shared with `add_project`) and [`labeled_prose`],
//! its sibling for a `match` arm whose value is words rather than a value to
//! compare -- the prose/Plex split is a type decision, so both live here.

use iced::font::Weight;
use iced::{Color, Font, Theme, color};

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// Ground: near-black with a green cast, not neutral grey.
pub const INK: Color = color!(0x1B1D1A);
/// Raised surfaces -- sidebar, drawer, top bar, rows.
pub const BARK: Color = color!(0x242723);
/// Primary text on [`INK`]; warm, never pure white.
pub const PARCHMENT: Color = color!(0xE8E4D9);
/// Secondary text, labels, disabled.
pub const SLATE: Color = color!(0x8B9187);
/// Interactive: selection, focus, links, primary action.
pub const JACKET: Color = color!(0x5E94D6);
/// [`JACKET`] under the pointer. Only ever a hover or pressed state -- never
/// a resting colour, so it carries no meaning of its own.
pub const JACKET_HOVER: Color = color!(0x7CA9E0);
/// Attention only: drift, absent, a destructive confirm. Rationed -- see the
/// module docs.
pub const BOOT: Color = color!(0xE3B23C);
/// Confirmation: installed, present, success.
pub const MOSS: Color = color!(0x5F9169);
/// The hairline that separates one region from the next -- see
/// [`hairline_row`] and this module's "Every region has an edge" docs.
pub const EDGE: Color = color!(0x3A3E37);
/// The scrim behind a modal: [`INK`] at four-fifths, so the application stays
/// legible underneath while reading as unavailable.
pub const SCRIM: Color = Color {
    a: 0.8,
    ..color!(0x11120F)
};

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
pub const LABEL: f32 = 11.0;
/// All Plex-set data: versions, constraints, paths, keys, URLs.
pub const DATA: f32 = 12.0;
/// Default prose size.
pub const BODY: f32 = 13.0;
/// Section and pane titles -- the tab bar's labels, in `app::view`.
pub const TITLE: f32 = 17.0;
/// The project label in the detail header -- the largest thing on screen.
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
/// Between unrelated groups within a pane -- e.g. one tab bar entry to the
/// next in `app::view`.
pub const SPACE_3: f32 = SPACE_BASE * 4.0;
/// Between panes and major sections -- e.g. the detail header, tab bar, pane
/// and drawer stacked in `app::view`.
pub const SPACE_4: f32 = SPACE_BASE * 6.0;

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// Fixed sidebar width, so the dependency table keeps the full remaining
/// width instead of the sidebar's own content deciding the split.
pub const SIDEBAR_WIDTH: f32 = 240.0;
/// Thickness of the selected tab's underline in the tab bar -- the bar's
/// only indicator (no boxes, no pills, no background fill), so the state
/// glyph column stays the application's one symbolic vocabulary.
pub const TAB_UNDERLINE: f32 = 2.0;
/// Height of the application's top bar: its name on the left, the Help menu
/// on the right. 44pt is the smallest comfortable pointer target, and the
/// menu button is the one control in it.
pub const TOPBAR_HEIGHT: f32 = 44.0;
/// Every hairline in the application is exactly this thick. One value, so a
/// divider can never be a different weight in two places.
pub const HAIRLINE: f32 = 1.0;
/// Corner radius. Near-square on purpose: the shell is built from edged
/// panels, and a rounded control inside one reads as a different system.
pub const RADIUS: f32 = 2.0;
/// Width of the marker on the selected sidebar row. Thicker than a
/// [`HAIRLINE`] because it is an indicator, not a boundary -- the same
/// distinction [`TAB_UNDERLINE`] makes in the tab bar.
pub const SELECTED_MARKER: f32 = 3.0;
/// Width of a modal card. Wide enough for a licence's 80-column paragraphs
/// without becoming a second window.
pub const MODAL_WIDTH: f32 = 680.0;
/// Height of a modal's scrollable body, so a long licence scrolls inside the
/// card instead of growing it past the window.
pub const MODAL_BODY_HEIGHT: f32 = 420.0;
/// Tallest an overlaid dialog card may grow before its own body scrolls
/// instead. Without a bound, the add-project form on a small window grew past
/// the bottom of the screen with its buttons below the edge.
pub const OVERLAY_MAX_HEIGHT: f32 = 560.0;
/// Tallest the output drawer may grow when expanded. Beyond this its
/// transcript scrolls: a thousand-line resolution must not push the
/// application off the top of the window.
pub const DRAWER_MAX_HEIGHT: f32 = 220.0;
/// Width of the label column in an index card, so `name`, `url`, `kind` and
/// `auth` line up down the left of it.
pub const INDEX_LABEL_WIDTH: f32 = 72.0;
/// Width of the Preferences card. Wider than a modal: it carries a scope
/// switch, a section list and a panel of fields side by side.
pub const PREFS_WIDTH: f32 = 860.0;
/// Height of the Preferences body, so a long section scrolls inside the card
/// rather than growing it past the window.
pub const PREFS_BODY_HEIGHT: f32 = 420.0;
/// Width of the section list down the left of Preferences.
pub const PREFS_SECTION_WIDTH: f32 = 150.0;
/// Width of the project context menu's panel. Wider than the Help menu: its
/// rows carry a second line naming what a terminal will not activate.
pub const CONTEXT_MENU_WIDTH: f32 = 320.0;
/// Width of the drop-down panel the Help menu opens.
pub const MENU_WIDTH: f32 = 220.0;
/// Width of the state glyph column, fixed at the left of every row in every
/// pane (sidebar, dependency rows, member rows, index rows) so the label
/// that follows starts at the same x regardless of which glyph rendered.
pub const GLYPH_COLUMN_WIDTH: f32 = 20.0;
/// Package name column width in the Dependencies tab, so the constraint and
/// version that follow stack in a fixed vertical column across every group
/// (main, groups, extras) rather than only within one.
pub const DEP_NAME_COLUMN_WIDTH: f32 = 200.0;
/// Declared-constraint column width in the Dependencies tab, immediately
/// after the name column -- the version that follows is what a user
/// compares character by character, and this is what keeps it aligned.
pub const DEP_CONSTRAINT_COLUMN_WIDTH: f32 = 100.0;

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
// Surfaces and edges
// ---------------------------------------------------------------------------

/// A [`BARK`] surface: the sidebar, the top bar, the drawer, a row, a modal
/// card. Pass to `container.style(...)`.
pub fn surface(_theme: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(iced::Background::Color(BARK)),
        ..iced::widget::container::Style::default()
    }
}

/// A [`BARK`] surface with an [`EDGE`] outline: a modal card or a menu panel,
/// which float over the application and need a boundary of their own rather
/// than only a fill.
pub fn panel(_theme: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(iced::Background::Color(BARK)),
        border: iced::Border {
            color: EDGE,
            width: HAIRLINE,
            radius: RADIUS.into(),
        },
        ..iced::widget::container::Style::default()
    }
}

/// A full-width [`HAIRLINE`] in [`EDGE`]: the boundary between two stacked
/// regions.
///
/// A filled `container` rather than iced's `Rule`, matching the tab bar's
/// underline, so every line in the application is drawn one way.
pub fn hairline_row<'a, Message: 'a>() -> iced::Element<'a, Message> {
    iced::widget::container(
        iced::widget::Space::new()
            .width(iced::Length::Fill)
            .height(HAIRLINE),
    )
    .width(iced::Length::Fill)
    .style(|_theme| iced::widget::container::Style {
        background: Some(iced::Background::Color(EDGE)),
        ..iced::widget::container::Style::default()
    })
    .into()
}

/// [`hairline_row`]'s vertical twin: the boundary between two side-by-side
/// regions, such as the sidebar and the detail pane.
pub fn hairline_column<'a, Message: 'a>() -> iced::Element<'a, Message> {
    iced::widget::container(
        iced::widget::Space::new()
            .width(HAIRLINE)
            .height(iced::Length::Fill),
    )
    .height(iced::Length::Fill)
    .style(|_theme| iced::widget::container::Style {
        background: Some(iced::Background::Color(EDGE)),
        ..iced::widget::container::Style::default()
    })
    .into()
}

// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

use iced::widget::button;

/// The primary action: a [`JACKET`] fill with [`INK`] text.
///
/// [`INK`] rather than [`PARCHMENT`] on that fill, and the reason is measured
/// rather than felt: parchment on jacket is 3.1:1, which fails WCAG AA for
/// text at every size this application uses. Ink on jacket is 5.4:1. See
/// [`contrast_ratio`] and the test that holds every pairing to it.
pub fn button_primary(_theme: &Theme, status: button::Status) -> button::Style {
    let fill = match status {
        button::Status::Hovered | button::Status::Pressed => JACKET_HOVER,
        button::Status::Disabled => BARK,
        button::Status::Active => JACKET,
    };
    button::Style {
        background: Some(iced::Background::Color(fill)),
        text_color: if status == button::Status::Disabled {
            SLATE
        } else {
            INK
        },
        border: iced::Border {
            radius: RADIUS.into(),
            ..iced::Border::default()
        },
        ..button::Style::default()
    }
}

/// A secondary action: no fill, an [`EDGE`] outline, [`PARCHMENT`] text.
///
/// Most of the application's buttons are these. Only one control in any given
/// view should be [`button_primary`] -- a screen where everything is primary
/// has no primary.
pub fn button_quiet(_theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: match status {
            button::Status::Hovered | button::Status::Pressed => {
                Some(iced::Background::Color(BARK))
            }
            _ => None,
        },
        text_color: match status {
            button::Status::Disabled => SLATE,
            _ => PARCHMENT,
        },
        border: iced::Border {
            color: EDGE,
            width: HAIRLINE,
            radius: RADIUS.into(),
        },
        ..button::Style::default()
    }
}

/// One option of a segmented choice -- three buttons that are really one
/// setting with three values.
///
/// The selected one carries a [`JACKET`] outline and [`PARCHMENT`] text on a
/// [`BARK`] fill; the rest are quiet. An outline, not a fill, for the same
/// reason the tab bar underlines and the sidebar marks a leading edge:
/// indicators in this application are edges, and a [`JACKET`] fill could not
/// carry legible text anyway.
pub fn button_choice(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| button::Style {
        background: match (selected, status) {
            (true, _) | (_, button::Status::Hovered) | (_, button::Status::Pressed) => {
                Some(iced::Background::Color(BARK))
            }
            _ => None,
        },
        text_color: if selected { PARCHMENT } else { SLATE },
        border: iced::Border {
            color: if selected { JACKET } else { EDGE },
            width: if selected { SELECTED_MARKER } else { HAIRLINE },
            radius: RADIUS.into(),
        },
        ..button::Style::default()
    }
}

/// A destructive confirm: the [`BOOT`] fill this palette rations. Never use
/// for anything else -- see the module docs on what that ration protects.
pub fn button_danger(_theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(iced::Background::Color(match status {
            button::Status::Disabled => BARK,
            _ => BOOT,
        })),
        text_color: if status == button::Status::Disabled {
            SLATE
        } else {
            INK
        },
        border: iced::Border {
            radius: RADIUS.into(),
            ..iced::Border::default()
        },
        ..button::Style::default()
    }
}

/// Bare text that responds to the pointer: a menu entry, a tab. No fill and
/// no border at rest, so the region it sits in keeps its own shape.
///
/// `resting` is the colour when nothing is happening -- [`PARCHMENT`] for a
/// menu entry, [`SLATE`] for an unselected tab -- because those two differ in
/// weight but not in behaviour.
pub fn button_bare(resting: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| button::Style {
        background: match status {
            button::Status::Hovered | button::Status::Pressed => {
                Some(iced::Background::Color(BARK))
            }
            _ => None,
        },
        text_color: match status {
            button::Status::Hovered | button::Status::Pressed => PARCHMENT,
            button::Status::Disabled => resting,
            button::Status::Active => resting,
        },
        border: iced::Border {
            radius: RADIUS.into(),
            ..iced::Border::default()
        },
        ..button::Style::default()
    }
}

// ---------------------------------------------------------------------------
// Contrast
// ---------------------------------------------------------------------------

/// WCAG 2.1 relative luminance of `colour`, ignoring alpha.
#[cfg(test)]
fn relative_luminance(colour: Color) -> f32 {
    fn channel(c: f32) -> f32 {
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(colour.r) + 0.7152 * channel(colour.g) + 0.0722 * channel(colour.b)
}

/// The WCAG 2.1 contrast ratio between two colours, from 1.0 (identical) to
/// 21.0 (black on white).
///
/// Here rather than in a comment because "is this legible" is a question with
/// an answer, and the test below holds every pairing this application
/// actually renders to it. The palette's first draft failed on its most-used
/// colour -- `SLATE` carries nearly every path, version and label in the
/// interface and sat at 4.3:1 against the ground.
///
/// `cfg(test)`: nothing in a running frame asks this question, because the
/// answer is fixed at compile time. It exists so the palette is held to a
/// number rather than to somebody's eye, and the tests below are where that
/// happens.
#[cfg(test)]
pub fn contrast_ratio(a: Color, b: Color) -> f32 {
    let (light, dark) = {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        if la > lb { (la, lb) } else { (lb, la) }
    };
    (light + 0.05) / (dark + 0.05)
}

// ---------------------------------------------------------------------------
// Typographic compounds
// ---------------------------------------------------------------------------

/// A prose label followed by a mono value -- the crate's one shape for
/// "`name:` *value*". It lives here rather than in any one pane because the
/// prose/Plex split it encodes is a type decision and this module owns those;
/// `settings_editor` and `add_project` both render it.
///
/// Generic over the message type only so this module keeps knowing nothing
/// about `app::Message`; both callers instantiate it with that one type.
pub fn labeled_value<'a, Message: 'a>(
    label: &'static str,
    value: String,
) -> iced::Element<'a, Message> {
    iced::widget::row![
        iced::widget::text(label).size(BODY).color(SLATE),
        iced::widget::text(value)
            .font(FONT_DATA)
            .size(DATA)
            .color(SLATE),
    ]
    .spacing(SPACE_1)
    .into()
}

/// [`labeled_value`]'s sibling for a value that is *words*, not a value the
/// user compares character by character: "auto", "from PATH", "alongside each
/// project". The whole prose/Plex split is meaningless if a face is chosen by
/// the variable's name rather than by what the branch actually put in it, so
/// a `match` whose arms mix a path with a phrase must pick per arm.
pub fn labeled_prose<'a, Message: 'a>(
    label: &'static str,
    value: &'static str,
) -> iced::Element<'a, Message> {
    iced::widget::row![
        iced::widget::text(label).size(BODY).color(SLATE),
        iced::widget::text(value)
            .font(FONT_PROSE)
            .size(BODY)
            .color(SLATE),
    ]
    .spacing(SPACE_1)
    .into()
}

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
    fn every_text_colour_is_legible_on_every_ground_it_is_rendered_on() {
        // The user's report was "things are difficult to see", and this is the
        // measurable half of it. `INK` is the application ground and `BARK`
        // every raised surface, so a text colour has to clear WCAG AA (4.5:1)
        // on both -- there is no third ground.
        //
        // The first draft of this palette failed here on the colour it uses
        // most: `SLATE` sets nearly every path, version, label and secondary
        // line in the interface, and sat at 4.3:1 against `INK`.
        for (name, colour) in [
            ("PARCHMENT", PARCHMENT),
            ("SLATE", SLATE),
            ("JACKET", JACKET),
            ("BOOT", BOOT),
        ] {
            for (ground_name, ground) in [("INK", INK), ("BARK", BARK)] {
                let ratio = contrast_ratio(colour, ground);
                assert!(
                    ratio >= 4.5,
                    "{name} on {ground_name} is {ratio:.2}:1, below WCAG AA for text (4.5:1)"
                );
            }
        }
    }

    #[test]
    fn text_on_a_filled_button_is_legible() {
        // `PARCHMENT` on `JACKET` is the obvious choice and it is 3.1:1 --
        // which is why `button_primary` paints its text `INK` instead. The
        // same holds for the rationed `BOOT` fill of a destructive confirm.
        assert!(
            contrast_ratio(INK, JACKET) >= 4.5,
            "the primary button's text is {:.2}:1 on its own fill",
            contrast_ratio(INK, JACKET)
        );
        assert!(
            contrast_ratio(INK, BOOT) >= 4.5,
            "the destructive button's text is {:.2}:1 on its own fill",
            contrast_ratio(INK, BOOT)
        );
        assert!(
            contrast_ratio(PARCHMENT, JACKET) < 4.5,
            "if parchment on jacket ever clears AA, `button_primary`'s ink \
             text is no longer the reason it exists -- revisit it rather than \
             leaving this comment lying"
        );
    }

    #[test]
    fn a_state_glyph_is_legible_as_a_graphic_on_both_grounds() {
        // A glyph is a non-text graphic: WCAG asks 3:1 of it, not 4.5:1. It
        // still has to clear that, or the one symbolic vocabulary this
        // application has is unreadable on the surfaces it renders on.
        for state in [State::Present, State::Absent, State::Drifted] {
            for (ground_name, ground) in [("INK", INK), ("BARK", BARK)] {
                let ratio = contrast_ratio(state_glyph(state).colour, ground);
                assert!(
                    ratio >= 3.0,
                    "the {state:?} glyph is {ratio:.2}:1 on {ground_name}"
                );
            }
        }
    }

    #[test]
    fn the_hairline_is_visible_against_both_grounds() {
        // A divider nobody can see is not a divider. It is a boundary, not
        // text, so the bar is lower -- but it is not zero, and an `EDGE`
        // picked by eye against `INK` alone would vanish on `BARK`.
        for (name, ground) in [("INK", INK), ("BARK", BARK)] {
            let ratio = contrast_ratio(EDGE, ground);
            assert!(ratio >= 1.2, "the hairline is {ratio:.2}:1 on {name}");
        }
    }

    #[test]
    fn spacing_is_a_multiple_of_the_base() {
        for s in [SPACE_1, SPACE_2, SPACE_3, SPACE_4] {
            assert_eq!(s % SPACE_BASE, 0.0, "{s} is off the 4pt grid");
        }
    }
}
