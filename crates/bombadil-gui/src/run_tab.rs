//! A project's Run tab: entry points, saved commands, a picked file and
//! Jupyter, run as `uv run --no-sync …` in the project's own lane.

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnd {
    /// The program exited; `None` when a signal ended it.
    Exited(Option<i32>),
    /// The person pressed Stop.
    Stopped,
    /// It never started: no environment, uv missing, a composition error.
    Failed(String),
}

/// Lines kept per run; the oldest go first.
pub const MAX_LINES: usize = 2000;

/// One project's run: what it is, what it printed, and how to stop it.
#[derive(Debug, Clone)]
pub struct Run {
    pub label: String,
    pub lines: Vec<String>,
    pub started: std::time::Instant,
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// `None` while it runs.
    pub end: Option<RunEnd>,
}

impl Run {
    pub fn live(&self) -> bool {
        self.end.is_none()
    }

    pub fn push(&mut self, line: String) {
        self.lines.push(line);
        if self.lines.len() > MAX_LINES {
            let overflow = self.lines.len() - MAX_LINES;
            self.lines.drain(0..overflow);
        }
    }
}

/// A saved command's words. Whitespace separates them; `"…"` and `'…'`
/// group words. A backslash is an ordinary character, so a Windows path
/// survives.
pub fn split_command(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in command.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            None => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// The add/edit form under the saved commands.
#[derive(Debug, Clone, Default)]
pub struct CommandDraft {
    pub label: String,
    pub command: String,
    /// The row being edited, or `None` for a new one.
    pub editing: Option<usize>,
}

/// The saved command for a picked `.py` file: relative to the project when
/// it is inside it, quoted when the path has spaces.
pub fn file_command(project_dir: &std::path::Path, file: &std::path::Path) -> String {
    let path = file.strip_prefix(project_dir).unwrap_or(file);
    format!("python {}", quoted(&path.to_string_lossy()))
}

/// `word` as one word for [`split_command`]: as it is when it needs no
/// quoting, else in `"…"`, with each `"` it contains written as `'"'` --
/// adjacent quoted pieces join into the same word.
fn quoted(word: &str) -> String {
    let plain = !word
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\'');
    if plain {
        return word.to_string();
    }
    let mut out = String::new();
    for (i, part) in word.split('"').enumerate() {
        if i > 0 {
            out.push_str("'\"'");
        }
        if !part.is_empty() {
            out.push('"');
            out.push_str(part);
            out.push('"');
        }
    }
    out
}

/// `12s`, `3m 04s`, `1h 02m`: how long a run has been going.
fn elapsed(d: std::time::Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

/// The Run tab. Deliberately dumb: what it shows is decided in `app`.
pub fn view<'a>(
    project: &'a bombadil_core::model::Project,
    index: usize,
    entry_points: &'a [String],
    run: Option<&'a Run>,
    draft: &'a CommandDraft,
    blocked: Option<&'static str>,
    now: std::time::Instant,
) -> iced::Element<'a, crate::app::Message> {
    use crate::app::Message;
    use crate::theme;
    use iced::widget::{button, column, row, text, text_input};
    let id = project.id;
    let start = |label: &str, args: Vec<String>| Message::RunRequested {
        project: id,
        label: label.to_string(),
        args,
    };
    // A start button: enabled only when nothing blocks a run, and saying why
    // when something does.
    let starter = |label: String, message: Message| -> iced::Element<'a, Message> {
        let b = button(text(label).size(theme::BODY))
            .on_press_maybe(blocked.is_none().then_some(message))
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(theme::button_secondary);
        match blocked {
            Some(why) => theme::tip(b, why, iced::widget::tooltip::Position::Top),
            None => b.into(),
        }
    };
    let heading = |t: &'static str| text(t).size(theme::TITLE);
    let muted = |t: String| text(t).size(theme::BODY).style(theme::text_muted);

    let mut page = column![].spacing(theme::SPACE_3);

    if let Some(run) = run {
        let status: iced::Element<'a, Message> = match &run.end {
            None => row![
                text(format!(
                    "Running {} \u{2014} {}",
                    run.label,
                    elapsed(now.saturating_duration_since(run.started))
                ))
                .size(theme::BODY),
                button(text("Stop").size(theme::BODY))
                    .on_press(Message::RunStopRequested(id))
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_danger),
            ]
            .spacing(theme::SPACE_3)
            .align_y(iced::Alignment::Center)
            .into(),
            Some(end) => {
                let words = match end {
                    RunEnd::Exited(Some(code)) => format!("{} exited with {code}", run.label),
                    RunEnd::Exited(None) => format!("{} was ended by the system", run.label),
                    RunEnd::Stopped => format!("{} stopped", run.label),
                    RunEnd::Failed(why) => format!("{} didn't start: {why}", run.label),
                };
                let mut c = column![text(words).size(theme::BODY)].spacing(theme::SPACE_1);
                if matches!(end, RunEnd::Exited(Some(code)) if *code != 0) {
                    c = c.push(muted(
                        bombadil_core::doctor::diagnose(&run.lines.join("\n")).sentence,
                    ));
                }
                c.into()
            }
        };
        let lines = run.lines.iter().fold(column![], |c, l| {
            c.push(text(l.clone()).font(theme::FONT_DATA).size(theme::DATA))
        });
        page = page.push(status).push(
            iced::widget::scrollable(lines)
                .height(320.0)
                .width(iced::Length::Fill)
                .anchor_bottom(),
        );
    }

    page = page.push(
        row![
            muted("Programs that ask for input need a terminal.".into()),
            button(text("Open terminal").size(theme::BODY))
                .on_press(Message::OpenTerminalHereRequested(index))
                .padding([theme::SPACE_1, theme::SPACE_3])
                .style(theme::button_quiet),
        ]
        .spacing(theme::SPACE_2)
        .align_y(iced::Alignment::Center),
    );

    page = page.push(heading("Entry points"));
    if entry_points.is_empty() {
        page = page.push(muted(
            "This project declares no entry points ([project.scripts]).".into(),
        ));
    } else {
        page = page.push(
            iced::widget::Row::with_children(
                entry_points
                    .iter()
                    .map(|name| starter(name.clone(), start(name, vec![name.clone()]))),
            )
            .spacing(theme::SPACE_2)
            .wrap(),
        );
    }

    page = page.push(heading("Saved commands"));
    for (row_index, c) in project.commands.iter().enumerate() {
        page = page.push(
            row![
                text(c.label.clone())
                    .size(theme::BODY)
                    .width(iced::Length::FillPortion(1)),
                text(c.command.clone())
                    .font(theme::FONT_DATA)
                    .size(theme::DATA)
                    .width(iced::Length::FillPortion(2)),
                starter("Run".into(), start(&c.label, split_command(&c.command))),
                button(text("Edit").size(theme::BODY))
                    .on_press(Message::CommandEdit(row_index))
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_quiet),
                button(text("Remove").size(theme::BODY))
                    .on_press(Message::CommandRemoved(row_index))
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_quiet),
            ]
            .spacing(theme::SPACE_2)
            .align_y(iced::Alignment::Center),
        );
    }
    page = page.push(
        row![
            text_input("Name (optional)", &draft.label)
                .on_input(Message::CommandDraftLabel)
                .on_submit(Message::CommandSaved)
                .style(theme::text_field)
                .width(iced::Length::FillPortion(1)),
            text_input("Command, e.g. pytest -q", &draft.command)
                .on_input(Message::CommandDraftCommand)
                .on_submit(Message::CommandSaved)
                .style(theme::text_field)
                .width(iced::Length::FillPortion(2)),
            button(
                text(if draft.editing.is_some() {
                    "Save"
                } else {
                    "Add"
                })
                .size(theme::BODY)
            )
            .on_press(Message::CommandSaved)
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(theme::button_secondary),
        ]
        .spacing(theme::SPACE_2),
    );

    page = page.push(heading("More"));
    page = page.push(
        row![
            button(text("Run a file\u{2026}").size(theme::BODY))
                .on_press_maybe(blocked.is_none().then_some(Message::RunFileRequested))
                .padding([theme::SPACE_1, theme::SPACE_3])
                .style(theme::button_secondary),
            starter(
                "Open in Jupyter".into(),
                start(
                    "Jupyter",
                    ["--with", "jupyterlab", "jupyter", "lab"]
                        .map(String::from)
                        .to_vec(),
                ),
            ),
        ]
        .spacing(theme::SPACE_2),
    );
    page = page.push(muted(
        "Jupyter runs from a temporary install; your project's dependencies don't change.".into(),
    ));
    page.into()
}

/// A two-button confirmation: the danger action and Cancel.
pub fn confirm_view<'a>(
    text: String,
    action: &'static str,
    confirm: crate::app::Message,
    cancel: crate::app::Message,
) -> iced::Element<'a, crate::app::Message> {
    use crate::theme;
    iced::widget::container(
        iced::widget::column![
            iced::widget::text(text).size(theme::BODY),
            iced::widget::row![
                iced::widget::button(iced::widget::text(action).size(theme::BODY))
                    .on_press(confirm)
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_danger),
                iced::widget::button(iced::widget::text("Cancel").size(theme::BODY))
                    .on_press(cancel)
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_quiet),
            ]
            .spacing(theme::SPACE_2),
        ]
        .spacing(theme::SPACE_3),
    )
    .padding(theme::SPACE_3)
    .style(theme::panel)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_split_on_whitespace_and_quotes_group() {
        assert_eq!(
            split_command("pytest -k \"not slow\""),
            ["pytest", "-k", "not slow"]
        );
        assert_eq!(split_command("  python  main.py "), ["python", "main.py"]);
        assert_eq!(split_command("echo 'a b' c"), ["echo", "a b", "c"]);
        assert!(split_command("   ").is_empty());
    }

    #[test]
    fn a_picked_file_runs_relative_to_the_project() {
        let dir = std::path::Path::new("/p/api");
        assert_eq!(file_command(dir, &dir.join("main.py")), "python main.py");
        let spaced = dir.join("my scripts").join("x.py");
        assert_eq!(
            split_command(&file_command(dir, &spaced)),
            [
                "python",
                std::path::Path::new("my scripts")
                    .join("x.py")
                    .to_str()
                    .unwrap()
            ]
        );
    }

    #[test]
    fn a_path_with_a_quote_in_it_survives() {
        let dir = std::path::Path::new("/p/api");
        for name in ["o'brien.py", "say \"hi\".py", "it's \"x\".py"] {
            let file = dir.join(name);
            assert_eq!(
                split_command(&file_command(dir, &file)),
                ["python", name],
                "{name}"
            );
        }
    }

    #[test]
    fn a_file_outside_the_project_keeps_its_full_path() {
        let outside = std::env::temp_dir().join("x.py");
        assert_eq!(
            split_command(&file_command(std::path::Path::new("/p/api"), &outside)),
            ["python", outside.to_str().unwrap()]
        );
    }

    #[test]
    fn backslashes_are_kept() {
        assert_eq!(
            split_command(r"python scripts\run.py"),
            ["python", r"scripts\run.py"]
        );
        assert_eq!(
            split_command(r#"python "C:\My Files\x.py""#),
            ["python", r"C:\My Files\x.py"]
        );
    }
}
