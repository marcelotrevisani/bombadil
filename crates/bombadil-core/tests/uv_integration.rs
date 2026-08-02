//! Proves the command builders and parsers agree with a real uv.
//!
//! Run with: cargo test -p bombadil-core --features uv-integration --test uv_integration
#![cfg(feature = "uv-integration")]

use bombadil_core::model::{Project, Script, UvSource, VenvLocation};
use bombadil_core::process::{CommandRunner, CommandSpec, SystemCommandRunner};
use bombadil_core::secrets::{InMemorySecretStore, KeyringSecretStore, SecretKey, SecretStore};
use bombadil_core::terminal::activation::{InitFile, ShellFamily, init_file};
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

#[test]
fn a_composed_environment_activates_a_real_venv() {
    let runner = SystemCommandRunner;
    let uv = resolve_uv(&runner);

    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("pyproject.toml"),
        "[project]\nname = \"envtest\"\nversion = \"0.1.0\"\n\
         requires-python = \">=3.10\"\ndependencies = [\"iniconfig\"]\n",
    )
    .unwrap();

    let venv_path = tmp.path().join("env");
    let ctx = ProjectContext {
        project_dir: project_dir.clone(),
        venv_path: venv_path.clone(),
    };

    let outcome = runner.run(&command::sync(&uv, &ctx)).unwrap();
    assert!(outcome.success(), "sync failed: {}", outcome.stderr);

    let project = Project {
        id: uuid::Uuid::from_u128(1),
        label: "envtest".into(),
        pyproject_path: project_dir.join("pyproject.toml"),
        index_names: vec![],
        venv: VenvLocation::Custom {
            path: venv_path.clone(),
        },
        env: vec![],
        scripts: vec![],
        members: Default::default(),
    };

    let inputs = bombadil_core::envspec::EnvInputs {
        parent: std::env::vars().collect(),
        config: bombadil_core::model::Config::default(),
        project,
        venv_path: venv_path.clone(),
    };

    let store = InMemorySecretStore::new();
    let env = bombadil_core::envspec::compose(&inputs, &store, &runner)
        .expect("composing an environment must succeed");

    // The proof: run python through the composed environment and confirm it is
    // the venv's interpreter, importing from the venv's site-packages -- not
    // whatever Python happens to be ambient on this machine.
    let mut spec = CommandSpec::new("python");
    spec.args = vec![
        "-c".into(),
        "import sys, iniconfig; print(sys.executable); print(iniconfig.__file__)".into(),
    ];
    spec.env = env.clone();
    spec.clear_env = true;

    let outcome = runner.run(&spec).unwrap();
    assert!(outcome.success(), "python failed: {}", outcome.stderr);

    // A vacuous pass lurks here: if python printed nothing, iterating stdout
    // lines directly would run zero times and still pass. Pin the shape
    // first.
    let lines: Vec<&str> = outcome.stdout.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "expected sys.executable and iniconfig.__file__; got {:?}",
        outcome.stdout
    );

    // On macOS, tempfile::tempdir() returns an unresolved /var/folders/...
    // path while Python reports the /private/var/folders/... it resolves to,
    // so both the raw and canonicalized venv path are accepted.
    let venv_str = venv_path.to_string_lossy().to_string();
    let venv_canonical = std::fs::canonicalize(&venv_path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| venv_str.clone());

    for line in lines {
        assert!(
            line.starts_with(&venv_str) || line.starts_with(&venv_canonical),
            "the composed environment must activate the venv; got {line}"
        );
    }
}

#[test]
fn a_pre_activate_script_can_export_into_the_composed_environment() {
    // The only proof the PowerShell dump form works on Windows. A red Windows
    // job here means that command is wrong -- which is exactly what this test
    // is for.
    let runner = SystemCommandRunner;
    let tmp = tempfile::tempdir().unwrap();

    let (script_path, body) = if cfg!(windows) {
        (
            tmp.path().join("pre.ps1"),
            "$env:FROM_SCRIPT = 'exported'\n",
        )
    } else {
        (tmp.path().join("pre.sh"), "export FROM_SCRIPT=exported\n")
    };
    std::fs::write(&script_path, body).unwrap();

    let script = Script {
        path: script_path,
        args: vec![],
        enabled: true,
    };

    let base: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let got = bombadil_core::envspec::apply(&script, &runner, &base)
        .expect("a pre-activate script must run and its exports be captured");

    assert_eq!(
        got.get("FROM_SCRIPT").map(String::as_str),
        Some("exported"),
        "the script's export was not captured"
    );
}

#[test]
fn the_real_keychain_round_trips_where_one_exists() {
    let store = KeyringSecretStore::new();
    if !store.is_available() {
        eprintln!("no keychain on this machine; skipping");
        return;
    }

    // Namespaced so it cannot collide with a real secret. Cleaned up before
    // any assertion runs, so a failed assertion never leaves a live entry
    // behind in a real OS keychain.
    let key = SecretKey::Index {
        name: "bombadil-integration-throwaway".into(),
    };
    store.set(&key, "hunter2").expect("set");
    let round_tripped = store.get(&key);
    store.delete(&key).expect("delete");
    let after_delete = store.get(&key);

    assert_eq!(round_tripped.unwrap().as_deref(), Some("hunter2"));
    assert_eq!(after_delete.unwrap(), None, "the secret must be gone");
}

#[test]
#[cfg(unix)]
fn spawn_returns_before_the_child_exits() {
    // `open` must return as soon as the terminal is launched: `run` uses
    // Command::output(), which waits for the child, and a terminal emulator
    // lives until the user closes its window. gnome-terminal and konsole
    // self-daemonize, which is why nothing caught this; kitty, alacritty,
    // foot, xterm and every Windows console kind do not.
    //
    // Unix-only: `sleep` is the portable long-running child here, and this
    // is a property of SystemCommandRunner rather than of any OS.
    let Some(sleep) = locate_on_path("sleep") else {
        panic!("sleep is required for this test on unix");
    };

    let spec = CommandSpec::new(&sleep).arg("30");

    let started = std::time::Instant::now();
    SystemCommandRunner
        .spawn(&spec)
        .expect("spawn must succeed");
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "spawn waited for the child ({elapsed:?}); under `run` this would take 30s"
    );
}

fn which_bash() -> Option<std::path::PathBuf> {
    locate_on_path("bash")
}

fn which_zsh() -> Option<std::path::PathBuf> {
    locate_on_path("zsh")
}

/// Write `file` under `tmp`, launch `shell` with the `InitFile`'s
/// `shell_args`/`env` (substituting `{file}`/`{dir}` for the file actually
/// written, exactly as a real caller must), and return the first entry of
/// the resulting `PATH`.
///
/// The child inherits the ambient environment rather than a cleared one, and
/// `HOME` is left untouched, so the shell really does read this machine's own
/// rc files -- that is the whole point of this test file over the hermetic
/// unit tests in `activation.rs`.
fn first_path_entry_via_init_file(
    shell: &std::path::Path,
    tmp: &std::path::Path,
    file: &InitFile,
) -> String {
    let path = tmp.join(&file.relative_path);
    std::fs::write(&path, &file.contents).unwrap();
    let dir = path.parent().unwrap().to_string_lossy().into_owned();
    let file_str = path.to_string_lossy().into_owned();

    let args: Vec<String> = file
        .shell_args
        .iter()
        .map(|a| a.replace("{file}", &file_str))
        .collect();

    let mut spec = CommandSpec::new(shell);
    spec.args = args;
    spec.args.push("-c".into());
    spec.args.push("printf '%s' \"$PATH\"".into());
    for (key, value) in &file.env {
        spec.env.insert(key.clone(), value.replace("{dir}", &dir));
    }

    let outcome = SystemCommandRunner.run(&spec).unwrap();
    assert!(outcome.success(), "shell failed: {}", outcome.stderr);
    outcome.stdout.split(':').next().unwrap_or_default().into()
}

#[test]
fn the_generated_init_file_puts_the_venv_first_in_a_real_interactive_bash_shell() {
    // Measured before this plan: without the init file an interactive bash
    // leaves the venv at position 3, behind ~/.local/bin, because .bashrc
    // prepends after us. This asserts the fix against a real bash.
    // A silent skip here is absence of signal dressed up as success: cargo
    // captures stdout on a passing test, so an eprintln'd "skipping" is
    // invisible and the run reports a green `ok`. This file already
    // hard-`expect`s a real uv, and the `uv-integration` feature gate already
    // means "I have a real toolchain" -- so be loud where the shell must
    // exist.
    let Some(bash) = which_bash() else {
        if cfg!(unix) {
            panic!("bash is required for this test on unix");
        }
        eprintln!("no bash on this machine; skipping");
        return;
    };

    let tmp = tempfile::tempdir().unwrap();
    let venv_bin = tmp.path().join("envs/api/bin");
    std::fs::create_dir_all(&venv_bin).unwrap();

    let file = init_file(ShellFamily::Bash, &venv_bin).expect("bash is a known shell");

    let first = first_path_entry_via_init_file(&bash, tmp.path(), &file);
    assert_eq!(
        first,
        venv_bin.to_string_lossy(),
        "the venv must be first after the user's rc has run; got {first}"
    );
}

#[test]
fn the_generated_init_file_puts_the_venv_first_in_a_real_interactive_zsh_shell() {
    // Same measurement as the bash case, but through ZDOTDIR rather than an
    // rcfile argument -- zsh has no --rcfile equivalent.
    // Loud only on macOS, where zsh is the default login shell and its
    // absence really is a broken machine. `ubuntu-latest` does not ship zsh,
    // so panicking on all of unix would turn a legitimate absence into a red
    // CI job. `#[ignore]` is not used anywhere here: it makes CI skip by
    // default, which is the same silence with extra steps.
    let Some(zsh) = which_zsh() else {
        if cfg!(target_os = "macos") {
            panic!("zsh is required for this test on macOS");
        }
        eprintln!("no zsh on this machine; skipping");
        return;
    };

    let tmp = tempfile::tempdir().unwrap();
    let venv_bin = tmp.path().join("envs/api/bin");
    std::fs::create_dir_all(&venv_bin).unwrap();

    let file = init_file(ShellFamily::Zsh, &venv_bin).expect("zsh is a known shell");

    let first = first_path_entry_via_init_file(&zsh, tmp.path(), &file);
    assert_eq!(
        first,
        venv_bin.to_string_lossy(),
        "the venv must be first after the user's rc has run; got {first}"
    );
}
