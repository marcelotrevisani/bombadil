//! Matches interpreters uv can see against a project's `requires-python`,
//! and picks the one to preselect in the add-project dialog.
//!
//! Spec §8: the interpreter selector must be pre-selected to satisfy
//! `requires-python`, and must offer to install a suitable interpreter
//! through uv when none is available.

use bombadil_core::uv::results::Interpreter;
use std::cmp::Ordering;

/// One parsed `requires-python` clause. Deliberately not a full PEP 440
/// grammar -- see `satisfying` for which forms are handled and why an
/// unrecognised one is not treated as an error.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Clause {
    /// `>=X` -- candidate must be >= X.
    AtLeast(Vec<u64>),
    /// `>X` -- candidate must be > X.
    GreaterThan(Vec<u64>),
    /// `==X` -- candidate's release segments must equal X's exactly.
    Exact(Vec<u64>),
    /// A bare version, or `==X.*` -- candidate's release segments must
    /// start with X's.
    Prefix(Vec<u64>),
    /// `~=X.Y[.Z..]`, PEP 440's "compatible release" operator -- candidate
    /// must be >= X.Y[.Z..] and share every segment but the last.
    Compatible(Vec<u64>),
}

impl Clause {
    fn matches(&self, candidate: &[u64]) -> bool {
        match self {
            Clause::AtLeast(v) => cmp_segments(candidate, v) != Ordering::Less,
            Clause::GreaterThan(v) => cmp_segments(candidate, v) == Ordering::Greater,
            Clause::Exact(v) => cmp_segments(candidate, v) == Ordering::Equal,
            Clause::Prefix(v) => starts_with(candidate, v),
            Clause::Compatible(v) => {
                v.len() >= 2
                    && starts_with(candidate, &v[..v.len() - 1])
                    && cmp_segments(candidate, v) != Ordering::Less
            }
        }
    }
}

/// The release segments of a version string: the leading digit run of each
/// dot-separated component, stopping at the first component that does not
/// start with a digit. `"3.15.0b4"` -> `[3, 15, 0]` (the `b4` pre-release
/// suffix is dropped); `"3.10.13"` -> `[3, 10, 13]`, unchanged.
///
/// This is deliberately not a full PEP 440 version parser: it ignores
/// epochs, pre/post/dev segments, and local version identifiers. That is
/// enough for comparing the version strings uv actually reports for
/// interpreters -- unadorned dotted numbers, apart from the occasional
/// pre-release build like the one above -- without pulling in a real PEP
/// 440 parser for a comparison this narrow.
fn release_segments(s: &str) -> Vec<u64> {
    s.split('.')
        .map_while(|part| {
            let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                None
            } else {
                digits.parse().ok()
            }
        })
        .collect()
}

/// Compares two release-segment vectors as dotted version tuples, treating a
/// missing trailing segment as `0` (so `[3, 11]` == `[3, 11, 0]`).
fn cmp_segments(a: &[u64], b: &[u64]) -> Ordering {
    let len = a.len().max(b.len());
    for i in 0..len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    Ordering::Equal
}

fn starts_with(candidate: &[u64], prefix: &[u64]) -> bool {
    candidate.len() >= prefix.len() && candidate[..prefix.len()] == *prefix
}

fn non_empty(v: Vec<u64>) -> Option<Vec<u64>> {
    (!v.is_empty()).then_some(v)
}

/// Parses one clause of a (possibly comma-separated) `requires-python`
/// string. `None` means this clause is outside the subset handled here --
/// PEP 440's `<`, `<=`, `!=`, `===`, epochs, and local versions are all real
/// syntax that a manifest could use but this parser does not attempt.
fn parse_clause(raw: &str) -> Option<Clause> {
    let raw = raw.trim();
    let (op, rest) = if let Some(rest) = raw.strip_prefix(">=") {
        (">=", rest)
    } else if let Some(rest) = raw.strip_prefix("==") {
        ("==", rest)
    } else if let Some(rest) = raw.strip_prefix("~=") {
        ("~=", rest)
    } else if let Some(rest) = raw.strip_prefix('>') {
        (">", rest)
    } else {
        ("", raw)
    };
    let rest = rest.trim();

    match op {
        ">=" => Some(Clause::AtLeast(non_empty(release_segments(rest))?)),
        ">" => Some(Clause::GreaterThan(non_empty(release_segments(rest))?)),
        "==" => match rest.strip_suffix(".*") {
            Some(prefix) => Some(Clause::Prefix(non_empty(release_segments(prefix))?)),
            None => Some(Clause::Exact(non_empty(release_segments(rest))?)),
        },
        "~=" => {
            let segments = non_empty(release_segments(rest))?;
            (segments.len() >= 2).then_some(Clause::Compatible(segments))
        }
        // A bare version: no recognised operator, and `rest` has to
        // actually look like one (start with a digit) rather than some
        // other syntax this parser does not understand, e.g. `<3.13`.
        "" if rest.starts_with(|c: char| c.is_ascii_digit()) => {
            Some(Clause::Prefix(non_empty(release_segments(rest))?))
        }
        _ => None,
    }
}

/// Parses every comma-separated clause of `requires_python`. `None` if any
/// one of them falls outside the subset `parse_clause` understands, or if
/// the string is empty -- see `satisfying` for why the caller must treat
/// that as "offer everything," not "offer nothing."
fn parse_requirement(requires_python: &str) -> Option<Vec<Clause>> {
    requires_python
        .split(',')
        .map(parse_clause)
        .collect::<Option<Vec<_>>>()
        .filter(|clauses| !clauses.is_empty())
}

/// Whether a single Python `version` string satisfies `requires_python`.
///
/// The single-candidate form of the rule `satisfying` applies to a whole
/// list -- extracted so a caller holding one version, not an `Interpreter`
/// list, does not have to wrap it in a throwaway `Interpreter` just to reuse
/// the comparator. `sidebar::state` is that caller: a venv's Python version
/// is a bare string, not a discovered `Interpreter`.
///
/// Handles `>=`, `>`, `==` (including the `==X.*` wildcard), `~=`, and bare
/// versions, comma-separated -- the forms that cover the overwhelming
/// majority of real `requires-python` manifests. PEP 440 also defines `<`,
/// `<=`, `!=`, `===`, epochs, pre/post/dev qualifiers and local versions,
/// none of which are implemented here.
///
/// When `requires_python` is absent, or present but this parser cannot make
/// sense of it (any single clause outside the subset above), the version is
/// treated as satisfying it rather than not. The two failure modes are not
/// symmetric: for `satisfying`, an over-strict filter can make a project
/// impossible to add through the UI at all, with no recourse. For
/// `sidebar::state`, the same permissive fallback means a venv is never
/// marked `Drifted` just because its manifest's `requires-python` used
/// syntax this parser does not read -- that would be a false alarm over a
/// working environment. An over-permissive filter or check just lets
/// through what uv itself will still catch, with a clear error, at `sync`
/// time. Silently permissive beats silently strict only because
/// "permissive" still leaves a path forward and "strict" does not -- which
/// is exactly the asymmetry that makes the fallback direction a deliberate
/// choice rather than a coin flip.
pub fn version_satisfies(version: &str, requires_python: Option<&str>) -> bool {
    match requires_python.and_then(parse_requirement) {
        None => true,
        Some(clauses) => {
            let segments = release_segments(version);
            clauses.iter().all(|clause| clause.matches(&segments))
        }
    }
}

/// The interpreters, from `interpreters`, whose version satisfies
/// `requires_python`. See [`version_satisfies`] for exactly which forms of
/// `requires_python` are handled and which fallback direction an unparseable
/// one takes -- this is that same check, run over a whole list.
pub fn satisfying<'a>(
    interpreters: &'a [Interpreter],
    requires_python: Option<&str>,
) -> Vec<&'a Interpreter> {
    interpreters
        .iter()
        .filter(|i| version_satisfies(&i.version, requires_python))
        .collect()
}

/// The interpreter to preselect in the add-project dialog: the newest one
/// satisfying `requires_python`, by release-segment ordering. `None` when
/// nothing does -- either because every discovered interpreter is too old
/// (or too new, for an `==`/`~=` requirement), or because `requires_python`
/// could not be parsed and `satisfying` fell back to "offer everything" but
/// that "everything" is empty.
///
/// This must never fall back to picking *something* the way `satisfying`
/// falls back to offering everything: silently preselecting an interpreter
/// that violates `requires-python` would build a venv that fails later, far
/// from its cause -- the exact silent-divergence failure this project has
/// spent several plans eliminating. Preselecting nothing here, so the
/// caller can offer to install a suitable interpreter through uv instead,
/// keeps the failure visible at the point the user can still act on it.
pub fn preselect<'a>(
    interpreters: &'a [Interpreter],
    requires_python: Option<&str>,
) -> Option<&'a Interpreter> {
    satisfying(interpreters, requires_python)
        .into_iter()
        .max_by(|a, b| cmp_segments(&release_segments(&a.version), &release_segments(&b.version)))
}

/// A plain version string to hand `uv python install` when nothing
/// discovered satisfies `requires_python` -- the "offer to install" half of
/// spec §8. Takes the version out of the requirement's *first*
/// comma-separated clause: in every real `requires-python` this project has
/// seen (`>=3.11`, `>=3.11,<4`, `~=3.11.2`), the first clause names the
/// floor, which is the version worth installing.
///
/// `None` when there is no requirement, or its first clause is not a
/// version this parser's subset recognises -- installing an arbitrary
/// interpreter to satisfy a requirement nobody could confirm is "suitable"
/// is worse than leaving the choice to the user instead of guessing.
pub fn suggested_install_version(requires_python: Option<&str>) -> Option<String> {
    let first = requires_python?.split(',').next()?.trim();
    let segments = match parse_clause(first)? {
        Clause::AtLeast(v)
        | Clause::GreaterThan(v)
        | Clause::Exact(v)
        | Clause::Prefix(v)
        | Clause::Compatible(v) => v,
    };
    Some(
        segments
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join("."),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// `Interpreter` carries `key`, `version`, `path` (`None` when uv can
    /// download it but it is not installed) and `implementation` -- see
    /// `bombadil_core::uv::results::Interpreter`. Only `version` matters to
    /// this module, so the rest are given plausible fixed values.
    fn interp(version: &str) -> Interpreter {
        Interpreter {
            key: format!("cpython-{version}-x86_64-unknown-linux-gnu"),
            version: version.to_string(),
            path: Some(PathBuf::from(format!("/usr/bin/python{version}"))),
            implementation: "cpython".to_string(),
        }
    }

    fn downloadable(version: &str) -> Interpreter {
        Interpreter {
            path: None,
            ..interp(version)
        }
    }

    #[test]
    fn only_interpreters_satisfying_the_requirement_are_offered() {
        let all = vec![interp("3.10.13"), interp("3.11.9"), interp("3.12.4")];
        let got = satisfying(&all, Some(">=3.11"));
        let versions: Vec<&str> = got.iter().map(|i| i.version.as_str()).collect();
        assert_eq!(versions, vec!["3.11.9", "3.12.4"]);
    }

    #[test]
    fn no_requirement_offers_everything() {
        // A manifest without requires-python constrains nothing; filtering to
        // an empty list would make the project uncreatable.
        let all = vec![interp("3.10.13"), interp("3.12.4")];
        assert_eq!(satisfying(&all, None).len(), 2);
    }

    #[test]
    fn the_preselection_is_the_newest_satisfying_interpreter() {
        let all = vec![interp("3.11.9"), interp("3.12.4"), interp("3.10.13")];
        assert_eq!(
            preselect(&all, Some(">=3.11")).map(|i| i.version.as_str()),
            Some("3.12.4")
        );
    }

    #[test]
    fn nothing_satisfying_preselects_nothing_rather_than_the_wrong_one() {
        // Silently preselecting an interpreter that violates requires-python
        // produces a venv that fails later, far from the cause.
        let all = vec![interp("3.9.18")];
        assert_eq!(preselect(&all, Some(">=3.11")), None);
    }

    #[test]
    fn a_bare_version_is_a_prefix_match() {
        let all = vec![interp("3.10.13"), interp("3.11.9")];
        let versions: Vec<&str> = satisfying(&all, Some("3.11"))
            .iter()
            .map(|i| i.version.as_str())
            .collect();
        assert_eq!(versions, vec!["3.11.9"]);
    }

    #[test]
    fn an_exact_operator_requires_the_release_segments_to_match() {
        let all = vec![interp("3.11.8"), interp("3.11.9")];
        let versions: Vec<&str> = satisfying(&all, Some("==3.11.9"))
            .iter()
            .map(|i| i.version.as_str())
            .collect();
        assert_eq!(versions, vec!["3.11.9"]);
    }

    #[test]
    fn an_equals_wildcard_behaves_like_a_prefix() {
        let all = vec![interp("3.10.13"), interp("3.11.9"), interp("3.11.2")];
        let versions: Vec<&str> = satisfying(&all, Some("==3.11.*"))
            .iter()
            .map(|i| i.version.as_str())
            .collect();
        assert_eq!(versions, vec!["3.11.9", "3.11.2"]);
    }

    #[test]
    fn compatible_release_allows_patch_drift_but_not_minor_drift() {
        // ~=3.11.2 means >=3.11.2, ==3.11.* -- 3.11.9 qualifies, 3.12.0 does
        // not even though it is newer, and 3.11.1 does not because it is
        // older than the floor.
        let all = vec![interp("3.11.1"), interp("3.11.9"), interp("3.12.0")];
        let versions: Vec<&str> = satisfying(&all, Some("~=3.11.2"))
            .iter()
            .map(|i| i.version.as_str())
            .collect();
        assert_eq!(versions, vec!["3.11.9"]);
    }

    #[test]
    fn comma_separated_clauses_are_all_required() {
        let all = vec![interp("3.10.13"), interp("3.11.9"), interp("3.13.0")];
        let versions: Vec<&str> = satisfying(&all, Some(">=3.11,<3.13"))
            .iter()
            .map(|i| i.version.as_str())
            .collect();
        // `<` is not in the handled subset, so the whole requirement is
        // unparsed and every interpreter is offered -- not just the ones
        // that happen to satisfy the clause this parser does understand.
        assert_eq!(versions, vec!["3.10.13", "3.11.9", "3.13.0"]);
    }

    #[test]
    fn an_unparseable_requirement_falls_back_to_offering_everything() {
        // Both candidates must *violate* the requirement they are offered
        // against, or the assertion holds whether the fallback fired or `<`
        // were implemented tomorrow -- the same non-discrimination
        // `sidebar` and `deps` carried with `<3.13`.
        let all = vec![interp("3.10.13"), interp("3.12.4")];
        assert_eq!(satisfying(&all, Some("<3.5")).len(), 2);
        assert_eq!(satisfying(&all, Some("not a specifier")).len(), 2);
        assert_eq!(satisfying(&all, Some("")).len(), 2);
    }

    #[test]
    fn a_preleased_suffixed_interpreter_still_compares_by_its_release_numbers() {
        let all = vec![interp("3.15.0b4")];
        let versions: Vec<&str> = satisfying(&all, Some(">=3.13"))
            .iter()
            .map(|i| i.version.as_str())
            .collect();
        assert_eq!(versions, vec!["3.15.0b4"]);
    }

    #[test]
    fn a_downloadable_interpreter_can_still_be_preselected() {
        // `path: None` means uv can install it but has not yet -- offering to
        // install *is* the point of this module, so a not-yet-installed
        // interpreter must not be filtered out here.
        let all = vec![downloadable("3.12.4")];
        assert_eq!(
            preselect(&all, Some(">=3.11")).map(|i| i.version.as_str()),
            Some("3.12.4")
        );
    }

    #[test]
    fn an_empty_interpreter_list_preselects_nothing() {
        assert_eq!(preselect(&[], Some(">=3.11")), None);
        assert_eq!(preselect(&[], None), None);
    }

    #[test]
    fn the_suggested_install_version_is_the_floor_of_the_first_clause() {
        assert_eq!(
            suggested_install_version(Some(">=3.11")),
            Some("3.11".to_string())
        );
        assert_eq!(
            suggested_install_version(Some(">=3.11,<4")),
            Some("3.11".to_string())
        );
        assert_eq!(
            suggested_install_version(Some("~=3.10.2")),
            Some("3.10.2".to_string())
        );
        assert_eq!(
            suggested_install_version(Some("==3.12.*")),
            Some("3.12".to_string())
        );
    }

    #[test]
    fn no_requirement_has_no_suggested_install_version() {
        assert_eq!(suggested_install_version(None), None);
    }

    #[test]
    fn an_unparseable_requirement_has_no_suggested_install_version() {
        // Nothing here confirms what "suitable" would mean, so guessing a
        // version to install would be worse than asking the user.
        assert_eq!(suggested_install_version(Some("<3.13")), None);
    }

    #[test]
    fn version_satisfies_agrees_with_satisfying_on_a_single_candidate() {
        // The whole point of extracting `version_satisfies` is that it is
        // the same rule `satisfying` applies per-candidate, not a second
        // one -- so it must agree with `satisfying` filtering a one-element
        // list, in both directions.
        assert!(version_satisfies("3.12.4", Some(">=3.11")));
        assert!(!version_satisfies("3.9.18", Some(">=3.11")));
    }

    #[test]
    fn version_satisfies_falls_back_to_true_when_the_requirement_is_unparseable() {
        // Same fallback direction as `satisfying`: a requirement this parser
        // cannot read must never read as "does not satisfy".
        // `<3.5`, not `<3.13`: 3.9.18 satisfies `<3.13`, so that pair would
        // hold even with `<` implemented and prove nothing about the
        // fallback.
        assert!(version_satisfies("3.9.18", Some("<3.5")));
        assert!(version_satisfies("3.9.18", Some("not a specifier")));
        assert!(version_satisfies("3.9.18", None));
    }
}
