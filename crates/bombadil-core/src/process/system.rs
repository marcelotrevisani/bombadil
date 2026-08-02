use super::{CommandError, CommandOutcome, CommandRunner, CommandSpec};

/// Runs commands for real, via `std::process`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCommandRunner;

/// Everything both `run` and `spawn` configure identically. Only the stdio
/// and the wait-or-not differ between them.
fn configure(spec: &CommandSpec) -> std::process::Command {
    let mut command = std::process::Command::new(&spec.program);
    command.args(&spec.args);

    if let Some(dir) = &spec.cwd {
        command.current_dir(dir);
    }
    if spec.clear_env {
        command.env_clear();
    }
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    command
}

impl CommandRunner for SystemCommandRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutcome, CommandError> {
        let output = configure(spec)
            .output()
            .map_err(|source| CommandError::Spawn {
                program: spec.program.clone(),
                source,
            })?;

        Ok(CommandOutcome {
            status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    fn spawn(&self, spec: &CommandSpec) -> Result<(), CommandError> {
        // `Stdio::null()`, not the inherited handles and not `piped()`:
        // `Command::output` uses pipes, and an interactive shell writing into
        // a pipe nobody reads is neither a tty nor drained. A terminal
        // emulator opens its own window and needs none of our handles.
        configure(spec)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|source| CommandError::Spawn {
                program: spec.program.clone(),
                source,
            })?;

        // The `Child` is dropped here without being waited on. On Unix that
        // does not reap, so one zombie accumulates per terminal opened, until
        // Bombadil itself exits. Acceptable for a desktop GUI where terminals
        // are opened by hand a handful of times per session; if that ever
        // stops being true, reap with a `waitpid(WNOHANG)` sweep rather than
        // by blocking here.
        //
        // Windows console kinds (`cmd.exe`, `powershell.exe`) likely also
        // need CREATE_NEW_CONSOLE via `CommandExt::creation_flags` to get
        // their own window rather than attaching to Bombadil's (absent)
        // console. That is a Windows-only follow-up, deliberately not done
        // here -- it cannot be verified from this machine.
        Ok(())
    }
}
