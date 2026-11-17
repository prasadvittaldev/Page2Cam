use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use p2c_core::{Frame, FrameSlot};

use crate::compose;

/// Output video format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputSpec {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// A destination for composited frames (virtual camera, stream encoder, ...).
pub trait FrameSink: Send {
    fn push(&mut self, frame: &Frame) -> anyhow::Result<()>;
}

/// Runs compositing at a fixed rate on its own thread.
pub struct Ticker {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl Ticker {
    pub fn spawn(spec: OutputSpec, page: FrameSlot, mut sinks: Vec<Box<dyn FrameSink>>) -> Ticker {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let handle = std::thread::Builder::new()
            .name("p2c-ticker".into())
            .spawn(move || {
                let period = Duration::from_secs_f64(1.0 / spec.fps.max(1) as f64);
                let mut out = Frame::new_black(spec.width, spec.height);
                let mut last_warn: Vec<Option<Instant>> = vec![None; sinks.len()];
                let start = Instant::now();
                let mut n: u32 = 0;
                while !stop2.load(Ordering::Relaxed) {
                    let latest = page.latest();
                    compose(latest.as_deref(), &mut out);
                    for (i, sink) in sinks.iter_mut().enumerate() {
                        if let Err(e) = sink.push(&out) {
                            let due =
                                last_warn[i].is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
                            if due {
                                tracing::warn!(sink = i, error = %e, "frame sink failed");
                                last_warn[i] = Some(Instant::now());
                            }
                        }
                    }
                    // Absolute deadlines avoid drift; if we fell behind, skip missed ticks.
                    n += 1;
                    let now = Instant::now();
                    let mut deadline = start + period * n;
                    if deadline < now {
                        n = (now.duration_since(start).as_secs_f64() / period.as_secs_f64()) as u32
                            + 1;
                        deadline = start + period * n;
                    }
                    std::thread::sleep(deadline - now);
                }
            })
            .expect("spawn ticker thread");
        Ticker { stop, handle }
    }

    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2c_core::{Frame, FrameSlot};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    struct Counter(Arc<AtomicUsize>);
    impl FrameSink for Counter {
        fn push(&mut self, _: &Frame) -> anyhow::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct Failing;
    impl FrameSink for Failing {
        fn push(&mut self, _: &Frame) -> anyhow::Result<()> {
            anyhow::bail!("broken sink")
        }
    }

    const SPEC: OutputSpec = OutputSpec {
        width: 16,
        height: 16,
        fps: 30,
    };

    #[test]
    fn ticker_runs_at_fps() {
        let n = Arc::new(AtomicUsize::new(0));
        let t = Ticker::spawn(SPEC, FrameSlot::new(), vec![Box::new(Counter(n.clone()))]);
        std::thread::sleep(Duration::from_secs(1));
        t.stop();
        let c = n.load(Ordering::SeqCst);
        assert!((27..=33).contains(&c), "pushed {c} frames");
    }

    #[test]
    fn failing_sink_does_not_starve_others() {
        let n = Arc::new(AtomicUsize::new(0));
        let sinks: Vec<Box<dyn FrameSink>> = vec![Box::new(Failing), Box::new(Counter(n.clone()))];
        let t = Ticker::spawn(SPEC, FrameSlot::new(), sinks);
        std::thread::sleep(Duration::from_secs(1));
        t.stop();
        assert!(n.load(Ordering::SeqCst) >= 27);
    }
}
