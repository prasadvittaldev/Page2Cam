/// Streaming linear resampler for interleaved stereo f32.
pub struct Resampler {
    step: f64,
    t: f64,
    prev: Option<[f32; 2]>,
    passthrough: bool,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        Resampler {
            step: in_rate as f64 / out_rate.max(1) as f64,
            t: 0.0,
            prev: None,
            passthrough: in_rate == out_rate,
        }
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if self.passthrough {
            return input.to_vec();
        }
        let mut buf: Vec<[f32; 2]> = Vec::with_capacity(input.len() / 2 + 1);
        buf.extend(self.prev);
        buf.extend(input.as_chunks::<2>().0.iter().copied());
        let mut out = Vec::with_capacity((input.len() as f64 / self.step) as usize + 4);
        while self.t + 1.0 < buf.len() as f64 {
            let i = self.t as usize;
            let f = (self.t - i as f64) as f32;
            for c in 0..2 {
                out.push(buf[i][c] + (buf[i + 1][c] - buf[i][c]) * f);
            }
            self.t += self.step;
        }
        if let Some(last) = buf.last() {
            self.t -= (buf.len() - 1) as f64;
            self.prev = Some(*last);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_linear_length() {
        let mut r = Resampler::new(44_100, 48_000);
        let input = vec![0.25f32; 441 * 2]; // 10 ms stereo
        let out = r.process(&input);
        let frames = out.len() / 2;
        assert!((479..=481).contains(&frames), "{frames}");
        assert!(out.iter().all(|s| (*s - 0.25).abs() < 1e-6));
    }

    #[test]
    fn same_rate_passthrough() {
        let mut r = Resampler::new(48_000, 48_000);
        assert_eq!(r.process(&[0.1, 0.2, 0.3, 0.4]), vec![0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn chunked_equals_whole_length() {
        let mut a = Resampler::new(16_000, 48_000);
        let mut b = Resampler::new(16_000, 48_000);
        let input: Vec<f32> = (0..3200).map(|i| (i as f32 * 0.01).sin()).collect();
        let whole = a.process(&input).len();
        let chunked: usize = input.chunks(100).map(|c| b.process(c).len()).sum();
        assert!(
            (whole as i64 - chunked as i64).abs() <= 2,
            "{whole} vs {chunked}"
        );
    }
}
