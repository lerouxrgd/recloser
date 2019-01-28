use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

enum RecloserState {
    Open(RingBitSet),
    HalfOpen(RingBitSet),
    Closed(RingBitSet),
}

impl RecloserState {
    pub fn call_permitted(&self) -> bool {
        match *self {
            RecloserState::Closed(_) => true,
            _ => true,
        }
    }
}

#[derive(Debug, Clone)]
struct RingBitSet {
    spinlock: Arc<AtomicBool>,
    len: usize,
    card: Arc<AtomicUsize>,
    ring: Arc<Box<[AtomicBool]>>,
    index: Arc<AtomicUsize>,
}

impl RingBitSet {
    pub fn new(len: usize) -> Self {
        let mut buf = Vec::with_capacity(len);

        for _ in 0..len {
            buf.push(AtomicBool::new(false));
        }

        Self {
            spinlock: Arc::new(AtomicBool::new(false)),
            len: len,
            card: Arc::new(AtomicUsize::new(0)),
            ring: Arc::new(buf.into_boxed_slice()),
            index: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn set_current(&mut self, val_new: bool) -> bool {
        while self
            .spinlock
            .compare_and_swap(false, true, Ordering::Acquire)
        {}

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
    use std::thread;

    #[test]
    fn it_works() {
        let mut rbs = RingBitSet::new(5);

        let mut rbs_ref = rbs.clone();
        let t = thread::spawn(move || {
            rbs_ref.set_current(true);
            rbs_ref.set_current(true);
            rbs_ref.set_current(false);
            rbs_ref.set_current(false);
        });

        rbs.set_current(true);
        rbs.set_current(false);

        t.join().expect("join failed");
        assert_eq!(
            rbs.card.load(Ordering::SeqCst),
            rbs.ring
                .iter()
                .map(|b| to_int(b.load(Ordering::SeqCst)))
                .fold(0, |acc, i| acc + i)
        );
        assert_eq!(6 % 5, rbs.index.load(Ordering::SeqCst));
    }
}
