use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How a member came to be in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberOrigin {
    /// Found by walking the project's own tree.
    Discovered,
    /// Named explicitly by the user, possibly outside the tree.
    Added,
}

/// One package that may be installed in editable mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRef {
    /// Relative to the project directory when the member is inside the tree,
    /// absolute when it is outside. Relative keeps a moved checkout working.
    pub path: PathBuf,
    pub origin: MemberOrigin,
    pub selected: bool,
}

/// Which of a project's packages are installed in editable mode.
///
/// `all_discovered` is a flag rather than an expanded list on purpose. Spec 5a
/// selects every discovered member by default; storing that as a list would
/// mean a package added to the repository later arrived unselected, and the
/// user would have to spot it. As a flag it means "whatever discovery finds",
/// so new packages are picked up on their own. `entries` therefore holds only
/// the deviations: explicit deselections, and projects added by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberSelection {
    #[serde(default = "default_true")]
    pub all_discovered: bool,
    #[serde(default)]
    pub entries: Vec<MemberRef>,
}

fn default_true() -> bool {
    true
}

impl Default for MemberSelection {
    fn default() -> Self {
        Self {
            all_discovered: true,
            entries: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_fresh_selection_takes_everything_discovery_finds() {
        // Spec 5a: all discovered members are selected by default, and a
        // package added to the repo later must be picked up without the user
        // having to notice it.
        let selection = MemberSelection::default();
        assert!(selection.all_discovered);
        assert!(selection.entries.is_empty());
    }

    #[test]
    fn a_selection_round_trips_through_toml() {
        let selection = MemberSelection {
            all_discovered: false,
            entries: vec![
                MemberRef {
                    path: PathBuf::from("packages/alpha"),
                    origin: MemberOrigin::Discovered,
                    selected: true,
                },
                MemberRef {
                    path: PathBuf::from("/work/shared-lib"),
                    origin: MemberOrigin::Added,
                    selected: false,
                },
            ],
        };

        let text = toml::to_string_pretty(&selection).expect("serialize");
        let back: MemberSelection = toml::from_str(&text).expect("deserialize");

        assert_eq!(back, selection);
    }

    #[test]
    fn an_absent_members_table_deserializes_to_the_default() {
        // Config files written before this feature existed must keep loading,
        // which is why no schema bump is needed.
        let back: MemberSelection = toml::from_str("").expect("deserialize");
        assert_eq!(back, MemberSelection::default());
    }
}
