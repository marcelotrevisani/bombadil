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

/// What the Run tab knows about the project's `just` recipes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JustState {
    NoJustfile,
    Loading,
    /// There is a justfile, but `just` couldn't be started.
    NotInstalled,
    /// `just` refused the justfile; its own words.
    Error(String),
    Recipes {
        recipes: Vec<bombadil_core::just::Recipe>,
        /// Which `just` read them, e.g. "Using the bundled just 1.58.0".
        using: String,
    },
}

/// Says which `just` runs the recipes.
pub fn just_label(found: &bombadil_core::just::Found) -> String {
    if found.bundled {
        format!(
            "Using the bundled just {}",
            bombadil_uv_bin::JUST_BUNDLED_VERSION
        )
    } else {
        format!("Using just from PATH ({})", found.path.display())
    }
}

/// Reads the recipes of the justfile in `dir`, by asking `just`. Blocking.
pub fn just_blocking(
    dir: &std::path::Path,
    just: &dyn Fn() -> Result<Option<bombadil_core::just::Found>, String>,
    runner: &dyn bombadil_core::process::CommandRunner,
) -> JustState {
    if bombadil_core::just::find_justfile(dir).is_none() {
        return JustState::NoJustfile;
    }
    let just = match just() {
        Ok(Some(just)) => just,
        Ok(None) => return JustState::NotInstalled,
        Err(why) => return JustState::Error(why),
    };
    match runner.run(&bombadil_core::just::dump(&just.path, dir)) {
        Err(_) => JustState::NotInstalled,
        Ok(out) if !out.success() => {
            let words = format!("{}{}", out.stderr, out.stdout);
            JustState::Error(words.trim().to_string())
        }
        Ok(out) => match bombadil_core::just::parse_dump(&out.stdout) {
            Ok(recipes) => JustState::Recipes {
                recipes,
                using: just_label(&just),
            },
            Err(e) => JustState::Error(e),
        },
    }
}

/// A run's arguments with a leading `just` swapped for the `just` the
/// settings chose, so a recipe still runs through `uv run` in the project's
/// environment. Anything else passes through untouched.
pub fn with_resolved_just(
    mut args: Vec<String>,
    just: &dyn Fn() -> Result<Option<bombadil_core::just::Found>, String>,
) -> Result<Vec<String>, String> {
    if args.first().map(String::as_str) != Some("just") {
        return Ok(args);
    }
    match just()? {
        Some(found) => {
            args[0] = found.path.to_string_lossy().into_owned();
            Ok(args)
        }
        None => Err(JUST_NOT_ON_PATH.into()),
    }
}

/// What to say when the settings ask for `just` from PATH and there is none.
pub const JUST_NOT_ON_PATH: &str =
    "just isn't on PATH. Install it, or choose the bundled just in Settings.";

/// What a recipe's Run button runs (`just <recipe> <args>`), or why it
/// can't yet.
pub fn just_args(recipe: &bombadil_core::just::Recipe, typed: &str) -> Result<Vec<String>, String> {
    let words = split_command(typed);
    if let Some(why) = bombadil_core::just::missing_args(recipe, &words) {
        return Err(why);
    }
    Ok(["just".to_string(), recipe.name.clone()]
        .into_iter()
        .chain(words)
        .collect())
}

/// What the view needs to draw the recipes.
pub struct JustView<'a> {
    pub state: Option<&'a JustState>,
    pub args: &'a std::collections::HashMap<String, String>,
    pub shown: Option<&'a (String, String)>,
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
// Every input is something the tab shows; the same allowance
// `preferences::view` takes.
#[allow(clippy::too_many_arguments)]
pub fn view<'a>(
    project: &'a bombadil_core::model::Project,
    index: usize,
    entry_points: &'a [String],
    run: Option<&'a Run>,
    draft: &'a CommandDraft,
    blocked: Option<&'static str>,
    now: std::time::Instant,
    just: JustView<'a>,
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

    page = just_section(page, &just, blocked, id);

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

/// The "Just recipes" section, when the project has a justfile.
fn just_section<'a>(
    mut page: iced::widget::Column<'a, crate::app::Message>,
    just: &JustView<'a>,
    blocked: Option<&'static str>,
    project: uuid::Uuid,
) -> iced::widget::Column<'a, crate::app::Message> {
    use crate::app::Message;
    use crate::theme;
    use iced::widget::{button, column, row, text, text_input};
    let muted = |t: String| text(t).size(theme::BODY).style(theme::text_muted);
    let state = match just.state {
        None | Some(JustState::NoJustfile) => return page,
        Some(state) => state,
    };
    page = page.push(text("Just recipes").size(theme::TITLE));
    match state {
        JustState::NoJustfile => {}
        JustState::Loading => page = page.push(muted("Reading the justfile\u{2026}".into())),
        JustState::NotInstalled => {
            page = page.push(
                row![
                    muted(JUST_NOT_ON_PATH.into()),
                    button(text("How to install just").size(theme::BODY))
                        .on_press(Message::GitHubOpen(
                            "https://just.systems/man/en/packages.html".into(),
                        ))
                        .padding([theme::SPACE_1, theme::SPACE_2])
                        .style(theme::button_quiet),
                ]
                .spacing(theme::SPACE_2)
                .align_y(iced::Alignment::Center),
            );
        }
        JustState::Error(why) => {
            page = page.push(
                text(why.clone())
                    .font(theme::FONT_DATA)
                    .size(theme::DATA)
                    .style(theme::text_warning),
            );
        }
        JustState::Recipes { recipes, using } => {
            page = page.push(muted(using.clone()));
            if recipes.is_empty() {
                page = page.push(muted("The justfile has no public recipes.".into()));
            }
            for recipe in recipes {
                let typed = just
                    .args
                    .get(&recipe.name)
                    .map(String::as_str)
                    .unwrap_or("");
                let run = super::run_tab::just_args(recipe, typed);
                let why = blocked
                    .map(str::to_string)
                    .or_else(|| run.as_ref().err().cloned());
                let start = button(text("Run").size(theme::BODY))
                    .on_press_maybe(run.ok().filter(|_| blocked.is_none()).map(|args| {
                        Message::RunRequested {
                            project,
                            label: format!("just {}", recipe.name),
                            args,
                        }
                    }))
                    .padding([theme::SPACE_1, theme::SPACE_3])
                    .style(theme::button_secondary);
                let start: iced::Element<'a, Message> = match why {
                    Some(why) => theme::tip(start, why, iced::widget::tooltip::Position::Top),
                    None => start.into(),
                };
                let params = recipe
                    .params
                    .iter()
                    .map(|p| match &p.default {
                        Some(d) => format!("{} = {d}", p.name),
                        None if p.optional_many => format!("{}\u{2026}", p.name),
                        None => p.name.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join("  ");
                let mut line = row![
                    column![
                        text(recipe.name.clone())
                            .font(theme::FONT_DATA)
                            .size(theme::DATA),
                    ]
                    .push(recipe.doc.clone().map(muted))
                    .push((!params.is_empty()).then(|| {
                        text(params.clone())
                            .font(theme::FONT_DATA)
                            .size(theme::DATA)
                            .style(theme::text_muted)
                    }))
                    .width(iced::Length::Fill),
                ]
                .spacing(theme::SPACE_2)
                .align_y(iced::Alignment::Center);
                if !recipe.params.is_empty() {
                    let name = recipe.name.clone();
                    line = line.push(
                        text_input("arguments", typed)
                            .on_input(move |v| Message::JustArgsChanged(name.clone(), v))
                            .style(theme::text_field)
                            .width(200.0),
                    );
                }
                line = line
                    .push(
                        button(text("Show").size(theme::BODY))
                            .on_press(Message::JustShowRequested(recipe.name.clone()))
                            .padding([theme::SPACE_1, theme::SPACE_2])
                            .style(theme::button_quiet),
                    )
                    .push(start);
                page = page.push(line);
                if let Some((shown, source)) = just.shown
                    && *shown == recipe.name
                {
                    page = page.push(
                        iced::widget::container(
                            text(source.clone())
                                .font(theme::FONT_DATA)
                                .size(theme::DATA),
                        )
                        .padding(theme::SPACE_2)
                        .width(iced::Length::Fill)
                        .style(theme::panel),
                    );
                }
            }
        }
    }
    page
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

    fn just_runner(
        outcome: Option<(i32, &str, &str)>,
    ) -> bombadil_core::process::FakeCommandRunner {
        let runner = bombadil_core::process::FakeCommandRunner::new();
        if let Some((status, stdout, stderr)) = outcome {
            runner.expect(
                "just",
                &["--dump"],
                bombadil_core::process::CommandOutcome {
                    status: Some(status),
                    stdout: stdout.into(),
                    stderr: stderr.into(),
                },
            );
        }
        runner
    }

    fn on_path() -> Result<Option<bombadil_core::just::Found>, String> {
        Ok(Some(bombadil_core::just::Found {
            path: "/tools/just".into(),
            bundled: false,
        }))
    }

    #[test]
    fn the_recipes_say_which_just_read_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("justfile"), "x:\n  echo\n").unwrap();
        let empty = r#"{"recipes":{}}"#;
        let using = |state| match state {
            JustState::Recipes { using, .. } => using,
            other => panic!("{other:?}"),
        };
        let mine = using(just_blocking(
            dir.path(),
            &on_path,
            &just_runner(Some((0, empty, ""))),
        ));
        assert!(mine.contains("from PATH"), "{mine}");
        assert!(mine.contains(&std::path::Path::new("/tools/just").display().to_string()));
        let ours = using(just_blocking(
            dir.path(),
            &|| {
                Ok(Some(bombadil_core::just::Found {
                    path: "/data/just".into(),
                    bundled: true,
                }))
            },
            &just_runner(Some((0, empty, ""))),
        ));
        assert_eq!(
            ours,
            format!(
                "Using the bundled just {}",
                bombadil_uv_bin::JUST_BUNDLED_VERSION
            )
        );
    }

    #[test]
    fn the_resolved_just_is_the_one_asked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("justfile"), "x:\n  echo\n").unwrap();
        let runner = just_runner(Some((0, r#"{"recipes":{}}"#, "")));
        let _ = just_blocking(dir.path(), &on_path, &runner);
        assert_eq!(
            runner.calls()[0].program,
            std::path::Path::new("/tools/just")
        );
    }

    #[test]
    fn a_bundled_just_that_cannot_be_unpacked_says_why() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("justfile"), "x:\n  echo\n").unwrap();
        let state = just_blocking(dir.path(), &|| Err("disk full".into()), &just_runner(None));
        assert_eq!(state, JustState::Error("disk full".into()));
    }

    #[test]
    fn a_recipe_run_uses_the_resolved_just() {
        let args = vec!["just".to_string(), "test".into()];
        assert_eq!(
            with_resolved_just(args.clone(), &on_path),
            Ok(vec!["/tools/just".to_string(), "test".into()])
        );
        assert!(with_resolved_just(args, &|| Ok(None)).is_err());
        let python = vec!["python".to_string(), "-V".into()];
        assert_eq!(
            with_resolved_just(python.clone(), &|| panic!("not a recipe")),
            Ok(python),
            "only a recipe run looks just up"
        );
    }

    #[test]
    fn a_folder_without_a_justfile_has_no_recipes() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            just_blocking(
                dir.path(),
                &|| panic!("no justfile, no just"),
                &just_runner(None)
            ),
            JustState::NoJustfile
        );
    }

    #[test]
    fn a_justfile_without_just_says_to_install_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("justfile"), "x:\n  echo\n").unwrap();
        assert_eq!(
            just_blocking(dir.path(), &|| Ok(None), &just_runner(None)),
            JustState::NotInstalled
        );
    }

    #[test]
    fn a_broken_justfile_shows_justs_words() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("justfile"), "x:\n  echo\n").unwrap();
        let state = just_blocking(
            dir.path(),
            &on_path,
            &just_runner(Some((1, "", "error: Unknown start of token\n"))),
        );
        assert_eq!(
            state,
            JustState::Error("error: Unknown start of token".into())
        );
    }

    #[test]
    fn recipes_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("justfile"), "x:\n  echo\n").unwrap();
        let dump = r#"{"recipes":{"test":{"name":"test","doc":"Run the tests.","private":false,"parameters":[]}}}"#;
        match just_blocking(dir.path(), &on_path, &just_runner(Some((0, dump, "")))) {
            JustState::Recipes { recipes, .. } => assert_eq!(recipes[0].name, "test"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_recipe_runs_as_just_with_its_arguments() {
        let recipe = bombadil_core::just::Recipe {
            name: "greet".into(),
            doc: None,
            params: vec![bombadil_core::just::Param {
                name: "who".into(),
                default: None,
                optional_many: false,
            }],
        };
        assert_eq!(just_args(&recipe, ""), Err("Needs: who".to_string()));
        assert_eq!(
            just_args(&recipe, r#""Ana Bo""#),
            Ok(vec!["just".into(), "greet".into(), "Ana Bo".into()])
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
