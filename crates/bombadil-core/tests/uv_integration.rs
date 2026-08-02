//! Proves the command builders and parsers agree with a real uv.
//!
//! Run with: cargo test -p bombadil-core --features uv-integration --test uv_integration
#![cfg(feature = "uv-integration")]

use bombadil_core::model::UvSource;
use bombadil_core::process::{CommandRunner, CommandSpec, SystemCommandRunner};
use bombadil_core::uv::command::{self, DependencyGroup, ProjectContext};
use bombadil_core::uv::results::{InstalledPackage, parse_pip_list, parse_tree};
use bombadil_core::uv::{ResolvedUv, UvVersion, locate_on_path, resolve};

fn write_project(dir: &std::path::Path) {
    std::fs::write(
        dir.join("pyproject.toml"),
        "[project]\nname = \"itest\"\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\ndependencies = []\n",
    )
    .unwrap();
}

fn resolve_uv(runner: &SystemCommandRunner) -> ResolvedUv {
    resolve(&UvSource::FromPath, runner, &locate_on_path, &|| {
        Err("bundled uv is not used by this test".to_string())
    })
    .expect("a uv on PATH at or above MIN_UV_VERSION is required for this test")
}

#[test]
fn sync_add_and_list_agree_with_a_real_uv() {
    let runner = SystemCommandRunner;
    let uv = resolve_uv(&runner);

    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();
    write_project(&project_dir);

    let ctx = ProjectContext {
        project_dir: project_dir.clone(),
        venv_path: tmp.path().join("env"),
    };

    // Create the environment.
    let outcome = runner.run(&command::sync(&uv, &ctx)).unwrap();
    assert!(outcome.success(), "sync failed: {}", outcome.stderr);
    assert!(
        ctx.venv_path.join("pyvenv.cfg").exists(),
        "UV_PROJECT_ENVIRONMENT must place the venv where we asked"
    );
    assert!(
        !project_dir.join(".venv").exists(),
        "uv must not also create a venv inside the project directory; \
         if it did, UV_PROJECT_ENVIRONMENT is being ignored"
    );

    // Add a tiny, dependency-free package.
    let outcome = runner
        .run(&command::add(&uv, &ctx, "iniconfig", DependencyGroup::Main))
        .unwrap();
    assert!(outcome.success(), "add failed: {}", outcome.stderr);

    // uv, not Bombadil, wrote the manifest.
    let manifest = std::fs::read_to_string(project_dir.join("pyproject.toml")).unwrap();
    assert!(manifest.contains("iniconfig"), "got:\n{manifest}");

    // The installed list parses and contains what we added.
    let outcome = runner.run(&command::pip_list(&uv, &ctx)).unwrap();
    assert!(outcome.success(), "pip list failed: {}", outcome.stderr);
    let packages = parse_pip_list(&outcome.stdout).expect("pip list JSON parses");
    assert!(
        packages.iter().any(|p| p.name == "iniconfig"),
        "got {packages:?}"
    );

    // The tree parses, or degrades in the way we designed for.
    let outcome = runner.run(&command::tree(&uv, &ctx, 2)).unwrap();
    assert!(outcome.success(), "tree failed: {}", outcome.stderr);
    parse_tree(&outcome.stdout).expect("tree JSON parses or degrades, never errors on valid JSON");
}

#[test]
fn clear_env_still_runs_uv_and_reports_the_same_version() {
    // Plan 2b's whole design rests on `clear_env: true` actually clearing the
    // child's environment (so an ambient UV_INDEX_* etc. cannot leak into a
    // command). This is the only place that is exercised, because
    // `SystemCommandRunner` cannot be unit-tested without spawning.
    let runner = SystemCommandRunner;
    let uv = resolve_uv(&runner);

    // Only reassigned under cfg(windows); allowed rather than cfg-gating the
    // `mut` itself, which would need duplicating the binding.
    #[allow(unused_mut)]
    let mut spec = CommandSpec::new(&uv.path).arg("--version").clear_env();

    // A genuinely empty environment can break process creation on Windows
    // (CreateProcess needs at least SYSTEMROOT to find system DLLs, and a
    // relative or missing PATH can stop it finding uv.exe at all), so carry
    // those two over there. On unix this is unneeded but harmless: uv only
    // needs argv[0] to be runnable, which cwd/exec path already guarantee.
    #[cfg(windows)]
    {
        if let Ok(systemroot) = std::env::var("SYSTEMROOT") {
            spec = spec.env("SYSTEMROOT", systemroot);
        }
        if let Ok(path) = std::env::var("PATH") {
            spec = spec.env("PATH", path);
        }
    }

    let outcome = runner.run(&spec).unwrap();
    assert!(
        outcome.success(),
        "uv --version failed under a cleared environment: {}",
        outcome.stderr
    );
    assert_eq!(
        UvVersion::parse(&outcome.stdout),
        Some(uv.version),
        "a cleared environment must not change which uv answers or what version it reports"
    );
}

#[test]
fn an_unknown_subcommand_is_reported_as_a_failed_outcome() {
    // `success()` is exercised as true throughout this file and the unit
    // tests only ever hand-construct a failing `CommandOutcome`; this proves
    // a real non-zero exit from uv is actually observed as failure.
    let runner = SystemCommandRunner;
    let uv = resolve_uv(&runner);

    let spec = CommandSpec::new(&uv.path).arg("definitely-not-a-subcommand");
    let outcome = runner.run(&spec).unwrap();

    assert!(
        !outcome.success(),
        "an unknown subcommand must not report success"
    );
    assert!(
        !outcome.stderr.is_empty(),
        "uv must explain the failure on stderr"
    );
}

/// Build a three-member uv workspace under `root`.
fn write_workspace(root: &std::path::Path) {
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"wsroot\"\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\n\
         dependencies = []\n\n[tool.uv.workspace]\nmembers = [\"packages/*\"]\n",
    )
    .unwrap();

    for member in ["alpha", "beta", "gamma"] {
        let dir = root.join("packages").join(member);
        std::fs::create_dir_all(dir.join("src").join(member)).unwrap();
        std::fs::write(
            dir.join("pyproject.toml"),
            format!(
                "[project]\nname = \"{member}\"\nversion = \"0.1.0\"\n\
                 requires-python = \">=3.10\"\ndependencies = []\n\n\
                 [build-system]\nrequires = [\"hatchling\"]\n\
                 build-backend = \"hatchling.build\"\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("src").join(member).join("__init__.py"), "").unwrap();
    }
}

#[test]
fn workspace_member_selection_agrees_with_a_real_uv() {
    let runner = SystemCommandRunner;
    let uv = resolve_uv(&runner);

    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().join("repo");
    std::fs::create_dir_all(&project_dir).unwrap();
    write_workspace(&project_dir);

    let ctx = ProjectContext {
        project_dir: project_dir.clone(),
        venv_path: tmp.path().join("env"),
    };

    // The one place that runs `pip list` and checks it succeeded, so every
    // caller below gets uv's stderr on failure instead of a parse panic.
    let list_packages = |ctx: &ProjectContext| -> Vec<InstalledPackage> {
        let outcome = runner.run(&command::pip_list(&uv, ctx)).unwrap();
        assert!(outcome.success(), "pip list failed: {}", outcome.stderr);
        parse_pip_list(&outcome.stdout).expect("pip list JSON parses")
    };

    let installed = |ctx: &ProjectContext| -> Vec<String> {
        let mut names: Vec<String> = list_packages(ctx).into_iter().map(|p| p.name).collect();
        names.sort();
        names
    };

    // Everything.
    let outcome = runner.run(&command::sync_all_members(&uv, &ctx)).unwrap();
    assert!(
        outcome.success(),
        "sync --all-packages failed: {}",
        outcome.stderr
    );
    assert_eq!(
        installed(&ctx),
        vec!["alpha", "beta", "gamma"],
        "initial sync must install every member"
    );

    // A selection of two proves the repeated `--package` flag actually works
    // end to end, not just a single one.
    let outcome = runner
        .run(&command::sync_members(
            &uv,
            &ctx,
            &["alpha".to_string(), "beta".to_string()],
        ))
        .unwrap();
    assert!(
        outcome.success(),
        "sync --package alpha --package beta failed: {}",
        outcome.stderr
    );
    assert_eq!(
        installed(&ctx),
        vec!["alpha", "beta"],
        "re-syncing with a two-member selection must remove the third"
    );

    // The behaviour this whole plan rests on: narrowing to one further
    // REMOVES the other, so deselection needs no manifest edit.
    let outcome = runner
        .run(&command::sync_members(&uv, &ctx, &["alpha".to_string()]))
        .unwrap();
    assert!(
        outcome.success(),
        "sync --package alpha failed: {}",
        outcome.stderr
    );
    assert_eq!(
        installed(&ctx),
        vec!["alpha"],
        "re-syncing with a smaller selection must remove the deselected members"
    );

    // And the survivor is genuinely editable, which is what "dev mode" means.
    let packages = list_packages(&ctx);
    let alpha = packages.iter().find(|p| p.name == "alpha").unwrap();
    assert!(
        alpha.is_editable(),
        "workspace members must be installed editable; got {alpha:?}"
    );
}

/// Build a loose (non-workspace) project directory named `itest-app`.
fn write_loose_project(dir: &std::path::Path) {
    std::fs::write(
        dir.join("pyproject.toml"),
        "[project]\nname = \"itest-app\"\nversion = \"0.1.0\"\n\
         requires-python = \">=3.10\"\ndependencies = []\n",
    )
    .unwrap();
}

/// Build a sibling library with its own `pyproject.toml`, installable by uv.
fn write_sibling_library(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("src").join("itest_lib")).unwrap();
    std::fs::write(
        dir.join("pyproject.toml"),
        "[project]\nname = \"itest-lib\"\nversion = \"0.1.0\"\n\
         requires-python = \">=3.10\"\ndependencies = []\n\n\
         [build-system]\nrequires = [\"hatchling\"]\n\
         build-backend = \"hatchling.build\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src").join("itest_lib").join("__init__.py"), "").unwrap();
}

#[test]
fn loose_package_editable_add_and_remove_agrees_with_a_real_uv() {
    // Proves the other half of spec 5a: `uv add --editable <path>` /
    // `uv remove` for a loose (non-workspace) project. Critically, this is
    // also the only place `InstalledPackage::is_editable()` is proven for a
    // package that entered the environment via `uv add --editable` rather
    // than as a workspace member -- the two code paths that produce an
    // editable install are not guaranteed to report themselves identically.
    let runner = SystemCommandRunner;
    let uv = resolve_uv(&runner);

    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().join("app");
    let lib_dir = tmp.path().join("itest-lib");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(&lib_dir).unwrap();
    write_loose_project(&project_dir);
    write_sibling_library(&lib_dir);

    let ctx = ProjectContext {
        project_dir: project_dir.clone(),
        venv_path: tmp.path().join("env"),
    };

    let list_packages = || -> Vec<InstalledPackage> {
        let outcome = runner.run(&command::pip_list(&uv, &ctx)).unwrap();
        assert!(outcome.success(), "pip list failed: {}", outcome.stderr);
        parse_pip_list(&outcome.stdout).expect("pip list JSON parses")
    };

    // Add the sibling as an editable dependency, using the same relative-path
    // computation the real caller would use.
    let relative = bombadil_core::members::path_for_manifest(&project_dir, &lib_dir);
    let outcome = runner
        .run(&command::add_editable(&uv, &ctx, &relative))
        .unwrap();
    assert!(
        outcome.success(),
        "add --editable failed: {}",
        outcome.stderr
    );

    let outcome = runner.run(&command::sync(&uv, &ctx)).unwrap();
    assert!(outcome.success(), "sync failed: {}", outcome.stderr);

    let packages = list_packages();
    let lib = packages
        .iter()
        .find(|p| p.name == "itest-lib")
        .unwrap_or_else(|| panic!("itest-lib not installed; got {packages:?}"));
    assert!(
        lib.is_editable(),
        "uv add --editable must produce an editable install; got {lib:?}"
    );
    assert_eq!(
        lib.editable_project_location
            .as_deref()
            .and_then(|p| p.file_name()),
        Some(std::ffi::OsStr::new("itest-lib")),
        "editable_project_location must point at the sibling library; got {lib:?}"
    );

    // Now remove it and confirm it is gone -- the other half of the mapping.
    let outcome = runner
        .run(&command::remove_editable(&uv, &ctx, "itest-lib"))
        .unwrap();
    assert!(outcome.success(), "remove failed: {}", outcome.stderr);

    let outcome = runner.run(&command::sync(&uv, &ctx)).unwrap();
    assert!(outcome.success(), "sync failed: {}", outcome.stderr);

    let packages = list_packages();
    assert!(
        !packages.iter().any(|p| p.name == "itest-lib"),
        "itest-lib must be gone after remove_editable; got {packages:?}"
    );
}
