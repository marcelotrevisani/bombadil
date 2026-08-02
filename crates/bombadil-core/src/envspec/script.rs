use super::parse_dump;
use crate::error::EnvError;
use crate::model::Script;
use crate::process::{CommandRunner, CommandSpec};
use std::collections::BTreeMap;

/// Build the command that sources a script and dumps the resulting
/// environment.
///
/// POSIX uses `sh -c '. <script>; env -0'`, verified to capture values with
/// spaces, `=` and newlines, and to surface the script's exit status.
/// stdout alone is redirected to `/dev/null` (not `2>&1`): anything the
/// script prints there would land on the same stream as `env -0`'s
/// NUL-separated dump and corrupt it. stderr is left alone deliberately —
/// it is the only diagnostic a user gets when their script fails, and
/// [`EnvError::ScriptFailed`] reports it. Redirecting stderr too would make
/// every failure report an empty message; resist "tidying" this back to
/// `2>&1`.
///
/// Windows uses PowerShell writing the same NUL-separated records. That form
/// is NOT verified on a development machine — the integration test runs on the
/// Windows CI job, which is what proves it. `[Console]::OutputEncoding` is
/// pinned to UTF-8 first: the dump is read back as UTF-8 (`parse_dump` drops
/// any record that isn't), but PowerShell's default console output encoding
/// is not guaranteed UTF-8, so without this a non-ASCII variable value would
/// silently vanish rather than error. `$ErrorActionPreference = 'Stop'` is
/// set next because `powershell -Command` otherwise exits 0 even when the
/// sourced script writes errors; without it a broken script would silently
/// produce a truncated environment instead of aborting.
pub fn dump_command(script: &Script) -> CommandSpec {
    let path = script.path.to_string_lossy();
    let args = script.args.join(" ");

    #[cfg(not(windows))]
    {
        let program = format!(". '{path}' {args} >/dev/null; env -0");
        CommandSpec::new("sh").arg("-c").arg(program)
    }

    #[cfg(windows)]
    {
        let program = format!(
            "[Console]::OutputEncoding = [Text.Encoding]::UTF8; \
             $ErrorActionPreference = 'Stop'; \
             . '{path}' {args} | Out-Null; \
             Get-ChildItem Env: | ForEach-Object {{ \
                 [Console]::Out.Write($_.Name + '=' + $_.Value + [char]0) }}"
        );
        CommandSpec::new("powershell")
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-Command")
            .arg(program)
    }
}

/// Run one pre-activate script and fold its environment changes into `base`.
///
/// A script cannot alter its parent's environment directly, so it is sourced
/// and the result dumped; the dump replaces `base` wholesale, which is what
/// makes both additions and **removals** take effect — a variable the script
/// `unset`s is genuinely absent from the dump.
///
/// `base` becomes the child's *entire* environment (`clear_env = true`), so
/// the script sees exactly `base` and the diff against it is meaningful.
/// That means `base` must carry `PATH`, or `sh` cannot find `env` to produce
/// the dump at all — this function does not add it; composing `base` with a
/// usable `PATH` is the caller's job.
///
/// A disabled script is not run. A script exiting non-zero aborts with its
/// output attached rather than applying partially.
pub fn apply(
    script: &Script,
    runner: &dyn CommandRunner,
    base: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, EnvError> {
    if !script.enabled {
        return Ok(base.clone());
    }

    let mut spec = dump_command(script);
    spec.env = base.clone();
    spec.clear_env = true;

    let outcome = runner.run(&spec).map_err(|source| EnvError::ScriptSpawn {
        script: script.path.clone(),
        message: source.to_string(),
    })?;

    if !outcome.success() {
        return Err(EnvError::ScriptFailed {
            script: script.path.clone(),
            status: outcome.status,
            output: outcome.stderr,
        });
    }

    Ok(parse_dump(outcome.stdout.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{CommandOutcome, FakeCommandRunner};
    use std::path::PathBuf;

    fn script(path: &str) -> Script {
        Script {
            path: PathBuf::from(path),
            args: vec![],
            enabled: true,
        }
    }

    fn base() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("KEEP".to_string(), "unchanged".to_string()),
            ("REPLACED".to_string(), "before".to_string()),
            ("REMOVED".to_string(), "present".to_string()),
        ])
    }

    fn dump(records: &[&str]) -> String {
        let mut out = String::new();
        for r in records {
            out.push_str(r);
            out.push('\0');
        }
        out
    }

    #[test]
    fn an_exported_variable_is_added() {
        let runner = FakeCommandRunner::new();
        runner.expect(
            "sh",
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: dump(&[
                    "KEEP=unchanged",
                    "REPLACED=before",
                    "REMOVED=present",
                    "NEW=added",
                ]),
                stderr: String::new(),
            },
        );

        let got = apply(&script("/p/pre.sh"), &runner, &base()).unwrap();

        assert_eq!(got.get("NEW").map(String::as_str), Some("added"));
        assert_eq!(got.get("KEEP").map(String::as_str), Some("unchanged"));

        // Pin the glue: dump_command and the CommandSpec fields are each
        // tested on their own, but nothing else confirms apply actually
        // wires them together instead of e.g. running a bare `sh`.
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        let spec = &calls[0];
        assert!(
            spec.clear_env,
            "the script must see exactly `base`, or the dump diff is meaningless"
        );
        assert_eq!(spec.env, base());
        assert!(
            spec.args
                .iter()
                .any(|a| a.contains("env -0") || a.contains("[char]0")),
            "apply must go through dump_command; got {:?}",
            spec.args
        );
    }

    #[test]
    fn a_changed_variable_takes_the_scripts_value() {
        let runner = FakeCommandRunner::new();
        runner.expect(
            "sh",
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: dump(&["KEEP=unchanged", "REPLACED=after", "REMOVED=present"]),
                stderr: String::new(),
            },
        );

        let got = apply(&script("/p/pre.sh"), &runner, &base()).unwrap();

        assert_eq!(got.get("REPLACED").map(String::as_str), Some("after"));
    }

    #[test]
    fn a_variable_the_script_unsets_is_removed() {
        // Verified against a real shell: an unset variable is genuinely absent
        // from the dump, so a script can remove one and we must honour it.
        let runner = FakeCommandRunner::new();
        runner.expect(
            "sh",
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: dump(&["KEEP=unchanged", "REPLACED=before"]),
                stderr: String::new(),
            },
        );

        let got = apply(&script("/p/pre.sh"), &runner, &base()).unwrap();

        assert!(!got.contains_key("REMOVED"), "got {got:?}");
    }

    #[test]
    fn a_failing_script_aborts_and_reports_its_output() {
        // Partial application would leave a half-built environment that is
        // worse than none, so a non-zero exit stops the whole composition.
        let runner = FakeCommandRunner::new();
        runner.expect(
            "sh",
            &[],
            CommandOutcome {
                status: Some(3),
                stdout: String::new(),
                stderr: "boom".into(),
            },
        );

        let err = apply(&script("/p/pre.sh"), &runner, &base()).unwrap_err();

        let rendered = err.to_string();
        assert!(rendered.contains("/p/pre.sh"), "got {rendered}");
        assert!(rendered.contains("boom"), "got {rendered}");
    }

    #[test]
    fn a_disabled_script_is_not_run_at_all() {
        let runner = FakeCommandRunner::new();
        let mut s = script("/p/pre.sh");
        s.enabled = false;

        let got = apply(&s, &runner, &base()).unwrap();

        assert_eq!(got, base());
        assert!(runner.calls().is_empty(), "a disabled script must not run");
    }

    #[cfg(unix)]
    #[test]
    fn the_posix_command_sources_the_script_and_dumps_the_environment() {
        let spec = dump_command(&script("/p/pre.sh"));
        assert_eq!(spec.program, PathBuf::from("sh"));
        assert_eq!(spec.args.len(), 2);
        assert_eq!(spec.args[0], "-c");
        assert!(spec.args[1].contains("/p/pre.sh"), "got {}", spec.args[1]);
        assert!(
            spec.args[1].contains("env -0"),
            "the dump must be NUL-separated; got {}",
            spec.args[1]
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_command_uses_powershell_and_the_same_nul_contract() {
        let spec = dump_command(&script(r"C:\p\pre.ps1"));
        assert!(
            spec.program
                .to_string_lossy()
                .to_lowercase()
                .contains("powershell"),
            "got {:?}",
            spec.program
        );
        let joined = spec.args.join(" ");
        assert!(joined.contains(r"C:\p\pre.ps1"), "got {joined}");
        assert!(
            joined.contains("[char]0"),
            "the dump must be NUL-separated; got {joined}"
        );
    }
}
