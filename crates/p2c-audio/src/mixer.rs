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
}

impl Mixer {
    pub fn new(page: AudioRing, mic: AudioRing, settings: MixSettings) -> Self {
        Mixer {
            page,
            mic,
            settings: Arc::new(RwLock::new(settings)),
        }
    }

    pub fn set(&self, s: MixSettings) {
        *self.settings.write() = s;
    }

    pub fn settings(&self) -> MixSettings {
        *self.settings.read()
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
        for (ring, on, gain) in [
            (&self.page, s.page_on, s.page_gain),
            (&self.mic, s.mic_on, s.mic_gain),
        ] {
            tmp.iter_mut().for_each(|x| *x = 0.0);
            let n = ring.pop_into(&mut tmp);
            if on {
                for (o, v) in out.iter_mut().zip(&tmp[..n]) {
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
