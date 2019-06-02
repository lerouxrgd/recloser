use std::sync::atomic::{
    AtomicBool, AtomicUsize,
    Ordering::{Acquire, Release, SeqCst},
};
use std::time::{Duration, Instant};

use crossbeam::epoch::{self as epoch, Atomic, Guard, Owned};
use crossbeam::utils::Backoff;

#[derive(Debug)]
pub enum Error<E> {
    Inner(E),
    Rejected,
}

pub trait FailurePredicate<E> {
    fn is_failure(&self, err: &E) -> bool;
}

impl<F, E> FailurePredicate<E> for F
where
    F: Fn(&E) -> bool,
{
    fn is_failure(&self, err: &E) -> bool {
        self(err)
    }
}

pub struct Recloser<P, E>
where
    P: FailurePredicate<E>,
{
    predicate: P,
    len: usize,
    failure_rate_threshold: f32,
    wait_open: Duration,
    state: Atomic<State>,
    marker: std::marker::PhantomData<E>,
}

impl<P, E> Recloser<P, E>
where
    P: FailurePredicate<E>,
{
    pub fn new(len: usize, failure_rate_threshold: f32, wait_open: Duration, predicate: P) -> Self {
        Recloser {
            predicate,
            len,
            failure_rate_threshold,
            wait_open,
            state: Atomic::new(State::Closed(RingBuffer::new(len))),
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
                if self.predicate.is_failure(&err) {
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
                        Owned::new(State::HalfOpen(RingBuffer::new(self.len))),
                        SeqCst,
                        &guard,
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
                if rb.set_current(false) < self.failure_rate_threshold {
                    self.state.swap(
                        Owned::new(State::Closed(RingBuffer::new(self.len))),
                        SeqCst,
                        &guard,
                    );
                }
            }
            State::Open(_) => unreachable!(),
        };
    }

    fn on_error(&self, guard: &Guard) {
        match unsafe { self.state.load(SeqCst, guard).deref() } {
            State::Closed(rb) | State::HalfOpen(rb) => {
                if rb.set_current(true) > self.failure_rate_threshold {
                    self.state.swap(
                        Owned::new(State::Open(Instant::now() + self.wait_open)),
                        SeqCst,
                        &guard,
                    );
                }
            }
            State::Open(_) => unreachable!(),
        };
    }
}

enum State {
    Open(Instant),
    HalfOpen(RingBuffer),
    Closed(RingBuffer),
}

/// A `true` value represents a call that failed
#[derive(Debug)]
struct RingBuffer {
    spinlock: AtomicBool,
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
            spinlock: AtomicBool::new(false),
            len: len,
            card: AtomicUsize::new(0),
            filling: AtomicUsize::new(0),
            ring: buf.into_boxed_slice(),
            index: AtomicUsize::new(0),
        }
    }

    pub fn set_current(&self, val_new: bool) -> f32 {
        let backoff = Backoff::new();
        while self.spinlock.compare_and_swap(false, true, Acquire) {
            backoff.snooze();
        }

        let i = self.index.load(SeqCst);
        let j = if i == self.len - 1 { 0 } else { i + 1 };

        let val_old = self.ring[i].load(SeqCst);

        let card_old = self.card.load(SeqCst);
        let card_new = card_old - to_int(val_old) + to_int(val_new);

        let failure_rate = if self.filling.load(SeqCst) == self.len {
            card_new as f32 / self.len as f32
        } else {
            self.filling.fetch_add(1, SeqCst);
            -1.0
        };

        self.ring[i].store(val_new, SeqCst);
        self.index.store(j, SeqCst);
        self.card.store(card_new, SeqCst);

        self.spinlock.store(false, Release);
        failure_rate
    }
}

fn to_int(b: bool) -> usize {
    match b {
        true => 1,
        false => 0,
    }
}

pub struct AnyError;

impl<E> FailurePredicate<E> for AnyError {
    fn is_failure(&self, _err: &E) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn bit_ring_buffer_correctness() {
        let rb = Arc::new(RingBuffer::new(7));

        let mut handles = Vec::with_capacity(5);
        let barrier = Arc::new(Barrier::new(5));

        for _ in 0..5 {
            let c = barrier.clone();
            let brb = rb.clone();
            handles.push(thread::spawn(move || {
                c.wait();
                for _ in 0..5 {
                    brb.set_current(true);
                    brb.set_current(false);
                    brb.set_current(true);
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
        assert_eq!((5 * 5 * 3) % 7, rb.index.load(SeqCst));
    }
}
