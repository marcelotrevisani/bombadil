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
        self.stream_cancellable(spec, on_line, &std::sync::atomic::AtomicBool::new(false))
    }

    fn stream_cancellable(
        &self,
        spec: &CommandSpec,
        on_line: &mut dyn FnMut(OutputLine),
        cancel: &std::sync::atomic::AtomicBool,
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
        let mut cancelled = false;
        loop {
            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                // Stopped, then not drained: a grandchild may still hold the
                // pipes open, and waiting on it is what Cancel is to avoid.
                stop_tree(&mut child);
                cancelled = true;
                break;
            }
            let line = match rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            let sink = match line {
                OutputLine::Stdout(_) => &mut collected_stdout,
                OutputLine::Stderr(_) => &mut collected_stderr,
            };
            sink.push_str(line.text());
            sink.push('\n');
            on_line(line);
        }
        // Joining is not for the output -- the channel already delivered all
        // of it -- but so a reader thread cannot outlive this call. Skipped
        // after a cancel, for the same reason the drain is.
        if !cancelled {
            for handle in [stdout, stderr].into_iter().flatten() {
                let _ = handle.join();
            }
        }

        let status = child.wait().map_err(spawn_error)?;
        Ok(CommandOutcome {
            status: status.code(),
            stdout: collected_stdout,
            stderr: collected_stderr,
        })
    }
}

/// How long a stopped program gets to exit on its own before it is killed.
#[cfg(unix)]
const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// Ends `child` and every process it started.
///
/// Never just `child.kill()` on Unix: that is SIGKILL, which `uv run` cannot
/// forward, so the Python it started keeps running with nobody to stop it
/// (verified with uv 0.12.19). SIGTERM is forwarded. On Windows `taskkill /T`
/// walks the tree, whatever uv does with job objects.
fn stop_tree(child: &mut std::process::Child) {
    let pid = child.id().to_string();
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let deadline = std::time::Instant::now() + STOP_GRACE;
        while std::time::Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    let _ = child.kill();
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn a_cancelled_stream_kills_the_child_and_returns() {
        let spec = CommandSpec::new("sh")
            .arg("-c")
            .arg("echo started; exec sleep 30");
        let cancel = AtomicBool::new(false);
        let started = std::time::Instant::now();
        let outcome = SystemCommandRunner
            .stream_cancellable(
                &spec,
                &mut |line| {
                    if line.text() == "started" {
                        cancel.store(true, Ordering::SeqCst);
                    }
                },
                &cancel,
            )
            .expect("the child must run");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
        assert_ne!(outcome.status, Some(0), "killed, not finished");
        assert!(outcome.stdout.contains("started"));
    }

    fn cancel_after(ms: u64) -> std::sync::Arc<AtomicBool> {
        let flag = std::sync::Arc::new(AtomicBool::new(false));
        let set = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            set.store(true, Ordering::SeqCst);
        });
        flag
    }

    #[test]
    fn stop_sends_term_first() {
        // A child that exits 0 on TERM: SIGKILL would leave no exit code.
        let spec = CommandSpec::new("sh")
            .arg("-c")
            .arg("trap 'exit 0' TERM; sleep 30 & wait");
        let cancel = cancel_after(300);
        let outcome = SystemCommandRunner
            .stream_cancellable(&spec, &mut |_| {}, &cancel)
            .unwrap();
        assert_eq!(outcome.status, Some(0), "TERM reached the child");
    }

    #[test]
    fn stop_kills_a_child_that_ignores_term() {
        let spec = CommandSpec::new("sh")
            .arg("-c")
            .arg("trap '' TERM; sleep 30");
        let cancel = cancel_after(300);
        let started = std::time::Instant::now();
        let outcome = SystemCommandRunner
            .stream_cancellable(&spec, &mut |_| {}, &cancel)
            .unwrap();
        let took = started.elapsed();
        assert_eq!(outcome.status, None, "killed, not exited");
        assert!(
            took >= std::time::Duration::from_secs(3),
            "{took:?}: waited the grace"
        );
        assert!(took < std::time::Duration::from_secs(10), "{took:?}");
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn pings_running() -> bool {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq PING.EXE", "/NH"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .to_ascii_lowercase()
            .contains("ping.exe")
    }

    #[test]
    fn stop_ends_the_grandchild_too() {
        assert!(
            !pings_running(),
            "a ping was already running; the check would lie"
        );
        let spec = CommandSpec::new("cmd")
            .arg("/C")
            .arg("ping -n 60 127.0.0.1 > nul");
        let cancel = Arc::new(AtomicBool::new(false));
        let set = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            set.store(true, Ordering::SeqCst);
        });
        SystemCommandRunner
            .stream_cancellable(&spec, &mut |_| {}, &cancel)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(!pings_running(), "the grandchild outlived Stop");
    }
}
