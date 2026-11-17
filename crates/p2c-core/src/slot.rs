use std::sync::Arc;

use parking_lot::Mutex;

use crate::Frame;

/// Holds only the newest frame. Producers overwrite; consumers read the latest
/// without draining a queue. `seq` counts puts so consumers can detect staleness.
#[derive(Clone, Default)]
pub struct FrameSlot {
    inner: Arc<Mutex<(Option<Arc<Frame>>, u64)>>,
}

impl FrameSlot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, f: Frame) {
        let mut g = self.inner.lock();
        g.0 = Some(Arc::new(f));
        g.1 += 1;
    }

    pub fn latest(&self) -> Option<Arc<Frame>> {
        self.inner.lock().0.clone()
    }

    pub fn seq(&self) -> u64 {
        self.inner.lock().1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Frame;

    #[test]
    fn slot_empty_returns_none() {
        assert!(FrameSlot::new().latest().is_none());
        assert_eq!(FrameSlot::new().seq(), 0);
    }

    #[test]
    fn slot_returns_latest_and_counts() {
        let s = FrameSlot::new();
        s.put(Frame::new_black(2, 2));
        s.put(Frame::new_black(4, 2));
        assert_eq!(s.latest().unwrap().width, 4);
        assert_eq!(s.seq(), 2);
    }

    #[test]
    fn slot_shared_across_threads() {
        let s = FrameSlot::new();
        let s2 = s.clone();
        std::thread::spawn(move || s2.put(Frame::new_black(2, 2)))
            .join()
            .unwrap();
        assert!(s.latest().is_some());
    }
}
