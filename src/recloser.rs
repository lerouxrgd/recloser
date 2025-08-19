#[cfg(test)]
use fake_clock::FakeClock as Instant;
#[cfg(feature = "tracing")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
#[cfg(not(test))]
use std::time::Instant;
#[cfg(feature = "tracing")]
use std::time::{SystemTime, UNIX_EPOCH};

use crossbeam_epoch::{self as epoch, Atomic, Guard, Owned};

use crate::error::{AnyError, Error, ErrorPredicate};
use crate::ring_buffer::RingBuffer;

pub const RECLOSER_EVENT: &str = "recloser_event";

/// A concurrent cirbuit breaker based on `RingBuffer`s that allows or rejects
/// calls depending on the state it is in.
#[derive(Debug)]
pub struct Recloser {
    threshold_closed: f32,
    threshold_half_open: f32,
    closed_len: usize,
    half_open_len: usize,
    open_wait: Duration,
    state: Atomic<State>,
    #[cfg(feature = "tracing")]
    state_started_ts: AtomicU64,
}

impl Recloser {
    /// Returns a builder to create a customized [`Recloser`].
    pub fn custom() -> RecloserBuilder {
        RecloserBuilder::new()
    }

    /// Wraps a function that may fail, records the result as success or failure. Uses
    /// default [`AnyError`] predicate that considers any [`Err(_)`](Result::Err) as a
    /// failure. Based on the result, state transition may happen.
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
        let shared = self.state.load(Ordering::Acquire, guard);
        // Safety: safe because `Shared::null()` is never used.
        match unsafe { shared.deref() } {
            State::Closed(_) => true,
            State::HalfOpen(_) => true,
            _old_state @ State::Open(until) => {
                if Instant::now() > *until {
                    let new_state = State::HalfOpen(RingBuffer::new(self.half_open_len));
                    #[cfg(feature = "tracing")]
                    let new_state_name = new_state.name();

                    let _swapped = self.state.compare_exchange(
                        shared,
                        Owned::new(new_state),
                        Ordering::Release,
                        Ordering::Relaxed,
                        guard,
                    );

                    #[cfg(feature = "tracing")]
                    if _swapped.is_ok() {
                        let swap_ts = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let old_state_ts = self.state_started_ts.swap(swap_ts, Ordering::Relaxed);
                        tracing::event!(
                            target: RECLOSER_EVENT,
                            tracing::Level::INFO,
                            state = _old_state.name(),
                            ended_ts = swap_ts,
                            duration_sec = swap_ts - old_state_ts
                        );
                        tracing::event!(
                            target: RECLOSER_EVENT,
                            tracing::Level::INFO,
                            state = new_state_name,
                            started_ts = swap_ts
                        );
                    }

                    true
                } else {
                    false
                }
            }
        }
    }

    pub(crate) fn on_success(&self, guard: &Guard) {
        let shared = self.state.load(Ordering::Acquire, guard);
        // Safety: safe because `Shared::null()` is never used.
        match unsafe { shared.deref() } {
            State::Closed(rb) => {
                rb.set_current(false);
            }
            _old_state @ State::HalfOpen(rb) => {
                let failure_rate = rb.set_current(false);
                if failure_rate > -1.0 && failure_rate <= self.threshold_half_open {
                    let new_state = State::Closed(RingBuffer::new(self.closed_len));
                    #[cfg(feature = "tracing")]
                    let new_state_name = new_state.name();

                    let _swapped = self.state.compare_exchange(
                        shared,
                        Owned::new(new_state),
                        Ordering::Release,
                        Ordering::Relaxed,
                        guard,
                    );

                    #[cfg(feature = "tracing")]
                    if _swapped.is_ok() {
                        let swap_ts = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let old_state_ts = self.state_started_ts.swap(swap_ts, Ordering::Relaxed);
                        tracing::event!(
                            target: RECLOSER_EVENT,
                            tracing::Level::INFO,
                            state = _old_state.name(),
                            ended_ts = swap_ts,
                            duration_sec = swap_ts - old_state_ts
                        );
                        tracing::event!(
                            target: RECLOSER_EVENT,
                            tracing::Level::INFO,
                            state = new_state_name,
                            started_ts = swap_ts
                        );
                    }
                }
            }
            State::Open(_) => (),
        };
    }

    pub(crate) fn on_error(&self, guard: &Guard) {
        let shared = self.state.load(Ordering::Acquire, guard);
        // Safety: safe because `Shared::null()` is never used.
        match unsafe { shared.deref() } {
            _old_state @ State::Closed(rb) => {
                let failure_rate = rb.set_current(true);
                if failure_rate > -1.0 && failure_rate >= self.threshold_closed {
                    let new_state = State::Open(Instant::now() + self.open_wait);
                    #[cfg(feature = "tracing")]
                    let new_state_name = new_state.name();

                    let _swapped = self.state.compare_exchange(
                        shared,
                        Owned::new(new_state),
                        Ordering::Release,
                        Ordering::Relaxed,
                        guard,
                    );

                    #[cfg(feature = "tracing")]
                    if _swapped.is_ok() {
                        let swap_ts = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let old_state_ts = self.state_started_ts.swap(swap_ts, Ordering::Relaxed);
                        tracing::event!(
                            target: RECLOSER_EVENT,
                            tracing::Level::INFO,
                            state = _old_state.name(),
                            ended_ts = swap_ts,
                            duration_sec = swap_ts - old_state_ts
                        );
                        tracing::event!(
                            target: RECLOSER_EVENT,
                            tracing::Level::INFO,
                            state = new_state_name,
                            started_ts = swap_ts
                        );
                    }
                }
            }
            _old_state @ State::HalfOpen(rb) => {
                let failure_rate = rb.set_current(true);
                if failure_rate > -1.0 && failure_rate >= self.threshold_half_open {
                    let new_state = State::Open(Instant::now() + self.open_wait);
                    #[cfg(feature = "tracing")]
                    let new_state_name = new_state.name();

                    let _swapped = self.state.compare_exchange(
                        shared,
                        Owned::new(new_state),
                        Ordering::Release,
                        Ordering::Relaxed,
                        guard,
                    );

                    #[cfg(feature = "tracing")]
                    if _swapped.is_ok() {
                        let swap_ts = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let old_state_ts = self.state_started_ts.swap(swap_ts, Ordering::Relaxed);
                        tracing::event!(
                            target: RECLOSER_EVENT,
                            tracing::Level::INFO,
                            state = _old_state.name(),
                            ended_ts = swap_ts,
                            duration_sec = swap_ts - old_state_ts
                        );
                        tracing::event!(
                            target: RECLOSER_EVENT,
                            tracing::Level::INFO,
                            state = new_state_name,
                            started_ts = swap_ts
                        );
                    }
                }
            }
            State::Open(_) => (),
        };
    }
}

/// The states a [`Recloser`] can be in.
#[derive(Debug)]
enum State {
    /// Allows calls until a failure_rate threshold is reached.
    Closed(RingBuffer),
    /// Rejects all calls until the future [`Instant`] is reached.
    Open(Instant),
    /// Allows calls until the underlying [`RingBuffer`] is full,
    /// then calculates a failure_rate based on which the next transition will happen.
    HalfOpen(RingBuffer),
}

#[cfg(feature = "tracing")]
impl State {
    fn name(&self) -> &'static str {
        match self {
            State::Closed(_) => "Closed",
            State::Open(_) => "Open",
            State::HalfOpen(_) => "HalfOpen",
        }
    }
}

/// A helper struct to build customized [`Recloser`].
#[derive(Debug)]
pub struct RecloserBuilder {
    threshold_closed: f32,
    threshold_half_open: f32,
    closed_len: usize,
    half_open_len: usize,
    open_wait: Duration,
}

impl RecloserBuilder {
    fn new() -> Self {
        RecloserBuilder {
            threshold_closed: 0.5,
            threshold_half_open: 0.5,
            closed_len: 100,
            half_open_len: 10,
            open_wait: Duration::from_secs(30),
        }
    }

    pub fn error_rate(mut self, threshold: f32) -> Self {
        self.threshold_closed = threshold;
        self.threshold_half_open = threshold;
        self
    }

    pub fn error_rate_closed(mut self, threshold: f32) -> Self {
        self.threshold_closed = threshold;
        self
    }

    pub fn error_rate_half_open(mut self, threshold: f32) -> Self {
        self.threshold_half_open = threshold;
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
        let state = State::Closed(RingBuffer::new(self.closed_len));

        #[cfg(feature = "tracing")]
        let state_started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        #[cfg(feature = "tracing")]
        tracing::event!(
            target: RECLOSER_EVENT,
            tracing::Level::INFO,
            state = state.name(),
            started_ts = state_started
        );

        Recloser {
            threshold_closed: self.threshold_closed,
            threshold_half_open: self.threshold_half_open,
            closed_len: self.closed_len,
            half_open_len: self.half_open_len,
            open_wait: self.open_wait,
            state: Atomic::new(state),
            #[cfg(feature = "tracing")]
            state_started_ts: AtomicU64::new(state_started),
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
        assert!(recl.call_permitted(guard));

        let f = || Err::<(), usize>(12);
        assert!(matches!(recl.call(f), Err(Error::Inner(12))));
        assert!(!recl.call_permitted(guard));
    }

    #[test]
    fn error_predicate() {
        let recl = Recloser::custom().closed_len(1).build();
        let guard = &epoch::pin();

        let f = || Err::<(), ()>(());
        let p = |_: &()| false;

        assert!(matches!(recl.call_with(p, f), Err(Error::Inner(()))));
        assert!(recl.call_permitted(guard));

        assert!(matches!(recl.call_with(p, f), Err(Error::Inner(()))));
        assert!(recl.call_permitted(guard));
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
                unsafe { recl.state.load(Relaxed, guard).deref() },
                State::Closed(_)
            ));
        }

        // Transition to State::Open on next call
        assert!(matches!(
            recl.call(|| Err::<(), ()>(())),
            Err(Error::Inner(()))
        ));
        assert!(matches!(
            unsafe { recl.state.load(Relaxed, guard).deref() },
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
            unsafe { recl.state.load(Relaxed, guard).deref() },
            State::HalfOpen(_)
        ));

        // Fill the State::HalfOpen ring buffer
        assert!(matches!(recl.call(|| Ok::<(), ()>(())), Ok(())));
        assert!(matches!(
            unsafe { recl.state.load(Relaxed, guard).deref() },
            State::HalfOpen(_)
        ));

        // Transition to State::Closed when failure rate below threshold
        assert!(matches!(recl.call(|| Ok::<(), ()>(())), Ok(())));
        assert!(matches!(
            unsafe { recl.state.load(Relaxed, guard).deref() },
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
                let mut rng = rand::rng();
                c.wait();
                for _ in 0..1000 {
                    let _ = recl.call(|| Ok::<(), ()>(()));
                    let _ = recl.call(|| Err::<(), ()>(()));
                    if rng.random::<f64>() < 0.5 {
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
