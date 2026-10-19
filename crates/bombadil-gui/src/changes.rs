//! What an environment-changing action did: the installed list before
//! against the one after.

use bombadil_core::osv::version_cmp;
use bombadil_core::uv::results::InstalledPackage;
use std::cmp::Ordering;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Added {
        name: String,
        version: String,
    },
    Removed {
        name: String,
        version: String,
    },
    Changed {
        name: String,
        from: String,
        to: String,
    },
}

impl Change {
    fn name(&self) -> &str {
        match self {
            Change::Added { name, .. }
            | Change::Removed { name, .. }
            | Change::Changed { name, .. } => name,
        }
    }
}

fn by_name(list: &[InstalledPackage]) -> BTreeMap<String, (&str, &str)> {
    list.iter()
        .map(|p| {
            (
                crate::deps::normalise(&p.name),
                (p.name.as_str(), p.version.as_str()),
            )
        })
        .collect()
}

pub fn diff(
    before: Option<&[InstalledPackage]>,
    after: &[InstalledPackage],
) -> Option<Vec<Change>> {
    let before = by_name(before?);
    let after = by_name(after);
    let mut out = Vec::new();
    for (key, (name, version)) in &after {
        match before.get(key) {
            None => out.push(Change::Added {
                name: name.to_string(),
                version: version.to_string(),
            }),
            Some((_, old)) if old != version => out.push(Change::Changed {
                name: name.to_string(),
                from: old.to_string(),
                to: version.to_string(),
            }),
            Some(_) => {}
        }
    }
    for (key, (name, version)) in &before {
        if !after.contains_key(key) {
            out.push(Change::Removed {
                name: name.to_string(),
                version: version.to_string(),
            });
        }
    }
    out.sort_by_key(|c| crate::deps::normalise(c.name()));
    Some(out)
}

fn short(c: &Change) -> String {
    match c {
        Change::Added { name, .. } => format!("added {name}"),
        Change::Removed { name, .. } => format!("removed {name}"),
        Change::Changed { name, from, to } => format!("{name} {from} → {to}"),
    }
}

pub fn summary(changes: Option<&[Change]>, after_count: usize) -> String {
    let Some(changes) = changes else {
        return format!(
            "installed {}",
            crate::health::plural(after_count, "package", "packages")
        );
    };
    if changes.is_empty() {
        return "nothing changed".into();
    }
    let shown: Vec<String> = changes.iter().take(3).map(short).collect();
    match changes.len() {
        n if n > 3 => format!("{} and {} more", shown.join(", "), n - 3),
        _ => shown.join(", "),
    }
}

pub fn drawer_lines(changes: &[Change]) -> Vec<String> {
    changes
        .iter()
        .map(|c| match c {
            Change::Added { name, version } => format!("+ {name} {version}"),
            Change::Removed { name, version } => format!("- {name} {version}"),
            Change::Changed { name, from, to } => {
                let arrow = if version_cmp(to, from) == Ordering::Less {
                    '↓'
                } else {
                    '↑'
                };
                format!("{arrow} {name} {from} → {to}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, version: &str) -> InstalledPackage {
        InstalledPackage {
            name: name.into(),
            version: version.into(),
            editable_project_location: None,
        }
    }

    #[test]
    fn a_diff_names_what_was_added_removed_and_moved() {
        let before = [
            pkg("numpy", "2.1.0"),
            pkg("six", "1.16.0"),
            pkg("Typing_Extensions", "4.0"),
        ];
        let after = [
            pkg("numpy", "2.3.1"),
            pkg("polars", "1.9.0"),
            pkg("typing-extensions", "4.0"),
        ];
        let d = diff(Some(&before), &after).unwrap();
        assert_eq!(
            d,
            [
                Change::Changed {
                    name: "numpy".into(),
                    from: "2.1.0".into(),
                    to: "2.3.1".into()
                },
                Change::Added {
                    name: "polars".into(),
                    version: "1.9.0".into()
                },
                Change::Removed {
                    name: "six".into(),
                    version: "1.16.0".into()
                },
            ]
        );
        assert_eq!(
            summary(Some(&d), 3),
            "numpy 2.1.0 → 2.3.1, added polars, removed six"
        );
        assert_eq!(
            drawer_lines(&d),
            ["↑ numpy 2.1.0 → 2.3.1", "+ polars 1.9.0", "- six 1.16.0"]
        );
    }

    #[test]
    fn long_summaries_are_capped() {
        let after: Vec<_> = (0..5).map(|i| pkg(&format!("p{i}"), "1")).collect();
        let d = diff(Some(&[]), &after).unwrap();
        assert_eq!(
            summary(Some(&d), 5),
            "added p0, added p1, added p2 and 2 more"
        );
    }

    #[test]
    fn nothing_changed_says_so() {
        let same = [pkg("a", "1")];
        assert_eq!(
            summary(Some(&diff(Some(&same), &same).unwrap()), 1),
            "nothing changed"
        );
    }

    #[test]
    fn a_first_install_is_counted() {
        assert_eq!(diff(None, &[pkg("a", "1")]), None);
        assert_eq!(summary(None, 12), "installed 12 packages");
        assert_eq!(summary(None, 1), "installed 1 package");
    }

    #[test]
    fn a_downgrade_points_down() {
        let d = diff(Some(&[pkg("a", "2.0")]), &[pkg("a", "1.5")]).unwrap();
        assert_eq!(drawer_lines(&d), ["↓ a 2.0 → 1.5"]);
    }
}
