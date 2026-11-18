use std::sync::Arc;

use p2c_core::AudioRing;
use parking_lot::RwLock;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixSettings {
    pub page_on: bool,
    pub page_gain: f32,
    pub mic_on: bool,
    pub mic_gain: f32,
}

impl Default for MixSettings {
    fn default() -> Self {
        MixSettings {
            page_on: true,
            page_gain: 1.0,
            mic_on: true,
            mic_gain: 1.0,
        }
    }
}

/// Mixes page and mic rings into one 48 kHz stereo stream. Missing samples are
/// silence, so the output never has gaps. Disabled sources are still drained so
/// re-enabling them doesn't replay old audio.
#[derive(Clone)]
pub struct Mixer {
    page: AudioRing,
    mic: AudioRing,
    settings: Arc<RwLock<MixSettings>>,
    delay: Arc<parking_lot::Mutex<MicDelay>>,
}

#[derive(Default)]
struct MicDelay {
    /// Current delay in frames.
    frames: usize,
    /// Frames of silence still to insert before the mic.
    insert: usize,
    /// Frames of queued mic audio still to skip.
    skip: usize,
}

impl Mixer {
    pub fn new(page: AudioRing, mic: AudioRing, settings: MixSettings) -> Self {
        Mixer {
            page,
            mic,
            settings: Arc::new(RwLock::new(settings)),
            delay: Arc::default(),
        }
    }

    pub fn set(&self, s: MixSettings) {
        *self.settings.write() = s;
    }

    pub fn settings(&self) -> MixSettings {
        *self.settings.read()
    }

    /// Delay the mic by `ms` relative to now (to line up with a video path
    /// that lags by that much). Growing inserts silence once; shrinking skips
    /// queued mic audio. Capped by the mic ring's capacity.
    pub fn set_mic_delay_ms(&self, ms: u32) {
        let want = ms as usize * p2c_core::SAMPLE_RATE as usize / 1000;
        let mut d = self.delay.lock();
        if want > d.frames {
            let grow = want - d.frames;
            let cancel = grow.min(d.skip);
            d.skip -= cancel;
            d.insert += grow - cancel;
        } else {
            let shrink = d.frames - want;
            let cancel = shrink.min(d.insert);
            d.insert -= cancel;
            d.skip += shrink - cancel;
        }
        d.frames = want;
    }

    /// Drop anything buffered (e.g. when a stream starts).
    pub fn reset(&self) {
        self.page.clear();
        self.mic.clear();
    }

    /// `frames` stereo frames (2 * frames samples).
    pub fn pull(&self, frames: usize) -> Vec<f32> {
        let s = self.settings();
        let mut out = vec![0.0f32; frames * 2];
        let mut tmp = vec![0.0f32; frames * 2];
        let ch = p2c_core::CHANNELS as usize;
        let mic_offset = {
            let mut d = self.delay.lock();
            while d.skip > 0 {
                let n = d.skip.min(frames);
                let got = self.mic.pop_into(&mut tmp[..n * ch]) / ch;
                d.skip -= got;
                if got < n {
                    d.skip = 0; // nothing more queued: caught up
                }
            }
            let ins = d.insert.min(frames);
            d.insert -= ins;
            ins * ch
        };
        for (ring, on, gain, offset) in [
            (&self.page, s.page_on, s.page_gain, 0),
            (&self.mic, s.mic_on, s.mic_gain, mic_offset),
        ] {
            tmp.iter_mut().for_each(|x| *x = 0.0);
            let n = ring.pop_into(&mut tmp[offset..]);
            if on {
                for (o, v) in out[offset..].iter_mut().zip(&tmp[offset..offset + n]) {
                    *o += v * gain;
                }
            }
        }
        out.iter_mut().for_each(|x| *x = x.clamp(-1.0, 1.0));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2c_core::AudioRing;

    fn mixer(s: MixSettings) -> (Mixer, AudioRing, AudioRing) {
        let page = AudioRing::new(1000);
        let mic = AudioRing::new(1000);
        (Mixer::new(page.clone(), mic.clone(), s), page, mic)
    }

    #[test]
    fn mixer_fills_silence() {
        let (m, page, _mic) = mixer(MixSettings::default());
        page.push(&[0.5, 0.5]);
        let out = m.pull(4);
        assert_eq!(out.len(), 8);
        assert_eq!(&out[..2], &[0.5, 0.5]);
        assert!(out[2..].iter().all(|s| *s == 0.0));
    }

    #[test]
    fn mixer_applies_gain_and_clamps() {
        let s = MixSettings {
            page_on: true,
            page_gain: 0.5,
            mic_on: true,
            mic_gain: 2.0,
        };
        let (m, page, mic) = mixer(s);
        page.push(&[0.4, -0.4]);
        mic.push(&[0.6, -0.6]);
        let out = m.pull(1);
        assert_eq!(out, vec![1.0, -1.0]); // 0.2 + 1.2 clamped
    }

    #[test]
    fn disabled_source_is_drained_but_silent() {
        let s = MixSettings {
            page_on: false,
            ..MixSettings::default()
        };
        let (m, page, _mic) = mixer(s);
        page.push(&[0.9, 0.9]);
        assert_eq!(m.pull(1), vec![0.0, 0.0]);
        assert_eq!(page.len_samples(), 0);
    }

    #[test]
    fn mic_delay_inserts_silence() {
        let (m, _page, mic) = mixer(MixSettings::default());
        mic.push(&[0.5; 2000]);
        m.set_mic_delay_ms(10);
        let out = m.pull(1000);
        assert!(
            out[..960].iter().all(|s| *s == 0.0),
            "first 480 frames not silent"
        );
        assert!(out[960..].iter().all(|s| *s == 0.5));
    }

    #[test]
    fn mic_delay_shrink_drops_samples() {
        let (m, _page, mic) = mixer(MixSettings::default());
        let ramp: Vec<f32> = (0..2000).map(|i| (i / 2) as f32 / 1000.0).collect();
        mic.push(&ramp);
        m.set_mic_delay_ms(10);
        assert!(m.pull(480).iter().all(|s| *s == 0.0));
        m.set_mic_delay_ms(0);
        // 480 queued frames are skipped to catch up.
        assert_eq!(m.pull(1), vec![0.48, 0.48]);
    }

    #[test]
    fn settings_change_live() {
        let (m, page, _mic) = mixer(MixSettings::default());
        m.set(MixSettings {
            page_gain: 0.0,
            ..MixSettings::default()
        });
        page.push(&[0.9, 0.9]);
        assert_eq!(m.pull(1), vec![0.0, 0.0]);
    }
}
