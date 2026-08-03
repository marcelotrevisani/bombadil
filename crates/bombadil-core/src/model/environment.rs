//! One virtual environment belonging to a project.
//!
//! Identified by its `location` and nothing else: two environments cannot
//! share a directory, so the directory is already a unique key, and a typed
//! name would be a second identity to keep in step with the folder it points
//! at.

use super::{PythonPin, VenvLocation};
use serde::{Deserialize, Serialize};

/// A virtual environment a project can build, sync into and open a terminal
/// on.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Environment {
    #[serde(default)]
    pub location: VenvLocation,
    /// The interpreter this environment is built with, and the one Sync
    /// asserts with `-p`. `Unpinned` lets uv choose, subject to the
    /// manifest's `requires-python`.
    ///
    /// Per environment rather than per project: carrying different
    /// interpreters is the whole reason to have more than one.
    #[serde(default)]
    pub python: PythonPin,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Project;
    use std::path::PathBuf;

    fn at(path: &str) -> VenvLocation {
        VenvLocation::Custom {
            path: PathBuf::from(path),
        }
    }

    #[test]
    fn the_active_environment_is_found_by_location() {
        let a = Environment {
            location: VenvLocation::Alongside,
            python: PythonPin::Unpinned,
        };
        let b = Environment {
            location: at("/envs/b"),
            python: PythonPin::Version("3.11".into()),
        };
        let project = Project {
            environments: vec![a.clone(), b.clone()],
            active: b.location.clone(),
            ..Project::default()
        };

        assert_eq!(project.active_environment(), Some(&b));
    }

    #[test]
    fn an_active_that_names_nothing_falls_back_to_the_first() {
        // `active` is a location, not an index, so removing an environment
        // can leave it naming one that is gone. Falling back keeps every
        // caller -- sync, the terminal launcher, the drift glyph -- working
        // against a real directory rather than each inventing its own
        // recovery and then disagreeing about which is current.
        let a = Environment {
            location: VenvLocation::Alongside,
            python: PythonPin::Unpinned,
        };
        let project = Project {
            environments: vec![a.clone()],
            active: at("/gone"),
            ..Project::default()
        };

        assert_eq!(project.active_environment(), Some(&a));
    }

    #[test]
    fn a_project_with_no_environments_has_no_active_one() {
        // Only reachable between removing the last environment and adding
        // another. Every caller has to handle it, so it is `None` rather than
        // a synthesised default nobody chose and nobody would recognise.
        assert_eq!(Project::default().active_environment(), None);
    }

    #[test]
    fn two_environments_of_one_project_keep_their_own_pins() {
        // The property the whole feature rests on: reading one environment's
        // pin must never answer with another's.
        let project = Project {
            environments: vec![
                Environment {
                    location: VenvLocation::Alongside,
                    python: PythonPin::Version("3.11".into()),
                },
                Environment {
                    location: at("/envs/api-312"),
                    python: PythonPin::Version("3.12".into()),
                },
            ],
            active: at("/envs/api-312"),
            ..Project::default()
        };

        assert_eq!(
            project.active_environment().map(|e| e.python.clone()),
            Some(PythonPin::Version("3.12".into()))
        );
        assert_eq!(
            project.environments[0].python,
            PythonPin::Version("3.11".into()),
            "the inactive one keeps its own"
        );
    }
}
