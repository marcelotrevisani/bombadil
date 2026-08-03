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
//! pulling in an async runtime (no `tokio`) for what an `mpsc` channel
//! already does.
//!
//! [`stream`] is the same deal for work that produces output as it runs: the
//! thread and the channel are unchanged, and `futures::stream::unfold` only
//! turns the receiver into the `Stream` `iced::Task::stream` wants. `futures`
//! is not a runtime and was already in the lockfile via iced.

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

/// Run blocking work that produces items as it goes, off the render thread,
/// and deliver each one as it is produced.
///
/// The streaming counterpart of [`run`], for `CommandRunner::stream`: `run`
/// can only report a job's single final value, so a `uv sync` behind it stays
/// invisible until it exits.
///
/// `timeout` bounds the wait for *each* item rather than the whole job, which
/// is the only bound that still means anything here. A large `uv sync` can
/// legitimately outlast any total budget, but one that has printed nothing
/// for `timeout` has stopped making progress -- and unlike [`run`]'s deadline,
/// this one cannot fire on a job that is plainly still working.
///
/// The stream ends when `work` returns. A `work` that panics ends it with
/// [`JobResult::Failed`]: the thread dies without sending its end marker, and
/// the channel disconnecting is how that arrives here.
pub fn stream<T, F>(work: F, timeout: Duration) -> impl futures::Stream<Item = JobResult<T>>
where
    T: Send + 'static,
    F: FnOnce(&mut dyn FnMut(T)) + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let emitter = tx.clone();
        work(&mut move |item| {
            let _ = emitter.send(Some(item));
        });
        // The end marker. Its absence is what tells the reader a panic
        // happened rather than a normal finish.
        let _ = tx.send(None);
    });

    futures::stream::unfold(Some(rx), move |state| async move {
        let rx = state?;
        match rx.recv_timeout(timeout) {
            Ok(Some(item)) => Some((JobResult::Done(item), Some(rx))),
            Ok(None) => None,
            Err(mpsc::RecvTimeoutError::Timeout) => Some((JobResult::TimedOut, None)),
            Err(mpsc::RecvTimeoutError::Disconnected) => Some((JobResult::Failed, None)),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
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

    // --- stream ---------------------------------------------------------

    #[test]
    fn every_item_arrives_in_order_and_the_stream_then_ends() {
        let got: Vec<_> = futures::executor::block_on(
            stream(
                |emit| {
                    for i in 0..3 {
                        emit(i);
                    }
                },
                Duration::from_secs(5),
            )
            .collect(),
        );

        assert_eq!(
            got,
            vec![JobResult::Done(0), JobResult::Done(1), JobResult::Done(2)],
            "the stream must end when the work returns, with nothing after it"
        );
    }

    #[test]
    fn an_item_reaches_the_consumer_before_the_work_has_finished() {
        // The whole reason this exists next to `run`. A version that gathered
        // everything and handed it over at the end would satisfy the ordering
        // test above and still leave the drawer blank for the length of a
        // `uv sync`, so the assertion is on *when* the first item arrived.
        let started = std::time::Instant::now();
        let mut arrivals = Vec::new();

        futures::executor::block_on(
            stream(
                |emit| {
                    emit(());
                    std::thread::sleep(Duration::from_millis(300));
                    emit(());
                },
                Duration::from_secs(5),
            )
            .for_each(|_| {
                arrivals.push(started.elapsed());
                std::future::ready(())
            }),
        );

        assert_eq!(arrivals.len(), 2);
        assert!(
            arrivals[0] + Duration::from_millis(100) < arrivals[1],
            "both items arrived at once ({arrivals:?}); that is the buffered \
             behaviour `run` already had"
        );
    }

    #[test]
    fn work_that_goes_silent_times_out_rather_than_hanging_the_stream() {
        // The bound is per item, not on the whole job: the first item arrives
        // and is delivered, and only the silence after it is a timeout.
        let got: Vec<_> = futures::executor::block_on(
            stream(
                |emit| {
                    emit(1);
                    std::thread::sleep(Duration::from_secs(30));
                },
                Duration::from_millis(50),
            )
            .collect(),
        );

        assert_eq!(got, vec![JobResult::Done(1), JobResult::TimedOut]);
    }

    #[test]
    fn work_that_keeps_producing_is_not_timed_out() {
        // The other half of "per item, not per job": a long `uv sync` that is
        // still printing must not be declared dead. A total-budget timeout
        // would cut this off at 50ms.
        let got: Vec<_> = futures::executor::block_on(
            stream(
                |emit| {
                    for i in 0..5 {
                        std::thread::sleep(Duration::from_millis(30));
                        emit(i);
                    }
                },
                Duration::from_millis(200),
            )
            .collect(),
        );

        assert_eq!(got.len(), 5, "got {got:?}");
        assert!(
            got.iter().all(|item| matches!(item, JobResult::Done(_))),
            "a job that kept producing was cut off: {got:?}"
        );
    }

    #[test]
    fn work_that_panics_ends_the_stream_as_a_failure() {
        // Same contract as `run`: a panicking job is reported, never
        // propagated, and never leaves the stream open forever.
        let got: Vec<_> = futures::executor::block_on(
            stream(
                |emit| {
                    emit(1);
                    panic!("boom");
                },
                Duration::from_secs(5),
            )
            .collect(),
        );

        assert_eq!(got, vec![JobResult::Done(1), JobResult::Failed]);
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
