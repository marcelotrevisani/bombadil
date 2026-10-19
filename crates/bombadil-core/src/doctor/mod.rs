//! uv's failures in plain words, each with the buttons that fix it. Matched
//! on uv 0.12.19's own wording; the fixtures are real captures.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoctorFix {
    /// `uv add <package>`: let uv choose versions that fit.
    LetUvPick(String),
    /// Raise `requires-python` to this specifier, then sync.
    RequirePython(String),
    Retry,
    OpenIndexes,
    ShowOutput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Auth {
        host: String,
    },
    NeedsNewerPython {
        package: String,
        allowed: String,
        needed: String,
    },
    NotFound {
        package: String,
    },
    NoSuchVersion {
        package: String,
        spec: String,
    },
    Network {
        host: String,
    },
    Build,
    Other {
        line: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    pub kind: Kind,
    pub sentence: String,
    pub fixes: Vec<DoctorFix>,
}

/// The text between `start` and the next `end` after it.
fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = text.find(start)? + start.len();
    let len = text[from..].find(end)?;
    Some(&text[from..from + len])
}

/// `host[:port]` of a URL.
fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest).to_string()
}

fn package_name(requirement: &str) -> String {
    requirement
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect()
}

pub fn diagnose(transcript: &str) -> Diagnosis {
    let d = |kind, sentence: String, fixes| Diagnosis {
        kind,
        sentence,
        fixes,
    };
    if transcript.contains("could not be queried due to a lack of valid authentication credentials")
    {
        let host = between(transcript, "An index URL (", ")")
            .map(host)
            .unwrap_or_default();
        return d(
            Kind::Auth { host: host.clone() },
            format!("{host} refused the saved credentials"),
            vec![DoctorFix::OpenIndexes],
        );
    }
    if let (Some(allowed), Some(needed)) = (
        between(transcript, "The `requires-python` value (", ")"),
        between(transcript, "(like ", ")"),
    ) {
        // Several ranges read "(e.g., all of:\n    numpy>=…"; one reads
        // "(e.g., numpy==2.3.0 only supports >=3.11)".
        let package = transcript
            .split_once("The `requires-python` value (")
            .and_then(|(_, hint)| hint.split_once("(e.g., "))
            .map(|(_, rest)| package_name(rest.strip_prefix("all of:\n").unwrap_or(rest)))
            .filter(|p| !p.is_empty());
        if let Some(package) = package {
            return d(
                Kind::NeedsNewerPython {
                    package: package.clone(),
                    allowed: allowed.to_string(),
                    needed: needed.to_string(),
                },
                format!("{package} needs Python {needed}; this project allows {allowed}"),
                vec![
                    DoctorFix::LetUvPick(package),
                    DoctorFix::RequirePython(needed.to_string()),
                ],
            );
        }
    }
    // The word just before the marker: "Because foo depends on bar and bar
    // was not found" is about bar.
    if let Some(package) = transcript
        .split_once(" was not found in the package registry")
        .and_then(|(before, _)| before.split_whitespace().last())
    {
        let package = package_name(package);
        return d(
            Kind::NotFound {
                package: package.clone(),
            },
            format!("There's no package called {package} on the index. Check the spelling."),
            vec![],
        );
    }
    if let Some(requirement) = between(transcript, "Because there is no version of ", " and ") {
        let package = package_name(requirement);
        let spec = requirement.trim()[package.len()..].to_string();
        return d(
            Kind::NoSuchVersion {
                package: package.clone(),
                spec: spec.clone(),
            },
            format!("{package} has no version matching {spec}"),
            vec![DoctorFix::LetUvPick(package)],
        );
    }
    if let Some(url) = between(transcript, "Failed to fetch: `", "`")
        .or_else(|| between(transcript, "error sending request for url (", ")"))
    {
        let host = host(url);
        return d(
            Kind::Network { host: host.clone() },
            format!("Couldn't reach {host}. Check your connection or the index settings."),
            vec![DoctorFix::Retry],
        );
    }
    if transcript.contains("The build backend returned an error") {
        return d(
            Kind::Build,
            "A package had to be built from source and failed. A newer version may have \
             ready-made files for your Python."
                .into(),
            vec![DoctorFix::ShowOutput],
        );
    }
    let line = transcript
        .lines()
        .find_map(|l| l.trim().strip_prefix("error: "))
        .unwrap_or_default()
        .to_string();
    let sentence = match line.is_empty() {
        true => "uv couldn't finish".to_string(),
        false => format!("uv couldn't finish: {line}"),
    };
    d(Kind::Other { line }, sentence, vec![DoctorFix::ShowOutput])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> &'static str {
        match name {
            "python" => include_str!("fixtures/python.txt"),
            "not_found" => include_str!("fixtures/not_found.txt"),
            "no_version" => include_str!("fixtures/no_version.txt"),
            "network" => include_str!("fixtures/network.txt"),
            "auth" => include_str!("fixtures/auth.txt"),
            "build" => include_str!("fixtures/build.txt"),
            "python_single" => include_str!("fixtures/python_single.txt"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn a_requirement_needing_a_newer_python_offers_both_fixes() {
        let d = diagnose(fixture("python"));
        assert_eq!(
            d.kind,
            Kind::NeedsNewerPython {
                package: "numpy".into(),
                allowed: ">=3.8".into(),
                needed: ">=3.11".into()
            }
        );
        assert_eq!(
            d.sentence,
            "numpy needs Python >=3.11; this project allows >=3.8"
        );
        assert_eq!(
            d.fixes,
            [
                DoctorFix::LetUvPick("numpy".into()),
                DoctorFix::RequirePython(">=3.11".into())
            ]
        );
    }

    #[test]
    fn a_single_requirement_needing_a_newer_python_is_recognised() {
        let d = diagnose(fixture("python_single"));
        assert_eq!(
            d.kind,
            Kind::NeedsNewerPython {
                package: "numpy".into(),
                allowed: ">=3.10".into(),
                needed: ">=3.11".into()
            }
        );
    }

    #[test]
    fn a_missing_package_is_named_not_the_one_needing_it() {
        let d = diagnose(
            "  × No solution found when resolving dependencies:\n  ╰─▶ Because foo depends on bar and bar was not found in the package registry, we can conclude that foo cannot be used.\n",
        );
        assert_eq!(
            d.kind,
            Kind::NotFound {
                package: "bar".into()
            }
        );
    }

    #[test]
    fn auth_is_not_mistaken_for_a_missing_package() {
        let d = diagnose(fixture("auth"));
        assert_eq!(
            d.kind,
            Kind::Auth {
                host: "127.0.0.1:8765".into()
            }
        );
        assert_eq!(d.sentence, "127.0.0.1:8765 refused the saved credentials");
        assert_eq!(d.fixes, [DoctorFix::OpenIndexes]);
    }

    #[test]
    fn a_missing_package_asks_to_check_the_spelling() {
        let d = diagnose(fixture("not_found"));
        assert_eq!(
            d.kind,
            Kind::NotFound {
                package: "nmupyxxq".into()
            }
        );
        assert_eq!(
            d.sentence,
            "There's no package called nmupyxxq on the index. Check the spelling."
        );
        assert!(d.fixes.is_empty());
    }

    #[test]
    fn a_missing_version_lets_uv_pick() {
        let d = diagnose(fixture("no_version"));
        assert_eq!(
            d.kind,
            Kind::NoSuchVersion {
                package: "six".into(),
                spec: "==9.9".into()
            }
        );
        assert_eq!(d.sentence, "six has no version matching ==9.9");
        assert_eq!(d.fixes, [DoctorFix::LetUvPick("six".into())]);
    }

    #[test]
    fn an_unreachable_index_offers_retry() {
        let d = diagnose(fixture("network"));
        assert_eq!(
            d.kind,
            Kind::Network {
                host: "127.0.0.1:9".into()
            }
        );
        assert_eq!(
            d.sentence,
            "Couldn't reach 127.0.0.1:9. Check your connection or the index settings."
        );
        assert_eq!(d.fixes, [DoctorFix::Retry]);
    }

    #[test]
    fn a_failed_build_points_at_the_output() {
        let d = diagnose(fixture("build"));
        assert_eq!(d.kind, Kind::Build);
        assert!(
            d.sentence
                .starts_with("A package had to be built from source and failed.")
        );
        assert_eq!(d.fixes, [DoctorFix::ShowOutput]);
    }

    #[test]
    fn anything_else_quotes_uvs_first_error() {
        let d = diagnose("Resolved 3 packages\nerror: something new went wrong\n  cause: x\n");
        assert_eq!(
            d.kind,
            Kind::Other {
                line: "something new went wrong".into()
            }
        );
        assert_eq!(d.sentence, "uv couldn't finish: something new went wrong");
        assert_eq!(d.fixes, [DoctorFix::ShowOutput]);
        let d = diagnose("");
        assert_eq!(d.sentence, "uv couldn't finish");
    }

    #[test]
    fn truncated_output_never_panics() {
        for name in [
            "python",
            "not_found",
            "no_version",
            "network",
            "auth",
            "build",
            "python_single",
        ] {
            let text = fixture(name);
            for cut in (0..text.len())
                .step_by(7)
                .filter(|c| text.is_char_boundary(*c))
            {
                let _ = diagnose(&text[..cut]);
            }
        }
    }
}
