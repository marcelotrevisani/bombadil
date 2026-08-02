use crate::sidebar;
use bombadil_core::model::{Config, Project};
use iced::Task;

/// Everything the window shows. Deliberately plain data: `update` is the only
/// place it changes, which is what makes the logic testable without a display.
pub struct App {
    // Not read yet: `projects` and `selected` are the only fields `view` and
    // `update` need this task. Kept whole rather than trimmed to just the
    // projects, because settings/index/save features in later tasks need the
    // rest of the config and re-deriving `App` then would be needless churn.
    #[allow(dead_code)]
    pub config: Config,
    pub projects: Vec<Project>,
    pub selected: Option<usize>,
}

#[derive(Debug, Clone)]
pub enum Message {
    ProjectSelected(usize),
}

impl App {
    pub fn new(config: Config) -> Self {
        let projects = config.projects.clone();
        let selected = (!projects.is_empty()).then_some(0);
        Self {
            config,
            projects,
            selected,
        }
    }

    pub fn selected_project(&self) -> Option<&Project> {
        self.selected.and_then(|i| self.projects.get(i))
    }
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        // A stale index can arrive from a message queued before a project was
        // removed; ignoring it is better than taking the window down.
        Message::ProjectSelected(i) if i < app.projects.len() => {
            app.selected = Some(i);
        }
        Message::ProjectSelected(_) => {}
    }
    Task::none()
}

pub fn view(app: &App) -> iced::Element<'_, Message> {
    // No real interpreter probe is wired yet -- every project renders hollow
    // until a later task supplies one (job::run is the seam for it, since a
    // real probe touches disk/spawns a process). `entries` itself stays free
    // of I/O regardless of what the caller passes here.
    let entries = sidebar::entries(&app.config, &|_path| None);
    iced::widget::row![
        iced::widget::container(sidebar::view(&entries, app.selected)).width(240),
        iced::widget::container(iced::widget::text(
            app.selected_project()
                .map(|p| p.label.clone())
                .unwrap_or_default()
        ))
        .padding(12),
    ]
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bombadil_core::model::Config;

    #[test]
    fn an_empty_config_produces_no_selection() {
        // Selecting a project that does not exist would panic in `view` on the
        // first frame, so the empty case has to be representable.
        let app = App::new(Config::default());
        assert!(app.selected.is_none());
        assert!(app.projects.is_empty());
    }

    #[test]
    fn the_first_project_is_selected_on_load() {
        let config = Config {
            projects: vec![project("api"), project("web")],
            ..Config::default()
        };
        let app = App::new(config);
        assert_eq!(
            app.selected_project().map(|p| p.label.as_str()),
            Some("api")
        );
    }

    #[test]
    fn selecting_a_project_changes_the_selection() {
        let config = Config {
            projects: vec![project("api"), project("web")],
            ..Config::default()
        };
        let mut app = App::new(config);
        let _ = update(&mut app, Message::ProjectSelected(1));
        assert_eq!(
            app.selected_project().map(|p| p.label.as_str()),
            Some("web")
        );
    }

    #[test]
    fn selecting_out_of_range_is_ignored_rather_than_clamped() {
        // A stale index can arrive from a message queued before a project was
        // removed. Two projects with the FIRST selected is what separates
        // "ignored" from "clamped to the last valid index" -- with one project
        // both behaviours produce the same answer and the test proves nothing.
        let config = Config {
            projects: vec![project("api"), project("web")],
            ..Config::default()
        };
        let mut app = App::new(config);
        let _ = update(&mut app, Message::ProjectSelected(99));
        assert_eq!(
            app.selected_project().map(|p| p.label.as_str()),
            Some("api"),
            "an out-of-range selection must be ignored, not clamped to the last project"
        );
    }

    fn project(label: &str) -> bombadil_core::model::Project {
        bombadil_core::model::Project {
            id: uuid::Uuid::new_v4(),
            label: label.into(),
            pyproject_path: std::path::PathBuf::from(format!("/p/{label}/pyproject.toml")),
            index_names: vec![],
            venv: bombadil_core::model::VenvLocation::Alongside,
            env: vec![],
            scripts: vec![],
            members: Default::default(),
        }
    }
}
