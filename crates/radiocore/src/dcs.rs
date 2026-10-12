//! Digital-coded squelch (DCS, also sold as DPL): a 23-bit word repeated
//! under the voice at 134.4 bits a second, by which radios sharing a
//! frequency tell their own group's transmissions from others'.

use std::f32::consts::PI;

use crate::biquad::Biquad;

const BIT_RATE: f32 = 134.4;
const WORD_BITS: u32 = 23;
const WORD_MASK: u32 = (1 << WORD_BITS) - 1;
/// How hard a change of sign pulls the bit clock towards itself.
const CLOCK_GAIN: f32 = 0.15;
/// How far past zero, in Hz of deviation, the signal must go to count as
/// having changed sign: what is left of the voice mustn't.
const SWING_HZ: f32 = 80.0;
/// The code is there once it has been read this many bits running (two
/// whole words, so two readings that share no bits), and gone once it
/// hasn't been read for this many.
const OPEN_BITS: u32 = WORD_BITS;
const CLOSE_BITS: u32 = 2 * WORD_BITS;

/// The word sent for a code: its nine bits, three fixed ones, and eleven
/// check bits from the (23, 12) Golay code. Sent lowest bit first.
pub fn codeword(code: u16) -> u32 {
    let data = 0x800 | u32::from(code & 0x1ff);
    let mut check = data;
    for _ in 0..12 {
        if check & 1 != 0 {
            check ^= 0xc75;
        }
        check >>= 1;
    }
    (check << 12) | data
}

/// Listens for one code in a channel's demodulated signal.
pub struct Dcs {
    /// The word as it sits in the register at each of the 23 places the
    /// reading can have started, the right way up and inverted: which way
    /// up it arrives depends on the transmitter and the receiver both.
    words: Vec<u32>,
    /// Slow average of the deviation: a carrier off frequency.
    dc: f32,
    dc_a: f32,
    lowpass: [Biquad; 2],
    /// Where in the current bit the clock is, from 0 to 1.
    phase: f32,
    step: f32,
    positive: bool,
    sum: f32,
    register: u32,
    /// Bits in a row at which the register held the word, and bits since
    /// it last did.
    read: u32,
    unread: u32,
    present: bool,
}

impl Dcs {
    /// A listener for `code` (its three octal digits as a number: 0o023)
    /// in a signal sampled at `sample_rate`.
    pub fn new(sample_rate: f32, code: u16) -> Self {
        let word = codeword(code);
        let rotations = (0..WORD_BITS).map(|n| ((word >> n) | (word << (WORD_BITS - n))) & WORD_MASK);
        Self {
            words: rotations.flat_map(|w| [w, !w & WORD_MASK]).collect(),
            dc: 0.0,
            dc_a: 1.0 - (-2.0 * PI * 2.0 / sample_rate).exp(),
            // Fourth order: the code is all below 70 Hz, the voice all above 300.
            lowpass: crate::biquad::BUTTERWORTH4_Q.map(|q| Biquad::lowpass(sample_rate, 200.0, q)),
            phase: 0.0,
            step: BIT_RATE / sample_rate,
            positive: false,
            sum: 0.0,
            register: 0,
            read: 0,
            unread: CLOSE_BITS,
            present: false,
        }
    }

    /// Forget what has been read: what follows doesn't continue it.
    pub fn resync(&mut self) {
        (self.read, self.unread, self.present) = (0, CLOSE_BITS, false);
        (self.register, self.sum) = (0, 0.0);
    }

    /// Whether the code is being received.
    pub fn present(&self) -> bool {
        self.present
    }

    /// Take the next sample of the signal's deviation, in Hz.
    pub fn step(&mut self, deviation: f32) {
        self.dc += self.dc_a * (deviation - self.dc);
        let x = self
            .lowpass
            .iter_mut()
            .fold(deviation - self.dc, |x, stage| stage.step(x));
        // Bits change at the clock's zero: a change seen elsewhere means
        // the clock is early or late, and is nudged.
        if (self.positive && x < -SWING_HZ) || (!self.positive && x > SWING_HZ) {
            self.positive = !self.positive;
            self.phase -= CLOCK_GAIN * if self.phase < 0.5 { self.phase } else { self.phase - 1.0 };
        }
        self.sum += x;
        self.phase += self.step;
        if self.phase < 1.0 {
            return;
        }
        self.phase -= 1.0;
        let bit = u32::from(self.sum > 0.0);
        self.sum = 0.0;
        self.register = (self.register >> 1) | (bit << (WORD_BITS - 1));
        // One bit may be wrong; the code can put right more, but the more
        // that is forgiven, the more often noise passes.
        let register = self.register;
        if self.words.iter().any(|w| (w ^ register).count_ones() <= 1) {
            self.read += 1;
            self.unread = 0;
        } else {
            self.read = 0;
            self.unread += 1;
        }
        self.present = if self.present {
            self.unread < CLOSE_BITS
        } else {
            self.read >= OPEN_BITS
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f32 = 48_000.0;

    /// Deviation of a transmission carrying `code` under a 1 kHz tone.
    fn signal(code: u16, inverted: bool, seconds: f32) -> Vec<f32> {
        let word = codeword(code);
        (0..(seconds * FS) as usize)
            .map(|n| {
                let t = n as f32 / FS;
                let bit = (word >> ((t * BIT_RATE) as u32 % WORD_BITS)) & 1 == 1;
                let level = if bit != inverted { 600.0 } else { -600.0 };
                level + 3000.0 * (2.0 * PI * 1000.0 * t).sin() + 150.0
            })
            .collect()
    }

    fn hears(listening_for: u16, signal: &[f32]) -> bool {
        let mut dcs = Dcs::new(FS, listening_for);
        signal.iter().for_each(|&x| dcs.step(x));
        dcs.present()
    }

    #[test]
    fn makes_the_published_codewords() {
        // From the tables radios are programmed from.
        assert_eq!(codeword(0o023), 0x763813);
        assert_eq!(codeword(0o023) & 0xfff, 0x813);
    }

    #[test]
    fn hears_its_own_code_only() {
        let sent = signal(0o023, false, 1.5);
        assert!(hears(0o023, &sent));
        assert!(!hears(0o025, &sent));
        assert!(!hears(0o624, &sent));
        // Either way up.
        assert!(hears(0o023, &signal(0o023, true, 1.5)));
        // A tone with no code under it, and silence.
        let plain: Vec<f32> = (0..72_000)
            .map(|n| 3000.0 * (2.0 * PI * 1000.0 * n as f32 / FS).sin())
            .collect();
        assert!(!hears(0o023, &plain));
        assert!(!hears(0o023, &vec![0.0; 72_000]));
    }

    #[test]
    fn opens_within_half_a_second_and_closes_after() {
        let mut dcs = Dcs::new(FS, 0o624);
        let sent = signal(0o624, false, 1.0);
        let opened = sent.iter().position(|&x| {
            dcs.step(x);
            dcs.present()
        });
        assert!(opened.is_some_and(|n| n < (0.5 * FS) as usize), "opened at {opened:?}");
        // The code stops; the voice goes on.
        let closed = (0..FS as usize).position(|n| {
            dcs.step(3000.0 * (2.0 * PI * 1000.0 * n as f32 / FS).sin());
            !dcs.present()
        });
        assert!(closed.is_some_and(|n| n < (0.5 * FS) as usize), "closed at {closed:?}");
    }
}
