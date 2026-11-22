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
    /// Overlay-layer sound (videos, YouTube): mixed as page sound.
    overlay: Option<AudioRing>,
    mic: AudioRing,
    settings: Arc<RwLock<MixSettings>>,
    delay: Arc<parking_lot::Mutex<MicDelay>>,
}

/// The mic is played `frames` behind real time by keeping that much audio
/// queued: after a reset, an underrun or a late mic start it waits until the
/// backlog is built up again, and it skips ahead if the backlog grows past
/// the target plus some slack.
#[derive(Default)]
struct MicDelay {
    /// Target backlog in frames (0 = no delay).
    frames: usize,
    /// Waiting for the backlog to reach `frames`.
    priming: bool,
}

/// Backlog above the target that is tolerated before skipping (20 ms).
const DELAY_SLACK_FRAMES: usize = 960;

impl Mixer {
    pub fn new(page: AudioRing, mic: AudioRing, settings: MixSettings) -> Self {
        Mixer {
            page,
            overlay: None,
            mic,
            settings: Arc::new(RwLock::new(settings)),
            delay: Arc::default(),
        }
    }

    /// Also mix `ring` (the overlay browser's sound) as part of page sound:
    /// same on/off and gain.
    pub fn with_overlay(mut self, ring: AudioRing) -> Self {
        self.overlay = Some(ring);
        self
    }

    pub fn set(&self, s: MixSettings) {
        *self.settings.write() = s;
    }

    pub fn settings(&self) -> MixSettings {
        *self.settings.read()
    }

    /// Play the mic `ms` late (to line up with a video path that lags by that
    /// much). Kept through stream restarts and mic restarts. Limited by the
    /// mic ring's capacity.
    pub fn set_mic_delay_ms(&self, ms: u32) {
        let want = ms as usize * p2c_core::SAMPLE_RATE as usize / 1000;
        let mut d = self.delay.lock();
        if want > d.frames {
            d.priming = true;
        }
        d.frames = want;
    }

    /// Drop anything buffered (e.g. when a stream starts).
    pub fn reset(&self) {
        self.page.clear();
        if let Some(o) = &self.overlay {
            o.clear();
        }
        self.mic.clear();
        let mut d = self.delay.lock();
        d.priming = d.frames > 0;
    }

    /// `frames` stereo frames (2 * frames samples).
    pub fn pull(&self, frames: usize) -> Vec<f32> {
        let s = self.settings();
        let mut out = vec![0.0f32; frames * 2];
        let mut tmp = vec![0.0f32; frames * 2];
        let ch = p2c_core::CHANNELS as usize;
        let hold_mic = {
            let mut d = self.delay.lock();
            if d.frames == 0 {
                d.priming = false;
            } else {
                let queued = self.mic.len_samples() / ch;
                if queued == 0 {
                    d.priming = true; // underrun or restart: build the delay again
                }
                if d.priming && queued >= d.frames {
                    d.priming = false;
                }
                if !d.priming && queued > d.frames + DELAY_SLACK_FRAMES {
                    let mut drop = vec![0.0f32; (queued - d.frames) * ch];
                    self.mic.pop_into(&mut drop);
                }
            }
            d.priming
        };
        let sources = [
            Some((&self.page, s.page_on, s.page_gain, false)),
            self.overlay
                .as_ref()
                .map(|o| (o, s.page_on, s.page_gain, false)),
            Some((&self.mic, s.mic_on, s.mic_gain, hold_mic)),
        ];
        for (ring, on, gain, hold) in sources.into_iter().flatten() {
            if hold {
                continue;
            }
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
    fn mic_delay_waits_for_backlog() {
        // 10 ms = 480 frames: mic audio is held back until that much is queued.
        let (m, _page, mic) = mixer(MixSettings::default());
        m.set_mic_delay_ms(10);
        mic.push(&[0.5; 480]); // 240 frames
        assert!(
            m.pull(240).iter().all(|s| *s == 0.0),
            "played before the delay"
        );
        mic.push(&[0.5; 480]); // 480 queued now
        assert!(m.pull(240).iter().all(|s| *s == 0.5));
    }

    #[test]
    fn mic_delay_survives_reset_and_late_mic() {
        // Stream restarts clear the rings; the mic may also start later.
        let (m, _page, mic) = mixer(MixSettings::default());
        m.set_mic_delay_ms(10);
        assert!(m.pull(100).iter().all(|s| *s == 0.0));
        mic.push(&[0.5; 960]);
        assert!(m.pull(240).iter().all(|s| *s == 0.5));
        m.reset();
        mic.push(&[0.5; 480]); // 240 frames < 480
        assert!(
            m.pull(240).iter().all(|s| *s == 0.0),
            "delay lost after reset"
        );
    }

    #[test]
    fn mic_delay_shrink_skips_backlog() {
        let (m, _page, mic) = mixer(MixSettings::default());
        let ramp: Vec<f32> = (0..4000).map(|i| (i / 2) as f32 / 10000.0).collect();
        mic.push(&ramp); // 2000 frames queued
        m.set_mic_delay_ms(10);
        // Backlog 2000 > 480 + 960 slack: skip down to 480 frames queued.
        let out = m.pull(1);
        assert_eq!(out, vec![0.152, 0.152]);
    }

    #[test]
    fn no_delay_plays_immediately() {
        let (m, _page, mic) = mixer(MixSettings::default());
        mic.push(&[0.5; 2]);
        assert_eq!(m.pull(1), vec![0.5, 0.5]);
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

    #[test]
    fn overlay_sound_follows_page_settings() {
        let (page, mic, ov) = (AudioRing::new(1000), AudioRing::new(1000), AudioRing::new(1000));
        let m = Mixer::new(page.clone(), mic, MixSettings::default()).with_overlay(ov.clone());
        page.push(&[0.25, 0.25]);
        ov.push(&[0.5, 0.5]);
        assert_eq!(&m.pull(1)[..], &[0.75, 0.75]);
        m.set(MixSettings {
            page_on: true,
            page_gain: 0.5,
            ..MixSettings::default()
        });
        ov.push(&[0.5, 0.5]);
        assert_eq!(&m.pull(1)[..], &[0.25, 0.25]);
        m.set(MixSettings {
            page_on: false,
            ..MixSettings::default()
        });
        ov.push(&[0.5, 0.5]);
        assert_eq!(&m.pull(1)[..], &[0.0, 0.0]);
        // Drained while off: nothing replays when page sound comes back.
        m.set(MixSettings::default());
        assert_eq!(&m.pull(1)[..], &[0.0, 0.0]);
    }
}
