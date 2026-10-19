//! Preferences › Disk: what environments and uv's cache cost, and the safe
//! ways to get the space back.

use crate::app::Message;
use crate::theme;
use bombadil_core::housekeeping::{dir_size, format_size, leftovers};
use bombadil_core::model::{Config, DefaultVenvLocation, VenvLocation};
use bombadil_core::process::CommandRunner;
use std::path::PathBuf;

/// One registered environment and its size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvSize {
    pub project: uuid::Uuid,
    pub label: String,
    pub location: VenvLocation,
    pub path: PathBuf,
    /// The project's default: never offered for deletion.
    pub default: bool,
    /// Another registered environment uses the same folder: never offered.
    pub shared: bool,
    pub size: Option<u64>,
}

/// What a Delete button asks to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delete {
    /// A folder in the central venv folder no project uses.
    Leftover(PathBuf),
    /// A project's extra environment: removed from the project, folder deleted.
    Environment(uuid::Uuid, VenvLocation, PathBuf),
}

impl Delete {
    pub fn path(&self) -> &std::path::Path {
        match self {
            Delete::Leftover(p) | Delete::Environment(_, _, p) => p,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub envs: Vec<EnvSize>,
    pub leftovers: Vec<(PathBuf, Option<u64>)>,
    /// uv's cache folder and its size, when uv could say where it is.
    pub cache: Option<(PathBuf, Option<u64>)>,
    /// A measurement is running.
    pub measuring: bool,
    /// The delete waiting for "Delete" / "Cancel".
    pub confirm: Option<Delete>,
    /// The cache's size before a prune, to say how much it freed.
    pub before_prune: Option<u64>,
}

/// The delete a row offers, or `None` for a project's default environment.
pub fn deletable(env: &EnvSize) -> Option<Delete> {
    (!env.default && !env.shared)
        .then(|| Delete::Environment(env.project, env.location.clone(), env.path.clone()))
}

/// "Freed 1.2 MB", or "Nothing to free".
pub fn freed_text(before: u64, after: u64) -> String {
    match before.saturating_sub(after) {
        0 => "Nothing to free".into(),
        freed => format!("Freed {}", format_size(freed)),
    }
}

/// `path` as the filesystem resolves it, or as written.
fn resolved(path: &std::path::Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Every registered environment's folder, with the project and location it
/// belongs to.
fn registered_envs(config: &Config) -> Vec<(uuid::Uuid, VenvLocation, PathBuf)> {
    config
        .projects
        .iter()
        .flat_map(|p| {
            p.environments.iter().filter_map(move |e| {
                bombadil_core::venv_path::resolve(p, e, &config.settings)
                    .ok()
                    .map(|path| (p.id, e.location.clone(), path))
            })
        })
        .collect()
}

/// What a leftover must not be or contain: every registered environment
/// folder and every project folder.
fn registered(config: &Config) -> Vec<PathBuf> {
    registered_envs(config)
        .into_iter()
        .map(|(_, _, path)| path)
        .chain(
            config
                .projects
                .iter()
                .filter_map(|p| p.pyproject_path.parent().map(std::path::Path::to_path_buf)),
        )
        .collect()
}

/// Every size the section shows. Blocking: walks folders and asks uv
/// where its cache is.
pub fn measure_blocking(config: &Config, runner: &dyn CommandRunner) -> State {
    let mut envs: Vec<EnvSize> = config
        .projects
        .iter()
        .flat_map(|project| {
            project.environments.iter().filter_map(move |environment| {
                let path =
                    bombadil_core::venv_path::resolve(project, environment, &config.settings)
                        .ok()?;
                Some(EnvSize {
                    project: project.id,
                    label: project.label.clone(),
                    location: environment.location.clone(),
                    size: Some(dir_size(&path)),
                    path,
                    default: environment.location == project.active,
                    shared: false,
                })
            })
        })
        .collect();
    envs.sort_by_key(|e| std::cmp::Reverse(e.size));
    let folders: Vec<PathBuf> = envs.iter().map(|e| resolved(&e.path)).collect();
    for (i, env) in envs.iter_mut().enumerate() {
        env.shared = folders
            .iter()
            .enumerate()
            .any(|(j, f)| j != i && *f == folders[i]);
    }
    let leftovers = match &config.settings.default_venv_location {
        DefaultVenvLocation::Central { path } => leftovers(path, &registered(config))
            .into_iter()
            .map(|p| {
                let size = dir_size(&p);
                (p, Some(size))
            })
            .collect(),
        DefaultVenvLocation::Alongside => Vec::new(),
    };
    let cache = crate::app::resolve_uv_for(config, runner)
        .ok()
        .and_then(|uv| runner.run(&bombadil_core::uv::command::cache_dir(&uv)).ok())
        .filter(|out| out.success())
        .map(|out| PathBuf::from(out.stdout.trim()))
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| {
            let size = dir_size(&p);
            (p, Some(size))
        });
    State {
        envs,
        leftovers,
        cache,
        ..State::default()
    }
}

/// Deletes what `delete` names, through the guarded deletes. Blocking.
pub fn delete_blocking(delete: &Delete, config: &Config) -> Result<(), String> {
    match delete {
        Delete::Leftover(path) => {
            let DefaultVenvLocation::Central { path: central } =
                &config.settings.default_venv_location
            else {
                return Err("there is no central venv folder".into());
            };
            bombadil_core::venv_path::delete_leftover(central, path, &registered(config))
                .map_err(|e| e.to_string())
        }
        Delete::Environment(id, location, path) => {
            let project = config
                .projects
                .iter()
                .find(|p| p.id == *id)
                .ok_or("the project is no longer in Bombadil")?;
            if project.active == *location {
                return Err("a project's default environment is not deleted here".into());
            }
            // Another environment -- in this project or any other -- may
            // name the same folder; deleting it would take that one too.
            let folder = resolved(path);
            if registered_envs(config)
                .iter()
                .any(|(p, l, other)| (p != id || l != location) && resolved(other) == folder)
            {
                return Err("another environment uses this folder, so it is not deleted".into());
            }
            let environment = project
                .environments
                .iter()
                .find(|e| e.location == *location)
                .ok_or("the environment is no longer in the project")?;
            bombadil_core::venv_path::delete(project, environment, &config.settings, path)
                .map_err(|e| e.to_string())
        }
    }
}

fn size_text(size: Option<u64>) -> String {
    size.map_or_else(|| "measuring\u{2026}".to_string(), format_size)
}

/// The section. Deliberately dumb.
pub fn view<'a>(state: &'a State, idle: bool) -> iced::Element<'a, Message> {
    use iced::widget::{button, column, row, text};
    let muted = |t: String| text(t).size(theme::BODY).style(theme::text_muted);
    let heading = |t: &'static str| text(t).size(theme::TITLE);
    let data = |t: String| text(t).font(theme::FONT_DATA).size(theme::DATA);
    let mut page = column![
        heading("Disk usage"),
        muted("Sizes are measured each time this section opens.".into()),
    ]
    .spacing(theme::SPACE_2);

    if let Some(delete) = &state.confirm {
        page = page.push(
            iced::widget::container(
                column![
                    text(format!(
                        "Delete {}? This can't be undone.",
                        delete.path().display()
                    ))
                    .size(theme::BODY),
                    row![
                        button(text("Delete").size(theme::BODY))
                            .on_press(Message::DiskDeleteConfirmed)
                            .padding([theme::SPACE_1, theme::SPACE_3])
                            .style(theme::button_danger),
                        button(text("Cancel").size(theme::BODY))
                            .on_press(Message::DiskDeleteCancelled)
                            .padding([theme::SPACE_1, theme::SPACE_3])
                            .style(theme::button_quiet),
                    ]
                    .spacing(theme::SPACE_2),
                ]
                .spacing(theme::SPACE_2),
            )
            .padding(theme::SPACE_3)
            .style(theme::panel),
        );
    }

    page = page.push(heading("Environments"));
    if state.measuring && state.envs.is_empty() {
        page = page.push(muted("Measuring\u{2026}".into()));
    }
    for env in &state.envs {
        let mut line = row![
            text(env.label.clone())
                .size(theme::BODY)
                .width(iced::Length::FillPortion(2)),
            data(env.path.display().to_string()).width(iced::Length::FillPortion(5)),
            data(size_text(env.size)).width(iced::Length::FillPortion(1)),
        ]
        .spacing(theme::SPACE_2)
        .align_y(iced::Alignment::Center);
        line = match deletable(env) {
            Some(delete) => line.push(
                button(text("Remove and delete").size(theme::BODY))
                    .on_press(Message::DiskDeleteRequested(delete))
                    .padding([theme::SPACE_1, theme::SPACE_2])
                    .style(theme::button_quiet),
            ),
            None if env.default => line.push(muted("default".into())),
            None => line.push(muted("shared".into())),
        };
        page = page.push(line);
    }
    let total: u64 = state.envs.iter().filter_map(|e| e.size).sum();
    if !state.envs.is_empty() {
        page = page.push(muted(format!("Total: {}", format_size(total))));
    }

    if !state.leftovers.is_empty() {
        page = page.push(heading("Leftover environments")).push(muted(
            "Folders in your central venv folder that no project in Bombadil uses.".into(),
        ));
        for (path, size) in &state.leftovers {
            page = page.push(
                row![
                    data(path.display().to_string()).width(iced::Length::FillPortion(7)),
                    data(size_text(*size)).width(iced::Length::FillPortion(1)),
                    button(text("Delete").size(theme::BODY))
                        .on_press(Message::DiskDeleteRequested(Delete::Leftover(path.clone())))
                        .padding([theme::SPACE_1, theme::SPACE_2])
                        .style(theme::button_quiet),
                ]
                .spacing(theme::SPACE_2)
                .align_y(iced::Alignment::Center),
            );
        }
    }

    page = page.push(heading("uv cache"));
    match &state.cache {
        Some((path, size)) => {
            page = page
                .push(
                    row![
                        data(path.display().to_string()).width(iced::Length::FillPortion(7)),
                        data(size_text(*size)).width(iced::Length::FillPortion(1)),
                    ]
                    .spacing(theme::SPACE_2),
                )
                .push(
                    button(text("Remove unused cache files").size(theme::BODY))
                        .on_press_maybe(
                            (idle && !state.measuring).then_some(Message::CachePruneRequested),
                        )
                        .padding([theme::SPACE_1, theme::SPACE_3])
                        .style(theme::button_secondary),
                )
                .push(muted(
                    "Removes cache entries uv no longer needs. Your environments are not touched."
                        .into(),
                ));
        }
        None if state.measuring => page = page.push(muted("Measuring\u{2026}".into())),
        None => page = page.push(muted("uv didn't say where its cache is.".into())),
    }
    page.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{fake_uv_settings, version_probe};
    use bombadil_core::model::{Environment, Project, VenvLocation};
    use bombadil_core::process::{CommandOutcome, FakeCommandRunner};

    fn venv(dir: &std::path::Path, bytes: usize) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("pyvenv.cfg"), "version_info = 3.12.0\n").unwrap();
        std::fs::write(dir.join("blob"), vec![0u8; bytes]).unwrap();
    }

    fn project(label: &str, envs: &[&std::path::Path]) -> Project {
        let environments: Vec<Environment> = envs
            .iter()
            .map(|p| Environment {
                location: VenvLocation::Custom {
                    path: p.to_path_buf(),
                },
                python: Default::default(),
            })
            .collect();
        Project {
            id: uuid::Uuid::new_v4(),
            label: label.into(),
            pyproject_path: std::path::Path::new("/p")
                .join(label)
                .join("pyproject.toml"),
            active: environments[0].location.clone(),
            environments,
            ..Default::default()
        }
    }

    #[test]
    fn measuring_lists_every_environment_largest_first() {
        let dir = tempfile::tempdir().unwrap();
        let (small, big, extra) = (
            dir.path().join("s"),
            dir.path().join("b"),
            dir.path().join("x"),
        );
        venv(&small, 10);
        venv(&big, 5000);
        venv(&extra, 100);
        let cache = dir.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        std::fs::write(cache.join("c"), vec![0u8; 7]).unwrap();
        let config = Config {
            projects: vec![project("api", &[&small]), project("web", &[&big, &extra])],
            settings: fake_uv_settings(),
            ..Config::default()
        };
        let runner = FakeCommandRunner::new();
        version_probe(&runner);
        runner.expect(
            "uv",
            &["cache", "dir"],
            CommandOutcome {
                status: Some(0),
                stdout: format!("{}\n", cache.display()),
                stderr: String::new(),
            },
        );
        let state = measure_blocking(&config, &runner);
        let labels: Vec<&str> = state.envs.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["web", "web", "api"], "largest first");
        assert!(state.envs[0].default);
        assert!(!state.envs[1].default);
        assert_eq!(
            state.cache.as_ref().map(|(p, s)| (p.clone(), *s)),
            Some((cache, Some(7)))
        );
        assert!(state.leftovers.is_empty(), "no central folder");
    }

    #[test]
    fn leftovers_come_from_the_central_folder() {
        let central = tempfile::tempdir().unwrap();
        let used = central.path().join("used");
        let old = central.path().join("old");
        venv(&used, 1);
        venv(&old, 1);
        let mut config = Config {
            projects: vec![project("api", &[&used])],
            ..Config::default()
        };
        config.settings.default_venv_location =
            bombadil_core::model::DefaultVenvLocation::Central {
                path: central.path().to_path_buf(),
            };
        let state = measure_blocking(&config, &FakeCommandRunner::new());
        let left: Vec<_> = state.leftovers.iter().map(|(p, _)| p.clone()).collect();
        assert_eq!(left, [old]);
    }

    #[test]
    fn a_registered_project_folder_in_the_central_folder_is_not_a_leftover() {
        let central = tempfile::tempdir().unwrap();
        let proj = central.path().join("proj");
        venv(&proj, 1);
        let mut p = project("proj", &[&central.path().join("envs-elsewhere")]);
        p.pyproject_path = proj.join("pyproject.toml");
        let mut config = Config {
            projects: vec![p],
            ..Config::default()
        };
        config.settings.default_venv_location =
            bombadil_core::model::DefaultVenvLocation::Central {
                path: central.path().to_path_buf(),
            };
        assert!(
            measure_blocking(&config, &FakeCommandRunner::new())
                .leftovers
                .is_empty()
        );
        assert!(delete_blocking(&Delete::Leftover(proj.clone()), &config).is_err());
        assert!(proj.exists());
    }

    #[test]
    fn an_extra_environment_sharing_a_folder_is_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("api").join(".venv");
        venv(&shared, 1);
        // The default lives beside the project; an extra names the same
        // folder by its full path.
        let mut p = project("api", &[&shared]);
        p.pyproject_path = dir.path().join("api").join("pyproject.toml");
        p.environments.insert(
            0,
            Environment {
                location: VenvLocation::Alongside,
                python: Default::default(),
            },
        );
        p.active = VenvLocation::Alongside;
        let extra = p.environments[1].location.clone();
        let config = Config {
            projects: vec![p.clone()],
            ..Config::default()
        };
        let measured = measure_blocking(&config, &FakeCommandRunner::new());
        assert!(
            measured.envs.iter().all(|e| deletable(e).is_none()),
            "a folder another environment uses is never offered: {:?}",
            measured.envs
        );
        assert!(
            delete_blocking(&Delete::Environment(p.id, extra, shared.clone()), &config).is_err()
        );
        assert!(shared.exists());
    }

    #[test]
    fn the_default_environment_is_never_deletable() {
        let e = |default: bool| EnvSize {
            project: uuid::Uuid::nil(),
            label: "api".into(),
            location: VenvLocation::Alongside,
            path: "/p/.venv".into(),
            default,
            shared: false,
            size: Some(1),
        };
        assert_eq!(deletable(&e(true)), None);
        assert!(matches!(
            deletable(&e(false)),
            Some(Delete::Environment(..))
        ));
    }

    #[test]
    fn deleting_a_leftover_removes_it() {
        let central = tempfile::tempdir().unwrap();
        let old = central.path().join("old");
        venv(&old, 1);
        let mut config = Config::default();
        config.settings.default_venv_location =
            bombadil_core::model::DefaultVenvLocation::Central {
                path: central.path().to_path_buf(),
            };
        delete_blocking(&Delete::Leftover(old.clone()), &config).unwrap();
        assert!(!old.exists());
    }

    #[test]
    fn deleting_an_extra_environment_removes_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (main, extra) = (dir.path().join("m"), dir.path().join("x"));
        venv(&main, 1);
        venv(&extra, 1);
        let p = project("api", &[&main, &extra]);
        let location = p.environments[1].location.clone();
        let config = Config {
            projects: vec![p.clone()],
            ..Config::default()
        };
        delete_blocking(&Delete::Environment(p.id, location, extra.clone()), &config).unwrap();
        assert!(!extra.exists());
        assert!(main.exists());
    }

    #[test]
    fn freed_space_reads_plainly() {
        assert_eq!(freed_text(3_000_000, 1_741_825), "Freed 1.2 MB");
        assert_eq!(freed_text(10, 10), "Nothing to free");
        assert_eq!(freed_text(10, 20), "Nothing to free");
    }
}
