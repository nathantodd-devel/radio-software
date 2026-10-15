//! Overlap-save FFT channelizer: pulls many narrow channels out of one
//! wideband I/Q stream for the cost of a single forward FFT per block.

use std::f64::consts::PI;

use rustfft::num_complex::Complex32;

use crate::fft::Fft;

pub struct Channelizer {
    n: usize,
    bin_hz: f64,
    m: usize,
    hop: usize,
    fft: Fft,
    ifft: Fft,
    window: Vec<Complex32>,
    spec: Vec<Complex32>,
    chan: Vec<Complex32>,
    /// Prototype low-pass response for bins -m/2..m/2, scaled by 1/n.
    resp: Vec<Complex32>,
    /// Centre bin of each channel, relative to the tuned frequency.
    bins: Vec<i64>,
    odd_block: bool,
}

impl Channelizer {
    /// `fs` is the input rate; channels come out at `bin_hz * m` samples/s,
    /// low-passed at `cutoff_hz` either side of `offsets_hz[i]`. Offsets are
    /// rounded to the nearest bin.
    pub fn new(fs: f64, bin_hz: f64, m: usize, cutoff_hz: f64, offsets_hz: &[f64]) -> Self {
        let n = (fs / bin_hz).round() as usize;
        assert!(
            n.is_multiple_of(2) && m.is_multiple_of(2) && m < n,
            "bad channelizer geometry"
        );
        let hop = n / 2;

        let mut fft = Fft::forward(n);
        let ifft = Fft::inverse(m);

        // Hamming-windowed sinc no longer than hop+1 taps, so the last `hop`
        // samples of each circular convolution are free of time aliasing.
        let taps = hop + 1;
        let mid = (taps - 1) as f64 / 2.0;
        let wc = 2.0 * cutoff_hz / fs;
        let mut h: Vec<f64> = (0..taps)
            .map(|i| {
                let t = i as f64 - mid;
                let sinc = if t == 0.0 { wc } else { (PI * wc * t).sin() / (PI * t) };
                sinc * (0.54 - 0.46 * (2.0 * PI * i as f64 / (taps - 1) as f64).cos())
            })
            .collect();
        let sum: f64 = h.iter().sum();
        h.iter_mut().for_each(|v| *v /= sum);

        let mut hf: Vec<Complex32> = h.iter().map(|&v| Complex32::new(v as f32, 0.0)).collect();
        hf.resize(n, Complex32::ZERO);
        fft.process(&mut hf);
        let half = m as i64 / 2;
        let resp = (-half..half)
            .map(|j| hf[j.rem_euclid(n as i64) as usize] / n as f32)
            .collect();

        Self {
            n,
            bin_hz,
            m,
            hop,
            fft,
            ifft,
            window: vec![Complex32::ZERO; n],
            spec: vec![Complex32::ZERO; n],
            chan: vec![Complex32::ZERO; m],
            resp,
            bins: offsets_hz.iter().map(|f| (f / bin_hz).round() as i64).collect(),
            odd_block: false,
        }
    }

    /// Input samples consumed per call to [`process`](Self::process).
    pub fn hop(&self) -> usize {
        self.hop
    }

    /// Output samples per channel per call to [`process`](Self::process).
    pub fn out_len(&self) -> usize {
        self.m / 2
    }

    /// Add the power the last block had from `lo_hz` to `hi_hz` (relative to
    /// the tuned frequency) to `columns`, which split that span evenly. Each
    /// column gets the mean of the FFT bins in it, in units of full scale
    /// squared; where there are fewer bins than columns, they share bins.
    pub fn add_power(&self, lo_hz: f64, hi_hz: f64, columns: &mut [f32]) {
        let n = self.n as i64;
        let first = (lo_hz / self.bin_hz).ceil() as i64;
        let bins = ((hi_hz / self.bin_hz).floor() as i64 - first + 1).max(1);
        let count = columns.len() as i64;
        // An unnormalised FFT of a full-scale tone peaks at n.
        let scale = 1.0 / (self.n as f32 * self.n as f32);
        for (c, column) in (0..).zip(columns.iter_mut()) {
            let from = first + c * bins / count;
            let to = (first + (c + 1) * bins / count).max(from + 1);
            let sum: f32 = (from..to).map(|k| self.spec[k.rem_euclid(n) as usize].norm_sqr()).sum();
            *column += sum * scale / (to - from) as f32;
        }
    }

    /// Feed exactly `hop()` samples; `sink(channel, samples)` is called once
    /// per channel with `out_len()` baseband samples.
    pub fn process(&mut self, input: &[Complex32], mut sink: impl FnMut(usize, &[Complex32])) {
        assert_eq!(input.len(), self.hop);
        self.window.copy_within(self.hop.., 0);
        self.window[self.n - self.hop..].copy_from_slice(input);
        self.spec.copy_from_slice(&self.window);
        self.fft.process(&mut self.spec);

        let (n, m) = (self.n as i64, self.m as i64);
        for (c, &k0) in self.bins.iter().enumerate() {
            for (i, r) in self.resp.iter().enumerate() {
                let j = i as i64 - m / 2;
                self.chan[j.rem_euclid(m) as usize] = self.spec[(k0 + j).rem_euclid(n) as usize] * r;
            }
            self.ifft.process(&mut self.chan);
            let out = &mut self.chan[self.m / 2..];
            // Shifting by k0 bins restarts the mixer phase every block; with
            // 50% overlap that is a sign flip on odd blocks for odd bins.
            if self.odd_block && k0 % 2 != 0 {
                out.iter_mut().for_each(|v| *v = -*v);
            }
            sink(c, out);
        }
        self.odd_block = !self.odd_block;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(fs: f64, f: f64, len: usize) -> Vec<Complex32> {
        (0..len)
            .map(|i| {
                let p = 2.0 * PI * f * i as f64 / fs;
                Complex32::new(p.cos() as f32, p.sin() as f32)
            })
            .collect()
    }

    #[test]
    fn separates_tones_with_continuous_phase() {
        let fs = 1_000_000.0;
        // Channel 0 sits on an odd bin (exercises the sign flip) and sees a
        // tone 1 kHz above its centre; channel 1 is 25 kHz away and empty.
        let offsets = [-112_500.0, -87_500.0];
        let mut ch = Channelizer::new(fs, 500.0, 96, 9_000.0, &offsets);
        let x = tone(fs, -111_500.0, ch.hop() * 40);
        let mut out = vec![Vec::new(); 2];
        for block in x.chunks_exact(ch.hop()) {
            ch.process(block, |c, s| out[c].extend_from_slice(s));
        }
        let settled = &out[0][96..];
        for w in settled.windows(2) {
            assert!((w[0].norm() - 1.0).abs() < 0.01, "gain {}", w[0].norm());
            let step = (w[1] * w[0].conj()).arg();
            let want = 2.0 * std::f32::consts::PI * 1000.0 / 48_000.0;
            assert!((step - want).abs() < 1e-3, "phase step {step} vs {want}");
        }
        let leak = out[1][96..].iter().map(|v| v.norm()).fold(0.0, f32::max);
        assert!(leak < 0.005, "adjacent channel leak {leak}");
    }

    #[test]
    fn power_shows_where_a_tone_is() {
        let fs = 1_000_000.0;
        let mut ch = Channelizer::new(fs, 500.0, 96, 9_000.0, &[0.0]);
        // 100 columns of 2 kHz from -100 kHz; the tone is in column 70.
        let x = tone(fs, 41_000.0, ch.hop() * 4);
        let mut columns = vec![0.0; 100];
        for block in x.chunks_exact(ch.hop()) {
            ch.process(block, |_, _| {});
        }
        ch.add_power(-100_000.0, 100_000.0, &mut columns);
        let loudest = (0..100).max_by(|&a, &b| columns[a].total_cmp(&columns[b])).unwrap();
        assert_eq!(loudest, 70);
        let quiet = columns.iter().enumerate().filter(|&(c, _)| c.abs_diff(70) > 2);
        let floor = quiet.map(|(_, &p)| p).fold(0.0, f32::max);
        assert!(columns[70] > floor * 1000.0, "{} over {floor}", columns[70]);

        // Narrower than a bin a column: neighbouring columns share bins.
        let mut fine = vec![0.0; 40];
        ch.add_power(40_000.0, 42_000.0, &mut fine);
        assert!(fine.iter().all(|p| p.is_finite() && *p > 0.0));
    }
}
