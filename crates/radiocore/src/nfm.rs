//! Narrowband FM voice channel: carrier squelch, CTCSS decode, audio shaping.

use std::collections::VecDeque;
use std::f32::consts::PI;

use rustfft::num_complex::Complex32;

use crate::biquad::{BUTTERWORTH4_Q, Biquad};
use crate::level::CarrierLevel;

/// Audio is held back this long so a transmission isn't clipped while its
/// CTCSS tone is still being recognised.
const DELAY_MS: usize = 350;
/// The squelch tail (noise between carrier drop and squelch close) to erase.
const TAIL_MS: usize = 40;
const TONE_SUB_MS: usize = 50;
const TONE_SUBS: usize = 5;
/// CTCSS deviation in Hz needed to open / below which to close.
const TONE_OPEN_HZ: f32 = 100.0;
const TONE_CLOSE_HZ: f32 = 60.0;
/// The tone must also stand this far above what the demodulated signal's
/// overall level would put in the tone bin by chance, which is what keeps
/// noise and weak, crackly carriers from passing as a tone.
const TONE_OVER_CHANCE: f32 = 3.5;

pub struct NfmConfig {
    /// Input (and block) rate; audio comes out at a third of this.
    pub sample_rate: f32,
    /// Samples per call to [`NfmChannel::process`]; must be a multiple of 3.
    pub block_len: usize,
    /// Peak deviation in Hz that maps to full-scale audio.
    pub max_deviation: f32,
    /// CTCSS tone required to open the channel, if any.
    pub tone_hz: Option<f32>,
    /// Carrier power over the noise floor needed to open, as a power ratio.
    pub squelch: f32,
}

pub struct NfmChannel {
    cfg: NfmConfig,
    prev: Complex32,
    level: CarrierLevel,
    carrier: bool,

    /// Slow average of the deviation: a carrier that is off frequency, which
    /// the tone detector must not mistake for part of a tone.
    tone_dc: f32,
    tone_dc_a: f32,
    tone_osc: Complex32,
    tone_step: Complex32,
    tone_acc: Complex32,
    tone_n: usize,
    tone_sums: VecDeque<Complex32>,
    dev_sq_acc: f32,
    dev_sq_sums: VecDeque<f32>,
    /// Consecutive sub-blocks with the carrier present throughout.
    tone_clean_subs: usize,
    sub_clean: bool,
    tone_ok: bool,

    deemph: f32,
    deemph_a: f32,
    filters: Vec<Biquad>,
    decim: usize,
    delay: VecDeque<f32>,
    hold: usize,
}

impl NfmChannel {
    pub fn new(cfg: NfmConfig) -> Self {
        assert!(cfg.block_len.is_multiple_of(3));
        let fs = cfg.sample_rate;
        let mut filters = Vec::new();
        if let Some(tone) = cfg.tone_hz {
            filters.push(Biquad::notch(fs, tone, 4.0));
        }
        for q in BUTTERWORTH4_Q {
            filters.push(Biquad::highpass(fs, 300.0, q));
        }
        for q in BUTTERWORTH4_Q {
            filters.push(Biquad::lowpass(fs, 3000.0, q));
        }
        let audio_rate = fs as usize / 3;
        let tone_w = 2.0 * PI * cfg.tone_hz.unwrap_or(0.0) / fs;
        Self {
            prev: Complex32::ZERO,
            level: CarrierLevel::default(),
            carrier: false,
            tone_dc: 0.0,
            tone_dc_a: 1.0 - (-2.0 * PI * 20.0 / fs).exp(),
            tone_osc: Complex32::new(1.0, 0.0),
            tone_step: Complex32::new(tone_w.cos(), -tone_w.sin()),
            tone_acc: Complex32::ZERO,
            tone_n: 0,
            tone_sums: VecDeque::from(vec![Complex32::ZERO; TONE_SUBS]),
            dev_sq_acc: 0.0,
            dev_sq_sums: VecDeque::from(vec![0.0; TONE_SUBS]),
            tone_clean_subs: 0,
            sub_clean: false,
            tone_ok: false,
            deemph: 0.0,
            deemph_a: 1.0 - (-2.0 * PI * 300.0 / fs).exp(),
            filters,
            decim: 0,
            delay: VecDeque::from(vec![0.0; audio_rate * DELAY_MS / 1000]),
            hold: 0,
            cfg,
        }
    }

    /// Change the carrier level over the noise floor needed to open.
    pub fn set_squelch(&mut self, ratio: f32) {
        self.cfg.squelch = ratio;
    }

    /// Noise floor estimate for this channel (infinite until warmed up).
    pub fn floor(&self) -> f32 {
        self.level.floor()
    }

    /// Carrier level over the noise floor, in dB.
    pub fn snr_db(&self) -> f32 {
        self.level.snr_db()
    }

    /// Demodulate one block. Appends `block_len / 3` audio samples (delayed by
    /// `DELAY_MS`) to `audio` and returns whether they belong to an open
    /// transmission. `floor_cap` bounds the noise floor from above so a
    /// channel that was busy from the start can still open.
    pub fn process(&mut self, iq: &[Complex32], floor_cap: f32, audio: &mut Vec<f32>) -> bool {
        debug_assert_eq!(iq.len(), self.cfg.block_len);
        let fs = self.cfg.sample_rate;

        if self.level.update(iq) {
            let reference = self.level.floor().min(floor_cap);
            let was = self.carrier;
            self.carrier = self.level.power() > reference * if was { self.cfg.squelch / 1.6 } else { self.cfg.squelch };
            if was && !self.carrier {
                let tail = (fs as usize / 3 * TAIL_MS / 1000).min(self.delay.len());
                self.delay.iter_mut().rev().take(tail).for_each(|v| *v = 0.0);
            }
        }

        self.sub_clean &= self.carrier;

        let to_hz = fs / (2.0 * PI);
        for &x in iq {
            let dev = if self.carrier {
                (x * self.prev.conj()).arg() * to_hz
            } else {
                0.0
            };
            self.prev = x;

            self.tone_dc += self.tone_dc_a * (dev - self.tone_dc);
            let ac = dev - self.tone_dc;
            self.tone_acc += self.tone_osc * ac;
            self.dev_sq_acc += ac * ac;
            self.tone_osc *= self.tone_step;

            self.deemph += self.deemph_a * (dev / self.cfg.max_deviation - self.deemph);
            let mut a = self.deemph;
            for f in &mut self.filters {
                a = f.step(a);
            }
            self.decim += 1;
            if self.decim == 3 {
                self.decim = 0;
                self.delay.push_back(a);
                audio.push(self.delay.pop_front().unwrap());
            }
        }
        self.tone_osc /= self.tone_osc.norm();

        self.tone_n += iq.len();
        let sub_len = fs as usize * TONE_SUB_MS / 1000;
        if self.tone_n >= sub_len {
            self.tone_sums.pop_front();
            self.tone_sums.push_back(self.tone_acc);
            self.dev_sq_sums.pop_front();
            self.dev_sq_sums.push_back(self.dev_sq_acc);
            let window = (self.tone_n * TONE_SUBS) as f32;
            let amp = 2.0 * self.tone_sums.iter().sum::<Complex32>().norm() / window;
            let chance = 2.0 * (self.dev_sq_sums.iter().sum::<f32>() / window).sqrt() / window.sqrt();
            // A window only partly filled with signal is too short to tell
            // neighbouring tones (4 Hz apart) from each other.
            self.tone_clean_subs = if self.sub_clean { self.tone_clean_subs + 1 } else { 0 };
            self.sub_clean = true;
            self.tone_ok = self.tone_clean_subs >= TONE_SUBS
                && amp > if self.tone_ok { TONE_CLOSE_HZ } else { TONE_OPEN_HZ }
                && amp > chance * TONE_OVER_CHANCE;
            self.tone_acc = Complex32::ZERO;
            self.dev_sq_acc = 0.0;
            self.tone_n = 0;
        }

        if self.carrier && (self.cfg.tone_hz.is_none() || self.tone_ok) {
            self.hold = DELAY_MS * fs as usize / 1000 / self.cfg.block_len;
        } else if self.hold > 0 {
            self.hold -= 1;
        }
        self.hold > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f32 = 48_000.0;
    const BLOCK: usize = 48;

    /// Deterministic white-ish noise in [-1, 1].
    struct Lcg(u32);
    impl Lcg {
        fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (self.0 >> 8) as f32 / 8_388_608.0 - 1.0
        }
    }

    /// 1 s of noise, then 2 s of a 1 kHz tone at 3 kHz deviation with the
    /// given CTCSS tone, then 1 s of noise. Returns (open blocks, peak audio).
    fn run(tx_tone: f32, rx_tone: Option<f32>) -> (usize, f32) {
        let mut ch = NfmChannel::new(NfmConfig {
            sample_rate: FS,
            block_len: BLOCK,
            max_deviation: 5000.0,
            tone_hz: rx_tone,
            squelch: 4.0,
        });
        let mut rng = Lcg(1);
        let (mut phase, mut open, mut peak) = (0.0f32, 0, 0.0f32);
        let mut audio = Vec::new();
        for b in 0..4000 {
            let keyed = (1000..3000).contains(&b);
            let iq: Vec<Complex32> = (0..BLOCK)
                .map(|i| {
                    let t = (b * BLOCK + i) as f32 / FS;
                    let dev = 3000.0 * (2.0 * PI * 1000.0 * t).sin() + 600.0 * (2.0 * PI * tx_tone * t).sin();
                    phase = (phase + 2.0 * PI * dev / FS) % (2.0 * PI);
                    let noise = Complex32::new(rng.next(), rng.next()) * 0.01;
                    noise
                        + if keyed {
                            Complex32::from_polar(1.0, phase)
                        } else {
                            Complex32::ZERO
                        }
                })
                .collect();
            audio.clear();
            if ch.process(&iq, f32::INFINITY, &mut audio) {
                open += 1;
                peak = audio.iter().fold(peak, |m, v| m.max(v.abs()));
            }
        }
        (open, peak)
    }

    #[test]
    fn opens_on_matching_tone() {
        let (open, peak) = run(114.8, Some(114.8));
        // Whole 2 s transmission minus detection latency, plus the delay line.
        assert!((2000..=2150).contains(&open), "open for {open} ms");
        // 3 kHz of 5 kHz deviation at 1 kHz, after 300 Hz de-emphasis.
        assert!((0.1..0.3).contains(&peak), "peak {peak}");
    }

    #[test]
    fn stays_shut_on_wrong_tone() {
        assert_eq!(run(162.2, Some(114.8)).0, 0);
        assert_eq!(run(110.9, Some(114.8)).0, 0);
    }

    #[test]
    fn noise_burst_is_not_a_tone() {
        let mut ch = NfmChannel::new(NfmConfig {
            sample_rate: FS,
            block_len: BLOCK,
            max_deviation: 5000.0,
            tone_hz: Some(114.8),
            squelch: 4.0,
        });
        let mut rng = Lcg(7);
        let mut audio = Vec::new();
        let mut open = 0;
        for b in 0..60_000 {
            // Noise that jumps 10 dB every other second: opens the carrier
            // squelch with nothing but noise behind it.
            let level = if b > 1000 && (b / 2000) % 2 == 1 { 0.03 } else { 0.01 };
            let iq: Vec<Complex32> = (0..BLOCK)
                .map(|_| Complex32::new(rng.next(), rng.next()) * level)
                .collect();
            audio.clear();
            open += ch.process(&iq, f32::INFINITY, &mut audio) as usize;
        }
        assert_eq!(open, 0);
    }

    #[test]
    fn carrier_squelch_without_tone() {
        let (open, _) = run(114.8, None);
        assert!((2300..=2380).contains(&open), "open for {open} ms");
    }
}
