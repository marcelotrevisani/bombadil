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
//! `jacket` is the interactive colour (selection, focus, links, the
//! primary action). `boot` means *this needs you* and nothing else --
//! see below. `ink` is a dark green-cast ground rather than neutral grey,
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

/// Every colour the interface uses, by role. Field names keep their meaning
/// across palettes: `ink` is always the ground and `parchment` always the
/// main text, so in [`LIGHT`] `ink` is the light one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// The application ground.
    pub ink: Color,
    /// Raised surfaces: sidebar, top bar, drawer, rows, modal cards.
    pub bark: Color,
    /// Main text.
    pub parchment: Color,
    /// Secondary text: paths, versions, labels.
    pub slate: Color,
    /// Interactive: selection, focus, links, the primary action.
    pub jacket: Color,
    /// The primary action under the pointer.
    pub jacket_hover: Color,
    /// Rationed attention: drift and destructive confirms only.
    pub boot: Color,
    /// Healthy.
    pub moss: Color,
    /// Hairlines and outlines.
    pub edge: Color,
    /// The backdrop behind a modal.
    pub scrim: Color,
}

/// The original palette: near-black ground with a green cast, warm
/// parchment text.
pub const DARK: Palette = Palette {
    ink: color!(0x1B1D1A),
    bark: color!(0x242723),
    parchment: color!(0xE8E4D9),
    slate: color!(0x8B9187),
    jacket: color!(0x5E94D6),
    jacket_hover: color!(0x7CA9E0),
    boot: color!(0xE3B23C),
    moss: color!(0x5F9169),
    edge: color!(0x3A3E37),
    scrim: Color {
        a: 0.8,
        ..color!(0x11120F)
    },
};

/// Parchment: the dark palette inverted -- warm paper ground, ink text --
/// with every accent darkened until it clears AA on both light grounds.
pub const LIGHT: Palette = Palette {
    ink: color!(0xF5F1E6),
    bark: color!(0xEBE6D8),
    parchment: color!(0x1B1D1A),
    slate: color!(0x5C6258),
    jacket: color!(0x2E66A8),
    jacket_hover: color!(0x245489),
    boot: color!(0x8A5F00),
    moss: color!(0x3B6B46),
    edge: color!(0xCFC8B6),
    scrim: Color {
        a: 0.4,
        ..color!(0x1B1D1A)
    },
};

/// A palette entry named without choosing a palette: what a call site holds
/// when the colour depends on the theme it renders in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swatch {
    Ink,
    Bark,
    Parchment,
    Slate,
    Boot,
    Moss,
    Edge,
    Scrim,
}

impl Palette {
    pub fn get(&self, swatch: Swatch) -> Color {
        match swatch {
            Swatch::Ink => self.ink,
            Swatch::Bark => self.bark,
            Swatch::Parchment => self.parchment,
            Swatch::Slate => self.slate,
            Swatch::Boot => self.boot,
            Swatch::Moss => self.moss,
            Swatch::Edge => self.edge,
            Swatch::Scrim => self.scrim,
        }
    }
}

/// The palette `theme` was built from.
///
/// Keyed on `is_dark` because there are exactly two palettes, one of each.
/// A third palette needs a real key -- the theme's name, say.
pub fn palette(theme: &Theme) -> &'static Palette {
    if theme.extended_palette().is_dark {
        &DARK
    } else {
        &LIGHT
    }
}

/// The application's [`Theme`] for `mode`. `Mode::None` -- the OS gave no
/// answer -- is dark, which is what Bombadil looked like before this existed.
pub fn theme(mode: iced::theme::Mode) -> Theme {
    let (name, p) = match mode {
        iced::theme::Mode::Light => ("Bombadil Light", &LIGHT),
        _ => ("Bombadil", &DARK),
    };
    Theme::custom(
        name.to_string(),
        iced::theme::Palette {
            background: p.ink,
            text: p.parchment,
            primary: p.jacket,
            success: p.moss,
            // Warning and danger are both `boot`: one rationed colour for
            // "needs attention" and "about to destroy something".
            warning: p.boot,
            danger: p.boot,
        },
    )
}

// ---------------------------------------------------------------------------
// Type scale (points)
// ---------------------------------------------------------------------------

/// Secondary text, disabled labels.
pub const LABEL: f32 = 12.0;
/// All Plex-set data: versions, constraints, paths, keys, URLs.
pub const DATA: f32 = 13.0;
/// Default prose size.
pub const BODY: f32 = 14.0;
/// Section and pane titles -- the tab bar's labels, in `app::view`.
pub const TITLE: f32 = 18.0;
/// The project label in the detail header -- the largest thing on screen.
pub const DISPLAY: f32 = 26.0;

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

/// The grab strip on the sidebar's edge.
pub const HANDLE_WIDTH: f32 = 6.0;
/// The collapsed sidebar: one icon wide.
pub const RAIL_WIDTH: f32 = 44.0;
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
/// Corner radius of a control: buttons, fields, rows. Graded by role rather
/// than one value everywhere -- a control sits inside a surface, so its
/// corner is the tighter of the two, and a 2pt square everywhere read as
/// dated rather than deliberate.
pub const RADIUS: f32 = 6.0;
/// Corner radius of a surface that holds controls: the summary card, a modal,
/// a menu panel.
pub const RADIUS_SURFACE: f32 = 10.0;
/// A fully round end: status badges, which are labels rather than controls.
pub const RADIUS_PILL: f32 = 999.0;
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
/// Height of one environment row in the sidebar tree.
///
/// Explicit, not derived from its content: the row carries a `Fill`-height
/// marker down its leading edge, and a `Fill` child inside a `Shrink` row
/// promotes the whole row to `Fill` -- several of those in one column then
/// divide the space between them and collapse to nothing. A fixed height
/// gives the marker something real to fill.
pub const TREE_ROW_HEIGHT: f32 = 38.0;
/// Height of a project row in the sidebar: its label over its version and
/// branch, at [`BODY`] and [`DATA`], plus padding.
pub const PROJECT_ROW_HEIGHT: f32 = 52.0;
/// Width of the sidebar's disclosure triangle, so every project's glyph and
/// label start at the same x whether it is open or closed.
pub const DISCLOSURE_WIDTH: f32 = 16.0;
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

use iced::widget::text;

/// Text in `swatch`, resolved against whichever palette is rendering.
pub fn tinted(swatch: Swatch) -> impl Fn(&Theme) -> text::Style {
    move |theme| text::Style {
        color: Some(palette(theme).get(swatch)),
    }
}

/// Secondary text: paths, versions, labels. The most used colour in the app.
pub fn text_muted(theme: &Theme) -> text::Style {
    tinted(Swatch::Slate)(theme)
}

/// Rationed attention: drift, failures.
pub fn text_warning(theme: &Theme) -> text::Style {
    tinted(Swatch::Boot)(theme)
}

/// A plain background fill in `swatch`, for a container that is only a
/// coloured block (a marker bar, a row background).
pub fn fill(swatch: Swatch) -> impl Fn(&Theme) -> iced::widget::container::Style {
    move |theme| iced::widget::container::Style {
        background: Some(iced::Background::Color(palette(theme).get(swatch))),
        ..iced::widget::container::Style::default()
    }
}

// Surfaces and edges
// ---------------------------------------------------------------------------

/// A `bark` surface: the sidebar, the top bar, the drawer, a row, a modal
/// card. Pass to `container.style(...)`.
pub fn surface(theme: &Theme) -> iced::widget::container::Style {
    let p = palette(theme);
    iced::widget::container::Style {
        background: Some(iced::Background::Color(p.bark)),
        ..iced::widget::container::Style::default()
    }
}

/// A `bark` surface with an `edge` outline: a modal card or a menu panel,
/// which float over the application and need a boundary of their own rather
/// than only a fill.
pub fn panel(theme: &Theme) -> iced::widget::container::Style {
    let p = palette(theme);
    iced::widget::container::Style {
        background: Some(iced::Background::Color(p.bark)),
        border: iced::Border {
            color: p.edge,
            width: HAIRLINE,
            radius: RADIUS_SURFACE.into(),
        },
        ..iced::widget::container::Style::default()
    }
}

/// A full-width [`HAIRLINE`] in `edge`: the boundary between two stacked
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
    .style(fill(Swatch::Edge))
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
    .style(fill(Swatch::Edge))
    .into()
}

/// A small outlined label in a pill: a state a row is in ("default"), not
/// something to press -- hence the round ends no control has.
pub fn badge<'a, Message: 'a>(label: &'a str) -> iced::Element<'a, Message> {
    iced::widget::container(iced::widget::text(label).font(FONT_PROSE).size(LABEL))
        .padding([0.0, SPACE_2])
        .style(|t| {
            let p = palette(t);
            iced::widget::container::Style {
                text_color: Some(p.jacket),
                border: iced::Border {
                    color: p.jacket,
                    width: HAIRLINE,
                    radius: RADIUS_PILL.into(),
                },
                ..iced::widget::container::Style::default()
            }
        })
        .into()
}

/// Every text field: a `bark` well with an `edge` outline that turns `jacket`
/// under focus, so where typing goes is visible without a caret hunt.
pub fn text_field(
    theme: &Theme,
    status: iced::widget::text_input::Status,
) -> iced::widget::text_input::Style {
    use iced::widget::text_input::Status;
    let p = palette(theme);
    iced::widget::text_input::Style {
        background: iced::Background::Color(match status {
            Status::Disabled => p.ink,
            _ => p.bark,
        }),
        border: iced::Border {
            color: match status {
                Status::Focused { .. } => p.jacket,
                Status::Hovered => p.slate,
                _ => p.edge,
            },
            width: HAIRLINE,
            radius: RADIUS.into(),
        },
        icon: p.slate,
        placeholder: p.slate,
        value: p.parchment,
        selection: Color {
            a: 0.35,
            ..p.jacket
        },
    }
}

// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

use iced::widget::button;

/// The primary action: a `jacket` fill with `ink` text.
///
/// `ink` rather than `parchment` on that fill, and the reason is measured
/// rather than felt: parchment on jacket is 3.1:1, which fails WCAG AA for
/// text at every size this application uses. Ink on jacket is 5.4:1. See
/// [`contrast_ratio`] and the test that holds every pairing to it.
pub fn button_primary(theme: &Theme, status: button::Status) -> button::Style {
    let p = palette(theme);
    let fill = match status {
        button::Status::Hovered | button::Status::Pressed => Some(p.jacket_hover),
        // Disabled reads the same in every role: muted text, no fill.
        button::Status::Disabled => None,
        button::Status::Active => Some(p.jacket),
    };
    button::Style {
        background: fill.map(iced::Background::Color),
        text_color: if status == button::Status::Disabled {
            p.slate
        } else {
            p.ink
        },
        border: iced::Border {
            radius: RADIUS.into(),
            ..iced::Border::default()
        },
        ..button::Style::default()
    }
}

/// Toolbar and row controls: no border at rest, so a row of them reads as
/// part of the row rather than as a row of buttons. `slate` is kept for
/// disabled -- resting in it too made a dead control look live.
pub fn button_quiet(theme: &Theme, status: button::Status) -> button::Style {
    let p = palette(theme);
    button::Style {
        background: match status {
            button::Status::Hovered | button::Status::Pressed => {
                Some(iced::Background::Color(p.bark))
            }
            _ => None,
        },
        text_color: match status {
            button::Status::Disabled => p.slate,
            _ => p.parchment,
        },
        border: iced::Border {
            color: p.edge,
            width: match status {
                button::Status::Hovered | button::Status::Pressed => HAIRLINE,
                _ => 0.0,
            },
            radius: RADIUS.into(),
        },
        ..button::Style::default()
    }
}

/// Every other real action on a surface: a `slate` outline, `parchment`
/// text, a `bark` fill under the pointer. Visibly a button, unlike quiet;
/// never competing with the one primary.
pub fn button_secondary(theme: &Theme, status: button::Status) -> button::Style {
    let p = palette(theme);
    button::Style {
        background: match status {
            button::Status::Hovered | button::Status::Pressed => {
                Some(iced::Background::Color(p.bark))
            }
            _ => None,
        },
        text_color: match status {
            button::Status::Disabled => p.slate,
            _ => p.parchment,
        },
        border: iced::Border {
            color: if status == button::Status::Disabled {
                p.edge
            } else {
                p.slate
            },
            width: HAIRLINE,
            radius: RADIUS.into(),
        },
        ..button::Style::default()
    }
}

/// One option of a segmented choice -- three buttons that are really one
/// setting with three values.
///
/// The selected one carries a `jacket` outline and `parchment` text on a
/// `bark` fill; the rest are quiet. An outline, not a fill, for the same
/// reason the tab bar underlines and the sidebar marks a leading edge:
/// indicators in this application are edges, and a `jacket` fill could not
/// carry legible text anyway.
pub fn button_choice(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let p = palette(theme);
        button::Style {
            background: match (selected, status) {
                (true, _) | (_, button::Status::Hovered) | (_, button::Status::Pressed) => {
                    Some(iced::Background::Color(p.bark))
                }
                _ => None,
            },
            text_color: if selected { p.parchment } else { p.slate },
            border: iced::Border {
                color: if selected { p.jacket } else { p.edge },
                width: if selected { SELECTED_MARKER } else { HAIRLINE },
                radius: RADIUS.into(),
            },
            ..button::Style::default()
        }
    }
}

/// A destructive confirm: the `boot` fill this palette rations. Never use
/// for anything else -- see the module docs on what that ration protects.
pub fn button_danger(theme: &Theme, status: button::Status) -> button::Style {
    let p = palette(theme);
    button::Style {
        background: match status {
            button::Status::Disabled => None,
            _ => Some(iced::Background::Color(p.boot)),
        },
        text_color: if status == button::Status::Disabled {
            p.slate
        } else {
            p.ink
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
/// `resting` is the colour when nothing is happening -- `parchment` for a
/// menu entry, `slate` for an unselected tab -- because those two differ in
/// weight but not in behaviour.
pub fn button_bare(resting: Swatch) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let p = palette(theme);
        button::Style {
            background: match status {
                button::Status::Hovered | button::Status::Pressed => {
                    Some(iced::Background::Color(p.bark))
                }
                _ => None,
            },
            text_color: match status {
                button::Status::Hovered | button::Status::Pressed => p.parchment,
                button::Status::Disabled | button::Status::Active => p.get(resting),
            },
            border: iced::Border {
                radius: RADIUS.into(),
                ..iced::Border::default()
            },
            ..button::Style::default()
        }
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
        iced::widget::text(label).size(BODY).style(text_muted),
        iced::widget::text(value)
            .font(FONT_DATA)
            .size(DATA)
            .style(text_muted),
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
        iced::widget::text(label).size(BODY).style(text_muted),
        iced::widget::text(value)
            .font(FONT_PROSE)
            .size(BODY)
            .style(text_muted),
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
///
/// Declared from least to most attention needed: the derived order is what
/// `detail::worst` takes the maximum of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
    pub swatch: Swatch,
}

/// The one mapping from domain [`State`] to the glyph and colour that
/// represents it. Every pane's row-building code calls this rather than
/// picking a glyph or colour itself.
pub fn state_glyph(state: State) -> Glyph {
    match state {
        State::Present => Glyph {
            glyph: '\u{25cf}', // ● filled
            swatch: Swatch::Moss,
        },
        State::Absent => Glyph {
            glyph: '\u{25cb}', // ○ hollow
            swatch: Swatch::Slate,
        },
        State::Drifted => Glyph {
            glyph: '\u{25cd}', // ◍ ringed
            swatch: Swatch::Boot,
        },
    }
}

/// What a [`State`] glyph means, in words, for its tooltip.
pub fn state_meaning(state: State) -> &'static str {
    match state {
        State::Present => "Present and up to date",
        State::Absent => "Not there yet: no environment, or not installed",
        State::Drifted => "There, but out of step with what is declared",
    }
}

/// The one tooltip look: a `panel` card in the label size.
pub fn tip<'a, M: 'a>(
    content: impl Into<iced::Element<'a, M>>,
    text: impl Into<String>,
    position: iced::widget::tooltip::Position,
) -> iced::Element<'a, M> {
    iced::widget::tooltip(
        content,
        iced::widget::container(iced::widget::text(text.into()).size(LABEL))
            .padding(SPACE_1)
            .style(panel),
        position,
    )
    .into()
}

/// `content` as is when nothing runs; otherwise with the tooltip saying what
/// it waits for -- a disabled button with no reason reads as broken.
pub fn wait_tip<'a, M: 'a>(
    content: impl Into<iced::Element<'a, M>>,
    waiting: Option<&str>,
) -> iced::Element<'a, M> {
    match waiting {
        Some(label) => tip(
            content,
            format!("Wait for {label} to finish"),
            iced::widget::tooltip::Position::Bottom,
        ),
        None => content.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_says_what_it_means() {
        // The glyphs are colour plus shape; the tooltip is the words.
        let all = [State::Present, State::Absent, State::Drifted].map(state_meaning);
        for (i, a) in all.iter().enumerate() {
            assert!(!a.trim().is_empty());
            assert!(!all[i + 1..].contains(a), "{a} is said twice");
        }
    }

    #[test]
    fn each_state_gets_a_distinct_glyph_and_swatch() {
        let all = [State::Present, State::Absent, State::Drifted].map(state_glyph);
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a.glyph, b.glyph);
                assert_ne!(a.swatch, b.swatch);
            }
        }
    }

    #[test]
    fn only_the_drifted_state_uses_the_attention_colour() {
        assert_eq!(state_glyph(State::Drifted).swatch, Swatch::Boot);
        assert_ne!(state_glyph(State::Present).swatch, Swatch::Boot);
        assert_ne!(state_glyph(State::Absent).swatch, Swatch::Boot);
    }

    #[test]
    fn a_state_glyph_is_legible_as_a_graphic_in_both_palettes() {
        for (mode, p) in PALETTES {
            for state in [State::Present, State::Absent, State::Drifted] {
                for (ground_name, ground) in [("ink", p.ink), ("bark", p.bark)] {
                    let ratio = contrast_ratio(p.get(state_glyph(state).swatch), ground);
                    assert!(
                        ratio >= 3.0,
                        "{mode}: the {state:?} glyph is {ratio:.2}:1 on {ground_name}"
                    );
                }
            }
        }
    }

    #[test]
    fn text_styles_follow_the_theme() {
        // The point of the whole change: the same style fn gives a different
        // colour per palette.
        let dark = theme(iced::theme::Mode::Dark);
        let light = theme(iced::theme::Mode::Light);
        assert_eq!(text_muted(&dark).color, Some(DARK.slate));
        assert_eq!(text_muted(&light).color, Some(LIGHT.slate));
        assert_eq!(tinted(Swatch::Boot)(&light).color, Some(LIGHT.boot));
    }

    /// The text and background a style actually paints, with an absent
    /// background meaning the `ground` it sits on.
    fn painted(style: button::Style, ground: Color) -> (Color, Color) {
        let bg = match style.background {
            Some(iced::Background::Color(c)) => c,
            _ => ground,
        };
        (style.text_color, bg)
    }

    #[test]
    fn a_disabled_button_does_not_look_like_an_enabled_one() {
        // Legible is not enough: a disabled control that renders exactly
        // like an enabled one gets clicked, and nothing happens. Choice is
        // left out on purpose -- its disabled state *is* "already selected".
        type Role = fn(&Theme, button::Status) -> button::Style;
        for mode in [iced::theme::Mode::Dark, iced::theme::Mode::Light] {
            let t = theme(mode);
            for (role, style) in [
                ("primary", button_primary as Role),
                ("secondary", button_secondary),
                ("quiet", button_quiet),
                ("danger", button_danger),
            ] {
                assert_ne!(
                    style(&t, button::Status::Active).text_color,
                    style(&t, button::Status::Disabled).text_color,
                    "{mode:?}: a disabled {role} button looks enabled"
                );
            }
        }
    }

    #[test]
    fn every_role_is_legible_in_every_state() {
        use button::Status::*;
        type Role = Box<dyn Fn(&Theme, button::Status) -> button::Style>;
        for (mode_name, mode) in [
            ("dark", iced::theme::Mode::Dark),
            ("light", iced::theme::Mode::Light),
        ] {
            let t = theme(mode);
            let p = palette(&t);
            let roles: [(&str, Role); 6] = [
                ("primary", Box::new(button_primary)),
                ("secondary", Box::new(button_secondary)),
                ("quiet", Box::new(button_quiet)),
                ("danger", Box::new(button_danger)),
                ("choice/on", Box::new(button_choice(true))),
                ("choice/off", Box::new(button_choice(false))),
            ];
            for (role, style) in &roles {
                for status in [Active, Hovered, Pressed, Disabled] {
                    for (ground_name, ground) in [("ink", p.ink), ("bark", p.bark)] {
                        let (fg, bg) = painted(style(&t, status), ground);
                        let ratio = contrast_ratio(fg, bg);
                        assert!(
                            ratio >= 4.5,
                            "{mode_name}: {role} {status:?} on {ground_name} is {ratio:.2}:1"
                        );
                    }
                }
            }
        }
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

    const PALETTES: [(&str, &Palette); 2] = [("dark", &DARK), ("light", &LIGHT)];

    #[test]
    fn every_text_colour_is_legible_on_every_ground_in_both_palettes() {
        // `slate` sets nearly every path, version and label; it failed this
        // once already in the dark palette's first draft.
        // `moss` is not here: it marks glyphs (3:1), never sets text, and
        // dark moss is 4.1:1 on bark.
        for (mode, p) in PALETTES {
            for (name, colour) in [
                ("parchment", p.parchment),
                ("slate", p.slate),
                ("jacket", p.jacket),
                ("boot", p.boot),
            ] {
                for (ground_name, ground) in [("ink", p.ink), ("bark", p.bark)] {
                    let ratio = contrast_ratio(colour, ground);
                    assert!(
                        ratio >= 4.5,
                        "{mode}: {name} on {ground_name} is {ratio:.2}:1, below WCAG AA (4.5:1)"
                    );
                }
            }
        }
    }

    #[test]
    fn text_on_a_filled_button_is_legible_in_both_palettes() {
        // Filled buttons paint their text `ink`: parchment on jacket is
        // under AA in both palettes.
        for (mode, p) in PALETTES {
            for (name, fill) in [
                ("jacket", p.jacket),
                ("jacket_hover", p.jacket_hover),
                ("boot", p.boot),
            ] {
                let ratio = contrast_ratio(p.ink, fill);
                assert!(ratio >= 4.5, "{mode}: ink on {name} is {ratio:.2}:1");
            }
            assert!(
                contrast_ratio(p.parchment, p.jacket) < 4.5,
                "{mode}: parchment on jacket clears AA now -- revisit why filled buttons use ink"
            );
        }
    }

    #[test]
    fn the_hairline_is_visible_in_both_palettes() {
        for (mode, p) in PALETTES {
            for (name, ground) in [("ink", p.ink), ("bark", p.bark)] {
                let ratio = contrast_ratio(p.edge, ground);
                assert!(
                    ratio >= 1.2,
                    "{mode}: the hairline is {ratio:.2}:1 on {name}"
                );
            }
        }
    }

    #[test]
    fn the_theme_built_for_a_mode_reads_back_as_that_palette() {
        // `palette()` keys on `is_dark`; if `theme()` ever builds a light
        // palette iced classifies as dark, every style reads the wrong colours.
        assert_eq!(palette(&theme(iced::theme::Mode::Dark)), &DARK);
        assert_eq!(palette(&theme(iced::theme::Mode::Light)), &LIGHT);
        assert_eq!(palette(&theme(iced::theme::Mode::None)), &DARK);
    }

    #[test]
    fn the_dark_palette_is_unchanged() {
        assert_eq!(DARK.ink, color!(0x1B1D1A));
        assert_eq!(DARK.parchment, color!(0xE8E4D9));
        assert_eq!(DARK.slate, color!(0x8B9187));
        assert_eq!(DARK.jacket, color!(0x5E94D6));
    }

    #[test]
    fn spacing_is_a_multiple_of_the_base() {
        for s in [SPACE_1, SPACE_2, SPACE_3, SPACE_4] {
            assert_eq!(s % SPACE_BASE, 0.0, "{s} is off the 4pt grid");
        }
    }
}
