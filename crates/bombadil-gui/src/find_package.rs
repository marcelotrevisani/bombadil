//! "Find package": which projects use a package, from each project's
//! `uv.lock`. Read on the machine; nothing is asked of uv or the network.

use crate::app::Message;
use crate::deps::normalise;
use crate::theme;
use bombadil_core::model::Project;

/// What one project's lock file and manifest say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub project: uuid::Uuid,
    pub label: String,
    /// `(name, version)` from `uv.lock`; `None` when there is no lock file,
    /// or one this build can't read.
    pub packages: Option<Vec<(String, String)>>,
    /// Normalised names the manifest asks for, in any group or extra.
    pub declared: Vec<String>,
}

/// One project that has a matching package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub project: uuid::Uuid,
    pub label: String,
    pub name: String,
    pub version: String,
    pub declared: bool,
}

/// Every package whose normalised name contains the normalised `query`,
/// by project label then name. An empty query finds nothing.
pub fn find(index: &[Entry], query: &str) -> Vec<Hit> {
    let want = normalise(query.trim());
    if want.is_empty() {
        return Vec::new();
    }
    let want = want.as_str();
    let mut hits: Vec<Hit> = index
        .iter()
        .flat_map(|e| {
            e.packages
                .iter()
                .flatten()
                .filter_map(move |(name, version)| {
                    let key = normalise(name);
                    key.contains(want).then(|| Hit {
                        project: e.project,
                        label: e.label.clone(),
                        name: name.clone(),
                        version: version.clone(),
                        declared: e.declared.contains(&key),
                    })
                })
        })
        .collect();
    hits.sort_by(|a, b| a.label.cmp(&b.label).then(a.name.cmp(&b.name)));
    hits
}

/// How many projects have no lock file this build can read.
pub fn unsynced(index: &[Entry]) -> usize {
    index.iter().filter(|e| e.packages.is_none()).count()
}

/// The name a requirement string starts with: `Requests>=2` -> `Requests`.
fn requirement_name(requirement: &str) -> &str {
    let end = requirement
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .unwrap_or(requirement.len());
    &requirement[..end]
}

/// Reads every project's `uv.lock` and manifest. Blocking: run off the
/// render thread.
pub fn index_blocking(projects: &[Project]) -> Vec<Entry> {
    projects
        .iter()
        .map(|p| {
            let lock = p.pyproject_path.with_file_name("uv.lock");
            let packages = std::fs::read_to_string(lock)
                .ok()
                .and_then(|t| bombadil_core::housekeeping::lock_packages(&t));
            let declared = bombadil_core::pyproject::load(&p.pyproject_path)
                .map(|m| {
                    let groups =
                        m.dependency_groups
                            .into_values()
                            .flatten()
                            .filter_map(|g| match g {
                                bombadil_core::pyproject::GroupEntry::Requirement(r) => Some(r),
                                bombadil_core::pyproject::GroupEntry::IncludeGroup(_) => None,
                            });
                    m.dependencies
                        .into_iter()
                        .chain(m.optional_dependencies.into_values().flatten())
                        .chain(groups)
                        .map(|r| normalise(requirement_name(&r)))
                        .collect()
                })
                .unwrap_or_default();
            Entry {
                project: p.id,
                label: p.label.clone(),
                packages,
                declared,
            }
        })
        .collect()
}

/// The dialog. Deliberately dumb.
pub fn view<'a>(query: &str, index: Option<&[Entry]>) -> iced::Element<'a, Message> {
    use iced::widget::{button, column, row, text, text_input};
    let muted = |t: String| text(t).size(theme::BODY).style(theme::text_muted);
    let mut page = column![
        text("Find package")
            .font(theme::FONT_PROSE_SEMIBOLD)
            .size(theme::DISPLAY),
        text_input("Package name, e.g. requests", query)
            .on_input(Message::FindQuery)
            .style(theme::text_field),
    ]
    .spacing(theme::SPACE_2);
    let Some(index) = index else {
        return page.push(muted("Reading projects\u{2026}".into())).into();
    };
    let hits = find(index, query);
    if !query.trim().is_empty() && hits.is_empty() {
        page = page.push(muted("No project uses a package by that name.".into()));
    }
    for hit in hits {
        page = page.push(
            button(
                row![
                    text(hit.label.clone())
                        .size(theme::BODY)
                        .width(iced::Length::FillPortion(2)),
                    text(hit.name.clone())
                        .font(theme::FONT_DATA)
                        .size(theme::DATA)
                        .width(iced::Length::FillPortion(2)),
                    text(hit.version.clone())
                        .font(theme::FONT_DATA)
                        .size(theme::DATA)
                        .width(iced::Length::FillPortion(1)),
                    muted(
                        if hit.declared {
                            "declared"
                        } else {
                            "pulled in"
                        }
                        .into()
                    ),
                ]
                .spacing(theme::SPACE_2),
            )
            .on_press(Message::FindPicked(hit.project, hit.name.clone()))
            .padding(theme::SPACE_1)
            .width(iced::Length::Fill)
            .style(theme::button_quiet),
        );
    }
    let n = unsynced(index);
    if n > 0 {
        page = page.push(muted(match n {
            1 => "1 project isn't synced yet".to_string(),
            n => format!("{n} projects aren't synced yet"),
        }));
    }
    page.push(
        button(text("Close").size(theme::BODY))
            .on_press(Message::FindClosed)
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(theme::button_quiet),
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(label: &str, packages: Option<&[(&str, &str)]>, declared: &[&str]) -> Entry {
        Entry {
            project: uuid::Uuid::new_v4(),
            label: label.into(),
            packages: packages.map(|p| {
                p.iter()
                    .map(|(n, v)| (n.to_string(), v.to_string()))
                    .collect()
            }),
            declared: declared.iter().map(|d| d.to_string()).collect(),
        }
    }

    #[test]
    fn matching_ignores_case_and_separators() {
        let index = [entry(
            "api",
            Some(&[("Typing_Extensions", "4.12.2"), ("six", "1.16.0")]),
            &[],
        )];
        let hits = find(&index, "typing-ext");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Typing_Extensions");
        assert_eq!(hits[0].version, "4.12.2");
    }

    #[test]
    fn an_empty_query_finds_nothing() {
        let index = [entry("api", Some(&[("six", "1")]), &[])];
        assert!(find(&index, "  ").is_empty());
    }

    #[test]
    fn declared_is_marked_and_results_are_ordered() {
        let index = [
            entry("web", Some(&[("requests", "2.32")]), &[]),
            entry("api", Some(&[("requests", "2.31")]), &["requests"]),
        ];
        let hits = find(&index, "requests");
        assert_eq!(hits[0].label, "api");
        assert!(hits[0].declared);
        assert!(!hits[1].declared);
    }

    #[test]
    fn an_unreadable_lock_is_skipped() {
        let index = [
            entry("api", None, &[]),
            entry("web", Some(&[("six", "1")]), &[]),
        ];
        assert_eq!(find(&index, "six").len(), 1);
        assert_eq!(unsynced(&index), 1);
    }

    #[test]
    fn index_reads_lock_and_manifest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"api\"\ndependencies = [\"Requests>=2\"]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("uv.lock"),
            "version = 1\n\n[[package]]\nname = \"requests\"\nversion = \"2.32.3\"\n",
        )
        .unwrap();
        let project = bombadil_core::model::Project {
            label: "api".into(),
            pyproject_path: dir.path().join("pyproject.toml"),
            ..Default::default()
        };
        let index = index_blocking(&[project]);
        assert_eq!(index[0].packages.as_deref().unwrap().len(), 1);
        assert_eq!(index[0].declared, ["requests"]);
        assert!(find(&index, "requests")[0].declared);
    }
}
