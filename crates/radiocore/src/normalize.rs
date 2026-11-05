//! Volume normalisation: brings a channel's audio to one loudness, so quiet
//! and loud transmitters (and analog and P25 channels) sound alike.

/// Loudness (RMS) audio is brought to at a volume of 1. Speech's syllables
/// and gaps leave it playing at about this times the volume.
const TARGET_RMS: f32 = 0.2;
/// Peaks are held under this, full scale being 1, so loud audio at a high
/// volume is turned down rather than clipped.
const CEILING: f32 = 0.95;
/// The most a quiet channel is turned up and a loud one turned down by, so
/// silence isn't brought up to a roar of noise.
const MAX_GAIN: f32 = 30.0;
const MIN_GAIN: f32 = 0.1;
/// How long the loudness is measured over: long enough to take in a few
/// syllables, so speech is judged by its average and not by its peaks.
const LOUDNESS_SECS: f32 = 0.3;
/// How quickly the gain comes back after a peak has turned it down.
const LIMITER_RELEASE_SECS: f32 = 0.1;

pub struct Normalizer {
    /// Mean square loudness the gain is set from; none until the first
    /// audio, which sets it outright so a new channel is right at once.
    level: Option<f32>,
    /// How far the limiter has turned the gain down, from 1 (not at all).
    limiting: f32,
    gain: f32,
    smoothing: f32,
    limiter_release: f32,
}

impl Normalizer {
    pub fn new(sample_rate: f32) -> Self {
        let coefficient = |secs: f32| 1.0 - (-1.0 / (secs * sample_rate)).exp();
        Self {
            level: None,
            limiting: 1.0,
            gain: 0.0,
            smoothing: coefficient(LOUDNESS_SECS),
            limiter_release: coefficient(LIMITER_RELEASE_SECS),
        }
    }

    /// Normalise a block of audio in place and apply `volume`, leaving it
    /// ready to play, full scale being 1.
    pub fn process(&mut self, audio: &mut [f32], volume: f32) {
        if audio.is_empty() {
            return;
        }
        // The per-sample smoothing applied for the whole block at once.
        let per_block = |rate: f32| 1.0 - (1.0 - rate).powi(audio.len() as i32);
        let power = audio.iter().map(|v| v * v).sum::<f32>() / audio.len() as f32;
        let level = self.level.get_or_insert(power);
        *level += per_block(self.smoothing) * (power - *level);
        let mut gain = (TARGET_RMS / level.sqrt().max(1e-9)).clamp(MIN_GAIN, MAX_GAIN) * volume;

        // Turned down at once for a peak that would go over the ceiling,
        // and back up gradually.
        let peak = audio.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let needed = if peak > 0.0 {
            (CEILING / (peak * gain)).min(1.0)
        } else {
            1.0
        };
        if needed < self.limiting {
            self.limiting = needed;
        } else {
            self.limiting += per_block(self.limiter_release) * (1.0 - self.limiting);
        }
        gain *= self.limiting;

        // Glide from the last block's gain to this one's, so it doesn't
        // step, unless that would let a peak through.
        let from = if peak * self.gain.max(gain) > CEILING || self.gain == 0.0 {
            gain
        } else {
            self.gain
        };
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
    /// seconds of normalising at `volume`, 16 samples at a time.
    fn settled_rms(amplitude: f32, volume: f32) -> f32 {
        let mut n = Normalizer::new(RATE);
        let mut last = Vec::new();
        for block in 0..3000 {
            let mut audio: Vec<f32> = (0..16)
                .map(|i| amplitude * (2.0 * std::f32::consts::PI * 1000.0 * (block * 16 + i) as f32 / RATE).sin())
                .collect();
            n.process(&mut audio, volume);
            if block >= 2000 {
                last.extend(audio);
            }
        }
        (last.iter().map(|v| v * v).sum::<f32>() / last.len() as f32).sqrt()
    }

    #[test]
    fn quiet_and_loud_come_out_alike() {
        for amplitude in [0.03, 0.6] {
            let rms = settled_rms(amplitude, 2.0);
            assert!((rms / (2.0 * TARGET_RMS) - 1.0).abs() < 0.05, "came out at {rms}");
        }
    }

    #[test]
    fn loud_volume_is_limited_not_clipped() {
        let rms = settled_rms(0.3, 10.0);
        // A sine whose peaks are at the ceiling.
        assert!((rms - CEILING / 2f32.sqrt()).abs() < 0.02, "came out at {rms}");
    }

    #[test]
    fn peaks_are_limited() {
        let mut n = Normalizer::new(RATE);
        let mut audio = vec![0.01; 16];
        n.process(&mut audio, 3.0);
        let mut audio = vec![0.0; 16];
        audio[5] = 1.0;
        n.process(&mut audio, 3.0);
        assert!(audio.iter().all(|v| v.abs() <= CEILING + 1e-6));
    }
}
