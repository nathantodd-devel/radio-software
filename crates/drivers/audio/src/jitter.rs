//! What sits between the scanner, which hands over audio in bursts as the
//! radio delivers samples, and an audio device, which takes it steadily and
//! to a clock of its own.
//!
//! Played straight through, each gap between bursts would be a gap in the
//! sound. So a little audio is kept in hand: playing starts once there is
//! enough to ride out the gaps, and the speed is nudged, by too little to
//! hear, to keep about that much waiting however the two clocks differ.

use std::collections::VecDeque;

/// How much audio to keep waiting, in seconds: longer than the bursts it
/// arrives in (tens of milliseconds from a radio at a low sample rate) and
/// the hiccups of a busy phone, short enough not to be noticed as a delay.
const HELD_SECS: f32 = 0.15;
/// With this many times as much waiting, the device has stalled: the oldest
/// is dropped rather than played late for good.
const MOST_HELD: usize = 4;
/// The most the speed is changed to keep the right amount waiting.
const MOST_NUDGE: f32 = 0.01;

pub struct Jitter {
    queue: VecDeque<i16>,
    /// Samples to have waiting.
    held: usize,
    /// Whether audio is being played, rather than waited for.
    playing: bool,
    /// Input samples per output sample.
    step: f32,
    /// Position between `prev` and `next`, 0 to 1.
    pos: f32,
    prev: f32,
    next: f32,
}

impl Jitter {
    /// For audio arriving at `rate` samples a second and leaving at
    /// `device_rate`.
    pub fn new(rate: u32, device_rate: u32) -> Self {
        Self {
            queue: VecDeque::new(),
            held: (rate as f32 * HELD_SECS) as usize,
            playing: false,
            step: rate as f32 / device_rate as f32,
            pos: 0.0,
            prev: 0.0,
            next: 0.0,
        }
    }

    /// Take audio from the scanner.
    pub fn push(&mut self, pcm: &[i16]) {
        self.queue.extend(pcm);
        if self.queue.len() > self.held * MOST_HELD {
            self.queue.drain(..self.queue.len() - self.held);
        }
    }

    /// Fill one device buffer of interleaved frames, every channel alike.
    pub fn fill<T: Copy>(&mut self, data: &mut [T], channels: usize, convert: impl Fn(f32) -> T) {
        if !self.playing && self.queue.len() >= self.held {
            self.playing = true;
        }
        // A little faster with more than enough waiting, slower with less.
        let surplus = (self.queue.len() as f32 - self.held as f32) / self.held as f32;
        let step = self.step * (1.0 + (surplus * MOST_NUDGE).clamp(-MOST_NUDGE, MOST_NUDGE));
        for frame in data.chunks_mut(channels) {
            if self.playing {
                self.pos += step;
                while self.pos >= 1.0 {
                    self.pos -= 1.0;
                    self.prev = self.next;
                    match self.queue.pop_front() {
                        Some(sample) => self.next = f32::from(sample) / 32768.0,
                        // Run dry: silence until there is enough again,
                        // not a stutter of whatever trickles in.
                        None => {
                            self.next = 0.0;
                            self.playing = false;
                        }
                    }
                }
            } else {
                // Settle to silence from wherever the sound left off.
                (self.prev, self.next) = (self.next, 0.0);
            }
            frame.fill(convert(self.prev + (self.next - self.prev) * self.pos));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    /// Run for `seconds`: audio arrives `burst_ms` at a time from a source
    /// whose clock runs `drift` fast, and a 48 kHz stereo device takes 10 ms
    /// at a time. Returns how many times the sound stopped after it first
    /// started, and the least and most audio waiting over the last second.
    fn run(seconds: usize, burst_ms: usize, drift: f64) -> (usize, usize, usize) {
        let mut jitter = Jitter::new(RATE, 48_000);
        let (mut made, mut stops, mut started) = (0usize, 0, false);
        let (mut least, mut most) = (usize::MAX, 0);
        let mut out = vec![0.0f32; 480 * 2];
        for ms in (0..seconds * 1000).step_by(10) {
            // What the source has produced by now, a whole burst at a time.
            let due = ((ms as f64 / 1000.0) * f64::from(RATE) * (1.0 + drift)) as usize;
            let burst = RATE as usize * burst_ms / 1000;
            while made + burst <= due {
                jitter.push(&vec![8000; burst]);
                made += burst;
            }
            let was_playing = jitter.playing;
            jitter.fill(&mut out, 2, |v| v);
            started |= jitter.playing;
            stops += usize::from(started && was_playing && !jitter.playing);
            if ms >= (seconds - 1) * 1000 {
                least = least.min(jitter.queue.len());
                most = most.max(jitter.queue.len());
            }
        }
        (stops, least, most)
    }

    #[test]
    fn plays_bursts_without_gaps() {
        // An RTL-SDR at its lowest rate delivers 32 ms at a time; a busy
        // phone, more.
        for burst_ms in [1, 26, 32, 60] {
            let (stops, least, _) = run(20, burst_ms, 0.0);
            assert_eq!(stops, 0, "{burst_ms} ms bursts");
            assert!(least > 0, "{burst_ms} ms bursts");
        }
    }

    #[test]
    fn keeps_up_with_a_clock_that_differs() {
        // The radio's clock and the audio device's never agree exactly.
        for drift in [0.002, -0.002] {
            let (stops, least, most) = run(120, 32, drift);
            assert_eq!(stops, 0, "drift {drift}");
            let held = (RATE as f32 * HELD_SECS) as usize;
            assert!(
                least > held / 4 && most < held * 2,
                "drift {drift}: {least}..{most} waiting"
            );
        }
    }

    #[test]
    fn recovers_from_running_dry_and_from_a_stall() {
        let mut jitter = Jitter::new(RATE, RATE);
        let mut out = vec![0.0f32; 160];
        // Nothing plays until enough is waiting.
        jitter.push(&[8000; 800]);
        jitter.fill(&mut out, 1, |v| v);
        assert!(out.iter().all(|&v| v == 0.0));
        jitter.push(&[8000; 2000]);
        jitter.fill(&mut out, 1, |v| v);
        assert!(out[80..].iter().all(|&v| v > 0.2));
        // The source stops: the sound ends, and waits to start again.
        for _ in 0..40 {
            jitter.fill(&mut out, 1, |v| v);
        }
        assert!(!jitter.playing && out.iter().all(|&v| v == 0.0));
        // A device that stalled doesn't leave seconds of old audio to play.
        jitter.push(&vec![8000; RATE as usize * 3]);
        assert!(jitter.queue.len() <= RATE as usize * 6 / 10);
    }
}
