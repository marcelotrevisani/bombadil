//! Lucide icons (https://lucide.dev), drawn from Lucide's own icon font as
//! published in `lucide-static` 1.49.0 -- subset to the glyphs Bombadil uses.
//! ISC, with MIT for the Feather-derived ones; both notices ship in
//! `LUCIDE-LICENSE` and Help > Bundled licences.
//!
//! A font, not SVG: iced's SVG support pulls in resvg's text shaping
//! (rustybuzz), which `cargo deny` flags as unmaintained (RUSTSEC-2026-0206).
//! A glyph is tinted like any text, so an icon follows light and dark exactly
//! as text does. The busy spinner is the one icon that turns, so it is drawn
//! on a canvas instead.
//!
//! `assets/lucide-icons.ttf` is made from `lucide-static@1.49.0/font/lucide.ttf`
//! with (fonttools):
//!
//! ```text
//! pyftsubset lucide.ttf --no-hinting --layout-features='' \
//!   --output-file=lucide-icons.ttf --unicodes=U+E181,U+E093,U+E145,U+E148,\
//!   U+E18E,U+E06C,U+E070,U+E06D,U+E0F2,U+E193,U+E1B2,U+E1F9,U+E0B6,U+E12A,\
//!   U+E04A,U+E0D7
//! ```
//!
//! The terminal and code glyphs (U+E181, U+E093) are still in the file but
//! unused: the terminal and PyCharm buttons are drawn as [`Tile`]s.
//!
//! A new icon means adding its codepoint (from `font/info.json`) here and to
//! that list, then regenerating the file.

use crate::app::Message;
use crate::theme::{self, Swatch};
use iced::widget::canvas;

/// The subset font, registered with the application in `lib.rs`.
pub const FONT_BYTES: &[u8] = include_bytes!("../assets/lucide-icons.ttf");

/// The family the font declares.
const FONT: iced::Font = iced::Font::with_name("lucide");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Refresh,
    Rotate,
    Trash,
    Check,
    ChevronUp,
    ChevronDown,
    Heart,
    Alert,
    X,
    Pencil,
    Ellipsis,
    PanelLeft,
    ArrowUp,
    Folder,
}

impl Icon {
    #[cfg(test)]
    const ALL: [Icon; 14] = [
        Icon::Refresh,
        Icon::Rotate,
        Icon::Trash,
        Icon::Check,
        Icon::ChevronUp,
        Icon::ChevronDown,
        Icon::Heart,
        Icon::Alert,
        Icon::X,
        Icon::Pencil,
        Icon::Ellipsis,
        Icon::PanelLeft,
        Icon::ArrowUp,
        Icon::Folder,
    ];

    /// The glyph's codepoint in Lucide's font (its `font/info.json`).
    fn codepoint(self) -> char {
        match self {
            Icon::Refresh => '\u{e145}',
            Icon::Rotate => '\u{e148}',
            Icon::Trash => '\u{e18e}',
            Icon::Check => '\u{e06c}',
            Icon::ChevronUp => '\u{e070}',
            Icon::ChevronDown => '\u{e06d}',
            Icon::Heart => '\u{e0f2}',
            Icon::Alert => '\u{e193}',
            Icon::X => '\u{e1b2}',
            Icon::Pencil => '\u{e1f9}',
            Icon::Ellipsis => '\u{e0b6}',
            Icon::PanelLeft => '\u{e12a}',
            Icon::ArrowUp => '\u{e04a}',
            Icon::Folder => '\u{e0d7}',
        }
    }
}

/// `which` at `size`, tinted `swatch` in whichever palette is rendering.
pub fn icon<'a, M: 'a>(which: Icon, size: f32, swatch: Swatch) -> iced::Element<'a, M> {
    iced::widget::text(which.codepoint().to_string())
        .font(FONT)
        .size(size)
        .line_height(1.0)
        .style(theme::tinted(swatch))
        .into()
}

/// The busy indicator: a three-quarter ring turned by `angle` radians.
struct Spinner {
    angle: f32,
    swatch: Swatch,
}

impl<M> canvas::Program<M> for Spinner {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        theme: &iced::Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let width = (bounds.width / 8.0).max(1.5);
        let ring = canvas::Path::new(|b| {
            b.arc(canvas::path::Arc {
                center: frame.center(),
                radius: bounds.width.min(bounds.height) / 2.0 - width,
                start_angle: iced::Radians(self.angle),
                end_angle: iced::Radians(self.angle + std::f32::consts::PI * 1.5),
            });
        });
        frame.stroke(
            &ring,
            canvas::Stroke::default()
                .with_color(theme::palette(theme).get(self.swatch))
                .with_width(width)
                .with_line_cap(canvas::LineCap::Round),
        );
        vec![frame.into_geometry()]
    }
}

/// The spinner at `size`, turned by `angle` radians.
pub fn spinner<'a, M: 'a>(angle: f32, size: f32, swatch: Swatch) -> iced::Element<'a, M> {
    iced::widget::canvas(Spinner { angle, swatch })
        .width(size)
        .height(size)
        .into()
}

/// The glyph's tint, matching what `theme::button_quiet` paints text in:
/// `parchment` when it can be pressed, `slate` when it cannot. The glyph's own
/// style cannot see its button's status, so the state is decided here.
fn tint(enabled: bool) -> Swatch {
    if enabled {
        Swatch::Parchment
    } else {
        Swatch::Slate
    }
}

/// A quiet icon-only button. `tip` is required: an icon alone is unlabelled
/// for anyone who does not already know it.
pub fn icon_button<'a>(
    which: Icon,
    tip: impl Into<String>,
    on_press: Option<Message>,
) -> iced::Element<'a, Message> {
    theme::tip(
        // 18, not 16: Lucide's strokes thin with size, and at 16 they were
        // hard to make out on the dark ground.
        iced::widget::button(icon(which, 18.0, tint(on_press.is_some())))
            .on_press_maybe(on_press)
            .padding([theme::SPACE_1, theme::SPACE_2])
            .style(theme::button_quiet),
        tip,
        iced::widget::tooltip::Position::Bottom,
    )
}

/// Which app a [`tile_button`] opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tile {
    /// A filled square with "PC" and a short bar beneath it, after the
    /// JetBrains product-icon layout, without drawing their logo.
    Pycharm,
    /// An outlined square with a prompt in it.
    Terminal,
    /// An outlined square with "Ju": Jupyter, without drawing its logo.
    Jupyter,
}

/// Side of a [`Tile`], in points. Larger than a glyph icon: a tile's
/// lettering has to stay legible inside it.
const TILE: f32 = 20.0;

/// The tile itself. `enabled` picks the tint as [`tint`] does, and in each
/// palette the tile is drawn in the text colour, so it inverts with the
/// theme instead of a dark square vanishing into the dark ground.
fn tile<'a, M: 'a>(which: Tile, enabled: bool) -> iced::Element<'a, M> {
    let swatch = tint(enabled);
    let filled = which == Tile::Pycharm;
    let (label, font, size) = match which {
        Tile::Pycharm => ("PC", theme::FONT_PROSE_SEMIBOLD, 9.0),
        Tile::Terminal => (">_", theme::FONT_DATA, 11.0),
        Tile::Jupyter => ("Ju", theme::FONT_PROSE_SEMIBOLD, 9.0),
    };
    let letters = iced::widget::text(label)
        .font(font)
        .size(size)
        .line_height(1.0)
        .style(move |t: &iced::Theme| iced::widget::text::Style {
            color: Some(if filled {
                theme::palette(t).ink
            } else {
                theme::palette(t).get(swatch)
            }),
        });
    let mut body = iced::widget::column![letters].spacing(2.0);
    if filled {
        body = body.push(
            iced::widget::container(iced::widget::Space::new().width(7.0).height(1.5)).style(
                |t: &iced::Theme| iced::widget::container::Style {
                    background: Some(iced::Background::Color(theme::palette(t).ink)),
                    ..iced::widget::container::Style::default()
                },
            ),
        );
    }
    iced::widget::container(body)
        .width(TILE)
        .height(TILE)
        .padding([2.0, 3.0])
        // "PC" sits low over its bar, as on JetBrains tiles; the prompt
        // is centred.
        .align_x(if filled {
            iced::alignment::Horizontal::Left
        } else {
            iced::alignment::Horizontal::Center
        })
        .align_y(if filled {
            iced::alignment::Vertical::Bottom
        } else {
            iced::alignment::Vertical::Center
        })
        .style(move |t: &iced::Theme| {
            let colour = theme::palette(t).get(swatch);
            iced::widget::container::Style {
                background: filled.then_some(iced::Background::Color(colour)),
                border: iced::Border {
                    color: colour,
                    width: 1.5,
                    radius: 3.0.into(),
                },
                ..iced::widget::container::Style::default()
            }
        })
        .into()
}

/// [`icon_button`] for a [`Tile`]: same quiet button, same required tip.
pub fn tile_button<'a>(
    which: Tile,
    tip: impl Into<String>,
    on_press: Option<Message>,
) -> iced::Element<'a, Message> {
    theme::tip(
        iced::widget::button(tile(which, on_press.is_some()))
            .on_press_maybe(on_press)
            .padding([theme::SPACE_1, theme::SPACE_2])
            .style(theme::button_quiet),
        tip,
        iced::widget::tooltip::Position::Bottom,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disabled_icon_button_is_tinted_like_disabled_text() {
        // The glyph is the whole button; if it ignores the state, "Move up"
        // on the first script looks pressable and does nothing.
        assert_eq!(tint(true), Swatch::Parchment);
        assert_eq!(tint(false), Swatch::Slate);
    }

    #[test]
    fn every_icon_is_a_glyph_in_the_bundled_font() {
        // A wrong codepoint renders as a blank box, silently.
        let face = ttf_parser::Face::parse(FONT_BYTES, 0).expect("the bundled font parses");
        for which in Icon::ALL {
            assert!(
                face.glyph_index(which.codepoint()).is_some(),
                "{which:?} ({:#x}) is not in assets/lucide-icons.ttf",
                which.codepoint() as u32
            );
        }
    }
}
