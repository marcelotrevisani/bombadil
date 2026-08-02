use super::parse_dump;
use crate::error::EnvError;
use crate::model::Script;
use crate::process::{CommandRunner, CommandSpec};
use std::collections::BTreeMap;

/// Written as the last record of every dump, and required by [`apply`].
///
/// A sourced script runs in the *same* shell, so an `exit 0` anywhere in it —
/// `[ -f "$creds" ] || exit 0` is an everyday guard — terminates `sh -c`
/// before `env -0` runs. Verified: `sh -c ". './exit0.sh' >/dev/null; env -0"`
/// exits 0 with zero bytes of stdout. Without this marker that is
/// indistinguishable from "the environment is genuinely empty", and since the
/// dump replaces the environment wholesale it would silently discard the
/// parent environment, every configured variable, every keychain secret and
/// every index credential — and return `Ok`. Dot-sourcing in PowerShell
/// behaves the same way.
const SENTINEL_KEY: &str = "__BOMBADIL_DUMP_OK";

/// Escape a string for interpolation inside a single-quoted shell literal.
///
/// Without this, a path or argument containing an apostrophe (`o'brien`) ends
/// the quote early: verified to fail with `Syntax error: Unterminated quoted
/// string`.
fn quote(raw: &str) -> String {
    if cfg!(windows) {
        raw.replace('\'', "''")
    } else {
        raw.replace('\'', r"'\''")
    }
}

/// Build the command that sources a script and dumps the resulting
/// environment.
///
/// POSIX uses `sh -c "set -- <args>; . '<script>' >/dev/null; env -0;
/// <sentinel>"`, verified to capture values with spaces, `=` and newlines, and
/// to surface the script's exit status.
/// stdout alone is redirected to `/dev/null` (not `2>&1`): anything the
/// script prints there would land on the same stream as `env -0`'s
/// NUL-separated dump and corrupt it. stderr is left alone deliberately —
/// it is the only diagnostic a user gets when their script fails, and
/// [`EnvError::ScriptFailed`] reports it. Redirecting stderr too would make
/// every failure report an empty message; resist "tidying" this back to
/// `2>&1`.
///
/// Arguments go through positional parameters rather than trailing operands
/// of `.`, because dash's `.` builtin *ignores* them: verified, `sh -c
/// ". './argp.sh' a b; env -0"` yields an empty `$1` while bash yields `a`.
/// `/bin/sh` is dash on Debian and Ubuntu, so the trailing-operand form made
/// [`Script::args`] silently do nothing there. `set --` is emitted even with
/// no arguments; it simply clears the (already empty) parameters.
///
/// The sentinel is written with `printf '...\000'`: three-digit octal, verified
/// to emit a real NUL under both dash and bash on this machine. A bare `\0` is
/// not portably required to mean NUL.
///
/// Windows uses PowerShell writing the same NUL-separated records. That form
/// is NOT verified on a development machine — the integration test runs on the
/// Windows CI job, which is what proves it. `[Console]::OutputEncoding` is
/// pinned to UTF-8 first: the dump is read back as UTF-8 (`parse_dump` drops
/// any record that isn't), but PowerShell's default console output encoding
/// is not guaranteed UTF-8, so without this a non-ASCII variable value would
/// silently vanish rather than error. It must be the **BOM-less**
/// `UTF8Encoding $false` — `[Text.Encoding]::UTF8` emits a BOM, which would
/// prefix redirected stdout with `EF BB BF` and rename the dump's first key to
/// `\u{feff}Path`. `$ErrorActionPreference = 'Stop'` is
/// set next because `powershell -Command` otherwise exits 0 even when the
/// sourced script writes errors; without it a broken script would silently
/// produce a truncated environment instead of aborting.
pub fn dump_command(script: &Script) -> CommandSpec {
    let path = quote(&script.path.to_string_lossy());
    let args = script
        .args
        .iter()
        .map(|a| format!("'{}'", quote(a)))
        .collect::<Vec<_>>()
        .join(" ");

    #[cfg(not(windows))]
    {
        let program =
            format!("set -- {args}; . '{path}' >/dev/null; env -0; printf '{SENTINEL_KEY}=1\\000'");
        CommandSpec::new("sh").arg("-c").arg(program)
    }

    #[cfg(windows)]
    {
        let program = format!(
            "[Console]::OutputEncoding = (New-Object System.Text.UTF8Encoding $false); \
             $ErrorActionPreference = 'Stop'; \
             . '{path}' {args} | Out-Null; \
             Get-ChildItem Env: | ForEach-Object {{ \
                 [Console]::Out.Write($_.Name + '=' + $_.Value + [char]0) }}; \
             [Console]::Out.Write('{SENTINEL_KEY}=1' + [char]0)"
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
/// output attached rather than applying partially, and a dump missing its
/// trailing [`SENTINEL_KEY`] record aborts with
/// [`EnvError::ScriptDumpIncomplete`] rather than returning the truncated
/// environment as if it were complete.
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

    let mut env = parse_dump(outcome.stdout.as_bytes());
    // Absent sentinel means the dump never finished -- an `exit` in the
    // script, most likely. Returning what we got would wipe the environment.
    if env.remove(SENTINEL_KEY).is_none() {
        return Err(EnvError::ScriptDumpIncomplete {
            script: script.path.clone(),
        });
    }
    Ok(env)
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

    /// A complete dump: the records plus the trailing sentinel a real shell
    /// writes once `env -0` has run to the end.
    fn dump(records: &[&str]) -> String {
        let mut out = truncated_dump(records);
        out.push_str(SENTINEL_KEY);
        out.push_str("=1\0");
        out
    }

    /// A dump cut short, as a script calling `exit` produces.
    fn truncated_dump(records: &[&str]) -> String {
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
    fn an_empty_dump_is_an_error_not_an_empty_environment() {
        // Verified against a real shell: a sourced script calling `exit 0`
        // ends `sh -c` before `env -0` runs, so the command exits 0 with zero
        // bytes of stdout. Trusting that would replace the whole composed
        // environment -- parent, variables, keychain secrets, index
        // credentials -- with nothing, and still return Ok.
        let runner = FakeCommandRunner::new();
        runner.expect(
            "sh",
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let err = apply(&script("/p/pre.sh"), &runner, &base()).unwrap_err();

        assert!(
            matches!(err, EnvError::ScriptDumpIncomplete { .. }),
            "got {err:?}"
        );
        assert!(err.to_string().contains("/p/pre.sh"), "got {err}");
    }

    #[test]
    fn a_truncated_dump_is_an_error_not_a_partial_environment() {
        // `exit` part-way through is the same failure with some records
        // already written: plausible-looking output that is missing whatever
        // came after it.
        let runner = FakeCommandRunner::new();
        runner.expect(
            "sh",
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: truncated_dump(&["KEEP=unchanged"]),
                stderr: String::new(),
            },
        );

        let err = apply(&script("/p/pre.sh"), &runner, &base()).unwrap_err();

        assert!(
            matches!(err, EnvError::ScriptDumpIncomplete { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn the_sentinel_never_reaches_the_child_process() {
        let runner = FakeCommandRunner::new();
        runner.expect(
            "sh",
            &[],
            CommandOutcome {
                status: Some(0),
                stdout: dump(&["KEEP=unchanged"]),
                stderr: String::new(),
            },
        );

        let got = apply(&script("/p/pre.sh"), &runner, &base()).unwrap();

        assert!(!got.contains_key(SENTINEL_KEY), "got {got:?}");
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
        assert!(
            spec.args[1].contains(r"printf '__BOMBADIL_DUMP_OK=1\000'"),
            "the dump must be self-terminating; got {}",
            spec.args[1]
        );
    }

    #[cfg(unix)]
    #[test]
    fn posix_arguments_go_through_positional_parameters() {
        // Verified against real shells: dash's `.` builtin ignores trailing
        // operands, so `. 'x.sh' 'a b'` leaves $1 empty there while bash sets
        // it. `set --` before the `.` works on both.
        let mut s = script("/p/pre.sh");
        s.args = vec!["a b".to_string(), "c".to_string()];

        let spec = dump_command(&s);

        assert!(
            spec.args[1].starts_with("set -- 'a b' 'c'; . '/p/pre.sh'"),
            "got {}",
            spec.args[1]
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_apostrophe_in_the_path_or_an_argument_is_escaped() {
        // Verified: unescaped, `sh -c ". 'o'brien/pre.sh'; env -0"` dies with
        // "Unterminated quoted string" and the script cannot run at all.
        let mut s = script("/home/o'brien/pre.sh");
        s.args = vec!["it's".to_string()];

        let spec = dump_command(&s);

        assert!(
            spec.args[1].contains(r"'/home/o'\''brien/pre.sh'"),
            "got {}",
            spec.args[1]
        );
        assert!(spec.args[1].contains(r"'it'\''s'"), "got {}", spec.args[1]);
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
        assert!(
            joined.contains("__BOMBADIL_DUMP_OK=1"),
            "the dump must be self-terminating; got {joined}"
        );
        assert!(
            joined.contains("UTF8Encoding $false"),
            "the encoding must be BOM-less, or the first key gains a \\u{{feff}}; got {joined}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn an_apostrophe_in_the_windows_path_is_doubled() {
        let spec = dump_command(&script(r"C:\o'brien\pre.ps1"));
        assert!(
            spec.args.join(" ").contains(r"C:\o''brien\pre.ps1"),
            "got {:?}",
            spec.args
        );
    }
}
