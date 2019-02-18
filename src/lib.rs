use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crossbeam_utils::Backoff;

pub struct Recloser {
    state: RecloserState,
}

impl Recloser {
    pub fn new(len: usize) -> Self {
        Recloser {
            state: RecloserState::Closed(RingBitBuffer::new(len)),
        }
    }
}

enum RecloserState {
    Open(RingBitBuffer),
    HalfOpen(RingBitBuffer),
    Closed(RingBitBuffer),
}

impl RecloserState {
    pub fn call_permitted(&self) -> bool {
        match *self {
            RecloserState::Closed(_) => true,
            _ => true,
        }
    }
}

#[derive(Debug)]
struct RingBitBuffer {
    spinlock: AtomicBool,
    len: usize,
    card: AtomicUsize,
    ring: Box<[AtomicBool]>,
    index: AtomicUsize,
}

impl RingBitBuffer {
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
    fn it_works() {
        let rbs = Arc::new(RingBitBuffer::new(7));

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
