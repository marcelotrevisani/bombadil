use super::ResolvedUv;
use crate::process::CommandSpec;
use std::path::PathBuf;

/// Which part of `pyproject.toml` a dependency operation targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyGroup {
    Main,
    Dev,
    /// A PEP 735 dependency group.
    Named(String),
    /// An entry under `[project.optional-dependencies]`.
    Optional(String),
}

impl DependencyGroup {
    /// The flags uv needs to target this group. Main needs none.
    fn flags(&self) -> Vec<String> {
        match self {
            DependencyGroup::Main => Vec::new(),
            DependencyGroup::Dev => vec!["--dev".into()],
            DependencyGroup::Named(name) => vec!["--group".into(), name.clone()],
            DependencyGroup::Optional(extra) => vec!["--optional".into(), extra.clone()],
        }
    }
}

/// The project a uv command applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectContext {
    pub project_dir: PathBuf,
    pub venv_path: PathBuf,
}

/// Base spec for a project-scoped command: correct working directory, and uv
/// pointed at the venv this project actually uses.
fn project_spec(uv: &ResolvedUv, ctx: &ProjectContext) -> CommandSpec {
    CommandSpec::new(&uv.path)
        .cwd(&ctx.project_dir)
        .env("UV_PROJECT_ENVIRONMENT", ctx.venv_path.to_string_lossy())
}

pub fn sync(uv: &ResolvedUv, ctx: &ProjectContext) -> CommandSpec {
    project_spec(uv, ctx).arg("sync")
}

pub fn sync_with_python(uv: &ResolvedUv, ctx: &ProjectContext, python: &str) -> CommandSpec {
    project_spec(uv, ctx).args(["sync", "-p", python])
}

pub fn add(
    uv: &ResolvedUv,
    ctx: &ProjectContext,
    requirement: &str,
    group: DependencyGroup,
) -> CommandSpec {
    project_spec(uv, ctx)
        .args(["add", requirement])
        .args(group.flags())
}

pub fn remove(
    uv: &ResolvedUv,
    ctx: &ProjectContext,
    package: &str,
    group: DependencyGroup,
) -> CommandSpec {
    project_spec(uv, ctx)
        .args(["remove", package])
        .args(group.flags())
}

pub fn upgrade_package(uv: &ResolvedUv, ctx: &ProjectContext, package: &str) -> CommandSpec {
    project_spec(uv, ctx).args(["lock", "-P", package])
}

pub fn tree(uv: &ResolvedUv, ctx: &ProjectContext, depth: u32) -> CommandSpec {
    project_spec(uv, ctx).args([
        "tree".to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--depth".to_string(),
        depth.to_string(),
    ])
}

pub fn pip_list(uv: &ResolvedUv, ctx: &ProjectContext) -> CommandSpec {
    project_spec(uv, ctx).args(["pip", "list", "--format", "json"])
}

/// Interpreter discovery is machine-wide, not project-scoped, so it carries
/// neither a working directory nor UV_PROJECT_ENVIRONMENT.
pub fn python_list(uv: &ResolvedUv) -> CommandSpec {
    CommandSpec::new(&uv.path).args(["python", "list", "--output-format", "json"])
}

pub fn python_install(uv: &ResolvedUv, version: &str) -> CommandSpec {
    CommandSpec::new(&uv.path).args(["python", "install", version])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uv::{UvOrigin, UvVersion};
    use std::path::{Path, PathBuf};

    fn uv() -> ResolvedUv {
        ResolvedUv {
            path: PathBuf::from("/data/uv/uv"),
            version: UvVersion {
                major: 0,
                minor: 12,
                patch: 1,
            },
            origin: UvOrigin::Bundled,
            newer_than_bundled: false,
        }
    }

    fn ctx() -> ProjectContext {
        ProjectContext {
            project_dir: PathBuf::from("/proj/my-api"),
            venv_path: PathBuf::from("/envs/my-api-3f9c1a"),
        }
    }

    #[test]
    fn every_command_runs_in_the_project_directory() {
        let spec = sync(&uv(), &ctx());
        assert_eq!(spec.cwd, Some(PathBuf::from("/proj/my-api")));
    }

    #[test]
    fn every_command_points_uv_at_the_resolved_venv() {
        // This pairing is what lets a project's venv live anywhere.
        let spec = sync(&uv(), &ctx());
        assert_eq!(
            spec.env.get("UV_PROJECT_ENVIRONMENT").map(String::as_str),
            Some("/envs/my-api-3f9c1a")
        );
    }

    #[test]
    fn sync_is_a_bare_sync() {
        assert_eq!(sync(&uv(), &ctx()).args, vec!["sync"]);
    }

    #[test]
    fn creating_a_venv_pins_the_interpreter() {
        assert_eq!(
            sync_with_python(&uv(), &ctx(), "3.12").args,
            vec!["sync", "-p", "3.12"]
        );
    }

    #[test]
    fn add_targets_the_right_group() {
        let cases = [
            (DependencyGroup::Main, vec!["add", "httpx>=0.27"]),
            (DependencyGroup::Dev, vec!["add", "httpx>=0.27", "--dev"]),
            (
                DependencyGroup::Named("test".into()),
                vec!["add", "httpx>=0.27", "--group", "test"],
            ),
            (
                DependencyGroup::Optional("cli".into()),
                vec!["add", "httpx>=0.27", "--optional", "cli"],
            ),
        ];
        for (group, expected) in cases {
            assert_eq!(
                add(&uv(), &ctx(), "httpx>=0.27", group.clone()).args,
                expected,
                "group {group:?}"
            );
        }
    }

    #[test]
    fn remove_targets_the_right_group() {
        let cases = [
            (DependencyGroup::Main, vec!["remove", "httpx"]),
            (DependencyGroup::Dev, vec!["remove", "httpx", "--dev"]),
            (
                DependencyGroup::Named("test".into()),
                vec!["remove", "httpx", "--group", "test"],
            ),
            (
                DependencyGroup::Optional("cli".into()),
                vec!["remove", "httpx", "--optional", "cli"],
            ),
        ];
        for (group, expected) in cases {
            assert_eq!(
                remove(&uv(), &ctx(), "httpx", group.clone()).args,
                expected,
                "group {group:?}"
            );
        }
    }

    #[test]
    fn upgrading_one_package_locks_only_that_package() {
        assert_eq!(
            upgrade_package(&uv(), &ctx(), "httpx").args,
            vec!["lock", "-P", "httpx"]
        );
    }

    #[test]
    fn tree_requests_json_at_the_given_depth() {
        assert_eq!(
            tree(&uv(), &ctx(), 2).args,
            vec!["tree", "--format", "json", "--depth", "2"]
        );
    }

    #[test]
    fn pip_list_requests_json() {
        assert_eq!(
            pip_list(&uv(), &ctx()).args,
            vec!["pip", "list", "--format", "json"]
        );
    }

    #[test]
    fn python_list_requests_json_and_needs_no_project() {
        let spec = python_list(&uv());
        assert_eq!(spec.args, vec!["python", "list", "--output-format", "json"]);
        assert_eq!(
            spec.cwd, None,
            "interpreter discovery is not project-scoped"
        );
    }

    #[test]
    fn python_install_names_the_version() {
        assert_eq!(
            python_install(&uv(), "3.13").args,
            vec!["python", "install", "3.13"]
        );
    }

    #[test]
    fn the_program_is_always_the_resolved_binary() {
        assert_eq!(sync(&uv(), &ctx()).program, PathBuf::from("/data/uv/uv"));
        assert_eq!(python_list(&uv()).program, PathBuf::from("/data/uv/uv"));
    }

    #[test]
    fn every_project_scoped_builder_pairs_cwd_with_uv_project_environment() {
        // The pairing is the entire mechanism by which a venv can live outside
        // .venv. Asserting it on `sync` alone would let a new builder that
        // bypasses project_spec through unnoticed, and the failure is silent.
        let uv = uv();
        let ctx = ctx();
        let specs: Vec<(&str, CommandSpec)> = vec![
            ("sync", sync(&uv, &ctx)),
            ("sync_with_python", sync_with_python(&uv, &ctx, "3.12")),
            ("add", add(&uv, &ctx, "httpx", DependencyGroup::Main)),
            ("remove", remove(&uv, &ctx, "httpx", DependencyGroup::Main)),
            ("upgrade_package", upgrade_package(&uv, &ctx, "httpx")),
            ("tree", tree(&uv, &ctx, 2)),
            ("pip_list", pip_list(&uv, &ctx)),
        ];

        for (name, spec) in specs {
            assert_eq!(
                spec.cwd.as_deref(),
                Some(Path::new("/proj/my-api")),
                "{name} must run in the project directory"
            );
            assert_eq!(
                spec.env.get("UV_PROJECT_ENVIRONMENT").map(String::as_str),
                Some("/envs/my-api-3f9c1a"),
                "{name} must point uv at the resolved venv"
            );
            assert_eq!(spec.program, uv.path, "{name} must run the resolved uv");
        }
    }

    #[test]
    fn machine_wide_builders_carry_no_project_context() {
        // Interpreter discovery is not project-scoped. Routing these through
        // project_spec would silently scope them to one project's venv.
        for (name, spec) in [
            ("python_list", python_list(&uv())),
            ("python_install", python_install(&uv(), "3.13")),
        ] {
            assert_eq!(spec.cwd, None, "{name} must not be project-scoped");
            assert!(
                !spec.env.contains_key("UV_PROJECT_ENVIRONMENT"),
                "{name} must not carry UV_PROJECT_ENVIRONMENT"
            );
        }
    }
}
