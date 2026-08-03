use super::{CommandError, CommandOutcome, CommandRunner, CommandSpec, OutputLine};

use std::io::{BufRead, BufReader, Read};
use std::sync::mpsc;

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

    /// Pipes both output streams and reads them line by line while the child
    /// is still running, which is what `run` cannot do: `Command::output()`
    /// waits for the child to exit and hands back the whole buffer at once.
    ///
    /// One reader thread per pipe, both feeding a single channel this thread
    /// drains. Two threads rather than polling: a `BufReader::lines` loop on
    /// each pipe is the only portable way to read both without deadlocking on
    /// whichever one the child happens to fill first, and the channel is what
    /// puts the lines back into arrival order.
    fn stream(
        &self,
        spec: &CommandSpec,
        on_line: &mut dyn FnMut(OutputLine),
    ) -> Result<CommandOutcome, CommandError> {
        let spawn_error = |source| CommandError::Spawn {
            program: spec.program.clone(),
            source,
        };

        let mut child = configure(spec)
            // `null()`, not inherited: uv prompts for nothing, and a child
            // reading an inherited stdin that never answers would hang.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(spawn_error)?;

        let (tx, rx) = mpsc::channel();
        // `take()`, so the pipes move onto the reader threads and are closed
        // when those threads end -- which is what makes `rx` disconnect and
        // the loop below terminate.
        let stdout = child.stdout.take().map(|pipe| {
            let tx = tx.clone();
            std::thread::spawn(move || pump(pipe, &tx, OutputLine::Stdout))
        });
        let stderr = child.stderr.take().map(|pipe| {
            let tx = tx.clone();
            std::thread::spawn(move || pump(pipe, &tx, OutputLine::Stderr))
        });
        // The last sender this thread holds. Without dropping it the loop
        // below would never see a disconnect and would wait forever.
        drop(tx);

        let mut collected_stdout = String::new();
        let mut collected_stderr = String::new();
        for line in rx {
            let sink = match line {
                OutputLine::Stdout(_) => &mut collected_stdout,
                OutputLine::Stderr(_) => &mut collected_stderr,
            };
            sink.push_str(line.text());
            sink.push('\n');
            on_line(line);
        }
        // Joining is not for the output -- the channel already delivered all
        // of it -- but so a reader thread cannot outlive this call.
        for handle in [stdout, stderr].into_iter().flatten() {
            let _ = handle.join();
        }

        let status = child.wait().map_err(spawn_error)?;
        Ok(CommandOutcome {
            status: status.code(),
            stdout: collected_stdout,
            stderr: collected_stderr,
        })
    }
}

/// Read one pipe to EOF, sending each line as it lands.
///
/// Lossy UTF-8 rather than a hard error: uv is not the only thing that can
/// write to these pipes, and a stray non-UTF-8 byte from some tool in the
/// chain must not turn a working sync into a failed one.
fn pump(
    pipe: impl Read,
    tx: &mpsc::Sender<OutputLine>,
    tag: fn(String) -> OutputLine,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(pipe);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        if reader.read_until(b'\n', &mut buffer)? == 0 {
            return Ok(());
        }
        while buffer.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
            buffer.pop();
        }
        if tx
            .send(tag(String::from_utf8_lossy(&buffer).into_owned()))
            .is_err()
        {
            // The receiver is gone: nobody is left to show these lines.
            return Ok(());
        }
    }
}
