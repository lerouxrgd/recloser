use std::sync::atomic::{
    AtomicBool, AtomicUsize,
    Ordering::{Acquire, Relaxed, Release},
};

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
        while self.spin_lock.swap(true, Acquire) {
            std::hint::spin_loop();
        }

        let i = self.index.load(Relaxed);
        let j = if i == self.len - 1 { 0 } else { i + 1 };

        let val_old = self.ring[i].load(Relaxed);

        let card_old = self.card.load(Relaxed);
        let card_new = card_old - to_int(val_old) + to_int(val_new);

        let rate = if self.filling.load(Relaxed) == self.len {
            card_new as f32 / self.len as f32
        } else {
            self.filling.fetch_add(1, Relaxed);
            -1.0
        };

        self.ring[i].store(val_new, Relaxed);
        self.index.store(j, Relaxed);
        self.card.store(card_new, Relaxed);

        self.spin_lock.store(false, Release);
        rate
    }
}

#[inline(always)]
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
        let num_threads = 8;
        let rb_len = 7;
        let loop_len = 1000;

        let rb = Arc::new(RingBuffer::new(rb_len));

        let mut handles = Vec::with_capacity(num_threads);
        let barrier = Arc::new(Barrier::new(num_threads));

        for _ in 0..num_threads {
            let c = barrier.clone();
            let rb = rb.clone();
            handles.push(thread::spawn(move || {
                c.wait();
                for _ in 0..loop_len {
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
            rb.card.load(Relaxed),
            rb.ring
                .iter()
                .map(|b| to_int(b.load(Relaxed)))
                .fold(0, |acc, i| acc + i)
        );
        assert_eq!(
            (num_threads * loop_len * 3) % rb_len,
            rb.index.load(Relaxed)
        );
    }
}
