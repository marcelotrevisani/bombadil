//! Proves the command builders and parsers agree with a real uv.
//!
//! Run with: cargo test -p bombadil-core --features uv-integration --test uv_integration
#![cfg(feature = "uv-integration")]

use bombadil_core::model::UvSource;
use bombadil_core::process::{CommandRunner, CommandSpec, SystemCommandRunner};
use bombadil_core::uv::command::{self, DependencyGroup, ProjectContext};
use bombadil_core::uv::results::{parse_pip_list, parse_tree};
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
