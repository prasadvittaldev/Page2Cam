//! Process loopback against real Windows audio: two child processes play
//! different tones; capturing one of them must hear only its tone.
#![cfg(windows)]

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use cpal::traits::HostTrait;
use p2c_audio::ProcessLoopback;
use p2c_core::{AudioRing, SAMPLE_RATE};

/// Kills the tone players even when an assertion fails.
struct Players(Vec<Child>);

impl Drop for Players {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// 16-bit mono PCM sine, 48 kHz.
fn write_tone(path: &Path, hz: f32, secs: f32) {
    let n = (SAMPLE_RATE as f32 * secs) as u32;
    let mut b = Vec::with_capacity(44 + n as usize * 2);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + n * 2).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    b.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(n * 2).to_le_bytes());
    for i in 0..n {
        let t = i as f32 / SAMPLE_RATE as f32;
        let v = (t * hz * std::f32::consts::TAU).sin() * 0.3 * 32767.0;
        b.extend_from_slice(&(v as i16).to_le_bytes());
    }
    std::fs::write(path, b).unwrap();
}

fn play(path: &Path) -> Child {
    let script = format!(
        "(New-Object Media.SoundPlayer '{}').PlaySync()",
        path.display()
    );
    Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawning powershell")
}

/// Goertzel power of `hz` in the left channel of interleaved stereo.
fn goertzel(stereo: &[f32], hz: f32) -> f32 {
    let w = std::f32::consts::TAU * hz / SAMPLE_RATE as f32;
    let k = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for f in stereo.as_chunks::<2>().0 {
        let s = f[0] + k * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    s1 * s1 + s2 * s2 - k * s1 * s2
}

#[test]
#[ignore = "plays tones on the real audio output"]
fn captures_only_the_target_process() {
    if cpal::default_host().default_output_device().is_none() {
        eprintln!("skipped: no audio output endpoint");
        return;
    }
    let dir = std::env::temp_dir().join(format!("p2c-loopback-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (a_wav, b_wav) = (dir.join("a440.wav"), dir.join("b1000.wav"));
    write_tone(&a_wav, 440.0, 5.0);
    write_tone(&b_wav, 1000.0, 5.0);

    let players = Players(vec![play(&a_wav), play(&b_wav)]);
    let ring = AudioRing::new(5000);
    let cap = ProcessLoopback::start(players.0[0].id(), ring.clone()).expect("starting loopback");
    // PowerShell needs a moment before it plays; then listen for 2 s.
    std::thread::sleep(Duration::from_millis(1500));
    ring.clear();
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(cap.error(), None);
    drop(cap);
    let mut buf = vec![0.0f32; ring.len_samples()];
    let n = ring.pop_into(&mut buf);
    buf.truncate(n);
    drop(players);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        n > SAMPLE_RATE as usize,
        "too little audio captured: {n} samples"
    );
    let p440 = goertzel(&buf, 440.0);
    let p1000 = goertzel(&buf, 1000.0);
    eprintln!("samples {n}, 440 Hz {p440:.1}, 1000 Hz {p1000:.1}");
    assert!(p440 > 1.0, "no 440 Hz energy captured ({p440})");
    assert!(
        p1000 < p440 / 10.0,
        "1000 Hz leaked in: {p1000} vs 440 Hz {p440}"
    );
}
