//! The Doctor tab: the last problem with its fixes, and newer versions.

use crate::app::{Message, fix_label};
use crate::theme;
use bombadil_core::doctor::{Diagnosis, DoctorFix};
use bombadil_core::uv::results::Outdated;

/// One secondary button per fix; disabled while a command runs.
pub fn fix_buttons<'a>(
    project: uuid::Uuid,
    fixes: &[DoctorFix],
    enabled: bool,
) -> iced::widget::Row<'a, Message> {
    fixes
        .iter()
        .fold(iced::widget::row![].spacing(theme::SPACE_2), |row, fix| {
            row.push(
                iced::widget::button(iced::widget::text(fix_label(fix)).size(theme::BODY))
                    .on_press_maybe(
                        enabled.then(|| Message::DoctorFixRequested(project, fix.clone())),
                    )
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_secondary),
            )
        })
}

/// `pinned`: normalised names whose declared constraint is `==`.
pub fn view<'a>(
    project: usize,
    id: uuid::Uuid,
    failure: Option<&Diagnosis>,
    outdated: Option<&Result<Vec<Outdated>, String>>,
    pinned: &[String],
    enabled: bool,
) -> iced::Element<'a, Message> {
    let heading = |t: &'static str| iced::widget::text(t).size(theme::TITLE);
    let muted = |t: String| {
        iced::widget::text(t)
            .size(theme::BODY)
            .style(theme::text_muted)
    };
    let mut col = iced::widget::column![heading("Last problem")].spacing(theme::SPACE_2);
    col = match failure {
        None => col.push(muted("Nothing has failed.".into())),
        Some(d) => col
            .push(iced::widget::text(d.sentence.clone()).size(theme::BODY))
            .push(fix_buttons(id, &d.fixes, enabled)),
    };
    col = col.push(heading("Newer versions"));
    col = match outdated {
        None => col.push(muted("Checking\u{2026}".into())),
        Some(Err(why)) => col.push(muted(format!("Couldn't check for newer versions: {why}"))),
        Some(Ok(list)) if list.is_empty() => col.push(muted("Everything is up to date.".into())),
        Some(Ok(list)) => list.iter().fold(col, |col, o| {
            let (label, message) = if pinned.contains(&crate::deps::normalise(&o.name)) {
                (
                    "Update pin",
                    Message::DoctorPinRequested(project, format!("{}=={}", o.name, o.latest)),
                )
            } else {
                (
                    "Upgrade",
                    Message::DepUpgradeRequested(project, o.name.clone()),
                )
            };
            col.push(
                iced::widget::row![
                    iced::widget::text(o.name.clone())
                        .font(theme::FONT_DATA)
                        .size(theme::DATA)
                        .width(theme::DEP_NAME_COLUMN_WIDTH),
                    iced::widget::text(format!("{} \u{2192} {}", o.version, o.latest))
                        .font(theme::FONT_DATA)
                        .size(theme::DATA),
                    iced::widget::button(iced::widget::text(label).size(theme::LABEL))
                        .on_press_maybe(enabled.then_some(message))
                        .padding([0.0, theme::SPACE_2])
                        .style(theme::button_secondary),
                ]
                .spacing(theme::SPACE_3)
                .align_y(iced::Alignment::Center),
            )
        }),
    };
    iced::widget::scrollable(col).into()
}
