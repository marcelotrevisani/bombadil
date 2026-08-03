//! The application shell: the top bar, its Help menu, the modal frame those
//! menu entries open, and the empty state shown before there is anything to
//! tend.
//!
//! # Every region has an edge
//!
//! The one structural idea the shell adds. This application's whole job is
//! keeping an account of *bounded* things -- one project, one environment,
//! one interpreter -- and the interface had no boundaries at all: the sidebar
//! was the same `ink` as the pane beside it, the root column had no padding,
//! and the licence footer was three lines of legal text serving as the visual
//! base of the window.
//!
//! So: every region is either a `bark` surface or an `ink` ground, and where
//! two meet there is a [`theme::hairline_row`] or [`theme::hairline_column`].
//! No shadows, no gradients, no rounded panels -- the shape of the thing is
//! the edge around it. Indicators are edges too, which is why the selected
//! tab is an underline and the selected project a left marker rather than
//! either being a filled block.
//!
//! # The licence lives behind a menu, not in the chrome
//!
//! Apache-2.0 §4 and OFL 1.1 both require the notice to be *reachable*, not
//! permanently on screen. It used to occupy the bottom of every frame. Now
//! Help > Bundled licences opens it in a modal, which satisfies the same
//! obligation and gives the bottom of the window back to the output drawer,
//! where a running command actually needs it.

use crate::about;
use crate::app::Message;
use crate::theme;

/// Which modal is open. `App` holds an `Option<Modal>`; `None` is "no modal",
/// which is why there is no `None` variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modal {
    /// What this application is, and which version of it and of uv is
    /// running -- the two facts a bug report needs.
    About,
    /// The bundled licences in full: uv's Apache-2.0 and both fonts' OFL 1.1.
    Licences,
}

impl Modal {
    /// The card's title. A modal with no title is a box of text with no claim
    /// about what it is.
    pub fn title(self) -> &'static str {
        match self {
            Modal::About => "About Bombadil",
            Modal::Licences => "Bundled licences",
        }
    }
}

/// Every entry in the Help menu, in the order it is shown. One list, so the
/// menu and the test that holds it to having a licence entry cannot disagree.
pub const MENU_ENTRIES: [(&str, Modal); 2] = [
    ("About Bombadil", Modal::About),
    ("Bundled licences", Modal::Licences),
];

/// This build's version, for the About modal.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The top bar: the application's name on the left, the Help menu on the
/// right, on a `bark` surface with a hairline beneath it.
pub fn topbar<'a>(menu_open: bool) -> iced::Element<'a, Message> {
    let name = iced::widget::text("Bombadil")
        .font(theme::FONT_PROSE_SEMIBOLD)
        .size(theme::TITLE);

    // Toggles rather than opens: pressing the button that opened the menu is
    // how a user closes it again, and a press that did nothing would read as
    // a broken control.
    let help = iced::widget::button(iced::widget::text("Help").size(theme::BODY))
        .on_press(Message::MenuToggled)
        .padding([theme::SPACE_1, theme::SPACE_2])
        .style(theme::button_bare(if menu_open {
            theme::PARCHMENT
        } else {
            theme::SLATE
        }));

    let bar = iced::widget::container(
        iced::widget::row![
            name,
            iced::widget::Space::new().width(iced::Length::Fill),
            help
        ]
        .align_y(iced::Alignment::Center),
    )
    .width(iced::Length::Fill)
    .height(theme::TOPBAR_HEIGHT)
    .padding([0.0, theme::SPACE_3])
    .style(theme::surface);

    iced::widget::column![bar, theme::hairline_row()].into()
}

/// The Help menu's drop-down, anchored under the top bar's right edge.
///
/// Returned as a full-window layer rather than a widget in the bar: iced has
/// no popover, so the panel is positioned by aligning it within a
/// transparent full-size layer, and that layer is also what makes a press
/// anywhere else dismiss the menu.
pub fn menu_layer<'a>() -> iced::Element<'a, Message> {
    let mut panel = iced::widget::column![].spacing(0);
    for (label, modal) in MENU_ENTRIES {
        panel = panel.push(
            iced::widget::button(iced::widget::text(label).size(theme::BODY))
                .on_press(Message::ModalOpened(modal))
                .width(iced::Length::Fill)
                .padding([theme::SPACE_2, theme::SPACE_3])
                .style(theme::button_bare(theme::PARCHMENT)),
        );
    }

    dismissable(
        iced::widget::container(panel)
            .width(theme::MENU_WIDTH)
            .style(theme::panel)
            .into(),
        [theme::TOPBAR_HEIGHT + theme::SPACE_1, theme::SPACE_3],
        iced::alignment::Horizontal::Right,
        Message::MenuDismissed,
    )
}

/// A floating panel over a full-window layer that dismisses on a press
/// outside it.
///
/// The nesting is the whole point and it is easy to get backwards. `opaque`
/// goes around **the panel**, so presses on the panel stop there; the
/// `mouse_area` wraps the full-window positioner *outside* that, so presses
/// anywhere else reach it and dismiss. Written the other way round --
/// `mouse_area(opaque(positioner))` -- the opaque layer swallows every press
/// before the `mouse_area` can see one, and nothing ever dismisses. That was
/// a real bug in all three of these layers: the context menu stayed on screen
/// through any number of clicks elsewhere.
///
/// No scrim: a menu is a list of choices, not a demand for an answer, and
/// dimming the application behind one says the wrong thing. `modal_layer`
/// builds its own.
fn dismissable<'a>(
    panel: iced::Element<'a, Message>,
    padding: [f32; 2],
    align_x: iced::alignment::Horizontal,
    dismiss: Message,
) -> iced::Element<'a, Message> {
    let positioner = iced::widget::container(iced::widget::opaque(panel))
        .width(iced::Length::Fill)
        .height(iced::Length::Fill)
        .padding(padding)
        .align_x(align_x)
        .align_y(iced::alignment::Vertical::Top);

    iced::widget::opaque(iced::widget::mouse_area(positioner).on_press(dismiss))
}

/// Wraps `body` in a modal card over a scrim: a title, the body, and a Close
/// button. Pressing the scrim closes it too, which is what makes it a modal
/// rather than a panel that happens to float.
pub fn modal_layer<'a>(modal: Modal) -> iced::Element<'a, Message> {
    let body: iced::Element<'a, Message> = match modal {
        Modal::About => about_body(),
        Modal::Licences => licences_body(),
    };

    let card = iced::widget::container(
        iced::widget::column![
            iced::widget::text(modal.title()).size(theme::DISPLAY),
            theme::hairline_row(),
            body,
            iced::widget::row![
                iced::widget::Space::new().width(iced::Length::Fill),
                iced::widget::button(iced::widget::text("Close").size(theme::BODY))
                    .on_press(Message::ModalClosed)
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_quiet),
            ],
        ]
        .spacing(theme::SPACE_3),
    )
    .width(theme::MODAL_WIDTH)
    .padding(theme::SPACE_4)
    .style(theme::panel);

    // Centred rather than anchored, so it gets `center` inside the positioner
    // instead of a top alignment -- otherwise the same nesting as
    // `dismissable`, for the same reason.
    let positioner = iced::widget::container(iced::widget::center(iced::widget::opaque(card)))
        .width(iced::Length::Fill)
        .height(iced::Length::Fill)
        .style(|_theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(theme::SCRIM)),
            ..iced::widget::container::Style::default()
        });

    iced::widget::opaque(iced::widget::mouse_area(positioner).on_press(Message::ModalClosed))
}

/// A panel anchored beside the sidebar, dismissed by a press anywhere else.
///
/// For the project context menu. A right-click menu is the one surface that
/// must *not* be centred over a scrim: it belongs next to the thing it acts
/// on, and dimming the whole application to offer four entries reads as a
/// dialog demanding an answer rather than a menu offering choices.
///
/// ponytail: anchored to the sidebar's edge, not to the pointer.
/// `mouse_area::on_right_press` carries no coordinates in iced 0.14, so
/// placing it under the cursor would mean tracking every mouse move through
/// `update` to keep a position nothing else needs. The menu names the project
/// it acts on instead. Track the cursor if that stops being enough.
pub fn anchored_layer<'a>(
    content: iced::Element<'a, Message>,
    dismiss: Message,
) -> iced::Element<'a, Message> {
    dismissable(
        iced::widget::container(content)
            .width(theme::CONTEXT_MENU_WIDTH)
            .style(theme::panel)
            .into(),
        [
            theme::TOPBAR_HEIGHT + theme::SPACE_3,
            theme::SIDEBAR_WIDTH + theme::SPACE_2,
        ],
        iced::alignment::Horizontal::Left,
        dismiss,
    )
}

/// Puts `content` on a card over a scrim, as a layer of its own.
///
/// For the application's existing dialogs -- add project, the context menu,
/// the two confirmations. They used to be rows in the root column, so opening
/// one shoved the entire application down the window and left the thing being
/// answered sharing a surface with the thing it was about. As a layer the
/// body stays put underneath.
///
/// Deliberately **not** dismissable by pressing the scrim, unlike
/// [`modal_layer`]: every one of these carries an answer the user is part way
/// through giving, and a stray click is not consent to discard a half-filled
/// add-project form. They each have their own Cancel.
pub fn overlay_layer<'a>(content: iced::Element<'a, Message>) -> iced::Element<'a, Message> {
    let card = iced::widget::container(iced::widget::scrollable(content))
        .max_width(theme::MODAL_WIDTH)
        .max_height(theme::OVERLAY_MAX_HEIGHT)
        .padding(theme::SPACE_4)
        .style(theme::panel);

    iced::widget::opaque(
        iced::widget::container(iced::widget::center(card))
            .width(iced::Length::Fill)
            .height(iced::Length::Fill)
            .style(|_theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(theme::SCRIM)),
                ..iced::widget::container::Style::default()
            }),
    )
}

/// The About body: what this is, which version, and what it bundles.
fn about_body<'a>() -> iced::Element<'a, Message> {
    iced::widget::column![
        iced::widget::text("Keeps an account of uv virtual environments it does not own.")
            .size(theme::BODY),
        theme::labeled_value("version", version().to_string()),
        iced::widget::text(about::attribution())
            .size(theme::LABEL)
            .color(theme::SLATE),
        iced::widget::text(about::font_attribution())
            .size(theme::LABEL)
            .color(theme::SLATE),
        iced::widget::button(iced::widget::text("Read the bundled licences").size(theme::BODY))
            .on_press(Message::ModalOpened(Modal::Licences))
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(theme::button_quiet),
    ]
    .spacing(theme::SPACE_2)
    .into()
}

/// The Licences body: all three texts in one scrollable, each under its own
/// heading, in the mono face they were written in.
fn licences_body<'a>() -> iced::Element<'a, Message> {
    let mut column = iced::widget::column![].spacing(theme::SPACE_3);
    for (heading, text) in [
        ("uv - Apache License 2.0", about::license()),
        (
            "Atkinson Hyperlegible Next - SIL Open Font License 1.1",
            about::atkinson_license(),
        ),
        (
            "IBM Plex Mono - SIL Open Font License 1.1",
            about::plex_license(),
        ),
    ] {
        column = column.push(
            iced::widget::column![
                iced::widget::text(heading)
                    .font(theme::FONT_PROSE_SEMIBOLD)
                    .size(theme::BODY),
                iced::widget::text(text)
                    .font(theme::FONT_DATA)
                    .size(theme::LABEL)
                    .color(theme::SLATE),
            ]
            .spacing(theme::SPACE_2),
        );
    }

    iced::widget::scrollable(column)
        .height(theme::MODAL_BODY_HEIGHT)
        .into()
}

/// What the detail area shows before any project has been added.
///
/// The one screen where the primary action is the only thing on it. An empty
/// screen is an invitation, so it says what to choose and what happens after.
pub fn empty_state<'a>() -> iced::Element<'a, Message> {
    iced::widget::center(
        iced::widget::column![
            iced::widget::text("No projects yet").size(theme::DISPLAY),
            iced::widget::text(
                "Choose a project's pyproject.toml. Bombadil keeps the account of its \
                 environment: which interpreter it runs, what is installed, and what \
                 has drifted from the manifest."
            )
            .size(theme::BODY)
            .color(theme::SLATE)
            .width(420.0),
            iced::widget::button(iced::widget::text("Add project").size(theme::BODY))
                .on_press(Message::AddProjectPickManifestRequested)
                .padding([theme::SPACE_2, theme::SPACE_3])
                .style(theme::button_primary),
        ]
        .spacing(theme::SPACE_3)
        .align_x(iced::Alignment::Start),
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_offers_the_bundled_licences() {
        // The whole reason the footer could be removed: Apache-2.0 §4 and OFL
        // 1.1 require the notice to be reachable, and this menu is now the
        // only route to it. An entry list that lost it would be a licence
        // violation shipped as a layout tweak.
        assert!(
            MENU_ENTRIES
                .iter()
                .any(|(_, modal)| *modal == Modal::Licences),
            "no menu entry opens the licences"
        );
    }

    #[test]
    fn every_menu_entry_is_labelled_and_opens_a_distinct_modal() {
        // Two entries opening the same modal would make one of them a lie
        // about what pressing it does.
        for (label, _) in MENU_ENTRIES {
            assert!(!label.trim().is_empty(), "a menu entry has no label");
        }
        let modals: Vec<Modal> = MENU_ENTRIES.iter().map(|(_, modal)| *modal).collect();
        for (i, modal) in modals.iter().enumerate() {
            assert!(
                !modals[i + 1..].contains(modal),
                "{modal:?} is opened by two entries"
            );
        }
    }

    #[test]
    fn every_modal_has_a_title() {
        for modal in [Modal::About, Modal::Licences] {
            assert!(!modal.title().trim().is_empty(), "{modal:?} has no title");
        }
        assert_ne!(
            Modal::About.title(),
            Modal::Licences.title(),
            "two modals sharing a title cannot be told apart once open"
        );
    }

    #[test]
    fn the_reported_version_is_this_builds_version() {
        // The About modal exists so a bug report can name a build. A hardcoded
        // string would go stale at the first release and nobody would notice.
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
        assert!(!version().is_empty());
    }
}
