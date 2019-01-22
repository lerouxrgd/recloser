use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

#[derive(Debug)]
struct RingBitSet {
    len: usize,
    card: AtomicUsize,
    ring: Box<[AtomicBool]>,
    index: AtomicUsize,
}

impl RingBitSet {
    pub fn new(len: usize) -> Self {
        let mut buf = Vec::with_capacity(len);

        for _ in 0..len {
            buf.push(AtomicBool::new(false));
        }

        Self {
            len: len,
            card: AtomicUsize::new(0),
            ring: buf.into_boxed_slice(),
            index: AtomicUsize::new(0),
        }
    }

    pub fn set_current(&mut self, val_new: bool) -> bool {
        loop {
            let i = self.index.load(Ordering::Acquire);
            let j = if i == self.len - 1 { 0 } else { i + 1 };

            let val_old = self.ring[i].load(Ordering::Acquire);

            let card_old = self.card.load(Ordering::Acquire);
            let card_new = card_old - to_int(val_old) + to_int(val_new);

            if self.index.compare_and_swap(i, j, Ordering::Release) == i
                && self.ring[i].compare_and_swap(val_old, val_new, Ordering::Release) == val_old
                && self
                    .card
                    .compare_and_swap(card_old, card_new, Ordering::Release)
                    == card_old
            {
                return val_old;
            }
        }
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
    use super::*;

    #[test]
    fn it_works() {
        let mut rbs = RingBitSet::new(5);
        rbs.set_current(true);
        rbs.set_current(true);
        rbs.set_current(true);
        rbs.set_current(true);
        rbs.set_current(true);
        rbs.set_current(false);
        println!("--> {:?}", rbs);
        assert_eq!(2 + 2, 4);
    }
}
