use super::ResolvedUv;
use crate::process::CommandSpec;
use std::path::{Path, PathBuf};

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

/// Install every workspace member in editable mode.
///
/// `--all-packages` is required and not a nicety: plain `uv sync` in a
/// workspace root installs no members at all. Verified against uv 0.12.1.
pub fn sync_all_members(uv: &ResolvedUv, ctx: &ProjectContext) -> CommandSpec {
    project_spec(uv, ctx).args(["sync", "--all-packages"])
}

/// Install exactly the named members and remove the rest.
///
/// Re-syncing with a smaller set removes the members no longer named —
/// verified against uv 0.12.1 — which is what lets a user's selection live in
/// Bombadil's own config instead of being written into the shared
/// `[tool.uv.workspace] members` list.
pub fn sync_members(
    uv: &ResolvedUv,
    ctx: &ProjectContext,
    package_names: &[String],
) -> CommandSpec {
    if package_names.is_empty() {
        // Say what is meant rather than relying on plain `uv sync` happening
        // to install nothing.
        return project_spec(uv, ctx).args(["sync", "--no-install-workspace"]);
    }

    let mut spec = project_spec(uv, ctx).arg("sync");
    for name in package_names {
        // Two `.arg` calls rather than `.args(["--package", name])`: the array
        // form would mix `&str` and `&String` elements, which does not compile.
        spec = spec.arg("--package").arg(name.clone());
    }
    spec
}

/// Add a local package by its folder into `group`. `path` is written
/// verbatim into `[tool.uv.sources]`; pass it through
/// `members::path_for_manifest` so it stays relative.
pub fn add_path(
    uv: &ResolvedUv,
    ctx: &ProjectContext,
    path: &Path,
    editable: bool,
    group: DependencyGroup,
) -> CommandSpec {
    let spec = project_spec(uv, ctx).arg("add");
    let spec = if editable {
        spec.arg("--editable")
    } else {
        spec
    };
    spec.arg(path.to_string_lossy()).args(group.flags())
}

/// Add a project as an editable path dependency.
///
/// `path` is written verbatim by uv into `[tool.uv.sources]`, so callers
/// should pass the output of `members::path_for_manifest` rather than an
/// absolute path — `pyproject.toml` is shared with teammates.
pub fn add_editable(uv: &ResolvedUv, ctx: &ProjectContext, path: &Path) -> CommandSpec {
    project_spec(uv, ctx)
        .args(["add", "--editable"])
        .arg(path.to_string_lossy())
}

/// Drop a previously added editable dependency.
///
/// The package must be removed via uv *before* its `MemberRef` row is deleted
/// from the selection: the name needed to call this at all lives only on that
/// row for a project outside the registered tree (discovery never reports
/// it), so deleting the row first loses the handle needed to un-wire it.
pub fn remove_editable(uv: &ResolvedUv, ctx: &ProjectContext, package_name: &str) -> CommandSpec {
    project_spec(uv, ctx).args(["remove", package_name])
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

/// Lock a new version for one package, without touching the environment.
///
/// This is `uv lock -P <package>` only. `uv lock` never installs anything, so
/// an "Upgrade" action wired to this alone changes the lockfile but leaves
/// the venv running the old version; it **must** be followed by [`sync`] to
/// actually apply the upgrade.
pub fn lock_upgrade_package(uv: &ResolvedUv, ctx: &ProjectContext, package: &str) -> CommandSpec {
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

/// Lists what is actually installed in the project's virtual environment.
///
/// `--python` is required and not redundant: `uv pip` commands are a
/// pip-compatibility surface and do NOT honour `UV_PROJECT_ENVIRONMENT` the
/// way project commands do. Without it, uv resolves an environment the way
/// pip would and reports whatever it finds first — the user's system or
/// conda environment. Verified against uv 0.12.1. The directory form is used
/// so no platform-specific interpreter path has to be constructed.
pub fn pip_list(uv: &ResolvedUv, ctx: &ProjectContext) -> CommandSpec {
    project_spec(uv, ctx)
        .args(["pip", "list", "--python"])
        .arg(ctx.venv_path.to_string_lossy())
        .args(["--format", "json"])
}

/// What has a newer release on the index. Needs the network.
pub fn pip_list_outdated(uv: &ResolvedUv, ctx: &ProjectContext) -> CommandSpec {
    project_spec(uv, ctx)
        .args([
            "pip",
            "list",
            "--outdated",
            "--exclude-editable",
            "--python",
        ])
        .arg(ctx.venv_path.to_string_lossy())
        .args(["--format", "json"])
}

/// Interpreter discovery is machine-wide, not project-scoped, so it carries
/// neither a working directory nor UV_PROJECT_ENVIRONMENT.
pub fn python_list(uv: &ResolvedUv) -> CommandSpec {
    CommandSpec::new(&uv.path).args(["python", "list", "--output-format", "json"])
}

/// The interpreters already on this machine; nothing uv would download.
pub fn python_list_installed(uv: &ResolvedUv) -> CommandSpec {
    CommandSpec::new(&uv.path).args([
        "python",
        "list",
        "--only-installed",
        "--output-format",
        "json",
    ])
}

/// `sync` as a question: would it change anything? Takes the very sync spec
/// Bombadil would run (pin, members) so the answer is about that sync.
/// Verified against uv 0.12.19: read-only, offline; exit 0 in step, exit 1
/// with "The environment is outdated" when not.
pub fn as_check(sync: CommandSpec) -> CommandSpec {
    sync.args(["--check", "--frozen", "--offline"])
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
    fn a_local_package_is_added_by_path_into_its_group() {
        let path = Path::new("../mylib");
        assert_eq!(
            add_path(&uv(), &ctx(), path, true, DependencyGroup::Main).args,
            vec!["add", "--editable", "../mylib"]
        );
        assert_eq!(
            add_path(
                &uv(),
                &ctx(),
                path,
                false,
                DependencyGroup::Named("dev".into())
            )
            .args,
            vec!["add", "../mylib", "--group", "dev"]
        );
        assert_eq!(
            add_path(
                &uv(),
                &ctx(),
                path,
                true,
                DependencyGroup::Optional("cli".into())
            )
            .args,
            vec!["add", "--editable", "../mylib", "--optional", "cli"]
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
            lock_upgrade_package(&uv(), &ctx(), "httpx").args,
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
    fn pip_list_outdated_asks_the_index_about_the_venv() {
        assert_eq!(
            pip_list_outdated(&uv(), &ctx()).args,
            vec![
                "pip",
                "list",
                "--outdated",
                // A local package's name can belong to someone else on PyPI.
                "--exclude-editable",
                "--python",
                "/envs/my-api-3f9c1a",
                "--format",
                "json"
            ]
        );
    }

    #[test]
    fn pip_list_requests_json_and_pins_the_venv_explicitly() {
        // `uv pip` ignores UV_PROJECT_ENVIRONMENT, so --python must carry the
        // venv directory directly or this lists the wrong environment.
        assert_eq!(
            pip_list(&uv(), &ctx()).args,
            vec![
                "pip",
                "list",
                "--python",
                "/envs/my-api-3f9c1a",
                "--format",
                "json"
            ]
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
    fn a_check_is_the_same_sync_made_read_only_and_offline() {
        assert_eq!(
            as_check(sync_all_members(&uv(), &ctx())).args,
            vec!["sync", "--all-packages", "--check", "--frozen", "--offline"]
        );
    }

    #[test]
    fn installed_pythons_only() {
        assert_eq!(
            python_list_installed(&uv()).args,
            vec![
                "python",
                "list",
                "--only-installed",
                "--output-format",
                "json"
            ]
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
            (
                "lock_upgrade_package",
                lock_upgrade_package(&uv, &ctx, "httpx"),
            ),
            ("tree", tree(&uv, &ctx, 2)),
            ("pip_list", pip_list(&uv, &ctx)),
            ("pip_list_outdated", pip_list_outdated(&uv, &ctx)),
            ("as_check", as_check(sync(&uv, &ctx))),
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
            ("python_list_installed", python_list_installed(&uv())),
        ] {
            assert_eq!(spec.cwd, None, "{name} must not be project-scoped");
            assert!(
                !spec.env.contains_key("UV_PROJECT_ENVIRONMENT"),
                "{name} must not carry UV_PROJECT_ENVIRONMENT"
            );
        }
    }

    #[test]
    fn syncing_all_members_uses_all_packages() {
        // Plain `uv sync` in a workspace installs NO members -- verified
        // against uv 0.12.1. "Everything" is specifically --all-packages.
        assert_eq!(
            sync_all_members(&uv(), &ctx()).args,
            vec!["sync", "--all-packages"]
        );
    }

    #[test]
    fn syncing_a_subset_repeats_the_package_flag() {
        let names = vec!["alpha".to_string(), "beta".to_string()];
        assert_eq!(
            sync_members(&uv(), &ctx(), &names).args,
            vec!["sync", "--package", "alpha", "--package", "beta"]
        );
    }

    #[test]
    fn syncing_an_empty_selection_says_so_explicitly() {
        // Plain `uv sync` happens to install no members today, but relying on
        // that coincidence would break if uv changed its default.
        assert_eq!(
            sync_members(&uv(), &ctx(), &[]).args,
            vec!["sync", "--no-install-workspace"]
        );
    }

    #[test]
    fn adding_an_editable_project_passes_the_path_through() {
        let spec = add_editable(&uv(), &ctx(), std::path::Path::new("../shared-lib"));
        assert_eq!(spec.args, vec!["add", "--editable", "../shared-lib"]);
    }

    #[test]
    fn removing_an_editable_project_names_the_package() {
        assert_eq!(
            remove_editable(&uv(), &ctx(), "shared-lib").args,
            vec!["remove", "shared-lib"]
        );
    }

    #[test]
    fn the_new_builders_are_project_scoped_like_the_rest() {
        // Same invariant the parametrized test enforces for the others: a
        // builder that bypassed project_spec would put the venv elsewhere.
        for (name, spec) in [
            ("sync_all_members", sync_all_members(&uv(), &ctx())),
            (
                "sync_members",
                sync_members(&uv(), &ctx(), &["alpha".to_string()]),
            ),
            (
                "add_editable",
                add_editable(&uv(), &ctx(), std::path::Path::new("../x")),
            ),
            ("remove_editable", remove_editable(&uv(), &ctx(), "x")),
        ] {
            assert_eq!(
                spec.cwd.as_deref(),
                Some(std::path::Path::new("/proj/my-api")),
                "{name} must run in the project directory"
            );
            assert_eq!(
                spec.env.get("UV_PROJECT_ENVIRONMENT").map(String::as_str),
                Some("/envs/my-api-3f9c1a"),
                "{name} must point uv at the resolved venv"
            );
        }
    }
}
