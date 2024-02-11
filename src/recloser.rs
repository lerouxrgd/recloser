#[cfg(test)]
use fake_clock::FakeClock as Instant;
#[cfg(not(test))]
use std::time::Instant;

use std::sync::atomic::Ordering::{Acquire, Release};
use std::time::Duration;

use crossbeam_epoch::{self as epoch, Atomic, Guard, Owned};

use crate::error::{AnyError, Error, ErrorPredicate};
use crate::ring_buffer::RingBuffer;

/// A concurrent cirbuit breaker based on `RingBuffer`s that allows or rejects
/// calls depending on the state it is in.
#[derive(Debug)]
pub struct Recloser {
    threshold: f32,
    closed_len: usize,
    half_open_len: usize,
    open_wait: Duration,
    state: Atomic<State>,
}

impl Recloser {
    /// Returns a builder to create a customized `Recloser`.
    pub fn custom() -> RecloserBuilder {
        RecloserBuilder::new()
    }

    /// Wraps a function that may fail, records the result as success or failure.
    /// Uses default `AnyError` predicate that considers any `Err(_)` as a failure.
    /// Based on the result, state transition may happen.
    pub fn call<F, T, E>(&self, f: F) -> Result<T, Error<E>>
    where
        F: FnOnce() -> Result<T, E>,
    {
        self.call_with(AnyError, f)
    }

    /// Wraps a function that may fail, the custom `predicate` will be used to
    /// determine whether the result was a success or failure.
    /// Based on the result, state transition may happen.
    pub fn call_with<P, F, T, E>(&self, predicate: P, f: F) -> Result<T, Error<E>>
    where
        P: ErrorPredicate<E>,
        F: FnOnce() -> Result<T, E>,
    {
        let guard = &epoch::pin();

        if !self.call_permitted(guard) {
            return Err(Error::Rejected);
        }

        match f() {
            Ok(ok) => {
                self.on_success(guard);
                Ok(ok)
            }
            Err(err) => {
                if predicate.is_err(&err) {
                    self.on_error(guard);
                } else {
                    self.on_success(guard);
                }
                Err(Error::Inner(err))
            }
        }
    }

    pub(crate) fn call_permitted(&self, guard: &Guard) -> bool {
        // Safety: safe because `Shared::null()` is never used.
        match unsafe { self.state.load(Acquire, guard).deref() } {
            State::Closed(_) => true,
            State::HalfOpen(_) => true,
            State::Open(until) => {
                if Instant::now() > *until {
                    self.state.store(
                        Owned::new(State::HalfOpen(RingBuffer::new(self.half_open_len))),
                        Release,
                    );
                    true
                } else {
                    false
                }
            }
        }
    }

    pub(crate) fn on_success(&self, guard: &Guard) {
        // Safety: safe because `Shared::null()` is never used.
        match unsafe { self.state.load(Acquire, guard).deref() } {
            State::Closed(rb) => {
                rb.set_current(false);
            }
            State::HalfOpen(rb) => {
                let failure_rate = rb.set_current(false);
                if failure_rate > -1.0 && failure_rate <= self.threshold {
                    self.state.store(
                        Owned::new(State::Closed(RingBuffer::new(self.closed_len))),
                        Release,
                    );
                }
            }
            State::Open(_) => (),
        };
    }

    pub(crate) fn on_error(&self, guard: &Guard) {
        // Safety: safe because `Shared::null()` is never used.
        match unsafe { self.state.load(Acquire, guard).deref() } {
            State::Closed(rb) | State::HalfOpen(rb) => {
                let failure_rate = rb.set_current(true);
                if failure_rate > -1.0 && failure_rate >= self.threshold {
                    self.state.store(
                        Owned::new(State::Open(Instant::now() + self.open_wait)),
                        Release,
                    );
                }
            }
            State::Open(_) => (),
        };
    }
}

/// The states a `Recloser` can be in.
#[derive(Debug)]
enum State {
    /// Allows calls until a failure_rate threshold is reached.
    Closed(RingBuffer),
    /// Rejects all calls until the future `Instant` is reached.
    Open(Instant),
    /// Allows calls until the underlying `RingBuffer` is full,
    /// then calculates a failure_rate based on which the next transition will happen.
    HalfOpen(RingBuffer),
}

/// A helper struct to build customized `Recloser`.
#[derive(Debug)]
pub struct RecloserBuilder {
    threshold: f32,
    closed_len: usize,
    half_open_len: usize,
    open_wait: Duration,
}

impl RecloserBuilder {
    fn new() -> Self {
        RecloserBuilder {
            threshold: 0.5,
            closed_len: 100,
            half_open_len: 10,
            open_wait: Duration::from_secs(30),
        }
    }

    pub fn error_rate(mut self, threshold: f32) -> Self {
        self.threshold = threshold;
        self
    }

    pub fn closed_len(mut self, closed_len: usize) -> Self {
        self.closed_len = closed_len;
        self
    }

    pub fn half_open_len(mut self, half_open_len: usize) -> Self {
        self.half_open_len = half_open_len;
        self
    }

    pub fn open_wait(mut self, open_wait: Duration) -> Self {
        self.open_wait = open_wait;
        self
    }

    pub fn build(self) -> Recloser {
        Recloser {
            threshold: self.threshold,
            closed_len: self.closed_len,
            half_open_len: self.half_open_len,
            open_wait: self.open_wait,
            state: Atomic::new(State::Closed(RingBuffer::new(self.closed_len))),
        }
    }
}

impl Default for Recloser {
    fn default() -> Self {
        Recloser::custom().build()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use fake_clock::FakeClock;
    use rand::prelude::*;

    use super::*;

    fn sleep(time: u64) {
        FakeClock::advance_time(time);
    }

    #[test]
    fn multi_errors() {
        let recl = Recloser::custom().closed_len(1).build();
        let guard = &epoch::pin();

        let f = || Err::<(), ()>(());
        assert!(matches!(recl.call(f), Err(Error::Inner(()))));
        assert_eq!(true, recl.call_permitted(guard));

        let f = || Err::<(), usize>(12);
        assert!(matches!(recl.call(f), Err(Error::Inner(12))));
        assert_eq!(false, recl.call_permitted(guard));
    }

    #[test]
    fn error_predicate() {
        let recl = Recloser::custom().closed_len(1).build();
        let guard = &epoch::pin();

        let f = || Err::<(), ()>(());
        let p = |_: &()| false;

        assert!(matches!(recl.call_with(p, f), Err(Error::Inner(()))));
        assert_eq!(true, recl.call_permitted(guard));

        assert!(matches!(recl.call_with(p, f), Err(Error::Inner(()))));
        assert_eq!(true, recl.call_permitted(guard));
    }

    #[test]
    fn recloser_correctness() {
        let recl = Recloser::custom()
            .error_rate(0.5)
            .closed_len(2)
            .half_open_len(2)
            .open_wait(Duration::from_secs(1))
            .build();

        let guard = &epoch::pin();

        // Fill the State::Closed ring buffer
        for _ in 0..2 {
            assert!(matches!(
                recl.call(|| Err::<(), ()>(())),
                Err(Error::Inner(()))
            ));
            assert!(matches!(
                unsafe { &recl.state.load(Relaxed, guard).deref() },
                State::Closed(_)
            ));
        }

        // Transition to State::Open on next call
        assert!(matches!(
            recl.call(|| Err::<(), ()>(())),
            Err(Error::Inner(()))
        ));
        assert!(matches!(
            unsafe { &recl.state.load(Relaxed, guard).deref() },
            State::Open(_)
        ));
        assert!(matches!(
            recl.call(|| Err::<(), ()>(())),
            Err(Error::Rejected)
        ));

        // Transition to State::HalfOpen on first call after 1 sec
        sleep(1500);
        assert!(matches!(recl.call(|| Ok::<(), ()>(())), Ok(())));
        assert!(matches!(
            unsafe { &recl.state.load(Relaxed, guard).deref() },
            State::HalfOpen(_)
        ));

        // Fill the State::HalfOpen ring buffer
        assert!(matches!(recl.call(|| Ok::<(), ()>(())), Ok(())));
        assert!(matches!(
            unsafe { &recl.state.load(Relaxed, guard).deref() },
            State::HalfOpen(_)
        ));

        // Transition to State::Closed when failure rate below threshold
        assert!(matches!(recl.call(|| Ok::<(), ()>(())), Ok(())));
        assert!(matches!(
            unsafe { &recl.state.load(Relaxed, guard).deref() },
            State::Closed(_)
        ));
    }

    #[test]
    fn recloser_concurrent() {
        let recl = Arc::new(
            Recloser::custom()
                .error_rate(0.5)
                .closed_len(10)
                .half_open_len(5)
                .open_wait(Duration::from_secs(1))
                .build(),
        );

        let mut handles = Vec::with_capacity(8);
        let barrier = Arc::new(Barrier::new(8));

        for _ in 0..8 {
            let c = barrier.clone();
            let recl = recl.clone();
            handles.push(thread::spawn(move || {
                let mut rng = rand::thread_rng();
                c.wait();
                for _ in 0..1000 {
                    let _ = recl.call(|| Ok::<(), ()>(()));
                    let _ = recl.call(|| Err::<(), ()>(()));
                    if rng.gen::<f64>() < 0.5 {
                        let _ = recl.call(|| Err::<(), ()>(()));
                    } else {
                        let _ = recl.call(|| Ok::<(), ()>(()));
                    }
                    sleep(1500);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }
}
