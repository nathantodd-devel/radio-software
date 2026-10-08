//! The receive loop: Airspy samples in, speaker audio, recordings and
//! [`Event`]s out. Blocking; front ends run it on a thread of its own and
//! steer it through [`Controls`].

use std::fs;
use std::io::{self, BufReader, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc::Receiver;

use std::collections::VecDeque;

use airspy::Airspy;
use chrono::Local;
use radiocore::{Channelizer, Complex32, NfmChannel, NfmConfig, P25Channel, P25Frame};

use crate::plan::{BIN_HZ, Entry, Kind, Plan};
use crate::wav;

const CHANNEL_BINS: usize = 96; // 48 kHz per channel
const CHANNEL_RATE: f32 = 48_000.0;
const AUDIO_RATE: u32 = 16_000;
const IF_CUTOFF_HZ: f64 = 9_000.0;
/// Fraction of the sample rate the Airspy's filters pass cleanly.
const USABLE_BANDWIDTH: f64 = 0.9;
/// Blocks (milliseconds) between [`Event::Levels`] reports.
const LEVEL_BLOCKS: u64 = 100;
/// Decoded P25 speech as audio; at this scale it sits about level with the
/// analog channels at the same volume setting.
const P25_LEVEL: f32 = 1.0 / 32768.0;
/// P25 voice arrives 180 ms at a time; this much is buffered (in 16 kHz
/// samples) before a call starts playing, so playback doesn't run dry.
const P25_PREBUFFER: usize = 4320;

pub enum Source {
    /// Receive from the first Airspy found.
    Airspy,
    /// Read 16-bit I/Q from stdin.
    Stdin,
}

pub struct Config {
    /// Airspy linearity gain, 0-21.
    pub gain: u8,
    /// Seconds to stay on a channel after it goes quiet.
    pub hold_secs: f32,
    /// Directory to save every transmission to, as WAV files.
    pub record: Option<PathBuf>,
    /// Airspy sample rate; `None` for the fastest the device offers.
    pub rate: Option<u32>,
    pub source: Source,
    /// Play audio through the speakers.
    pub audio: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gain: 17,
            hold_secs: 1.5,
            record: None,
            rate: None,
            source: Source::Airspy,
            audio: true,
        }
    }
}

/// Knobs that can be turned while the engine runs.
pub struct Controls {
    stop: AtomicBool,
    muted: AtomicBool,
    volume: AtomicU32,
    squelch_db: AtomicU32,
    pinned: AtomicUsize,
    skipped: Vec<AtomicBool>,
}

impl Controls {
    /// Controls for running `plan`, with its channels' saved skips applied.
    pub fn new(plan: &Plan, volume: f32, squelch_db: f32) -> Self {
        Self {
            stop: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            volume: AtomicU32::new(volume.to_bits()),
            squelch_db: AtomicU32::new(squelch_db.to_bits()),
            pinned: AtomicUsize::new(usize::MAX),
            skipped: plan.entries.iter().map(|e| AtomicBool::new(e.skip)).collect(),
        }
    }

    /// Ask [`run`] to return.
    pub fn stop(&self) {
        self.stop.store(true, Relaxed);
    }

    pub fn muted(&self) -> bool {
        self.muted.load(Relaxed)
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Relaxed);
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Relaxed))
    }

    pub fn set_volume(&self, volume: f32) {
        self.volume.store(volume.to_bits(), Relaxed);
    }

    /// Carrier level over the noise floor needed to open a channel.
    pub fn squelch_db(&self) -> f32 {
        f32::from_bits(self.squelch_db.load(Relaxed))
    }

    pub fn set_squelch_db(&self, db: f32) {
        self.squelch_db.store(db.to_bits(), Relaxed);
    }

    /// The one channel being listened to, when not scanning.
    pub fn pinned(&self) -> Option<usize> {
        Some(self.pinned.load(Relaxed)).filter(|&c| c != usize::MAX)
    }

    pub fn set_pinned(&self, channel: Option<usize>) {
        self.pinned.store(channel.unwrap_or(usize::MAX), Relaxed);
    }

    /// Skipped channels are ignored entirely: not played, logged or recorded.
    pub fn skipped(&self, channel: usize) -> bool {
        self.skipped[channel].load(Relaxed)
    }

    pub fn set_skipped(&self, channel: usize, skipped: bool) {
        self.skipped[channel].store(skipped, Relaxed);
    }
}

pub enum Event<'a> {
    /// A transmission started; `playing` if it is the one on the speaker.
    /// `talkgroup` is set for P25 calls.
    Opened {
        channel: usize,
        snr_db: f32,
        playing: bool,
        talkgroup: Option<u16>,
    },
    /// A transmission ended after `secs` seconds.
    Closed { channel: usize, secs: f32 },
    /// The channel on the speaker changed.
    Playing(Option<usize>),
    /// Carrier level over the noise floor for every channel, in dB.
    Levels(&'a [f32]),
}

/// Sample rate used when the device can't be asked (reading from stdin).
const DEFAULT_RATE: u32 = 10_000_000;

/// The sample rate `cfg` will run at: the one it names, or the fastest the
/// attached Airspy offers. The Airspy must not be in use when asking it.
pub fn sample_rate(cfg: &Config) -> Result<u32, String> {
    match (cfg.rate, &cfg.source) {
        (Some(rate), _) => Ok(rate),
        (None, Source::Stdin) => Ok(DEFAULT_RATE),
        (None, Source::Airspy) => {
            let rates = Airspy::open()
                .and_then(|device| device.sample_rates())
                .map_err(|e| e.to_string())?;
            rates
                .into_iter()
                .max()
                .ok_or("the Airspy offers no sample rates".into())
        }
    }
}

/// Widest spread of frequencies, in Hz, one tuning can cover at `rate`.
pub fn usable_span_hz(rate: u32) -> f64 {
    rate as f64 * USABLE_BANDWIDTH
}

/// Samples to throw away after tuning, in seconds of reception, while the
/// tuner and gain settle.
const SETTLE_SECS: f64 = 0.3;

/// Where the samples come from.
enum Input {
    Stdin(BufReader<io::StdinLock<'static>>, Vec<u8>),
    Airspy {
        /// Held open for as long as it is receiving.
        _device: Airspy,
        blocks: Receiver<airspy::Block>,
        /// Samples received but not yet handed on.
        queue: VecDeque<i16>,
    },
}

impl Input {
    fn open(center_hz: f64, rate: u32, cfg: &Config) -> Result<Self, String> {
        match cfg.source {
            Source::Stdin => Ok(Input::Stdin(
                BufReader::with_capacity(1 << 20, io::stdin().lock()),
                Vec::new(),
            )),
            Source::Airspy => (|| {
                let mut device = Airspy::open()?;
                device.set_sample_rate(rate)?;
                device.set_linearity_gain(cfg.gain)?;
                let blocks = device.start()?;
                device.set_frequency(center_hz.round() as u32)?;
                let mut settling = (rate as f64 * SETTLE_SECS) as usize;
                while settling > 0 {
                    let Ok(block) = blocks.recv() else { break };
                    settling = settling.saturating_sub(block.iq.len() / 2);
                }
                Ok(Input::Airspy {
                    _device: device,
                    blocks,
                    queue: VecDeque::new(),
                })
            })()
            .map_err(|e: airspy::Error| e.to_string()),
        }
    }

    /// Fill `iq`; false once the input has ended.
    fn read(&mut self, iq: &mut [Complex32]) -> bool {
        let sample = |i: i16, q: i16| Complex32::new(i as f32 / 32768.0, q as f32 / 32768.0);
        match self {
            Input::Stdin(reader, raw) => {
                raw.resize(iq.len() * 4, 0);
                if reader.read_exact(raw).is_err() {
                    return false;
                }
                for (s, b) in iq.iter_mut().zip(raw.as_chunks::<4>().0) {
                    *s = sample(i16::from_le_bytes([b[0], b[1]]), i16::from_le_bytes([b[2], b[3]]));
                }
            }
            Input::Airspy { blocks, queue, .. } => {
                while queue.len() < iq.len() * 2 {
                    let Ok(block) = blocks.recv() else { return false };
                    queue.extend(block.iq);
                }
                for s in iq.iter_mut() {
                    *s = sample(queue.pop_front().unwrap(), queue.pop_front().unwrap());
                }
            }
        }
        true
    }
}

fn to_pcm(audio: &[f32], volume: f32, pcm: &mut Vec<i16>) {
    pcm.clear();
    pcm.extend(
        audio
            .iter()
            .map(|v| (v * volume * 32767.0).clamp(-32767.0, 32767.0) as i16),
    );
}

/// A receiver on one channelizer output.
enum Rx {
    /// Analog FM, feeding the plan entry with the same index.
    Nfm(Box<NfmChannel>),
    P25(Box<P25Rx>),
}

/// A P25 frequency, and the call on it if there is one.
#[derive(Default)]
struct P25Rx {
    /// The system this frequency belongs to.
    system: i64,
    channel: P25Channel,
    frames: Vec<P25Frame>,
    /// Plan entry this call is playing on.
    row: Option<usize>,
    talkgroup: Option<u16>,
    /// Decoded speech at the audio rate, waiting to be played.
    fifo: VecDeque<f32>,
    playing: bool,
    ended: bool,
    /// Last 8 kHz samples, for interpolating up to the audio rate.
    last: [f32; 3],
}

impl P25Rx {
    /// Queue 8 kHz speech for playback at 16 kHz.
    fn queue(&mut self, pcm: &[i16]) {
        for &s in pcm {
            let x = s as f32 * P25_LEVEL;
            let [a, b, c] = self.last;
            // Half-band interpolation: the in-between sample from its four
            // neighbours, then the sample itself.
            self.fifo.push_back((9.0 * (b + c) - (a + x)) / 16.0);
            self.fifo.push_back(c);
            self.last = [b, c, x];
        }
    }

    fn hang_up(&mut self) {
        *self = Self {
            system: self.system,
            channel: std::mem::take(&mut self.channel),
            ..Self::default()
        };
    }
}

/// Receive until the input ends or [`Controls::stop`] is called.
pub fn run(plan: &Plan, cfg: &Config, controls: &Controls, mut on_event: impl FnMut(Event)) -> Result<(), String> {
    let (lo, hi, center_hz) = plan.span();
    if plan.frequencies().next().is_none() {
        return Err("nothing to scan: no channels selected".into());
    }
    let rate = sample_rate(cfg)?;
    if rate as f64 % (2.0 * BIN_HZ) != 0.0 {
        return Err("sample rate must be a multiple of 1000".into());
    }
    if hi - lo > usable_span_hz(rate) {
        return Err(format!(
            "these channels spread over {:.1} MHz; this Airspy covers {:.1} MHz at a time",
            (hi - lo) / 1e6,
            usable_span_hz(rate) / 1e6
        ));
    }

    // Channelizer outputs: the analog entries, then the P25 frequencies.
    let mut squelch_db = controls.squelch_db();
    let (mut offsets, mut rxs, mut rx_rows) = (Vec::new(), Vec::new(), Vec::new());
    let block_len = CHANNEL_BINS / 2;
    for (row, e) in plan.entries.iter().enumerate() {
        if let Kind::Analog {
            freq_hz,
            tone_hz,
            narrow,
        } = e.kind
        {
            offsets.push(freq_hz - center_hz);
            rx_rows.push(row);
            rxs.push(Rx::Nfm(Box::new(NfmChannel::new(NfmConfig {
                sample_rate: CHANNEL_RATE,
                block_len,
                max_deviation: if narrow { 2500.0 } else { 5000.0 },
                tone_hz,
                squelch: 10f32.powf(squelch_db / 10.0),
            }))));
        }
    }
    for f in &plan.p25 {
        offsets.push(f.freq_hz - center_hz);
        rxs.push(Rx::P25(Box::new(P25Rx {
            system: f.system,
            ..P25Rx::default()
        })));
    }
    // The entry for a call: its talkgroup's, or its system's catch-all.
    let talkgroup_row = |system: i64, talkgroup: Option<u16>| {
        let listed =
            |e: &Entry| matches!(e.kind, Kind::Talkgroup { system: s, id } if s == system && Some(id) == talkgroup);
        let other = |e: &Entry| matches!(e.kind, Kind::OtherTalkgroups { system: s } if s == system);
        plan.entries
            .iter()
            .position(listed)
            .or_else(|| plan.entries.iter().position(other))
    };
    let mut channelizer = Channelizer::new(rate as f64, BIN_HZ, CHANNEL_BINS, IF_CUTOFF_HZ, &offsets);
    assert_eq!(block_len, channelizer.out_len());
    let rows = plan.entries.len();

    if let Some(dir) = &cfg.record {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut speaker = if cfg.audio {
        Some(audio::Output::open(AUDIO_RATE)?)
    } else {
        None
    };
    let mut input = Input::open(center_hz, rate, cfg)?;

    let hop = channelizer.hop();
    let audio_len = block_len / 3;
    let block_secs = block_len as f32 / CHANNEL_RATE;
    let hold_blocks = (cfg.hold_secs / block_secs) as u32;
    let mut iq = vec![Complex32::ZERO; hop];
    let mut audio = vec![Vec::with_capacity(audio_len); rows];
    let mut open = vec![false; rows];
    let mut open_blocks = vec![0u32; rows];
    // Which receiver's P25 call has each talkgroup entry, if any.
    let mut row_rx: Vec<Option<usize>> = vec![None; rows];
    let mut recorders: Vec<Option<wav::Writer>> = (0..rows).map(|_| None).collect();
    let mut pcm = Vec::with_capacity(audio_len);
    let silence = vec![0i16; audio_len];
    let mut floor_cap = f32::INFINITY;
    let mut floors = Vec::with_capacity(rows);
    let mut levels = vec![0.0; rows];
    let mut current: Option<usize> = None;
    let mut playing: Option<usize> = None;
    let mut idle_blocks = 0u32;
    let mut blocks = 0u64;

    while !controls.stop.load(Relaxed) && input.read(&mut iq) {
        channelizer.process(&iq, |i, samples| match &mut rxs[i] {
            Rx::Nfm(channel) => {
                let row = rx_rows[i];
                audio[row].clear();
                open[row] = channel.process(samples, floor_cap, &mut audio[row]) && !controls.skipped(row);
            }
            Rx::P25(rx) => {
                let frames = &mut rx.frames;
                rx.channel.process(samples, |frame| frames.push(frame));
            }
        });

        for (i, rx) in rxs.iter_mut().enumerate() {
            let Rx::P25(rx) = rx else { continue };
            for frame in std::mem::take(&mut rx.frames) {
                match frame {
                    P25Frame::Voice { talkgroup, pcm } => {
                        // The talkgroup can arrive a frame after the voice does.
                        let row = talkgroup_row(rx.system, talkgroup);
                        if rx.row != row && !rx.playing {
                            if let Some(old) = rx.row.take() {
                                row_rx[old] = None;
                            }
                            // The same call can be on two sites' frequencies.
                            if let Some(row) = row.filter(|&row| row_rx[row].is_none()) {
                                row_rx[row] = Some(i);
                                rx.row = Some(row);
                                rx.talkgroup = talkgroup;
                            }
                        }
                        if rx.row.is_some() {
                            rx.queue(&pcm);
                        }
                    }
                    P25Frame::End => rx.ended = true,
                }
            }
            let Some(row) = rx.row else {
                rx.ended = false;
                continue;
            };
            rx.playing |= rx.fifo.len() >= P25_PREBUFFER || rx.ended;
            audio[row].clear();
            open[row] = false;
            if rx.playing {
                if rx.fifo.is_empty() && rx.ended {
                    row_rx[row] = None;
                    rx.hang_up();
                } else {
                    audio[row].extend((0..audio_len).map(|_| rx.fifo.pop_front().unwrap_or(0.0)));
                    open[row] = !controls.skipped(row);
                }
            }
        }

        if blocks.is_multiple_of(LEVEL_BLOCKS) {
            // Most channels are idle most of the time, so the median floor
            // is a fair picture of the band's noise.
            floors.clear();
            floors.extend(rxs.iter().filter_map(|rx| match rx {
                Rx::Nfm(channel) => Some(channel.floor()).filter(|f| f.is_finite()),
                Rx::P25(_) => None,
            }));
            if !floors.is_empty() {
                floors.sort_by(f32::total_cmp);
                floor_cap = floors[floors.len() / 2] * 2.0;
            }
            if controls.squelch_db() != squelch_db {
                squelch_db = controls.squelch_db();
                for rx in &mut rxs {
                    if let Rx::Nfm(channel) = rx {
                        channel.set_squelch(10f32.powf(squelch_db / 10.0));
                    }
                }
            }
            levels.fill(0.0);
            for (i, rx) in rxs.iter().enumerate() {
                match rx {
                    Rx::Nfm(channel) => levels[rx_rows[i]] = channel.snr_db(),
                    Rx::P25(rx) => {
                        if let Some(row) = rx.row {
                            levels[row] = rx.channel.snr_db();
                        }
                    }
                }
            }
            on_event(Event::Levels(&levels));
        }
        blocks += 1;

        if let Some(pin) = controls.pinned() {
            current = Some(pin);
        } else {
            match current {
                Some(c) if open[c] => idle_blocks = 0,
                Some(_) if idle_blocks < hold_blocks => idle_blocks += 1,
                _ => {
                    current = open.iter().position(|&o| o);
                    idle_blocks = 0;
                }
            }
        }
        let now_playing = current.filter(|&c| open[c]);

        let volume = controls.volume();
        for c in 0..rows {
            if open[c] && open_blocks[c] == 0 {
                let e = &plan.entries[c];
                let (snr_db, talkgroup) = match row_rx[c].map(|i| &rxs[i]) {
                    Some(Rx::P25(rx)) => (rx.channel.snr_db(), rx.talkgroup),
                    _ => match rx_rows.iter().position(|&row| row == c).map(|i| &rxs[i]) {
                        Some(Rx::Nfm(channel)) => (channel.snr_db(), None),
                        _ => (0.0, None),
                    },
                };
                on_event(Event::Opened {
                    channel: c,
                    snr_db,
                    playing: now_playing == Some(c),
                    talkgroup,
                });
                if let Some(dir) = &cfg.record {
                    let name = format!(
                        "{}_{}_{}.wav",
                        Local::now().format("%Y%m%d-%H%M%S"),
                        talkgroup
                            .map_or(e.id(), |tg| format!("TG{tg}"))
                            .replace(|ch: char| !ch.is_alphanumeric() && ch != '.', ""),
                        e.tag.replace(|ch: char| !ch.is_alphanumeric(), "_")
                    );
                    match wav::Writer::create(&dir.join(name), AUDIO_RATE) {
                        Ok(w) => recorders[c] = Some(w),
                        Err(err) => eprintln!("scanner: can't record: {err}"),
                    }
                }
            }
            if open[c] {
                open_blocks[c] += 1;
                if let Some(w) = &mut recorders[c] {
                    to_pcm(&audio[c], volume, &mut pcm);
                    w.write(&pcm);
                }
            } else if open_blocks[c] > 0 {
                on_event(Event::Closed {
                    channel: c,
                    secs: open_blocks[c] as f32 * block_secs,
                });
                open_blocks[c] = 0;
                if let Some(w) = recorders[c].take() {
                    w.finish();
                }
            }
        }
        if now_playing != playing {
            playing = now_playing;
            on_event(Event::Playing(playing));
        }

        if let Some(out) = &mut speaker {
            // Written every block, silence included, to keep the output running.
            let written = match playing {
                Some(c) if !controls.muted() => {
                    to_pcm(&audio[c], volume, &mut pcm);
                    out.write(&pcm)
                }
                _ => out.write(&silence),
            };
            if written.is_err() {
                break;
            }
        }
    }

    recorders.into_iter().flatten().for_each(wav::Writer::finish);
    drop(speaker);
    if matches!(input, Input::Airspy { .. }) && !controls.stop.load(Relaxed) {
        return Err("the Airspy stopped sending samples (unplugged?)".into());
    }
    Ok(())
}
