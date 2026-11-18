use std::sync::Arc;

use parking_lot::Mutex;

use crate::{Frame, Mask};

/// Holds only the newest value. Producers overwrite; consumers read the latest
/// without draining a queue. `seq` counts puts so consumers can detect staleness.
pub struct Slot<T> {
    inner: Arc<Mutex<(Option<Arc<T>>, u64)>>,
}

/// Latest video frame.
pub type FrameSlot = Slot<Frame>;
/// Latest cut-out mask.
pub type MaskSlot = Slot<Mask>;

impl<T> Clone for Slot<T> {
    fn clone(&self) -> Self {
        Slot {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Slot {
            inner: Arc::new(Mutex::new((None, 0))),
        }
    }
}

impl<T> Slot<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, v: T) {
        let mut g = self.inner.lock();
        g.0 = Some(Arc::new(v));
        g.1 += 1;
    }

    /// Like `put`, for a value that is already shared (no copy).
    pub fn put_arc(&self, v: Arc<T>) {
        let mut g = self.inner.lock();
        g.0 = Some(v);
        g.1 += 1;
    }

    pub fn latest(&self) -> Option<Arc<T>> {
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
    fn slot_holds_any_type() {
        let s: Slot<crate::Mask> = Slot::new();
        assert!(s.latest().is_none());
        s.put(crate::Mask::new(2, 1, vec![0, 255]).unwrap());
        assert_eq!((s.latest().unwrap().data[1], s.seq()), (255, 1));
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
