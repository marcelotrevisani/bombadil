//! The single answer to "what applies to this project".
//!
//! Every setting here has a global value and a per-project override, and
//! combining the two is exactly the kind of small rule that gets re-derived at
//! each call site and then drifts. Sync, the terminal launcher, the drift
//! check and the Preferences UI all ask here instead.
//!
//! Not every setting is in [`Effective`]. Environment variables and indexes
//! *merge* rather than override -- a project's variables layer over the global
//! ones by key -- and `envspec::compose` already owns that. Only the settings
//! where a project replaces one value with another live here.

use crate::model::{Config, Project, PythonPin, TerminalChoice, UvSource};
use crate::terminal::Os;

/// What applies to one project, after its overrides are resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    pub python: PythonPin,
    pub uv_source: UvSource,
    /// `None` when neither the project nor the global config names one. The
    /// caller decides what to do about that -- the GUI's one-press button
    /// detects what is installed rather than guessing.
    pub terminal: Option<TerminalChoice>,
}

/// Resolves `project`'s settings against `config`'s globals for `os`.
///
/// `os` is a parameter rather than `Os::host()` read inside, so a test can ask
/// about a platform it is not running on -- the same shape `terminal::detect`
/// and `terminal::open` already take.
///
/// `project` is passed rather than looked up by id inside: the caller
/// generally holds a clone already, and looking it up here would make this
/// silently answer about a different project when the two disagree.
pub fn effective(config: &Config, project: &Project, os: Os) -> Effective {
    Effective {
        // From the active environment now that the pin lives there. Task 2
        // removes this field from `Effective` entirely; the callers that read
        // it move to asking the environment directly.
        python: project
            .active_environment()
            .map(|environment| environment.python.clone())
            .unwrap_or_else(|| config.settings.python.clone()),
        uv_source: project
            .uv_source
            .resolve(&config.settings.uv_source)
            .clone(),
        // The project's own map first, then the global one, and only for
        // `os`: a project override for macOS must leave the Linux default
        // exactly where it was.
        terminal: project
            .terminals
            .get(&os)
            .or_else(|| config.terminal_defaults.get(&os))
            .cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Environment, Override, Settings, VenvLocation};
    use crate::terminal::TerminalKind;
    use std::collections::BTreeMap;

    fn config_with(settings: Settings, project: Project) -> Config {
        Config {
            settings,
            projects: vec![project],
            ..Config::default()
        }
    }

    #[test]
    fn a_project_that_overrides_nothing_gets_every_global_value() {
        let settings = Settings {
            python: PythonPin::Version("3.12".into()),
            uv_source: UvSource::Bundled,
            ..Settings::default()
        };
        let project = Project::default();
        let mut config = config_with(settings, project.clone());
        config
            .terminal_defaults
            .insert(Os::Linux, TerminalChoice::Detected(TerminalKind::Kitty));

        let got = effective(&config, &project, Os::Linux);

        assert_eq!(got.python, PythonPin::Version("3.12".into()));
        assert_eq!(got.uv_source, UvSource::Bundled);
        assert_eq!(
            got.terminal,
            Some(TerminalChoice::Detected(TerminalKind::Kitty))
        );
    }

    #[test]
    fn a_project_override_replaces_the_global_value() {
        let settings = Settings {
            python: PythonPin::Version("3.12".into()),
            uv_source: UvSource::Bundled,
            ..Settings::default()
        };
        let project = Project {
            environments: vec![Environment {
                location: VenvLocation::Alongside,
                python: PythonPin::Version("3.11".into()),
            }],
            active: VenvLocation::Alongside,
            uv_source: Override::Set(UvSource::FromPath),
            ..Project::default()
        };
        let config = config_with(settings, project.clone());

        let got = effective(&config, &project, Os::Linux);

        assert_eq!(got.python, PythonPin::Version("3.11".into()));
        assert_eq!(got.uv_source, UvSource::FromPath);
    }

    #[test]
    fn a_project_can_unpin_against_a_global_pin() {
        // The reason `PythonPin::Unpinned` is a value and not the absence of
        // one. `Override::Inherit` would hand the global pin straight back.
        let settings = Settings {
            python: PythonPin::Version("3.12".into()),
            ..Settings::default()
        };
        let project = Project {
            environments: vec![Environment {
                location: VenvLocation::Alongside,
                python: PythonPin::Unpinned,
            }],
            active: VenvLocation::Alongside,
            ..Project::default()
        };
        let config = config_with(settings, project.clone());

        assert_eq!(
            effective(&config, &project, Os::Linux).python,
            PythonPin::Unpinned
        );
    }

    #[test]
    fn a_terminal_override_applies_only_to_the_os_it_names() {
        // The whole reason this is a map and not an `Override`. A project
        // opened in iTerm2 on macOS must still open on Linux.
        let project = Project {
            terminals: BTreeMap::from([(
                Os::MacOs,
                TerminalChoice::Detected(TerminalKind::ITerm2),
            )]),
            ..Project::default()
        };
        let mut config = config_with(Settings::default(), project.clone());
        config
            .terminal_defaults
            .insert(Os::Linux, TerminalChoice::Detected(TerminalKind::Kitty));

        assert_eq!(
            effective(&config, &project, Os::MacOs).terminal,
            Some(TerminalChoice::Detected(TerminalKind::ITerm2))
        );
        assert_eq!(
            effective(&config, &project, Os::Linux).terminal,
            Some(TerminalChoice::Detected(TerminalKind::Kitty)),
            "an override for another OS must not take this one's default away"
        );
    }

    #[test]
    fn nothing_configured_anywhere_leaves_the_terminal_unanswered() {
        let project = Project::default();
        let config = config_with(Settings::default(), project.clone());
        assert_eq!(effective(&config, &project, Os::Linux).terminal, None);
    }
}
