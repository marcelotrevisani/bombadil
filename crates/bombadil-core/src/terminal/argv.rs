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
//! ## Known limitation: three terminals cannot carry the shell
//!
//! `open -a Terminal <cwd>` and `open -a iTerm <cwd>` (macOS), and
//! `cmd.exe /K cd /d <cwd>` (Windows) launch the user's own default shell --
//! they have no flag to hand it a specific shell binary or arguments. Task 2's
//! activation re-assertion (the generated init file, passed in here as
//! `shell`/`shell_args`) therefore never reaches those three. The GUI should
//! surface this as a known limitation rather than Bombadil silently doing
//! nothing.

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
            .arg(format!("'{cwd_str}'")),
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
    let substituted = template
        .replace("{cwd}", &cwd.to_string_lossy())
        .replace("{shell}", shell);

    let mut tokens = substituted.split_whitespace();
    let program = tokens.next().ok_or(TerminalError::EmptyTemplate)?;

    Ok(CommandSpec::new(program)
        .args(tokens.map(str::to_string))
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
        // A missing arm would mean a detected terminal that cannot be opened.
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
