//! P25 Phase 2 (TDMA) voice receiver for one 12.5 kHz channel.
//!
//! A Phase 2 channel carries two conversations, taking turns in 30 ms slots.
//! Twelve slots make a superframe: ten carry voice or signalling for the two
//! conversations alternately, and the last two carry slower signalling. Each
//! slot is 20 dibits that mark time (a sync pattern before six slots of the
//! twelve, information before the others) and a burst of 160.
//!
//! Voice bursts hold four frames of half-rate speech ("4V"), or two and the
//! rest of the encryption details ("2V"). Everything in a burst except the
//! marks saying what kind it is is scrambled with a sequence made from the
//! network, system and site (see [`P25p2Channel::set_identity`]), which the
//! system's Phase 1 control channel announces; without them nothing can be
//! read. Where in the superframe a slot is, which the scrambling depends on,
//! is found by trying each place and seeing which lets the speech's own
//! error correction succeed.
//!
//! Worked out and checked against recordings of a live system.

use blip25_vocoder::halfrate::frame::decode_frame;
use blip25_vocoder::vocoder::{Rate, Vocoder};
use rustfft::num_complex::Complex32;

use crate::level::CarrierLevel;

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};

/// Samples per symbol; the channel must be sampled at 48 kHz.
const SPS: usize = 8;
/// Dibits in a slot: the 20 that mark time, then the burst.
const SLOT_DIBITS: usize = 180;
const MARK_DIBITS: usize = 20;
const SLOTS: usize = 12;
/// The sync pattern, as 20 dibits.
const SYNC: u64 = 0x57_5D57_F7FF;
/// RMS phase error, in radians, under which a sync pattern is one.
const SYNC_MAX_ERROR: f32 = 0.4;
const SYNC_MAX_OFFSET: f32 = 0.6;
/// How far either side of where a slot should start its sync is looked for.
const SYNC_REFINE: usize = 3;
/// Slots without a sync pattern after which the signal is taken to be gone.
/// One comes at least every third slot.
const LOST_SLOTS: usize = 24;
/// Slots without voice after which a conversation is taken to be over.
const CALL_TIMEOUT_SLOTS: usize = 2 * SLOTS;

/// Bursts of signalling in a row, where a conversation's speech would be,
/// that mean the transmission is over.
const TRANSMISSION_OVER_BURSTS: u32 = 3;

/// Where in a burst its kind is marked: four dibits, two bits each.
const DUID_AT: [usize; 4] = [0, 37, 122, 159];
/// Where a burst's voice frames start; a 2V burst has only the first two.
const VOICE_AT: [usize; 4] = [1, 38, 86, 123];
const VOICE_DIBITS: usize = 36;
/// Where a 4V burst carries its part of the encryption details: 24 bits.
const ESS_AT: usize = 74;
/// The scrambling sequence runs from this many dibits before each burst.
const SCRAMBLE_LEAD: usize = 10;
const ALGID_CLEAR: u8 = 0x80;

/// The most bit errors, over a 4V burst's four voice frames, with which a
/// place in the superframe is believed; a wrong place gives hundreds.
const PLACED_MAX_ERRORS: u32 = 24;
/// 4V bursts in a row too garbled to believe before the place is looked for
/// again.
const MISPLACED_BURSTS: u32 = 6;
/// Speech held back, in frames, while waiting to learn whether a
/// conversation is encrypted: the details come once every 18 frames.
const MAX_PENDING_FRAMES: usize = 20;
const FRAME_SAMPLES: usize = 160;

/// What kind of burst a slot carries.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Burst {
    Voice4,
    Voice2,
    /// Signalling, or nothing.
    Other,
}

/// The kind of burst marked by these eight bits, forgiving one wrong bit.
fn burst_kind(marks: u8) -> Burst {
    // The marks are a code whose words differ in at least four bits.
    const VOICE4: u8 = 0x00;
    const VOICE2: u8 = 0x65;
    if (marks ^ VOICE4).count_ones() <= 1 {
        Burst::Voice4
    } else if (marks ^ VOICE2).count_ones() <= 1 {
        Burst::Voice2
    } else {
        Burst::Other
    }
}

/// The scrambling sequence for a superframe, as dibits.
fn scrambling(wacn: u32, system: u16, nac: u16) -> Vec<u8> {
    const MASK: u64 = (1 << 44) - 1;
    let mut register = (u64::from(wacn) << 24 | u64::from(system) << 12 | u64::from(nac)) & MASK;
    if register == 0 {
        register = MASK;
    }
    let mut bit = || {
        let out = (register >> 43) as u8 & 1;
        let feedback =
            (register >> 43 ^ register >> 33 ^ register >> 19 ^ register >> 14 ^ register >> 8 ^ register >> 3) & 1;
        register = (register << 1 | feedback) & MASK;
        out
    };
    (0..SLOTS * SLOT_DIBITS).map(|_| bit() << 1 | bit()).collect()
}

pub enum P25p2Frame {
    /// Decoded speech at 8 kHz from one of the channel's two conversations.
    Voice { slot: u8, pcm: Vec<i16> },
    /// That conversation ended.
    End { slot: u8 },
}

/// One of the two conversations a channel carries.
struct Conversation {
    vocoder: Vocoder,
    /// Whether it is unencrypted, once a burst has said.
    clear: Option<bool>,
    /// The next 4V burst starts the encryption details, which begin with
    /// the algorithm: it follows a 2V burst.
    details_next: bool,
    pending: Vec<i16>,
    in_call: bool,
    slots_since_voice: usize,
    /// Bursts of signalling in a row where its speech would be.
    signalling_bursts: u32,
}

impl Conversation {
    fn new() -> Self {
        Self {
            vocoder: Vocoder::new(Rate::HalfRate3600x2450),
            clear: None,
            details_next: false,
            pending: Vec::new(),
            in_call: false,
            slots_since_voice: 0,
            signalling_bursts: 0,
        }
    }

    fn end(&mut self, slot: u8, sink: &mut impl FnMut(P25p2Frame)) {
        if self.in_call {
            sink(P25p2Frame::End { slot });
        }
        self.in_call = false;
        self.clear = None;
        self.details_next = false;
        self.pending.clear();
        self.vocoder.reset();
    }
}

/// Slot timing that has been found.
struct Timing {
    /// Where in `phase` the next slot starts.
    start: usize,
    /// Carrier offset in radians per symbol.
    offset: f32,
    /// Slots since timing was found.
    count: usize,
    slots_since_sync: usize,
}

pub struct P25p2Channel {
    level: CarrierLevel,
    /// The last symbol's worth of input, for the differential demodulator.
    history: [Complex32; SPS],
    /// Phase advance over one symbol, for every sample not yet consumed.
    phase: Vec<f32>,
    /// Next sample in `phase` to test for sync, while there is no timing.
    search: usize,
    timing: Option<Timing>,
    /// The scrambling sequence, once the system's identity is known.
    scrambling: Option<Vec<u8>>,
    identity: Option<(u32, u16, u16)>,
    /// What to add to a slot's count to get its place in the superframe.
    place: Option<usize>,
    misplaced: u32,
    conversations: [Conversation; 2],
}

impl Default for P25p2Channel {
    fn default() -> Self {
        Self::new()
    }
}

impl P25p2Channel {
    pub fn new() -> Self {
        Self {
            level: CarrierLevel::default(),
            history: [Complex32::ZERO; SPS],
            phase: Vec::new(),
            search: 0,
            timing: None,
            scrambling: None,
            identity: None,
            place: None,
            misplaced: 0,
            conversations: [Conversation::new(), Conversation::new()],
        }
    }

    /// Say which network (WACN), system and site (its network access code)
    /// the channel belongs to: what its traffic is scrambled with.
    pub fn set_identity(&mut self, wacn: u32, system: u16, nac: u16) {
        if self.identity != Some((wacn, system, nac)) {
            self.identity = Some((wacn, system, nac));
            self.scrambling = Some(scrambling(wacn, system, nac));
            self.place = None;
        }
    }

    /// Carrier level over the noise floor, in dB.
    pub fn snr_db(&self) -> f32 {
        self.level.snr_db()
    }

    /// Whether a Phase 2 signal is being received.
    pub fn locked(&self) -> bool {
        self.timing.is_some()
    }

    /// Forget everything in progress, as after the receiver was tuned away
    /// and back.
    pub fn resync(&mut self) {
        self.history = [Complex32::ZERO; SPS];
        self.phase.clear();
        self.search = 0;
        self.lose_timing(&mut |_| {});
    }

    fn lose_timing(&mut self, sink: &mut impl FnMut(P25p2Frame)) {
        self.timing = None;
        self.place = None;
        self.misplaced = 0;
        for (slot, conversation) in self.conversations.iter_mut().enumerate() {
            conversation.end(slot as u8, sink);
        }
    }

    /// Feed 48 kHz baseband; `sink` gets whatever speech is decoded.
    pub fn process(&mut self, iq: &[Complex32], mut sink: impl FnMut(P25p2Frame)) {
        self.level.update(iq);
        for (i, &x) in iq.iter().enumerate() {
            let then = if i < SPS { self.history[i] } else { iq[i - SPS] };
            self.phase.push((x * then.conj()).arg());
        }
        let n = iq.len().min(SPS);
        self.history.copy_within(n.., 0);
        self.history[SPS - n..].copy_from_slice(&iq[iq.len() - n..]);

        loop {
            let Some(timing) = &mut self.timing else {
                if self.phase.len() < self.search + MARK_DIBITS * SPS + SYNC_REFINE {
                    break;
                }
                match self.sync_near(self.search, 0, SYNC_REFINE) {
                    Some((start, offset)) => {
                        self.timing = Some(Timing {
                            start,
                            offset,
                            count: 0,
                            slots_since_sync: 0,
                        });
                    }
                    None => self.search += 1,
                }
                continue;
            };
            if self.phase.len() < timing.start + SLOT_DIBITS * SPS + SYNC_REFINE {
                break;
            }
            // Slots follow one another exactly; where one starts with the
            // sync pattern, the timing is trued up on it.
            let (start, count) = (timing.start, timing.count);
            let mut offset = timing.offset;
            let from = start.saturating_sub(SYNC_REFINE);
            match self.sync_near(from, start - from, start - from + SYNC_REFINE) {
                Some((found, found_offset)) => {
                    let timing = self.timing.as_mut().unwrap();
                    (timing.start, timing.offset, timing.slots_since_sync) = (found, found_offset, 0);
                    offset = found_offset;
                }
                None => {
                    let timing = self.timing.as_mut().unwrap();
                    timing.slots_since_sync += 1;
                    if timing.slots_since_sync > LOST_SLOTS {
                        self.search = start;
                        self.lose_timing(&mut sink);
                        continue;
                    }
                }
            }
            let start = self.timing.as_ref().unwrap().start;
            let dibits = self.dibits(start, offset);
            self.slot(count, &dibits[MARK_DIBITS..], &mut sink);
            let timing = self.timing.as_mut().unwrap();
            timing.start = start + SLOT_DIBITS * SPS;
            timing.count += 1;
        }

        let done = self
            .timing
            .as_ref()
            .map_or(self.search, |t| t.start.saturating_sub(SYNC_REFINE));
        let done = done.min(self.phase.len());
        self.phase.drain(..done);
        self.search = self.search.saturating_sub(done);
        if let Some(timing) = &mut self.timing {
            timing.start -= done;
        }
    }

    /// The best-aligned sync pattern starting within `before` samples before
    /// to `after` samples after `at + before`, and its carrier offset.
    fn sync_near(&self, at: usize, before: usize, after: usize) -> Option<(usize, f32)> {
        let _ = before;
        (at..=at + after)
            .filter_map(|start| self.sync_error(start).map(|(error, offset)| (error, start, offset)))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, start, offset)| (start, offset))
    }

    /// RMS phase error against the sync pattern starting at `start`, and the
    /// carrier offset that gives it; `None` if this is no sync.
    fn sync_error(&self, start: usize) -> Option<(f32, f32)> {
        if start + (MARK_DIBITS - 1) * SPS >= self.phase.len() {
            return None;
        }
        let expected = |k: usize| [1.0, 3.0, -1.0, -3.0][(SYNC >> (38 - 2 * k) & 3) as usize] * FRAC_PI_4;
        let at = |k: usize| wrap(self.phase[start + k * SPS] - expected(k));
        let offset = (0..MARK_DIBITS).map(at).sum::<f32>() / MARK_DIBITS as f32;
        if offset.abs() > SYNC_MAX_OFFSET {
            return None;
        }
        let error = ((0..MARK_DIBITS).map(|k| wrap(at(k) - offset).powi(2)).sum::<f32>() / MARK_DIBITS as f32).sqrt();
        (error < SYNC_MAX_ERROR).then_some((error, offset))
    }

    /// Slice a slot's symbols into dibits.
    fn dibits(&self, start: usize, offset: f32) -> [u8; SLOT_DIBITS] {
        let mut dibits = [0u8; SLOT_DIBITS];
        for (k, dibit) in dibits.iter_mut().enumerate() {
            let v = wrap(self.phase[start + k * SPS] - offset);
            *dibit = u8::from(v < 0.0) << 1 | u8::from(v.abs() > FRAC_PI_2);
        }
        dibits
    }

    /// A burst's voice frame `which`, unscrambled as if the burst were at
    /// `place` in the superframe.
    fn voice_frame(scrambling: &[u8], burst: &[u8], place: usize, which: usize) -> [u8; VOICE_DIBITS] {
        let mut frame = [0u8; VOICE_DIBITS];
        let at = VOICE_AT[which];
        for (j, dibit) in frame.iter_mut().enumerate() {
            *dibit = burst[at + j] ^ scrambling[(place * SLOT_DIBITS + SCRAMBLE_LEAD + at + j) % scrambling.len()];
        }
        frame
    }

    /// Deal with the burst of the `count`th slot since timing was found.
    fn slot(&mut self, count: usize, burst: &[u8], sink: &mut impl FnMut(P25p2Frame)) {
        for (slot, conversation) in self.conversations.iter_mut().enumerate() {
            conversation.slots_since_voice += 1;
            if conversation.in_call && conversation.slots_since_voice > CALL_TIMEOUT_SLOTS {
                conversation.end(slot as u8, sink);
            }
        }
        let Some(scrambling) = &self.scrambling else { return };
        let marks = DUID_AT.iter().fold(0u8, |marks, &at| marks << 2 | burst[at]);
        let kind = burst_kind(marks);
        if kind == Burst::Other {
            // Signalling where a conversation's speech would be: between
            // transmissions, bar the odd burst borrowed during one. What
            // was known of the last transmission says nothing of the next,
            // least of all whether it is encrypted.
            if let Some(place) = self
                .place
                .map(|shift| (count + shift) % SLOTS)
                .filter(|&place| place < 10)
            {
                let conversation = &mut self.conversations[place % 2];
                conversation.signalling_bursts += 1;
                if conversation.signalling_bursts == TRANSMISSION_OVER_BURSTS {
                    conversation.end((place % 2) as u8, sink);
                }
            }
            return;
        }
        let errors = |place: usize, frames: usize| -> u32 {
            (0..frames)
                .map(|which| u32::from(decode_frame(&Self::voice_frame(scrambling, burst, place, which)).error_total()))
                .sum()
        };

        // Find where in the superframe the slots are, from a 4V burst: only
        // at the right place does its speech come out nearly free of errors.
        if self.place.is_none() && kind == Burst::Voice4 {
            let best = (0..SLOTS).map(|place| (errors(place, 4), place)).min();
            if let Some((found, place)) = best.filter(|best| best.0 <= PLACED_MAX_ERRORS) {
                let _ = found;
                self.place = Some((place + SLOTS - count % SLOTS) % SLOTS);
                self.misplaced = 0;
            }
        }
        let Some(place) = self.place.map(|shift| (count + shift) % SLOTS) else {
            return;
        };
        // The last two slots are signalling's; voice never comes in them.
        if place >= 10 {
            return;
        }
        if kind == Burst::Voice4 {
            // Timing that has slipped a slot shows as speech that is all errors.
            if errors(place, 4) > PLACED_MAX_ERRORS * 2 {
                self.misplaced += 1;
                if self.misplaced >= MISPLACED_BURSTS {
                    self.place = None;
                }
                return;
            }
            self.misplaced = 0;
        }

        let slot = place % 2;
        let conversation = &mut self.conversations[slot];
        conversation.in_call = true;
        conversation.slots_since_voice = 0;
        conversation.signalling_bursts = 0;
        if kind == Burst::Voice4 && conversation.details_next {
            // The first eight bits of the encryption details: the algorithm.
            let algid = (0..4).fold(0u8, |v, j| {
                let at = ESS_AT + j;
                v << 2 | (burst[at] ^ scrambling[(place * SLOT_DIBITS + SCRAMBLE_LEAD + at) % scrambling.len()])
            });
            // Exactly: the commonest ciphers' numbers are one bit from it.
            conversation.clear = Some(algid == ALGID_CLEAR);
        }
        conversation.details_next = kind == Burst::Voice2;

        if conversation.clear == Some(false) {
            conversation.pending.clear();
            return;
        }
        let frames = if kind == Burst::Voice4 { 4 } else { 2 };
        for which in 0..frames {
            let frame = Self::voice_frame(scrambling, burst, place, which);
            let mut packed = [0u8; 9];
            for (j, dibit) in frame.iter().enumerate() {
                packed[j / 4] |= dibit << (6 - 2 * (j % 4));
            }
            match conversation.vocoder.decode_bits(&packed) {
                Ok(pcm) => conversation.pending.extend_from_slice(&pcm),
                Err(_) => conversation.pending.extend_from_slice(&[0; FRAME_SAMPLES]),
            }
        }
        if conversation.clear == Some(true) {
            sink(P25p2Frame::Voice {
                slot: slot as u8,
                pcm: std::mem::take(&mut conversation.pending),
            });
        } else if conversation.pending.len() > MAX_PENDING_FRAMES * FRAME_SAMPLES {
            // Still not known to be in the clear: the oldest is let go.
            let excess = conversation.pending.len() - MAX_PENDING_FRAMES * FRAME_SAMPLES;
            conversation.pending.drain(..excess);
        }
    }
}

/// An angle brought into -π to π.
fn wrap(angle: f32) -> f32 {
    (angle + PI).rem_euclid(TAU) - PI
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_the_kinds_of_burst() {
        assert_eq!(burst_kind(0x00), Burst::Voice4);
        assert_eq!(burst_kind(0x04), Burst::Voice4);
        assert_eq!(burst_kind(0x65), Burst::Voice2);
        assert_eq!(burst_kind(0x61), Burst::Voice2);
        // Signalling: with and without scrambling, on either kind of channel.
        for marks in [0x39, 0x9a, 0xc6, 0xff] {
            assert_eq!(burst_kind(marks), Burst::Other);
        }
    }

    /// Superframes of two conversations, as a transmitter would send them.
    fn transmit(superframes: usize, identity: (u32, u16, u16), algid: u8) -> Vec<Complex32> {
        use blip25_vocoder::halfrate::frame::encode_frame;
        let scrambling = scrambling(identity.0, identity.1, identity.2);
        let speech = encode_frame(&[0x123, 0x456, 0x12, 0x345]);
        let mut dibits = vec![0u8; 50];
        for _ in 0..superframes {
            for place in 0..SLOTS {
                dibits.extend((0..MARK_DIBITS).map(|k| (SYNC >> (38 - 2 * k) & 3) as u8));
                let mut burst = [0u8; SLOT_DIBITS - MARK_DIBITS];
                // Four bursts of 4V then one of 2V for each conversation,
                // and two slots of signalling.
                let (marks, frames) = match place {
                    0..8 => (0x00u8, 4),
                    8..10 => (0x65, 2),
                    _ => (0x39, 0),
                };
                for &at in &VOICE_AT[..frames] {
                    burst[at..at + VOICE_DIBITS].copy_from_slice(&speech);
                }
                if place < 2 {
                    for j in 0..4 {
                        burst[ESS_AT + j] = algid >> (6 - 2 * j) & 3;
                    }
                }
                for (at, dibit) in burst.iter_mut().enumerate() {
                    *dibit ^= scrambling[place * SLOT_DIBITS + SCRAMBLE_LEAD + at];
                }
                for (j, &at) in DUID_AT.iter().enumerate() {
                    burst[at] = marks >> (6 - 2 * j) & 3;
                }
                dibits.extend(burst);
            }
        }
        dibits.extend([0u8; 50]);
        let mut phase = 0.0f32;
        let mut iq = Vec::new();
        for dibit in dibits {
            let step = [1.0, 3.0, -1.0, -3.0][usize::from(dibit)] * FRAC_PI_4 / SPS as f32;
            for _ in 0..SPS {
                phase += step;
                iq.push(Complex32::from_polar(0.5, phase));
            }
        }
        iq
    }

    /// Bursts of speech received from each of the two conversations.
    fn receive(iq: &[Complex32], identity: (u32, u16, u16)) -> [usize; 2] {
        let mut channel = P25p2Channel::new();
        channel.set_identity(identity.0, identity.1, identity.2);
        let mut heard = [0; 2];
        for block in iq.chunks(48) {
            channel.process(block, |frame| {
                if let P25p2Frame::Voice { slot, pcm } = frame {
                    assert_eq!(pcm.len() % FRAME_SAMPLES, 0);
                    heard[usize::from(slot)] += 1;
                }
            });
        }
        heard
    }

    const IDENTITY: (u32, u16, u16) = (0x92762, 0x338, 0x028);

    #[test]
    fn hears_both_conversations() {
        let iq = transmit(4, IDENTITY, ALGID_CLEAR);
        // Nothing is played until a conversation has said it is in the
        // clear, a superframe in; what was held back comes with it.
        let heard = receive(&iq, IDENTITY);
        assert!(heard[0] >= 14 && heard[1] >= 14, "{heard:?}");
    }

    #[test]
    fn stays_silent_on_encrypted_calls_and_other_systems() {
        // AES.
        assert_eq!(receive(&transmit(4, IDENTITY, 0x84), IDENTITY), [0, 0]);
        // Another site's scrambling.
        assert_eq!(
            receive(&transmit(4, IDENTITY, ALGID_CLEAR), (0x92762, 0x338, 0x029)),
            [0, 0]
        );
    }

    #[test]
    fn scrambling_is_made_from_the_identity() {
        let a = scrambling(0x92762, 0x338, 0x028);
        assert_eq!(a.len(), 2160);
        // The register's own 44 bits come out first.
        let seed = 0x92762u64 << 24 | 0x338 << 12 | 0x028;
        let first: u64 = a[..22].iter().fold(0, |v, &dibit| v << 2 | u64::from(dibit));
        assert_eq!(first, seed);
        assert_ne!(a, scrambling(0x92762, 0x338, 0x029));
        // No identity at all still scrambles.
        assert!(scrambling(0, 0, 0).iter().any(|&dibit| dibit != 0));
    }
}
