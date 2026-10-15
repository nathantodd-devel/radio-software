//! Volume normalisation: brings a channel's audio to one loudness, so quiet
//! and loud transmitters (and analog and P25 channels) sound alike.

/// Loudness (RMS) audio is brought to, about that of a typical analog
/// channel unnormalised.
const TARGET_RMS: f32 = 0.08;
/// Peaks are held under this, so a sudden shout doesn't clip before the
/// gain has had time to come down. At the default volume of 3 this is
/// just under full scale.
const PEAK_LIMIT: f32 = 0.3;
/// The most a quiet channel is turned up and a loud one turned down by, so
/// a carrier with nobody speaking isn't brought up to a roar of noise.
const MAX_GAIN: f32 = 10.0;
const MIN_GAIN: f32 = 0.1;
/// How quickly the measured loudness follows the audio: fast when it gets
/// louder, so a loud start is turned down at once; slow when it gets
/// quieter, so pauses between words aren't turned up.
const ATTACK_SECS: f32 = 0.02;
const RELEASE_SECS: f32 = 1.5;

pub struct Normalizer {
    /// Mean square loudness the gain is set from; none until the first
    /// audio, which sets it outright so a new channel is right at once.
    level: Option<f32>,
    gain: f32,
    attack: f32,
    release: f32,
}

impl Normalizer {
    pub fn new(sample_rate: f32) -> Self {
        let coefficient = |secs: f32| 1.0 - (-1.0 / (secs * sample_rate)).exp();
        Self {
            level: None,
            gain: 1.0,
            attack: coefficient(ATTACK_SECS),
            release: coefficient(RELEASE_SECS),
        }
    }

    /// Normalise a block of audio in place.
    pub fn process(&mut self, audio: &mut [f32]) {
        if audio.is_empty() {
            return;
        }
        let power = audio.iter().map(|v| v * v).sum::<f32>() / audio.len() as f32;
        let level = self.level.get_or_insert(power);
        let rate = if power > *level { self.attack } else { self.release };
        // The per-sample smoothing applied for the whole block at once.
        let k = 1.0 - (1.0 - rate).powi(audio.len() as i32);
        *level += k * (power - *level);
        let mut gain = (TARGET_RMS / level.sqrt().max(1e-9)).clamp(MIN_GAIN, MAX_GAIN);
        let peak = audio.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let limited = peak * gain.max(self.gain) > PEAK_LIMIT;
        if limited {
            gain = gain.min(PEAK_LIMIT / peak);
        }
        // Glide from the last block's gain to this one's, so it doesn't
        // step, unless that would let a peak through.
        let from = if limited { gain } else { self.gain };
        let step = (gain - from) / audio.len() as f32;
        for (i, v) in audio.iter_mut().enumerate() {
            *v *= from + step * (i + 1) as f32;
        }
        self.gain = gain;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 16_000.0;

    /// RMS of the last second of a 1 kHz tone of `amplitude` after three
    /// seconds of normalising, 16 samples at a time.
    fn settled_rms(amplitude: f32) -> f32 {
        let mut n = Normalizer::new(RATE);
        let mut last = Vec::new();
        for block in 0..3000 {
            let mut audio: Vec<f32> = (0..16)
                .map(|i| amplitude * (2.0 * std::f32::consts::PI * 1000.0 * (block * 16 + i) as f32 / RATE).sin())
                .collect();
            n.process(&mut audio);
            if block >= 2000 {
                last.extend(audio);
            }
        }
        (last.iter().map(|v| v * v).sum::<f32>() / last.len() as f32).sqrt()
    }

    #[test]
    fn quiet_and_loud_come_out_alike() {
        let quiet = settled_rms(0.03);
        let loud = settled_rms(0.6);
        for rms in [quiet, loud] {
            assert!((rms / TARGET_RMS - 1.0).abs() < 0.05, "came out at {rms}");
        }
    }

    #[test]
    fn peaks_are_limited() {
        let mut n = Normalizer::new(RATE);
        let mut audio = vec![0.0; 16];
        audio[5] = 1.0;
        n.process(&mut audio);
        assert!(audio.iter().all(|v| v.abs() <= PEAK_LIMIT + 1e-6));
    }
}
