use super::{CommandError, CommandOutcome, CommandRunner, CommandSpec};

/// Runs commands for real, via `std::process`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutcome, CommandError> {
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

        let output = command.output().map_err(|source| CommandError::Spawn {
            program: spec.program.clone(),
            source,
        })?;

        Ok(CommandOutcome {
            status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}
