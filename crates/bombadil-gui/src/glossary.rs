//! One plain sentence per word of Python packaging jargon. Tooltips and the
//! Help › Glossary card both read this table, so they cannot disagree.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Term {
    Environment,
    Sync,
    LockFile,
    Dependency,
    Upgrade,
    Index,
    Editable,
    Member,
    Interpreter,
    Drifted,
    EnvVars,
    Inherited,
}

impl Term {
    #[cfg(test)]
    pub const ALL: [Term; 12] = [
        Term::Environment,
        Term::Sync,
        Term::LockFile,
        Term::Dependency,
        Term::Upgrade,
        Term::Index,
        Term::Editable,
        Term::Member,
        Term::Interpreter,
        Term::Drifted,
        Term::EnvVars,
        Term::Inherited,
    ];
}

/// Each term, its name on the Glossary card, and its sentence.
pub const TERMS: &[(Term, &str, &str)] = &[
    (
        Term::Environment,
        "Environment (venv)",
        "A private folder with its own Python and packages, so this project can't break others.",
    ),
    (
        Term::Sync,
        "Sync",
        "Makes the environment match exactly what the project declares, adding and removing packages.",
    ),
    (
        Term::LockFile,
        "Lock file (uv.lock)",
        "The exact versions chosen for every package, so every machine installs the same thing.",
    ),
    (
        Term::Dependency,
        "Dependency",
        "A package this project needs, listed in pyproject.toml.",
    ),
    (
        Term::Upgrade,
        "Upgrade",
        "Picks the newest versions the project's rules allow, then syncs.",
    ),
    (
        Term::Index,
        "Index",
        "A server packages are downloaded from; PyPI is the default one.",
    ),
    (
        Term::Editable,
        "Editable",
        "The package is used straight from its folder, so edits apply without reinstalling.",
    ),
    (
        Term::Member,
        "Workspace member",
        "A project inside this one that shares its lock file and environment.",
    ),
    (
        Term::Interpreter,
        "Interpreter",
        "The Python program that runs your code; each environment uses one version.",
    ),
    (
        Term::Drifted,
        "Drifted",
        "The environment exists but no longer matches what the project declares.",
    ),
    (
        Term::EnvVars,
        "Env vars",
        "Settings passed to your code when it runs, such as a database address.",
    ),
    (
        Term::Inherited,
        "Inherited",
        "Comes from the global settings unless this project overrides it.",
    ),
];

pub fn explain(term: Term) -> &'static str {
    TERMS
        .iter()
        .find(|(t, _, _)| *t == term)
        .map_or("", |(_, _, sentence)| sentence)
}

/// The rows in the order the Glossary card shows them.
pub fn alphabetical() -> impl Iterator<Item = &'static (Term, &'static str, &'static str)> {
    let mut rows: Vec<_> = TERMS.iter().collect();
    rows.sort_by_key(|(_, name, _)| name.to_lowercase());
    rows.into_iter()
}

/// While something runs, what it waits for; otherwise `term`'s sentence.
pub fn wait_tip<'a, M: 'a>(
    content: impl Into<iced::Element<'a, M>>,
    waiting: Option<&str>,
    term: Term,
) -> iced::Element<'a, M> {
    match waiting {
        Some(_) => crate::theme::wait_tip(content, waiting),
        None => tip(content, term),
    }
}

/// `content` with `term`'s sentence as its tooltip.
pub fn tip<'a, M: 'a>(
    content: impl Into<iced::Element<'a, M>>,
    term: Term,
) -> iced::Element<'a, M> {
    crate::theme::tip(
        content,
        explain(term),
        iced::widget::tooltip::Position::Bottom,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_term_is_listed_once() {
        for term in Term::ALL {
            let rows = TERMS.iter().filter(|(t, _, _)| *t == term).count();
            assert_eq!(rows, 1, "{term:?}");
        }
        assert_eq!(TERMS.len(), Term::ALL.len());
    }

    #[test]
    fn every_explanation_is_one_short_plain_sentence() {
        // Words a beginner would need explained in turn, and that have no
        // row of their own.
        const JARGON: [&str; 6] = [
            "resolve",
            "wheel",
            "site-packages",
            "PEP",
            "transitive",
            "manifest",
        ];
        for (term, name, sentence) in TERMS {
            assert!(!name.is_empty(), "{term:?}");
            assert!(sentence.ends_with('.'), "{name}: {sentence}");
            assert!(
                !sentence.trim_end_matches('.').contains(". "),
                "{name}: one sentence"
            );
            assert!(sentence.len() <= 110, "{name}: {} chars", sentence.len());
            for word in JARGON {
                assert!(!sentence.contains(word), "{name} uses {word}");
            }
            assert_eq!(explain(*term), *sentence);
        }
    }

    #[test]
    fn the_card_lists_terms_alphabetically() {
        let names: Vec<&str> = alphabetical().map(|(_, name, _)| *name).collect();
        let mut sorted = names.clone();
        sorted.sort_by_key(|n| n.to_lowercase());
        assert_eq!(names, sorted);
        assert_eq!(names.len(), TERMS.len());
    }
}
