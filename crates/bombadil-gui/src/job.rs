//! The job seam: run blocking `bombadil-core` calls off iced's render thread.
//!
//! `CommandRunner::run` is synchronous and `SystemCommandRunner` calls
//! `Command::output()`, which blocks until the child exits. Calling it from
//! `update` would freeze the window. `iced::Task::perform` takes an `async`
//! block, but iced's executor is not a blocking pool, so the blocking work
//! still has to move to a real OS thread -- `run` below does that with
//! `std::thread::spawn` and hands the result back over an `mpsc` channel.
//!
//! `rx.recv_timeout` blocks whatever thread calls `run`. Inside
//! `Task::perform` that is one of iced's executor threads, not the render
//! thread, so the window keeps painting while a job is in flight. Blocking an
//! executor thread is a deliberate tradeoff, not an oversight: this
//! application only ever has a handful of jobs in flight at once (startup
//! probes, a uv command), so parking a thread per job is cheap and avoids
//! pulling in an async runtime (no `tokio`, no `futures`) for what an
//! `mpsc` channel already does.

use std::sync::mpsc;
use std::time::Duration;

/// What became of a piece of background work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobResult<T> {
    Done(T),
    /// The work exceeded its budget. The thread is left running -- it cannot
    /// be killed safely -- but the UI stops waiting on it.
    TimedOut,
    /// The work panicked. Reported rather than propagated so one bad call
    /// cannot take the window down.
    Failed,
}

/// Run blocking work off the render thread, bounded by `timeout`.
///
/// `CommandRunner::run` is synchronous and `SystemCommandRunner` blocks until
/// the child exits, so calling it from `update` would freeze the window. The
/// timeout exists because startup probes can hang indefinitely on a stale
/// network mount; a probe that never answers must degrade to "absent" rather
/// than making the application appear dead.
///
/// A timed-out thread is abandoned, not killed: there is no safe way to stop
/// it, and leaking one thread beats leaving the UI wedged.
pub async fn run<T, F>(work: F, timeout: Duration) -> JobResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });

    match rx.recv_timeout(timeout) {
        Ok(value) => JobResult::Done(value),
        Err(mpsc::RecvTimeoutError::Timeout) => JobResult::TimedOut,
        // A panicking job arrives here as `Disconnected`, not as a caught
        // unwind: the panic happens before the send, so the sender is
        // dropped during unwinding and the channel closes. `std::thread`
        // already isolates the panic from this thread, so catching it would
        // add nothing.
        Err(mpsc::RecvTimeoutError::Disconnected) => JobResult::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn work_that_finishes_returns_its_value() {
        let got = pollster_block(run(|| 21 * 2, Duration::from_secs(5)));
        assert_eq!(got, JobResult::Done(42));
    }

    #[test]
    fn work_that_hangs_times_out_rather_than_blocking_forever() {
        // This is the whole point: a probe walking a stale network mount must
        // degrade to a timeout, not freeze the window.
        let got: JobResult<()> = pollster_block(run(
            || std::thread::sleep(Duration::from_secs(30)),
            Duration::from_millis(50),
        ));
        assert_eq!(got, JobResult::TimedOut);
    }

    #[test]
    fn work_that_panics_is_reported_rather_than_killing_the_app() {
        // A panic inside a worker thread must not poison the UI. The user gets
        // a failure they can see instead of a window that stops updating.
        let got: JobResult<()> = pollster_block(run(|| panic!("boom"), Duration::from_secs(5)));
        assert_eq!(got, JobResult::Failed);
    }

    #[test]
    fn a_timed_out_job_does_not_block_the_caller_for_the_works_duration() {
        let started = std::time::Instant::now();
        let _: JobResult<()> = pollster_block(run(
            || std::thread::sleep(Duration::from_secs(30)),
            Duration::from_millis(50),
        ));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "returned after {:?}; the timeout did not bound the wait",
            started.elapsed()
        );
    }

    /// Drive a future to completion on this thread. `job::run`'s future does
    /// its waiting on a channel, so a trivial executor is enough and avoids
    /// pulling in a runtime.
    fn pollster_block<F: std::future::Future>(mut f: F) -> F::Output {
        use std::task::{Context, Poll, Waker};
        let mut f = unsafe { std::pin::Pin::new_unchecked(&mut f) };
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
            std::thread::yield_now();
        }
    }
}
