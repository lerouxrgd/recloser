#[cfg(test)]
use fake_clock::FakeClock as Instant;
#[cfg(not(test))]
use std::time::Instant;

use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;

use crossbeam::epoch::{self, Atomic, Guard, Owned};

use crate::error::{AnyError, Error, ErrorPredicate};
use crate::ring_buffer::RingBuffer;

#[derive(Debug)]
pub struct Recloser {
    threshold: f32,
    closed_len: usize,
    half_open_len: usize,
    open_wait: Duration,
    state: Atomic<State>,
}

impl Recloser {
    pub fn custom() -> RecloserBuilder {
        RecloserBuilder::new()
    }

    pub fn call<F, T, E>(&self, f: F) -> Result<T, Error<E>>
    where
        F: FnOnce() -> Result<T, E>,
    {
        self.call_with(AnyError, f)
    }

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
        match unsafe { self.state.load(SeqCst, guard).deref() } {
            State::Closed(_) => true,
            State::HalfOpen(_) => true,
            State::Open(until) => {
                if Instant::now() > *until {
                    self.state.swap(
                        Owned::new(State::HalfOpen(RingBuffer::new(self.half_open_len))),
                        SeqCst,
                        guard,
                    );
                    true
                } else {
                    false
                }
            }
        }
    }

    pub(crate) fn on_success(&self, guard: &Guard) {
        match unsafe { self.state.load(SeqCst, guard).deref() } {
            State::Closed(rb) => {
                rb.set_current(false);
            }
            State::HalfOpen(rb) => {
                let failure_rate = rb.set_current(false);
                if failure_rate != -1.0 && failure_rate <= self.threshold {
                    self.state.swap(
                        Owned::new(State::Closed(RingBuffer::new(self.closed_len))),
                        SeqCst,
                        guard,
                    );
                }
            }
            State::Open(_) => (),
        };
    }

    pub(crate) fn on_error(&self, guard: &Guard) {
        match unsafe { self.state.load(SeqCst, guard).deref() } {
            State::Closed(rb) | State::HalfOpen(rb) => {
                let failure_rate = rb.set_current(true);
                if failure_rate != -1.0 && failure_rate >= self.threshold {
                    self.state.swap(
                        Owned::new(State::Open(Instant::now() + self.open_wait)),
                        SeqCst,
                        guard,
                    );
                }
            }
            State::Open(_) => (),
        };
    }
}

#[derive(Debug)]
enum State {
    Open(Instant),
    HalfOpen(RingBuffer),
    Closed(RingBuffer),
}

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
    use std::sync::{Arc, Barrier};
    use std::thread;

    use fake_clock::FakeClock;
    use matches::assert_matches;
    use rand::prelude::*;

    use super::*;

    fn sleep(time: u64) {
        FakeClock::advance_time(time);
    }

    #[test]
    fn multi_error_types() {
        let recl = Recloser::custom()
            .error_rate(0.5)
            .closed_len(2)
            .half_open_len(2)
            .open_wait(Duration::from_secs(1))
            .build();

        let _ = recl.call(|| Err::<(), ()>(()));
        let _ = recl.call(|| Err::<usize, usize>(12));
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
            assert_matches!(recl.call(|| Err::<(), ()>(())), Err(Error::Inner(())));
            assert_matches!(
                unsafe { &recl.state.load(SeqCst, guard).deref() },
                State::Closed(_)
            );
        }

        // Transition to State::Open on next call
        assert_matches!(recl.call(|| Err::<(), ()>(())), Err(Error::Inner(())));
        assert_matches!(
            unsafe { &recl.state.load(SeqCst, guard).deref() },
            State::Open(_)
        );
        assert_matches!(recl.call(|| Err::<(), ()>(())), Err(Error::Rejected));

        // Transition to State::HalfOpen on first call after 1 sec
        sleep(1500);
        assert_matches!(recl.call(|| Ok::<(), ()>(())), Ok(()));
        assert_matches!(
            unsafe { &recl.state.load(SeqCst, guard).deref() },
            State::HalfOpen(_)
        );

        // Fill the State::HalfOpen ring buffer
        assert_matches!(recl.call(|| Ok::<(), ()>(())), Ok(()));
        assert_matches!(
            unsafe { &recl.state.load(SeqCst, guard).deref() },
            State::HalfOpen(_)
        );

        // Transition to State::Closed when failure rate below threshold
        assert_matches!(recl.call(|| Ok::<(), ()>(())), Ok(()));
        assert_matches!(
            unsafe { &recl.state.load(SeqCst, guard).deref() },
            State::Closed(_)
        );
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
