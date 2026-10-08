//! P25 Phase 1 (FDMA) voice receiver for one 12.5 kHz channel: C4FM/CQPSK
//! demodulation, framing, link control and IMBE voice decoding.
//!
//! Every voice frame names its talkgroup, so a receiver that listens to all
//! of a site's frequencies at once never needs to follow the control channel.

use blip25_vocoder::vocoder::{Rate, Vocoder};
use rustfft::num_complex::Complex32;

use crate::level::CarrierLevel;

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};
use std::sync::OnceLock;

/// Samples per symbol; the channel must be sampled at 48 kHz.
const SPS: usize = 10;
/// Samples of decoded audio (8 kHz) per voice frame handed out.
pub const VOICE_FRAME_SAMPLES: usize = 9 * 160;

/// Frame sync, 24 dibits: 1 where the symbol is +3, 0 where it is -3.
const SYNC: u32 = 0b1111_1011_0011_0000_1010_0000;
const SYNC_SYMBOLS: usize = 24;
/// RMS phase error in radians over the sync word below which it is a sync.
const SYNC_MAX_ERROR: f32 = 0.5;
/// Largest carrier offset, in radians per symbol, that still slices cleanly.
const SYNC_MAX_OFFSET: f32 = 0.7;
/// The best alignment is looked for this many samples past the first match.
const SYNC_REFINE: usize = 5;

/// A status symbol follows every 35 dibits of payload.
const STATUS_PERIOD: usize = 36;
/// Dibits, status symbols included, up to the end of the network ID.
const NID_DIBITS: usize = 57;
const LDU_DIBITS: usize = 864;
const TDU_DIBITS: usize = 72;
const TDULC_DIBITS: usize = 216;

const DUID_TDU: u8 = 0x3;
const DUID_LDU1: u8 = 0x5;
const DUID_LDU2: u8 = 0xA;
const DUID_TDULC: u8 = 0xF;

/// BCH(63,16,23) generator polynomial of the network ID.
const NID_GENERATOR: u64 = 0o6331141367235453;
const NID_MAX_ERRORS: u32 = 11;

/// Bit offsets within a voice frame, status symbols removed.
const IMBE_OFFSETS: [usize; 9] = [112, 256, 440, 624, 808, 992, 1176, 1360, 1536];
const WORD_OFFSETS: [usize; 6] = [400, 584, 768, 952, 1136, 1320];
/// Link control format: group voice channel user.
const LCF_GROUP_VOICE: u8 = 0x00;
const ALGID_CLEAR: u8 = 0x80;
/// Voice frames held back while waiting to learn whether a call is
/// encrypted and which talkgroup it is for.
const MAX_PENDING_FRAMES: usize = 3;
/// Samples without a voice frame after which a call is taken to be over.
const CALL_TIMEOUT: usize = 2 * LDU_DIBITS * SPS + 480;

pub enum P25Frame {
    /// 180 ms of decoded speech at 8 kHz, with the network access code it
    /// was sent under and, if known yet, the talkgroup it is addressed to
    /// and the radio it came from.
    Voice {
        nac: u16,
        talkgroup: Option<u16>,
        source: Option<u32>,
        pcm: Vec<i16>,
    },
    /// The transmission ended.
    End,
}

pub struct P25Channel {
    level: CarrierLevel,
    /// The last symbol's worth of input, for the differential demodulator.
    history: [Complex32; SPS],
    /// Phase advance over one symbol, for every sample not yet consumed.
    phase: Vec<f32>,
    /// Next sample in `phase` to test for frame sync.
    search: usize,
    frame: Option<Frame>,
    vocoder: Vocoder,
    nac: u16,
    talkgroup: Option<u16>,
    source: Option<u32>,
    /// Whether the call is unencrypted, once a frame has said so.
    clear: Option<bool>,
    pending: Vec<i16>,
    in_call: bool,
    since_voice: usize,
}

/// A frame whose sync has been found and that is waiting for its samples.
struct Frame {
    start: usize,
    /// Carrier offset in radians per symbol.
    offset: f32,
    duid: Option<u8>,
}

impl Default for P25Channel {
    fn default() -> Self {
        Self::new()
    }
}

impl P25Channel {
    pub fn new() -> Self {
        Self {
            level: CarrierLevel::default(),
            history: [Complex32::ZERO; SPS],
            phase: Vec::new(),
            search: 0,
            frame: None,
            vocoder: Vocoder::new(Rate::FullRate7200x4400),
            nac: 0,
            talkgroup: None,
            source: None,
            clear: None,
            pending: Vec::new(),
            in_call: false,
            since_voice: 0,
        }
    }

    /// Carrier level over the noise floor, in dB.
    pub fn snr_db(&self) -> f32 {
        self.level.snr_db()
    }

    /// Forget everything in progress, as after the receiver was tuned away
    /// and back: the samples that follow don't continue the ones before.
    pub fn resync(&mut self) {
        self.history = [Complex32::ZERO; SPS];
        self.phase.clear();
        self.search = 0;
        self.frame = None;
        self.end_call(&mut |_| {});
    }

    /// Feed 48 kHz baseband; `sink` gets whatever frames complete.
    pub fn process(&mut self, iq: &[Complex32], mut sink: impl FnMut(P25Frame)) {
        self.level.update(iq);
        for (i, &x) in iq.iter().enumerate() {
            // C4FM and CQPSK both move the phase by ±π/4 or ±3π/4 per symbol.
            let then = if i < SPS { self.history[i] } else { iq[i - SPS] };
            self.phase.push((x * then.conj()).arg());
        }
        let n = iq.len().min(SPS);
        self.history.copy_within(n.., 0);
        self.history[SPS - n..].copy_from_slice(&iq[iq.len() - n..]);

        loop {
            let Some(frame) = &mut self.frame else {
                if self.phase.len() < self.search + SYNC_SYMBOLS * SPS + SYNC_REFINE {
                    break;
                }
                if let Some(found) = self.find_sync() {
                    self.frame = Some(found);
                } else {
                    self.search += 1;
                }
                continue;
            };
            if frame.duid.is_none() {
                if self.phase.len() < frame.start + (NID_DIBITS + 1) * SPS {
                    break;
                }
                let (start, offset) = (frame.start, frame.offset);
                let bits = self.bits(start, offset, NID_DIBITS + 1);
                let nid = bits[48..112].iter().fold(0u64, |v, &b| v << 1 | b as u64);
                match decode_nid(nid) {
                    Some((nac, duid)) => {
                        self.nac = nac;
                        self.frame.as_mut().unwrap().duid = Some(duid);
                    }
                    None => {
                        self.search = start + 1;
                        self.frame = None;
                    }
                }
                continue;
            }
            let (start, offset, duid) = (frame.start, frame.offset, frame.duid.unwrap());
            let dibits = match duid {
                DUID_LDU1 | DUID_LDU2 => LDU_DIBITS,
                DUID_TDU => TDU_DIBITS,
                DUID_TDULC => TDULC_DIBITS,
                // Headers, trunking and data: nothing in them we need.
                _ => NID_DIBITS,
            };
            if self.phase.len() < start + dibits * SPS {
                break;
            }
            match duid {
                DUID_LDU1 | DUID_LDU2 => {
                    let bits = self.bits(start, offset, LDU_DIBITS);
                    self.voice(duid, &bits, &mut sink);
                }
                DUID_TDU | DUID_TDULC => self.end_call(&mut sink),
                _ => {}
            }
            // The next frame's sync may sit a sample or two early.
            self.search = start + dibits * SPS - 2 * SPS;
            self.frame = None;
        }

        let done = self.frame.as_ref().map_or(self.search, |f| f.start);
        self.phase.drain(..done);
        self.search -= done;
        if let Some(frame) = &mut self.frame {
            frame.start = 0;
        }

        self.since_voice += iq.len();
        if self.in_call && self.since_voice > CALL_TIMEOUT {
            self.end_call(&mut sink);
        }
    }

    /// Test for frame sync at `self.search`, and settle on the best-aligned
    /// sample nearby.
    fn find_sync(&self) -> Option<Frame> {
        let mut best = (SYNC_MAX_ERROR, self.sync_error(self.search)?.1, self.search);
        for start in self.search..=self.search + SYNC_REFINE {
            if let Some((error, offset)) = self.sync_error(start).filter(|&(e, _)| e < best.0) {
                best = (error, offset, start);
            }
        }
        Some(Frame {
            start: best.2,
            offset: best.1,
            duid: None,
        })
    }

    /// RMS phase error against the sync word starting at `start`, and the
    /// carrier offset that gives it; `None` if this is no sync.
    fn sync_error(&self, start: usize) -> Option<(f32, f32)> {
        let expected = |k: usize| if SYNC >> (SYNC_SYMBOLS - 1 - k) & 1 == 1 { 3.0 } else { -3.0 } * FRAC_PI_4;
        let at = |k: usize| self.phase[start + k * SPS] - expected(k);
        let offset = (0..SYNC_SYMBOLS).map(at).sum::<f32>() / SYNC_SYMBOLS as f32;
        if offset.abs() > SYNC_MAX_OFFSET {
            return None;
        }
        let error = ((0..SYNC_SYMBOLS).map(|k| (at(k) - offset).powi(2)).sum::<f32>() / SYNC_SYMBOLS as f32).sqrt();
        (error < SYNC_MAX_ERROR).then_some((error, offset))
    }

    /// Slice `dibits` symbols from `start` into bits, dropping status symbols.
    fn bits(&self, start: usize, offset: f32, dibits: usize) -> Vec<u8> {
        let mut bits = Vec::with_capacity(dibits * 2);
        for k in (0..dibits).filter(|k| k % STATUS_PERIOD != STATUS_PERIOD - 1) {
            let v = self.phase[start + k * SPS] - offset;
            bits.push((v < 0.0) as u8);
            bits.push((v.abs() > FRAC_PI_2) as u8);
        }
        bits
    }

    fn voice(&mut self, duid: u8, bits: &[u8], sink: &mut impl FnMut(P25Frame)) {
        // 24 six-bit words, each in a Hamming(10,6) codeword, spread through
        // the frame: link control in LDU1, encryption sync in LDU2.
        let mut words = [None; 24];
        for (i, word) in words.iter_mut().enumerate() {
            let at = WORD_OFFSETS[i / 4] + i % 4 * 10;
            *word = hamming_10_6(bits[at..at + 10].iter().fold(0, |v, &b| v << 1 | b as u16));
        }
        let field = |first: usize, count: usize| -> Option<u64> {
            words[first..first + count]
                .iter()
                .try_fold(0u64, |v, w| Some(v << 6 | (*w)? as u64))
        };
        if duid == DUID_LDU1 {
            // Format, manufacturer, options, reserved, talkgroup, source.
            if let Some(lc) = field(0, 8) {
                let (format, mfid) = ((lc >> 40) as u8, (lc >> 32) as u8);
                if format == LCF_GROUP_VOICE && mfid == 0 {
                    self.talkgroup = Some(lc as u16);
                    // The radio's unit ID is the next 24 bits; consoles send 0.
                    self.source = field(8, 4).map(|id| id as u32).filter(|&id| id != 0).or(self.source);
                }
            }
        } else if let Some(es) = field(12, 2) {
            // Message indicator (72 bits), then the algorithm ID.
            self.clear = Some((es >> 4) as u8 == ALGID_CLEAR);
        }

        self.in_call = true;
        self.since_voice = 0;
        if self.clear == Some(false) {
            self.pending.clear();
            return;
        }
        for at in IMBE_OFFSETS {
            let mut packed = [0u8; 18];
            for (i, &b) in bits[at..at + 144].iter().enumerate() {
                packed[i / 8] |= b << (7 - i % 8);
            }
            match self.vocoder.decode_bits(&packed) {
                Ok(pcm) => self.pending.extend_from_slice(&pcm),
                Err(_) => self.pending.extend_from_slice(&[0; 160]),
            }
        }
        let waited = self.pending.len() >= MAX_PENDING_FRAMES * VOICE_FRAME_SAMPLES;
        if self.clear == Some(true) && (self.talkgroup.is_some() || waited) {
            sink(P25Frame::Voice {
                nac: self.nac,
                talkgroup: self.talkgroup,
                source: self.source,
                pcm: std::mem::take(&mut self.pending),
            });
        } else if waited {
            self.pending.drain(..VOICE_FRAME_SAMPLES);
        }
    }

    fn end_call(&mut self, sink: &mut impl FnMut(P25Frame)) {
        if self.in_call {
            sink(P25Frame::End);
        }
        self.in_call = false;
        self.talkgroup = None;
        self.source = None;
        self.clear = None;
        self.pending.clear();
        self.vocoder.reset();
    }
}

fn nid_codeword(info: u16) -> u64 {
    let mut rem = (info as u64) << 47;
    for bit in (47..63).rev() {
        if rem >> bit & 1 == 1 {
            rem ^= NID_GENERATOR << (bit - 47);
        }
    }
    (info as u64) << 47 | rem
}

/// Network access code and data unit ID from a received 64-bit network ID.
fn decode_nid(received: u64) -> Option<(u16, u8)> {
    // The 64th bit is a parity bit that adds little; the BCH code is in the
    // other 63.
    let received = received >> 1;
    let info = (received >> 47) as u16;
    let unpack = |info: u16| Some((info >> 4, (info & 0xF) as u8));
    if nid_codeword(info) == received {
        return unpack(info);
    }
    // With only 65536 codewords, the nearest one can simply be looked for.
    static CODEWORDS: OnceLock<Vec<u64>> = OnceLock::new();
    let codewords = CODEWORDS.get_or_init(|| (0..=u16::MAX).map(nid_codeword).collect());
    let (errors, info) = codewords
        .iter()
        .zip(0..=u16::MAX)
        .map(|(cw, info)| ((cw ^ received).count_ones(), info))
        .min()?;
    (errors <= NID_MAX_ERRORS).then(|| unpack(info)).flatten()
}

/// Decode a Hamming(10,6,3) codeword to its six data bits, correcting one
/// bit error; `None` if it has more.
fn hamming_10_6(word: u16) -> Option<u8> {
    const PARITY: [u16; 6] = [0b1110, 0b1101, 0b1011, 0b0111, 0b0011, 0b1100];
    let data = (word >> 4) as u8;
    let syndrome = (0..6)
        .filter(|k| data >> (5 - k) & 1 == 1)
        .fold(word & 0xF, |s, k| s ^ PARITY[k]);
    if syndrome == 0 || syndrome.is_power_of_two() {
        return Some(data);
    }
    let k = PARITY.iter().position(|&p| p == syndrome)?;
    Some(data ^ 1 << (5 - k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nid_matches_on_air_capture() {
        // Received from the San Mateo County simulcast site: NAC 38D, LDU1.
        let on_air = 0x38d5_85f1_436c_a54f;
        assert_eq!(nid_codeword(0x38d5), on_air >> 1);
        assert_eq!(decode_nid(on_air), Some((0x38d, DUID_LDU1)));
        // Eleven bit errors, some of them in the NAC and DUID themselves.
        assert_eq!(decode_nid(on_air ^ 0x1410_0408_8011_2240), Some((0x38d, DUID_LDU1)));
        assert_eq!(decode_nid(on_air ^ 0xffff_ff00_0000_0000), None);
    }

    #[test]
    fn hamming_corrects_one_error() {
        for data in 0..64u16 {
            let parity = (0..6)
                .filter(|k| data >> (5 - k) & 1 == 1)
                .fold(0, |s, k| s ^ [0b1110, 0b1101, 0b1011, 0b0111, 0b0011, 0b1100][k]);
            let word = data << 4 | parity;
            assert_eq!(hamming_10_6(word), Some(data as u8));
            for bit in 0..10 {
                assert_eq!(hamming_10_6(word ^ 1 << bit), Some(data as u8));
            }
        }
    }

    #[test]
    fn sync_word_matches_standard() {
        // 0x5575F5FF77FF as dibits: 01 is +3, 11 is -3.
        let mut sync = 0u32;
        for k in 0..SYNC_SYMBOLS {
            let dibit = 0x5575_F5FF_77FFu64 >> (46 - 2 * k) & 3;
            sync = sync << 1 | (dibit == 1) as u32;
        }
        assert_eq!(sync, SYNC);
    }
}
