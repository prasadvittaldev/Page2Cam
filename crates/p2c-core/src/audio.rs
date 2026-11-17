//! Shared audio buffer: interleaved stereo f32 at 48 kHz.
use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;

/// Bounded FIFO of interleaved samples. Producers push; the mixer pops.
/// When full, the oldest samples are dropped so latency never grows.
#[derive(Clone)]
pub struct AudioRing {
    inner: Arc<Mutex<VecDeque<f32>>>,
    cap: usize,
}

impl std::fmt::Debug for AudioRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AudioRing({} samples)", self.len_samples())
    }
}

impl AudioRing {
    /// Ring holding at most `max_ms` of audio.
    pub fn new(max_ms: u32) -> Self {
        let cap = (SAMPLE_RATE as usize * CHANNELS as usize * max_ms as usize) / 1000;
        AudioRing {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(cap))),
            cap,
        }
    }

    pub fn push(&self, samples: &[f32]) {
        let mut q = self.inner.lock();
        q.extend(samples.iter().copied());
        let excess = q.len().saturating_sub(self.cap);
        q.drain(..excess);
    }

    /// Fill `out` from the front; returns how many samples were available.
    pub fn pop_into(&self, out: &mut [f32]) -> usize {
        let mut q = self.inner.lock();
        let n = out.len().min(q.len());
        for (o, s) in out.iter_mut().zip(q.drain(..n)) {
            *o = s;
        }
        n
    }

    pub fn len_samples(&self) -> usize {
        self.inner.lock().len()
    }

    pub fn clear(&self) {
        self.inner.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_then_pop_in_order() {
        let r = AudioRing::new(1000);
        r.push(&[0.1, 0.2, 0.3, 0.4]);
        let mut out = [0.0; 4];
        assert_eq!(r.pop_into(&mut out), 4);
        assert_eq!(out, [0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn pop_reports_shortfall() {
        let r = AudioRing::new(1000);
        r.push(&[0.5, 0.5]);
        let mut out = [0.0; 6];
        assert_eq!(r.pop_into(&mut out), 2);
    }

    #[test]
    fn cap_drops_oldest() {
        // 1 ms at 48 kHz stereo = 96 samples
        let r = AudioRing::new(1);
        let a: Vec<f32> = (0..200).map(|i| i as f32).collect();
        r.push(&a);
        assert_eq!(r.len_samples(), 96);
        let mut out = [0.0; 1];
        r.pop_into(&mut out);
        assert_eq!(out[0], 104.0);
    }
}
