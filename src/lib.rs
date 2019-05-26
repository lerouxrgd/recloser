use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use crossbeam_utils::Backoff;

#[derive(Debug)]
pub enum Error<E> {
    Inner(E),
    Rejected,
}

pub trait FailurePredicate<E> {
    fn is_failure(&self, err: &E) -> bool;
}

pub struct Recloser<P, E>
where
    P: FailurePredicate<E>,
{
    predicate: P,
    state: RecloserState,
    marker: std::marker::PhantomData<E>,
}

impl<P, E> Recloser<P, E>
where
    P: FailurePredicate<E>,
{
    pub fn new(len: usize, predicate: P) -> Self {
        Recloser {
            predicate,
            state: RecloserState::Closed(BitRingBuffer::new(len)),
            marker: std::marker::PhantomData,
        }
    }

    pub fn call<F, R>(&self, f: F) -> Result<R, Error<E>>
    where
        F: FnOnce() -> Result<R, E>,
    {
        if !self.call_permitted() {
            return Err(Error::Rejected);
        }

        match f() {
            Ok(ok) => {
                self.on_success();
                Ok(ok)
            }
            Err(err) => {
                if self.predicate.is_failure(&err) {
                    self.on_error();
                } else {
                    self.on_success();
                }
                Err(Error::Inner(err))
            }
        }
    }

    fn call_permitted(&self) -> bool {
        match self.state {
            RecloserState::Closed(_) => true,
            RecloserState::Open(until) => {
                if Instant::now() > until {
                    // transit_to_half_open();
                    true
                } else {
                    false
                }
            }
            _ => true,
        }
    }

    fn on_success(&self) {}

    fn on_error(&self) {}
}

impl<F, E> FailurePredicate<E> for F
where
    F: Fn(&E) -> bool,
{
    fn is_failure(&self, err: &E) -> bool {
        self(err)
    }
}

pub struct AnyError;

impl<E> FailurePredicate<E> for AnyError {
    fn is_failure(&self, _err: &E) -> bool {
        true
    }
}

enum RecloserState {
    Open(Instant),
    HalfOpen(BitRingBuffer),
    Closed(BitRingBuffer),
}

impl RecloserState {}

#[derive(Debug)]
struct BitRingBuffer {
    spinlock: AtomicBool,
    len: usize,
    card: AtomicUsize,
    ring: Box<[AtomicBool]>,
    index: AtomicUsize,
}

impl BitRingBuffer {
    pub fn new(len: usize) -> Self {
        let mut buf = Vec::with_capacity(len);

        for _ in 0..len {
            buf.push(AtomicBool::new(false));
        }

        Self {
            spinlock: AtomicBool::new(false),
            len: len,
            card: AtomicUsize::new(0),
            ring: buf.into_boxed_slice(),
            index: AtomicUsize::new(0),
        }
    }

    pub fn set_current(&self, val_new: bool) -> bool {
        let backoff = Backoff::new();
        while self
            .spinlock
            .compare_and_swap(false, true, Ordering::Acquire)
        {
            backoff.snooze();
        }

        let i = self.index.load(Ordering::SeqCst);
        let j = if i == self.len - 1 { 0 } else { i + 1 };

        let val_old = self.ring[i].load(Ordering::SeqCst);

        let card_old = self.card.load(Ordering::SeqCst);
        let card_new = card_old - to_int(val_old) + to_int(val_new);

        self.ring[i].store(val_new, Ordering::SeqCst);
        self.index.store(j, Ordering::SeqCst);
        self.card.store(card_new, Ordering::SeqCst);

        self.spinlock.store(false, Ordering::Release);
        return val_old;
    }

    pub fn failure_rate(&self) -> f32 {
        let backoff = Backoff::new();
        while self
            .spinlock
            .compare_and_swap(false, true, Ordering::Acquire)
        {
            backoff.snooze();
        }
        self.card.load(Ordering::SeqCst) as f32 / self.len as f32
    }
}

fn to_int(b: bool) -> usize {
    match b {
        true => 1,
        false => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn bit_ring_buffer_correctness() {
        let rbs = Arc::new(BitRingBuffer::new(7));

        let mut handles = Vec::with_capacity(5);
        let barrier = Arc::new(Barrier::new(5));

        for _ in 0..5 {
            let c = barrier.clone();
            let rbs = rbs.clone();
            handles.push(thread::spawn(move || {
                c.wait();
                for _ in 0..5 {
                    rbs.set_current(true);
                    rbs.set_current(false);
                    rbs.set_current(true);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(
            rbs.card.load(Ordering::SeqCst),
            rbs.ring
                .iter()
                .map(|b| to_int(b.load(Ordering::SeqCst)))
                .fold(0, |acc, i| acc + i)
        );
        assert_eq!((5 * 5 * 3) % 7, rbs.index.load(Ordering::SeqCst));
    }
}
