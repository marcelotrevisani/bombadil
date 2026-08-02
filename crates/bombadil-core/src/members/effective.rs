use super::DiscoveredMember;
use crate::model::{MemberOrigin, MemberSelection};
use std::path::PathBuf;

/// One member with the user's choice applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveMember {
    /// Relative to the project directory for in-tree members, absolute for added ones.
    pub path: PathBuf,
    pub name: Option<String>,
    pub origin: MemberOrigin,
    pub is_workspace_member: bool,
    pub selected: bool,
}

/// Apply a saved selection to what discovery just found.
///
/// Precedence: an explicit `entries` row wins over `all_discovered` for the
/// same path, so a deliberate deselection survives rediscovery. Discovered
/// paths with no entry take `all_discovered`, which is why a package added to
/// the repository later is picked up automatically. `Added` entries are
/// always included, since they exist only because the user named them.
///
/// Entries are matched to discovered members by path: for a `Discovered`
/// entry this is the stored relative path, compared against
/// `DiscoveredMember::relative_path`.
pub fn effective(
    selection: &MemberSelection,
    discovered: &[DiscoveredMember],
) -> Vec<EffectiveMember> {
    let mut result: Vec<EffectiveMember> = discovered
        .iter()
        .map(|member| {
            let entry = selection
                .entries
                .iter()
                .find(|e| e.path == member.relative_path);
            EffectiveMember {
                path: member.relative_path.clone(),
                // Prefer the freshly discovered name: it reflects the manifest
                // as it is now, whereas the entry's stored name is only a
                // fallback for when discovery has nothing (an Added member
                // outside the tree).
                name: member
                    .name
                    .clone()
                    .or_else(|| entry.and_then(|e| e.name.clone())),
                origin: MemberOrigin::Discovered,
                is_workspace_member: member.is_workspace_member,
                selected: entry.map_or(selection.all_discovered, |e| e.selected),
            }
        })
        .collect();

    for entry in &selection.entries {
        if discovered.iter().any(|m| m.relative_path == entry.path) {
            continue; // already represented above, with entry precedence applied
        }
        match entry.origin {
            // Named explicitly by the user; never dropped just for not (or no
            // longer) being on disk where discovery looks.
            MemberOrigin::Added => result.push(EffectiveMember {
                path: entry.path.clone(),
                name: entry.name.clone(),
                origin: MemberOrigin::Added,
                is_workspace_member: false,
                selected: entry.selected,
            }),
            // A stale deviation for a directory discovery no longer finds.
            MemberOrigin::Discovered => {}
        }
    }

    result.sort_by(|a, b| a.path.cmp(&b.path));
    result
}

/// The `--package` arguments for a selection.
///
/// Members with no `[project].name` are filtered out: uv cannot select them
/// and passing nothing for them is better than failing the whole sync. The
/// caller shows them disabled. Filtering belongs here, not in the view.
pub fn selected_package_names(effective: &[EffectiveMember]) -> Vec<String> {
    // `effective`'s own contract sorts its input by path, so this is already
    // deterministic without a second sort.
    effective
        .iter()
        .filter(|m| m.selected)
        .filter_map(|m| m.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MemberRef;

    fn discovered(path: &str, name: Option<&str>, is_workspace_member: bool) -> DiscoveredMember {
        DiscoveredMember {
            relative_path: PathBuf::from(path),
            name: name.map(String::from),
            is_workspace_member,
        }
    }

    #[test]
    fn discovered_with_no_entry_is_selected_when_all_discovered_is_true() {
        let selection = MemberSelection {
            all_discovered: true,
            entries: Vec::new(),
        };
        let found = [discovered("packages/alpha", Some("alpha"), true)];

        let got = effective(&selection, &found);

        assert_eq!(got.len(), 1);
        assert!(got[0].selected);
    }

    #[test]
    fn discovered_with_no_entry_is_not_selected_when_all_discovered_is_false() {
        let selection = MemberSelection {
            all_discovered: false,
            entries: Vec::new(),
        };
        let found = [discovered("packages/alpha", Some("alpha"), true)];

        let got = effective(&selection, &found);

        assert!(!got[0].selected);
    }

    #[test]
    fn an_explicit_deselection_wins_over_all_discovered_true() {
        // The precedence rule: a deliberate deselection must survive
        // rediscovery even though all_discovered says "select everything".
        let selection = MemberSelection {
            all_discovered: true,
            entries: vec![MemberRef {
                path: PathBuf::from("packages/alpha"),
                origin: MemberOrigin::Discovered,
                selected: false,
                name: None,
            }],
        };
        let found = [discovered("packages/alpha", Some("alpha"), true)];

        let got = effective(&selection, &found);

        assert!(!got[0].selected);
    }

    #[test]
    fn an_explicit_selection_wins_over_all_discovered_false() {
        let selection = MemberSelection {
            all_discovered: false,
            entries: vec![MemberRef {
                path: PathBuf::from("packages/alpha"),
                origin: MemberOrigin::Discovered,
                selected: true,
                name: None,
            }],
        };
        let found = [discovered("packages/alpha", Some("alpha"), true)];

        let got = effective(&selection, &found);

        assert!(got[0].selected);
    }

    #[test]
    fn an_added_entry_appears_even_though_discovery_never_finds_it() {
        let selection = MemberSelection {
            all_discovered: true,
            entries: vec![MemberRef {
                path: PathBuf::from("/work/shared-lib"),
                origin: MemberOrigin::Added,
                selected: true,
                name: Some("shared-lib".into()),
            }],
        };

        let got = effective(&selection, &[]);

        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, PathBuf::from("/work/shared-lib"));
        assert_eq!(got[0].origin, MemberOrigin::Added);
        assert!(got[0].selected);
        assert_eq!(got[0].name.as_deref(), Some("shared-lib"));
    }

    #[test]
    fn a_stale_discovered_entry_for_a_vanished_directory_is_dropped() {
        let selection = MemberSelection {
            all_discovered: true,
            entries: vec![MemberRef {
                path: PathBuf::from("packages/gone"),
                origin: MemberOrigin::Discovered,
                selected: false,
                name: None,
            }],
        };

        let got = effective(&selection, &[]);

        assert!(got.is_empty(), "got {got:?}");
    }

    #[test]
    fn selected_package_names_keeps_only_selected_named_members_in_path_order() {
        let selection = MemberSelection {
            all_discovered: true,
            entries: vec![
                MemberRef {
                    path: PathBuf::from("packages/unselected"),
                    origin: MemberOrigin::Discovered,
                    selected: false,
                    name: None,
                },
                MemberRef {
                    path: PathBuf::from("a-added"),
                    origin: MemberOrigin::Added,
                    selected: true,
                    name: Some("added-lib".into()),
                },
            ],
        };
        let found = [
            discovered("packages/nameless", None, true),
            discovered("packages/unselected", Some("unselected"), true),
            discovered("packages/zeta", Some("zeta"), true),
        ];

        let members = effective(&selection, &found);
        let names = selected_package_names(&members);

        // "a-added" sorts before "packages/zeta" by path; "packages/unselected"
        // is excluded (deselected) and "packages/nameless" has no name.
        assert_eq!(names, vec!["added-lib".to_string(), "zeta".to_string()]);
    }
}
