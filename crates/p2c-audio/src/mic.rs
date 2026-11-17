//! Default microphone → 48 kHz stereo ring.
use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use p2c_core::{AudioRing, SAMPLE_RATE};

use crate::Resampler;

/// Keeps the input stream alive on its own thread (cpal streams aren't Send everywhere).
pub struct MicCapture {
    stop: std::sync::mpsc::Sender<()>,
    pub name: String,
}

impl MicCapture {
    pub fn start(ring: AudioRing) -> anyhow::Result<MicCapture> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<anyhow::Result<String>>();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        std::thread::Builder::new()
            .name("p2c-mic".into())
            .spawn(move || {
                let stream = match open(ring) {
                    Ok((s, name)) => {
                        let _ = ready_tx.send(Ok(name));
                        s
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let _ = stop_rx.recv();
                drop(stream);
            })?;
        let name = ready_rx.recv().context("microphone thread exited")??;
        Ok(MicCapture {
            stop: stop_tx,
            name,
        })
    }

    pub fn stop(self) {
        let _ = self.stop.send(());
    }
}

fn open(ring: AudioRing) -> anyhow::Result<(cpal::Stream, String)> {
    let host = cpal::default_host();
    let dev = host.default_input_device().context("no microphone found")?;
    let name = dev.name().unwrap_or_else(|_| "microphone".into());
    let cfg = dev
        .default_input_config()
        .context("microphone has no usable format")?;
    let channels = cfg.channels() as usize;
    let rate = cfg.sample_rate().0;
    let fmt = cfg.sample_format();
    let config: cpal::StreamConfig = cfg.into();
    let mut rs = Resampler::new(rate, SAMPLE_RATE);
    let mut push = move |data: Vec<f32>| ring.push(&rs.process(&to_stereo(&data, channels)));
    let err = |e| tracing::warn!(error = %e, "microphone stream error");
    let stream = match fmt {
        cpal::SampleFormat::F32 => {
            dev.build_input_stream(&config, move |d: &[f32], _: &_| push(d.to_vec()), err, None)
        }
        cpal::SampleFormat::I16 => dev.build_input_stream(
            &config,
            move |d: &[i16], _: &_| push(d.iter().map(|s| *s as f32 / 32768.0).collect()),
            err,
            None,
        ),
        cpal::SampleFormat::U16 => dev.build_input_stream(
            &config,
            move |d: &[u16], _: &_| {
                push(d.iter().map(|s| (*s as f32 - 32768.0) / 32768.0).collect())
            },
            err,
            None,
        ),
        other => anyhow::bail!("unsupported microphone sample format {other:?}"),
    }
    .context("opening microphone")?;
    stream.play().context("starting microphone")?;
    Ok((stream, name))
}

/// Interleaved `channels`-channel audio → stereo (mono duplicated, extras dropped).
pub(crate) fn to_stereo(data: &[f32], channels: usize) -> Vec<f32> {
    match channels {
        0 => Vec::new(),
        1 => data.iter().flat_map(|s| [*s, *s]).collect(),
        2 => data.to_vec(),
        n => data.chunks_exact(n).flat_map(|f| [f[0], f[1]]).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_stereo_handles_mono_and_multichannel() {
        assert_eq!(to_stereo(&[0.1, 0.2], 1), vec![0.1, 0.1, 0.2, 0.2]);
        assert_eq!(
            to_stereo(&[0.1, 0.2, 0.3, 0.4], 2),
            vec![0.1, 0.2, 0.3, 0.4]
        );
        assert_eq!(
            to_stereo(&[0.1, 0.2, 0.9, 0.3, 0.4, 0.9], 3),
            vec![0.1, 0.2, 0.3, 0.4]
        );
    }
}
