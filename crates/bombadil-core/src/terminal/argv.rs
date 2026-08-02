//! Building the argv used to spawn a terminal emulator.
//!
//! Every function here takes the target [`Os`] as a parameter rather than
//! asking [`Os::host`]; that is what lets all three operating systems'
//! argv be exercised by tests on whichever platform CI happens to run.
//!
//! Nothing here passes `--wait` to any emulator: verified against a real
//! `gnome-terminal`, it blocks the spawn until the user closes the window,
//! which would hang the GUI. The consequence is that a successful spawn says
//! nothing about whether the terminal actually opened -- that is handled
//! later, at spawn time, not here.
//!
//! ## Known limitation: six terminals are degraded, in two different ways
//!
//! Six kinds do not get full activation, and they do not all fail the same
//! way. [`TerminalKind::activation_support`] is the machine-readable form of
//! what follows, and the GUI should drive its warning off that rather than
//! off this comment.
//!
//! **`ActivationSupport::PathOnly` -- `PowerShell`, `Cmd`, `GitBash`.** These
//! are spawned as ordinary child processes, so the composed environment (and
//! `clear_env`) does reach them. What does not reach them is the re-assertion:
//! none of `cmd.exe /K cd /d <cwd>`, `powershell.exe -NoExit -Command
//! Set-Location ...` or `bash.exe --login -i` has a flag to hand the emulator
//! a specific shell binary plus arguments, so the generated init file passed
//! in here as `shell`/`shell_args` is dropped. `Cmd` and `PowerShell` are
//! near-fine in practice -- the composed `PATH` already leads with the venv
//! and neither has an rc that re-prepends -- while `GitBash` runs an
//! interactive login bash, whose rc reproduces the measured demotion.
//!
//! **`ActivationSupport::None` -- `TerminalApp`, `ITerm2`, `Wsl`.** These get
//! no composed environment at all, so the venv is not merely demoted on
//! `PATH`, it is absent from it. `open -a Terminal <cwd>` and `open -a iTerm
//! <cwd>` hand the request to LaunchServices, which does not propagate the
//! calling process's environment -- that is precisely why `launchctl setenv`
//! exists. `spec.env` and `spec.clear_env` are delivered to the `open` binary
//! and discarded; `cwd` survives only because it is an argument. `wsl.exe`
//! fails the same way across the WSL boundary: nothing crosses it except via
//! `WSLENV`, and a `C:\...\Scripts` path would be meaningless inside the
//! distro regardless.
//!
//! **`ActivationSupport::Full`** is all seven Linux kinds plus
//! `WindowsTerminal` -- the *only* Windows kind where activation works. The
//! GUI should steer Windows users toward it, and must not report the venv as
//! active for any of the six above.

use super::{Os, TerminalChoice, TerminalKind};
use crate::error::TerminalError;
use crate::process::CommandSpec;
use std::path::Path;

/// Build the argv to spawn `choice` on `os`, opening in `cwd` and, where the
/// emulator supports it, running `shell` with `shell_args`.
pub fn build(
    choice: &TerminalChoice,
    os: Os,
    cwd: &Path,
    shell: &str,
    shell_args: &[String],
) -> Result<CommandSpec, TerminalError> {
    match choice {
        TerminalChoice::Detected(kind) => build_detected(*kind, os, cwd, shell, shell_args),
        TerminalChoice::Custom { template } => build_custom(template, cwd, shell),
    }
}

fn build_detected(
    kind: TerminalKind,
    os: Os,
    cwd: &Path,
    shell: &str,
    shell_args: &[String],
) -> Result<CommandSpec, TerminalError> {
    if kind.os() != os {
        return Err(TerminalError::WrongOs { kind, os });
    }

    let cwd_str = cwd.to_string_lossy().into_owned();

    use TerminalKind::*;
    let spec = match kind {
        GnomeTerminal => CommandSpec::new("gnome-terminal")
            .arg(format!("--working-directory={cwd_str}"))
            .arg("--")
            .arg(shell)
            .args(shell_args.to_vec()),
        Konsole => CommandSpec::new("konsole")
            .arg("--workdir")
            .arg(cwd_str)
            .arg("-e")
            .arg(shell)
            .args(shell_args.to_vec()),
        Kitty => CommandSpec::new("kitty")
            .arg("--directory")
            .arg(cwd_str)
            .arg(shell)
            .args(shell_args.to_vec()),
        Alacritty => CommandSpec::new("alacritty")
            .arg("--working-directory")
            .arg(cwd_str)
            .arg("-e")
            .arg(shell)
            .args(shell_args.to_vec()),
        WezTerm => CommandSpec::new("wezterm")
            .arg("start")
            .arg("--cwd")
            .arg(cwd_str)
            .arg("--")
            .arg(shell)
            .args(shell_args.to_vec()),
        Foot => CommandSpec::new("foot")
            .arg(format!("--working-directory={cwd_str}"))
            .arg(shell)
            .args(shell_args.to_vec()),
        Xterm => CommandSpec::new("xterm")
            .arg("-e")
            .arg(shell)
            .args(shell_args.to_vec()),
        TerminalApp => CommandSpec::new("open")
            .arg("-a")
            .arg("Terminal")
            .arg(cwd_str),
        ITerm2 => CommandSpec::new("open").arg("-a").arg("iTerm").arg(cwd_str),
        WindowsTerminal => CommandSpec::new("wt.exe")
            .arg("-d")
            .arg(cwd_str)
            .arg(shell)
            .args(shell_args.to_vec()),
        PowerShell => CommandSpec::new("powershell.exe")
            .arg("-NoExit")
            .arg("-Command")
            .arg("Set-Location")
            .arg("-LiteralPath")
            // PowerShell's single-quoted strings escape an embedded quote by
            // doubling it; this project has shipped an unescaped-quote bug
            // in a shell interpolation once before (see activation.rs).
            .arg(format!("'{}'", cwd_str.replace('\'', "''"))),
        Cmd => CommandSpec::new("cmd.exe")
            .arg("/K")
            .arg("cd")
            .arg("/d")
            .arg(cwd_str),
        GitBash => CommandSpec::new("bash.exe").arg("--login").arg("-i"),
        Wsl => CommandSpec::new("wsl.exe").arg("--cd").arg(cwd_str),
    };

    Ok(spec.cwd(cwd.to_path_buf()))
}

fn build_custom(template: &str, cwd: &Path, shell: &str) -> Result<CommandSpec, TerminalError> {
    // Tokenize first, then substitute into each token: substituting into the
    // whole template before splitting would split a cwd containing a space
    // (ordinary on Windows and macOS) into two arguments.
    let cwd_str = cwd.to_string_lossy();
    let mut tokens = template
        .split_whitespace()
        .map(|tok| tok.replace("{cwd}", &cwd_str).replace("{shell}", shell));

    let program = tokens.next().ok_or(TerminalError::EmptyTemplate)?;

    Ok(CommandSpec::new(program)
        .args(tokens)
        .cwd(cwd.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn cwd() -> &'static Path {
        Path::new("/proj/my-api")
    }

    fn built(kind: TerminalKind, os: Os) -> CommandSpec {
        build(
            &TerminalChoice::Detected(kind),
            os,
            cwd(),
            "/bin/bash",
            &["-i".to_string()],
        )
        .expect("argv must build")
    }

    #[test]
    fn gnome_terminal_gets_the_working_directory_and_the_shell_after_a_separator() {
        let spec = built(TerminalKind::GnomeTerminal, Os::Linux);
        let args = spec.args.join(" ");
        assert!(
            args.contains("--working-directory=/proj/my-api"),
            "got {args}"
        );
        let sep = spec.args.iter().position(|a| a == "--").expect("needs --");
        let shell = spec
            .args
            .iter()
            .position(|a| a == "/bin/bash")
            .expect("needs shell");
        assert!(
            sep < shell,
            "the shell must follow the separator; got {args}"
        );
    }

    #[test]
    fn no_terminal_is_ever_told_to_wait() {
        // --wait blocks until the user closes the window, which would hang the
        // GUI. Verified against gnome-terminal.
        for os in [Os::Linux, Os::MacOs, Os::Windows] {
            for kind in TerminalKind::for_os(os) {
                let spec = built(kind, os);
                assert!(
                    !spec.args.iter().any(|a| a == "--wait"),
                    "{kind:?} must not wait; got {:?}",
                    spec.args
                );
            }
        }
    }

    #[test]
    fn every_terminal_on_every_os_builds_without_erroring() {
        // The exhaustive match in build_detected (no wildcard arm) is what
        // actually guarantees every kind is handled: a missing arm fails to
        // compile. This test only checks that the arm which exists for each
        // kind produces a spawnable spec.
        for os in [Os::Linux, Os::MacOs, Os::Windows] {
            for kind in TerminalKind::for_os(os) {
                let spec = build(&TerminalChoice::Detected(kind), os, cwd(), "/bin/bash", &[])
                    .unwrap_or_else(|e| panic!("{kind:?} on {os:?} failed: {e}"));
                assert!(
                    !spec.program.as_os_str().is_empty(),
                    "{kind:?} has no program"
                );
            }
        }
    }

    #[test]
    fn a_terminal_from_the_wrong_os_is_refused() {
        // Offering konsole on Windows would produce an argv that cannot run.
        let err = build(
            &TerminalChoice::Detected(TerminalKind::Konsole),
            Os::Windows,
            cwd(),
            "cmd.exe",
            &[],
        )
        .unwrap_err();
        let rendered = err.to_string();
        assert!(
            rendered.contains("Konsole") || rendered.contains("konsole"),
            "got {rendered}"
        );
    }

    #[test]
    fn windows_terminal_uses_its_own_directory_flag() {
        let spec = built(TerminalKind::WindowsTerminal, Os::Windows);
        let args = spec.args.join(" ");
        assert!(args.contains("-d"), "got {args}");
        assert!(args.contains("/proj/my-api"), "got {args}");
    }

    #[test]
    fn macos_terminal_app_is_launched_through_open() {
        // Terminal.app is a bundle, not a binary, so it is launched via `open`.
        let spec = built(TerminalKind::TerminalApp, Os::MacOs);
        assert_eq!(spec.program, std::path::PathBuf::from("open"));
        let args = spec.args.join(" ");
        assert!(args.contains("-a"), "got {args}");
    }

    #[test]
    fn a_custom_template_substitutes_both_placeholders() {
        let spec = build(
            &TerminalChoice::Custom {
                template: "myterm --cd {cwd} -e {shell}".into(),
            },
            Os::Linux,
            cwd(),
            "/bin/zsh",
            &[],
        )
        .unwrap();
        assert_eq!(spec.program, std::path::PathBuf::from("myterm"));
        assert_eq!(spec.args, vec!["--cd", "/proj/my-api", "-e", "/bin/zsh"]);
    }

    #[test]
    fn a_custom_template_keeps_a_spacy_cwd_as_one_argument() {
        // Substituting into the whole string before splitting on whitespace
        // would split "/proj/my api" into two arguments.
        let spec = build(
            &TerminalChoice::Custom {
                template: "myterm --cd {cwd} -e {shell}".into(),
            },
            Os::Linux,
            Path::new("/proj/my api"),
            "/bin/zsh",
            &[],
        )
        .unwrap();
        assert_eq!(spec.args, vec!["--cd", "/proj/my api", "-e", "/bin/zsh"]);
    }

    #[test]
    fn powershell_escapes_an_apostrophe_in_the_working_directory() {
        // A single quote inside -LiteralPath '<cwd>' would otherwise close
        // the quoted string early; PowerShell's escape is a doubled quote.
        let spec = build(
            &TerminalChoice::Detected(TerminalKind::PowerShell),
            Os::Windows,
            Path::new("/proj/o'brien"),
            "powershell.exe",
            &[],
        )
        .unwrap();
        assert!(
            spec.args.iter().any(|a| a == "'/proj/o''brien'"),
            "got {:?}",
            spec.args
        );
    }

    #[test]
    fn a_custom_template_that_is_empty_is_refused_rather_than_spawned() {
        let err = build(
            &TerminalChoice::Custom {
                template: "   ".into(),
            },
            Os::Linux,
            cwd(),
            "/bin/zsh",
            &[],
        )
        .unwrap_err();
        assert!(err.to_string().contains("empty"), "got {err}");
    }

    #[test]
    fn shell_args_reach_the_spawned_shell() {
        // Task 2's --rcfile arguments are useless if they are dropped here.
        let spec = build(
            &TerminalChoice::Detected(TerminalKind::GnomeTerminal),
            Os::Linux,
            cwd(),
            "/bin/bash",
            &[
                "--rcfile".to_string(),
                "/tmp/x/bombadil-bashrc".to_string(),
                "-i".to_string(),
            ],
        )
        .unwrap();
        let args = spec.args.join(" ");
        assert!(args.contains("--rcfile"), "got {args}");
        assert!(args.contains("/tmp/x/bombadil-bashrc"), "got {args}");
    }
}
