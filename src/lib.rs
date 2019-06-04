#[cfg(test)]
use fake_clock::FakeClock as Instant;
#[cfg(not(test))]
use std::time::Instant;

use std::sync::atomic::{
    AtomicBool, AtomicUsize,
    Ordering::{Acquire, Release, SeqCst},
};
use std::time::Duration;

use crossbeam::epoch::{self as epoch, Atomic, Guard, Owned};
use crossbeam::utils::Backoff;

#[derive(Debug)]
pub enum Error<E> {
    Inner(E),
    Rejected,
}

pub trait ErrorPredicate<E> {
    fn is_err(&self, err: &E) -> bool;
}

impl<F, E> ErrorPredicate<E> for F
where
    F: Fn(&E) -> bool,
{
    fn is_err(&self, err: &E) -> bool {
        self(err)
    }
}

#[derive(Debug)]
pub struct Recloser<P, E>
where
    P: ErrorPredicate<E>,
{
    predicate: P,
    threshold: f32,
    closed_len: usize,
    half_open_len: usize,
    open_wait: Duration,
    state: Atomic<State>,
    marker: std::marker::PhantomData<E>,
}

impl<P, E> Recloser<P, E>
where
    P: ErrorPredicate<E>,
{
    pub fn new(
        predicate: P,
        threshold: f32,
        closed_len: usize,
        half_open_len: usize,
        open_wait: Duration,
    ) -> Self {
        Recloser {
            predicate,
            threshold,
            closed_len,
            half_open_len,
            open_wait,
            state: Atomic::new(State::Closed(RingBuffer::new(closed_len))),
            marker: std::marker::PhantomData,
        }
    }

    pub fn call<F, R>(&self, f: F) -> Result<R, Error<E>>
    where
        F: FnOnce() -> Result<R, E>,
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
                if self.predicate.is_err(&err) {
                    self.on_error(guard);
                } else {
                    self.on_success(guard);
                }
                Err(Error::Inner(err))
            }
        }
    }

    fn call_permitted(&self, guard: &Guard) -> bool {
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

    fn on_success(&self, guard: &Guard) {
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

    fn on_error(&self, guard: &Guard) {
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

/// A `true` value represents a call that failed
#[derive(Debug)]
struct RingBuffer {
    spin_lock: AtomicBool,
    len: usize,
    card: AtomicUsize,
    filling: AtomicUsize,
    ring: Box<[AtomicBool]>,
    index: AtomicUsize,
}

impl RingBuffer {
    pub fn new(len: usize) -> Self {
        let mut buf = Vec::with_capacity(len);

        for _ in 0..len {
            buf.push(AtomicBool::new(false));
        }

        RingBuffer {
            spin_lock: AtomicBool::new(false),
            len: len,
            card: AtomicUsize::new(0),
            filling: AtomicUsize::new(0),
            ring: buf.into_boxed_slice(),
            index: AtomicUsize::new(0),
        }
    }

    pub fn set_current(&self, val_new: bool) -> f32 {
        let backoff = Backoff::new();
        while self.spin_lock.compare_and_swap(false, true, Acquire) {
            backoff.snooze();
        }

        let i = self.index.load(SeqCst);
        let j = if i == self.len - 1 { 0 } else { i + 1 };

        let val_old = self.ring[i].load(SeqCst);

        let card_old = self.card.load(SeqCst);
        let card_new = card_old - to_int(val_old) + to_int(val_new);

        let rate = if self.filling.load(SeqCst) == self.len {
            card_new as f32 / self.len as f32
        } else {
            self.filling.fetch_add(1, SeqCst);
            -1.0
        };

        self.ring[i].store(val_new, SeqCst);
        self.index.store(j, SeqCst);
        self.card.store(card_new, SeqCst);

        self.spin_lock.store(false, Release);
        rate
    }
}

fn to_int(b: bool) -> usize {
    match b {
        true => 1,
        false => 0,
    }
}

#[derive(Debug)]
pub struct AnyError;

impl<E> ErrorPredicate<E> for AnyError {
    fn is_err(&self, _err: &E) -> bool {
        true
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
    fn ring_buffer_correctness() {
        let rb = Arc::new(RingBuffer::new(7));

        let mut handles = Vec::with_capacity(8);
        let barrier = Arc::new(Barrier::new(8));

        for _ in 0..8 {
            let c = barrier.clone();
            let rb = rb.clone();
            handles.push(thread::spawn(move || {
                c.wait();
                for _ in 0..100 {
                    rb.set_current(true);
                    rb.set_current(false);
                    rb.set_current(true);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(
            rb.card.load(SeqCst),
            rb.ring
                .iter()
                .map(|b| to_int(b.load(SeqCst)))
                .fold(0, |acc, i| acc + i)
        );
        assert_eq!((8 * 100 * 3) % 7, rb.index.load(SeqCst));
    }

    #[test]
    fn recloser_correctness() {
        let recl = Recloser::new(AnyError, 0.5, 2, 2, Duration::from_secs(1));
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
    fn recloser_shared() {
        let recl = Arc::new(Recloser::new(AnyError, 0.5, 10, 5, Duration::from_secs(1)));

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
