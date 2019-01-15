use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Debug)]
pub struct RingBitSet {
    len: usize,
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
            ring: buf.into_boxed_slice(),
            index: AtomicUsize::new(0),
        }
    }

    pub fn set_current(&mut self, b: bool) {
        loop {
            let i = self.index.load(Ordering::Acquire);
            let j = if i == self.len - 1 { 0 } else { i + 1 };
            let v = self.ring[i].load(Ordering::Acquire);
            if self.index.compare_and_swap(i, j, Ordering::Release) == i
                && self.ring[i].compare_and_swap(v, b, Ordering::Release) == v
            {
                break;
            }
        }
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
