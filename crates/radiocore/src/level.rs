//! Carrier level of a channel against its own tracked noise floor.

use std::collections::VecDeque;

use rustfft::num_complex::Complex32;

/// Carrier power is averaged over this many blocks: long enough to be steady
/// on noise, short enough to follow a carrier within a few milliseconds.
const POWER_BLOCKS: usize = 8;
/// Noise floor estimate creeps up by about 1 dB per 10 s of blocks.
const FLOOR_RISE: f32 = 1.000_02;
const WARMUP_BLOCKS: u32 = 200;

pub struct CarrierLevel {
    recent: VecDeque<f32>,
    power: f32,
    floor: f32,
    warmup: u32,
}

impl Default for CarrierLevel {
    fn default() -> Self {
        Self {
            recent: VecDeque::from(vec![0.0; POWER_BLOCKS]),
            power: 0.0,
            floor: f32::INFINITY,
            warmup: WARMUP_BLOCKS,
        }
    }
}

impl CarrierLevel {
    /// Measure one block. Returns false while still warming up, when there
    /// is no floor to compare against yet.
    pub fn update(&mut self, iq: &[Complex32]) -> bool {
        let p = iq.iter().map(|v| v.norm_sqr()).sum::<f32>() / iq.len() as f32;
        self.recent.pop_front();
        self.recent.push_back(p);
        self.power = self.recent.iter().sum::<f32>() / POWER_BLOCKS as f32;
        if self.warmup > 0 {
            self.warmup -= 1;
            if self.warmup == 0 {
                self.floor = self.power;
            }
            return false;
        }
        self.floor = (self.floor * FLOOR_RISE).min(self.power);
        true
    }

    pub fn power(&self) -> f32 {
        self.power
    }

    /// Noise floor estimate (infinite until warmed up).
    pub fn floor(&self) -> f32 {
        self.floor
    }

    /// Carrier-to-noise-floor ratio in dB (0 until warmed up).
    pub fn snr_db(&self) -> f32 {
        if self.floor.is_finite() {
            10.0 * (self.power / self.floor).log10()
        } else {
            0.0
        }
    }
}
