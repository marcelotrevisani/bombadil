use crate::model::{Config, EnvVar, IndexKind};
use std::collections::HashMap;

/// One human-readable configuration problem. An empty result means valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub message: String,
}

impl Problem {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

fn is_valid_index_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn check_env(problems: &mut Vec<Problem>, scope: &str, vars: &[EnvVar]) {
    for var in vars {
        if var.key.is_empty() {
            problems.push(Problem::new(format!(
                "{scope}: an environment variable has an empty name"
            )));
        } else if var.key.contains('=') || var.key.contains('\0') {
            problems.push(Problem::new(format!(
                "{scope}: environment variable name {:?} may not contain '=' or a null byte",
                var.key
            )));
        }
        if var.secret && !var.value.is_empty() {
            problems.push(Problem::new(format!(
                "{scope}: {} is marked secret but carries a value; secret values belong in the keychain",
                var.key
            )));
        }
    }
}

/// Check invariants that span more than one value.
pub fn validate(config: &Config) -> Vec<Problem> {
    let mut problems = Vec::new();

    // Index names become part of UV_INDEX_<NAME>_USERNAME, so they must be
    // usable in an environment variable name and unique once uppercased.
    let mut seen: HashMap<String, &str> = HashMap::new();
    for index in &config.indexes {
        if !is_valid_index_name(&index.name) {
            problems.push(Problem::new(format!(
                "index name {:?} must contain only letters, digits, '_' or '-'",
                index.name
            )));
        }
        if let Some(first) = seen.insert(index.name.to_ascii_uppercase(), &index.name) {
            problems.push(Problem::new(format!(
                "index names {:?} and {:?} collide: both become UV_INDEX_{}_USERNAME",
                first,
                index.name,
                index.name.to_ascii_uppercase()
            )));
        }
        if !(index.url.starts_with("http://") || index.url.starts_with("https://")) {
            problems.push(Problem::new(format!(
                "index {:?} has URL {:?}; only http:// and https:// are supported",
                index.name, index.url
            )));
        }
    }

    check_env(&mut problems, "global", &config.global_env);

    let by_name: HashMap<&str, IndexKind> = config
        .indexes
        .iter()
        .map(|i| (i.name.as_str(), i.kind))
        .collect();

    for project in &config.projects {
        let scope = format!("project {:?}", project.label);
        check_env(&mut problems, &scope, &project.env);

        let mut defaults = 0;
        for name in &project.index_names {
            match by_name.get(name.as_str()) {
                None => problems.push(Problem::new(format!(
                    "{scope} references index {name:?}, which is not defined"
                ))),
                Some(IndexKind::Default) => defaults += 1,
                Some(IndexKind::Extra) => {}
            }
        }
        if defaults > 1 {
            problems.push(Problem::new(format!(
                "{scope} selects more than one default index; at most one is allowed"
            )));
        }
    }

    problems
}

#[cfg(test)]
mod tests {
    use super::validate;
    use crate::model::{Config, EnvVar, Index, IndexAuth, IndexKind, Project, VenvLocation};
    use std::path::PathBuf;
    use uuid::Uuid;

    fn index(name: &str, kind: IndexKind) -> Index {
        Index {
            name: name.into(),
            url: "https://example.com/simple".into(),
            kind,
            default_for_new_projects: false,
            auth: IndexAuth::None,
        }
    }

    fn project(index_names: Vec<String>) -> Project {
        Project {
            id: Uuid::nil(),
            label: "p".into(),
            pyproject_path: PathBuf::from("/p/pyproject.toml"),
            index_names,
            venv: VenvLocation::Alongside,
            env: vec![],
            scripts: vec![],
        }
    }

    /// Struct-update syntax throughout, not `let mut c = Config::default(); c.x = ..`:
    /// clippy's `field_reassign_with_default` rejects the latter and CI denies warnings.
    fn config_with(indexes: Vec<Index>, projects: Vec<Project>, global_env: Vec<EnvVar>) -> Config {
        Config {
            indexes,
            projects,
            global_env,
            ..Config::default()
        }
    }

    fn messages(config: &Config) -> Vec<String> {
        validate(config).into_iter().map(|p| p.message).collect()
    }

    #[test]
    fn a_default_config_is_valid() {
        assert!(validate(&Config::default()).is_empty());
    }

    #[test]
    fn index_names_must_be_usable_in_an_environment_variable() {
        let c = config_with(vec![index("has spaces", IndexKind::Extra)], vec![], vec![]);
        assert!(
            messages(&c).iter().any(|m| m.contains("has spaces")),
            "got {:?}",
            messages(&c)
        );
    }

    #[test]
    fn index_names_collide_case_insensitively() {
        // Both would become UV_INDEX_PRIV_USERNAME.
        let c = config_with(
            vec![
                index("priv", IndexKind::Extra),
                index("PRIV", IndexKind::Extra),
            ],
            vec![],
            vec![],
        );
        assert!(
            messages(&c)
                .iter()
                .any(|m| m.contains("UV_INDEX_PRIV_USERNAME")),
            "got {:?}",
            messages(&c)
        );
    }

    #[test]
    fn an_index_url_must_be_http_or_https() {
        let bad = Index {
            url: "ftp://example.com/simple".into(),
            ..index("priv", IndexKind::Extra)
        };
        let c = config_with(vec![bad], vec![], vec![]);
        assert!(messages(&c).iter().any(|m| m.contains("ftp://")));
    }

    #[test]
    fn a_project_cannot_reference_an_unknown_index() {
        let c = config_with(vec![], vec![project(vec!["nope".into()])], vec![]);
        assert!(messages(&c).iter().any(|m| m.contains("nope")));
    }

    #[test]
    fn a_project_may_select_at_most_one_default_index() {
        let c = config_with(
            vec![
                index("a", IndexKind::Default),
                index("b", IndexKind::Default),
            ],
            vec![project(vec!["a".into(), "b".into()])],
            vec![],
        );
        assert!(
            messages(&c)
                .iter()
                .any(|m| m.contains("more than one default")),
            "got {:?}",
            messages(&c)
        );
    }

    #[test]
    fn one_default_and_several_extras_is_fine() {
        let c = config_with(
            vec![
                index("a", IndexKind::Default),
                index("b", IndexKind::Extra),
                index("c", IndexKind::Extra),
            ],
            vec![project(vec!["a".into(), "b".into(), "c".into()])],
            vec![],
        );
        assert!(validate(&c).is_empty(), "got {:?}", messages(&c));
    }

    #[test]
    fn an_environment_variable_key_cannot_be_empty_or_contain_equals() {
        let c = config_with(
            vec![],
            vec![],
            vec![
                EnvVar {
                    key: String::new(),
                    value: "x".into(),
                    secret: false,
                },
                EnvVar {
                    key: "A=B".into(),
                    value: "x".into(),
                    secret: false,
                },
            ],
        );
        assert_eq!(messages(&c).len(), 2, "got {:?}", messages(&c));
    }

    #[test]
    fn a_secret_variable_must_not_carry_a_value() {
        let c = config_with(
            vec![],
            vec![],
            vec![EnvVar {
                key: "TOKEN".into(),
                value: "leaked".into(),
                secret: true,
            }],
        );
        assert!(messages(&c).iter().any(|m| m.contains("TOKEN")));
    }
}
