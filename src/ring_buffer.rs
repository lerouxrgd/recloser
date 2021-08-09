use std::sync::atomic::{
    AtomicBool, AtomicUsize,
    Ordering::{Acquire, Release, SeqCst},
};

use crossbeam::utils::Backoff;

/// Records successful and failed calls, calculates failure rate.
/// A `true` value in the ring represents a call that failed.
/// Therefore the failure rate is the ratio: card/len.
#[derive(Debug)]
pub struct RingBuffer {
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
            len,
            card: AtomicUsize::new(0),
            filling: AtomicUsize::new(0),
            ring: buf.into_boxed_slice(),
            index: AtomicUsize::new(0),
        }
    }

    pub fn set_current(&self, val_new: bool) -> f32 {
        let backoff = Backoff::new();
        while self
            .spin_lock
            .compare_exchange_weak(false, true, Acquire, Acquire)
            .is_err()
        {
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
    if b {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::*;

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
}
